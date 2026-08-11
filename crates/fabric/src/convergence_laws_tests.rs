//! The collaborative algebra's CRDT laws, verified against generated programs
//! rather than hand-picked scenarios.
//!
//! `tests/collaborative_convergence.rs` pins the *semantics* — add-wins sets,
//! summed counters, an agreed LWW winner — with one fixed fork and one fixed op
//! list per case. Those fixtures say what the algebra means, and they are worth
//! keeping. What they cannot say is that the meaning holds for op sequences
//! nobody thought to write down, and that is precisely where CRDT bugs live:
//! not in the case an author imagined while writing the merge, but in the
//! combination they did not.
//!
//! So this module asserts the three laws every convergent replicated type owes,
//! over randomly generated programs:
//!
//! - **Convergence** — replicas that have seen the same operations, in any
//!   arrival order, project the same view.
//! - **Commutativity + associativity** — merging the same material in a
//!   different permutation reaches the same state, so no delivery order is
//!   privileged. (These are one property here rather than two: over a set of
//!   exports, order-independence *is* both.)
//! - **Idempotence** — re-delivering known material changes neither the view
//!   nor the `changed` bookkeeping, so a duplicate frame cannot manufacture a
//!   Receipt and, downstream, a spurious commit and gossip round.
//!
//! ## Generating programs, not ops
//!
//! Three ops cannot be generated in isolation: `ListRemove` and `ListMove` take
//! a **stable element id** the Engine mints at insert, and `TextSplice` takes
//! Unicode-scalar coordinates that must fall inside the current text. A
//! generator that invented those would produce mostly-rejected garbage.
//!
//! What is generated instead is an *abstract* program ([`OpSpec`]) that names
//! its target positionally — "remove the 3rd list element" — and is lowered
//! against the replica's own current view at apply time. A spec with no legal
//! lowering (removing from an empty list) is skipped rather than failed: it is
//! the generator that was unlucky, not the engine that was wrong.
//!
//! ## Small domains on purpose
//!
//! Paths, values, and entry names are drawn from deliberately tiny alphabets.
//! A large value space would make concurrent replicas almost never touch the
//! same path, and a convergence test where nothing collides proves nothing.
//! Collisions are the interesting states, so the generator is biased to produce
//! them constantly.
//!
//! ## Running more of it
//!
//! Case count is proptest's `PROPTEST_CASES` (default 256, dropped to a cheaper
//! number below so this stays on the every-push tier). The nightly tier raises
//! it. A failure writes its seed to `proptest-regressions/` next to this file;
//! that file is committed, so the shrunk counterexample becomes a permanent
//! regression case replayed on every later run.

use crate::{CollaborativeView, Engine, Key, Op, Transaction};
use proptest::prelude::*;

/// Every replica edits one Body. Per-Body is where the algebra's semantics
/// live — cross-Body atomicity is `batch_atomicity`'s claim, not this one.
fn key() -> Key {
    Key::from_bytes(b"body/laws".to_vec())
}

/// Path alphabets, one per collaborative type. A path is bound to its container
/// type by first use, so the generator must stay type-correct: `reg0` is always
/// a register, never a counter.
const REGISTERS: [&str; 2] = ["reg0", "reg1"];
const MAPS: [&str; 2] = ["map0", "map1"];
const LISTS: [&str; 2] = ["list0", "list1"];
const TEXTS: [&str; 1] = ["text0"];
const SETS: [&str; 2] = ["set0", "set1"];
const COUNTERS: [&str; 2] = ["ctr0", "ctr1"];
const TREES: [&str; 1] = ["tree0"];
const LOGS: [&str; 1] = ["log0"];
/// Small on purpose: a retention nothing reaches never trims, and a law that
/// only holds while the type is idle is not the law being claimed.
const LOG_RETAIN: u64 = 4;

/// A tiny value alphabet, so two replicas writing "a different value" collide
/// on the same one often.
fn val(n: u8) -> Vec<u8> {
    vec![b'v', b'a' + (n % 6)]
}

fn entry(n: u8) -> String {
    format!("e{}", n % 4)
}

/// One generated operation, naming its target positionally so it can be lowered
/// against whatever state the replica actually reached.
#[derive(Debug, Clone)]
enum OpSpec {
    RegisterSet {
        path: usize,
        value: u8,
    },
    RegisterClear {
        path: usize,
    },
    MapSet {
        path: usize,
        entry: u8,
        value: u8,
    },
    MapRemove {
        path: usize,
        entry: u8,
    },
    ListInsert {
        path: usize,
        at: u8,
        value: u8,
    },
    /// Remove the `nth` element *as this replica currently sees the list*.
    ListRemoveNth {
        path: usize,
        nth: u8,
    },
    ListMoveNth {
        path: usize,
        nth: u8,
        to: u8,
    },
    TextSplice {
        at: u8,
        delete: u8,
        insert: u8,
    },
    SetAdd {
        path: usize,
        value: u8,
    },
    SetRemove {
        path: usize,
        value: u8,
    },
    CounterAdd {
        path: usize,
        delta: i8,
    },
    /// Insert under the `nth` node this replica currently sees — or at the
    /// root of the forest when `nth` names nothing — optionally after that
    /// parent's `sibling`th child.
    TreeInsertUnder {
        nth: Option<u8>,
        sibling: Option<u8>,
        value: u8,
    },
    /// Re-parent the `nth` node under the `to`th, which is the interesting op:
    /// concurrent re-parenting is exactly what a hand-encoded parent field has
    /// no answer for.
    TreeMoveNth {
        nth: u8,
        to: Option<u8>,
    },
    TreeRemoveNth {
        nth: u8,
    },
    TreeSetNth {
        nth: u8,
        entry: u8,
        value: u8,
    },
    /// Appends trim, so replicas that ran different numbers of them have
    /// deleted different entries — which is exactly the state the convergence
    /// law has to hold across.
    LogAppend {
        value: u8,
    },
}

/// Index into a fixed-size path alphabet without risking an out-of-range panic
/// on a generator change.
fn pick<'a>(table: &[&'a str], index: usize) -> &'a str {
    table[index % table.len()]
}

/// Lower an abstract spec against `view`, the replica's current projection.
///
/// `None` means this spec has no legal lowering in this state (an empty list
/// has no 3rd element). Skipping is deliberate — see the module docs.
fn lower(spec: &OpSpec, view: &CollaborativeView) -> Option<Op> {
    let key = key();
    Some(match *spec {
        OpSpec::RegisterSet { path, value } => Op::RegisterSet {
            key,
            path: pick(&REGISTERS, path).to_string(),
            value: val(value),
        },
        OpSpec::RegisterClear { path } => Op::RegisterClear {
            key,
            path: pick(&REGISTERS, path).to_string(),
        },
        OpSpec::MapSet {
            path,
            entry: e,
            value,
        } => Op::MapSet {
            key,
            path: pick(&MAPS, path).to_string(),
            entry: entry(e),
            value: val(value),
        },
        OpSpec::MapRemove { path, entry: e } => Op::MapRemove {
            key,
            path: pick(&MAPS, path).to_string(),
            entry: entry(e),
        },
        OpSpec::ListInsert { path, at, value } => {
            let path = pick(&LISTS, path).to_string();
            // An insert index must land inside the list or exactly at its end.
            let len = view.lists.get(&path).map_or(0, Vec::len);
            Op::ListInsert {
                key,
                path,
                index: (at as usize % (len + 1)) as u64,
                value: val(value),
            }
        }
        OpSpec::ListRemoveNth { path, nth } => {
            let path = pick(&LISTS, path).to_string();
            let elements = view.lists.get(&path)?;
            let target = elements.get(nth as usize % elements.len().max(1))?;
            Op::ListRemove {
                key,
                path,
                element: target.element.clone(),
            }
        }
        OpSpec::ListMoveNth { path, nth, to } => {
            let path = pick(&LISTS, path).to_string();
            let elements = view.lists.get(&path)?;
            let target = elements.get(nth as usize % elements.len().max(1))?;
            let element = target.element.clone();
            Op::ListMove {
                key,
                path,
                element,
                index: (to as usize % elements.len()) as u64,
            }
        }
        OpSpec::TextSplice { at, delete, insert } => {
            let path = pick(&TEXTS, 0).to_string();
            // Unicode-scalar coordinates: count chars, not bytes. The alphabet
            // is ASCII, but counting chars is what the op's contract says and
            // is what keeps this honest if the alphabet ever widens.
            let len = view.texts.get(&path).map_or(0, |t| t.chars().count());
            let index = at as usize % (len + 1);
            Op::TextSplice {
                key,
                path,
                index: index as u64,
                delete: (delete as usize % (len - index + 1)) as u64,
                insert: format!("{}", (b'a' + (insert % 6)) as char),
            }
        }
        OpSpec::SetAdd { path, value } => Op::SetAdd {
            key,
            path: pick(&SETS, path).to_string(),
            value: val(value),
        },
        OpSpec::SetRemove { path, value } => Op::SetRemove {
            key,
            path: pick(&SETS, path).to_string(),
            value: val(value),
        },
        OpSpec::CounterAdd { path, delta } => Op::CounterAdd {
            key,
            path: pick(&COUNTERS, path).to_string(),
            delta: i64::from(delta),
        },
        OpSpec::TreeInsertUnder {
            nth,
            sibling,
            value,
        } => {
            let path = pick(&TREES, 0).to_string();
            let nodes = view.trees.get(&path).map_or(&[][..], Vec::as_slice);
            let parent = nth.and_then(|n| nth_node(nodes, n));
            let after = sibling.and_then(|s| nth_child(nodes, parent.as_deref(), s));
            Op::TreeInsert {
                key,
                path,
                parent,
                after,
                value: val(value),
            }
        }
        OpSpec::TreeMoveNth { nth, to } => {
            let path = pick(&TREES, 0).to_string();
            let nodes = view.trees.get(&path).map_or(&[][..], Vec::as_slice);
            let node = nth_node(nodes, nth)?;
            let parent = to.and_then(|t| nth_node(nodes, t));
            // A move under one's own descendant is refused by the engine, and
            // `run` treats a refused op as a finding. The generator is what
            // must avoid asking — the refusal itself is pinned by a unit test.
            if parent
                .as_deref()
                .is_some_and(|p| p == node || is_descendant_of(nodes, p, &node))
            {
                return None;
            }
            Op::TreeMove {
                key,
                path,
                node,
                parent,
                after: None,
            }
        }
        OpSpec::TreeRemoveNth { nth } => {
            let path = pick(&TREES, 0).to_string();
            let nodes = view.trees.get(&path).map_or(&[][..], Vec::as_slice);
            Op::TreeRemove {
                key,
                path,
                node: nth_node(nodes, nth)?,
            }
        }
        OpSpec::LogAppend { value } => Op::LogAppend {
            key,
            path: pick(&LOGS, 0).to_string(),
            value: val(value),
            retain: LOG_RETAIN,
        },
        OpSpec::TreeSetNth {
            nth,
            entry: e,
            value,
        } => {
            let path = pick(&TREES, 0).to_string();
            let nodes = view.trees.get(&path).map_or(&[][..], Vec::as_slice);
            Op::TreeSet {
                key,
                path,
                node: nth_node(nodes, nth)?,
                entry: entry(e),
                value: val(value),
            }
        }
    })
}

/// The `nth` node of a projected hierarchy, or `None` when there is none — an
/// empty forest has no third node, which is the generator being unlucky.
fn nth_node(nodes: &[crate::TreeNode], nth: u8) -> Option<String> {
    if nodes.is_empty() {
        return None;
    }
    nodes
        .get(nth as usize % nodes.len())
        .map(|n| n.node.clone())
}

/// The `nth` child of a parent (`None` = a root of the forest), for `after`
/// placement. Placement names a sibling, so the candidates are exactly the
/// children of the parent the same op named.
fn nth_child(nodes: &[crate::TreeNode], parent: Option<&str>, nth: u8) -> Option<String> {
    let children: Vec<&crate::TreeNode> = nodes
        .iter()
        .filter(|n| n.parent.as_deref() == parent)
        .collect();
    if children.is_empty() {
        return None;
    }
    children
        .get(nth as usize % children.len())
        .map(|n| n.node.clone())
}

/// Whether `node` sits somewhere under `ancestor`, by walking parents up. The
/// projection is pre-order with parents named, so this is a lookup chain rather
/// than a search.
fn is_descendant_of(nodes: &[crate::TreeNode], node: &str, ancestor: &str) -> bool {
    let mut current = Some(node);
    // Bounded by the node count: a converged hierarchy has no cycles, and a
    // bound is cheaper than trusting that here.
    for _ in 0..nodes.len() {
        let Some(here) = current else { return false };
        if here == ancestor {
            return true;
        }
        current = nodes
            .iter()
            .find(|n| n.node == here)
            .and_then(|n| n.parent.as_deref());
    }
    false
}

fn op_spec() -> impl Strategy<Value = OpSpec> {
    prop_oneof![
        1 => (0usize..2, 0u8..6).prop_map(|(path, value)| OpSpec::RegisterSet { path, value }),
        1 => (0usize..2).prop_map(|path| OpSpec::RegisterClear { path }),
        1 => (0usize..2, 0u8..4, 0u8..6).prop_map(|(path, entry, value)| OpSpec::MapSet {
            path,
            entry,
            value
        }),
        1 => (0usize..2, 0u8..4).prop_map(|(path, entry)| OpSpec::MapRemove { path, entry }),
        1 => (0usize..2, 0u8..8, 0u8..6).prop_map(|(path, at, value)| OpSpec::ListInsert {
            path,
            at,
            value
        }),
        1 => (0usize..2, 0u8..8).prop_map(|(path, nth)| OpSpec::ListRemoveNth { path, nth }),
        1 => (0usize..2, 0u8..8, 0u8..8).prop_map(|(path, nth, to)| OpSpec::ListMoveNth {
            path,
            nth,
            to
        }),
        1 => (0u8..12, 0u8..4, 0u8..6).prop_map(|(at, delete, insert)| OpSpec::TextSplice {
            at,
            delete,
            insert
        }),
        1 => (0usize..2, 0u8..6).prop_map(|(path, value)| OpSpec::SetAdd { path, value }),
        1 => (0usize..2, 0u8..6).prop_map(|(path, value)| OpSpec::SetRemove { path, value }),
        1 => (0usize..2, -4i8..5).prop_map(|(path, delta)| OpSpec::CounterAdd { path, delta }),
        // Weighted up: a hierarchy only gets interesting once it has depth, and
        // depth needs inserts to outnumber the ops that flatten it.
        3 => (prop::option::of(0u8..8), prop::option::of(0u8..4), 0u8..6).prop_map(
            |(nth, sibling, value)| OpSpec::TreeInsertUnder {
                nth,
                sibling,
                value
            }
        ),
        2 => (0u8..8, prop::option::of(0u8..8))
            .prop_map(|(nth, to)| OpSpec::TreeMoveNth { nth, to }),
        1 => (0u8..8).prop_map(|nth| OpSpec::TreeRemoveNth { nth }),
        1 => (0u8..8, 0u8..4, 0u8..6).prop_map(|(nth, entry, value)| OpSpec::TreeSetNth {
            nth,
            entry,
            value
        }),
        2 => (0u8..6).prop_map(|value| OpSpec::LogAppend { value }),
    ]
}

/// A program: one op sequence per replica, applied concurrently after the fork.
/// 2–4 replicas, because three is the smallest number that can distinguish
/// order-dependence from a simple two-way merge bug, and four is enough to make
/// the permutation space interesting without making the test slow.
fn program() -> impl Strategy<Value = Vec<Vec<OpSpec>>> {
    prop::collection::vec(prop::collection::vec(op_spec(), 0..8), 2..=4)
}

/// The common ancestor: a Body whose every path is created up front.
///
/// This mirrors the documented discipline `collaborative_convergence.rs` states
/// — paths are created in the Body's creating transaction, before concurrent
/// editing — rather than testing around it. Concurrent *type binding* of a
/// fresh path is a different question from convergence of a bound one, and
/// conflating them would make a failure here ambiguous.
fn ancestor() -> Engine {
    let mut ops = vec![Op::CreateBody { key: key() }];
    for path in REGISTERS {
        ops.push(Op::RegisterSet {
            key: key(),
            path: path.into(),
            value: val(0),
        });
    }
    for path in MAPS {
        ops.push(Op::MapSet {
            key: key(),
            path: path.into(),
            entry: entry(0),
            value: val(0),
        });
    }
    for path in LISTS {
        ops.push(Op::ListInsert {
            key: key(),
            path: path.into(),
            index: 0,
            value: val(0),
        });
    }
    for path in TEXTS {
        ops.push(Op::TextSplice {
            key: key(),
            path: path.into(),
            index: 0,
            delete: 0,
            insert: "seed".into(),
        });
    }
    for path in SETS {
        ops.push(Op::SetAdd {
            key: key(),
            path: path.into(),
            value: val(0),
        });
    }
    for path in COUNTERS {
        ops.push(Op::CounterAdd {
            key: key(),
            path: path.into(),
            delta: 1,
        });
    }
    for path in LOGS {
        ops.push(Op::LogAppend {
            key: key(),
            path: path.into(),
            value: val(0),
            retain: LOG_RETAIN,
        });
    }
    for path in TREES {
        ops.push(Op::TreeInsert {
            key: key(),
            path: path.into(),
            parent: None,
            after: None,
            value: val(0),
        });
    }
    let mut engine = Engine::new();
    engine
        .commit(Transaction::new("ancestor", ops))
        .expect("ancestor commits");
    engine
}

/// Fork `count` independent replicas from one ancestor.
///
/// Each is a *separate* Engine with its own activation peer id — a clone would
/// share Loro's underlying document and the replicas would not be concurrent at
/// all, which is the failure mode that makes a convergence test vacuous.
fn fork(count: usize) -> Vec<Engine> {
    let origin = ancestor();
    let export = origin.export_body(&key()).expect("ancestor exports");
    (0..count)
        .map(|_| {
            let mut replica = Engine::new();
            replica
                .import_body(&key(), &export)
                .expect("replica adopts the ancestor");
            replica
        })
        .collect()
}

/// Apply one replica's program, lowering each spec against the state the
/// previous ops actually produced. Returns how many ops were applied.
fn run(replica: &mut Engine, specs: &[OpSpec]) -> usize {
    let mut applied = 0;
    for spec in specs {
        let view = replica
            .read_collaborative(&key())
            .expect("a collaborative Body projects");
        let Some(op) = lower(spec, &view) else {
            continue;
        };
        // A lowered op is a legal op. If the Engine refuses one, that is the
        // finding — surface it rather than swallowing it into a skip.
        replica
            .commit(Transaction::new("generated", vec![op.clone()]))
            .unwrap_or_else(|failure| panic!("engine refused a legal op {op:?}: {failure:?}"));
        applied += 1;
    }
    applied
}

fn view_of(replica: &Engine) -> CollaborativeView {
    replica
        .read_collaborative(&key())
        .expect("a collaborative Body projects")
}

/// 64 cases rather than proptest's default 256: this tier runs on every push,
/// and each case builds and merges up to four real Loro documents. Coverage
/// comes from accumulating distinct seeds across runs, not from one exhaustive
/// run — the nightly tier raises `PROPTEST_CASES`.
///
/// Written this way rather than as `ProptestConfig::with_cases(64)` because
/// that helper takes the default config and then OVERWRITES `cases`, which
/// would silently discard the environment variable and leave the nightly depth
/// knob doing nothing. `ProptestConfig::default()` already reads
/// `PROPTEST_CASES`; the only thing to change is the value when nobody asked.
fn config() -> ProptestConfig {
    let from_env = std::env::var("PROPTEST_CASES").is_ok();
    let default = ProptestConfig::default();
    ProptestConfig {
        cases: if from_env { default.cases } else { 64 },
        ..default
    }
}

proptest! {
    #![proptest_config(config())]

    /// **Convergence.** Replicas that fork, edit concurrently, and then
    /// exchange everything must project identical views.
    #[test]
    fn replicas_converge_after_exchanging_all_material(programs in program()) {
        let mut replicas = fork(programs.len());
        for (replica, specs) in replicas.iter_mut().zip(&programs) {
            run(replica, specs);
        }

        // Snapshot every export BEFORE importing any, so each replica sends
        // only what it authored. Exporting lazily would let earlier imports
        // leak into later exports and the replicas would converge trivially.
        let exports: Vec<_> = replicas
            .iter()
            .map(|r| r.export_body(&key()).expect("replica exports"))
            .collect();
        for replica in &mut replicas {
            for export in &exports {
                replica.import_body(&key(), export).expect("merge succeeds");
            }
        }

        let first = view_of(&replicas[0]);
        for (index, replica) in replicas.iter().enumerate().skip(1) {
            prop_assert_eq!(
                &first,
                &view_of(replica),
                "replica 0 and replica {} diverged",
                index
            );
        }
    }

    /// **Commutativity and associativity.** The same material delivered in a
    /// different order reaches the same state, so no arrival order is
    /// privileged. Reversal is the cheapest permutation that exercises this and
    /// the one that shrinks to a readable counterexample.
    #[test]
    fn merge_order_does_not_change_the_result(programs in program()) {
        let mut replicas = fork(programs.len());
        for (replica, specs) in replicas.iter_mut().zip(&programs) {
            run(replica, specs);
        }
        let exports: Vec<_> = replicas
            .iter()
            .map(|r| r.export_body(&key()).expect("replica exports"))
            .collect();

        let mut forward = Engine::new();
        for export in &exports {
            forward.import_body(&key(), export).expect("forward merge");
        }
        let mut backward = Engine::new();
        for export in exports.iter().rev() {
            backward.import_body(&key(), export).expect("backward merge");
        }

        prop_assert_eq!(
            view_of(&forward),
            view_of(&backward),
            "merge order changed the converged view"
        );
    }

    /// **Idempotence.** Re-delivering known material is a no-op — in the view,
    /// and in the `changed` bookkeeping that decides whether a Receipt exists.
    ///
    /// The second half is the one with teeth downstream: a `Some(Receipt)` for
    /// material the Engine already held would tell Replica something changed
    /// when nothing did, and that becomes a durable commit and a gossip round
    /// for a duplicate frame.
    #[test]
    fn redelivering_known_material_changes_nothing(programs in program()) {
        let mut replicas = fork(programs.len());
        for (replica, specs) in replicas.iter_mut().zip(&programs) {
            run(replica, specs);
        }
        let exports: Vec<_> = replicas
            .iter()
            .map(|r| r.export_body(&key()).expect("replica exports"))
            .collect();

        let mut merged = Engine::new();
        for export in &exports {
            merged.import_body(&key(), export).expect("first delivery");
        }
        let settled = view_of(&merged);

        for export in &exports {
            let receipt = merged
                .import_body(&key(), export)
                .expect("re-delivery succeeds");
            prop_assert!(
                receipt.is_none(),
                "re-importing known material reported a change"
            );
        }
        prop_assert_eq!(settled, view_of(&merged), "re-delivery moved the view");
    }
}

/// What happens to Bodies written before `check_path_type` stopped creating the
/// roots it asked about.
///
/// The fix stops NEW phantoms. It cannot remove the ones already written,
/// because at rest a phantom root is byte-for-byte a container that was written
/// and then emptied — the same empty root, with no operations distinguishing
/// them. Dropping empty roots would silently delete legitimately-empty lists;
/// keeping them means an old Body still projects paths under types it never
/// really had.
///
/// Neither option is free, so the choice is written down and pinned here rather
/// than left to be rediscovered. A reader who sees an old Body projecting a
/// `list` at a counter's path should find this test, not a mystery.
#[cfg(test)]
mod legacy_phantom_roots {
    use crate::{BodyExport, Engine, Key, Op, Transaction};
    use loro::{ExportMode, LoroDoc};

    fn key() -> Key {
        Key::from_bytes(b"legacy".to_vec())
    }

    /// A Body as a pre-fix build would have left it: a real counter at `votes`,
    /// plus the four empty roots the old sibling probe created by asking.
    fn body_written_before_the_fix() -> BodyExport {
        let doc = LoroDoc::new();
        doc.set_record_timestamp(true);
        doc.set_change_merge_interval(-1);
        doc.set_peer_id(4242).expect("fresh doc");
        doc.get_map("body").insert("k", "v").expect("body root");
        doc.get_map("cnt:votes")
            .insert("4242", 1i64)
            .expect("the real counter");
        // The probe's side effect, reproduced exactly: accessing a root creates
        // it, so the old `check_path_type` left one behind per sibling tag.
        doc.get_map("map:votes");
        doc.get_movable_list("list:votes");
        doc.get_text("text:votes");
        doc.get_map("set:votes");
        doc.commit();
        BodyExport::Collaborative(doc.export(ExportMode::Snapshot).expect("export"))
    }

    #[test]
    fn an_old_body_still_projects_its_phantoms_and_that_is_tolerated() {
        let mut engine = Engine::new();
        engine
            .import_body(&key(), &body_written_before_the_fix())
            .expect("an old Body still imports");
        let view = engine
            .read_collaborative(&key())
            .expect("and still projects");

        // The real data is intact — that is the part that matters.
        assert_eq!(view.counters.get("votes"), Some(&1));

        // And the phantoms are still there, because nothing can safely remove
        // them. Asserted rather than lamented: if a future change DOES clean
        // them up, this test fails and forces the cleanup to be deliberate.
        assert!(
            view.lists.contains_key("votes"),
            "an old Body keeps the empty roots the probe created"
        );
        assert!(view.maps.contains_key("votes"));
        assert!(view.texts.contains_key("votes"));
        assert!(view.sets.contains_key("votes"));
    }

    #[test]
    fn a_body_written_after_the_fix_has_none() {
        let mut engine = Engine::new();
        engine
            .commit(Transaction::new(
                "c",
                vec![
                    Op::CreateBody { key: key() },
                    Op::CounterAdd {
                        key: key(),
                        path: "votes".into(),
                        delta: 1,
                    },
                ],
            ))
            .expect("commits");
        let view = engine.read_collaborative(&key()).expect("projects");
        assert_eq!(view.counters.get("votes"), Some(&1));
        assert!(view.lists.is_empty(), "no phantom list");
        assert!(view.maps.is_empty(), "no phantom map");
        assert!(view.texts.is_empty(), "no phantom text");
        assert!(view.sets.is_empty(), "no phantom set");
    }
}
