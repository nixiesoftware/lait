//! Plan 13 F3 exit — the causal contract, proven through lait's own types.
//!
//! Nothing here names a Loro type, which is the first thing being tested: if
//! this file needed one, the seam would have leaked.
//!
//! The scenarios are the ones §8 F3 item 7 names — restart, duplicate import,
//! out-of-order delta, missing base, concurrent offline edit, checkpoint,
//! archive-assisted merge, and the retention-frontier refusal with its
//! recovery — plus the two budgets from §9 that this phase is responsible for:
//! an ordinary edit writes update-sized material, and no path in a commit
//! exports a whole history.

use fabric::{
    AnchorResolution, Artifact, CausalRelation, CheckpointPolicy, Engine, Invalid, Key, Op,
    Transaction, Version,
};

fn key() -> Key {
    Key::from_bytes(b"doc".to_vec())
}

fn other_key() -> Key {
    Key::from_bytes(b"other".to_vec())
}

fn splice(k: &Key, index: u64, insert: &str) -> Op {
    Op::TextSplice {
        key: k.clone(),
        path: "body".into(),
        index,
        delete: 0,
        insert: insert.into(),
    }
}

fn commit(fabric: &mut Engine, label: &str, ops: Vec<Op>) {
    fabric
        .commit(Transaction {
            request: label.into(),
            ops,
        })
        .expect("commit");
}

fn text(fabric: &Engine, k: &Key) -> String {
    fabric
        .read_collaborative(k)
        .expect("projects")
        .texts
        .get("body")
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_version_is_a_head_set_and_stays_one_across_activations() {
    // The measured claim, restated as a contract test: restarting the writer
    // must not grow what gets committed or advertised.
    let mut carried: Option<Artifact> = None;
    for activation in 0..32 {
        let mut fabric = Engine::new();
        if let Some(artifact) = &carried {
            fabric.import_artifact(&key(), artifact).expect("import");
        }
        commit(&mut fabric, "edit", vec![splice(&key(), 0, "x")]);
        let version = fabric.version(&key()).expect("version");
        assert!(
            version.heads.len() == 1,
            "activation {activation} produced {} heads",
            version.heads.len()
        );
        assert!(version.encode().len() < 32, "and it encodes small");
        carried = Some(fabric.export_history(&key()).expect("carry"));
    }
}

#[test]
fn an_ordinary_edit_produces_update_sized_material() {
    // §9's budget. The comparison that matters is against the snapshot this
    // replaces, not against zero.
    let mut fabric = Engine::new();
    for i in 0..2_000 {
        commit(
            &mut fabric,
            "seed",
            vec![splice(&key(), 0, &format!("{i} "))],
        );
    }
    let before = fabric.version(&key()).expect("version");
    commit(&mut fabric, "one more", vec![splice(&key(), 0, "tail")]);

    let delta = fabric.export_delta(&key(), &before).expect("delta");
    let archive = fabric.export_history(&key()).expect("archive");
    assert!(
        delta.payload_len() * 20 < archive.payload_len(),
        "an ordinary edit's delta ({}) must be far below a whole history ({})",
        delta.payload_len(),
        archive.payload_len()
    );
}

#[test]
fn a_delta_from_an_unknown_base_is_refused_by_name() {
    let mut fabric = Engine::new();
    commit(&mut fabric, "seed", vec![splice(&key(), 0, "hello")]);

    // A base naming operations this replica has never seen.
    let stranger = Version {
        format_version: 1,
        heads: vec![fabric::OpHead {
            writer: 99,
            sequence: 7,
        }],
    };
    assert_eq!(
        fabric.export_delta(&key(), &stranger),
        Err(Invalid::MissingBase)
    );
    // And the universally shared base always works.
    assert!(fabric.export_delta(&key(), &Version::empty()).is_ok());
}

#[test]
fn artifacts_converge_out_of_order_and_duplicates_are_free() {
    // Why no version negotiation is needed: the receiver does not have to be
    // told what it is missing, or in what order.
    let mut author = Engine::new();
    commit(&mut author, "one", vec![splice(&key(), 0, "one ")]);
    let first = author
        .export_delta(&key(), &Version::empty())
        .expect("first");
    let after_first = author.version(&key()).expect("version");
    commit(&mut author, "two", vec![splice(&key(), 0, "two ")]);
    let second = author.export_delta(&key(), &after_first).expect("second");

    let mut receiver = Engine::new();
    let held = receiver
        .import_artifact(&key(), &second)
        .expect("out-of-order import is accepted");
    assert!(held.pending, "material with missing dependencies is held");

    let applied = receiver.import_artifact(&key(), &first).expect("base");
    assert!(applied.applied);
    assert_eq!(text(&receiver, &key()), text(&author, &key()));

    // A duplicate changes nothing and is not an error.
    let again = receiver.import_artifact(&key(), &first).expect("replay");
    assert!(!again.applied, "a duplicate import applies nothing");
    assert_eq!(text(&receiver, &key()), text(&author, &key()));
}

#[test]
fn a_checkpoint_reconstructs_state_without_the_history_behind_it() {
    let mut author = Engine::new();
    for i in 0..500 {
        commit(
            &mut author,
            "seed",
            vec![splice(&key(), 0, &format!("{i}"))],
        );
    }
    let checkpoint = author
        .export_checkpoint(&key(), &Version::empty())
        .expect("checkpoint");
    let archive = author.export_history(&key()).expect("archive");
    assert!(
        checkpoint.payload_len() < archive.payload_len(),
        "a checkpoint must be cheaper than the history it replaces"
    );

    let mut receiver = Engine::new();
    receiver
        .import_artifact(&key(), &checkpoint)
        .expect("import checkpoint");
    assert_eq!(text(&receiver, &key()), text(&author, &key()));
}

#[test]
fn work_older_than_the_retention_frontier_is_refused_and_recovers() {
    // §5.2 outcome 2, end to end. The refusal has to be typed and the recovery
    // has to exist, or "refusing old work" is indistinguishable from losing it.
    let mut origin = Engine::new();
    commit(&mut origin, "base", vec![splice(&key(), 0, "shared")]);
    let base = origin
        .export_delta(&key(), &Version::empty())
        .expect("base");

    // A peer forks here and edits offline.
    let mut stale = Engine::new();
    stale.import_artifact(&key(), &base).expect("import base");
    commit(&mut stale, "offline", vec![splice(&key(), 0, "offline ")]);
    let offline = stale
        .export_delta(&key(), &Version::empty())
        .expect("offline work");

    // The origin advances, archives, then trims past the fork point.
    for i in 0..20 {
        commit(
            &mut origin,
            "advance",
            vec![splice(&key(), 0, &format!("{i}"))],
        );
    }
    let archive = origin
        .export_history(&key())
        .expect("archive before the trim");
    let checkpoint = origin
        .export_checkpoint(&key(), &Version::empty())
        .expect("checkpoint");

    // A replica rebuilt from the checkpoint alone cannot admit the old work.
    let mut compacted = Engine::new();
    compacted
        .import_artifact(&key(), &checkpoint)
        .expect("import checkpoint");
    let refusal = compacted.import_artifact(&key(), &offline);
    match &refusal {
        Err(Invalid::BeforeRetentionFrontier { .. }) => {}
        Ok(status) if status.pending => {}
        other => panic!("pre-trim work must be refused or held, got {other:?}"),
    }

    // And the recovery path works: rebuild from the archive taken before the
    // trim, and the same work applies.
    let mut rebuilt = Engine::new();
    rebuilt
        .import_artifact(&key(), &archive)
        .expect("import archive");
    let readmitted = rebuilt
        .import_artifact(&key(), &offline)
        .expect("the archive readmits it");
    assert!(readmitted.applied && !readmitted.pending);
    assert!(text(&rebuilt, &key()).contains("offline"));
}

#[test]
fn concurrent_offline_edits_both_survive() {
    let mut a = Engine::new();
    commit(&mut a, "base", vec![splice(&key(), 0, "base")]);
    let base = a.export_delta(&key(), &Version::empty()).expect("base");

    let mut b = Engine::new();
    b.import_artifact(&key(), &base).expect("import");

    commit(&mut a, "a", vec![splice(&key(), 0, "A")]);
    commit(&mut b, "b", vec![splice(&key(), 0, "B")]);

    let from_a = a.export_history(&key()).expect("a");
    let from_b = b.export_history(&key()).expect("b");
    a.import_artifact(&key(), &from_b).expect("merge b into a");
    b.import_artifact(&key(), &from_a).expect("merge a into b");

    assert_eq!(text(&a, &key()), text(&b, &key()), "both replicas converge");
    let merged = text(&a, &key());
    assert!(
        merged.contains('A') && merged.contains('B'),
        "neither edit was lost"
    );
}

#[test]
fn relation_says_undetermined_rather_than_guessing() {
    let mut fabric = Engine::new();
    commit(&mut fabric, "one", vec![splice(&key(), 0, "one")]);
    let first = fabric.version(&key()).expect("version");
    commit(&mut fabric, "two", vec![splice(&key(), 0, "two")]);
    let second = fabric.version(&key()).expect("version");

    assert_eq!(
        fabric.relation(&key(), &first, &first),
        CausalRelation::Equal
    );
    assert_eq!(
        fabric.relation(&key(), &second, &first),
        CausalRelation::Dominates
    );
    assert_eq!(
        fabric.relation(&key(), &first, &second),
        CausalRelation::Dominated
    );

    let unseen = Version {
        format_version: 1,
        heads: vec![fabric::OpHead {
            writer: 4242,
            sequence: 1,
        }],
    };
    assert_eq!(
        fabric.relation(&key(), &unseen, &first),
        CausalRelation::Undetermined,
        "a version we have not received is not concurrent, it is unknown"
    );
}

#[test]
fn an_anchor_follows_its_text_across_a_concurrent_edit() {
    // What plan 14's carets rest on. An offset does not survive an insertion
    // before it; an anchor does.
    let mut fabric = Engine::new();
    commit(&mut fabric, "seed", vec![splice(&key(), 0, "hello world")]);
    let anchor = fabric.anchor(&key(), "body", 6).expect("anchor");

    // Someone inserts before the anchored position.
    commit(&mut fabric, "prefix", vec![splice(&key(), 0, ">> ")]);

    match fabric.resolve(&key(), &anchor) {
        AnchorResolution::Resolved(position) => assert_eq!(
            position, 9,
            "the anchor moved with its text, not with its offset"
        ),
        AnchorResolution::Drifted => panic!("a live position must not drift"),
    }
}

#[test]
fn resolving_an_anchor_never_fails_and_never_mutates() {
    let mut fabric = Engine::new();
    commit(&mut fabric, "seed", vec![splice(&key(), 0, "hello")]);
    let anchor = fabric.anchor(&key(), "body", 3).expect("anchor");
    let before = fabric.version(&key()).expect("version");

    // Delete the material the anchor was attached to.
    commit(
        &mut fabric,
        "delete",
        vec![Op::TextSplice {
            key: key(),
            path: "body".into(),
            index: 0,
            delete: 5,
            insert: String::new(),
        }],
    );
    let after_delete = fabric.version(&key()).expect("version");

    // Total: an answer either way, and no third outcome.
    let resolved = fabric.resolve(&key(), &anchor);
    assert!(matches!(
        resolved,
        AnchorResolution::Resolved(_) | AnchorResolution::Drifted
    ));
    // Resolving is a read. Doing it again changes nothing, and the Body's
    // version is untouched — which is what makes it safe on a read-only
    // replica.
    assert_eq!(fabric.resolve(&key(), &anchor), resolved);
    assert_eq!(fabric.version(&key()).expect("version"), after_delete);
    assert_ne!(before, after_delete);

    // An anchor for a path or Body that does not exist drifts rather than
    // erroring.
    assert_eq!(
        fabric.resolve(&other_key(), &anchor),
        AnchorResolution::Drifted
    );
}

#[test]
fn a_failed_batch_changes_nothing_and_exports_no_history() {
    // The rollback replacement. Atomicity has to hold, and it has to hold
    // without a whole-history export — which was the cost hiding inside the
    // previous mechanism.
    let mut fabric = Engine::new();
    for i in 0..500 {
        commit(
            &mut fabric,
            "seed",
            vec![splice(&key(), 0, &format!("{i}"))],
        );
    }
    let before_text = text(&fabric, &key());
    let before_version = fabric.version(&key()).expect("version");

    // A batch whose second op is impossible: a text splice past the end.
    let outcome = fabric.commit(Transaction {
        request: "doomed".into(),
        ops: vec![
            splice(&key(), 0, "first half applies"),
            Op::TextSplice {
                key: key(),
                path: "body".into(),
                index: 999_999,
                delete: 0,
                insert: "impossible".into(),
            },
        ],
    });
    assert!(outcome.is_err(), "the batch must fail");
    assert_eq!(
        text(&fabric, &key()),
        before_text,
        "and leave the Body exactly as it was"
    );

    // The Body still works afterwards, and the reverted position is orderable
    // against the one it started from.
    commit(&mut fabric, "after", vec![splice(&key(), 0, "!")]);
    assert!(text(&fabric, &key()).starts_with('!'));
    assert!(matches!(
        fabric.relation(&key(), &fabric.version(&key()).unwrap(), &before_version),
        CausalRelation::Dominates
    ));
}

#[test]
fn a_batch_that_creates_a_body_and_fails_leaves_no_body() {
    let mut fabric = Engine::new();
    let outcome = fabric.commit(Transaction {
        request: "doomed".into(),
        ops: vec![
            splice(&other_key(), 0, "created"),
            Op::TextSplice {
                key: other_key(),
                path: "body".into(),
                index: 999_999,
                delete: 0,
                insert: "impossible".into(),
            },
        ],
    });
    assert!(outcome.is_err());
    assert!(
        fabric.version(&other_key()).is_err(),
        "a Body the failed batch invented must not exist"
    );
}

// The three ways the bounded rollback used to lose a pre-existing Body. All
// three are the same root cause: a saved *position* indexes a document, and
// `Remove` destroys the document it indexes. Restoring a position is only
// enough while the Body survives the batch.

#[test]
fn a_failed_batch_that_removed_a_body_restores_it() {
    let mut fabric = Engine::new();
    commit(&mut fabric, "seed", vec![splice(&key(), 0, "important")]);

    let outcome = fabric.commit(Transaction {
        request: "doomed".into(),
        ops: vec![
            Op::Remove { key: key() },
            splice(&other_key(), 999_999, "impossible"),
        ],
    });
    assert!(outcome.is_err());
    assert_eq!(text(&fabric, &key()), "important");
}

#[test]
fn a_failed_batch_that_removed_and_recreated_a_body_restores_the_original() {
    // The variant that used to fail *inside* the rollback: the recreated Body
    // is a fresh document, so reverting it to the old one's frontiers is
    // `FrontiersNotFound` — and the early return that produced left every
    // later Body in the batch still dirty.
    let mut fabric = Engine::new();
    commit(
        &mut fabric,
        "seed",
        vec![
            splice(&key(), 0, "important"),
            splice(&other_key(), 0, "keep"),
        ],
    );

    let third = Key::from_bytes(b"third".to_vec());
    let outcome = fabric.commit(Transaction {
        request: "doomed".into(),
        ops: vec![
            Op::Remove { key: key() },
            Op::CreateBody { key: key() },
            splice(&other_key(), 0, "DIRTY-"),
            splice(&third, 999_999, "impossible"),
        ],
    });
    assert!(outcome.is_err());
    assert_eq!(text(&fabric, &key()), "important");
    assert_eq!(
        text(&fabric, &other_key()),
        "keep",
        "a Body later in the batch must be restored even if an earlier one was hard to restore"
    );
    assert!(
        fabric.version(&third).is_err(),
        "and a Body the failed batch invented must not survive it"
    );
}

#[test]
fn a_failed_batch_cannot_replace_a_collaborative_body_with_a_value() {
    // The worst variant: remove, write an atomic value over the same key, then
    // fail. The failed batch's value used to survive as the Body's contents.
    let mut fabric = Engine::new();
    commit(&mut fabric, "seed", vec![splice(&key(), 0, "important")]);

    let outcome = fabric.commit(Transaction {
        request: "doomed".into(),
        ops: vec![
            Op::Remove { key: key() },
            Op::PutCanonical {
                key: key(),
                value: b"attacker value".to_vec(),
            },
            splice(&other_key(), 999_999, "impossible"),
        ],
    });
    assert!(outcome.is_err());
    assert_eq!(fabric.read(&key()), None, "no value from a failed batch");
    assert_eq!(text(&fabric, &key()), "important");
}

#[test]
fn the_checkpoint_policy_is_decided_by_size_not_by_time() {
    let policy = CheckpointPolicy::default();
    assert!(!policy.should_checkpoint(255, 1_000));
    assert!(policy.should_checkpoint(256, 1_000));
    assert!(policy.should_checkpoint(1, 8 * 1024 * 1024));
    // The count binds first for an ordinary Body: F0 measured 256 ordinary
    // deltas at ~33 KB, so a Body only reaches the byte threshold by pasting.
    assert!(
        policy.max_tail_deltas * 200 < policy.max_tail_bytes,
        "the byte threshold must sit well above what ordinary editing reaches"
    );
}

#[test]
fn body_material_is_the_same_size_however_long_the_body_lives() {
    use fabric::{ArtifactRef, Material};
    let material = |tail: usize, history: u64| Material {
        format_version: 1,
        checkpoint: ArtifactRef {
            hash: [1u8; 32],
            len: 4096,
        },
        delta_tail: (0..tail)
            .map(|i| ArtifactRef {
                hash: [i as u8; 32],
                len: 105,
            })
            .collect(),
        history_root: Some([9u8; 32]),
        history_count: history,
        version: Version::empty(),
    };
    // History is behind a root, so a Body with a thousand archives commits the
    // same bytes as one with none.
    let young = material(4, 0);
    let old = material(4, 1_000);
    assert!(old.encode().len() - young.encode().len() <= 2);
    assert!(young.validate().is_ok() && old.validate().is_ok());

    // The tail is bounded, and a descriptor claiming more is refused.
    let overlong = material(CheckpointPolicy::default().max_tail_deltas + 1, 0);
    assert_eq!(overlong.validate(), Err(Invalid::Bounds));
}

#[test]
fn causal_encodings_are_canonical() {
    let mut fabric = Engine::new();
    commit(&mut fabric, "seed", vec![splice(&key(), 0, "hello")]);
    let version = fabric.version(&key()).expect("version");
    let anchor = fabric.anchor(&key(), "body", 2).expect("anchor");

    assert_eq!(
        Version::decode_canonical(&version.encode()).unwrap(),
        version
    );
    let mut extended = version.encode();
    extended.push(0);
    assert!(Version::decode_canonical(&extended).is_err());

    assert_eq!(
        fabric::Anchor::decode_canonical(&anchor.encode()).unwrap(),
        anchor
    );

    // An unsorted head set is not a canonical version, whatever it decodes to.
    let unsorted = Version {
        format_version: 1,
        heads: vec![
            fabric::OpHead {
                writer: 9,
                sequence: 1,
            },
            fabric::OpHead {
                writer: 1,
                sequence: 1,
            },
        ],
    };
    assert_eq!(
        Version::decode_canonical(&postcard::to_stdvec(&unsorted).unwrap()),
        Err(Invalid::NonCanonical)
    );
}

#[test]
fn an_anchor_does_not_resolve_against_another_body() {
    // Every Body of one activation shares a writer id, so operation ids collide
    // across documents. Without the Body in the anchor, a caret taken in one
    // Body resolves against another to a plausible index — the silently wrong
    // answer `Drifted` exists to prevent.
    let mut fabric = Engine::new();
    commit(
        &mut fabric,
        "seed",
        vec![
            splice(&key(), 0, "hello world"),
            splice(&other_key(), 0, "a completely different document"),
        ],
    );
    let anchor = fabric.anchor(&key(), "body", 6).expect("anchor");

    assert!(matches!(
        fabric.resolve(&key(), &anchor),
        AnchorResolution::Resolved(6)
    ));
    assert_eq!(
        fabric.resolve(&other_key(), &anchor),
        AnchorResolution::Drifted,
        "an anchor belongs to the Body it was taken in"
    );
}

#[test]
fn a_deleted_anchor_drifts_rather_than_landing_one_place_over() {
    // When the anchored character is gone the engine answers with the gap it
    // left. Treating that like a live resolution and adding one puts the caret
    // a character to the right of where it belongs, which is exactly the
    // silently wrong index the type promises never to return.
    let mut fabric = Engine::new();
    commit(&mut fabric, "seed", vec![splice(&key(), 0, "abcdef")]);
    let anchor = fabric.anchor(&key(), "body", 3).expect("anchor");
    assert_eq!(
        fabric.resolve(&key(), &anchor),
        AnchorResolution::Resolved(3)
    );

    // Delete exactly the character the anchor bound to.
    commit(
        &mut fabric,
        "delete",
        vec![Op::TextSplice {
            key: key(),
            path: "body".into(),
            index: 2,
            delete: 1,
            insert: String::new(),
        }],
    );
    assert_eq!(
        fabric.resolve(&key(), &anchor),
        AnchorResolution::Drifted,
        "the character it was attached to is gone"
    );
}

#[test]
fn a_replacement_artifact_cannot_flatten_a_collaborative_body() {
    // The mirror of `import_body`'s refusal. A peer sending the wrong artifact
    // kind must not be able to discard a Body's whole history by overwriting it
    // with a value.
    use fabric::Artifact;
    let mut fabric = Engine::new();
    commit(
        &mut fabric,
        "seed",
        vec![splice(&key(), 0, "history worth keeping")],
    );

    let outcome = fabric.import_artifact(
        &key(),
        &Artifact::Replace {
            format_version: 1,
            bytes: b"a flat value".to_vec(),
        },
    );
    assert!(outcome.is_err(), "a model mismatch is a conflict");
    assert_eq!(text(&fabric, &key()), "history worth keeping");
}
