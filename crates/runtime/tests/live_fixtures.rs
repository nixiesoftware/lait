//! The transient core: what it accepts, what it refuses, and what it forgets.
//!
//! Everything here is pure. No transport, no Station, no clock but the one the
//! test supplies — which is the point of landing this layer on its own, because
//! the properties that matter are all about what a peer can make this Station
//! do, and none of them need a network to demonstrate.

use std::time::{Duration, Instant};

use runtime::budget::{deadline, slots};
use runtime::transient::{
    AdmitOutcome, LiveControl, Target, TransientError, TransientItem, TransientKind,
    TransientPayload, TransientStore, MAX_ANCHOR_BYTES, MAX_SCOPE_FIELD_BYTES,
    MAX_TRANSIENT_ITEM_BYTES,
};

const EPOCH: [u8; 16] = [7u8; 16];
const OTHER_EPOCH: [u8; 16] = [9u8; 16];

fn issue_scope() -> Target {
    Target::Body {
        world: "com.example.notes".into(),
        body: [1u8; 16],
    }
}

fn caret_scope(field: &str) -> Target {
    Target::Field {
        world: "com.example.notes".into(),
        body: [1u8; 16],
        field: field.into(),
    }
}

/// An encoded anchor naming `path`, built the way a real one is so the decode
/// inside `validate` sees the real shape.
fn anchor(path: &str) -> Vec<u8> {
    replica::Anchor {
        format_version: 1,
        body: [1u8; 32],
        path: path.into(),
        anchored_to: None,
        offset: 0,
        after: false,
        taken_at: replica::Version::empty(),
    }
    .encode()
}

fn item(scope: Target, payload: TransientPayload, seq: u64) -> TransientItem {
    TransientItem {
        connection_epoch: EPOCH,
        seq,
        scope,
        payload,
    }
}

#[test]
fn an_epoch_is_compared_and_never_ordered() {
    // Two epochs are 16 random bytes each. They have no order, so the only
    // answerable question is whether this is the one this session was admitted
    // at — and the outcome says exactly that rather than "older".
    let mut store = TransientStore::new();
    let now = Instant::now();
    let mut stray = item(issue_scope(), TransientPayload::Presence, 1);
    stray.connection_epoch = OTHER_EPOCH;
    assert_eq!(store.admit(&stray, &EPOCH, now), AdmitOutcome::WrongEpoch);
    assert!(store.is_empty(), "and nothing was stored on the way to no");

    let mine = item(issue_scope(), TransientPayload::Presence, 1);
    assert_eq!(store.admit(&mine, &EPOCH, now), AdmitOutcome::Stored);
}

#[test]
fn an_out_of_order_datagram_does_not_undo_a_newer_one() {
    // Datagrams reorder. A cursor that jumps backward because an older packet
    // arrived late is worse than one that misses an update.
    let mut store = TransientStore::new();
    let now = Instant::now();
    assert_eq!(
        store.admit(
            &item(issue_scope(), TransientPayload::Presence, 5),
            &EPOCH,
            now
        ),
        AdmitOutcome::Stored
    );
    assert_eq!(
        store.admit(
            &item(issue_scope(), TransientPayload::Presence, 3),
            &EPOCH,
            now
        ),
        AdmitOutcome::Stale
    );
    assert_eq!(
        store.admit(
            &item(issue_scope(), TransientPayload::Presence, 5),
            &EPOCH,
            now
        ),
        AdmitOutcome::Stale,
        "a duplicate is stale too — equal is not newer"
    );
}

#[test]
fn a_payload_a_scope_cannot_carry_is_refused() {
    // The legality table, one illegal pairing per scope. This is what makes
    // "at most two kinds per scope" true, which is what makes the slot ceiling
    // a derivation rather than a guess.
    let mut store = TransientStore::new();
    let now = Instant::now();
    let illegal = [
        (issue_scope(), TransientPayload::Typing),
        (
            issue_scope(),
            TransientPayload::Caret {
                anchor: anchor("text"),
            },
        ),
        (caret_scope("text"), TransientPayload::Presence),
        (caret_scope("text"), TransientPayload::Typing),
        (
            Target::Typing {
                world: "com.example.notes".into(),
                body: [1u8; 16],
                field: "text".into(),
            },
            TransientPayload::Presence,
        ),
        (
            Target::Content { content: [3u8; 32] },
            TransientPayload::Presence,
        ),
    ];
    for (scope, payload) in illegal {
        assert_eq!(
            store.admit(&item(scope.clone(), payload, 1), &EPOCH, now),
            AdmitOutcome::Refused(TransientError::IllegalForScope),
            "{scope:?}"
        );
    }
    assert!(store.is_empty());
}

#[test]
fn an_anchor_cannot_name_a_field_the_scope_did_not() {
    // Load-bearing rather than tidy. The anchor's path becomes a loro container
    // key on the resolve path, so an anchor free to name any path is a peer
    // choosing which container this Station touches. Binding it to the
    // subscribed scope means a peer can only ask about what it said it was
    // watching.
    let mut store = TransientStore::new();
    let now = Instant::now();
    let elsewhere = item(
        caret_scope("text"),
        TransientPayload::Caret {
            anchor: anchor("description"),
        },
        1,
    );
    assert_eq!(
        store.admit(&elsewhere, &EPOCH, now),
        AdmitOutcome::Refused(TransientError::AnchorOutsideScope)
    );

    let matching = item(
        caret_scope("text"),
        TransientPayload::Caret {
            anchor: anchor("text"),
        },
        1,
    );
    assert_eq!(store.admit(&matching, &EPOCH, now), AdmitOutcome::Stored);
}

#[test]
fn an_oversize_anchor_is_refused_before_it_is_resolved() {
    let mut store = TransientStore::new();
    let now = Instant::now();
    let huge = item(
        caret_scope("text"),
        TransientPayload::Caret {
            anchor: vec![0u8; MAX_ANCHOR_BYTES + 1],
        },
        1,
    );
    assert_eq!(
        store.admit(&huge, &EPOCH, now),
        AdmitOutcome::Refused(TransientError::Bounds)
    );

    // And a path that is inside the anchor bound but past the field bound.
    let long_path = "f".repeat(MAX_SCOPE_FIELD_BYTES + 1);
    let deep = item(
        caret_scope(&long_path),
        TransientPayload::Caret {
            anchor: anchor(&long_path),
        },
        1,
    );
    assert_eq!(
        store.admit(&deep, &EPOCH, now),
        AdmitOutcome::Refused(TransientError::Bounds)
    );
}

#[test]
fn a_retirement_cannot_be_undone_by_a_datagram_already_in_flight() {
    // Retirement and a datagram on the wire race by nature. Losing that race
    // rebuilds the slot for a full TTL after the peer said it was done, so the
    // watermark outlives the slot.
    let mut store = TransientStore::new();
    let now = Instant::now();
    let caret = |seq| {
        item(
            caret_scope("text"),
            TransientPayload::Caret {
                anchor: anchor("text"),
            },
            seq,
        )
    };
    assert_eq!(store.admit(&caret(4), &EPOCH, now), AdmitOutcome::Stored);
    store.retire(&caret_scope("text"), TransientKind::Caret, EPOCH, 6, now);
    assert!(store
        .get(&caret_scope("text"), TransientKind::Caret)
        .is_none());

    assert_eq!(store.admit(&caret(5), &EPOCH, now), AdmitOutcome::Retired);
    assert_eq!(
        store.admit(&caret(6), &EPOCH, now),
        AdmitOutcome::Retired,
        "at the watermark is covered, not above it"
    );
    // Past the watermark is a peer that started again, which is allowed.
    assert_eq!(store.admit(&caret(7), &EPOCH, now), AdmitOutcome::Stored);
}

#[test]
fn a_watermark_does_not_outlive_the_flight_it_guards() {
    // It only has to cover datagrams that could still be in the air. Keeping it
    // forever would be a table that grows with every scope anyone ever watched.
    let mut store = TransientStore::new();
    let now = Instant::now();
    store.retire(&caret_scope("text"), TransientKind::Caret, EPOCH, 6, now);
    let later = now + deadline::CARET_GRACE + Duration::from_secs(1);
    store.sweep(later);
    let caret = item(
        caret_scope("text"),
        TransientPayload::Caret {
            anchor: anchor("text"),
        },
        5,
    );
    assert_eq!(store.admit(&caret, &EPOCH, later), AdmitOutcome::Stored);
}

#[test]
fn everything_expires_without_anyone_saying_goodbye() {
    // A tab closing, a laptop sleeping and a network dropping all deliver
    // nothing. Expiry is the only mechanism that works for all three.
    let mut store = TransientStore::new();
    let now = Instant::now();
    store.admit(
        &item(issue_scope(), TransientPayload::Presence, 1),
        &EPOCH,
        now,
    );
    store.admit(
        &item(
            caret_scope("text"),
            TransientPayload::Caret {
                anchor: anchor("text"),
            },
            1,
        ),
        &EPOCH,
        now,
    );
    assert_eq!(store.len(), 2);

    // A caret goes first: a cursor nobody has moved for half a minute is a
    // cursor nobody is behind.
    let after_cursor = now + deadline::CURSOR_TTL + Duration::from_secs(1);
    store.sweep(after_cursor);
    assert_eq!(store.len(), 1, "the caret expired and the presence did not");

    let after_presence = now + deadline::PRESENCE_TTL + Duration::from_secs(1);
    store.sweep(after_presence);
    assert!(store.is_empty());
}

#[test]
fn a_disconnect_forgets_what_that_session_believed() {
    let mut store = TransientStore::new();
    let now = Instant::now();
    store.admit(
        &item(issue_scope(), TransientPayload::Presence, 1),
        &EPOCH,
        now,
    );
    assert_eq!(store.forget_session(&OTHER_EPOCH), 0);
    assert_eq!(store.forget_session(&EPOCH), 1);
    assert!(store.is_empty());
}

#[test]
fn a_full_table_evicts_rather_than_growing() {
    // Nothing here is correctness, so the cost of a full table is a stale
    // cursor. An unbounded one would be a Station a Space can make allocate
    // without ever committing anything.
    let mut store = TransientStore::with_capacity(2);
    let now = Instant::now();
    for n in 0..2u8 {
        let scope = Target::Body {
            world: "com.example.notes".into(),
            body: [n; 16],
        };
        assert_eq!(
            store.admit(&item(scope, TransientPayload::Presence, 1), &EPOCH, now),
            AdmitOutcome::Stored
        );
    }
    let overflow = Target::Body {
        world: "com.example.notes".into(),
        body: [99u8; 16],
    };
    assert_eq!(
        store.admit(&item(overflow, TransientPayload::Presence, 1), &EPOCH, now),
        AdmitOutcome::Evicted
    );
    assert_eq!(store.len(), 2, "and the table did not grow past its bound");

    // An update to something already held is not an eviction — the slot exists.
    assert_eq!(
        store.admit(
            &item(
                Target::Body {
                    world: "com.example.notes".into(),
                    body: [0u8; 16],
                },
                TransientPayload::Presence,
                2
            ),
            &EPOCH,
            now
        ),
        AdmitOutcome::Stored
    );
}

#[test]
fn a_malformed_item_is_refused_inside_its_own_ceiling() {
    // The decode order is what makes each check protect the next: the outer
    // ceiling first so no allocation is sized by a peer, then postcard, then
    // re-encode equality so one item has one spelling.
    assert_eq!(
        TransientItem::decode_canonical(&vec![0u8; MAX_TRANSIENT_ITEM_BYTES + 1]),
        Err(TransientError::TooLarge)
    );
    for corpus in [
        &b""[..],
        &b"\xff\xff\xff\xff"[..],
        &b"not postcard at all"[..],
    ] {
        assert!(
            TransientItem::decode_canonical(corpus).is_err(),
            "{corpus:?} decoded"
        );
    }

    // A legal item round-trips, and its encoding is the only one it has.
    let legal = item(issue_scope(), TransientPayload::Presence, 1);
    let bytes = legal.encode();
    assert_eq!(TransientItem::decode_canonical(&bytes), Ok(legal));
}

#[test]
fn a_maximal_selection_fits_the_item_ceiling() {
    // The ceiling has to admit the largest thing the protocol can legitimately
    // produce, or it is a bound on honest clients rather than hostile ones.
    let field = "f".repeat(MAX_SCOPE_FIELD_BYTES);
    let selection = item(
        caret_scope(&field),
        TransientPayload::Selection {
            anchor: anchor(&field),
            focus: anchor(&field),
        },
        u64::MAX,
    );
    let encoded = selection.encode();
    assert!(
        encoded.len() <= MAX_TRANSIENT_ITEM_BYTES,
        "a maximal selection encodes to {} bytes, past the {MAX_TRANSIENT_ITEM_BYTES} ceiling",
        encoded.len()
    );
    assert_eq!(
        TransientItem::decode_canonical(&encoded).map(|i| i.seq),
        Ok(u64::MAX)
    );
}

#[test]
fn a_subscription_cannot_name_more_scopes_than_the_connection_may_hold() {
    let scopes: Vec<Target> = (0..=slots::MAX_SUBSCRIBED_SCOPES_PER_CONNECTION)
        .map(|n| Target::Body {
            world: "com.example.notes".into(),
            body: [n as u8; 16],
        })
        .collect();
    assert_eq!(
        LiveControl::Subscribe { scopes }.validate(),
        Err(TransientError::Bounds)
    );

    let fits: Vec<Target> = (0..slots::MAX_SUBSCRIBED_SCOPES_PER_CONNECTION)
        .map(|n| Target::Body {
            world: "com.example.notes".into(),
            body: [n as u8; 16],
        })
        .collect();
    let control = LiveControl::Subscribe { scopes: fits };
    assert_eq!(control.validate(), Ok(()));
    assert_eq!(
        LiveControl::decode_canonical(&control.encode()),
        Ok(control)
    );
}

#[test]
fn a_world_a_scope_names_is_parsed_rather_than_measured() {
    // A World id has a shape, and something that is merely short is not
    // therefore one. `Signal::WorldSignal` has always parsed; a scope used to
    // measure, which meant the two World-facing shapes disagreed about what a
    // World id *is* — so a scope and a signal about the same World could stop
    // matching.
    let mut store = TransientStore::new();
    let now = Instant::now();
    for bad in [
        String::new(),
        // No dot-separated labels: long enough, and not a World id.
        "w".repeat(MAX_SCOPE_FIELD_BYTES + 1),
        "wwwwwwww".into(),
        // Past the grammar's own 63-byte ceiling, well inside the scope bound
        // that used to be the only check.
        format!("com.example.{}", "a".repeat(60)),
        "-com.example".into(),
    ] {
        let scope = Target::Body {
            world: bad.clone(),
            body: [1u8; 16],
        };
        assert_eq!(
            store.admit(&item(scope, TransientPayload::Presence, 1), &EPOCH, now),
            AdmitOutcome::Refused(TransientError::Malformed),
            "{bad:?} is not a World id"
        );
    }
}

#[test]
fn a_field_path_is_bounded_rather_than_parsed() {
    // The other half, and the reason both rules exist. A field is a name inside
    // a Body's collaborative schema — the substrate has no grammar for it — but
    // the bound is load-bearing, because the path reaches loro's container
    // namespace on the receiver.
    let mut store = TransientStore::new();
    let now = Instant::now();
    for bad in [String::new(), "f".repeat(MAX_SCOPE_FIELD_BYTES + 1)] {
        let scope = Target::Field {
            world: "com.example.notes".into(),
            body: [1u8; 16],
            field: bad.clone(),
        };
        assert_eq!(
            store.admit(&item(scope, TransientPayload::Presence, 1), &EPOCH, now),
            AdmitOutcome::Refused(TransientError::Bounds),
            "{} is not a field path",
            bad.len()
        );
    }
}

/// A dialer that can tell "another generation" from "not there".
#[cfg(test)]
mod provider_refusals {
    use runtime::fetch::ProviderRefusal;
    use runtime::plane::Refusal;

    #[test]
    fn exactly_one_refusal_is_worth_telling_someone_about() {
        // Every other refusal is a peer exercising a policy it is entitled to,
        // and a fetcher that reported those would be reporting normal
        // operation. A version mismatch is the one an operator can fix — and
        // the one that, collapsed into "unavailable", presents as an
        // intermittent network fault for a week.
        assert!(
            ProviderRefusal::Refused(Refusal::UnsupportedVersion { supported: 2 }).is_actionable()
        );
        for quiet in [
            ProviderRefusal::Unreachable,
            ProviderRefusal::Refused(Refusal::Refused),
            ProviderRefusal::Refused(Refusal::Malformed),
            ProviderRefusal::Unintelligible,
        ] {
            assert!(!quiet.is_actionable(), "{quiet:?}");
        }
    }

    #[test]
    fn a_peer_that_answered_nonsense_is_not_a_peer_that_refused() {
        // Ours to explain rather than theirs to have sent, so it is its own
        // variant. Folding it into `Refused` would attribute our own decode
        // failure to the peer's policy.
        assert_ne!(
            ProviderRefusal::Unintelligible,
            ProviderRefusal::Refused(Refusal::Refused)
        );
        assert_ne!(
            ProviderRefusal::Unintelligible,
            ProviderRefusal::Unreachable
        );
    }
}
