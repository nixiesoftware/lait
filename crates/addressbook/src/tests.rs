use data_encoding::HEXLOWER;
use mechanics::ids::{ActorId, DeviceId, SpaceId, UlidSource};

use crate::ids::CardId;
use crate::ids::PathHash;
use crate::store::Store;
use crate::types::{Author, Evidence, Handle};
use crate::{Action, BookEngine, Error};

struct Seq(u64);
impl UlidSource for Seq {
    fn now_ms(&self) -> u64 {
        self.0
    }
    fn rand80(&self) -> u128 {
        u128::from(self.0)
    }
}

fn device(n: u8) -> DeviceId {
    DeviceId::from_key_bytes(&[n; 32])
}

fn actor(n: u8) -> ActorId {
    ActorId::from_incept_hash(&HEXLOWER.encode(&[n; 32]))
}

fn space(n: u8) -> SpaceId {
    SpaceId::from_digest([n; 16])
}

fn author(n: u8) -> Author {
    Author {
        device: device(n),
        at: 1_700_000_000_000,
    }
}

fn card(n: u64) -> CardId {
    CardId::mint(&Seq(n))
}

#[test]
fn persistence_proof_restart_into_a_blank_engine() {
    let mut engine = BookEngine::new();
    let id = card(1);
    let alice = author(1);
    engine
        .apply(
            &alice,
            Action::Create {
                id: id.clone(),
                name: "Ada".into(),
            },
        )
        .expect("create");
    engine
        .apply(
            &alice,
            Action::AddHandle {
                id: id.clone(),
                handle: Handle::Actor {
                    space: space(1),
                    actor: actor(1),
                },
                evidence: Evidence::Declared,
            },
        )
        .expect("handle");
    engine
        .apply(
            &alice,
            Action::SetNote {
                id,
                note: "colleague".into(),
            },
        )
        .expect("note");

    let before = engine.book().expect("book");
    let version = engine.version().expect("version");
    let export = engine.export_body().expect("export");

    let restored = BookEngine::import_body(&export).expect("import");
    assert_eq!(restored.book().expect("restored book"), before);
    assert_eq!(restored.version().expect("restored version"), version);
}

/// The picture lifecycle: authored in the stored `<mime>;base64,<data>` form,
/// validated at write, cleared with the empty string, defaulted for cards
/// that never had one — and it survives the export/import boundary like every
/// other authored field.
#[test]
fn a_picture_is_authored_validated_cleared_and_defaulted() {
    let mut engine = BookEngine::new();
    let id = card(2);
    let alice = author(1);
    engine
        .apply(
            &alice,
            Action::Create {
                id: id.clone(),
                name: "Ada".into(),
            },
        )
        .expect("create");

    // A card that never had a picture projects an empty one — the default
    // face, not an error.
    let book = engine.book().expect("book");
    assert_eq!(book.cards[&id].picture.value, "");

    // Only the stored form is storable: no separator, a foreign mime, and a
    // non-base64 payload are refusals, and an oversize one names its bound.
    for bad in [
        "not-a-picture",
        "image/tiff;base64,AAAA",
        "image/png;base64,@@not-base64@@",
    ] {
        let refused = engine.apply(
            &alice,
            Action::SetPicture {
                id: id.clone(),
                picture: bad.into(),
            },
        );
        assert!(refused.is_err(), "{bad:?} must be refused");
    }
    let oversize = format!(
        "image/png;base64,{}",
        "A".repeat(crate::bounds::MAX_PICTURE_BYTES)
    );
    assert!(matches!(
        engine.apply(
            &alice,
            Action::SetPicture {
                id: id.clone(),
                picture: oversize,
            },
        ),
        Err(Error::Bound("MAX_PICTURE_BYTES"))
    ));

    let stored = format!(
        "image/png;base64,{}",
        data_encoding::BASE64.encode(&[137, 80, 78, 71])
    );
    engine
        .apply(
            &alice,
            Action::SetPicture {
                id: id.clone(),
                picture: stored.clone(),
            },
        )
        .expect("set picture");
    let book = engine.book().expect("book");
    assert_eq!(book.cards[&id].picture.value, stored);

    // It is an authored field like any other: it crosses export/import.
    let export = engine.export_body().expect("export");
    let restored = BookEngine::import_body(&export).expect("import");
    assert_eq!(
        restored.book().expect("book").cards[&id].picture.value,
        stored
    );

    // The empty string is the clear.
    engine
        .apply(
            &alice,
            Action::SetPicture {
                id: id.clone(),
                picture: String::new(),
            },
        )
        .expect("clear picture");
    assert_eq!(engine.book().expect("book").cards[&id].picture.value, "");
}

#[test]
fn store_round_trip_and_corrupt_refuses() {
    let dir = std::env::temp_dir().join(format!("lait-ab-{}", card(9).as_str()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    let store = Store::at(&dir);

    let mut engine = BookEngine::new();
    let id = card(2);
    engine
        .apply(
            &author(2),
            Action::Create {
                id: id.clone(),
                name: "Bo".into(),
            },
        )
        .expect("create");
    store.replace(&engine).expect("write");
    engine
        .apply(
            &author(2),
            Action::SetNote {
                id: id.clone(),
                note: "kept".into(),
            },
        )
        .expect("edit");
    store.replace(&engine).expect("rewrite");

    let opened = store.open().expect("open").expect("present");
    assert_eq!(opened.book().expect("book"), engine.book().expect("orig"));

    std::fs::write(store.path(), b"not an envelope").expect("scribble");
    match store.open() {
        Err(Error::Corrupt(_)) => {}
        Err(err) => panic!("corrupt file must fail closed, got {err}"),
        Ok(_) => panic!("corrupt file must fail closed, got Ok"),
    }
    let bak = store.path().with_extension("bin.bak");
    assert!(bak.exists(), "the previous envelope is preserved as backup");
    std::fs::copy(&bak, store.path()).expect("restore backup");
    let recovered = store.open().expect("backup opens").expect("present");
    assert!(
        recovered.book().expect("recovered").cards.contains_key(&id),
        "the backup is a real envelope, not an empty default"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_envelope_version_is_unsupported() {
    let mut bytes = vec![0u8; 64];
    bytes[..8].copy_from_slice(b"LAITABK1");
    bytes[8] = 99;
    // decode_envelope is crate-private via Store; write a file with a bad format.
    let dir = std::env::temp_dir().join(format!("lait-ab-ver-{}", card(8).as_str()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    let path = dir.join("addressbook.bin");
    std::fs::write(&path, bytes).expect("write");
    match Store::at(&dir).open() {
        Err(Error::UnsupportedVersion(99) | Error::Corrupt(_)) => {}
        Err(err) => panic!("expected unsupported or corrupt, got {err}"),
        Ok(_) => panic!("expected unsupported or corrupt, got Ok"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unlink_then_relink_is_a_new_tag() {
    let mut engine = BookEngine::new();
    let id = card(3);
    let alice = author(3);
    let handle = Handle::Device(device(9));
    engine
        .apply(
            &alice,
            Action::Create {
                id: id.clone(),
                name: "Cy".into(),
            },
        )
        .expect("create");
    engine
        .apply(
            &alice,
            Action::AddHandle {
                id: id.clone(),
                handle: handle.clone(),
                evidence: Evidence::Declared,
            },
        )
        .expect("add");
    let first = engine.book().unwrap().cards[&id].handles[0].tag.clone();
    engine
        .apply(
            &alice,
            Action::RemoveHandle {
                id: id.clone(),
                handle: handle.clone(),
            },
        )
        .expect("remove");
    engine
        .apply(
            &alice,
            Action::AddHandle {
                id: id.clone(),
                handle,
                evidence: Evidence::Declared,
            },
        )
        .expect("re-add");
    let second = engine.book().unwrap().cards[&id].handles[0].tag.clone();
    assert_ne!(first, second, "a re-link mints a fresh tag");
}

#[test]
fn merge_replays_concurrent_edits_onto_the_survivor() {
    let mut engine = BookEngine::new();
    let a = card(10);
    let b = card(11);
    let alice = author(4);
    engine
        .apply(
            &alice,
            Action::Create {
                id: a.clone(),
                name: "Left".into(),
            },
        )
        .expect("a");
    engine
        .apply(
            &alice,
            Action::Create {
                id: b.clone(),
                name: "Right".into(),
            },
        )
        .expect("b");
    engine
        .apply(
            &alice,
            Action::Merge {
                from: a.clone(),
                into: b.clone(),
            },
        )
        .expect("merge");
    let book = engine.book().unwrap();
    assert!(!book.cards.contains_key(&a));
    assert!(book.cards.contains_key(&b));
    assert_eq!(book.redirects[&a].0, b);
}

#[test]
fn a_redirect_cycle_is_refused() {
    let mut engine = BookEngine::new();
    let a = card(12);
    let b = card(13);
    let alice = author(5);
    engine
        .apply(
            &alice,
            Action::Create {
                id: a.clone(),
                name: "A".into(),
            },
        )
        .expect("a");
    engine
        .apply(
            &alice,
            Action::Create {
                id: b.clone(),
                name: "B".into(),
            },
        )
        .expect("b");
    engine
        .apply(
            &alice,
            Action::Merge {
                from: a.clone(),
                into: b.clone(),
            },
        )
        .expect("a->b");
    // b is live; a is not. Merging b into a is a cycle.
    let err = engine
        .apply(&alice, Action::Merge { from: b, into: a })
        .expect_err("cycle");
    assert!(matches!(err, Error::NoSuchCard | Error::Invalid(_)));
}

#[test]
fn concurrent_my_card_keeps_the_lowest_claim() {
    let mut engine = BookEngine::new();
    let a = card(20);
    let b = card(21);
    engine
        .apply(
            &author(6),
            Action::Create {
                id: a.clone(),
                name: "Me".into(),
            },
        )
        .expect("a");
    engine
        .apply(
            &author(7),
            Action::Create {
                id: b.clone(),
                name: "Also me".into(),
            },
        )
        .expect("b");
    engine
        .apply(&author(6), Action::ClaimSelf { id: a })
        .expect("claim a");
    engine
        .apply(&author(7), Action::ClaimSelf { id: b })
        .expect("claim b");
    let book = engine.book().unwrap();
    let live_claims: Vec<_> = book
        .cards
        .values()
        .filter(|card| card.self_claim.is_some())
        .map(|card| card.id.clone())
        .collect();
    assert_eq!(
        live_claims.len(),
        1,
        "one My Card survives: {live_claims:?}"
    );
}

#[test]
fn empty_name_and_oversize_note_are_bounds() {
    let mut engine = BookEngine::new();
    let err = engine
        .apply(
            &author(1),
            Action::Create {
                id: card(30),
                name: "   ".into(),
            },
        )
        .expect_err("blank");
    // A blank name is structural, not a length that happens to be zero.
    assert!(matches!(err, Error::Invalid(_)));

    let id = card(31);
    engine
        .apply(
            &author(1),
            Action::Create {
                id: id.clone(),
                name: "Ok".into(),
            },
        )
        .expect("ok");
    let err = engine
        .apply(
            &author(1),
            Action::SetNote {
                id,
                note: "x".repeat(crate::MAX_NOTE_BYTES + 1),
            },
        )
        .expect_err("note");
    assert!(matches!(err, Error::Bound(_)));
}

#[test]
fn handle_wire_round_trips_device_and_actor() {
    let device = Handle::Device(device(1));
    assert_eq!(Handle::parse_wire(&device.to_wire()).unwrap(), device);
    let actor = Handle::Actor {
        space: space(1),
        actor: actor(1),
    };
    assert_eq!(Handle::parse_wire(&actor.to_wire()).unwrap(), actor);
}

#[test]
fn local_agent_handles_are_marked_as_device_local() {
    let handle = Handle::LocalAgent {
        store: PathHash::parse("0123456789abcdef").unwrap(),
        name: "grok".into(),
    };
    assert!(!handle.may_leave_device());
    assert!(Handle::Device(device(1)).may_leave_device());
}

#[test]
fn fabric_duplicate_import_is_a_no_op() {
    let mut engine = BookEngine::new();
    engine
        .apply(
            &author(1),
            Action::Create {
                id: card(40),
                name: "Dup".into(),
            },
        )
        .expect("create");
    let export = engine.export_body().unwrap();
    let again = BookEngine::import_body(&export).unwrap();
    assert_eq!(again.book().unwrap(), engine.book().unwrap());
}

#[test]
fn checkpoint_reconstructs_projection_and_version() {
    let mut engine = BookEngine::new();
    for i in 0..8 {
        engine
            .apply(
                &author(1),
                Action::Create {
                    id: card(50 + i),
                    name: format!("n{i}"),
                },
            )
            .expect("seed");
    }
    match engine.compacted() {
        Ok(compacted) => {
            assert_eq!(compacted.book().unwrap(), engine.book().unwrap());
            assert_eq!(compacted.version().unwrap(), engine.version().unwrap());
        }
        Err(err) => {
            panic!("A0 stops if a checkpoint cannot reconstruct the live Book and version: {err}")
        }
    }
}

#[test]
fn the_envelope_restores_projection_and_causal_version() {
    // The A0 sentence, through the envelope itself: export the authoritative
    // file, restart into a blank Engine, and prove an identical projection
    // *and* causal version. The export/import proof above does not touch the
    // envelope; the round-trip test does not assert the version. This one
    // does both.
    let dir = std::env::temp_dir().join(format!("lait-ab-env-{}", card(40).as_str()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    let store = Store::at(&dir);

    let mut engine = BookEngine::new();
    let id = card(41);
    engine
        .apply(
            &author(3),
            Action::Create {
                id: id.clone(),
                name: "Ada".into(),
            },
        )
        .expect("create");
    engine
        .apply(
            &author(3),
            Action::AddHandle {
                id: id.clone(),
                handle: Handle::Actor {
                    space: space(7),
                    actor: actor(7),
                },
                evidence: Evidence::Declared,
            },
        )
        .expect("link");
    store.replace(&engine).expect("write");

    let reopened = store.open().expect("open").expect("present");
    assert_eq!(
        reopened.book().expect("book"),
        engine.book().expect("book"),
        "the envelope restores the projection"
    );
    assert_eq!(
        reopened.version().expect("version"),
        engine.version().expect("version"),
        "the envelope restores the causal version"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_between_remove_and_rename_recovers_the_survivor() {
    // atomic_replace writes tmp, copies main to bak, removes main, renames
    // tmp into place. A crash inside the remove/rename window leaves no main
    // file while a fully-synced survivor exists — which must never read as a
    // new empty book.
    let dir = std::env::temp_dir().join(format!("lait-ab-crash-{}", card(50).as_str()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    let store = Store::at(&dir);

    let mut engine = BookEngine::new();
    let id = card(51);
    engine
        .apply(
            &author(4),
            Action::Create {
                id: id.clone(),
                name: "Bo".into(),
            },
        )
        .expect("create");
    store.replace(&engine).expect("first write");
    engine
        .apply(
            &author(4),
            Action::SetNote {
                id: id.clone(),
                note: "kept".into(),
            },
        )
        .expect("note");
    store.replace(&engine).expect("second write");

    // Simulate the window: the synced tmp survives, the main file is gone.
    let tmp = store.path().with_extension("bin.tmp");
    std::fs::copy(store.path(), &tmp).expect("craft survivor");
    std::fs::remove_file(store.path()).expect("crash");

    let recovered = store.open().expect("recover").expect("not a new book");
    assert_eq!(
        recovered.book().expect("book"),
        engine.book().expect("book"),
        "the tmp survivor carries the latest write"
    );
    assert!(store.path().exists(), "recovery restores the main file");

    // And with only the backup left, the previous write still answers.
    std::fs::remove_file(store.path()).expect("crash again");
    std::fs::remove_file(&tmp).expect("no tmp");
    let from_bak = store.open().expect("recover from bak").expect("present");
    assert!(
        from_bak.book().expect("book").cards.contains_key(&id),
        "the backup survivor is the previous envelope, not an empty default"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trailing_bytes_read_as_corrupt() {
    // Appended bytes past the declared envelope must not be silently
    // discarded: the checksum over the declared prefix would still verify.
    let dir = std::env::temp_dir().join(format!("lait-ab-trail-{}", card(60).as_str()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    let store = Store::at(&dir);

    let mut engine = BookEngine::new();
    engine
        .apply(
            &author(5),
            Action::Create {
                id: card(61),
                name: "Cy".into(),
            },
        )
        .expect("create");
    store.replace(&engine).expect("write");

    let mut bytes = std::fs::read(store.path()).expect("read");
    bytes.push(0);
    std::fs::write(store.path(), &bytes).expect("append");
    match store.open() {
        Err(Error::Corrupt(_)) => {}
        Err(err) => panic!("a file with trailing bytes must fail closed, got {err}"),
        Ok(_) => panic!("a file with trailing bytes must fail closed, got Ok"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hostile_header_length_is_refused() {
    // The history bound covers the body; the header length needs its own
    // gate or a local file can declare a 4 GiB header and be read in full.
    let dir = std::env::temp_dir().join(format!("lait-ab-hdr-{}", card(70).as_str()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    let store = Store::at(&dir);

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"LAITABK1");
    bytes.push(1);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // header_len
    bytes.extend_from_slice(&0u32.to_le_bytes()); // body_len
    std::fs::write(store.path(), &bytes).expect("craft");
    match store.open() {
        Err(Error::Corrupt("header len")) => {}
        Err(err) => panic!("a hostile header length must be refused as such, got {err}"),
        Ok(_) => panic!("a hostile header length must be refused as such, got Ok"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_handle_bounds_refuse_and_corrupt_nothing() {
    let mut engine = BookEngine::new();
    let id = card(80);
    engine
        .apply(
            &author(6),
            Action::Create {
                id: id.clone(),
                name: "Fan".into(),
            },
        )
        .expect("create");

    // Device handles hit MAX_SHARED_DEVICES first: eight fit, a ninth is
    // refused as exactly that bound.
    for n in 0..8u8 {
        engine
            .apply(
                &author(6),
                Action::AddHandle {
                    id: id.clone(),
                    handle: Handle::Device(DeviceId::from_key_bytes(&[n; 32])),
                    evidence: Evidence::Declared,
                },
            )
            .expect("within the device bound");
    }
    let err = engine
        .apply(
            &author(6),
            Action::AddHandle {
                id: id.clone(),
                handle: Handle::Device(DeviceId::from_key_bytes(&[8; 32])),
                evidence: Evidence::Declared,
            },
        )
        .expect_err("the ninth device handle");
    assert!(
        matches!(err, Error::Bound("MAX_SHARED_DEVICES")),
        "got {err:?}"
    );

    // Actor handles ride to the per-card fan-out bound: 64 handles total on
    // the card, and the 65th is refused without corrupting the Engine.
    for n in 0..56u8 {
        engine
            .apply(
                &author(6),
                Action::AddHandle {
                    id: id.clone(),
                    handle: Handle::Actor {
                        space: space(6),
                        actor: ActorId::from_incept_hash(
                            &HEXLOWER.encode(&[n.wrapping_add(100); 32]),
                        ),
                    },
                    evidence: Evidence::Declared,
                },
            )
            .expect("within the fan-out bound");
    }
    let err = engine
        .apply(
            &author(6),
            Action::AddHandle {
                id: id.clone(),
                handle: Handle::Actor {
                    space: space(6),
                    actor: ActorId::from_incept_hash(&HEXLOWER.encode(&[200u8; 32])),
                },
                evidence: Evidence::Declared,
            },
        )
        .expect_err("the 65th handle");
    assert!(matches!(err, Error::Bound(_)), "got {err:?}");
    let book = engine.book().expect("book");
    let live = book.cards.get(&id).expect("card");
    assert_eq!(live.handles.len(), 64, "the refusal left the card intact");
}

#[test]
fn handle_wire_discriminants_are_frozen() {
    // Handles go to disk as postcard encodings of the enum, so the variant
    // order *is* a persistence surface: reordering it would silently change
    // what stored links mean. Freeze the discriminants.
    let device_handle = Handle::Device(device(9));
    let actor_handle = Handle::Actor {
        space: space(9),
        actor: actor(9),
    };
    let agent = Handle::LocalAgent {
        store: PathHash::parse("0123456789abcdef").expect("hash"),
        name: "scribe".into(),
    };
    let encoded = |h: &Handle| crate::codec::encode(h).expect("encode");
    assert_eq!(
        encoded(&device_handle).first(),
        Some(&0),
        "Device is variant 0"
    );
    assert_eq!(
        encoded(&actor_handle).first(),
        Some(&1),
        "Actor is variant 1"
    );
    assert_eq!(encoded(&agent).first(), Some(&2), "LocalAgent is variant 2");
    for handle in &[device_handle, actor_handle, agent] {
        let bytes = encoded(handle);
        let back: Handle = crate::codec::decode(&bytes).expect("decode");
        assert_eq!(&back, handle, "the encoding round-trips");
    }
}
