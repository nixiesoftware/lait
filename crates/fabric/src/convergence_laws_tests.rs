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
    })
}

fn op_spec() -> impl Strategy<Value = OpSpec> {
    prop_oneof![
        (0usize..2, 0u8..6).prop_map(|(path, value)| OpSpec::RegisterSet { path, value }),
        (0usize..2).prop_map(|path| OpSpec::RegisterClear { path }),
        (0usize..2, 0u8..4, 0u8..6).prop_map(|(path, entry, value)| OpSpec::MapSet {
            path,
            entry,
            value
        }),
        (0usize..2, 0u8..4).prop_map(|(path, entry)| OpSpec::MapRemove { path, entry }),
        (0usize..2, 0u8..8, 0u8..6).prop_map(|(path, at, value)| OpSpec::ListInsert {
            path,
            at,
            value
        }),
        (0usize..2, 0u8..8).prop_map(|(path, nth)| OpSpec::ListRemoveNth { path, nth }),
        (0usize..2, 0u8..8, 0u8..8).prop_map(|(path, nth, to)| OpSpec::ListMoveNth {
            path,
            nth,
            to
        }),
        (0u8..12, 0u8..4, 0u8..6).prop_map(|(at, delete, insert)| OpSpec::TextSplice {
            at,
            delete,
            insert
        }),
        (0usize..2, 0u8..6).prop_map(|(path, value)| OpSpec::SetAdd { path, value }),
        (0usize..2, 0u8..6).prop_map(|(path, value)| OpSpec::SetRemove { path, value }),
        (0usize..2, -4i8..5).prop_map(|(path, delta)| OpSpec::CounterAdd { path, delta }),
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

proptest! {
    // 64 rather than proptest's default 256: this tier runs on every push, and
    // each case builds and merges up to four real Loro documents. Coverage
    // comes from accumulating distinct seeds across runs, not from one
    // exhaustive run — raise PROPTEST_CASES in the nightly tier.
    #![proptest_config(ProptestConfig::with_cases(64))]

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
