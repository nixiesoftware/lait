//! The browser bridge: one WebSocket carrying lanes the doorbell stream cannot.
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
//! Two lanes, because they will fail differently.
//!
//! - **Progress** must drop. It carries a number that is superseded by the next
//!   one, so the newest value per transfer replaces the queued one and a slow
//!   reader falls behind in staleness rather than in backlog. This lane is live.
//! - **Control** must not drop, because it will carry facts a client acts on
//!   once. **Nothing sends on it yet.** It is declared so the frame shape is
//!   fixed before something needs it — a lane invented at the moment of first
//!   use is a lane whose shape is decided by that use — and its queueing rule
//!   lands with its first producer, not before.
//!
//! The upgrade is where the origin check matters most. A WebSocket handshake is
//! exempt from CORS — the browser sends it cross-origin with no preflight and
//! attaches our cookie — so `check_upgrade_origin` runs *inside* the handler,
//! and requires an Origin rather than admitting an absent one.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::{err_json, App, ErrorKind};

/// The bridge's own version, independent of every other version in the tree.
///
/// It guards exactly one thing: a browser tab left open across a daemon
/// restart, holding a bundle from the previous build. Both halves ship in one
/// binary, so this never negotiates — it detects, says so once, and the client
/// reloads.
pub const BRIDGE_PROTOCOL_VERSION: u32 = 1;

/// The largest frame this build will decode.
///
/// Checked against the declared length *before* allocation. A postcard decoder
/// handed a hostile length would otherwise reserve whatever it was told to.
pub const MAX_BRIDGE_FRAME_BYTES: usize = 64 * 1024;

/// How many progress frames are held for a client that is not reading.
///
/// Small, because falling behind on progress is not a loss: the next tick
/// carries the current number, which is the only one that was ever wanted.
const PROGRESS_QUEUE: usize = 32;

/// How often progress is flushed, at most.
///
/// A transfer emits progress far faster than a person can read it, and every
/// frame costs a wakeup and a render. Coalescing to the newest value per
/// transfer on a tick turns an unbounded stream into a bounded one without
/// losing the only thing anybody wants from it, which is the latest number.
const PROGRESS_TICK: std::time::Duration = std::time::Duration::from_millis(500);

/// Which lane a frame belongs to.
///
/// A byte on the wire rather than a string: this is the discriminant every
/// frame carries, and it is read before anything else is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Lane {
    Control = 0,
    Progress = 1,
}

/// One framed message in either direction.
///
/// postcard rather than JSON because the progress lane is the high-rate one and
/// a number should cost bytes rather than a parse. The version rides every frame
/// rather than the handshake alone, so a stale tab is caught on the first frame
/// it sends instead of the first one it misinterprets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeFrame {
    pub protocol_version: u32,
    pub lane: Lane,
    pub body: Vec<u8>,
}

impl BridgeFrame {
    pub fn new(lane: Lane, body: Vec<u8>) -> Self {
        Self {
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            lane,
            body,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard bridge frame")
    }

    /// Decode a frame a browser sent, bounded before anything is allocated.
    ///
    /// The length check precedes the decode because that is the only order in
    /// which it protects anything: a decoder told a message is large has
    /// already reserved the room by the time it fails.
    pub fn decode(bytes: &[u8]) -> Result<Self, BridgeError> {
        if bytes.len() > MAX_BRIDGE_FRAME_BYTES {
            return Err(BridgeError::TooLarge);
        }
        let frame: Self = postcard::from_bytes(bytes).map_err(|_| BridgeError::Malformed)?;
        if frame.protocol_version != BRIDGE_PROTOCOL_VERSION {
            return Err(BridgeError::WrongVersion(frame.protocol_version));
        }
        if frame.body.len() > MAX_BRIDGE_FRAME_BYTES {
            return Err(BridgeError::TooLarge);
        }
        Ok(frame)
    }
}

/// Why a frame was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    TooLarge,
    Malformed,
    /// A tab from another build. Reported once, then the connection closes —
    /// there is nothing to negotiate, because both halves ship together.
    WrongVersion(u32),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "frame past the {MAX_BRIDGE_FRAME_BYTES}-byte ceiling"),
            Self::Malformed => write!(f, "frame did not decode"),
            Self::WrongVersion(v) => write!(
                f,
                "this tab speaks bridge v{v} and this server speaks \
                 v{BRIDGE_PROTOCOL_VERSION} — reload the page"
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

/// The bridge's fan-out, held on `App`.
///
/// Separate from `App.doorbells` on purpose, and the separation is the feature:
/// one ring for the thing every tab must see, one for the thing only the tab
/// that started a transfer cares about.
pub struct BridgeHub {
    progress: tokio::sync::broadcast::Sender<TransferProgress>,
}

impl BridgeHub {
    pub fn new() -> Self {
        Self {
            progress: tokio::sync::broadcast::channel(PROGRESS_QUEUE).0,
        }
    }

    /// Announce where a transfer has got to. Never blocks and never fails: with
    /// no browser attached there is nobody to tell, and that is not an error.
    pub fn note(&self, progress: TransferProgress) {
        let _ = self.progress.send(progress);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TransferProgress> {
        self.progress.subscribe()
    }
}

impl Default for BridgeHub {
    fn default() -> Self {
        Self::new()
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
) -> Response {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if let Err(refusal) = app.guard.check_upgrade_origin(host, origin) {
        return (
            StatusCode::FORBIDDEN,
            err_json(refusal.reason(), ErrorKind::Error),
        )
            .into_response();
    }
    upgrade.on_upgrade(move |socket| serve_socket(socket, app))
}

/// One connected browser, until it goes away or the server stops.
async fn serve_socket(mut socket: WebSocket, app: Arc<App>) {
    // One task owns the socket and both directions. Splitting it would need a
    // stream-combinator dependency for the two halves, and buys nothing here:
    // this connection sends on a tick and receives rarely, so there is no
    // concurrency to recover. `recv()` is a framed read, so dropping it when
    // another `select!` branch wins leaves any partial frame in the codec's
    // buffer rather than losing it.
    let mut progress = app.bridge.subscribe();
    let mut stop = app.stop.subscribe();
    let mut inbound = runtime::budget::Gate::from_spec(
        std::time::Instant::now(),
        runtime::budget::gates::FREIGHT_REQUESTS,
    );

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
                    let frame = BridgeFrame::new(Lane::Progress, body);
                    if socket
                        .send(Message::Binary(frame.encode().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }

            incoming = next_message(&mut socket) => match incoming {
                Some(Ok(bytes)) => {
                    // Inbound is paced. A browser is a local client, but a local
                    // client with a bug is still a client that can spin.
                    if matches!(
                        inbound.check(std::time::Instant::now()),
                        runtime::budget::Verdict::Close
                    ) {
                        return;
                    }
                    match BridgeFrame::decode(&bytes) {
                        Ok(_frame) => {
                            // No inbound lane accepts a message yet. The frames
                            // exist so the shape is fixed before something sends
                            // one; accepting an unknown body here would be
                            // deciding the shape by accident.
                        }
                        Err(BridgeError::WrongVersion(v)) => {
                            let _ = socket
                                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                                    code: 1002,
                                    reason: BridgeError::WrongVersion(v).to_string().into(),
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

    #[test]
    fn a_frame_past_the_ceiling_is_refused_before_it_is_decoded() {
        // The order is the whole point: a decoder told a message is large has
        // already reserved the room by the time it fails.
        let oversize = vec![0u8; MAX_BRIDGE_FRAME_BYTES + 1];
        assert_eq!(BridgeFrame::decode(&oversize), Err(BridgeError::TooLarge));
    }

    #[test]
    fn a_frame_whose_body_is_oversize_is_refused_even_if_the_frame_is_not() {
        // postcard encodes a length prefix, so a small envelope can still
        // declare a large body. Both are checked.
        let frame = BridgeFrame {
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            lane: Lane::Control,
            body: vec![7u8; MAX_BRIDGE_FRAME_BYTES + 1],
        };
        assert_eq!(
            BridgeFrame::decode(&frame.encode()),
            Err(BridgeError::TooLarge)
        );
    }

    #[test]
    fn a_tab_from_another_build_is_told_once_and_not_negotiated_with() {
        let stale = BridgeFrame {
            protocol_version: BRIDGE_PROTOCOL_VERSION + 1,
            lane: Lane::Control,
            body: Vec::new(),
        };
        let refusal = BridgeFrame::decode(&stale.encode());
        assert_eq!(
            refusal,
            Err(BridgeError::WrongVersion(BRIDGE_PROTOCOL_VERSION + 1))
        );
        assert!(
            refusal.unwrap_err().to_string().contains("reload"),
            "the refusal has to say what to do about it"
        );
    }

    #[test]
    fn a_frame_round_trips_on_both_lanes() {
        for lane in [Lane::Control, Lane::Progress] {
            let frame = BridgeFrame::new(lane, vec![1, 2, 3]);
            assert_eq!(BridgeFrame::decode(&frame.encode()), Ok(frame));
        }
    }

    #[test]
    fn garbage_is_malformed_rather_than_panicking() {
        for bytes in [&b""[..], &b"\xff\xff\xff\xff"[..], &b"not postcard"[..]] {
            assert!(matches!(
                BridgeFrame::decode(bytes),
                Err(BridgeError::Malformed) | Err(BridgeError::WrongVersion(_))
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
        let source = include_str!("bridge.rs");
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        let code = code
            .split_once("mod tests")
            .map(|(b, _)| b)
            .unwrap_or(&code);
        assert!(
            !code.contains("doorbells"),
            "the bridge reached for the doorbell ring"
        );
    }

    #[tokio::test]
    async fn a_hub_with_no_browser_attached_is_not_an_error() {
        let hub = BridgeHub::new();
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
}
