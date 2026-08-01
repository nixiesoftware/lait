//! F0 §5.2 — executable evidence for the causal-compaction question.
//!
//! Plan 13 F0 item 8 may not be closed by argument. These tests measure the
//! pinned Loro 1.13.6 directly and record which of §5.2's three outcomes it
//! reaches. They name `loro` because this is the Engine package, the one place
//! the workspace permits it.
//!
//! The question: a fresh random peer id per writable Station activation grows
//! every Body's version vector by one dead entry per restart, forever. Does an
//! archive-assisted checkpoint remove inactive activation peer ids from the
//! live causal summary while archives still admit pre-checkpoint work?

use loro::{ExportMode, Frontiers, LoroDoc};

/// Configure a document the way `fabric::op::configure` does, so the evidence
/// is measured under production settings rather than Loro defaults.
fn configured(peer: u64) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.set_record_timestamp(true);
    doc.set_change_merge_interval(-1);
    doc.set_peer_id(peer).expect("fresh doc accepts a peer id");
    doc
}

/// Write `n` ops as `peer`, then return the doc's serialized updates.
fn activation_updates(base: Option<&[u8]>, peer: u64, n: usize) -> Vec<u8> {
    let doc = configured(peer);
    if let Some(bytes) = base {
        doc.import(bytes).expect("import base");
    }
    doc.set_peer_id(peer).expect("peer id before ops");
    let text = doc.get_text("body");
    for i in 0..n {
        text.insert(text.len_unicode(), &format!("{i} "))
            .expect("insert");
    }
    doc.commit();
    doc.export(ExportMode::all_updates())
        .expect("export updates")
}

/// A doc carrying `activations` sequential activations, each a distinct peer id
/// writing a few ops — the restart pattern that grows the summary.
fn doc_after_activations(activations: u64, ops_each: usize) -> LoroDoc {
    let live = configured(1);
    let mut carried: Option<Vec<u8>> = None;
    for a in 0..activations {
        let peer = 1000 + a;
        let updates = activation_updates(carried.as_deref(), peer, ops_each);
        live.import(&updates).expect("import activation");
        carried = Some(live.export(ExportMode::all_updates()).expect("carry"));
    }
    live
}

#[test]
fn version_vector_grows_one_entry_per_activation() {
    for activations in [1u64, 8, 64] {
        let doc = doc_after_activations(activations, 3);
        let vv = doc.oplog_vv();
        // The live doc's own peer plus one entry per activation that authored.
        assert!(
            vv.len() as u64 >= activations,
            "at {activations} activations the summary held {} entries — \
             expected growth of one per activation",
            vv.len()
        );
    }
}

#[test]
fn frontiers_stay_bounded_while_the_version_vector_grows() {
    // The load-bearing measurement: frontier size is a function of concurrency,
    // version-vector size is a function of lifetime activations. Sequential
    // activations converge to a single head.
    let doc = doc_after_activations(64, 3);
    let vv_entries = doc.oplog_vv().len();
    let frontier_entries = doc.oplog_frontiers().len();
    assert!(
        vv_entries >= 64,
        "expected an unbounded-shaped summary, saw {vv_entries}"
    );
    assert_eq!(
        frontier_entries, 1,
        "sequential activations must converge to one head; saw {frontier_entries}"
    );
}

#[test]
fn shallow_snapshot_does_not_shrink_the_live_version_vector() {
    // Outcome-1 candidate: does checkpointing compact the summary? Measured,
    // not assumed. The shallow snapshot keeps `shallow_since_vv` so the doc
    // still knows the trimmed ops are included — which is exactly why the
    // summary cannot shrink.
    let doc = doc_after_activations(32, 3);
    let before = doc.oplog_vv().len();

    let shallow = doc
        .export(ExportMode::shallow_snapshot(&doc.oplog_frontiers()))
        .expect("shallow snapshot");
    let compacted = LoroDoc::new();
    compacted.import(&shallow).expect("import shallow snapshot");
    let after = compacted.oplog_vv().len();

    assert_eq!(
        before, after,
        "a shallow snapshot preserved {after} summary entries against {before} — \
         if this ever shrinks, §5.2 outcome 1 is reachable and this test is the \
         evidence that says so"
    );
    assert!(
        !compacted.shallow_since_vv().is_empty(),
        "a shallow doc must record the version its trimmed history starts from"
    );
}

#[test]
fn shallow_snapshot_refuses_work_predating_its_root() {
    // The §5.2 outcome-2 mechanism: a writer that went offline before the
    // checkpoint cannot merge into the compacted document.
    let origin = configured(1);
    let text = origin.get_text("body");
    text.insert(0, "shared base").expect("insert");
    origin.commit();
    let base = origin.export(ExportMode::all_updates()).expect("base");

    // A peer forks here and edits offline.
    let stale = configured(2);
    stale.import(&base).expect("import base");
    stale
        .get_text("body")
        .insert(0, "offline ")
        .expect("insert");
    stale.commit();
    let offline_work = stale.export(ExportMode::all_updates()).expect("offline");

    // Meanwhile the origin advances and checkpoints past the fork point.
    for i in 0..8 {
        origin
            .get_text("body")
            .insert(0, &format!("{i}"))
            .expect("insert");
    }
    origin.commit();
    let shallow = origin
        .export(ExportMode::shallow_snapshot(&origin.oplog_frontiers()))
        .expect("shallow snapshot");
    let compacted = LoroDoc::new();
    compacted.import(&shallow).expect("import shallow");

    let outcome = compacted.import(&offline_work);
    assert!(
        outcome.is_err() || outcome.as_ref().is_ok_and(|s| s.pending.is_some()),
        "pre-checkpoint work must not be silently absorbed: {outcome:?}"
    );
}

#[test]
fn a_full_archive_readmits_pre_checkpoint_work() {
    // Outcome 2's recovery path: the archive is a complete snapshot taken
    // before trimming, so reconstructing from it accepts the stale writer's
    // work. This is what makes `BeforeRetentionFrontier` recoverable rather
    // than lossy.
    let origin = configured(1);
    origin
        .get_text("body")
        .insert(0, "shared base")
        .expect("insert");
    origin.commit();
    let base = origin.export(ExportMode::all_updates()).expect("base");

    let stale = configured(2);
    stale.import(&base).expect("import base");
    stale
        .get_text("body")
        .insert(0, "offline ")
        .expect("insert");
    stale.commit();
    let offline_work = stale.export(ExportMode::all_updates()).expect("offline");

    for i in 0..8 {
        origin
            .get_text("body")
            .insert(0, &format!("{i}"))
            .expect("insert");
    }
    origin.commit();
    // The archive is taken BEFORE the trim — §5.3's required history artifact.
    let archive = origin.export(ExportMode::Snapshot).expect("archive");

    let rebuilt = LoroDoc::new();
    rebuilt.import(&archive).expect("import archive");
    let status = rebuilt.import(&offline_work).expect("import offline work");
    assert!(
        status.pending.is_none(),
        "a document rebuilt from a complete archive must admit the stale \
         writer's work outright, not hold it pending: {status:?}"
    );
}

#[test]
fn a_frontier_outside_local_history_does_not_convert() {
    // Why `Version` cannot be the wire input to a delta computation:
    // expanding a frontier needs the DAG it names. A diverged peer cannot do it.
    let a = configured(1);
    a.get_text("body").insert(0, "a").expect("insert");
    a.commit();

    let b = configured(2);
    b.get_text("body").insert(0, "b").expect("insert");
    b.commit();

    assert!(
        b.frontiers_to_vv(&a.oplog_frontiers()).is_none(),
        "a frontier naming ops the local replica has never seen must not expand"
    );
    assert!(
        a.frontiers_to_vv(&a.oplog_frontiers()).is_some(),
        "a frontier inside local history must expand"
    );
    // An empty frontier is the universally convertible base.
    assert!(b.frontiers_to_vv(&Frontiers::default()).is_some());
}

#[test]
fn artifacts_converge_without_exchanging_a_version_vector() {
    // The design this evidence supports: Bodies converge by exchanging
    // content-addressed update artifacts, imported in any order, with Loro
    // holding causally-incomplete ones pending. No summary crosses the wire.
    let a = configured(1);
    a.get_text("body").insert(0, "one ").expect("insert");
    a.commit();
    let first = a.export(ExportMode::all_updates()).expect("first");
    a.get_text("body").insert(0, "two ").expect("insert");
    a.commit();
    let second = a
        .export(ExportMode::updates(&{
            let seen = LoroDoc::new();
            seen.import(&first).expect("import first");
            seen.oplog_vv()
        }))
        .expect("second");

    // Deliberately out of order: the dependent artifact arrives first.
    let b = configured(2);
    let held = b.import(&second).expect("import out of order");
    assert!(
        held.pending.is_some(),
        "an artifact whose dependencies are missing must be held pending"
    );
    b.import(&first).expect("import the base artifact");
    assert_eq!(
        b.get_text("body").to_string(),
        a.get_text("body").to_string(),
        "out-of-order artifact delivery must still converge"
    );
}

#[test]
fn recorded_measurements() {
    // The numbers §5.2 quotes, regenerated rather than remembered.
    println!("activations | vv entries | frontier entries | snapshot B | shallow B");
    for activations in [1u64, 8, 32, 64, 128] {
        let doc = doc_after_activations(activations, 4);
        let snapshot = doc.export(ExportMode::Snapshot).expect("snapshot");
        let shallow = doc
            .export(ExportMode::shallow_snapshot(&doc.oplog_frontiers()))
            .expect("shallow");
        println!(
            "{activations:>11} | {:>10} | {:>16} | {:>10} | {:>9}",
            doc.oplog_vv().len(),
            doc.oplog_frontiers().len(),
            snapshot.len(),
            shallow.len()
        );
    }
}
