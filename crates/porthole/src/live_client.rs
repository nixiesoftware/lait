//! A minimal Live-plane CLIENT for a browser tab — presence and carets over
//! the same `lait/session/1` connection a daemon uses, driven on the Worker's
//! event loop with no threads.
//!
//! The native Live plane (`runtime::plane::live`) is a large tokio/thread
//! machine (a current-thread runtime, JoinSets, timers, an OS-thread dialer)
//! and is `cfg(not(wasm32))`. But a tab does not need the plane — it needs to
//! be ONE peer on it: dial a peer's Live plane, say what issue it is looking
//! at, publish its own presence/caret as datagrams, and receive others'. The
//! wire vocabulary it speaks is all wasm-eligible: the admission frames live in
//! `contact::admission` (re-exported through `runtime::plane`), and the
//! `TransientItem`/`LiveControl` shapes in `runtime::transient` are ungated.
//!
//! Security is unchanged from the daemon path: confidentiality and identity
//! come from the ADMITTED iroh QUIC connection — the acceptor checks Space
//! membership on the `Open` and binds every datagram to the transport-
//! authenticated device, never to anything in the payload. So a tab's carets
//! are as authentic and private as a daemon's, with no per-message crypto.
//!
//! This is a client, not a `serve_session` port: it publishes and subscribes,
//! and it does not relay other peers' presence onward (that fan-out is the
//! daemon's, which is why tab↔daemon works and all-tabs-no-daemon is the
//! deferred cloud-relay case).

use comms::{Connection, Transport};
use mechanics::ids::SpaceId;
use mechanics::station::Key;
use runtime::plane::{feature, stream_kind, Accept, Open, Plane, Refusal, LIVE_ALPN, SPACE_ID_LEN};
use runtime::transient::{LiveControl, RelayedPresence, Target, TransientItem, TransientPayload};

/// Why a Live dial did not become a session.
#[derive(Debug)]
pub enum LiveError {
    /// The space id is not the 29 rendered bytes the wire carries.
    SpaceShape,
    /// System entropy was unavailable for the connection epoch.
    Entropy,
    /// The transport could not open the session connection.
    Unreachable(String),
    /// A flow could not be opened or written.
    Flow(String),
    /// The peer answered, but not with an Accept — its refusal, or unintelligible.
    Refused(String),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpaceShape => write!(f, "space id is not 29 rendered bytes"),
            Self::Entropy => write!(f, "system entropy unavailable"),
            Self::Unreachable(e) => write!(f, "the Live peer is unreachable: {e}"),
            Self::Flow(e) => write!(f, "a Live flow failed: {e}"),
            Self::Refused(e) => write!(f, "the Live peer refused: {e}"),
        }
    }
}

/// One Live-plane session a tab holds to a peer: the connection, the epoch
/// every datagram it sends is bound to, and a monotonic sequence per publish.
///
/// Every method takes `&self` with interior mutability, so the packaged handle
/// can hold one directly and drive it across `await`s without a `RefCell`
/// borrow spanning a yield — the hazard the single-threaded Worker turns into a
/// panic if another frame re-enters mid-await.
pub struct LiveClient {
    connection: Box<dyn Connection>,
    /// The connection epoch sent in the `Open` — every `TransientItem` this tab
    /// publishes carries it, so the acceptor binds the item to this session.
    epoch: [u8; 16],
    /// Whatever this tab has told the peer it is watching. The publish side may
    /// only publish into a scope it subscribed, exactly as the acceptor
    /// enforces on receive.
    subscribed: std::cell::RefCell<std::collections::BTreeSet<Target>>,
    seq: std::cell::Cell<u64>,
    /// Whether the peer agreed to relay OTHER peers' presence (negotiated
    /// `feature::PRESENCE_RELAY`). When set, every inbound presence datagram is a
    /// `RelayedPresence` carrying the true author's station; when not, it is a
    /// bare `TransientItem` and the author is the responder itself.
    relay: bool,
}

impl LiveClient {
    /// Dial `responder`'s Live plane and complete the `Open`/`Accept`
    /// handshake — the dialer half of `runtime::plane::live::dial`, reproduced
    /// against the wasm-available wire. The tab does not pre-check admission
    /// (that is the acceptor's job); it dials and lets the peer admit it.
    pub async fn connect(
        transport: &dyn Transport,
        space: &SpaceId,
        local: &Key,
        responder: &Key,
    ) -> Result<Self, LiveError> {
        let mut space_bytes = [0u8; SPACE_ID_LEN];
        let raw = space.as_str().as_bytes();
        if raw.len() != SPACE_ID_LEN {
            return Err(LiveError::SpaceShape);
        }
        space_bytes.copy_from_slice(raw);

        let mut connection_id = [0u8; 16];
        getrandom03::fill(&mut connection_id).map_err(|_| LiveError::Entropy)?;
        let mut epoch = [0u8; 16];
        getrandom03::fill(&mut epoch).map_err(|_| LiveError::Entropy)?;

        let connection = transport
            .connect_session(responder.as_device(), LIVE_ALPN)
            .await
            .map_err(|e| LiveError::Unreachable(format!("{e:#}")))?;

        let open = Open {
            plane: Plane::Live,
            protocol_version: Plane::Live.protocol_version(),
            features: feature::LOCAL_SUPPORTED,
            space: space_bytes,
            initiator_station: local.key_bytes(),
            responder_station: responder.key_bytes(),
            connection_id,
            connection_epoch: epoch,
            authority_frontier: Vec::new(),
            // CONTROL carries the subscription; a tab needs no media lanes.
            requested_lanes: vec![stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL],
        };

        // The Open rides a uni flow, then the Accept comes back on one.
        let mut flow = connection
            .open_uni()
            .await
            .map_err(|e| LiveError::Flow(format!("open_uni: {e:#}")))?;
        flow.write_all(&open.encode())
            .await
            .map_err(|e| LiveError::Flow(format!("write open: {e:#}")))?;
        flow.finish()
            .map_err(|e| LiveError::Flow(format!("finish open: {e:#}")))?;

        let mut recv = connection
            .accept_uni()
            .await
            .map_err(|e| LiveError::Flow(format!("accept_uni: {e:#}")))?
            .ok_or_else(|| LiveError::Flow("the peer opened no accept flow".into()))?;
        let answer = recv
            .read_to_end(runtime::plane::bounds::MAX_OPENING_BYTES)
            .await
            .map_err(|e| LiveError::Flow(format!("read accept: {e:#}")))?;

        match Accept::decode_canonical(&answer) {
            Ok(accept) => Ok(Self {
                connection,
                epoch,
                subscribed: std::cell::RefCell::new(std::collections::BTreeSet::new()),
                seq: std::cell::Cell::new(0),
                // The peer relays only if it agreed to; the accept is its answer.
                relay: accept.capability.features & feature::PRESENCE_RELAY != 0,
            }),
            Err(_) => Err(LiveError::Refused(
                match Refusal::decode_canonical(&answer) {
                    Ok(refusal) => format!("{refusal:?}"),
                    Err(_) => "unintelligible answer".into(),
                },
            )),
        }
    }

    /// Declare what this tab is watching. Replaces the subscription set — the
    /// same `LiveControl::Subscribe` a daemon sends, framed for the CONTROL
    /// lane (`[CONTROL][u32 LE len][postcard]`). Publishing into an unsubscribed
    /// scope is dropped by the acceptor, so the publish side checks it too.
    pub async fn subscribe(&self, scopes: Vec<Target>) -> Result<(), LiveError> {
        let body = LiveControl::Subscribe {
            scopes: scopes.clone(),
        }
        .encode();
        let mut framed = Vec::with_capacity(1 + 4 + body.len());
        framed.push(stream_kind::CONTROL);
        framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
        framed.extend_from_slice(&body);

        let (mut send, _recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| LiveError::Flow(format!("open control bi: {e:#}")))?;
        send.write_all(&framed)
            .await
            .map_err(|e| LiveError::Flow(format!("write subscribe: {e:#}")))?;
        send.finish()
            .map_err(|e| LiveError::Flow(format!("finish subscribe: {e:#}")))?;

        // Set after the write succeeds — a subscription recorded but not sent
        // would let `publish` send into a scope the peer never heard, which the
        // acceptor drops silently. Brief borrow, after the await, never across.
        *self.subscribed.borrow_mut() = scopes.into_iter().collect();
        Ok(())
    }

    /// Whether this scope is already subscribed — lets a caller skip re-opening
    /// a CONTROL stream per keystroke and send steady-state carets as pure
    /// datagrams.
    pub fn is_subscribed(&self, scope: &Target) -> bool {
        self.subscribed.borrow().contains(scope)
    }

    /// Publish one payload into a scope this tab is watching, as an unreliable
    /// datagram bound to this session's epoch — the tab's whole send side. A
    /// caller must have `subscribe`d the scope; an oversize datagram (past the
    /// path's capacity) is dropped rather than truncated, exactly as the
    /// daemon's `publish` does.
    pub fn publish(&self, scope: Target, payload: TransientPayload) -> bool {
        if !self.subscribed.borrow().contains(&scope) {
            return false;
        }
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        let item = TransientItem {
            connection_epoch: self.epoch,
            seq,
            scope,
            payload,
        };
        let encoded = item.encode();
        if !runtime::plane::datagram_fits(encoded.len(), self.connection.datagram_capacity()) {
            return false;
        }
        self.connection.send_datagram(&encoded).is_ok()
    }

    /// The next presence item, with its author — or `None` when none is pending.
    /// The receive side the viewer's facepile/carets draw from; the Worker pumps
    /// this like it pumps the doorbell ring. Bound and canonicality are checked
    /// exactly as the daemon checks an inbound datagram, so a malformed or oversize
    /// one is skipped, never trusted.
    ///
    /// The `Option<[u8; 32]>` is the ORIGIN station when the peer is relaying
    /// (every datagram is a `RelayedPresence` that names the true author), and
    /// `None` when it is not (a bare `TransientItem` whose author is the responder
    /// this tab dialed — the caller attributes it to that).
    pub async fn next_item(&self) -> Option<(Option<[u8; 32]>, TransientItem)> {
        loop {
            let payload = self.connection.read_datagram().await.ok()??;
            if self.relay {
                match RelayedPresence::decode_canonical(&payload) {
                    Ok(relayed) => return Some((Some(relayed.origin), relayed.item)),
                    // Skip a malformed frame and keep reading — one bad datagram
                    // from a supporter is not the end of the session.
                    Err(_) => continue,
                }
            } else {
                match TransientItem::decode_canonical(&payload) {
                    Ok(item) => return Some((None, item)),
                    Err(_) => continue,
                }
            }
        }
    }
}
