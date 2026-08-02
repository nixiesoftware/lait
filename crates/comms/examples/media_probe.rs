//! **Does the tunnel between two Stations carry realtime media?**
//!
//! Plan 14 §10 reserves a media plane and specifies its send discipline — one
//! unidirectional flow per encoded frame, expired frames reset by the sender and
//! stopped by the receiver, keyframes prioritised. Nothing implements it, and the
//! open question is not whether the framing is right. It is whether the *path*
//! two Stations get is good enough to be worth encoding for.
//!
//! This probe answers that with numbers instead of opinion. It pushes synthetic
//! frames at a chosen bitrate through lait's own [`comms`] seam — no iroh types
//! appear below, which is deliberate: if the seam cannot express the measurement,
//! it cannot express the media plane either, and finding that out here is cheap.
//!
//! **What it is not.** It does not speak `lait/session/1`. Opening a real Live
//! connection means an `Open` frame, admission, and a Space — that measures
//! *admission*, and this measures the *path*. It gets its own ALPN so it can
//! never be mistaken for a Station by a Station.
//!
//! # Running it
//!
//! One machine, both halves — proves the harness works, tells you nothing about
//! the network:
//!
//! ```sh
//! cargo run -p comms --example media_probe -- --loopback
//! ```
//!
//! Two machines — the measurement that matters. On the first:
//!
//! ```sh
//! cargo run -p comms --example media_probe -- --listen
//! # prints: device id 3f2a...  (64 hex chars)
//! ```
//!
//! On the second, with that id:
//!
//! ```sh
//! cargo run -p comms --example media_probe -- --dial 3f2a... --bitrate 8 --seconds 30
//! ```
//!
//! `LAIT_NETWORK` and `LAIT_RELAY` select the network the same way the daemon
//! reads them, so the same probe measures the n0 mesh, a lait-hosted relay, or a
//! direct-only path without a recompile:
//!
//! ```sh
//! LAIT_NETWORK=local LAIT_RELAY=https://relay.example cargo run -p comms --example media_probe -- --listen
//! ```
//!
//! # What it reports, and why each number is there
//!
//! - **path, and when it changed.** A connection opens relayed and gains a direct
//!   path once holepunching succeeds. The time between those is dead air a user
//!   sees as a slow start, and the share of runs that never promote is the number
//!   that decides whether lait needs its own relay fleet.
//! - **rtt, congestion window, loss.** What a rate controller would consume.
//!   `cwnd / rtt` is the transport's own opinion about throughput.
//! - **offered vs delivered bitrate.** Whether the path carried what was asked.
//! - **frames expired.** Frames dropped rather than sent late — the sender-side
//!   half of §10.2's deadline policy, and the honest cost of a bad path.
//! - **one-way delay variation (p50/p95).** The jitter number, computed as
//!   `(arrive_i - arrive_{i-1}) - (send_i - send_{i-1})`. Differences only, so it
//!   needs no clock sync between the two machines — and it is exactly the input a
//!   delay-based congestion controller (GCC, SCReAM) regulates on.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use comms::policy::Network;
use comms::{
    Connection, DefaultFactory, PathKind, Protocols, RecvFlow, Transport, TransportFactory,
};
use mechanics::ids::DeviceId;

/// The probe's own protocol. Never `lait/session/1`: a Station that somehow
/// received this must refuse it as an unknown protocol rather than half-accept
/// it as a Live peer.
const PROBE_ALPN: &[u8] = b"lait/probe/media/1";

/// Per-frame header: `seq`, the sender's clock, the payload length, and flags.
/// Fixed size so the receiver can `read_exact` it before trusting anything.
const FRAME_HEADER_BYTES: usize = 24;
const FLAG_KEYFRAME: u32 = 1;

/// Ceiling on one probe frame. Well above a realistic keyframe at 8 Mbps and
/// well below anything that would make a receiver's pre-allocation interesting —
/// this is the same "check the declared length before reserving" rule the real
/// planes use.
const MAX_FRAME_PAYLOAD: usize = 4 * 1024 * 1024;

/// How many frames may be in flight as unfinished flows at once.
///
/// Matches `runtime::budget::slots::MAX_STREAM_WORKERS`, on purpose: that is the
/// bound a real media lane would inherit, and the point of the probe is to find
/// out whether a number frozen for cursors survives 60 fps. A frame that finds
/// the budget full is counted expired at source rather than queued — queueing it
/// would measure the queue instead of the path.
const MAX_FRAMES_IN_FLIGHT: usize = 32;

const CONTROL_START_MAGIC: u32 = 0x6c_61_69_74; // "lait"
const CONTROL_REPORT_MAGIC: u32 = 0x72_70_72_74; // "rprt"
const START_BYTES: usize = 28;
const REPORT_BYTES: usize = 52;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let options = Options::from_args()?;
    match options.mode {
        Mode::Loopback => loopback(&options).await,
        Mode::Listen => listen(&options).await,
        Mode::Dial(ref peer) => {
            let peer = DeviceId::parse(peer)
                .ok_or_else(|| anyhow!("--dial wants a 64-char hex device id"))?;
            dial(&options, peer).await
        }
    }
}

// ---------------------------------------------------------------- options ---

#[derive(Debug, Clone)]
enum Mode {
    Loopback,
    Listen,
    Dial(String),
}

#[derive(Debug, Clone)]
struct Options {
    mode: Mode,
    /// Target offered bitrate in bits per second.
    bitrate: u64,
    fps: u32,
    seconds: u32,
    /// How late a frame may be before it is dropped rather than sent.
    deadline: Duration,
    /// Every Nth frame is a keyframe, sent at four times the average size —
    /// screen content is bursty, and a probe with uniform frames would measure a
    /// traffic shape no encoder produces.
    keyframe_every: u64,
    /// Which of the two fixed probe identities to use. Stable across runs so a
    /// listener's id does not change between attempts.
    seed: u8,
}

impl Options {
    fn from_args() -> Result<Self> {
        let mut mode = None;
        let mut bitrate_mbps = 8.0_f64;
        let mut fps = 60_u32;
        let mut seconds = 20_u32;
        let mut deadline_ms = 200_u64;
        let mut keyframe_every = 60_u64;
        let mut seed = None;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| anyhow!("{arg} wants a value"));
            match arg.as_str() {
                "--loopback" => mode = Some(Mode::Loopback),
                "--listen" => mode = Some(Mode::Listen),
                "--dial" => mode = Some(Mode::Dial(value()?)),
                "--bitrate" => bitrate_mbps = value()?.parse().context("--bitrate")?,
                "--fps" => fps = value()?.parse().context("--fps")?,
                "--seconds" => seconds = value()?.parse().context("--seconds")?,
                "--deadline-ms" => deadline_ms = value()?.parse().context("--deadline-ms")?,
                "--keyframe-every" => {
                    keyframe_every = value()?.parse().context("--keyframe-every")?
                }
                "--seed" => seed = Some(value()?.parse().context("--seed")?),
                "--help" | "-h" => {
                    println!("{USAGE}");
                    std::process::exit(0);
                }
                other => return Err(anyhow!("unknown argument {other}\n\n{USAGE}")),
            }
        }

        let mode =
            mode.ok_or_else(|| anyhow!("pick one of --loopback / --listen / --dial\n\n{USAGE}"))?;
        if fps == 0 {
            return Err(anyhow!("--fps must be at least 1"));
        }
        if keyframe_every == 0 {
            return Err(anyhow!("--keyframe-every must be at least 1"));
        }
        let seed = seed.unwrap_or(match mode {
            Mode::Listen | Mode::Loopback => 1,
            Mode::Dial(_) => 2,
        });

        Ok(Self {
            mode,
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an operator-supplied megabit figure, bounded by the run below"
            )]
            bitrate: (bitrate_mbps * 1_000_000.0) as u64,
            fps,
            seconds,
            deadline: Duration::from_millis(deadline_ms),
            keyframe_every,
            seed,
        })
    }

    /// Bytes in an ordinary (non-key) frame at the offered bitrate.
    ///
    /// Keyframes take four shares, so the average over a keyframe period is the
    /// requested bitrate rather than 4× it — a probe that quietly offered more
    /// than it claimed would blame the path for its own arithmetic.
    fn frame_payload(&self) -> usize {
        let per_second = self.bitrate / 8;
        let extra_key_shares = 3;
        let shares =
            u64::from(self.fps) + (u64::from(self.fps) / self.keyframe_every) * extra_key_shares;
        let bytes = per_second.checked_div(shares.max(1)).unwrap_or(0);
        usize::try_from(bytes)
            .unwrap_or(0)
            .clamp(64, MAX_FRAME_PAYLOAD / 4)
    }
}

const USAGE: &str = "\
media_probe — measure whether a Station-to-Station path carries realtime media

  --loopback              both halves in this process (harness smoke test)
  --listen                accept a probe and report what arrived
  --dial <64-hex-id>      push frames at a listening probe

  --bitrate <mbps>        offered bitrate            [8]
  --fps <n>               frames per second          [60]
  --seconds <n>           run length                 [20]
  --deadline-ms <n>       drop a frame older than    [200]
  --keyframe-every <n>    keyframe period in frames  [60]
  --seed <0-255>          which fixed probe identity [1 listen / 2 dial]

LAIT_NETWORK = public | local | isolated, and LAIT_RELAY for local, are read
exactly as the daemon reads them.";

// ------------------------------------------------------------------ modes ---

async fn build_transport(seed: u8) -> Result<Arc<dyn Transport>> {
    let network = Network::from_env().context("LAIT_NETWORK / LAIT_RELAY")?;
    let factory = DefaultFactory;
    let protocols = Protocols {
        framed: &[],
        session: &[PROBE_ALPN],
    };
    factory
        .build(&[seed; 32], &network, protocols)
        .await
        .context("build transport")
}

async fn listen(options: &Options) -> Result<()> {
    let transport = build_transport(options.seed).await?;
    println!("device id {}", transport.my_id().as_str());
    println!("waiting for a probe to dial…");

    while let Some(incoming) = transport.accept_connection().await {
        if incoming.alpn != PROBE_ALPN {
            continue;
        }
        println!("\nprobe from {}", incoming.from.short());
        let connection: Arc<dyn Connection> = Arc::from(incoming.connection);
        if let Err(e) = respond(connection).await {
            println!("probe ended: {e:#}");
        }
        println!("waiting for the next probe…");
    }
    Ok(())
}

async fn dial(options: &Options, peer: DeviceId) -> Result<()> {
    let transport = build_transport(options.seed).await?;
    println!("device id {}", transport.my_id().as_str());
    println!("dialling {}…", peer.short());

    // Under `Public` this is a no-op — n0 discovery already resolves a bare id.
    // Under `Local` it is the whole resolution story: there is no discovery
    // service, and a peer becomes reachable only once it has been registered as
    // `{id, relay}`. The daemon does exactly this; a probe that skipped it would
    // work against n0 and fail against a lait relay, which is the comparison the
    // probe exists to make.
    transport.learn(peer.clone(), &[]);

    let dial_started = Instant::now();
    let connection: Arc<dyn Connection> = Arc::from(
        transport
            .connect_session(peer, PROBE_ALPN)
            .await
            .context("connect to the listening probe")?,
    );
    println!("connected in {:?}", dial_started.elapsed());

    let outcome = send_run(&connection, options).await?;
    outcome.print(options);
    connection.close(0, b"probe complete");
    Ok(())
}

async fn loopback(options: &Options) -> Result<()> {
    // Two independent transports, so the frames really cross a socket rather
    // than a channel. The path will be direct and the numbers will be a machine
    // measuring itself — useful for proving the harness, useless for the network
    // question, and the report says so rather than letting a reader forget.
    let responder = build_transport(options.seed).await?;
    let dialer = build_transport(options.seed.wrapping_add(1)).await?;

    let responder_id = responder.my_id();
    let accepting = tokio::spawn(async move {
        while let Some(incoming) = responder.accept_connection().await {
            if incoming.alpn == PROBE_ALPN {
                let connection: Arc<dyn Connection> = Arc::from(incoming.connection);
                let _ = respond(connection).await;
                return;
            }
        }
    });

    dialer.learn(responder_id.clone(), &[]);
    let connection: Arc<dyn Connection> = Arc::from(
        dialer
            .connect_session(responder_id, PROBE_ALPN)
            .await
            .context("loopback connect")?,
    );
    let outcome = send_run(&connection, options).await?;
    outcome.print(options);
    println!(
        "\nNOTE  --loopback measures this machine, not a network. The path is\n      \
              always direct and the rtt is a socket round trip. Run --listen /\n      \
              --dial on two hosts for a number that means anything."
    );

    connection.close(0, b"probe complete");
    accepting.abort();
    Ok(())
}

// ----------------------------------------------------------------- sender ---

/// Everything the sending half observed.
struct Outcome {
    frames_offered: u64,
    frames_sent: u64,
    frames_expired_at_source: u64,
    /// Already past their deadline before the transport was asked to carry
    /// them. Distinct from an in-flight expiry: this one never touched the wire.
    frames_expired_before_send: u64,
    frames_expired_in_flight: u64,
    /// Frames the clock could not wake in time for. Not a network fact — a
    /// pacing fact, and the report separates them because a slow pacer offers
    /// less than it claims and would otherwise read as a slow path.
    frames_late: u64,
    bytes_written: u64,
    elapsed: Duration,
    /// Every time the selected path changed, and when.
    path_timeline: Vec<(Duration, PathKind, usize)>,
    rtt_samples: Vec<Duration>,
    final_cwnd: Option<u64>,
    final_loss: Option<f64>,
    congestion_events: Option<u64>,
    last_report: Option<Report>,
}

async fn send_run(connection: &Arc<dyn Connection>, options: &Options) -> Result<Outcome> {
    // The dialer speaks first — lait's rule on every plane, and the reason an
    // accepted flow may not exist on the wire until the opener writes.
    let (mut control_send, control_recv) =
        connection.open_bi().await.context("open control flow")?;
    control_send
        .write_all(&encode_start(options))
        .await
        .context("write start")?;

    let latest_report = Arc::new(Mutex::new(None));
    let reports = tokio::spawn(read_reports(control_recv, Arc::clone(&latest_report)));

    // Path sampling runs beside the send loop rather than inside it: a sample
    // taken only when a frame is due would miss a promotion that happens during
    // a stall, which is exactly when it is most interesting.
    let path_timeline = Arc::new(Mutex::new(Vec::new()));
    let sampling = tokio::spawn(sample_path(
        Arc::clone(connection),
        Arc::clone(&path_timeline),
    ));

    let payload = frame_payload_bytes(options.frame_payload());
    let keyframe_payload = frame_payload_bytes(options.frame_payload() * 4);

    let frames_sent = Arc::new(AtomicU64::new(0));
    let bytes_written = Arc::new(AtomicU64::new(0));
    let expired_in_flight = Arc::new(AtomicU64::new(0));
    let expired_before_send = Arc::new(AtomicU64::new(0));
    let in_flight = Arc::new(tokio::sync::Semaphore::new(MAX_FRAMES_IN_FLIGHT));

    let interval_nanos = 1_000_000_000 / u64::from(options.fps);
    let frame_interval = Duration::from_nanos(interval_nanos);
    let total_frames = u64::from(options.fps) * u64::from(options.seconds);
    let started = Instant::now();
    let mut expired_at_source = 0_u64;
    let mut frames_late = 0_u64;

    for seq in 0..total_frames {
        // Absolute deadlines, not a repeating interval. A `tokio::time::interval`
        // that misses a tick either bursts or slips, and slipping silently
        // stretches the run — which turns up in the report as a bitrate the path
        // supposedly could not carry, when in fact the sender never offered it.
        // Scheduling against `started` cannot accumulate drift, and what the
        // clock *cannot* deliver is counted below instead of hidden.
        let due = started + Duration::from_nanos(interval_nanos.saturating_mul(seq));
        tokio::time::sleep_until(due.into()).await;
        if Instant::now().saturating_duration_since(due) > frame_interval {
            frames_late += 1;
        }
        let is_key = seq % options.keyframe_every == 0;
        let body: &Arc<Vec<u8>> = if is_key { &keyframe_payload } else { &payload };

        // No permit means MAX_FRAMES_IN_FLIGHT frames are still unfinished. The
        // path is behind; a real encoder's answer is to skip this frame, not to
        // let a queue grow behind it.
        let Ok(permit) = Arc::clone(&in_flight).try_acquire_owned() else {
            expired_at_source += 1;
            continue;
        };

        let connection = Arc::clone(connection);
        let body = Arc::clone(body);
        let frames_sent = Arc::clone(&frames_sent);
        let bytes_written = Arc::clone(&bytes_written);
        let expired_in_flight = Arc::clone(&expired_in_flight);
        // The deadline runs from when the frame was *due*, not from when this
        // task got scheduled. An encoder's frame is stale from the moment it was
        // meant to exist, and measuring from the send attempt would forgive
        // exactly the lateness worth counting.
        let deadline = options.deadline;

        let expired_before_send = Arc::clone(&expired_before_send);

        tokio::spawn(async move {
            let _permit = permit;

            // Stale before it was ever offered to the transport. Checked
            // explicitly rather than left to the timeout below, because a
            // `write_all` on an unblocked path completes without ever yielding —
            // `timeout(ZERO, ready_future)` returns `Ok`, so a deadline that has
            // already passed would silently send anyway. The deadline is a
            // policy; it should not depend on whether the socket happened to
            // block.
            if due.elapsed() >= deadline {
                expired_before_send.fetch_add(1, Ordering::Relaxed);
                return;
            }

            let header = encode_frame_header(seq, body.len(), is_key);

            let Ok(mut flow) = connection.open_uni().await else {
                expired_in_flight.fetch_add(1, Ordering::Relaxed);
                return;
            };
            // §10.2: keyframes outrank the frames that depend on them. Advisory —
            // correctness never rests on it, but a starved keyframe stalls every
            // later frame, so it is worth asking for.
            flow.set_priority(if is_key { 10 } else { 0 });

            let remaining = deadline.saturating_sub(due.elapsed());
            let write = async {
                flow.write_all(&header).await?;
                flow.write_all(&body).await?;
                flow.finish()
            };
            match tokio::time::timeout(remaining, write).await {
                Ok(Ok(())) => {
                    frames_sent.fetch_add(1, Ordering::Relaxed);
                    bytes_written
                        .fetch_add((FRAME_HEADER_BYTES + body.len()) as u64, Ordering::Relaxed);
                }
                // Past its deadline mid-write, or the write failed. Either way the
                // frame is worthless now: reset so the receiver learns it was
                // abandoned rather than reading a truncated one, and so the bytes
                // still queued are not sent at the expense of the next frame.
                _ => {
                    flow.reset(1);
                    expired_in_flight.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    let elapsed = started.elapsed();

    // Let stragglers land and one more report come back before reading the
    // receiver's view. Two deadlines is long enough that anything still missing
    // is missing rather than late.
    tokio::time::sleep(options.deadline * 2 + Duration::from_millis(600)).await;

    let quality = connection.quality();
    sampling.abort();
    reports.abort();
    let _ = control_send.finish();

    let (timeline, rtts) = {
        let mut guard = path_timeline.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *guard)
    }
    .into_iter()
    .fold(
        (Vec::new(), Vec::new()),
        |(mut timeline, mut rtts), sample| {
            let PathSample {
                at,
                via,
                open_paths,
                rtt,
            } = sample;
            if timeline
                .last()
                .is_none_or(|(_, last_via, last_open)| *last_via != via || *last_open != open_paths)
            {
                timeline.push((at, via, open_paths));
            }
            if let Some(rtt) = rtt {
                rtts.push(rtt);
            }
            (timeline, rtts)
        },
    );

    let last_report = latest_report
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();

    Ok(Outcome {
        frames_offered: total_frames,
        frames_sent: frames_sent.load(Ordering::Relaxed),
        frames_expired_at_source: expired_at_source,
        frames_expired_before_send: expired_before_send.load(Ordering::Relaxed),
        frames_expired_in_flight: expired_in_flight.load(Ordering::Relaxed),
        frames_late,
        bytes_written: bytes_written.load(Ordering::Relaxed),
        elapsed,
        path_timeline: timeline,
        rtt_samples: rtts,
        final_cwnd: quality.congestion_window,
        final_loss: quality.loss_ratio(),
        congestion_events: quality.congestion_events,
        last_report,
    })
}

struct PathSample {
    at: Duration,
    via: PathKind,
    open_paths: usize,
    rtt: Option<Duration>,
}

/// Sample the path every 50 ms for the life of the run.
///
/// Fast enough to time a holepunch promotion usefully, slow enough that the
/// sampling is not itself a load. Only *changes* are kept in the timeline; the
/// rtt from every sample is kept, because a distribution needs the boring ones.
async fn sample_path(connection: Arc<dyn Connection>, into: Arc<Mutex<Vec<PathSample>>>) {
    let started = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(50));
    loop {
        ticker.tick().await;
        let quality = connection.quality();
        let sample = PathSample {
            at: started.elapsed(),
            via: quality.via,
            open_paths: quality.open_paths,
            rtt: quality.rtt,
        };
        into.lock().unwrap_or_else(|p| p.into_inner()).push(sample);
    }
}

async fn read_reports(mut recv: Box<dyn RecvFlow>, into: Arc<Mutex<Option<Report>>>) {
    loop {
        let Ok(bytes) = recv.read_exact(REPORT_BYTES).await else {
            return;
        };
        if let Some(report) = decode_report(&bytes) {
            *into.lock().unwrap_or_else(|p| p.into_inner()) = Some(report);
        }
    }
}

fn frame_payload_bytes(len: usize) -> Arc<Vec<u8>> {
    // A cheap non-constant pattern. Nothing on this path compresses, but a
    // buffer of zeroes is the kind of thing that makes a surprising result hard
    // to rule out later.
    Arc::new(
        (0..len.min(MAX_FRAME_PAYLOAD))
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect(),
    )
}

// -------------------------------------------------------------- responder ---

async fn respond(connection: Arc<dyn Connection>) -> Result<()> {
    let (mut report_send, mut control_recv) = connection
        .accept_bi()
        .await
        .context("accept control flow")?
        .ok_or_else(|| anyhow!("peer opened no control flow"))?;

    let start = decode_start(
        &control_recv
            .read_exact(START_BYTES)
            .await
            .context("read start")?,
    )
    .ok_or_else(|| anyhow!("malformed start"))?;
    println!(
        "  run: {:.1} Mbps offered, {} fps, {} s, {} ms deadline",
        start.bitrate as f64 / 1_000_000.0,
        start.fps,
        start.seconds,
        start.deadline_ms
    );

    let arrivals = Arc::new(Mutex::new(Arrivals::new()));
    let reporting = {
        let arrivals = Arc::clone(&arrivals);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(500));
            loop {
                ticker.tick().await;
                let report = arrivals.lock().unwrap_or_else(|p| p.into_inner()).report();
                if report_send
                    .write_all(&encode_report(&report))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        })
    };

    // One task per frame: a frame is one flow, and reading them serially would
    // measure this loop's scheduling instead of the path's delivery.
    while let Some(flow) = connection.accept_uni().await.transpose() {
        let Ok(flow) = flow else { break };
        let arrivals = Arc::clone(&arrivals);
        tokio::spawn(async move {
            if let Some((seq, sent_nanos, bytes)) = read_frame(flow).await {
                arrivals
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .record(seq, sent_nanos, bytes);
            }
        });
    }

    reporting.abort();
    let final_report = arrivals.lock().unwrap_or_else(|p| p.into_inner()).report();
    println!(
        "  received {} frames / {:.1} MB, delay variation p50 {} µs p95 {} µs",
        final_report.frames,
        final_report.bytes as f64 / 1_000_000.0,
        final_report.dv_p50_micros,
        final_report.dv_p95_micros
    );
    Ok(())
}

/// Read one frame, or nothing if the sender abandoned it.
///
/// A reset arrives here as a read error, which is the distinction the whole
/// finish/reset split exists for: an abandoned frame must not be counted as a
/// short one.
async fn read_frame(mut flow: Box<dyn RecvFlow>) -> Option<(u64, u64, usize)> {
    let header = flow.read_exact(FRAME_HEADER_BYTES).await.ok()?;
    let seq = u64::from_le_bytes(header.get(0..8)?.try_into().ok()?);
    let sent_nanos = u64::from_le_bytes(header.get(8..16)?.try_into().ok()?);
    let len = u32::from_le_bytes(header.get(16..20)?.try_into().ok()?) as usize;
    if len > MAX_FRAME_PAYLOAD {
        flow.stop(2);
        return None;
    }
    let body = flow.read_exact(len).await.ok()?;
    Some((seq, sent_nanos, FRAME_HEADER_BYTES + body.len()))
}

/// What arrived, and when, in the only terms that need no shared clock.
struct Arrivals {
    baseline: Instant,
    frames: u64,
    bytes: u64,
    reordered: u64,
    highest_seq: u64,
    /// `seq -> (sender nanos, local arrival nanos)`, kept so a frame that
    /// arrives out of order can still be paired with its neighbour.
    seen: HashMap<u64, (u64, u64)>,
    delay_variation_micros: Vec<i64>,
}

impl Arrivals {
    fn new() -> Self {
        Self {
            baseline: Instant::now(),
            frames: 0,
            bytes: 0,
            reordered: 0,
            highest_seq: 0,
            seen: HashMap::new(),
            delay_variation_micros: Vec::new(),
        }
    }

    fn record(&mut self, seq: u64, sent_nanos: u64, bytes: usize) {
        let arrived = u64::try_from(self.baseline.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.frames += 1;
        self.bytes += bytes as u64;
        if seq < self.highest_seq {
            self.reordered += 1;
        }
        self.highest_seq = self.highest_seq.max(seq);
        self.seen.insert(seq, (sent_nanos, arrived));

        // Delay variation against the immediately preceding frame, when both are
        // in hand. Differences of differences: the sender's clock offset cancels,
        // which is what makes this comparable across two machines that have never
        // agreed on the time.
        if let Some(prev) = seq.checked_sub(1).and_then(|p| self.seen.get(&p)).copied() {
            self.push_variation(prev, (sent_nanos, arrived));
        }
        if let Some(next) = self.seen.get(&seq.saturating_add(1)).copied() {
            self.push_variation((sent_nanos, arrived), next);
        }
    }

    fn push_variation(&mut self, earlier: (u64, u64), later: (u64, u64)) {
        let sent_delta =
            i64::try_from(later.0).unwrap_or(i64::MAX) - i64::try_from(earlier.0).unwrap_or(0);
        let arrived_delta =
            i64::try_from(later.1).unwrap_or(i64::MAX) - i64::try_from(earlier.1).unwrap_or(0);
        self.delay_variation_micros
            .push((arrived_delta - sent_delta) / 1_000);
    }

    fn report(&self) -> Report {
        let mut sorted = self.delay_variation_micros.clone();
        sorted.sort_unstable();
        Report {
            frames: self.frames,
            bytes: self.bytes,
            reordered: self.reordered,
            dv_p50_micros: percentile(&sorted, 50),
            dv_p95_micros: percentile(&sorted, 95),
            highest_seq: self.highest_seq,
        }
    }
}

fn percentile(sorted: &[i64], p: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (sorted.len().saturating_sub(1)).saturating_mul(p) / 100;
    sorted.get(index).copied().unwrap_or(0)
}

// ---------------------------------------------------------------- reports ---

#[derive(Debug, Clone)]
struct Report {
    frames: u64,
    bytes: u64,
    reordered: u64,
    dv_p50_micros: i64,
    dv_p95_micros: i64,
    highest_seq: u64,
}

struct Start {
    bitrate: u64,
    fps: u32,
    seconds: u32,
    deadline_ms: u32,
}

fn encode_start(options: &Options) -> Vec<u8> {
    let mut out = Vec::with_capacity(START_BYTES);
    out.extend_from_slice(&CONTROL_START_MAGIC.to_le_bytes());
    out.extend_from_slice(&options.bitrate.to_le_bytes());
    out.extend_from_slice(&options.fps.to_le_bytes());
    out.extend_from_slice(&options.seconds.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(options.deadline.as_millis())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u32::try_from(options.keyframe_every)
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out
}

fn decode_start(bytes: &[u8]) -> Option<Start> {
    if u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) != CONTROL_START_MAGIC {
        return None;
    }
    Some(Start {
        bitrate: u64::from_le_bytes(bytes.get(4..12)?.try_into().ok()?),
        fps: u32::from_le_bytes(bytes.get(12..16)?.try_into().ok()?),
        seconds: u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?),
        deadline_ms: u32::from_le_bytes(bytes.get(20..24)?.try_into().ok()?),
    })
}

fn encode_report(report: &Report) -> Vec<u8> {
    let mut out = Vec::with_capacity(REPORT_BYTES);
    out.extend_from_slice(&CONTROL_REPORT_MAGIC.to_le_bytes());
    out.extend_from_slice(&report.frames.to_le_bytes());
    out.extend_from_slice(&report.bytes.to_le_bytes());
    out.extend_from_slice(&report.reordered.to_le_bytes());
    out.extend_from_slice(&report.dv_p50_micros.to_le_bytes());
    out.extend_from_slice(&report.dv_p95_micros.to_le_bytes());
    out.extend_from_slice(&report.highest_seq.to_le_bytes());
    out
}

fn decode_report(bytes: &[u8]) -> Option<Report> {
    if u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) != CONTROL_REPORT_MAGIC {
        return None;
    }
    Some(Report {
        frames: u64::from_le_bytes(bytes.get(4..12)?.try_into().ok()?),
        bytes: u64::from_le_bytes(bytes.get(12..20)?.try_into().ok()?),
        reordered: u64::from_le_bytes(bytes.get(20..28)?.try_into().ok()?),
        dv_p50_micros: i64::from_le_bytes(bytes.get(28..36)?.try_into().ok()?),
        dv_p95_micros: i64::from_le_bytes(bytes.get(36..44)?.try_into().ok()?),
        highest_seq: u64::from_le_bytes(bytes.get(44..52)?.try_into().ok()?),
    })
}

fn encode_frame_header(seq: u64, len: usize, keyframe: bool) -> Vec<u8> {
    let sent_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let mut out = Vec::with_capacity(FRAME_HEADER_BYTES);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&sent_nanos.to_le_bytes());
    out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&(if keyframe { FLAG_KEYFRAME } else { 0 }).to_le_bytes());
    out
}

// ----------------------------------------------------------------- output ---

impl Outcome {
    #[allow(
        clippy::cast_precision_loss,
        reason = "counters rendered for a human, at magnitudes far below 2^53"
    )]
    fn print(&self, options: &Options) {
        let seconds = self.elapsed.as_secs_f64().max(f64::EPSILON);
        let offered = (options.bitrate as f64) / 1_000_000.0;
        let written = (self.bytes_written as f64 * 8.0) / seconds / 1_000_000.0;

        println!("\n--- path ---");
        if self.path_timeline.is_empty() {
            println!("  no path samples (the transport reported nothing)");
        }
        for (at, via, open) in &self.path_timeline {
            println!("  {:>8.3}s  {via:?}  ({open} open)", at.as_secs_f64());
        }
        let promoted = self
            .path_timeline
            .iter()
            .find(|(_, via, _)| *via == PathKind::Direct)
            .map(|(at, _, _)| *at);
        match promoted {
            Some(at) if at.is_zero() => println!("  direct from the first sample"),
            Some(at) => println!("  promoted to direct after {:.3}s", at.as_secs_f64()),
            None => println!("  NEVER promoted to a direct path — this run was relayed throughout"),
        }

        println!("\n--- transport ---");
        let mut rtts = self.rtt_samples.clone();
        rtts.sort_unstable();
        if rtts.is_empty() {
            println!("  rtt                  (not reported)");
        } else {
            let pick = |p: usize| {
                rtts.get((rtts.len().saturating_sub(1)).saturating_mul(p) / 100)
                    .copied()
                    .unwrap_or_default()
            };
            println!(
                "  rtt                  p50 {:?}  p95 {:?}  min {:?}",
                pick(50),
                pick(95),
                rtts.first().copied().unwrap_or_default()
            );
        }
        match self.final_cwnd {
            Some(cwnd) => println!("  congestion window    {cwnd} bytes"),
            None => println!("  congestion window    (not reported)"),
        }
        match self.final_loss {
            Some(loss) => println!("  loss                 {:.3}%", loss * 100.0),
            None => println!("  loss                 (not reported)"),
        }
        if let Some(events) = self.congestion_events {
            println!("  congestion events    {events}");
        }

        println!("\n--- pacing ---");
        let achieved_fps = self.frames_offered as f64 / seconds;
        println!(
            "  frame rate           {achieved_fps:.1} fps achieved of {} requested",
            options.fps
        );
        println!(
            "  late wake-ups        {} of {} frames",
            self.frames_late, self.frames_offered
        );
        if achieved_fps < f64::from(options.fps) * 0.9 {
            println!(
                "  WARNING  this run offered less than it claimed: the pacer managed {achieved_fps:.1} of\n           \
                 the {} fps requested, so every bitrate below is against what was\n           \
                 actually sent, and the shortfall is this sender's, not the path's.\n           \
                 Look at late wake-ups first (a clock or scheduling problem) and at\n           \
                 frames expired at source second (the in-flight budget saturating).",
                options.fps
            );
        }

        println!("\n--- offered vs delivered ---");
        println!("  requested            {offered:.2} Mbps");
        println!("  written              {written:.2} Mbps over {seconds:.1}s");
        println!("  frames offered       {}", self.frames_offered);
        println!("  frames sent          {}", self.frames_sent);
        println!(
            "  frames expired       {} at source (in-flight budget full) / {} before send \
             (already stale) / {} in flight (deadline or write failure)",
            self.frames_expired_at_source,
            self.frames_expired_before_send,
            self.frames_expired_in_flight
        );

        println!("\n--- as the receiver saw it ---");
        match &self.last_report {
            None => println!("  no report came back — the receiver never spoke"),
            Some(report) => {
                let delivered = (report.bytes as f64 * 8.0) / seconds / 1_000_000.0;
                let lost = self.frames_sent.saturating_sub(report.frames);
                println!("  delivered            {delivered:.2} Mbps");
                println!(
                    "  frames               {} received, {} of the sent frames never arrived",
                    report.frames, lost
                );
                println!("  reordered            {}", report.reordered);
                println!(
                    "  delay variation      p50 {} µs   p95 {} µs",
                    report.dv_p50_micros, report.dv_p95_micros
                );
            }
        }
    }
}
