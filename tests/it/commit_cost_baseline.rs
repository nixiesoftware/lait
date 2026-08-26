//! Plan 13 F0 — the commit-cost baseline.
//!
//! The docket's headline claim is that a one-Body edit currently costs
//! O(total Bodies + total required objects), and F1 makes it O(changed × radix
//! depth). Neither half of that sentence means anything without a number, so
//! this harness measures the *write* path directly: build a durable Replica at
//! a given Body count, then edit exactly one Body and record what that costs.
//!
//! It shares no code with `issues_reference_perf.rs` deliberately. That gate
//! measures request latency through the control socket, records p95 only, and
//! reports RSS on Linux alone. This one measures p50/p95/p99, counts objects
//! and bytes at the store, and reports peak RSS everywhere it runs — including
//! Windows, which is where lait is developed.
//!
//! Default runs use the small scales so every CI leg exercises the harness.
//! `LAIT_COMMIT_BASELINE_FULL=1` adds the 50k and 100k points.
//!
//! The docket asked for the quota to be raised for the 100k point. It cannot
//! be, and does not need to be: 100,000 *is* `max_space_bodies`'s protocol
//! maximum and `set_quota` clamps to it, so raising is not expressible — and
//! the operation being measured edits a Body that already exists, which adds
//! no Body and so never reaches the count check. The harness asserts it sat at
//! the ceiling rather than claiming to have lifted it.
//!
//! Corpus construction is batched. Building N Bodies one commit at a time is
//! O(N^2) against exactly the cost this harness exists to expose, which puts
//! 100k out of reach; batching the build leaves the *measured* operation — one
//! single-Body edit — untouched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::body::{BodyBinding, Op, StaticBodyKeys, SupportedSchemas, MUTATION_ATOMIC};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::frontier::AuthorityFrontier;
use replica::transaction::{CommitAuthorization, CommitContext, SeedSigner};
use replica::Replica;

const WRITER_SEED: [u8; 32] = [61u8; 32];
const EPOCH: [u8; 16] = [3u8; 16];
const EPOCH_KEY: [u8; 32] = [4u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway root that removes itself — see [`crate::head::temp_root`],
/// which is the one place that knows how.
fn temp_store(tag: &str) -> crate::head::TempRoot {
    crate::head::temp_root(&format!("baseline-{tag}"))
}

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

fn world() -> WorldId {
    WorldId::parse("com.example.notes").unwrap()
}

fn device() -> mechanics::ids::DeviceId {
    mechanics::actor::device_from_seed(&WRITER_SEED)
}

fn keys() -> Arc<StaticBodyKeys> {
    Arc::new(StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
    ))
}

fn binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("blob").unwrap(),
        schema_version: 1,
        encoding: EncodingId::parse("bytes").unwrap(),
        mutation_model: MUTATION_ATOMIC,
    }
}

fn supported() -> SupportedSchemas {
    let mut s = SupportedSchemas::new();
    s.declare(
        world(),
        SchemaId::parse("blob").unwrap(),
        1,
        EncodingId::parse("bytes").unwrap(),
        MUTATION_ATOMIC,
    );
    s
}

fn demand() -> Vec<u8> {
    use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.notes", "write"),
        Resource::root("com.example.notes"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

fn body(n: u64) -> BodyKey {
    let mut raw = [0u8; 16];
    raw[..8].copy_from_slice(&n.to_le_bytes());
    BodyKey::new(world(), BodyId::from_bytes(raw))
}

fn request(n: u64) -> [u8; 16] {
    let mut raw = [0u8; 16];
    raw[..8].copy_from_slice(&n.to_le_bytes());
    raw[15] = 0xA5;
    raw
}

/// One durable commit of `ops` through the public signed path. The measured
/// operation always passes a single op; corpus construction passes many.
fn commit(replica: &mut Replica, seq: u64, ops: &[(BodyKey, Op)]) {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let actor = mechanics::ids::ActorId::from_incept_hash(&"d".repeat(64));
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
    };
    let bindings: Vec<(BodyKey, BodyBinding)> =
        ops.iter().map(|(k, _)| (k.clone(), binding())).collect();
    replica
        .commit_action(
            &ctx,
            &CommitAuthorization {
                actor: actor.as_str(),
                parent_manifest_root: [0u8; 32],
                demand: demand(),
                intent_digest: [7u8; 32],
                authorizer: &replica::transaction::StaticAuthorizer {
                    world: world(),
                    implementation_id: [0u8; 32],
                },
            },
            &world(),
            &device(),
            &request(seq),
            &[7u8; 32],
            Vec::new(),
            Vec::new(),
            actor.as_str(),
            ops,
            &bindings,
            &[],
        )
        .expect("durable commit");
}

fn replace(key: &BodyKey, value: &[u8]) -> (BodyKey, Op) {
    (
        key.clone(),
        Op::ReplaceAtomic {
            value: value.to_vec(),
        },
    )
}

/// What one store directory currently holds: object count and total bytes,
/// plus the manifest's size. These are the durable cost the commit paid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct StoreFootprint {
    objects: u64,
    object_bytes: u64,
    manifest_bytes: u64,
}

fn footprint(root: &Path) -> StoreFootprint {
    let mut out = StoreFootprint::default();
    if let Ok(entries) = std::fs::read_dir(root.join("objects")) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    out.objects += 1;
                    out.object_bytes += meta.len();
                }
            }
        }
    }
    out.manifest_bytes = std::fs::metadata(root.join("current-manifest"))
        .map(|m| m.len())
        .unwrap_or(0);
    out
}

/// Peak resident set of this process, in bytes, on every platform the harness
/// runs on. A blank number is reported as blank rather than as zero.
fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        (ok != 0).then_some(counters.PeakWorkingSetSize as u64)
    }
    #[cfg(target_vendor = "apple")]
    {
        // `ru_maxrss` is bytes on Darwin (kilobytes on Linux, which takes the
        // /proc path above).
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        (ok == 0).then_some(usage.ru_maxrss as u64)
    }
    #[cfg(not(any(target_os = "linux", windows, target_vendor = "apple")))]
    {
        None
    }
}

fn percentile(sorted: &[u128], q: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64) * q).ceil() as usize;
    sorted[idx.saturating_sub(1).min(sorted.len() - 1)]
}

/// One scale's measurement.
#[derive(Debug)]
struct Measurement {
    bodies: u64,
    at_protocol_ceiling: bool,
    edit_p50_us: u128,
    edit_p95_us: u128,
    edit_p99_us: u128,
    objects_before: u64,
    objects_written_per_edit: f64,
    object_bytes_before: u64,
    object_bytes_per_edit: f64,
    manifest_bytes: u64,
}

/// Build `bodies` Bodies, then time `samples` further single-Body edits.
fn measure(bodies: u64, samples: usize) -> Measurement {
    let dir = temp_store(&format!("{bodies}"));
    let mut replica = Replica::open(dir.as_path(), keys()).expect("open store");
    replica.set_supported(supported());

    let quota = *replica.quota();
    let at_protocol_ceiling = bodies >= quota.max_space_bodies;
    assert!(
        bodies <= quota.max_space_bodies,
        "{bodies} Bodies exceeds the protocol maximum {} — the quota cannot be          raised past it, so this scale is not measurable",
        quota.max_space_bodies
    );

    let payload = vec![0xABu8; 256];
    let mut created = 0u64;
    let mut seq = 0u64;
    while created < bodies {
        let batch = CORPUS_BATCH.min(bodies - created);
        let ops: Vec<(BodyKey, Op)> = (created..created + batch)
            .map(|n| replace(&body(n), &payload))
            .collect();
        commit(&mut replica, seq, &ops);
        created += batch;
        seq += 1;
    }
    let before = footprint(&dir);

    // Edit one already-present Body, repeatedly. This is the operation whose
    // cost the docket claims is proportional to the whole store.
    let target = body(0);
    let mut timings: Vec<u128> = Vec::with_capacity(samples);
    for i in 0..samples {
        let value = vec![(i % 251) as u8; 256];
        let op = [replace(&target, &value)];
        let started = Instant::now();
        commit(&mut replica, seq + i as u64, &op);
        timings.push(started.elapsed().as_micros());
    }
    let after = footprint(&dir);
    timings.sort_unstable();

    let per_edit = samples as f64;
    let measurement = Measurement {
        bodies,
        at_protocol_ceiling,
        edit_p50_us: percentile(&timings, 0.50),
        edit_p95_us: percentile(&timings, 0.95),
        edit_p99_us: percentile(&timings, 0.99),
        objects_before: before.objects,
        objects_written_per_edit: (after.objects.saturating_sub(before.objects)) as f64 / per_edit,
        object_bytes_before: before.object_bytes,
        object_bytes_per_edit: (after.object_bytes.saturating_sub(before.object_bytes)) as f64
            / per_edit,
        manifest_bytes: after.manifest_bytes,
    };
    let _ = std::fs::remove_dir_all(&dir);
    measurement
}

/// The scales the docket names. The two large ones are slow enough that they
/// run only under the explicit flag, in the dedicated CI job.
fn scales() -> Vec<u64> {
    if std::env::var("LAIT_COMMIT_BASELINE_FULL").is_ok() {
        vec![1_000, 10_000, 50_000, 100_000]
    } else {
        vec![500, 5_000]
    }
}

/// Bodies created per corpus-construction transaction.
const CORPUS_BATCH: u64 = 2_000;

#[test]
fn commit_cost_baseline() {
    let samples = 20;
    let mut rows = Vec::new();
    for bodies in scales() {
        rows.push(measure(bodies, samples));
    }

    println!(
        "\n{:>8} {:>9} {:>9} {:>9} {:>10} {:>11} {:>13} {:>10}",
        "bodies",
        "p50 us",
        "p95 us",
        "p99 us",
        "objs/edit",
        "bytes/edit",
        "manifest B",
        "bodyquota"
    );
    for row in &rows {
        println!(
            "{:>8} {:>9} {:>9} {:>9} {:>10.1} {:>11.0} {:>13} {:>10}",
            row.bodies,
            row.edit_p50_us,
            row.edit_p95_us,
            row.edit_p99_us,
            row.objects_written_per_edit,
            row.object_bytes_per_edit,
            row.manifest_bytes,
            if row.at_protocol_ceiling {
                "ceiling"
            } else {
                "under"
            },
        );
    }
    match peak_rss_bytes() {
        Some(rss) => println!("peak RSS: {} MiB", rss / (1024 * 1024)),
        None => println!("peak RSS: unavailable on this platform"),
    }

    // This began as a recording harness whose assertions pinned the *problem*:
    // the manifest grew with the Body count and a one-Body edit rewrote it.
    // F1 made those assertions fail, which is what F1 was for, so they now
    // state the property instead — and a regression re-inflates them.
    let first = rows.first().expect("at least one scale");
    let last = rows.last().expect("at least one scale");
    assert!(
        last.object_bytes_before > first.object_bytes_before,
        "a larger store must hold more object bytes"
    );
    assert!(
        last.manifest_bytes < 4 * first.manifest_bytes,
        "the commit point must not grow with the Body count: {} bytes at {} Bodies against {} at {}",
        last.manifest_bytes,
        last.bodies,
        first.manifest_bytes,
        first.bodies
    );
    assert!(
        last.object_bytes_per_edit < 4.0 * first.object_bytes_per_edit,
        "bytes written per one-Body edit must be bounded by what changed, not by the store: {:.0} at {} Bodies against {:.0} at {}",
        last.object_bytes_per_edit,
        last.bodies,
        first.object_bytes_per_edit,
        first.bodies
    );

    if let Ok(path) = std::env::var("LAIT_COMMIT_BASELINE_REPORT") {
        let report: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "bodies": r.bodies,
                    "at_protocol_ceiling": r.at_protocol_ceiling,
                    "edit_p50_us": r.edit_p50_us as u64,
                    "edit_p95_us": r.edit_p95_us as u64,
                    "edit_p99_us": r.edit_p99_us as u64,
                    "objects_before": r.objects_before,
                    "objects_written_per_edit": r.objects_written_per_edit,
                    "object_bytes_before": r.object_bytes_before,
                    "object_bytes_per_edit": r.object_bytes_per_edit,
                    "manifest_bytes": r.manifest_bytes,
                })
            })
            .collect();
        let body = serde_json::json!({
            "peak_rss_bytes": peak_rss_bytes(),
            "platform": std::env::consts::OS,
            "scales": report,
        });
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).expect("write report");
    }
}

#[test]
fn peak_rss_is_reported_on_this_platform() {
    // F0 item 1: "reports peak RSS on every platform it runs on, not only
    // Linux." The three tier-1 targets must answer; anywhere else may not.
    let rss = peak_rss_bytes();
    if cfg!(any(target_os = "linux", windows, target_vendor = "apple")) {
        assert!(
            rss.is_some_and(|v| v > 0),
            "peak RSS must be readable on a tier-1 platform"
        );
    }
}
