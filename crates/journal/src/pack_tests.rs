//! The pack log's crash matrix: every boundary in [`crate::PACK_FAULT_POINTS`],
//! plus the failure shapes the prior format never had — torn tails, torn
//! length prefixes, stale seals from a prior slot life, a genuine seal
//! embedded in an object payload, and a flush that reports failure — and the
//! reader mechanics the semantic layer stands on: views pin their generation
//! across compaction, and the graveyard drains only when they let go.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::medium::{Medium, MemMedium, ReadAt, SlotWriter};
use crate::{object_content_hash, Failure, PackStore, Provenance};

fn obj(n: u8) -> Vec<u8> {
    vec![n; 64]
}

fn medium() -> Arc<MemMedium> {
    Arc::new(MemMedium::new())
}

fn open(medium: &Arc<MemMedium>) -> PackStore {
    PackStore::open(medium.clone(), "hot").expect("pack opens")
}

/// Read a slot's full contents so a test can perform byte surgery on it.
fn slot_bytes(medium: &MemMedium, name: &str) -> Vec<u8> {
    let (writer, read) = medium.open_slot(name).unwrap();
    let len = usize::try_from(writer.len()).unwrap();
    let mut bytes = vec![0u8; len];
    read.read_at(0, &mut bytes).unwrap();
    bytes
}

fn write_slot_bytes(medium: &MemMedium, name: &str, bytes: &[u8]) {
    let (mut writer, _) = medium.open_slot(name).unwrap();
    writer.truncate(0).unwrap();
    writer.append(bytes).unwrap();
}

#[test]
fn a_fresh_pack_round_trips_and_reopens() {
    for dir in [false, true] {
        let root = std::env::temp_dir().join(format!("lait-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let medium: Arc<dyn Medium> = if dir {
            Arc::new(crate::DirMedium::open(&root).unwrap())
        } else {
            Arc::new(MemMedium::new())
        };
        let mut pack = PackStore::open(medium.clone(), "hot").unwrap();
        assert_eq!(pack.sequence(), 0);
        assert!(pack.manifest().is_none());

        let seq = pack.commit(&[obj(1), obj(2)], b"m1".to_vec()).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(pack.read(&object_content_hash(&obj(1))).unwrap(), obj(1));

        drop(pack);
        let pack = PackStore::open(medium, "hot").unwrap();
        assert_eq!(pack.sequence(), 1);
        assert_eq!(pack.manifest(), Some(b"m1".as_slice()));
        assert_eq!(pack.read(&object_content_hash(&obj(2))).unwrap(), obj(2));
        if dir {
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

#[test]
fn an_unsealed_tail_falls_off_at_open() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();

    // A commit whose seal never landed: object records with no seal after
    // them. Recovery must expose exactly the sealed state.
    let sealed = slot_bytes(&medium, "hot-0");
    let mut torn = sealed.clone();
    torn.extend_from_slice(&[0xAB; 200]);
    write_slot_bytes(&medium, "hot-0", &torn);

    drop(pack);
    let pack = open(&medium);
    assert_eq!(pack.sequence(), 1);
    assert_eq!(pack.manifest(), Some(b"m1".as_slice()));
    // The unverified tail was truncated, not merely ignored.
    assert_eq!(slot_bytes(&medium, "hot-0").len(), sealed.len());
}

#[test]
fn a_torn_length_prefix_cannot_derail_recovery() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();

    // Garbage that starts like a record and claims an enormous length.
    let mut torn = slot_bytes(&medium, "hot-0");
    torn.extend_from_slice(b"lps1");
    torn.extend_from_slice(&u32::MAX.to_le_bytes());
    torn.extend_from_slice(&[0x11; 64]);
    write_slot_bytes(&medium, "hot-0", &torn);

    drop(pack);
    let pack = open(&medium);
    assert_eq!(pack.sequence(), 1);
}

#[test]
fn a_torn_object_fails_its_seal_and_recovery_steps_back() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();
    let sealed_at_one = slot_bytes(&medium, "hot-0").len();
    pack.commit(&[obj(2)], b"m2".to_vec()).unwrap();

    // The reordering model: commit 2's seal persisted, its object bytes did
    // not. The seal is structurally perfect; only the delta re-hash can
    // reject it.
    let mut torn = slot_bytes(&medium, "hot-0");
    let object_at = sealed_at_one + 8; // past the record's magic + length
    for byte in torn.get_mut(object_at..object_at + 8).unwrap() {
        *byte ^= 0xFF;
    }
    write_slot_bytes(&medium, "hot-0", &torn);

    drop(pack);
    let pack = open(&medium);
    assert_eq!(pack.sequence(), 1, "recovery steps back to the intact seal");
    assert_eq!(pack.manifest(), Some(b"m1".as_slice()));
    assert!(!pack.contains(&object_content_hash(&obj(2))));
}

#[test]
fn a_stale_seal_from_a_prior_slot_life_cannot_join() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();
    pack.commit(&[obj(2)], b"m2".to_vec()).unwrap();
    let old_life = slot_bytes(&medium, "hot-0");

    // Compaction moves the store to hot-1 under a fresh salt.
    pack.compact(&|_| true).unwrap();
    assert_eq!(pack.sequence(), 2);

    // A stale tail resurfaces bytes from the previous life — including its
    // genuinely checksummed seals. The new salt must make them foreign.
    let mut haunted = slot_bytes(&medium, "hot-1");
    let clean_len = haunted.len();
    haunted.extend_from_slice(&old_life);
    write_slot_bytes(&medium, "hot-1", &haunted);

    drop(pack);
    let pack = open(&medium);
    assert_eq!(pack.sequence(), 2);
    assert_eq!(pack.manifest(), Some(b"m2".as_slice()));
    assert_eq!(slot_bytes(&medium, "hot-1").len(), clean_len);
}

#[test]
fn a_seal_embedded_in_an_object_cannot_be_elected() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();

    // Store this pack's own bytes — real seal record included — as an
    // object: the pack-backup-in-the-pack case. Same salt, genuine checks;
    // only the offset binding says those bytes are data here, not records.
    let backup = slot_bytes(&medium, "hot-0");
    pack.commit(&[backup], b"m2".to_vec()).unwrap();
    let sealed_at_two = slot_bytes(&medium, "hot-0").len();
    pack.commit(&[obj(3)], b"m3".to_vec()).unwrap();

    // Tear commit 3's seal so the scan has to walk past the embedded one.
    let mut torn = slot_bytes(&medium, "hot-0");
    for byte in torn.get_mut(sealed_at_two + 8..).unwrap().iter_mut() {
        *byte ^= 0xFF;
    }
    write_slot_bytes(&medium, "hot-0", &torn);

    drop(pack);
    let pack = open(&medium);
    assert_eq!(pack.sequence(), 2, "the embedded seal did not win");
    assert_eq!(pack.manifest(), Some(b"m2".as_slice()));
}

#[test]
fn a_fault_at_each_commit_point_recovers_the_prior_state() {
    for point in ["pack-objects", "pack-seal", "pack-flush"] {
        let medium = medium();
        let mut pack = open(&medium);
        pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();

        let armed = point;
        let mut pack = PackStore::open(medium.clone(), "hot")
            .unwrap()
            .with_fault_injector(Box::new(move |name| name == armed));
        let refused = pack.commit(&[obj(2)], b"m2".to_vec());
        assert!(refused.is_err(), "{point}: the commit must report failure");

        drop(pack);
        let pack = open(&medium);
        assert_eq!(pack.sequence(), 1, "{point}: the prior commit stands");
        assert!(!pack.contains(&object_content_hash(&obj(2))), "{point}");
    }
}

/// A medium whose slots can be told to fail their next flushes.
struct FlushFails {
    inner: MemMedium,
    armed: Arc<AtomicBool>,
}

struct FlushFailsWriter {
    inner: Box<dyn SlotWriter>,
    armed: Arc<AtomicBool>,
}

impl SlotWriter for FlushFailsWriter {
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn append(&mut self, bytes: &[u8]) -> Result<u64, std::io::Error> {
        self.inner.append(bytes)
    }
    fn flush(&mut self) -> Result<(), std::io::Error> {
        if self.armed.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("flush refused"));
        }
        self.inner.flush()
    }
    fn truncate(&mut self, new_len: u64) -> Result<(), std::io::Error> {
        self.inner.truncate(new_len)
    }
}

impl Medium for FlushFails {
    fn open_slot(
        &self,
        name: &str,
    ) -> Result<(Box<dyn SlotWriter>, Arc<dyn ReadAt>), std::io::Error> {
        let (writer, read) = self.inner.open_slot(name)?;
        Ok((
            Box::new(FlushFailsWriter {
                inner: writer,
                armed: self.armed.clone(),
            }),
            read,
        ))
    }
    fn remove_slot(&self, name: &str) -> Result<(), std::io::Error> {
        self.inner.remove_slot(name)
    }
    fn slot_names(&self) -> Result<Vec<String>, std::io::Error> {
        self.inner.slot_names()
    }
}

#[test]
fn a_flush_failure_poisons_the_writer_until_reopen() {
    let armed = Arc::new(AtomicBool::new(false));
    let shared: Arc<dyn Medium> = Arc::new(FlushFails {
        inner: MemMedium::new(),
        armed: armed.clone(),
    });
    let mut pack = PackStore::open(shared.clone(), "hot").unwrap();
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();

    armed.store(true, Ordering::SeqCst);
    assert!(matches!(
        pack.commit(&[obj(2)], b"m2".to_vec()),
        Err(Failure::OutcomeUnknown)
    ));
    // Poisoned: even a commit that could succeed is refused until reopen.
    armed.store(false, Ordering::SeqCst);
    assert!(matches!(
        pack.commit(&[obj(3)], b"m3".to_vec()),
        Err(Failure::OutcomeUnknown)
    ));

    // OutcomeUnknown means unknown: the refused commit's bytes may or may
    // not be durable. Reopening decides from what verifies — here the memory
    // medium kept every append, so the commit legitimately stands.
    drop(pack);
    let mut pack = PackStore::open(shared, "hot").unwrap();
    assert_eq!(pack.sequence(), 2, "the appended commit verified at reopen");
    assert_eq!(pack.manifest(), Some(b"m2".as_slice()));
    pack.commit(&[obj(3)], b"m3".to_vec())
        .expect("a reopened writer is clean");
}

#[test]
fn compaction_drops_the_dead_and_reopens_forward() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1), obj(2)], b"m1".to_vec()).unwrap();
    pack.commit(&[obj(3)], b"m2".to_vec()).unwrap();

    let keep = object_content_hash(&obj(3));
    pack.compact(&|hash| *hash == keep).unwrap();
    assert!(pack.contains(&keep));
    assert!(!pack.contains(&object_content_hash(&obj(1))));
    assert_eq!(pack.read(&keep).unwrap(), obj(3));
    assert_eq!(
        medium.slot_names().unwrap(),
        vec!["hot-1".to_owned()],
        "no reader held the old generation, so it is gone"
    );

    drop(pack);
    let mut pack = open(&medium);
    assert_eq!(pack.sequence(), 2);
    assert_eq!(pack.manifest(), Some(b"m2".as_slice()));
    // The pack keeps accepting commits in its new generation.
    pack.commit(&[obj(4)], b"m3".to_vec()).unwrap();
    assert_eq!(pack.sequence(), 3);
}

#[test]
fn a_view_pins_its_generation_until_it_lets_go() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1), obj(2)], b"m1".to_vec()).unwrap();
    let view = pack.view();

    // Compact away obj(2). The view predates the compaction: it must keep
    // reading BOTH objects, at their old offsets, from the retired slot.
    let keep = object_content_hash(&obj(1));
    pack.compact(&|hash| *hash == keep).unwrap();
    assert_eq!(pack.graveyard_depth(), 1, "the view holds the generation");
    assert!(
        medium.slot_names().unwrap().contains(&"hot-0".to_owned()),
        "a pinned slot is not deleted"
    );
    assert_eq!(view.read(&object_content_hash(&obj(2))).unwrap(), obj(2));
    assert_eq!(view.read(&keep).unwrap(), obj(1));
    // The store itself answers from the new generation.
    assert!(!pack.contains(&object_content_hash(&obj(2))));

    drop(view);
    pack.sweep_graveyard();
    assert_eq!(pack.graveyard_depth(), 0);
    assert_eq!(medium.slot_names().unwrap(), vec!["hot-1".to_owned()]);
}

#[test]
fn a_fault_at_each_pre_authority_compaction_point_leaves_the_old_pack_whole() {
    for point in [
        "pack-compact-objects",
        "pack-compact-seal",
        "pack-compact-flush",
    ] {
        let medium = medium();
        let mut pack = open(&medium);
        pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();
        drop(pack);

        let mut pack = PackStore::open(medium.clone(), "hot")
            .unwrap()
            .with_fault_injector(Box::new(move |name| name == point));
        assert!(pack.compact(&|_| true).is_err(), "{point}");
        assert_eq!(pack.sequence(), 1, "{point}");
        assert_eq!(pack.read(&object_content_hash(&obj(1))).unwrap(), obj(1));
        assert_eq!(
            medium.slot_names().unwrap(),
            vec!["hot-0".to_owned()],
            "{point}: the partial successor is discarded, not leaked"
        );

        drop(pack);
        let pack = open(&medium);
        assert_eq!(pack.sequence(), 1, "{point}");
        assert_eq!(pack.manifest(), Some(b"m1".as_slice()), "{point}");
    }
}

#[test]
fn a_crash_before_compaction_retire_elects_the_successor() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();
    drop(pack);

    let mut pack = PackStore::open(medium.clone(), "hot")
        .unwrap()
        .with_fault_injector(Box::new(|name| name == "pack-compact-retire"));
    pack.compact(&|_| true)
        .expect("retirement is cleanup; losing it must not fail the compaction");
    drop(pack);

    // Both slots survived the "crash". Election must pick the successor and
    // clean up the loser.
    assert_eq!(medium.slot_names().unwrap().len(), 2);
    let pack = open(&medium);
    assert_eq!(pack.sequence(), 1);
    assert_eq!(pack.read(&object_content_hash(&obj(1))).unwrap(), obj(1));
    assert_eq!(medium.slot_names().unwrap(), vec!["hot-1".to_owned()]);
}

#[test]
fn checkpoints_bound_the_recovery_walk() {
    let medium = medium();
    let mut pack = open(&medium);
    for n in 0..70u8 {
        pack.commit(&[obj(n)], vec![n]).unwrap();
    }
    assert!(
        pack.seals_since_checkpoint() < 64,
        "a checkpoint seal was written along the way"
    );

    drop(pack);
    let pack = open(&medium);
    assert_eq!(pack.sequence(), 70);
    assert_eq!(pack.manifest(), Some([69u8].as_slice()));
    for n in 0..70u8 {
        assert_eq!(pack.read(&object_content_hash(&obj(n))).unwrap(), obj(n));
    }
}

#[test]
fn an_object_is_stored_once_no_matter_how_often_it_arrives() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1), obj(1)], b"m1".to_vec()).unwrap();
    let after_first = slot_bytes(&medium, "hot-0").len();
    pack.commit(&[obj(1)], b"m2".to_vec()).unwrap();
    let growth = slot_bytes(&medium, "hot-0").len() - after_first;
    assert!(
        growth < 64,
        "a re-arriving object adds only a seal, not bytes ({growth})"
    );
    assert_eq!(pack.read(&object_content_hash(&obj(1))).unwrap(), obj(1));
}

#[test]
fn a_migrated_pack_continues_the_source_sequence_and_names_it() {
    let medium = medium();
    let mut pack = open(&medium);
    let provenance = Provenance {
        source_manifest: object_content_hash(b"the-source-manifest"),
        source_counter: 41,
    };
    let seq = pack
        .migrate_commit(
            &mut [obj(1)].into_iter().map(Ok),
            b"m1".to_vec(),
            provenance,
        )
        .unwrap();
    assert_eq!(
        seq, 42,
        "the first sequence sits strictly above the counter"
    );
    assert_eq!(pack.provenance(), Some(&provenance));

    // Provenance names the birth commit only; ordinary commits shed it.
    drop(pack);
    let mut pack = open(&medium);
    assert_eq!(pack.provenance(), Some(&provenance));
    assert_eq!(pack.commit(&[obj(2)], b"m2".to_vec()).unwrap(), 43);
    assert_eq!(pack.provenance(), None);

    // And it is a birth right only: a pack with history refuses it.
    assert!(pack
        .migrate_commit(
            &mut [obj(3)].into_iter().map(Ok),
            b"m3".to_vec(),
            provenance
        )
        .is_err());
}

#[test]
fn two_prefixes_share_one_medium_without_meeting() {
    let medium = medium();
    let mut hot = open(&medium);
    let mut cold = PackStore::open(medium.clone(), "cold").expect("cold pack opens");
    hot.commit(&[obj(1)], b"hot".to_vec()).unwrap();
    cold.commit(&[obj(200)], b"cold".to_vec()).unwrap();
    assert!(!cold.contains(&object_content_hash(&obj(1))));

    drop(hot);
    drop(cold);
    let hot = open(&medium);
    let cold = PackStore::open(medium, "cold").unwrap();
    assert_eq!(hot.manifest(), Some(b"hot".as_slice()));
    assert_eq!(cold.manifest(), Some(b"cold".as_slice()));
}

#[test]
fn a_torn_birth_seal_loses_the_election_to_its_predecessor() {
    let medium = medium();
    let mut pack = PackStore::open(medium.clone(), "hot")
        .unwrap()
        .with_fault_injector(Box::new(|name| name == "pack-compact-retire"));
    pack.commit(&[obj(1), obj(2)], b"m1".to_vec()).unwrap();
    pack.compact(&|_| true).unwrap();
    drop(pack);
    assert_eq!(medium.slot_names().unwrap().len(), 2);

    // The reordering model, aimed at the successor: its checkpoint seal
    // persisted whole, one of its copied objects did not. The seal has no
    // delta — only birth-seal checkpoint verification can reject it, and it
    // must, or the intact hot-0 would be deleted as the election's loser.
    // (72.. is the first object's payload: past the 64-byte slot header and
    // the record's 8-byte magic+length.)
    let mut torn = slot_bytes(&medium, "hot-1");
    let sealed = torn.len();
    for byte in torn.get_mut(72..80).unwrap() {
        *byte ^= 0xFF;
    }
    assert!(torn.len() == sealed);
    write_slot_bytes(&medium, "hot-1", &torn);

    let pack = open(&medium);
    assert_eq!(pack.sequence(), 1, "the intact predecessor is elected");
    assert_eq!(pack.manifest(), Some(b"m1".as_slice()));
    assert_eq!(pack.read(&object_content_hash(&obj(1))).unwrap(), obj(1));
    assert_eq!(pack.read(&object_content_hash(&obj(2))).unwrap(), obj(2));
    assert_eq!(
        medium.slot_names().unwrap(),
        vec!["hot-0".to_owned()],
        "the torn successor is the loser, not the survivor"
    );
}

#[test]
fn a_sole_torn_birth_seal_fails_the_open_rather_than_founding_fresh() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();
    pack.compact(&|_| true).unwrap();
    drop(pack);
    assert_eq!(medium.slot_names().unwrap(), vec!["hot-1".to_owned()]);

    // Rot inside the sole slot's birth checkpoint (72.. = first object
    // payload): with no sibling to elect, recovery must refuse — resetting
    // would erase acknowledged history, and the damaged slot must survive
    // for whatever restores it.
    let mut torn = slot_bytes(&medium, "hot-1");
    for byte in torn.get_mut(72..80).unwrap() {
        *byte ^= 0xFF;
    }
    write_slot_bytes(&medium, "hot-1", &torn);

    assert!(matches!(
        PackStore::open(medium.clone(), "hot"),
        Err(Failure::Integrity(crate::Defect::CorruptObject))
    ));
    assert_eq!(
        medium.slot_names().unwrap(),
        vec!["hot-1".to_owned()],
        "the damaged slot is preserved, never reset"
    );
}

#[test]
fn bytes_wearing_another_slots_name_are_history_not_a_slot() {
    let medium = medium();
    let mut pack = open(&medium);
    pack.commit(&[obj(1)], b"m1".to_vec()).unwrap();
    drop(pack);

    // A recycled physical file resurrecting a whole old slot is
    // self-consistent under its own salt; only the header's recorded name
    // can reject it. Simulate: hot-0's bytes appear under the name hot-1.
    let stolen = slot_bytes(&medium, "hot-0");
    let (mut writer, _) = medium.open_slot("hot-1").unwrap();
    writer.append(&stolen).unwrap();
    drop(writer);

    let pack = open(&medium);
    assert_eq!(pack.sequence(), 1, "hot-0 is elected on its own merits");
    assert_eq!(
        medium.slot_names().unwrap(),
        vec!["hot-0".to_owned()],
        "the misnamed copy is reset, never elected"
    );
}
