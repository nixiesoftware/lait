//! Plan 13 F0 item 3 — one collaborative Body at growing history and state.
//!
//! §2.2's failure is that `export_body` has one collaborative mode, a full
//! snapshot, and `Material::seal` caps the envelope at 64 MiB. Once
//! a Body's snapshot crosses that cap every later edit fails. §5.3's fix splits
//! *active state size* from *retained history*, and picks checkpoint thresholds
//! from encoded sizes. This measures the four export shapes those thresholds
//! are chosen against, and separates the two axes the single snapshot number
//! conflates: how much history a Body has, and how big its current state is.

use loro::{ExportMode, LoroDoc};

const ENVELOPE_CAP: usize = 64 * 1024 * 1024;

fn configured(peer: u64) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.set_record_timestamp(true);
    doc.set_change_merge_interval(-1);
    doc.set_peer_id(peer).expect("fresh doc accepts a peer id");
    doc
}

/// Deterministic pseudo-random ASCII. A repeated character measures Loro's
/// compressor, not a document — real prose sits between the two, closer to
/// this end.
fn filler(seed: u64, len: usize) -> String {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push((b'!' + ((state >> 33) % 94) as u8) as char);
    }
    out
}

/// A Body with `edits` separate committed changes, each appending `width`
/// characters — so history grows with `edits` and current state with
/// `edits * width`.
fn body_with_history(edits: usize, width: usize) -> LoroDoc {
    let doc = configured(7);
    let text = doc.get_text("body");
    for i in 0..edits {
        text.insert(text.len_unicode(), &filler(i as u64, width))
            .expect("insert");
        doc.commit();
    }
    doc
}

#[derive(Debug, Clone, Copy)]
struct Sizes {
    snapshot: usize,
    updates: usize,
    shallow: usize,
    state_chars: usize,
}

fn measure(doc: &LoroDoc) -> Sizes {
    Sizes {
        snapshot: doc.export(ExportMode::Snapshot).expect("snapshot").len(),
        updates: doc
            .export(ExportMode::all_updates())
            .expect("updates")
            .len(),
        shallow: doc
            .export(ExportMode::shallow_snapshot(&doc.oplog_frontiers()))
            .expect("shallow")
            .len(),
        state_chars: doc.get_text("body").len_unicode(),
    }
}

#[test]
fn history_dominates_a_snapshot_that_state_alone_would_not() {
    // Same current state, different history depth. If the two columns diverge,
    // a snapshot-sized envelope is paying for history, and §5.3's split is the
    // right shape.
    let few = body_with_history(10, 1_000);
    let many = body_with_history(1_000, 10);
    let few_sizes = measure(&few);
    let many_sizes = measure(&many);

    assert_eq!(
        few_sizes.state_chars, many_sizes.state_chars,
        "the two Bodies must hold the same current state for this comparison"
    );
    assert!(
        many_sizes.snapshot > few_sizes.snapshot,
        "deeper history must cost more snapshot bytes at equal state: \
         {} vs {}",
        many_sizes.snapshot,
        few_sizes.snapshot
    );
    // And the checkpoint is what removes that cost.
    assert!(
        many_sizes.shallow < many_sizes.snapshot,
        "a checkpoint must be cheaper than the snapshot it replaces: {} vs {}",
        many_sizes.shallow,
        many_sizes.snapshot
    );
}

#[test]
fn a_delta_is_orders_of_magnitude_smaller_than_a_snapshot() {
    // §9's budget: "one ordinary collaborative edit writes update-sized Body
    // material, not snapshot-sized material." This is the ratio that claim
    // rests on.
    let doc = body_with_history(2_000, 20);
    let before = doc.oplog_vv();
    doc.get_text("body")
        .insert(0, "one more edit")
        .expect("insert");
    doc.commit();

    let delta = doc.export(ExportMode::updates(&before)).expect("delta");
    let snapshot = doc.export(ExportMode::Snapshot).expect("snapshot");
    println!(
        "ordinary edit: delta {} B, whole snapshot {} B",
        delta.len(),
        snapshot.len()
    );
    assert!(
        delta.len() * 10 < snapshot.len(),
        "an ordinary edit's delta ({}) must be far below a whole snapshot ({})",
        delta.len(),
        snapshot.len()
    );
}

#[test]
fn a_checkpoint_reclaims_about_half_a_snapshot() {
    // What §5.3's checkpoint is actually worth, measured rather than assumed.
    // On incompressible content a snapshot runs about twice its current state:
    // the state, plus the history that produced it. Trimming at the retention
    // frontier gives the state back and drops the rest, so a checkpoint is
    // roughly half a snapshot and tracks state size alone from then on.
    //
    // Repeated single characters would report a very different ratio — Loro's
    // columnar encoding compresses them away — which is why `filler` exists.
    let doc = body_with_history(10_000, 100);
    let snapshot = doc.export(ExportMode::Snapshot).expect("snapshot").len();
    let shallow = doc
        .export(ExportMode::shallow_snapshot(&doc.oplog_frontiers()))
        .expect("shallow")
        .len();
    let ratio = shallow as f64 / snapshot as f64;
    println!(
        "checkpoint is {:.0}% of the snapshot it replaces",
        ratio * 100.0
    );
    assert!(
        (0.3..0.7).contains(&ratio),
        "checkpoint/snapshot ratio {ratio:.2} is outside the measured band —          if the encoding changed, the checkpoint thresholds need rechecking"
    );
}

#[test]
fn the_delta_count_threshold_binds_before_the_byte_threshold() {
    // §5.3 checkpoints at "either 256 deltas or 8 MiB encoded". Which one
    // actually fires decides whether the byte threshold is policy or decoration.
    let doc = body_with_history(1_000, 100);
    let mut tail_bytes = 0usize;
    for i in 0..256 {
        let before = doc.oplog_vv();
        doc.get_text("body")
            .insert(0, &filler(9_000 + i, 40))
            .expect("insert");
        doc.commit();
        tail_bytes += doc
            .export(ExportMode::updates(&before))
            .expect("delta")
            .len();
    }
    println!("256 ordinary deltas encode to {tail_bytes} B");
    assert!(
        tail_bytes < 8 * 1024 * 1024,
        "256 ordinary deltas ({tail_bytes} B) should sit far under the 8 MiB          byte threshold — the count is what bounds an ordinary Body, and the          byte threshold exists for the Body that pastes megabytes at a time"
    );
}

#[test]
fn recorded_export_sizes() {
    println!(
        "\n{:>7} {:>7} {:>12} {:>11} {:>11} {:>10} {:>9}",
        "edits", "width", "state chars", "snapshot B", "updates B", "shallow B", "shallow%"
    );
    for (edits, width) in [
        (100usize, 10usize),
        (1_000, 10),
        (10_000, 10),
        (1_000, 100),
        (10_000, 100),
        (10_000, 1_000),
    ] {
        let doc = body_with_history(edits, width);
        let s = measure(&doc);
        println!(
            "{:>7} {:>7} {:>12} {:>11} {:>11} {:>10} {:>8.0}%",
            edits,
            width,
            s.state_chars,
            s.snapshot,
            s.updates,
            s.shallow,
            100.0 * s.shallow as f64 / s.snapshot as f64,
        );
    }
}

#[test]
fn the_envelope_cap_is_reachable_by_history_alone() {
    // The claim in §2.2 is that a Body becomes permanently unwritable once its
    // snapshot crosses 64 MiB. Rather than build a 64 MiB document in a test,
    // measure the per-edit snapshot growth and report how many ordinary edits
    // that implies — the number is the argument.
    let small = body_with_history(1_000, 10);
    let large = body_with_history(11_000, 10);
    println!(
        "state-driven: 10M chars of incompressible text snapshots to {} B",
        body_with_history(10_000, 1_000)
            .export(ExportMode::Snapshot)
            .expect("s")
            .len()
    );
    let growth_per_edit = (large.export(ExportMode::Snapshot).expect("s").len()
        - small.export(ExportMode::Snapshot).expect("s").len()) as f64
        / 10_000.0;
    let edits_to_cap = ENVELOPE_CAP as f64 / growth_per_edit;
    println!(
        "snapshot grows ~{growth_per_edit:.1} B per edit; \
         the 64 MiB envelope caps out around {edits_to_cap:.0} edits"
    );
    assert!(
        growth_per_edit > 0.0,
        "history must cost snapshot bytes, or §2.2 is describing nothing"
    );
    // A collaborative document a team edits for a year reaches this. That it is
    // reachable at all is the point; the exact number is the operator's input
    // to checkpoint thresholds.
    assert!(
        edits_to_cap < 100_000_000.0,
        "the cap must be reachable by ordinary editing, not merely in theory"
    );
}
