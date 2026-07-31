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

    handle.set_partial(true);
    assert!(handle.view(None, now).partial);

    // Setting it to what it already is is not a change a reader should see.
    let generation = handle.generation();
    handle.set_partial(true);
    assert_eq!(handle.generation(), generation);
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
