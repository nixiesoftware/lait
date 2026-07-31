//! The Live plane's shared view: what a reader sees, and what it never sees.
//!
//! `LiveHandle` is the seam between the driver thread that writes transient
//! state and the daemon thread that reads it. Everything here is about the two
//! properties that seam has to hold: a position is resolved on every read and
//! never cached, and a peer that left leaves nothing behind.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mechanics::ids::StationId;
use replica::ids::BodyKey;
use runtime::budget::deadline;
use runtime::live::{AnchorSource, CaretState, LiveHandle};
use runtime::transient::{TransientItem, TransientPayload, TransientScope};

const WORLD: &str = "com.example.notes";

fn station(seed: u8) -> StationId {
    StationId::from_device(&mechanics::crypto::device_from_seed(&[seed; 32])).expect("station")
}

fn caret_scope(body: u8) -> TransientScope {
    TransientScope::TextCaret {
        world: WORLD.into(),
        body: [body; 16],
        field: "text".into(),
    }
}

fn issue_scope(body: u8) -> TransientScope {
    TransientScope::IssueView {
        world: WORLD.into(),
        body: [body; 16],
    }
}

/// An empty version, spelled canonically.
///
/// `FabricVersion::default()` is *not* it: `Default` gives `format_version: 0`
/// and `validate` requires the real one, so a hand-built anchor carrying the
/// default is refused as non-canonical before anything looks at its position.
fn version() -> replica::FabricVersion {
    replica::FabricVersion {
        format_version: replica::CAUSAL_FORMAT_VERSION,
        heads: Vec::new(),
    }
}

/// An anchor at `offset` in `field`, encoded the way a payload carries one.
fn anchor_bytes(field: &str, offset: u64) -> Vec<u8> {
    replica::FabricAnchor {
        format_version: 1,
        body: [3u8; 32],
        path: field.into(),
        anchored_to: None,
        offset,
        after: false,
        taken_at: version(),
    }
    .encode()
}

fn caret_item(scope: TransientScope, seq: u64, offset: u64) -> TransientItem {
    let field = scope
        .field()
        .expect("a caret scope names a field")
        .to_string();
    TransientItem {
        session_epoch: [1u8; 16],
        seq,
        scope,
        payload: TransientPayload::Caret {
            anchor: anchor_bytes(&field, offset),
        },
    }
}

fn presence_item(scope: TransientScope, seq: u64) -> TransientItem {
    TransientItem {
        session_epoch: [1u8; 16],
        seq,
        scope,
        payload: TransientPayload::Presence,
    }
}

/// A Replica stand-in that counts what it was asked, and answers what it is told
/// to.
///
/// Deliberately not a real Station. What is under test is the handle's contract
/// — resolve per read, never cache, never mutate — and a real Replica would
/// prove `fabric`'s algebra instead, which `fabric` already proves.
struct ScriptedAnchors {
    answer: std::sync::Mutex<replica::AnchorResolution>,
    resolves: AtomicUsize,
    mints: AtomicUsize,
}

impl ScriptedAnchors {
    fn new(answer: replica::AnchorResolution) -> Self {
        Self {
            answer: std::sync::Mutex::new(answer),
            resolves: AtomicUsize::new(0),
            mints: AtomicUsize::new(0),
        }
    }

    fn set(&self, answer: replica::AnchorResolution) {
        *self.answer.lock().unwrap() = answer;
    }

    fn resolves(&self) -> usize {
        self.resolves.load(Ordering::SeqCst)
    }
}

impl AnchorSource for ScriptedAnchors {
    fn anchor_in_body(
        &self,
        _key: &BodyKey,
        path: &str,
        position: u64,
    ) -> Option<replica::FabricAnchor> {
        self.mints.fetch_add(1, Ordering::SeqCst);
        Some(replica::FabricAnchor {
            format_version: 1,
            body: [3u8; 32],
            path: path.into(),
            anchored_to: None,
            offset: position,
            after: false,
            taken_at: version(),
        })
    }

    fn resolve_anchor(
        &self,
        _key: &BodyKey,
        _anchor: &replica::FabricAnchor,
    ) -> replica::AnchorResolution {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        *self.answer.lock().unwrap()
    }
}

#[test]
fn a_caret_is_resolved_on_every_read_and_never_cached_in_a_slot() {
    // The whole reason resolution is not stored: a resolved position is only
    // true against the Body as it stands. Caching one produces a number that
    // was right once, which is exactly the silently-wrong index the anchor
    // algebra exists to prevent.
    let anchors = Arc::new(ScriptedAnchors::new(replica::AnchorResolution::Resolved(
        12,
    )));
    let handle = LiveHandle::new(Some(anchors.clone()));
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 12), now);

    let first = handle.view(None, now);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].caret, Some(CaretState::At(12)));
    assert_eq!(anchors.resolves(), 1);

    // The Body moves under the caret. Nothing is re-sent, nothing is
    // re-recorded, and the next read tells the truth anyway.
    anchors.set(replica::AnchorResolution::Resolved(40));
    let second = handle.view(None, now);
    assert_eq!(second.entries[0].caret, Some(CaretState::At(40)));
    assert_eq!(anchors.resolves(), 2, "asked again, not remembered");

    // And the generation did not move: resolving is a read, not a change.
    assert_eq!(first.generation, second.generation);
}

#[test]
fn a_drifted_anchor_renders_as_drifted_and_is_not_a_position() {
    // `AnchorResolution` is total, so this is never an error — and `Drifted`
    // must not collapse into a number a renderer would draw. The material the
    // position was attached to is gone; there is no honest offset to show.
    let anchors = Arc::new(ScriptedAnchors::new(replica::AnchorResolution::Drifted));
    let handle = LiveHandle::new(Some(anchors));
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 12), now);

    let view = handle.view(None, now);
    assert_eq!(view.entries[0].caret, Some(CaretState::Drifted));
}

#[test]
fn no_replica_is_unresolved_rather_than_drifted() {
    // Two different facts. `Drifted` is an answer — the position is gone.
    // `Unresolved` is the absence of one, and a renderer that conflated them
    // would show a live caret as lost.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 12), now);

    let view = handle.view(None, now);
    assert_eq!(view.entries[0].caret, Some(CaretState::Unresolved));
}

#[test]
fn a_selection_resolves_both_ends() {
    let anchors = Arc::new(ScriptedAnchors::new(replica::AnchorResolution::Resolved(7)));
    let handle = LiveHandle::new(Some(anchors.clone()));
    let now = Instant::now();
    handle.record(
        &station(1),
        &TransientItem {
            session_epoch: [1u8; 16],
            seq: 1,
            scope: caret_scope(1),
            payload: TransientPayload::Selection {
                anchor: anchor_bytes("text", 7),
                focus: anchor_bytes("text", 19),
            },
        },
        now,
    );

    let view = handle.view(None, now);
    assert_eq!(view.entries[0].caret, Some(CaretState::At(7)));
    assert_eq!(view.entries[0].focus, Some(CaretState::At(7)));
    assert_eq!(anchors.resolves(), 2, "both ends, not just the near one");
}

#[test]
fn presence_carries_no_position_and_costs_no_resolve() {
    // A resolve is a commit-lock acquisition. Paying one for "somebody is
    // looking at this issue" would put the writer lock behind every presence
    // read, which is the cheapest and most frequent thing on the plane.
    let anchors = Arc::new(ScriptedAnchors::new(replica::AnchorResolution::Resolved(1)));
    let handle = LiveHandle::new(Some(anchors.clone()));
    let now = Instant::now();
    handle.record(&station(1), &presence_item(issue_scope(1), 1), now);

    let view = handle.view(None, now);
    assert_eq!(view.entries[0].caret, None);
    assert_eq!(view.entries[0].focus, None);
    assert_eq!(anchors.resolves(), 0);
}

#[test]
fn a_peer_that_left_leaves_nothing_behind() {
    // Presence has no goodbye it can rely on, so a session ending *is* the
    // goodbye. A slot surviving it is a cursor on screen belonging to somebody
    // who closed their laptop, and no TTL under a minute reaches it.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    handle.record(&station(1), &presence_item(issue_scope(1), 1), now);
    handle.record(&station(2), &presence_item(issue_scope(1), 1), now);
    assert_eq!(handle.view(None, now).entries.len(), 2);

    let generation = handle.generation();
    assert_eq!(handle.forget(&station(1)), 1);
    let after = handle.view(None, now);
    assert_eq!(after.entries.len(), 1);
    assert_eq!(after.entries[0].station, station(2));
    assert_ne!(after.generation, generation, "a reader can tell it changed");

    // Forgetting a peer that held nothing is not a change.
    assert_eq!(handle.forget(&station(9)), 0);
    assert_eq!(handle.generation(), after.generation);
}

#[test]
fn retiring_a_scope_drops_every_kind_it_admits() {
    // A peer saying it is done with a caret means the caret and the selection,
    // not whichever one it happened to name.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 3), now);
    handle.record(
        &station(1),
        &TransientItem {
            session_epoch: [1u8; 16],
            seq: 2,
            scope: caret_scope(1),
            payload: TransientPayload::Selection {
                anchor: anchor_bytes("text", 3),
                focus: anchor_bytes("text", 9),
            },
        },
        now,
    );
    assert_eq!(handle.view(None, now).entries.len(), 2);

    handle.retire(&station(1), &caret_scope(1));
    assert!(handle.view(None, now).entries.is_empty());
}

#[test]
fn a_scope_narrows_the_view_to_itself() {
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    handle.record(&station(1), &presence_item(issue_scope(1), 1), now);
    handle.record(&station(1), &presence_item(issue_scope(2), 1), now);

    let narrowed = handle.view(Some(&issue_scope(1)), now);
    assert_eq!(narrowed.entries.len(), 1);
    assert_eq!(narrowed.entries[0].scope, issue_scope(1));
    assert_eq!(handle.view(None, now).entries.len(), 2);
}

#[test]
fn a_caret_past_its_grace_window_is_shown_and_shown_as_uncertain() {
    // Not dropped. A caret whose Body has moved under it since it arrived is
    // not wrong yet — it is no longer *known* to be right, and hiding it would
    // make a quiet collaborator disappear.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 3), now);

    let fresh = handle.view(None, now);
    assert!(!fresh.entries[0].uncertain);

    let later = now + Duration::from_secs(5);
    let aged = handle.view(None, later);
    assert_eq!(aged.entries.len(), 1, "still shown");
    assert!(aged.entries[0].uncertain);
    assert!(aged.entries[0].age_ms >= 5_000);
}

#[test]
fn expiry_removes_a_caret_before_it_removes_presence() {
    // Two TTLs, because they answer different questions. A cursor that stopped
    // moving thirty seconds ago is stale; a person who has been reading for a
    // minute is still there.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 3), now);
    handle.record(&station(1), &presence_item(issue_scope(1), 1), now);
    assert_eq!(handle.view(None, now).entries.len(), 2);

    let after_cursor_ttl = now + Duration::from_secs(45);
    assert_eq!(handle.sweep(after_cursor_ttl), 1);
    let left = handle.view(None, after_cursor_ttl);
    assert_eq!(left.entries.len(), 1);
    assert_eq!(left.entries[0].scope, issue_scope(1));

    let after_presence_ttl = now + Duration::from_secs(120);
    assert_eq!(handle.sweep(after_presence_ttl), 1);
    assert!(handle.view(None, after_presence_ttl).entries.is_empty());
}

#[test]
fn partial_says_so_and_is_not_a_diagnostic() {
    // Awareness is allowed to be incomplete; durable convergence is not. The
    // surface that can be partial has to say when it is, or a viewer showing
    // three of five people tells a confident lie.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    assert!(!handle.view(None, now).partial);

    handle.set_capped(true);
    assert!(handle.view(None, now).partial);

    // Setting it to what it already is is not a change a reader should see.
    let generation = handle.generation();
    handle.set_capped(true);
    assert_eq!(handle.generation(), generation);
}

#[test]
fn a_gate_drop_makes_the_view_partial_and_then_stops() {
    // The two causes of `partial` behave differently and both are right. The
    // session cap is a standing condition; a gate drop is an event, and it
    // matters only for as long as it could still be the reason something is
    // missing.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    assert!(!handle.view(None, now).partial);

    handle.note_dropped(now);
    assert!(handle.view(None, now).partial);

    // One cursor TTL later, whatever was dropped has either been superseded or
    // expired. Saying "incomplete" past that is saying it forever.
    let later = now + Duration::from_secs(45);
    assert!(!handle.view(None, later).partial);

    // And the cap does not decay: it is true until a session ends.
    handle.set_capped(true);
    assert!(handle.view(None, later).partial);
}

#[test]
fn minting_an_anchor_needs_a_world_id_that_parses() {
    // A scope's `world` is peer-supplied and a bare length check is not a
    // grammar. Something merely short is not therefore a World id, and the
    // answer to one that is not is "there is no anchor to send".
    let anchors = Arc::new(ScriptedAnchors::new(replica::AnchorResolution::Resolved(1)));
    let handle = LiveHandle::new(Some(anchors));
    assert!(handle.anchor(WORLD, [1u8; 16], "text", 4).is_some());
    assert!(handle
        .anchor("not-a-world-id", [1u8; 16], "text", 4)
        .is_none());

    // And with no Replica behind it there is nothing to mint from.
    let bare = LiveHandle::new(None);
    assert!(bare.anchor(WORLD, [1u8; 16], "text", 4).is_none());
}

#[test]
fn a_minted_anchor_is_what_a_payload_carries() {
    // The round trip that makes the mint useful: what `anchor` returns has to
    // be something `TransientPayload::validate` accepts, or a browser holding
    // an offset still has no path to a caret.
    let anchors = Arc::new(ScriptedAnchors::new(replica::AnchorResolution::Resolved(4)));
    let handle = LiveHandle::new(Some(anchors));
    let encoded = handle.anchor(WORLD, [1u8; 16], "text", 4).expect("minted");

    let item = TransientItem {
        session_epoch: [1u8; 16],
        seq: 1,
        scope: caret_scope(1),
        payload: TransientPayload::Caret { anchor: encoded },
    };
    item.validate().expect("a minted anchor is a legal payload");

    // And it survives the wire, which is where the canonical rule bites.
    let bytes = item.encode();
    assert_eq!(TransientItem::decode_canonical(&bytes), Ok(item));
}

/// Revocation on the Live plane, over a real connection.
///
/// Two mechanisms have to agree and it is easy to build so that only one works:
/// the driver races `serve` against an authority tick and drops the serve
/// future, and the session itself re-asks on a beat. These drive
/// `serve_session` directly, so what they prove is the *inside* half — the one
/// that has to hold when the tick has not fired.
mod revocation {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use comms::mem::MemNet;
    use comms::Transport;
    use runtime::admission::AdmittedPeer;
    use runtime::lifecycle::CancelToken;
    use runtime::live::{serve_session, SessionContext};
    use runtime::transient::LiveControl;
    use runtime::world::{AuthorityView, PrincipalResolution};

    /// A membership that can be taken away.
    struct Revocable {
        admitted: AtomicBool,
    }

    impl AuthorityView for Revocable {
        fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
            if !self.admitted.load(Ordering::SeqCst) {
                return None;
            }
            Some(PrincipalResolution {
                actor: peer_actor(),
                authority_frontier: frontier(),
            })
        }
    }

    fn peer_actor() -> mechanics::ids::ActorId {
        mechanics::ids::ActorId::parse(&format!("act_{}", "ef".repeat(32))).expect("actor")
    }

    fn frontier() -> replica::frontier::AuthorityFrontier {
        replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9])
    }

    fn admitted(station: StationId) -> AdmittedPeer {
        AdmittedPeer {
            station,
            actor: peer_actor(),
            authority_frontier: frontier(),
            granted_lanes: vec![runtime::planes::stream_kind::CONTROL],
            session_id: [2u8; 16],
            session_epoch: [1u8; 16],
            features: 0,
        }
    }

    /// A connected pair, and the Station the server side believes it is serving.
    async fn pair(
        seed: u8,
    ) -> (
        Arc<dyn comms::Connection>,
        Arc<dyn comms::Connection>,
        StationId,
    ) {
        let net = MemNet::new();
        let client_device = mechanics::crypto::device_from_seed(&[seed; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(client_device.clone()));
        let b: Arc<dyn Transport> =
            Arc::new(net.peer(mechanics::crypto::device_from_seed(&[seed + 1; 32])));
        let accepting = {
            let b = b.clone();
            tokio::spawn(async move { b.accept_connection().await })
        };
        let dialer = a
            .connect_session(b.my_id(), b"lait/session/1")
            .await
            .expect("connect");
        let incoming = accepting.await.expect("accept task").expect("incoming");
        // Kept alive for the whole test: dropping a transport tears its
        // endpoint down, and these have to outlive the connections they minted.
        std::mem::forget((a, b));
        (
            Arc::from(dialer),
            Arc::from(incoming.connection),
            StationId::from_device(&client_device).expect("station"),
        )
    }

    /// Run one Live session the way `run_driver` does.
    ///
    /// `serve_session` is deliberately not `Send` — `Rc`/`RefCell`, one owner,
    /// no locking on the hot path — so it cannot be `tokio::spawn`ed. A
    /// current-thread runtime plus a `LocalSet` on its own thread is the shape
    /// the driver uses, and a test that reached for `spawn` would be exercising
    /// a session the plane never runs.
    fn serve_on_thread(
        connection: Arc<dyn comms::Connection>,
        peer: AdmittedPeer,
        handle: Arc<LiveHandle>,
        authority: Option<Arc<dyn AuthorityView>>,
    ) -> (Arc<AtomicBool>, CancelToken) {
        let ended = Arc::new(AtomicBool::new(false));
        let cancel = CancelToken::new();
        std::thread::spawn({
            let ended = ended.clone();
            let cancel = cancel.clone();
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, async move {
                    serve_session(
                        connection,
                        peer,
                        cancel,
                        SessionContext {
                            handle: Some(handle),
                            signals: None,
                            authority,
                        },
                    )
                    .await;
                });
                ended.store(true, Ordering::SeqCst);
            }
        });
        (ended, cancel)
    }

    async fn subscribe(connection: &dyn comms::Connection, scopes: Vec<TransientScope>) {
        let (mut send, _recv) = connection.open_bi().await.expect("open");
        let body = LiveControl::Subscribe { scopes }.encode();
        let mut framed = vec![runtime::planes::stream_kind::CONTROL];
        framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
        framed.extend_from_slice(&body);
        send.write_all(&framed).await.expect("write");
        send.finish().expect("finish");
    }

    /// Publish until it lands, or give up. Datagrams are unreliable by
    /// definition, and the session may not have adopted the subscription yet.
    async fn publish_until_seen(
        connection: &dyn comms::Connection,
        handle: &LiveHandle,
        item: &TransientItem,
    ) -> bool {
        for _ in 0..80 {
            let _ = connection.send_datagram(&item.encode());
            tokio::time::sleep(Duration::from_millis(25)).await;
            if !handle.view(None, Instant::now()).entries.is_empty() {
                return true;
            }
        }
        false
    }

    async fn wait_for(flag: &AtomicBool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        flag.load(Ordering::SeqCst)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_peer_loses_its_live_session_and_its_slots_go_with_it() {
        let (client, server, peer) = pair(71).await;
        let authority = Arc::new(Revocable {
            admitted: AtomicBool::new(true),
        });
        let handle = Arc::new(LiveHandle::new(None));
        let (ended, _cancel) = serve_on_thread(
            server,
            admitted(peer),
            handle.clone(),
            Some(authority.clone()),
        );

        subscribe(client.as_ref(), vec![issue_scope(1)]).await;
        assert!(
            publish_until_seen(client.as_ref(), &handle, &presence_item(issue_scope(1), 1)).await,
            "the peer is here before it is removed"
        );

        // Membership goes away. Nothing tells the session; it has to ask.
        authority.admitted.store(false, Ordering::SeqCst);
        assert!(
            wait_for(&ended, deadline::AUTHORITY_REVALIDATION * 4).await,
            "the session ended on its own beat"
        );

        // Immediately, not at TTL. A cursor lingering for thirty seconds after a
        // removal is a person still visibly in a room they were asked to leave.
        assert!(
            handle.view(None, Instant::now()).entries.is_empty(),
            "and it took what it was holding with it"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_peer_cannot_buy_a_subscription_between_beats() {
        // Why the check is inline as well as on a beat. `Subscribe` is the frame
        // that *acquires* something, and waiting a full revalidation interval to
        // refuse one is a window a revoked peer walks through.
        let (client, server, peer) = pair(73).await;
        let authority = Arc::new(Revocable {
            admitted: AtomicBool::new(false),
        });
        let handle = Arc::new(LiveHandle::new(None));
        let (ended, _cancel) =
            serve_on_thread(server, admitted(peer), handle.clone(), Some(authority));

        subscribe(client.as_ref(), vec![issue_scope(1)]).await;
        // Well inside `AUTHORITY_REVALIDATION`, so the beat has not run.
        assert!(
            wait_for(&ended, Duration::from_millis(1_200)).await,
            "refused on the frame, not on the timer"
        );
        assert!(handle.view(None, Instant::now()).entries.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_with_no_authority_view_serves_normally() {
        // The plane has to work without one. `serve_session` is driven directly
        // by MemNet harnesses that have no Space behind them, and an absent view
        // must mean "not our question here" rather than "refuse everyone".
        let (client, server, peer) = pair(75).await;
        let handle = Arc::new(LiveHandle::new(None));
        let (ended, cancel) = serve_on_thread(server, admitted(peer), handle.clone(), None);

        subscribe(client.as_ref(), vec![issue_scope(1)]).await;
        assert!(
            publish_until_seen(client.as_ref(), &handle, &presence_item(issue_scope(1), 1)).await
        );
        assert!(!ended.load(Ordering::SeqCst), "and it is still serving");
        cancel.cancel();
    }
}

/// Dialling out on the Live plane.
///
/// The ledger is unit-tested here because every one of its rules is a bound
/// somebody has to be able to read, and driving them through a real transport
/// would prove the transport instead.
mod dialling {
    use super::*;

    use comms::mem::MemNet;
    use comms::Transport;
    use runtime::live::{dial, DialLedger, DialRefusal};
    use runtime::planes::{bounds, stream_kind, Plane, SessionAccept, SessionOpen};
    use runtime::world::{AuthorityView, PrincipalResolution};

    struct Everyone;
    impl AuthorityView for Everyone {
        fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
            Some(PrincipalResolution {
                actor: mechanics::ids::ActorId::parse(&format!("act_{}", "12".repeat(32)))
                    .expect("actor"),
                authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(
                    vec![9],
                ),
            })
        }
    }

    struct Nobody;
    impl AuthorityView for Nobody {
        fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
            None
        }
    }

    fn space() -> mechanics::ids::SpaceId {
        mechanics::ids::SpaceId::from_digest([44u8; 16])
    }

    #[test]
    fn a_failed_dial_backs_off_and_a_successful_one_forgets() {
        // A Station sitting at its own ceiling refuses every dial with a bare
        // close, which reaches the dialer as an ordinary transport failure.
        // Without a cooldown that is a hot loop against a Station doing nothing
        // wrong.
        let mut ledger = DialLedger::new();
        let peer = station(1);
        let now = Instant::now();
        assert!(ledger.may_dial(&peer, now));

        ledger.begin(&peer);
        ledger.failed(&peer, now);
        assert!(!ledger.may_dial(&peer, now), "not immediately");
        assert!(ledger.may_dial(&peer, now + deadline::LIVE_DIAL * 2));

        // And it doubles.
        ledger.begin(&peer);
        ledger.failed(&peer, now);
        assert!(!ledger.may_dial(&peer, now + deadline::LIVE_DIAL * 2));

        // Capped, so a peer that has been unreachable all day is still retried
        // on the scale at which anyone would notice it come back.
        for _ in 0..20 {
            ledger.begin(&peer);
            ledger.failed(&peer, now);
        }
        assert!(ledger.may_dial(&peer, now + deadline::PRESENCE_TTL));

        // A peer that answers has answered. The cooldown was protecting against
        // one that would not.
        ledger.begin(&peer);
        ledger.established(&peer);
        ledger.ended(&peer);
        assert!(ledger.may_dial(&peer, now));
    }

    #[test]
    fn three_ceilings_answer_three_different_questions() {
        let mut ledger = DialLedger::new();
        let now = Instant::now();

        // How many dials may be outstanding while none has answered. The one
        // that is easy to forget, and the one that matters under a partition
        // where every dial is in flight and none is a session.
        for n in 0..runtime::budget::slots::MAX_LIVE_DIALS_IN_FLIGHT {
            assert!(ledger.may_dial(&station(n as u8 + 10), now));
            ledger.begin(&station(n as u8 + 10));
        }
        assert!(!ledger.may_dial(&station(99), now));
        assert_eq!(
            ledger.dials_in_flight(),
            runtime::budget::slots::MAX_LIVE_DIALS_IN_FLIGHT
        );

        // How many of the Station's sessions any one peer may take.
        let mut ledger = DialLedger::new();
        let peer = station(2);
        for _ in 0..runtime::budget::slots::MAX_LIVE_SESSIONS_PER_STATION {
            assert!(ledger.may_dial(&peer, now));
            ledger.begin(&peer);
            ledger.established(&peer);
        }
        assert!(!ledger.may_dial(&peer, now), "one peer, bounded share");
        assert!(ledger.may_dial(&station(3), now), "and others still may");
    }

    #[test]
    fn a_peer_already_being_dialled_is_not_dialled_again() {
        // Two dials to one peer would be two sessions carrying the same
        // presence, and the second would evict the first at
        // MAX_CONNECTIONS_PER_PEER_PLANE on the responder side — a race that
        // costs a round trip to lose.
        let mut ledger = DialLedger::new();
        let peer = station(4);
        let now = Instant::now();
        ledger.begin(&peer);
        assert!(!ledger.may_dial(&peer, now));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dial_carries_the_lanes_and_offers_no_feature_this_build_lacks() {
        // `feature::LOCAL_SUPPORTED` is zero, and offering a bit this build does
        // not honour is a promise a peer would be right to be annoyed about.
        // Freight's dialer still offers RESIDENCY_HINTS; that is a pre-existing
        // dishonesty on the wrong plane, and this one does not inherit it.
        let net = MemNet::new();
        let local_device = mechanics::crypto::device_from_seed(&[81u8; 32]);
        let peer_device = mechanics::crypto::device_from_seed(&[82u8; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(local_device.clone()));
        let b: Arc<dyn Transport> = Arc::new(net.peer(peer_device.clone()));

        let responder = tokio::spawn({
            let b = b.clone();
            async move {
                let incoming = b.accept_connection().await.expect("incoming");
                let mut recv = incoming
                    .connection
                    .accept_uni()
                    .await
                    .expect("accept")
                    .expect("a flow");
                let raw = recv
                    .read_to_end(bounds::MAX_OPENING_BYTES)
                    .await
                    .expect("opening");
                let open = SessionOpen::decode_canonical(&raw).expect("canonical opening");
                let accept = SessionAccept {
                    session_id: open.session_id,
                    session_epoch: open.session_epoch,
                    capability: runtime::planes::ProtocolCapability {
                        plane: Plane::Live,
                        protocol_version: open.protocol_version,
                        features: open.features & runtime::planes::feature::LOCAL_SUPPORTED,
                    },
                    // Grant one of the two asked for, so the test can tell the
                    // dialer reports what the *responder* said rather than what
                    // it hoped for.
                    granted_lanes: vec![stream_kind::CONTROL],
                };
                let mut send = incoming.connection.open_uni().await.expect("open");
                send.write_all(&accept.encode()).await.expect("write");
                send.finish().expect("finish");
                open
            }
        });

        let live = dial(
            a.as_ref(),
            &Everyone,
            &space(),
            &StationId::from_device(&local_device).expect("local"),
            &StationId::from_device(&peer_device).expect("peer"),
            [7u8; 16],
        )
        .await
        .expect("dialled");

        let open = responder.await.expect("responder");
        assert_eq!(open.plane, Plane::Live);
        assert_eq!(
            open.features,
            runtime::planes::feature::LOCAL_SUPPORTED,
            "what this build implements, and nothing it merely has a name for"
        );
        assert_eq!(
            open.features & runtime::planes::feature::UNSOLICITED_PROVIDE,
            0,
            "nothing serves a chunk without being asked, so it is not offered"
        );
        assert_eq!(
            open.requested_lanes,
            vec![stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL],
            "the ALPN does not type this plane, so the lanes are named"
        );

        // What the responder granted, not what the dialer asked for.
        assert_eq!(live.peer.granted_lanes, vec![stream_kind::CONTROL]);
        assert_eq!(live.peer.session_id, [7u8; 16]);
        // And the negotiated intersection reaches the plane that honours it.
        assert_eq!(
            live.peer.features,
            runtime::planes::feature::RESIDENCY_HINTS
        );
        // And the identity is this Station's own resolution, never the packet's.
        assert_eq!(live.peer.actor.as_str(), format!("act_{}", "12".repeat(32)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_we_would_refuse_on_arrival_is_not_dialled_at_all() {
        // Asked before the transport is touched. Dialling a peer we would refuse
        // when it answered is a round trip spent to be told what we already
        // knew — and on a partitioned network it is a round trip that takes the
        // full dial deadline.
        let net = MemNet::new();
        let local_device = mechanics::crypto::device_from_seed(&[85u8; 32]);
        let peer_device = mechanics::crypto::device_from_seed(&[86u8; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(local_device.clone()));
        // Deliberately nothing accepting: reaching the transport at all would
        // hang until the deadline, so a prompt answer *is* the assertion.
        let refused = dial(
            a.as_ref(),
            &Nobody,
            &space(),
            &StationId::from_device(&local_device).expect("local"),
            &StationId::from_device(&peer_device).expect("peer"),
            [8u8; 16],
        )
        .await;
        assert_eq!(refused.err(), Some(DialRefusal::NotAdmitted));
    }
}

/// Residency hints: who to ask first, and nothing more.
mod residency {
    use super::*;
    use runtime::live::{LiveHandle, NoResidency, ResidencyOracle, ResidencyState};

    /// An oracle that holds exactly the chunks it was given.
    struct Holds(Vec<u32>);

    impl ResidencyOracle for Holds {
        fn residency(&self, _content: &[u8; 32], wanted: &[u32]) -> ResidencyState {
            let held = wanted.iter().filter(|i| self.0.contains(i)).count();
            if held == 0 {
                ResidencyState::Absent
            } else if held == wanted.len() {
                ResidencyState::Complete
            } else {
                ResidencyState::Partial
            }
        }
    }

    #[test]
    fn a_station_with_no_content_answers_absent_rather_than_failing() {
        // `Absent` is the truth, not a placeholder. A Station holding no content
        // holds none of this content either, and a hint saying so sends the
        // asker to somebody who can help.
        let handle = LiveHandle::new(None);
        assert_eq!(
            handle.residency(&[1u8; 32], &[0, 1, 2]),
            ResidencyState::Absent
        );
        assert_eq!(
            NoResidency.residency(&[1u8; 32], &[]),
            ResidencyState::Absent
        );
    }

    #[test]
    fn three_states_and_no_bitmap() {
        // Three states rather than a chunk list is the whole design. A peer that
        // could read a complete bitmap off a hint could reconstruct which parts
        // of a file somebody had opened.
        let handle = LiveHandle::with_residency(None, Arc::new(Holds(vec![0, 2])));
        assert_eq!(
            handle.residency(&[1u8; 32], &[0, 2]),
            ResidencyState::Complete
        );
        assert_eq!(
            handle.residency(&[1u8; 32], &[0, 1]),
            ResidencyState::Partial
        );
        assert_eq!(
            handle.residency(&[1u8; 32], &[3, 4]),
            ResidencyState::Absent
        );
    }

    #[test]
    fn asking_about_nothing_is_absent_and_costs_nothing() {
        // An empty `wanted` must not read as "I hold everything you asked for",
        // which is what a naive all-of-them-are-present check returns for an
        // empty set.
        let handle = LiveHandle::with_residency(None, Arc::new(Holds(vec![0, 1, 2])));
        assert_eq!(handle.residency(&[1u8; 32], &[]), ResidencyState::Absent);
    }
}
