//! Plan 13 F0 item 6 — the clean break, made executable.
//!
//! This docket replaces the paged manifest root and the flat store manifest
//! with authenticated indexes, and carries **no reader** for either. That is a
//! choice, and a choice needs a test: an older store must be refused before any
//! write, never opened, upgraded, or served.
//!
//! No new machinery is needed for it. `StoreMarker` already front-loads an
//! independently parseable `MAGIC || version` prefix ahead of anything else in
//! the store, and every store-open path classifies through it. F1 bumps
//! `STORE_VERSION` when the indexed format lands, and every home written by the
//! paged writer becomes `UnsupportedStoreVersion` at that moment. What this
//! file pins is that the mechanism actually behaves that way — including the
//! part that is easy to lose, which is *refusing without touching anything*.

use mechanics::ids::SpaceId;
use replica::marker::{Invalid, StoreMarker, MAX_MARKER, STORE_MAGIC, STORE_VERSION};

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

/// A marker as some other generation of the format wrote it: our magic, a
/// version that is not ours.
fn marker_of_version(version: u8) -> Vec<u8> {
    let current = StoreMarker::new(&space()).expect("marker");
    let mut bytes = current.encode();
    bytes[STORE_MAGIC.len()] = version;
    bytes
}

#[test]
fn a_store_from_another_generation_is_refused_by_version() {
    for version in [0u8, STORE_VERSION + 1, 7, 255] {
        assert_eq!(
            StoreMarker::classify(&marker_of_version(version)),
            Err(Invalid::UnsupportedStoreVersion { found: version }),
            "version {version} must be refused as unsupported, not opened"
        );
    }
}

#[test]
fn the_version_is_read_before_the_body_is_trusted() {
    // The property that makes the clean break safe rather than merely stated.
    // An older store's *body* is a shape this build has no decoder for, so the
    // refusal has to come from the fixed prefix — before any postcard decode,
    // any checksum, and any allocation past the header bound.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(STORE_MAGIC);
    bytes.push(STORE_VERSION + 1);
    bytes.extend_from_slice(b"this is not a postcard MarkerBody and never will be");

    assert_eq!(
        StoreMarker::classify(&bytes),
        Err(Invalid::UnsupportedStoreVersion {
            found: STORE_VERSION + 1
        }),
        "an unsupported version must be reported as such, not as corruption — \
         the two lead a user to different actions"
    );
}

#[test]
fn a_foreign_directory_is_told_apart_from_an_old_store() {
    // Recreation guidance differs: one is "you pointed at the wrong folder",
    // the other is "this store predates a clean break". Collapsing them into a
    // single failure would make the guidance wrong half the time.
    assert_eq!(
        StoreMarker::classify(b"not lait at all"),
        Err(Invalid::NotAReplicaStore)
    );
    assert_eq!(StoreMarker::classify(&[]), Err(Invalid::NotAReplicaStore));
    assert_eq!(
        StoreMarker::classify(&marker_of_version(STORE_VERSION + 1)),
        Err(Invalid::UnsupportedStoreVersion {
            found: STORE_VERSION + 1
        })
    );
}

#[test]
fn a_corrupt_marker_of_the_current_version_is_neither() {
    let current = StoreMarker::new(&space()).expect("marker");
    let mut bytes = current.encode();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    assert_eq!(
        StoreMarker::classify(&bytes),
        Err(Invalid::CorruptStoreMarker)
    );
}

#[test]
fn an_oversized_header_is_refused_before_it_is_parsed() {
    let bytes = vec![0u8; MAX_MARKER + 1];
    assert_eq!(
        StoreMarker::classify(&bytes),
        Err(Invalid::CorruptStoreMarker),
        "the size bound is checked before the magic, so a huge foreign file \
         cannot make us allocate on the way to rejecting it"
    );
}

#[test]
fn the_current_version_still_opens() {
    // The control. A clean break that also refuses the current format is not a
    // clean break, it is an outage.
    let marker = StoreMarker::new(&space()).expect("marker");
    let classified = StoreMarker::classify(&marker.encode()).expect("current store opens");
    assert_eq!(classified.version, STORE_VERSION);
    assert_eq!(classified, marker);
}
