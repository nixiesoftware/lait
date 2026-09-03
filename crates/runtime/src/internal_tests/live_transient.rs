//! The Live plane's shared view: what a reader sees, and what it never sees.
//!
//! `LiveHandle` is the seam between the driver thread that writes transient
//! state and the daemon thread that reads it. Everything here is about the two
//! properties that seam has to hold: a position is resolved on every read and
//! never cached, and a peer that left leaves nothing behind.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use mechanics::station::Key;
use replica::body::BodyKey;
use runtime::budget::deadline;
use runtime::plane::live::{AnchorSource, CaretState, LiveHandle, LiveNarrow};
use runtime::transient::{RelayedPresence, Target, TransientItem, TransientPayload};

const WORLD: &str = "com.example.notes";

fn station(seed: u8) -> Key {
    Key::from_device(&mechanics::actor::device_from_seed(&[seed; 32])).expect("station")
}

fn caret_scope(body: u8) -> Target {
    Target::Field {
        world: WORLD.into(),
        body: [body; 16],
        field: "text".into(),
    }
}

fn issue_scope(body: u8) -> Target {
    Target::Body {
        world: WORLD.into(),
        body: [body; 16],
    }
}

/// An empty version, spelled canonically.
///
/// `Version::default()` is *not* it: `Default` gives `format_version: 0`
/// and `validate` requires the real one, so a hand-built anchor carrying the
/// default is refused as non-canonical before anything looks at its position.
fn version() -> fabric::Version {
    fabric::Version {
        format_version: fabric::CAUSAL_FORMAT_VERSION,
        heads: Vec::new(),
    }
}

/// An anchor at `offset` in `field`, encoded the way a payload carries one.
fn anchor_bytes(field: &str, offset: u64) -> Vec<u8> {
    fabric::Anchor {
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

fn caret_item(scope: Target, seq: u64, offset: u64) -> TransientItem {
    let field = scope
        .field()
        .expect("a caret scope names a field")
        .to_string();
    TransientItem {
        connection_epoch: [1u8; 16],
        seq,
        scope,
        payload: TransientPayload::Caret {
            anchor: anchor_bytes(&field, offset),
        },
    }
}

fn presence_item(scope: Target, seq: u64) -> TransientItem {
    TransientItem {
        connection_epoch: [1u8; 16],
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
/// prove `Engine`'s algebra instead, which `Engine` already proves.
struct ScriptedAnchors {
    answer: std::sync::Mutex<fabric::AnchorResolution>,
    resolves: AtomicUsize,
    mints: AtomicUsize,
}

impl ScriptedAnchors {
    fn new(answer: fabric::AnchorResolution) -> Self {
        Self {
            answer: std::sync::Mutex::new(answer),
            resolves: AtomicUsize::new(0),
            mints: AtomicUsize::new(0),
        }
    }

    fn set(&self, answer: fabric::AnchorResolution) {
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
    ) -> Result<Option<fabric::Anchor>, crate::world::BodyReadFailure> {
        self.mints.fetch_add(1, Ordering::SeqCst);
        Ok(Some(fabric::Anchor {
            format_version: 1,
            body: [3u8; 32],
            path: path.into(),
            anchored_to: None,
            offset: position,
            after: false,
            taken_at: version(),
        }))
    }

    /// Answers the anchor's own offset when told to resolve, so a caller that
    /// mixed up two anchors is caught rather than flattered. The first version
    /// of this ignored its argument entirely, which made
    /// `a_selection_resolves_both_ends` unable to tell the focus anchor from the
    /// near anchor resolved twice.
    fn resolve_anchor(
        &self,
        _key: &BodyKey,
        anchor: &fabric::Anchor,
    ) -> Result<fabric::AnchorResolution, crate::world::BodyReadFailure> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        Ok(match *self.answer.lock().unwrap() {
            fabric::AnchorResolution::Resolved(_) => {
                fabric::AnchorResolution::Resolved(anchor.offset)
            }
            drifted => drifted,
        })
    }
}

#[test]
fn a_caret_is_resolved_on_every_read_and_never_cached_in_a_slot() {
    // The whole reason resolution is not stored: a resolved position is only
    // true against the Body as it stands. Caching one produces a number that
    // was right once, which is exactly the silently-wrong index the anchor
    // algebra exists to prevent.
    let anchors = Arc::new(ScriptedAnchors::new(fabric::AnchorResolution::Resolved(12)));
    let handle = LiveHandle::new(Some(anchors.clone()));
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 12), now);

    let first = handle.view(None, now);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].caret, Some(CaretState::At(12)));
    assert_eq!(anchors.resolves(), 1);

    // The Body moves under the caret. Nothing is re-sent, nothing is
    // re-recorded, and the next read asks again rather than remembering.
    anchors.set(fabric::AnchorResolution::Drifted);
    let second = handle.view(None, now);
    assert_eq!(second.entries[0].caret, Some(CaretState::Drifted));
    assert_eq!(anchors.resolves(), 2, "asked again, not remembered");

    // And the generation did not move: resolving is a read, not a change.
    assert_eq!(first.generation, second.generation);
}

#[test]
fn a_drifted_anchor_renders_as_drifted_and_is_not_a_position() {
    // `Drifted` is an exact answer and must not collapse into a number a
    // renderer would draw. Image/key/capacity failure is distinct and tested
    // below as `Unresolved`.
    let anchors = Arc::new(ScriptedAnchors::new(fabric::AnchorResolution::Drifted));
    let handle = LiveHandle::new(Some(anchors));
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 12), now);

    let view = handle.view(None, now);
    assert_eq!(view.entries[0].caret, Some(CaretState::Drifted));
}

#[test]
fn governed_body_failure_is_unresolved_never_fake_drift() {
    struct RefusingAnchors;

    impl AnchorSource for RefusingAnchors {
        fn anchor_in_body(
            &self,
            key: &BodyKey,
            _path: &str,
            _position: u64,
        ) -> Result<Option<fabric::Anchor>, crate::world::BodyReadFailure> {
            Err(crate::world::BodyReadFailure::Capacity(
                crate::world::BodyReadCoordinate::new(key.clone(), Some([0x84; 32])),
            ))
        }

        fn resolve_anchor(
            &self,
            key: &BodyKey,
            _anchor: &fabric::Anchor,
        ) -> Result<fabric::AnchorResolution, crate::world::BodyReadFailure> {
            Err(crate::world::BodyReadFailure::KeyUnavailable(
                crate::world::BodyReadCoordinate::new(key.clone(), Some([0x85; 32])),
            ))
        }
    }

    let handle = LiveHandle::new(Some(Arc::new(RefusingAnchors)));
    let now = Instant::now();
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 12), now);
    assert_eq!(
        handle.view(None, now).entries[0].caret,
        Some(CaretState::Unresolved),
        "key/capacity failure is not positional drift",
    );
    assert!(
        matches!(
            handle.anchor("com.example.board", [1; 16], "text", 12),
            Err(crate::world::BodyReadFailure::Capacity(_))
        ),
        "a refused image remains typed and cannot mint a fake anchor"
    );
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
    let anchors = Arc::new(ScriptedAnchors::new(fabric::AnchorResolution::Resolved(7)));
    let handle = LiveHandle::new(Some(anchors.clone()));
    let now = Instant::now();
    handle.record(
        &station(1),
        &TransientItem {
            connection_epoch: [1u8; 16],
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
    // Two *different* positions. The mock answers each anchor's own offset, so
    // resolving the near anchor twice would show 7 and 7 and pass a test that
    // only counted calls.
    assert_eq!(view.entries[0].caret, Some(CaretState::At(7)));
    assert_eq!(view.entries[0].focus, Some(CaretState::At(19)));
    assert_eq!(anchors.resolves(), 2, "both ends, not just the near one");
}

#[test]
fn presence_carries_no_position_and_costs_no_resolve() {
    // A resolve is a commit-lock acquisition. Paying one for "somebody is
    // looking at this issue" would put the writer lock behind every presence
    // read, which is the cheapest and most frequent thing on the plane.
    let anchors = Arc::new(ScriptedAnchors::new(fabric::AnchorResolution::Resolved(1)));
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
            connection_epoch: [1u8; 16],
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
fn narrowing_to_a_body_gathers_every_scope_that_names_it() {
    // The distinction that matters, and getting it wrong is silent. Somebody
    // looking at an issue wants who is present, where their carets are, and who
    // is typing — three *different* scopes over one Body. Narrowing by scope
    // equality answers with presence alone, which looks exactly like a document
    // nobody has a cursor in.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    handle.record(&station(1), &presence_item(issue_scope(1), 1), now);
    handle.record(&station(1), &caret_item(caret_scope(1), 1, 3), now);
    handle.record(
        &station(2),
        &TransientItem {
            connection_epoch: [1u8; 16],
            seq: 1,
            scope: Target::Typing {
                world: WORLD.into(),
                body: [1u8; 16],
                field: "text".into(),
            },
            payload: TransientPayload::Typing,
        },
        now,
    );
    // A different Body, which must not be gathered.
    handle.record(&station(1), &presence_item(issue_scope(9), 1), now);

    let exact = handle.view(Some(&issue_scope(1)), now);
    assert_eq!(exact.entries.len(), 1, "scope equality sees presence only");

    let about = handle.view_narrowed(
        LiveNarrow::Body {
            world: WORLD,
            body: [1u8; 16],
        },
        now,
    );
    assert_eq!(about.entries.len(), 3, "presence, caret and typing");
    assert!(about
        .entries
        .iter()
        .all(|entry| entry.scope != issue_scope(9)));
}

#[test]
fn a_scope_that_names_no_body_is_reachable_only_exactly() {
    // Residency is about a content, not a Body, so `Body` narrowing must not
    // sweep it in — a reader asking about a document is not asking who holds
    // which chunks of an unrelated file.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    let residency = Target::Content { content: [4u8; 32] };
    handle.record(
        &station(1),
        &TransientItem {
            connection_epoch: [1u8; 16],
            seq: 1,
            scope: residency.clone(),
            payload: TransientPayload::Residency { chunks: vec![0, 1] },
        },
        now,
    );

    assert_eq!(
        handle
            .view_narrowed(
                LiveNarrow::Body {
                    world: WORLD,
                    body: [1u8; 16]
                },
                now
            )
            .entries
            .len(),
        0
    );
    assert_eq!(handle.view(Some(&residency), now).entries.len(), 1);
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

    handle.set_accepting_capped(true);
    assert!(handle.view(None, now).partial);

    // Setting it to what it already is is not a change a reader should see.
    let generation = handle.generation();
    handle.set_accepting_capped(true);
    assert_eq!(handle.generation(), generation);
}

#[test]
fn the_two_ceilings_do_not_overwrite_each_other() {
    // Two owners compute `partial` from disjoint counts: the accept side from
    // its session list, the dial side from its ledger. One flag meant each
    // unconditionally clobbered the other, so a Station at its dial ceiling
    // reported itself complete the moment any inbound session ended.
    let handle = LiveHandle::new(None);
    let now = Instant::now();

    handle.set_dialling_capped(true);
    assert!(handle.view(None, now).partial);

    // The accept side saying "not capped" must not answer for the dial side.
    handle.set_accepting_capped(false);
    assert!(handle.view(None, now).partial, "the dialer is still capped");

    handle.set_dialling_capped(false);
    assert!(!handle.view(None, now).partial);
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
    handle.set_accepting_capped(true);
    assert!(handle.view(None, later).partial);
}

#[test]
fn minting_an_anchor_needs_a_world_id_that_parses() {
    // A scope's `world` is peer-supplied and a bare length check is not a
    // grammar. Something merely short is not therefore a World id, and the
    // answer to one that is not is "there is no anchor to send".
    let anchors = Arc::new(ScriptedAnchors::new(fabric::AnchorResolution::Resolved(1)));
    let handle = LiveHandle::new(Some(anchors));
    assert!(handle
        .anchor(WORLD, [1u8; 16], "text", 4)
        .expect("anchor projection")
        .is_some());
    assert!(handle
        .anchor("not-a-world-id", [1u8; 16], "text", 4)
        .expect("invalid target is not a Body read")
        .is_none());

    // And with no Replica behind it there is nothing to mint from.
    let bare = LiveHandle::new(None);
    assert!(matches!(
        bare.anchor(WORLD, [1u8; 16], "text", 4),
        Err(crate::world::BodyReadFailure::CapabilityUnavailable)
    ));
}

#[test]
fn a_minted_anchor_is_what_a_payload_carries() {
    // The round trip that makes the mint useful: what `anchor` returns has to
    // be something `TransientPayload::validate` accepts, or a browser holding
    // an offset still has no path to a caret.
    let anchors = Arc::new(ScriptedAnchors::new(fabric::AnchorResolution::Resolved(4)));
    let handle = LiveHandle::new(Some(anchors));
    let encoded = handle
        .anchor(WORLD, [1u8; 16], "text", 4)
        .expect("anchor projection")
        .expect("minted");

    let item = TransientItem {
        connection_epoch: [1u8; 16],
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
    use runtime::plane::live::{serve_session, Context};
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

    fn admitted(station: Key) -> AdmittedPeer {
        AdmittedPeer {
            station,
            actor: peer_actor(),
            authority_frontier: frontier(),
            granted_lanes: vec![runtime::plane::stream_kind::CONTROL],
            connection_id: [2u8; 16],
            connection_epoch: [1u8; 16],
            features: 0,
        }
    }

    /// A connected pair, and the Station the server side believes it is serving.
    async fn pair(seed: u8) -> (Arc<dyn comms::Connection>, Arc<dyn comms::Connection>, Key) {
        let net = MemNet::new();
        let client_device = mechanics::actor::device_from_seed(&[seed; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(client_device.clone()));
        let b: Arc<dyn Transport> =
            Arc::new(net.peer(mechanics::actor::device_from_seed(&[seed + 1; 32])));
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
            Key::from_device(&client_device).expect("station"),
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
                        Context {
                            handle: Some(handle),
                            signals: None,
                            worlds: None,
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

    async fn subscribe(connection: &dyn comms::Connection, scopes: Vec<Target>) {
        let (mut send, _recv) = connection.open_bi().await.expect("open");
        let body = LiveControl::Subscribe { scopes }.encode();
        let mut framed = vec![runtime::plane::stream_kind::CONTROL];
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
    use runtime::plane::live::{dial, DialLedger, DialRefusal};
    use runtime::plane::{bounds, stream_kind, Accept, Open, Plane};
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
        // The offer is `feature::LOCAL_SUPPORTED` rather than a hand-picked
        // list, so it moves with the build: a bit joins that constant in the
        // same commit as the code that honours it. Asserted against the
        // concrete bit below rather than against the constant, which would be
        // the same tautology it is elsewhere.
        let net = MemNet::new();
        let local_device = mechanics::actor::device_from_seed(&[81u8; 32]);
        let peer_device = mechanics::actor::device_from_seed(&[82u8; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(local_device.clone()));
        let b: Arc<dyn Transport> = Arc::new(net.peer(peer_device.clone()));

        // The responder's connection is returned rather than dropped when it is
        // done, and that is not tidiness. Dropping a `MemConnection` closes it,
        // and `accept_uni` on the other side resolves to `None` the moment the
        // peer closes — whether or not a flow is already queued. A responder
        // that answered and immediately hung up therefore raced the dialer's
        // read and lost about a quarter of the time, reported as `Unreachable`.
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
                let open = Open::decode_canonical(&raw).expect("canonical opening");
                let accept = Accept {
                    connection_id: open.connection_id,
                    connection_epoch: open.connection_epoch,
                    capability: runtime::plane::Capability {
                        plane: Plane::Live,
                        protocol_version: open.protocol_version,
                        features: open.features & runtime::plane::feature::LOCAL_SUPPORTED,
                    },
                    // Grant one of the two asked for, so the test can tell the
                    // dialer reports what the *responder* said rather than what
                    // it hoped for.
                    granted_lanes: vec![stream_kind::CONTROL],
                };
                let mut send = incoming.connection.open_uni().await.expect("open");
                send.write_all(&accept.encode()).await.expect("write");
                send.finish().expect("finish");
                (open, incoming.connection)
            }
        });

        let live = dial(
            a.as_ref(),
            &Everyone,
            &space(),
            &Key::from_device(&local_device).expect("local"),
            &Key::from_device(&peer_device).expect("peer"),
            [7u8; 16],
        )
        .await
        .expect("dialled");

        let (open, _still_connected) = responder.await.expect("responder");
        assert_eq!(open.plane, Plane::Live);
        assert_eq!(
            open.features,
            runtime::plane::feature::LOCAL_SUPPORTED,
            "what this build implements, and nothing it merely has a name for"
        );
        assert_eq!(
            open.features & runtime::plane::feature::UNSOLICITED_PROVIDE,
            0,
            "nothing serves a chunk without being asked, so it is not offered"
        );
        assert_eq!(
            open.requested_lanes,
            vec![
                stream_kind::CONTROL,
                stream_kind::RELIABLE_SIGNAL,
                stream_kind::MEDIA_GROUP,
                stream_kind::MEDIA_CONTROL,
            ],
            "the ALPN does not type this plane, so the lanes are named"
        );

        // What the responder granted, not what the dialer asked for.
        assert_eq!(live.peer.granted_lanes, vec![stream_kind::CONTROL]);
        assert_eq!(live.peer.connection_id, [7u8; 16]);
        // And the negotiated intersection reaches the plane that honours it.
        assert_eq!(
            live.peer.features,
            runtime::plane::feature::RESIDENCY_HINTS
                | runtime::plane::feature::NATIVE_LIVE_MEDIA
                | runtime::plane::feature::RECIPROCAL_CONVERGE
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
        let local_device = mechanics::actor::device_from_seed(&[85u8; 32]);
        let peer_device = mechanics::actor::device_from_seed(&[86u8; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(local_device.clone()));
        // Deliberately nothing accepting: reaching the transport at all would
        // hang until the deadline, so a prompt answer *is* the assertion.
        let refused = dial(
            a.as_ref(),
            &Nobody,
            &space(),
            &Key::from_device(&local_device).expect("local"),
            &Key::from_device(&peer_device).expect("peer"),
            [8u8; 16],
        )
        .await;
        assert_eq!(refused.err(), Some(DialRefusal::NotAdmitted));
    }
}

/// Residency hints: who to ask first, and nothing more.
mod residency {
    use super::*;
    use runtime::plane::live::{LiveHandle, NoResidency, ResidencyOracle, ResidencyState};

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

/// A received offer costs a name and a content id, and nothing else.
mod offers_on_the_plane {
    use super::*;
    use runtime::signal::{OfferOutcome, PendingOffer};

    fn offer(from: u8, content: u8) -> PendingOffer {
        PendingOffer {
            from: station(from),
            connection_epoch: [2u8; 16],
            content: [content; 32],
            plaintext_len: 1_073_741_824,
            display_name: "big.iso".into(),
            media_type: "application/octet-stream".into(),
        }
    }

    #[test]
    fn an_offer_is_queued_and_survives_what_a_cursor_does_not() {
        // Beside the transient table rather than in it. A slot expires on a TTL
        // because a cursor that stopped moving is stale; an offer that has been
        // sitting for an hour is exactly as valid as it was when it arrived.
        let handle = LiveHandle::new(None);
        let now = Instant::now();
        assert_eq!(handle.offer(offer(1, 7)), OfferOutcome::Queued);

        handle.sweep(now + Duration::from_secs(600));
        assert_eq!(handle.pending_offers().len(), 1, "no TTL reaches an offer");

        // And a disconnect does not take it: the file is still there, and the
        // peer whose laptop slept is still somebody worth fetching from.
        handle.forget(&station(1));
        assert_eq!(handle.pending_offers().len(), 1);
    }

    #[test]
    fn losing_standing_takes_the_offers_with_it() {
        let handle = LiveHandle::new(None);
        handle.offer(offer(1, 7));
        handle.offer(offer(2, 8));
        assert_eq!(handle.forget_offers(&station(1)), 1);
        let left = handle.pending_offers();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].from, station(2));
    }

    #[test]
    fn a_gigabyte_offered_costs_a_name_and_a_content_id() {
        // The whole point. Receiving an offer must not let any member spend this
        // Station's disk by sending a message.
        let handle = LiveHandle::new(None);
        handle.offer(offer(1, 7));
        let held = handle.pending_offers();
        assert_eq!(held[0].plaintext_len, 1_073_741_824);
        // The name is what the sender called it, unrewritten: a name is
        // sanitised where it becomes a path, and rewriting it here would mean
        // the thing shown to a person is not the thing that was sent.
        assert_eq!(held[0].display_name, "big.iso");
    }

    #[test]
    fn taking_an_offer_is_the_only_way_it_leaves() {
        let handle = LiveHandle::new(None);
        handle.offer(offer(1, 7));
        assert!(handle.take_offer(&station(1), &[7u8; 32]).is_some());
        assert!(handle.pending_offers().is_empty());
        assert!(handle.take_offer(&station(1), &[7u8; 32]).is_none());
    }
}

/// A scope flood, and the ledger that ends it.
mod flooding {
    use super::*;
    use runtime::budget::{slots, Evictions, Verdict};
    use runtime::plane::live::Connection;
    use runtime::transient::AdmitOutcome;

    #[test]
    fn a_full_table_evicts_and_the_session_counts_it() {
        // The store's half of the chain. The ledger's half is
        // `the_shipped_eviction_chain_closes_a_flooding_connection`, which
        // drives a real session over a real connection — this used to claim
        // "every link is the shipped code" while calling `evictions.charge`
        // itself, which is the one link that mattered.
        let mut session = Connection::with_capacity(station(1), 4);
        let epoch = [1u8; 16];
        let now = Instant::now();
        let mut scopes = Vec::new();
        for n in 0..4u8 {
            let scope = issue_scope(n);
            scopes.push(scope.clone());
            session.subscribe(scopes.clone());
            assert_eq!(
                session.admit(&presence_item(scope, 1), &epoch, now),
                AdmitOutcome::Stored
            );
        }

        for n in 0..slots::MAX_EVICTIONS_PER_CONNECTION {
            let scope = issue_scope(100 + n as u8);
            scopes.push(scope.clone());
            session.subscribe(scopes.clone());
            assert_eq!(
                session.admit(&presence_item(scope, 1), &epoch, now),
                AdmitOutcome::Evicted,
                "a full table refuses rather than displacing"
            );
        }
        assert_eq!(
            session.counters().evictions,
            slots::MAX_EVICTIONS_PER_CONNECTION as u64,
            "and every refusal is counted"
        );
    }

    #[test]
    fn honest_traffic_never_pays_an_eviction_back() {
        // Why this is its own ledger and not a `Gate` used carefully. A gate
        // decrements its strike counter on every admitted item, so a peer
        // alternating one eviction with eight honest datagrams would sit at zero
        // forever while steadily displacing everybody else.
        let mut evictions = Evictions::new(slots::MAX_EVICTIONS_PER_CONNECTION);
        for _ in 0..(slots::MAX_EVICTIONS_PER_CONNECTION - 1) {
            assert_eq!(evictions.charge(1), Verdict::Allow);
        }
        let charged = evictions.charged();

        let mut session = Connection::with_capacity(station(1), 16);
        session.subscribe(vec![issue_scope(1)]);
        let epoch = [1u8; 16];
        let now = Instant::now();
        for seq in 1..=64 {
            session.admit(&presence_item(issue_scope(1), seq), &epoch, now);
        }

        assert_eq!(
            evictions.charged(),
            charged,
            "sixty-four admitted items paid nothing back"
        );
        assert_eq!(evictions.charge(1), Verdict::Close);
    }
}

/// The send side: this Station telling peers what it is looking at.
///
/// Everything else on this plane is about what *others* say. Without these, a
/// viewer renders every colleague and never itself, and the facepile on a
/// two-person issue shows one face on each screen.
mod publishing {
    use super::*;
    use runtime::plane::live::{LiveHandle, LocalPublication};

    fn scopes(bodies: &[u8]) -> Vec<Target> {
        bodies.iter().map(|b| issue_scope(*b)).collect()
    }

    #[test]
    fn a_declaration_replaces_rather_than_accumulates() {
        // A snapshot of what somebody has open. Incremental would let a client
        // that navigates faster than its messages arrive publish a set neither
        // side agrees on.
        let handle = LiveHandle::new(None);
        assert_eq!(handle.declared(), Vec::new());

        handle.declare_local(scopes(&[1, 2]));
        let (first, held) = (handle.local_generation(), handle.declared());
        assert_eq!(held, scopes(&[1, 2]));

        handle.declare_local(scopes(&[3]));
        let (second, held) = (handle.local_generation(), handle.declared());
        assert_eq!(held, scopes(&[3]), "the old set is gone, not merged");
        assert_ne!(first, second, "and a session can tell cheaply");
    }

    #[test]
    fn declaring_what_is_already_declared_moves_nothing() {
        // The pump re-sends the whole set every tick rather than remembering
        // what it said, so this is the common case and it must not make every
        // session republish twice a second.
        let handle = LiveHandle::new(None);
        handle.declare_local(scopes(&[1]));
        let generation = handle.local_generation();
        handle.declare_local(scopes(&[1]));
        assert_eq!(handle.local_generation(), generation);
    }

    #[test]
    fn an_empty_declaration_is_how_presence_stops() {
        // Not a no-op and not an error. A node looking at nothing is a real
        // state — every tab closed — and it has to be expressible, or presence
        // could only ever grow.
        let handle = LiveHandle::new(None);
        handle.declare_local(scopes(&[1]));
        let before = handle.local_generation();
        handle.declare_local(Vec::new());
        let (after, held) = (handle.local_generation(), handle.declared());
        assert!(held.is_empty());
        assert_ne!(before, after);
    }

    #[test]
    fn what_this_station_publishes_is_not_in_its_own_view() {
        // Two maps rather than one, and this is why: a viewer whose own
        // presence appeared in the table it reads would draw itself beside
        // everybody else, on every screen, for ever.
        let handle = LiveHandle::new(None);
        let now = Instant::now();
        handle.declare_local(scopes(&[1]));
        assert!(
            handle.view(None, now).entries.is_empty(),
            "declaring is not recording"
        );
    }

    #[test]
    fn moving_a_caret_moves_the_local_generation_without_changing_its_scope() {
        let handle = LiveHandle::new(None);
        let scope = caret_scope(1);
        handle.declare_local_publications(vec![LocalPublication {
            scope: scope.clone(),
            payload: TransientPayload::Caret {
                anchor: anchor_bytes("text", 1),
            },
        }]);
        let first = handle.local_generation();
        assert_eq!(handle.declared(), vec![scope.clone()]);

        handle.declare_local_publications(vec![LocalPublication {
            scope: scope.clone(),
            payload: TransientPayload::Caret {
                anchor: anchor_bytes("text", 2),
            },
        }]);
        assert_ne!(handle.local_generation(), first);
        assert_eq!(handle.declared(), vec![scope]);
    }
}

/// Two Stations, and one of them looking at something.
///
/// The property the whole send side exists for: a declaration on one node
/// becomes a face on the other. Driven through the shipped `serve_session` on
/// both ends over a real connection, because the pieces agreeing in isolation is
/// what the last version of this plane already had.
mod two_node_presence {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use comms::mem::MemNet;
    use comms::Transport;
    use runtime::admission::AdmittedPeer;
    use runtime::lifecycle::CancelToken;
    use runtime::plane::live::{serve_session, Context, LiveHandle, LocalPublication};

    fn actor() -> mechanics::ids::ActorId {
        mechanics::ids::ActorId::parse(&format!("act_{}", "cd".repeat(32))).expect("actor")
    }

    fn admitted(station: Key) -> AdmittedPeer {
        AdmittedPeer {
            station,
            actor: actor(),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
            granted_lanes: vec![runtime::plane::stream_kind::CONTROL],
            connection_id: [2u8; 16],
            connection_epoch: [1u8; 16],
            features: 0,
        }
    }

    /// Run one end of a Live session on its own thread, the way the driver does.
    fn serve(
        connection: Arc<dyn comms::Connection>,
        peer: AdmittedPeer,
        handle: Arc<LiveHandle>,
    ) -> (CancelToken, Arc<AtomicBool>) {
        let cancel = CancelToken::new();
        let ended = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let cancel = cancel.clone();
            let ended = ended.clone();
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                tokio::task::LocalSet::new().block_on(&runtime, async move {
                    serve_session(
                        connection,
                        peer,
                        cancel,
                        Context {
                            handle: Some(handle),
                            signals: None,
                            worlds: None,
                            authority: None,
                        },
                    )
                    .await;
                });
                ended.store(true, Ordering::SeqCst);
            }
        });
        (cancel, ended)
    }

    async fn wait_for(
        handle: &LiveHandle,
        within: Duration,
        want: impl Fn(&runtime::plane::live::LiveView) -> bool,
    ) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if want(&handle.view(None, Instant::now())) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn declaring_an_issue_puts_this_station_in_the_other_ones_table() {
        let net = MemNet::new();
        let a_device = mechanics::actor::device_from_seed(&[95u8; 32]);
        let b_device = mechanics::actor::device_from_seed(&[96u8; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(a_device.clone()));
        let b: Arc<dyn Transport> = Arc::new(net.peer(b_device.clone()));
        let accepting = {
            let b = b.clone();
            tokio::spawn(async move { b.accept_connection().await })
        };
        let dialer = a
            .connect_session(b.my_id(), b"lait/session/1")
            .await
            .expect("connect");
        let incoming = accepting.await.expect("accept task").expect("incoming");
        std::mem::forget((a, b));

        let a_handle = Arc::new(LiveHandle::new(None));
        let b_handle = Arc::new(LiveHandle::new(None));
        let a_station = Key::from_device(&a_device).expect("a");
        let b_station = Key::from_device(&b_device).expect("b");

        let (a_cancel, _) = serve(Arc::from(dialer), admitted(b_station), a_handle.clone());
        let (b_cancel, _) = serve(
            Arc::from(incoming.connection),
            admitted(a_station.clone()),
            b_handle.clone(),
        );

        // A opens an issue.
        a_handle.declare_local(vec![issue_scope(3)]);

        assert!(
            wait_for(&b_handle, Duration::from_secs(10), |view| {
                view.entries.len() == 1
            })
            .await,
            "B never saw A"
        );
        let seen = b_handle.view(None, Instant::now());
        assert_eq!(seen.entries[0].station, a_station);
        assert_eq!(seen.entries[0].scope, issue_scope(3));
        assert_eq!(
            seen.entries[0].kind,
            runtime::transient::TransientKind::Presence
        );

        // Cursor payloads use the same replace-all declaration. A collapsed
        // caret becoming a selection changes the payload kind on one scope;
        // the old kind is retired before the new one is published, so B never
        // settles with both decorations for A.
        a_handle.declare_local_publications(vec![LocalPublication {
            scope: caret_scope(3),
            payload: TransientPayload::Caret {
                anchor: anchor_bytes("text", 1),
            },
        }]);
        assert!(
            wait_for(&b_handle, Duration::from_secs(10), |view| {
                view.entries.len() == 1
                    && view.entries[0].kind == runtime::transient::TransientKind::Caret
            })
            .await,
            "B never saw A's caret"
        );
        a_handle.declare_local_publications(vec![LocalPublication {
            scope: caret_scope(3),
            payload: TransientPayload::Selection {
                anchor: anchor_bytes("text", 1),
                focus: anchor_bytes("text", 2),
            },
        }]);
        assert!(
            wait_for(&b_handle, Duration::from_secs(10), |view| {
                view.entries.len() == 1
                    && view.entries[0].kind == runtime::transient::TransientKind::Selection
            })
            .await,
            "B never replaced A's caret with its selection"
        );

        // And A does not see itself: two maps, so a viewer never draws its own
        // face beside everybody else's.
        assert!(a_handle.view(None, Instant::now()).entries.is_empty());

        // A closes the tab. Retirement is an optimisation over expiry, and the
        // difference a person sees is a face going now rather than in ninety
        // seconds — so it is asserted well inside the TTL.
        a_handle.declare_local(Vec::new());
        assert!(
            wait_for(&b_handle, Duration::from_secs(10), |view| view
                .entries
                .is_empty())
            .await,
            "A's face outlived the tab that held it"
        );

        a_cancel.cancel();
        b_cancel.cancel();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_that_ends_stops_counting_as_here() {
        // The test that was missing, and its absence was not neutral. Every
        // proof that presence gates delivery called `departed` by hand — the one
        // call no session made — so `present_stations()` meant "who has ever
        // been here", signals queued for people who had gone home, and the next
        // session that peer opened replayed the backlog. Driving a real teardown
        // is the only thing that could have caught it.
        let net = MemNet::new();
        let a_device = mechanics::actor::device_from_seed(&[97u8; 32]);
        let b_device = mechanics::actor::device_from_seed(&[98u8; 32]);
        let a: Arc<dyn Transport> = Arc::new(net.peer(a_device.clone()));
        let b: Arc<dyn Transport> = Arc::new(net.peer(b_device.clone()));
        let accepting = {
            let b = b.clone();
            tokio::spawn(async move { b.accept_connection().await })
        };
        let dialer = a
            .connect_session(b.my_id(), b"lait/session/1")
            .await
            .expect("connect");
        let incoming = accepting.await.expect("accept task").expect("incoming");
        std::mem::forget((a, b));

        let handle = Arc::new(LiveHandle::new(None));
        let peer = Key::from_device(&a_device).expect("a");
        let (cancel, ended) = serve(
            Arc::from(incoming.connection),
            admitted(peer.clone()),
            handle.clone(),
        );
        // Kept alive: dropping it closes the connection, which would end the
        // session for a reason other than the one under test.
        let _dialer: Arc<dyn comms::Connection> = Arc::from(dialer);

        let here = Instant::now() + Duration::from_secs(10);
        while Instant::now() < here && handle.present_stations().is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            handle.present_stations(),
            vec![peer.clone()],
            "a served session is a peer that is here"
        );

        // A signal for somebody who is about to leave.
        // An `Attention`, not a `Ping`. A ping expects an acknowledgement, and
        // the outbox refuses one for the reason the first version of this test
        // demonstrated: draining it parks the session for a full response
        // deadline waiting on a peer that is on its way out.
        assert!(handle.nudge(
            &peer,
            runtime::plane::Signal::Attention {
                scope: issue_scope(1)
            }
        ));

        cancel.cancel();
        let gone = Instant::now() + Duration::from_secs(10);
        while Instant::now() < gone && !ended.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(ended.load(Ordering::SeqCst), "the session ended");

        assert!(
            handle.present_stations().is_empty(),
            "and a peer that left is not here"
        );
        assert!(
            handle.take_outbound_for_test(&peer).is_empty(),
            "what was queued for them went with them — a queue nothing drains is              the mailbox this plane must not become, and the next session that              peer opened would have replayed it"
        );
    }
}

/// A World's own scope, checked against what that World declared.
///
/// The scope half of what `SignalPolicy::admits_contents` does for signals.
/// Without it, declaring a scope moves the implementation id — every peer sees a
/// different reviewed build — and buys no enforcement at all.
mod declared_scopes {
    use super::*;
    use replica::body::{EncodingId, SchemaId, WorldId};
    use replica::body::{MutationModel, Schema};
    use runtime::plane::live::admits_scope_for_test as admits;
    use runtime::transient::Invalid;
    use runtime::world::Builder;
    use runtime::world::ScopeSchema;
    use runtime::{
        world::Context, world::Effect, world::Intent, world::Projection, world::Query,
        world::Rejection, world::World,
    };

    const PAD: &str = "dev.example.pad";

    struct Pad(Vec<Schema>, Vec<ScopeSchema>);

    impl World for Pad {
        fn id(&self) -> WorldId {
            WorldId::parse(PAD).unwrap()
        }
        fn schemas(&self) -> &[Schema] {
            &self.0
        }
        fn scope_schemas(&self) -> &[ScopeSchema] {
            &self.1
        }
        fn submit(&self, _ctx: &mut Context<'_>, _intent: Intent) -> Result<Effect, Rejection> {
            Err(Rejection::InvalidRequest)
        }
        fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
            Err(Rejection::InvalidRequest)
        }
    }

    fn hosting(scopes: Vec<ScopeSchema>) -> runtime::world::Catalog {
        let schemas = vec![Schema {
            id: SchemaId::parse("entry").unwrap(),
            version: 1,
            encoding: EncodingId::parse("bytes").unwrap(),
            mutation: MutationModel::Atomic,
            readable_predecessors: vec![],
        }];
        let world = Pad(schemas.clone(), scopes.clone());
        Builder::new()
            .register(Arc::new(world))
            .build()
            .expect("registry")
    }

    fn custom(schema: &str, key: &str) -> Target {
        Target::World {
            world: PAD.into(),
            schema: schema.into(),
            key: key.into(),
        }
    }

    fn dragging(max_key_bytes: u32) -> Vec<ScopeSchema> {
        vec![ScopeSchema {
            name: SchemaId::parse("dragging").unwrap(),
            max_key_bytes,
        }]
    }

    #[test]
    fn a_key_past_the_worlds_own_ceiling_is_refused() {
        // The substrate's bound is 128 bytes and the World said 8. A
        // declaration may only tighten, and the tighter number is the one that
        // has to bite — otherwise `max_key_bytes` is a number read at
        // registration and nowhere else.
        let worlds = Some(hosting(dragging(8)));
        assert_eq!(admits(&worlds, &custom("dragging", "12345678")), Ok(()));
        assert_eq!(
            admits(&worlds, &custom("dragging", "123456789")),
            Err(Invalid::Bounds)
        );
    }

    #[test]
    fn a_schema_the_world_never_declared_is_not_declared() {
        // `NotDeclared` and not `Malformed`: it parsed, it is simply not a thing
        // this World says it has. Acting on it would be acting on a schema
        // nobody reviewed.
        let worlds = Some(hosting(dragging(64)));
        assert_eq!(
            admits(&worlds, &custom("resizing", "x")),
            Err(Invalid::NotDeclared)
        );
    }

    #[test]
    fn a_world_this_build_does_not_host_declares_nothing() {
        let worlds = Some(hosting(dragging(64)));
        let elsewhere = Target::World {
            world: "com.example.other".into(),
            schema: "dragging".into(),
            key: "x".into(),
        };
        assert_eq!(admits(&worlds, &elsewhere), Err(Invalid::NotDeclared));
    }

    #[test]
    fn a_world_that_declares_no_scopes_admits_none() {
        // Hosting a World is not hosting its scopes. A World that declared
        // nothing has nothing this plane may carry for it.
        let worlds = Some(hosting(Vec::new()));
        assert_eq!(
            admits(&worlds, &custom("dragging", "x")),
            Err(Invalid::NotDeclared)
        );
    }

    #[test]
    fn the_substrates_own_scopes_are_not_a_worlds_to_declare() {
        // A World does not get to widen or narrow what `Body` means. Those
        // are the substrate's shapes, bounded by the substrate's numbers.
        let worlds = Some(hosting(Vec::new()));
        assert_eq!(admits(&worlds, &issue_scope(1)), Ok(()));
        assert_eq!(admits(&worlds, &caret_scope(1)), Ok(()));
    }

    #[test]
    fn no_registry_checks_nothing_and_is_not_a_licence() {
        // The shape a MemNet harness with no Space behind it runs in. A Station
        // always has a registry, so the permissive case never reaches
        // production — asserted here so that stays a deliberate choice.
        assert_eq!(admits(&None, &custom("anything", "at-all")), Ok(()));
    }
}

/// Signals queued for a peer, and the rules that decide whether they go.
mod nudging {
    use super::*;
    use runtime::plane::live::LiveHandle;
    use runtime::plane::Signal;

    fn nudge(n: u8) -> Signal {
        Signal::WorldSignal {
            world: "com.example.notes".into(),
            schema: "assigned".into(),
            payload: vec![n],
        }
    }

    #[test]
    fn presence_is_the_gate_and_nothing_is_held_for_the_absent() {
        // The composition Linear cannot make: it picks a delivery channel from
        // what somebody configured months ago, and this picks from whether they
        // are here now. A peer with no session is not queued for — holding
        // signals would make this a mailbox, which is the one thing a plane that
        // keeps nothing must not become.
        let handle = LiveHandle::new(None);
        assert!(handle.present_stations().is_empty());

        handle.arrived(&station(1));
        assert_eq!(handle.present_stations(), vec![station(1)]);

        handle.departed(&station(1));
        assert!(
            handle.present_stations().is_empty(),
            "and leaving is leaving"
        );

        // The gate itself, which nothing asserted before: a nudge for somebody
        // who is not here is refused at the queue rather than held in it.
        assert!(
            !handle.nudge(&station(1), nudge(1)),
            "a peer that left is not queued for"
        );
        assert!(
            !handle.nudge(&station(9), nudge(1)),
            "and neither is one that never arrived"
        );
        assert!(handle.take_outbound_for_test(&station(9)).is_empty());
    }

    #[test]
    fn a_peer_with_two_sessions_is_here_until_both_end() {
        // `MAX_LIVE_SESSIONS_PER_STATION` is two, so a laptop and a phone are
        // one peer twice. The first to hang up has not left, and a refcount is
        // the difference between that and a person who vanishes when they close
        // one of two tabs.
        let handle = LiveHandle::new(None);
        handle.arrived(&station(1));
        handle.arrived(&station(1));
        handle.departed(&station(1));
        assert_eq!(handle.present_stations(), vec![station(1)]);
        handle.departed(&station(1));
        assert!(handle.present_stations().is_empty());
    }

    #[test]
    fn a_signal_that_expects_an_answer_is_not_queueable() {
        // An outbox is one-way. The queue is drained by a session beat, so a
        // signal waiting on a round trip parks that session for a full response
        // deadline — no datagram read, no presence published, no revalidation —
        // on behalf of a caller that has already returned.
        //
        // Found by a test that queued a `Ping` and then waited ten seconds for a
        // session to notice it had been cancelled.
        let handle = LiveHandle::new(None);
        handle.arrived(&station(1));
        assert!(
            !handle.nudge(&station(1), Signal::Ping { nonce: [1u8; 16] }),
            "a ping expects an acknowledgement"
        );
        assert!(
            handle.nudge(
                &station(1),
                Signal::Attention {
                    scope: issue_scope(1)
                }
            ),
            "an attention expects nothing"
        );
        assert_eq!(handle.take_outbound_for_test(&station(1)).len(), 1);
    }

    #[test]
    fn a_full_outbox_refuses_the_newest_rather_than_dropping_the_oldest() {
        // Both are facts. Evicting the oldest to keep the newest loses the older
        // one to make room for a thing of exactly equal standing, which is the
        // rule a cursor stream wants and a signal does not.
        let handle = LiveHandle::new(None);
        handle.arrived(&station(1));
        for n in 0..16u8 {
            assert!(handle.nudge(&station(1), nudge(n)), "{n} was refused early");
        }
        assert!(!handle.nudge(&station(1), nudge(99)));

        let taken = handle.take_outbound_for_test(&station(1));
        assert_eq!(taken.len(), 16);
        assert_eq!(
            taken[0],
            nudge(0),
            "the oldest is still there, which is the point"
        );
    }

    #[test]
    fn taking_is_taking_and_a_session_that_ends_drops_what_it_held() {
        let handle = LiveHandle::new(None);
        handle.arrived(&station(1));
        assert!(handle.nudge(&station(1), nudge(1)));
        assert_eq!(handle.take_outbound_for_test(&station(1)).len(), 1);
        assert!(handle.take_outbound_for_test(&station(1)).is_empty());

        // A peer that disconnects mid-fanout leaves nothing behind, and the
        // clearing rides `departed` rather than a second call somebody has to
        // remember — which is what it was, and what nothing ever called.
        handle.arrived(&station(2));
        assert!(handle.nudge(&station(2), nudge(2)));
        handle.departed(&station(2));
        assert!(handle.take_outbound_for_test(&station(2)).is_empty());

        // And a peer holding two sessions keeps its queue until the second ends.
        // Clearing on the first would drop a notification because somebody shut
        // one of two laptops.
        handle.arrived(&station(3));
        handle.arrived(&station(3));
        assert!(handle.nudge(&station(3), nudge(3)));
        handle.departed(&station(3));
        assert_eq!(handle.take_outbound_for_test(&station(3)).len(), 1);
    }
}

#[test]
fn relayable_gives_a_subscriber_only_other_peers_in_scopes_it_watches() {
    // The read side of a supporter's fanout (`PRESENCE_RELAY`). A supporter holds
    // several peers' presence in one shared handle; `relayable` is what it sends
    // to ONE subscriber: every OTHER peer in the scopes THAT subscriber watches,
    // never the subscriber itself, never a scope it did not ask for.
    let now = Instant::now();
    let handle = LiveHandle::new(None);
    let scope_a = caret_scope(1);
    let scope_b = caret_scope(2);
    handle.record(&station(1), &caret_item(scope_a.clone(), 1, 4), now);
    handle.record(&station(2), &caret_item(scope_a.clone(), 1, 7), now);
    handle.record(&station(3), &caret_item(scope_b.clone(), 1, 2), now);

    // Subscriber station 1, watching scope_a: sees station 2 only — not itself,
    // not station 3's scope_b (unsubscribed).
    let relayed = handle.relayable(&station(1), &[scope_a.clone()]);
    assert_eq!(
        relayed.len(),
        1,
        "one other peer in the one subscribed scope"
    );
    assert_eq!(relayed[0].0, station(2));
    assert_eq!(relayed[0].1.scope, scope_a);
    // Forwarded UNRESOLVED — the raw recorded payload, for the subscriber to
    // resolve against its OWN Bodies, never this Station's resolved position.
    assert!(matches!(
        relayed[0].1.payload,
        TransientPayload::Caret { .. }
    ));

    // Watching both scopes: both other peers, still never itself.
    let mut both: Vec<_> = handle
        .relayable(&station(1), &[scope_a, scope_b])
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    both.sort();
    assert_eq!(both, vec![station(2), station(3)]);
}

#[test]
fn relayed_presence_round_trips_carrying_its_origin() {
    let item = caret_item(caret_scope(1), 5, 9);
    let relayed = RelayedPresence {
        origin: station(7).key_bytes(),
        item: item.clone(),
    };
    let bytes = relayed.encode();
    let decoded = RelayedPresence::decode_canonical(&bytes).expect("round-trips");
    assert_eq!(
        decoded.origin,
        station(7).key_bytes(),
        "the author survives"
    );
    assert_eq!(decoded.item, item, "the item survives");
    // A truncated frame is refused, not silently half-read.
    assert!(RelayedPresence::decode_canonical(&bytes[..bytes.len() - 1]).is_err());
}

#[test]
fn a_body_subscription_relays_field_carets_under_that_body() {
    // The passive-viewer path: a reader watching a whole issue (a Body scope),
    // with no cursor of its own and so no field, must still be relayed the FIELD
    // carets peers hold inside that issue. A field-exact match alone would relay
    // nothing to it — the bug that made a passive tab see no caret.
    let now = Instant::now();
    let handle = LiveHandle::new(None);
    let field_here = caret_scope(1); // Field { body: 1, "text" }
    let field_other = caret_scope(2); // Field { body: 2, "text" }
    handle.record(&station(1), &caret_item(field_here.clone(), 1, 4), now);
    handle.record(&station(2), &caret_item(field_other.clone(), 1, 9), now);

    // Subscriber station 9 watches the whole issue body 1 (a Body scope).
    let watching_body_1 = issue_scope(1);
    let relayed = handle.relayable(&station(9), &[watching_body_1]);
    assert_eq!(relayed.len(), 1, "the field caret under body 1, not body 2");
    assert_eq!(relayed[0].0, station(1));
    assert_eq!(relayed[0].1.scope, field_here);
}
