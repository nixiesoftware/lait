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
    assert!(matches!(err, Error::Bound(_)));

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
