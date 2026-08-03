#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "WebSocket framing validates lane lengths and sequence bounds before arithmetic; conversions adapt the versioned wire representation."
)]

//! The browser session socket: one WebSocket carrying lanes the doorbell stream cannot.
//!
//! `/api/events` already exists and is not being replaced. It carries dirty
//! flags to every tab over one broadcast ring, and that ring has exactly the
//! properties a doorbell wants: one writer, cheap fan-out, and a `Lagged` that
//! costs a rebaseline. Those are also the properties that make it the wrong
//! place for anything else. A busy upload emitting progress twice a second onto
//! that ring is a rebaseline storm for every tab watching every space, so
//! progress gets its own lane on its own socket, and **nothing here ever
//! touches `App.doorbells`**.
//!
//! Three lanes, because they fail differently.
//!
//! - **Control** must not drop. It carries facts a client acts on once, and its
//!   first producer is the Live plane's delivered signals: an invitation, a file
//!   offer, somebody asking for attention. Nothing supersedes one of those, so
//!   this lane does not share the broadcast ring the other two use — every
//!   socket gets its own queue, and a socket that fills one is let go of rather
//!   than quietly served a stream with a hole in it. The reader is dropped,
//!   never the fact.
//! - **Progress** must drop. It carries a number that is superseded by the next
//!   one, so the newest value per transfer replaces the queued one and a slow
//!   reader falls behind in staleness rather than in backlog.
//! - **Transient** must drop, for the same reason and by the same mechanism. A
//!   caret is superseded by the next caret and a facepile by the next facepile,
//!   so the ring keeps the newest and a reader that cannot keep up sees an older
//!   truth rather than a longer queue.
//!
//! Progress bodies are postcard, because that lane is the high-rate one and a
//! number should cost bytes rather than a parse. Control and Transient bodies
//! are JSON, because they carry [`crate::control::Response`] values — which
//! already have a wire form the viewer's `types.ts` mirrors. Encoding those in
//! postcard would mean hand-writing a second decoder in the browser for shapes
//! that already have one, and then keeping the two in step. The envelope is
//! postcard either way, so the version and lane checks are the same for all
//! three.
//!
//! The transient lane is fed by one native Live subscription per question,
//! never one per tab: [`pump_transient`] holds each stream browsers have declared
//! an interest in, and [`Hub`] fans the answer out. That is
//! also why this socket has an inbound direction at all. A subscription has to
//! name a Space, and subscribing to every registered one would place a Station for every
//! Orbit on the machine because somebody opened a browser.
//!
//! The upgrade is where the origin check matters most. A WebSocket handshake is
//! exempt from CORS — the browser sends it cross-origin with no preflight and
//! attaches our cookie — so `check_upgrade_origin` runs *inside* the handler,
//! and requires an Origin rather than admitting an absent one.

use runtime::poison::LockRecovering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response as HttpResponse};
use serde::{Deserialize, Serialize};

use crate::control::{Request, Response};
use crate::orbits::StationIdentity;

use super::{err_json, App, ErrorKind};

/// The session socket's own version, independent of every other version in the tree.
///
/// It guards exactly one thing: a browser tab left open across a daemon
/// restart, holding a bundle from the previous build. Both halves ship in one
/// binary, so this never negotiates — it detects, says so once, and the client
/// reloads.
pub const PROTOCOL_VERSION: u32 = 2;

/// The largest frame this build will decode.
///
/// Checked against the declared length *before* allocation. A postcard decoder
/// handed a hostile length would otherwise reserve whatever it was told to.
///
/// The same number is given to the WebSocket itself at upgrade. A check here
/// alone protects nothing: the transport assembles a whole message before
/// anything above it is asked about the length, and its own default ceiling is
/// three orders of magnitude larger than this one.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// The largest body this build will enqueue for sending.
///
/// Under the frame ceiling by the width of the envelope, because the far side
/// checks the *frame*: a body that only just fits would become a frame the
/// browser discards without saying anything about it.
const MAX_BODY_BYTES: usize = MAX_FRAME_BYTES - 16;

/// How many progress frames are held for a client that is not reading.
///
/// Small, because falling behind on progress is not a loss: the next tick
/// carries the current number, which is the only one that was ever wanted.
const PROGRESS_QUEUE: usize = 32;

/// How many transient views are held for a client that is not reading.
///
/// Smaller than the progress queue, and for a stronger reason: every view in it
/// but the last is already wrong. It absorbs a scheduling hiccup, not a backlog.
const TRANSIENT_QUEUE: usize = 8;

/// The browser socket's private inbound request gate.
struct InboundGate {
    next: Instant,
    interval: Duration,
    burst: Duration,
    strikes: u16,
}

impl InboundGate {
    fn new(now: Instant) -> Self {
        const PER_SECOND: u32 = 64;
        const BURST: u32 = 256;
        let interval = Duration::from_nanos(1_000_000_000 / u64::from(PER_SECOND));
        Self {
            next: now,
            interval,
            burst: interval * (BURST - 1),
            strikes: 0,
        }
    }

    fn should_close(&mut self, now: Instant) -> bool {
        if self.next.max(now) <= now + self.burst {
            self.next = self.next.max(now) + self.interval;
            self.strikes = self.strikes.saturating_sub(1);
            false
        } else {
            self.strikes = self.strikes.saturating_add(1);
            self.strikes >= 128
        }
    }
}

/// How far behind one socket may fall on the Control lane before the hub lets go
/// of it.
///
/// Deep, because the only way to reach it is a client that has stopped reading
/// altogether: the producer is one drain per tick of a queue the daemon already
/// bounds. A socket at this depth is not slow, it is gone — and this lane's rule
/// is that the reader is dropped, never the fact.
const CONTROL_QUEUE: usize = 256;

/// How often progress is flushed, at most.
///
/// A transfer emits progress far faster than a person can read it, and every
/// frame costs a wakeup and a render. Coalescing to the newest value per
/// transfer on a tick turns an unbounded stream into a bounded one without
/// losing the only thing anybody wants from it, which is the latest number.
const PROGRESS_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// Presence declarations and destructive signal drains remain person-scale.
/// They do not become twelve times more frequent merely because carets do.
const TRANSIENT_HOUSEKEEPING: std::time::Duration = std::time::Duration::from_secs(1);

/// Which lane a frame belongs to.
///
/// A byte on the wire rather than a string: this is the discriminant every
/// frame carries, and it is read before anything else is trusted.
///
/// **Append, never insert or reorder.** postcard encodes a variant by its
/// declaration index and ignores the explicit discriminant entirely, so
/// `Transient` is 2 because it is written third and not because it says `= 2`.
/// Putting a new lane ahead of `Progress` would renumber `Progress`, and every
/// frame already in flight would decode as a different lane with no error
/// raised anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Lane {
    Control = 0,
    Progress = 1,
    Transient = 2,
}

/// One framed message in either direction.
///
/// postcard rather than JSON because the progress lane is the high-rate one and
/// a number should cost bytes rather than a parse. The version rides every frame
/// rather than the handshake alone, so a stale tab is caught on the first frame
/// it sends instead of the first one it misinterprets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub protocol_version: u32,
    pub lane: Lane,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(lane: Lane, body: Vec<u8>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            lane,
            body,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    /// Decode a frame a browser sent, bounded before anything is allocated.
    ///
    /// The length check precedes the decode because that is the only order in
    /// which it protects anything: a decoder told a message is large has
    /// already reserved the room by the time it fails. The fields are decoded in
    /// declaration order, so a lane byte this build does not know is refused
    /// before the body's declared length is read, let alone believed.
    pub fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(Invalid::TooLarge);
        }
        let frame: Self = postcard::from_bytes(bytes).map_err(|_| Invalid::Malformed)?;
        if frame.protocol_version != PROTOCOL_VERSION {
            return Err(Invalid::WrongVersion(frame.protocol_version));
        }
        if frame.body.len() > MAX_FRAME_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(frame)
    }
}

/// Why a frame was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    TooLarge,
    Malformed,
    /// A tab from another build. Reported once, then the connection closes —
    /// there is nothing to negotiate, because both halves ship together.
    WrongVersion(u32),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "frame past the {MAX_FRAME_BYTES}-byte ceiling"),
            Self::Malformed => write!(f, "frame did not decode"),
            Self::WrongVersion(v) => write!(
                f,
                "this tab speaks session v{v} and this server speaks \
                 v{PROTOCOL_VERSION} — reload the page"
            ),
        }
    }
}

/// What one transfer looks like to a browser.
///
/// Local state, and deliberately not an Observation: progress is not something
/// peers converge on, it is something this machine is currently doing. Sending
/// it through the doorbell ring would make it durable-looking and would cost
/// every other tab a rebaseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransferProgress {
    pub transfer: String,
    pub content: String,
    pub moved: u64,
    pub total: u64,
    pub done: bool,
}

/// One control-plane value, addressed to the Space it is about.
///
/// The value is flattened rather than nested, so the body a browser receives is
/// the same `kind`-tagged object an RPC reply is and goes through the same
/// decoder. A lane that invented its own tag vocabulary would need a second
/// decoder in the browser for shapes that already have one.
#[derive(Debug, Serialize)]
struct SpaceFrame<'a> {
    space: &'a str,
    /// Which question this answers, on the Transient lane. Carried because the
    /// daemon narrows the rows to an issue but counts generations for the whole
    /// table — a tab watching one issue has to know the answer is the one it
    /// asked for. Absent on Control, where a fact answers nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    issue: Option<&'a str>,
    #[serde(flatten)]
    value: &'a Response,
}

/// One question a browser has asked to be kept up to date on.
///
/// The pair and not the Space alone: the daemon narrows rows to an issue but
/// counts generations for the whole table, so two tabs on different issues are
/// two questions sharing one counter and have to be asked separately. Two tabs
/// on the *same* issue are one question, which is what the refcount is for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Watch {
    /// The local Orbit id, resolved through the same directory an RPC resolves
    /// through — so a socket cannot reach a Space the HTTP surface would refuse.
    pub space: String,
    /// An `iss_` doc id — never a project alias, which hashes to a Body nothing
    /// publishes under and answers an empty table for ever.
    ///
    /// Absent is not the whole table. It attaches the Space to the Control lane
    /// and asks no live question: an unscoped view carries Body ids a browser
    /// cannot name, so subscribing on its behalf would be a stream that
    /// nothing can draw. A tab looking at a board is in the room and is not
    /// asking who else is on any particular issue.
    pub issue: Option<String>,
}

/// What a browser declares it wants the live view of.
///
/// A declaration and not a subscription: it replaces whatever this socket said
/// last. It arrives on the Transient lane because that is what it is — a piece
/// of state superseded by the next one, rather than a fact acted on once.
#[derive(Debug, Clone, Deserialize)]
struct WatchRequest {
    /// Absent stops the watch.
    #[serde(default)]
    space: Option<String>,
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    cursor: Option<BrowserCursor>,
    #[serde(default)]
    typing: bool,
    #[serde(default)]
    preview: Option<BrowserTextPreview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct BrowserCursor {
    field: String,
    anchor: u64,
    #[serde(default)]
    focus: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct BrowserTextPreview {
    field: String,
    base: String,
    result: String,
    index: u64,
    delete: u64,
    insert: String,
    #[serde(default)]
    anchor: Option<u64>,
    #[serde(default)]
    focus: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BrowserMutation {
    request_id: String,
    space: String,
    request: serde_json::Value,
}

const MUTATION_QUEUE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserAwareness {
    watch: Watch,
    cursor: Option<BrowserCursor>,
    typing: bool,
    preview: Option<BrowserTextPreview>,
}

/// The session socket's fan-out, held on `App`.
///
/// Separate from `App.doorbells` on purpose, and the separation is the feature:
/// one ring for the thing every tab must see, one for the thing only the tab
/// that started a transfer cares about.
pub struct Hub {
    progress: tokio::sync::broadcast::Sender<TransferProgress>,
    /// Lossy by construction, like progress and for the same reason.
    transient: tokio::sync::broadcast::Sender<Arc<Vec<u8>>>,
    /// One queue per attached socket rather than one ring shared by all of them.
    /// A broadcast ring overwrites its oldest slot under pressure, which is the
    /// right rule for a caret and the wrong one for an invitation.
    control: Mutex<Vec<tokio::sync::mpsc::Sender<Arc<Vec<u8>>>>>,
    /// What browsers have declared an interest in, refcounted so a hundred tabs
    /// asking one question cost one subscription.
    watched: Mutex<BTreeMap<Watch, usize>>,
    /// Questions somebody has just started listening to.
    ///
    /// The pump remembers a generation per *question* and the answer goes out
    /// on a ring with no replay, so a socket joining a question another socket
    /// already holds would be answered "unchanged" on its behalf and sent
    /// nothing at all — a blank rail beside a tab drawing the room correctly,
    /// until some peer happens to move. Naming the question here forces the
    /// next subscription to send the whole table and broadcast it.
    fresh: Mutex<BTreeSet<Watch>>,
    watch_wake: tokio::sync::Notify,
    next_session: AtomicU64,
    awareness: Mutex<BTreeMap<u64, BrowserAwareness>>,
    awareness_generation: AtomicU64,
    awareness_dirty: Mutex<BTreeSet<String>>,
    /// Wakes the declaration half immediately; local input need not wait for
    /// housekeeping before entering Live.
    awareness_wake: tokio::sync::Notify,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            progress: tokio::sync::broadcast::channel(PROGRESS_QUEUE).0,
            transient: tokio::sync::broadcast::channel(TRANSIENT_QUEUE).0,
            control: Mutex::new(Vec::new()),
            watched: Mutex::new(BTreeMap::new()),
            fresh: Mutex::new(BTreeSet::new()),
            watch_wake: tokio::sync::Notify::new(),
            next_session: AtomicU64::new(1),
            awareness: Mutex::new(BTreeMap::new()),
            awareness_generation: AtomicU64::new(0),
            awareness_dirty: Mutex::new(BTreeSet::new()),
            awareness_wake: tokio::sync::Notify::new(),
        }
    }

    /// Announce where a transfer has got to. Never blocks and never fails: with
    /// no browser attached there is nobody to tell, and that is not an error.
    ///
    /// No producer yet — the content routes will call it when a transfer
    /// reports progress. Kept rather than deferred so the fan-out shape is fixed
    /// before something needs it.
    #[allow(dead_code)]
    pub fn note(&self, progress: TransferProgress) {
        let _ = self.progress.send(progress);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TransferProgress> {
        self.progress.subscribe()
    }

    pub fn subscribe_transient(&self) -> tokio::sync::broadcast::Receiver<Arc<Vec<u8>>> {
        self.transient.subscribe()
    }

    /// Take this socket's own Control queue.
    ///
    /// Handed out per socket rather than subscribed to, because the lane's rule
    /// is that nothing in it is superseded: a shared ring would decide which
    /// signal to lose, and there is no right answer to that question.
    pub fn attach_control(&self) -> tokio::sync::mpsc::Receiver<Arc<Vec<u8>>> {
        let (sink, queue) = tokio::sync::mpsc::channel(CONTROL_QUEUE);
        let mut sinks = self.control.lock_recovering();
        sinks.retain(|sink| !sink.is_closed());
        sinks.push(sink);
        queue
    }

    /// Send the live view for one question to every attached socket.
    fn note_transient(&self, space: &str, issue: Option<&str>, view: &Response) {
        if let Some(body) = encode_body(space, issue, view) {
            let _ = self.transient.send(body);
        }
    }

    /// Send one fact to every attached socket, or let go of the ones that cannot
    /// take it.
    fn note_control(&self, space: &str, fact: &Response) {
        let bodies = control_bodies(space, fact);
        if bodies.is_empty() {
            return;
        }
        let mut sinks = self.control.lock_recovering();
        for body in bodies {
            // Kept only while it accepts. A closed queue is a socket that went
            // away; a full one is a socket that stopped reading, and this lane
            // drops the reader rather than the fact. Letting go of the sender
            // delivers what is already queued and then ends the socket, so
            // nothing accepted is lost.
            sinks.retain(|sink| sink.try_send(body.clone()).is_ok());
        }
    }

    fn watch(&self, question: Watch) {
        let mut watched = self.watched.lock_recovering();
        let holders = watched.entry(question.clone()).or_insert(0);
        *holders = holders.saturating_add(1);
        drop(watched);
        self.refresh(&question);
    }

    /// Say that somebody needs the whole answer to this question rather than
    /// the pump's opinion about what they already hold.
    fn refresh(&self, question: &Watch) {
        self.fresh.lock_recovering().insert(question.clone());
        self.watch_wake.notify_one();
    }

    fn take_fresh(&self) -> BTreeSet<Watch> {
        std::mem::take(&mut *self.fresh.lock_recovering())
    }

    fn unwatch(&self, question: &Watch) {
        let mut watched = self.watched.lock_recovering();
        if let Some(holders) = watched.get_mut(question) {
            *holders = holders.saturating_sub(1);
            if *holders == 0 {
                watched.remove(question);
            }
        }
        drop(watched);
        self.watch_wake.notify_one();
    }

    fn watched(&self) -> Vec<Watch> {
        self.watched.lock_recovering().keys().cloned().collect()
    }

    fn attach_session(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    fn set_awareness(&self, session: u64, awareness: Option<BrowserAwareness>) {
        let mut held = self.awareness.lock_recovering();
        let previous_space = held.get(&session).map(|held| held.watch.space.clone());
        let next_space = awareness.as_ref().map(|held| held.watch.space.clone());
        let changed = match awareness {
            Some(awareness) => held.insert(session, awareness.clone()).as_ref() != Some(&awareness),
            None => held.remove(&session).is_some(),
        };
        if changed {
            drop(held);
            let mut dirty = self.awareness_dirty.lock_recovering();
            if let Some(space) = previous_space {
                dirty.insert(space);
            }
            if let Some(space) = next_space {
                dirty.insert(space);
            }
            self.awareness_generation.fetch_add(1, Ordering::SeqCst);
            self.awareness_wake.notify_one();
        }
    }

    fn awareness_generation(&self) -> u64 {
        self.awareness_generation.load(Ordering::SeqCst)
    }

    fn take_awareness_spaces(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.awareness_dirty.lock_recovering())
    }

    fn awareness(&self, space: &str) -> Vec<BrowserAwareness> {
        self.awareness
            .lock_recovering()
            .values()
            .filter(|awareness| awareness.watch.space == space)
            .cloned()
            .collect()
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// One Control-lane fact, as the frames that will carry it.
///
/// A batch too large for one frame is split rather than dropped. By the time
/// this runs the signals are already out of the daemon's queue and nothing
/// holds a second copy, so returning nothing would destroy exactly what this
/// lane exists to guarantee — and it would do it silently, since an oversize
/// frame costs no socket and raises no counter.
///
/// A lone signal that still will not fit is unrecoverable, so what is sent is
/// the fact that it happened: an empty drain carrying the loss. `dropped` is on
/// the reply precisely so a client that lost an invitation can say so.
fn control_bodies(space: &str, fact: &Response) -> Vec<Arc<Vec<u8>>> {
    if let Some(body) = encode_body(space, None, fact) {
        return vec![body];
    }
    let Response::Signals { signals, dropped } = fact else {
        return Vec::new();
    };
    if signals.len() < 2 {
        let lost = Response::Signals {
            signals: Vec::new(),
            dropped: dropped.saturating_add(signals.len() as u64),
        };
        return encode_body(space, None, &lost).into_iter().collect();
    }
    let (first, rest) = signals.split_at(signals.len() / 2);
    let mut bodies = control_bodies(
        space,
        &Response::Signals {
            signals: first.to_vec(),
            dropped: *dropped,
        },
    );
    // The count rides the first half alone. It is one number about the whole
    // batch, and repeating it on both halves would report every loss twice.
    bodies.extend(control_bodies(
        space,
        &Response::Signals {
            signals: rest.to_vec(),
            dropped: 0,
        },
    ));
    bodies
}

/// Encode one body once, for every socket that will receive it.
///
/// `None` rather than a truncated frame when it does not fit: the far side
/// checks the frame length and discards an oversize one without a word, so
/// sending it would be indistinguishable from sending nothing except in cost.
fn encode_body(space: &str, issue: Option<&str>, value: &Response) -> Option<Arc<Vec<u8>>> {
    let body = serde_json::to_vec(&SpaceFrame {
        space,
        issue,
        value,
    })
    .ok()?;
    if body.len() > MAX_BODY_BYTES {
        tracing::warn!(
            space,
            bytes = body.len(),
            "a session frame past the ceiling was not sent"
        );
        return None;
    }
    Some(Arc::new(body))
}

/// Poll the live view and drain delivered signals for every declared question,
/// through one native stream per question, until the server stops.
///
/// One subscription per *question* rather than one per socket: a hundred tabs on one
/// issue are one `live` request, because the hub fans the answer out. A question
/// nobody holds is not asked at all, which is what keeps this from placing a
/// Station for every Orbit on the machine.
///
/// The two halves run off different things. `live` streams per question, so a
/// declaration that names no issue costs no read. The drain is per *Space*, so
/// it runs for a tab on a board as much as for a tab on an issue: the Live
/// plane's queue is bounded and overwrites its oldest, and gating a lane that
/// must not drop on a lane that may would destroy invitations for want of a
/// facepile nobody asked for.
///
/// Draining is the sharper edge. `signals` empties the queue, so while a browser
/// is watching a Space this pump is the only thing that may drain it — a second
/// drainer would take half the set and neither would see the whole. An agent's
/// Space is skipped for that reason: it is observable here and not operable, and
/// taking its signals out from under it because somebody left a tab open is
/// exactly what `rpc` refuses at the door.
pub(super) async fn pump_transient(app: Arc<App>, mut stop: tokio::sync::watch::Receiver<bool>) {
    // Read before the loop, not just selected on inside it. `subscribe()` marks
    // the current value as seen, so a task that starts after the stop has been
    // latched would otherwise wait forever on a change that already happened.
    if *stop.borrow_and_update() {
        return;
    }
    let mut streams: BTreeMap<Watch, tokio::task::JoinHandle<()>> = BTreeMap::new();
    let mut declared_awareness = u64::MAX;
    let mut housekeeping = tokio::time::interval(TRANSIENT_HOUSEKEEPING);
    housekeeping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        enum Wake {
            Awareness,
            Watches,
            Housekeeping,
        }
        let wake = tokio::select! {
            biased;
            _ = stop.changed() => break,
            _ = app.socket.awareness_wake.notified() => Wake::Awareness,
            _ = app.socket.watch_wake.notified() => Wake::Watches,
            _ = housekeeping.tick() => Wake::Housekeeping,
        };
        let awareness = app.socket.awareness_generation();
        if awareness != declared_awareness {
            declare_all_watching(&app).await;
            declared_awareness = awareness;
        }
        if matches!(wake, Wake::Watches | Wake::Housekeeping) {
            reconcile_live_streams(&app, &mut streams);
        }
        if matches!(wake, Wake::Housekeeping) {
            tokio::select! {
                biased;
                _ = stop.changed() => return,
                () = sweep_housekeeping(&app) => {}
            }
        }
    }
    for (_, task) in streams {
        task.abort();
    }
}

fn reconcile_live_streams(
    app: &Arc<App>,
    streams: &mut BTreeMap<Watch, tokio::task::JoinHandle<()>>,
) {
    let fresh = app.socket.take_fresh();
    let watched: BTreeSet<_> = app
        .socket
        .watched()
        .into_iter()
        .filter(|question| question.issue.is_some())
        .collect();
    let removed: Vec<_> = streams
        .keys()
        .filter(|question| !watched.contains(*question) || fresh.contains(*question))
        .cloned()
        .collect();
    for question in removed {
        if let Some(task) = streams.remove(&question) {
            task.abort();
        }
    }
    for question in watched {
        if streams.contains_key(&question) {
            continue;
        }
        let task_app = app.clone();
        let task_question = question.clone();
        streams.insert(
            question,
            tokio::spawn(async move { stream_live(task_app, task_question).await }),
        );
    }
}

async fn stream_live(app: Arc<App>, question: Watch) {
    let mut backoff = Duration::from_millis(100);
    loop {
        let Ok(resolved) = app.directory.resolve(&question.space) else {
            return;
        };
        let route = crate::control::station_route(resolved.address);
        match app
            .daemon
            .subscribe_live(route, question.issue.clone())
            .await
        {
            Ok(mut subscription) => {
                backoff = Duration::from_millis(100);
                loop {
                    match subscription.next().await {
                        Ok(Some(view @ Response::Live { .. })) => app.socket.note_transient(
                            &question.space,
                            question.issue.as_deref(),
                            &view,
                        ),
                        Ok(Some(other)) => {
                            tracing::debug!(?other, "Live subscription returned a non-view");
                            break;
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            }
            Err(error) => tracing::debug!(%error, "Live subscription did not open"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(2));
    }
}

async fn sweep_housekeeping(app: &App) {
    let watched = app.socket.watched();
    let mut spaces: BTreeSet<&str> = BTreeSet::new();
    for question in &watched {
        spaces.insert(question.space.as_str());
    }
    for space in spaces {
        drain_signals(app, space).await;
    }
}

async fn declare_all_watching(app: &App) {
    let watched = app.socket.watched();
    let spaces = app.socket.take_awareness_spaces();
    for space in spaces {
        declare_watching(app, &space, &watched).await;
    }
}

/// Tell the engine what this node is looking at, so peers can draw a face.
///
/// Derived from the same declarations the transient subscriptions run on rather than
/// from a separate client verb. A tab that is asking about an issue *is*
/// looking at it, so there is nothing for a browser to say twice and nothing
/// for the two answers to disagree about.
///
/// A declaration with no issue is a tab saying which room it is in, and it
/// publishes no presence: being in a Space is not being on a document, and
/// treating it as one would put every open tab on every issue at once.
async fn declare_watching(app: &App, space: &str, watched: &[Watch]) {
    let Ok(resolved) = app.directory.resolve(space) else {
        return;
    };
    let issues: Vec<String> = watched
        .iter()
        .filter(|question| question.space == space)
        .filter_map(|question| question.issue.clone())
        .collect();
    let mut carets = Vec::new();
    let mut typing = Vec::new();
    let mut previews = Vec::new();
    for awareness in app.socket.awareness(space) {
        let Some(issue) = awareness.watch.issue else {
            continue;
        };
        if let Some(cursor) = awareness.cursor {
            carets.push(crate::control::WatchingCaret {
                issue: issue.clone(),
                field: cursor.field.clone(),
                anchor: cursor.anchor,
                focus: cursor.focus,
            });
        }
        if awareness.typing {
            typing.push(crate::control::WatchingTyping {
                issue: issue.clone(),
                field: "description".into(),
            });
        }
        if let Some(preview) = awareness.preview {
            previews.push(crate::control::WatchingPreview {
                issue,
                field: preview.field,
                base: preview.base,
                result: preview.result,
                index: preview.index,
                delete: preview.delete,
                insert: preview.insert,
                anchor: preview.anchor,
                focus: preview.focus,
            });
        }
    }
    let route = crate::control::station_route(resolved.address);
    // Failure is not reported. The declaration is lossy by nature — the next
    // tick carries the current set again — and a pump that logged every miss
    // would say the same thing twice a second while a daemon restarted.
    let _ = app
        .daemon
        .request(
            route,
            &Request::Watching {
                issues,
                carets,
                typing,
                previews,
            },
            None,
        )
        .await;
}

async fn drain_signals(app: &App, space: &str) {
    let Ok(resolved) = app.directory.resolve(space) else {
        return;
    };
    if matches!(resolved.identity, StationIdentity::Agent { .. }) {
        return;
    }
    let route = crate::control::station_route(resolved.address);
    match app.daemon.request(route, &Request::Signals, None).await {
        Ok(Response::Signals { signals, dropped }) => {
            // An empty drain is the normal answer and says nothing anybody has
            // to act on, so it costs no frame.
            if signals.is_empty() && dropped == 0 {
                return;
            }
            app.socket
                .note_control(space, &Response::Signals { signals, dropped });
        }
        answer => {
            tracing::debug!(space, ?answer, "signals were not drained");
        }
    }
}

/// `GET /api/session` — the upgrade.
///
/// The shared gate has already checked the credential and the Host. This adds
/// the one check the gate cannot make on its behalf, because it is only correct
/// for upgrades: Origin is required here, where elsewhere an absent one is a
/// non-browser client and perfectly fine.
pub(super) async fn session(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> HttpResponse {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if let Err(refusal) = app.guard.check_upgrade_origin(host, origin) {
        return (
            StatusCode::FORBIDDEN,
            err_json(refusal.reason(), ErrorKind::Error),
        )
            .into_response();
    }
    upgrade
        // The transport's ceiling, not just the decoder's. Left at its default
        // it assembles a 64 MiB message and this side copies it before
        // `Frame::decode` is ever asked about the length, so the check
        // that exists to bound an allocation runs after two of them.
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| serve_socket(socket, app))
}

/// One connected browser, until it goes away or the server stops.
async fn serve_socket(socket: WebSocket, app: Arc<App>) {
    let mut watching: Option<Watch> = None;
    let session = app.socket.attach_session();
    let (mutation_tx, mutation_rx) = tokio::sync::mpsc::channel(MUTATION_QUEUE);
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(MUTATION_QUEUE);
    let worker = tokio::spawn(run_mutations(app.clone(), mutation_rx, reply_tx));
    run_socket(
        socket,
        &app,
        session,
        &mut watching,
        mutation_tx,
        &mut reply_rx,
    )
    .await;
    worker.abort();
    // Every exit lands here, including the several that leave by returning. A
    // declaration outliving its socket would keep the pump asking a question
    // nobody is listening to the answer of.
    if let Some(question) = watching {
        app.socket.unwatch(&question);
    }
    app.socket.set_awareness(session, None);
}

async fn run_socket(
    mut socket: WebSocket,
    app: &App,
    session: u64,
    watching: &mut Option<Watch>,
    mutation_tx: tokio::sync::mpsc::Sender<BrowserMutation>,
    mutation_replies: &mut tokio::sync::mpsc::Receiver<Arc<Vec<u8>>>,
) {
    // One task owns the socket and both directions. Splitting it would need a
    // stream-combinator dependency for the two halves, and buys nothing here:
    // this connection sends on a tick and receives rarely, so there is no
    // concurrency to recover. `recv()` is a framed read, so dropping it when
    // another `select!` branch wins leaves any partial frame in the codec's
    // buffer rather than losing it.
    let mut progress = app.socket.subscribe();
    let mut transient = app.socket.subscribe_transient();
    let mut control = app.socket.attach_control();
    let mut stop = app.stop.subscribe();
    // Read before the loop for the same reason the pump does: a socket accepted
    // in the window between the stop and the listener closing would otherwise
    // select on a change that has already happened.
    if *stop.borrow_and_update() {
        return;
    }
    let mut inbound = InboundGate::new(Instant::now());

    // Coalesced by transfer id: the newest number for a transfer replaces the
    // one waiting to be sent, because the older one is not stale data, it is
    // wrong data. Flushed on a tick so a fast transfer costs one frame per tick
    // rather than one per chunk.
    let mut pending: BTreeMap<String, TransferProgress> = BTreeMap::new();
    let mut tick = tokio::time::interval(PROGRESS_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;

            _ = stop.changed() => break,

            // Ahead of every other lane, because that is what "does not drop"
            // has to mean in a loop that can send one frame at a time: a fact
            // with no successor must not wait behind a view that has one.
            fact = control.recv() => match fact {
                Some(body) => {
                    if send(&mut socket, Lane::Control, &body).await.is_err() {
                        return;
                    }
                }
                // The hub let go of this socket's queue, which it does only for
                // a socket that stopped reading. What was already accepted has
                // just been delivered, so closing is the honest end.
                None => break,
            },

            reply = mutation_replies.recv() => match reply {
                Some(body) => {
                    if send(&mut socket, Lane::Control, &body).await.is_err() {
                        return;
                    }
                }
                None => break,
            },

            view = transient.recv() => match view {
                Ok(body) => {
                    if send(&mut socket, Lane::Transient, &body).await.is_err() {
                        return;
                    }
                }
                // Falling behind on a view costs nothing to report and
                // everything to ignore. Restarting this question's subscription
                // makes its initial full snapshot supersede what was lost.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if let Some(question) = watching.as_ref() {
                        app.socket.refresh(question);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },

            update = progress.recv() => match update {
                Ok(update) => {
                    pending.insert(update.transfer.clone(), update);
                }
                // Falling behind on progress is not an event worth reporting:
                // the next tick carries the current number, which is the only
                // one that was ever wanted.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },

            _ = tick.tick() => {
                for (_, update) in std::mem::take(&mut pending) {
                    let body = match postcard::to_stdvec(&update) {
                        Ok(body) => body,
                        Err(_) => continue,
                    };
                    if send(&mut socket, Lane::Progress, &body).await.is_err() {
                        return;
                    }
                }
            }

            incoming = next_message(&mut socket) => match incoming {
                Some(Ok(bytes)) => {
                    // Inbound is paced. A browser is a local client, but a local
                    // client with a bug is still a client that can spin.
                    if inbound.should_close(Instant::now()) {
                        return;
                    }
                    match Frame::decode(&bytes) {
                        Ok(frame) if frame.lane == Lane::Transient => {
                            let Ok(declared) =
                                serde_json::from_slice::<WatchRequest>(&frame.body)
                            else {
                                // Both halves ship in one binary and the version
                                // has already been checked, so a body that does
                                // not decode is the same kind of wrong as a
                                // frame that does not.
                                return;
                            };
                            declare(app, session, watching, declared);
                        }
                        Ok(frame) if frame.lane == Lane::Control => {
                            let Ok(request) = serde_json::from_slice::<BrowserMutation>(&frame.body)
                            else {
                                return;
                            };
                            if request.request_id.is_empty()
                                || request.request_id.len() > 64
                                || mutation_tx.try_send(request).is_err()
                            {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(Invalid::WrongVersion(v)) => {
                            let _ = socket
                                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                                    code: 1002,
                                    reason: Invalid::WrongVersion(v).to_string().into(),
                                })))
                                .await;
                            return;
                        }
                        Err(_) => return,
                    }
                }
                Some(Err(_)) | None => return,
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

async fn run_mutations(
    app: Arc<App>,
    mut requests: tokio::sync::mpsc::Receiver<BrowserMutation>,
    replies: tokio::sync::mpsc::Sender<Arc<Vec<u8>>>,
) {
    while let Some(request) = requests.recv().await {
        let (status, response) =
            super::socket_editor_rpc(app.clone(), request.space.clone(), request.request).await;
        let body = serde_json::json!({
            "kind": "mutation",
            "space": request.space,
            "request_id": request.request_id,
            "ok": status.is_success(),
            "status": status.as_u16(),
            "response": response,
        });
        let Ok(encoded) = serde_json::to_vec(&body) else {
            continue;
        };
        if encoded.len() > MAX_BODY_BYTES || replies.send(Arc::new(encoded)).await.is_err() {
            return;
        }
    }
}

/// Replace this socket's standing declaration.
fn declare(app: &App, session: u64, watching: &mut Option<Watch>, declared: WatchRequest) {
    let next = declared.space.map(|space| Watch {
        space,
        issue: declared.issue.clone(),
    });
    let awareness = next.clone().map(|watch| BrowserAwareness {
        watch,
        cursor: declared.cursor,
        typing: declared.typing,
        preview: declared.preview,
    });
    app.socket.set_awareness(session, awareness);
    if next == *watching {
        return;
    }
    // Released before the new one is taken, so a socket moving between two
    // questions cannot leave the pump asking the one it has left. Taking the
    // new one marks it for a whole answer, because this socket has never been
    // sent one and the pump's memory of it belongs to whoever asked first.
    if let Some(previous) = watching.take() {
        app.socket.unwatch(&previous);
    }
    if let Some(question) = next {
        app.socket.watch(question.clone());
        *watching = Some(question);
    }
}

async fn send(socket: &mut WebSocket, lane: Lane, body: &[u8]) -> Result<(), ()> {
    let encoded = Frame::new(lane, body.to_vec()).encode().map_err(|_| ())?;
    socket
        .send(Message::Binary(encoded.into()))
        .await
        .map_err(|_| ())
}

async fn next_message(socket: &mut WebSocket) -> Option<Result<Vec<u8>, ()>> {
    loop {
        return match socket.recv().await {
            Some(Ok(Message::Binary(bytes))) => Some(Ok(bytes.to_vec())),
            // A text frame is not something this protocol has; a browser sending
            // one is confused about what it is talking to.
            Some(Ok(Message::Text(_))) => Some(Err(())),
            Some(Ok(Message::Close(_))) | None => None,
            // Ping and Pong are the transport's own business.
            Some(Ok(_)) => continue,
            Some(Err(_)) => Some(Err(())),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal_fact(nonce: &str) -> Response {
        Response::Signals {
            signals: vec![crate::control::SignalEntry {
                actor: "act_a".into(),
                connection_id: "0".repeat(32),
                connection_epoch: "1".repeat(32),
                signal: crate::control::SignalBody::Ping {
                    nonce: nonce.into(),
                },
            }],
            dropped: 0,
        }
    }

    fn live_view(generation: u64) -> Response {
        Response::Live {
            generation,
            partial: false,
            entries: Vec::new(),
        }
    }

    fn body_of(bytes: &Arc<Vec<u8>>) -> serde_json::Value {
        serde_json::from_slice(bytes).expect("a session body is JSON")
    }

    #[test]
    fn a_frame_past_the_ceiling_is_refused_before_it_is_decoded() {
        // The order is the whole point: a decoder told a message is large has
        // already reserved the room by the time it fails.
        let oversize = vec![0u8; MAX_FRAME_BYTES + 1];
        assert_eq!(Frame::decode(&oversize), Err(Invalid::TooLarge));
    }

    #[test]
    fn a_frame_whose_body_is_oversize_is_refused_even_if_the_frame_is_not() {
        // postcard encodes a length prefix, so a small envelope can still
        // declare a large body. Both are checked.
        let frame = Frame {
            protocol_version: PROTOCOL_VERSION,
            lane: Lane::Control,
            body: vec![7u8; MAX_FRAME_BYTES + 1],
        };
        assert_eq!(
            Frame::decode(&frame.encode().expect("encode test frame")),
            Err(Invalid::TooLarge)
        );
    }

    #[test]
    fn a_lane_this_build_does_not_know_is_refused_before_a_byte_is_reserved() {
        // Hand-built, because this build cannot encode a lane it does not have.
        // The frame is tiny and its declared body is enormous: the ceiling check
        // at the top cannot be what refuses it, and the length is never believed
        // because the lane is judged first. postcard reads the struct's fields in
        // declaration order — version, lane, body — so an unknown variant index
        // fails the decode before the body's length varint is even read.
        let unknown_lane = 7u8;
        let mut hostile = vec![PROTOCOL_VERSION as u8, unknown_lane];
        hostile.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x07]);
        assert!(hostile.len() < MAX_FRAME_BYTES);
        assert_eq!(Frame::decode(&hostile), Err(Invalid::Malformed));
    }

    #[test]
    fn a_tab_from_another_build_is_told_once_and_not_negotiated_with() {
        let stale = Frame {
            protocol_version: PROTOCOL_VERSION + 1,
            lane: Lane::Control,
            body: Vec::new(),
        };
        let refusal = Frame::decode(&stale.encode().expect("encode stale frame"));
        assert_eq!(refusal, Err(Invalid::WrongVersion(PROTOCOL_VERSION + 1)));
        assert!(
            refusal.unwrap_err().to_string().contains("reload"),
            "the refusal has to say what to do about it"
        );
    }

    #[test]
    fn a_frame_round_trips_on_every_lane() {
        for lane in [Lane::Control, Lane::Progress, Lane::Transient] {
            let frame = Frame::new(lane, vec![1, 2, 3]);
            assert_eq!(
                Frame::decode(&frame.encode().expect("encode round-trip frame")),
                Ok(frame)
            );
        }
    }

    #[test]
    fn appending_a_lane_left_the_two_before_it_where_they_were() {
        // postcard writes the *declaration index* and ignores `= 0` / `= 1`
        // entirely, so this is the only thing that catches an insertion: a lane
        // added ahead of Progress renumbers it, and every frame in flight
        // decodes as a different lane with nothing raised anywhere.
        for (lane, byte) in [
            (Lane::Control, 0u8),
            (Lane::Progress, 1),
            (Lane::Transient, 2),
        ] {
            let encoded = Frame::new(lane, Vec::new())
                .encode()
                .expect("encode lane frame");
            assert_eq!(
                encoded[1], byte,
                "{lane:?} moved on the wire; old frames now decode as another lane"
            );
        }
    }

    #[test]
    fn garbage_is_malformed_rather_than_panicking() {
        for bytes in [&b""[..], &b"\xff\xff\xff\xff"[..], &b"not postcard"[..]] {
            assert!(matches!(
                Frame::decode(bytes),
                Err(Invalid::Malformed) | Err(Invalid::WrongVersion(_))
            ));
        }
    }

    #[test]
    fn progress_coalesces_to_the_newest_value_per_transfer() {
        // The property the Progress lane exists for. An older number for a
        // transfer is not stale data, it is wrong data — so it is replaced
        // rather than queued behind.
        let mut pending: BTreeMap<String, TransferProgress> = BTreeMap::new();
        for moved in [10u64, 20, 30] {
            pending.insert(
                "t1".into(),
                TransferProgress {
                    transfer: "t1".into(),
                    content: "c".into(),
                    moved,
                    total: 100,
                    done: false,
                },
            );
        }
        pending.insert(
            "t2".into(),
            TransferProgress {
                transfer: "t2".into(),
                content: "c2".into(),
                moved: 5,
                total: 50,
                done: false,
            },
        );
        assert_eq!(pending.len(), 2, "one entry per transfer, not per update");
        assert_eq!(pending["t1"].moved, 30, "the newest number wins");
    }

    #[test]
    fn progress_never_rides_the_doorbell_ring() {
        // Structural rather than behavioural, and deliberately so: no code in
        // this module may reach `App.doorbells`. One `Lagged` on that ring costs
        // every tab a full rebaseline, and a busy upload would produce them by
        // the hundred — but that is a cost nothing here would *observe*, so no
        // behavioural test would catch someone wiring it up later.
        //
        // Comments are stripped first, because this file discusses the doorbell
        // ring at length in order to explain why it is not used.
        assert!(
            !code_before_tests().contains("doorbells"),
            "the session socket reached for the doorbell ring"
        );
    }

    #[test]
    fn the_control_lane_is_read_before_every_lane_that_may_drop() {
        // The ordering is invisible from outside: a socket cannot be built
        // without a live HTTP upgrade, and a test that rebuilt the `select!`
        // would stay green through exactly the reorder it exists to catch. So
        // the real one is read.
        //
        // What it guards: `biased` makes the arms a priority list, and a fact
        // with no successor must not wait behind a view that has one.
        let code = code_before_tests();
        let loop_body = code
            .split_once("async fn run_socket")
            .and_then(|(_, socket)| socket.split_once("biased;"))
            .map(|(_, body)| body)
            .expect("the socket loop selects biased");
        let control = loop_body.find("control.recv()").expect("the Control arm");
        for lossy in ["transient.recv()", "progress.recv()", "tick.tick()"] {
            let at = loop_body.find(lossy).expect(lossy);
            assert!(
                control < at,
                "{lossy} is polled before the Control lane, which may not drop"
            );
        }
    }

    #[test]
    fn the_upgrade_bounds_a_message_before_the_decoder_is_asked_about_it() {
        // Invisible from outside: a socket cannot be built without a live HTTP
        // upgrade, so no behavioural test here would notice the ceiling coming
        // off again.
        //
        // What it guards: the transport's default is 64 MiB, assembled whole
        // and then copied, before `Frame::decode` is handed a length to
        // check. A bound that runs after two allocations is not a bound.
        let code = code_before_tests();
        for guard in [
            ".max_message_size(MAX_FRAME_BYTES)",
            ".max_frame_size(MAX_FRAME_BYTES)",
        ] {
            assert!(code.contains(guard), "the upgrade lost {guard}");
        }
    }

    /// This file with its comment lines removed, up to the test module.
    fn code_before_tests() -> String {
        let source = include_str!("socket.rs");
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        code.split_once("mod tests")
            .map(|(before, _)| before.to_string())
            .unwrap_or(code)
    }

    #[tokio::test]
    async fn a_hub_with_no_browser_attached_is_not_an_error() {
        let hub = Hub::new();
        hub.note(TransferProgress {
            transfer: "t".into(),
            content: "c".into(),
            moved: 1,
            total: 2,
            done: false,
        });
        let mut watcher = hub.subscribe();
        hub.note(TransferProgress {
            transfer: "t".into(),
            content: "c".into(),
            moved: 2,
            total: 2,
            done: true,
        });
        let seen = watcher
            .recv()
            .await
            .expect("a subscriber sees what follows");
        assert!(seen.done);
    }

    #[tokio::test]
    async fn a_transient_flood_does_not_delay_a_control_frame() {
        // The two lanes share a socket and nothing else. A view arriving at full
        // rate occupies its own ring, so a signal is available to the socket
        // immediately rather than after the backlog ahead of it has drained.
        let hub = Hub::new();
        let mut views = hub.subscribe_transient();
        let mut facts = hub.attach_control();

        for generation in 0..(TRANSIENT_QUEUE as u64 * 100) {
            hub.note_transient("orb_a", Some("iss_1"), &live_view(generation));
        }
        hub.note_control("orb_a", &signal_fact("aa"));

        let fact = facts.try_recv().expect("the fact is there to be taken");
        assert_eq!(body_of(&fact)["kind"], "signals");
        assert!(
            matches!(
                views.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
            ),
            "the flood was supposed to overrun its own ring, not the other one"
        );
    }

    #[tokio::test]
    async fn a_control_frame_survives_pressure_that_a_transient_one_does_not() {
        // The rule, stated as an experiment: the same number of frames onto both
        // lanes, with nobody reading either, and then a look at what is left. A
        // caret superseded by the next caret has lost nothing; an invitation
        // dropped to make room for a ping is gone.
        let hub = Hub::new();
        let mut views = hub.subscribe_transient();
        let mut facts = hub.attach_control();
        let pressure = TRANSIENT_QUEUE * 4;
        assert!(pressure < CONTROL_QUEUE);

        for n in 0..pressure {
            hub.note_transient("orb_a", None, &live_view(n as u64));
            hub.note_control("orb_a", &signal_fact(&format!("{n:02}")));
        }

        for n in 0..pressure {
            let fact = facts.try_recv().expect("every fact is still queued");
            assert_eq!(
                body_of(&fact)["signals"][0]["signal"]["nonce"],
                format!("{n:02}"),
                "the Control lane kept order as well as content"
            );
        }

        assert!(
            matches!(
                views.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
            ),
            "the Transient lane was supposed to drop what it could not hold"
        );
        let mut left = Vec::new();
        while let Ok(view) = views.try_recv() {
            left.push(body_of(&view)["generation"].as_u64().expect("generation"));
        }
        assert_eq!(
            left.len(),
            TRANSIENT_QUEUE,
            "the ring holds its depth and no more"
        );
        assert_eq!(
            left.last().copied(),
            Some(pressure as u64 - 1),
            "what it keeps is the newest, which is the only one that is right"
        );
    }

    #[tokio::test]
    async fn a_socket_that_stops_reading_the_control_lane_is_let_go_of() {
        // The other half of "must not drop": there is no third option. Either
        // the fact is dropped or the reader is, and dropping the reader is the
        // one a client can notice and recover from.
        let hub = Hub::new();
        let mut facts = hub.attach_control();
        for n in 0..(CONTROL_QUEUE + 1) {
            hub.note_control("orb_a", &signal_fact(&format!("{n:04}")));
        }
        for _ in 0..CONTROL_QUEUE {
            facts.try_recv().expect("everything accepted is delivered");
        }
        assert!(
            facts.recv().await.is_none(),
            "the queue ends rather than resuming with a hole in it"
        );
    }

    #[test]
    fn a_body_carries_the_space_and_the_question_it_answers() {
        let body = encode_body("orb_a", Some("iss_1"), &live_view(41)).expect("encoded");
        let value = body_of(&body);
        assert_eq!(value["space"], "orb_a");
        assert_eq!(value["issue"], "iss_1");
        assert_eq!(value["kind"], "live");
        assert_eq!(value["generation"], 41);

        // The whole table is the absence of a question, not an empty one.
        let unscoped = encode_body("orb_a", None, &live_view(41)).expect("encoded");
        assert!(body_of(&unscoped).get("issue").is_none());
    }

    #[test]
    fn a_body_past_the_ceiling_is_not_sent_at_all() {
        // The far side checks the frame length and discards an oversize one
        // without a word, so sending it costs bytes and buys nothing.
        let huge = Response::Text {
            text: "x".repeat(MAX_FRAME_BYTES),
        };
        assert!(encode_body("orb_a", None, &huge).is_none());
    }

    #[test]
    fn one_question_held_by_two_tabs_is_subscribed_once() {
        let hub = Hub::new();
        let question = Watch {
            space: "orb_a".into(),
            issue: Some("iss_1".into()),
        };
        hub.watch(question.clone());
        hub.watch(question.clone());
        assert_eq!(hub.watched(), vec![question.clone()]);

        hub.unwatch(&question);
        assert_eq!(
            hub.watched(),
            vec![question.clone()],
            "one tab leaving does not stop the other one's question"
        );
        hub.unwatch(&question);
        assert!(
            hub.watched().is_empty(),
            "a question nobody holds is not asked"
        );
    }

    #[test]
    fn two_issues_in_one_space_are_two_questions() {
        // The daemon narrows the rows to an issue but counts generations for the
        // whole table, so these cannot share a subscription: one would be told
        // "unchanged" about rows it has never seen.
        let hub = Hub::new();
        for issue in ["iss_1", "iss_2"] {
            hub.watch(Watch {
                space: "orb_a".into(),
                issue: Some(issue.into()),
            });
        }
        assert_eq!(hub.watched().len(), 2);
    }

    #[test]
    fn browser_awareness_is_replace_all_and_marks_both_rooms_when_moved() {
        let hub = Hub::new();
        let session = hub.attach_session();
        let first = BrowserAwareness {
            watch: Watch {
                space: "orb_a".into(),
                issue: Some("iss_1".into()),
            },
            cursor: Some(BrowserCursor {
                field: "description".into(),
                anchor: 4,
                focus: Some(9),
            }),
            typing: true,
            preview: None,
        };
        hub.set_awareness(session, Some(first.clone()));
        let generation = hub.awareness_generation();
        assert_eq!(hub.awareness("orb_a"), vec![first.clone()]);
        assert_eq!(
            hub.take_awareness_spaces(),
            BTreeSet::from(["orb_a".into()])
        );

        hub.set_awareness(session, Some(first));
        assert_eq!(hub.awareness_generation(), generation, "an echo is cheap");
        assert!(hub.take_awareness_spaces().is_empty());

        hub.set_awareness(
            session,
            Some(BrowserAwareness {
                watch: Watch {
                    space: "orb_b".into(),
                    issue: Some("iss_2".into()),
                },
                cursor: None,
                typing: false,
                preview: None,
            }),
        );
        assert_eq!(
            hub.take_awareness_spaces(),
            BTreeSet::from(["orb_a".into(), "orb_b".into()]),
            "the old Station must be told to retire its publication too"
        );
    }

    #[test]
    fn a_second_tab_on_a_held_question_is_owed_the_whole_answer() {
        // The generation is remembered per question and the answer goes out on
        // a ring with no replay. Without this the joining tab is answered
        // "unchanged" on the strength of what the *first* tab holds, receives
        // nothing, and draws an empty rail beside one showing the room.
        let hub = Hub::new();
        let question = Watch {
            space: "orb_a".into(),
            issue: Some("iss_1".into()),
        };
        hub.watch(question.clone());
        assert!(hub.take_fresh().contains(&question));
        assert!(
            hub.take_fresh().is_empty(),
            "a taken mark is spent, or every subscription restarts for ever"
        );

        hub.watch(question.clone());
        assert!(
            hub.take_fresh().contains(&question),
            "the second holder has never been sent a view either"
        );
    }

    #[test]
    fn a_declaration_with_no_issue_is_a_room_and_not_a_question() {
        // What the Control lane runs on. A tab on a board declares its Space
        // and asks nothing, so the drain reaches it — the alternative is
        // signals destroyed for want of a facepile nobody asked for.
        let hub = Hub::new();
        let room = Watch {
            space: "orb_a".into(),
            issue: None,
        };
        hub.watch(room.clone());
        assert_eq!(hub.watched(), vec![room]);
    }

    #[test]
    fn a_drain_too_large_for_one_frame_is_split_rather_than_lost() {
        // By the time this runs the signals are out of the daemon's queue and
        // nothing holds a second copy, so "did not fit" and "never happened"
        // would be the same outcome.
        let fat = |n: usize| crate::control::SignalEntry {
            actor: "act_a".into(),
            connection_id: "0".repeat(32),
            connection_epoch: "1".repeat(32),
            signal: crate::control::SignalBody::Ping {
                nonce: format!("{n:04}").repeat(4_000),
            },
        };
        let batch = Response::Signals {
            signals: (0..8).map(fat).collect(),
            dropped: 3,
        };
        assert!(
            encode_body("orb_a", None, &batch).is_none(),
            "the batch has to be one this lane cannot frame whole"
        );

        let bodies = control_bodies("orb_a", &batch);
        assert!(bodies.len() > 1, "it was split");
        let mut carried = 0usize;
        let mut lost = 0u64;
        for body in &bodies {
            assert!(body.len() <= MAX_BODY_BYTES);
            let value = body_of(body);
            carried += value["signals"].as_array().expect("signals").len();
            lost += value["dropped"].as_u64().expect("dropped");
        }
        assert_eq!(carried, 8, "every signal is on some frame");
        assert_eq!(lost, 3, "the count is reported once, not once per frame");
    }

    #[test]
    fn a_signal_that_cannot_be_framed_at_all_is_still_counted() {
        // Unrecoverable either way. What is recoverable is that it happened,
        // which is the whole reason `dropped` is on the reply.
        let enormous = Response::Signals {
            signals: vec![crate::control::SignalEntry {
                actor: "act_a".into(),
                connection_id: "0".repeat(32),
                connection_epoch: "1".repeat(32),
                signal: crate::control::SignalBody::Ping {
                    nonce: "x".repeat(MAX_FRAME_BYTES * 2),
                },
            }],
            dropped: 1,
        };
        let bodies = control_bodies("orb_a", &enormous);
        assert_eq!(bodies.len(), 1);
        let value = body_of(&bodies[0]);
        assert_eq!(value["signals"].as_array().expect("signals").len(), 0);
        assert_eq!(value["dropped"], 2);
    }
}
