//! Convergence fixtures for the collaborative algebra over the Loro engine:
//! two replicas fork from a common ancestor, edit concurrently, cross-merge,
//! and must converge to the same view with the declared semantics — add-wins
//! sets, summed counters, no lost list inserts, preserved concurrent text, and
//! an agreed LWW register winner.

use fabric::{Engine, Key, Op, Transaction};

fn key() -> Key {
    Key::from_bytes(b"body/collab".to_vec())
}

fn req(ops: Vec<Op>) -> Transaction {
    Transaction::new("test", ops)
}

/// A common ancestor with every path initialized (the documented discipline:
/// paths are created in the Body's creating transaction, before concurrent
/// editing), then forked into two engines.
fn forked_pair() -> (Engine, Engine) {
    let mut a = Engine::new();
    a.commit(req(vec![
        Op::CreateBody { key: key() },
        Op::RegisterSet {
            key: key(),
            path: "title".into(),
            value: b"base".to_vec(),
        },
        Op::MapSet {
            key: key(),
            path: "fields".into(),
            entry: "seed".into(),
            value: b"1".to_vec(),
        },
        Op::ListInsert {
            key: key(),
            path: "items".into(),
            index: 0,
            value: b"first".to_vec(),
        },
        Op::ListInsert {
            key: key(),
            path: "items".into(),
            index: 1,
            value: b"second".to_vec(),
        },
        Op::TextSplice {
            key: key(),
            path: "notes".into(),
            index: 0,
            delete: 0,
            insert: "hello".into(),
        },
        Op::SetAdd {
            key: key(),
            path: "tags".into(),
            value: b"keep".to_vec(),
        },
        Op::CounterAdd {
            key: key(),
            path: "votes".into(),
            delta: 1,
        },
    ]))
    .unwrap();
    // Fork by cloning the single Body's canonical per-Body export into b.
    let export = a.export_body(&key()).unwrap();
    let mut b = Engine::new();
    b.import_body(&key(), &export).unwrap();
    (a, b)
}

/// Cross-merge both engines (per-Body) and assert their views are identical.
fn converge(a: &mut Engine, b: &mut Engine) -> fabric::CollaborativeView {
    let ea = a.export_body(&key()).unwrap();
    let eb = b.export_body(&key()).unwrap();
    a.import_body(&key(), &eb).unwrap();
    b.import_body(&key(), &ea).unwrap();
    let va = a.read_collaborative(&key()).unwrap();
    let vb = b.read_collaborative(&key()).unwrap();
    assert_eq!(va, vb, "both replicas converge to the same view");
    va
}

#[test]
fn concurrent_counter_increments_sum() {
    let (mut a, mut b) = forked_pair();
    a.commit(req(vec![Op::CounterAdd {
        key: key(),
        path: "votes".into(),
        delta: 5,
    }]))
    .unwrap();
    b.commit(req(vec![Op::CounterAdd {
        key: key(),
        path: "votes".into(),
        delta: 3,
    }]))
    .unwrap();
    let v = converge(&mut a, &mut b);
    // 1 (ancestor) + 5 + 3: concurrent increments never overwrite each other.
    assert_eq!(v.counters["votes"], 9);
}

#[test]
fn a_concurrent_add_survives_a_remove_add_wins() {
    let (mut a, mut b) = forked_pair();
    // A removes the common member while B concurrently re-adds it.
    a.commit(req(vec![Op::SetRemove {
        key: key(),
        path: "tags".into(),
        value: b"keep".to_vec(),
    }]))
    .unwrap();
    b.commit(req(vec![Op::SetAdd {
        key: key(),
        path: "tags".into(),
        value: b"keep".to_vec(),
    }]))
    .unwrap();
    let v = converge(&mut a, &mut b);
    // Add wins: B's add minted a tag A's remove never observed.
    assert_eq!(v.sets["tags"], vec![b"keep".to_vec()]);
}

#[test]
fn no_list_insert_is_lost() {
    let (mut a, mut b) = forked_pair();
    a.commit(req(vec![Op::ListInsert {
        key: key(),
        path: "items".into(),
        index: 2,
        value: b"from-a".to_vec(),
    }]))
    .unwrap();
    b.commit(req(vec![Op::ListInsert {
        key: key(),
        path: "items".into(),
        index: 2,
        value: b"from-b".to_vec(),
    }]))
    .unwrap();
    let v = converge(&mut a, &mut b);
    let values: Vec<&[u8]> = v.lists["items"]
        .iter()
        .map(|e| e.value.as_slice())
        .collect();
    assert_eq!(values.len(), 4, "2 ancestor + both concurrent inserts");
    assert!(values.contains(&&b"from-a"[..]));
    assert!(values.contains(&&b"from-b"[..]));
    // Ancestor order is preserved.
    assert_eq!(values[0], b"first");
    assert_eq!(values[1], b"second");
}

#[test]
fn stable_element_identity_survives_sync() {
    let (mut a, mut b) = forked_pair();
    // B learns the element ids from its own view (forked from the ancestor).
    let vb = b.read_collaborative(&key()).unwrap();
    let second = vb.lists["items"][1].element.clone();
    // A concurrently inserts at the front — shifting every index — while B
    // removes "second" BY ID. The remove targets the right element regardless.
    a.commit(req(vec![Op::ListInsert {
        key: key(),
        path: "items".into(),
        index: 0,
        value: b"shifter".to_vec(),
    }]))
    .unwrap();
    b.commit(req(vec![Op::ListRemove {
        key: key(),
        path: "items".into(),
        element: second,
    }]))
    .unwrap();
    let v = converge(&mut a, &mut b);
    let values: Vec<&[u8]> = v.lists["items"]
        .iter()
        .map(|e| e.value.as_slice())
        .collect();
    assert_eq!(values.len(), 2);
    assert!(values.contains(&&b"shifter"[..]));
    assert!(values.contains(&&b"first"[..]));
    assert!(!values.contains(&&b"second"[..]), "removed by stable id");
}

#[test]
fn concurrent_text_splices_both_survive() {
    let (mut a, mut b) = forked_pair();
    // Ancestor text is "hello". A prepends, B appends.
    a.commit(req(vec![Op::TextSplice {
        key: key(),
        path: "notes".into(),
        index: 0,
        delete: 0,
        insert: "A:".into(),
    }]))
    .unwrap();
    b.commit(req(vec![Op::TextSplice {
        key: key(),
        path: "notes".into(),
        index: 5,
        delete: 0,
        insert: ":B".into(),
    }]))
    .unwrap();
    let v = converge(&mut a, &mut b);
    let text = &v.texts["notes"];
    assert!(text.contains("A:"), "A's edit survives: {text}");
    assert!(text.contains(":B"), "B's edit survives: {text}");
    assert!(text.contains("hello"), "ancestor text survives: {text}");
}

#[test]
fn concurrent_register_sets_agree_on_one_winner() {
    let (mut a, mut b) = forked_pair();
    a.commit(req(vec![Op::RegisterSet {
        key: key(),
        path: "title".into(),
        value: b"from-a".to_vec(),
    }]))
    .unwrap();
    b.commit(req(vec![Op::RegisterSet {
        key: key(),
        path: "title".into(),
        value: b"from-b".to_vec(),
    }]))
    .unwrap();
    let v = converge(&mut a, &mut b);
    let winner = &v.registers["title"];
    assert!(
        winner == b"from-a" || winner == b"from-b",
        "one of the concurrent writes wins on both replicas"
    );
}

#[test]
fn concurrent_map_entries_merge_disjoint_and_lww_same_key() {
    let (mut a, mut b) = forked_pair();
    a.commit(req(vec![
        Op::MapSet {
            key: key(),
            path: "fields".into(),
            entry: "only_a".into(),
            value: b"a".to_vec(),
        },
        Op::MapSet {
            key: key(),
            path: "fields".into(),
            entry: "shared".into(),
            value: b"a".to_vec(),
        },
    ]))
    .unwrap();
    b.commit(req(vec![
        Op::MapSet {
            key: key(),
            path: "fields".into(),
            entry: "only_b".into(),
            value: b"b".to_vec(),
        },
        Op::MapSet {
            key: key(),
            path: "fields".into(),
            entry: "shared".into(),
            value: b"b".to_vec(),
        },
    ]))
    .unwrap();
    let v = converge(&mut a, &mut b);
    let fields = &v.maps["fields"];
    // Disjoint entries both survive; the contested one has a single winner.
    assert_eq!(fields["only_a"], b"a");
    assert_eq!(fields["only_b"], b"b");
    assert_eq!(fields["seed"], b"1");
    assert!(fields["shared"] == b"a" || fields["shared"] == b"b");
}

/// What placement in a converged sequence does and does not promise, pinned
/// because the product has to build a comment thread on top of the difference.
///
/// A replica fifty comments behind appends, and its node lands near the front:
/// "the end" is a statement about the writer's own view, and no sequence CRDT
/// can make it a statement about the converged one. A tree does not fix that
/// and this fixture exists so nobody believes it does — the fractional index a
/// creating replica generates is computed against the siblings it can see, in
/// exactly the way a list index is.
///
/// What the tree *does* promise is everything a thread actually needs from the
/// substrate: the append is not lost, it keeps the parent it was filed under,
/// and both replicas agree on where it sits. Chronology is the record's job —
/// a comment carries the time it was written, and the product orders siblings
/// by it (see `products/issues`), which is a total order no replica's local
/// view can skew.
#[test]
fn placement_is_local_but_the_converged_order_is_agreed() {
    let (mut a, mut b) = forked_pair();
    // A carries the conversation forward while B is offline.
    for i in 0..50u8 {
        a.commit(req(vec![Op::TreeInsert {
            key: key(),
            path: "comments".into(),
            parent: None,
            after: None,
            value: vec![b'a', i],
        }]))
        .unwrap();
    }
    // B, which has seen none of that, appends against a view of nothing.
    b.commit(req(vec![Op::TreeInsert {
        key: key(),
        path: "comments".into(),
        parent: None,
        after: None,
        value: b"from-b".to_vec(),
    }]))
    .unwrap();

    let before = b.read_collaborative(&key()).unwrap().trees["comments"].len();
    assert_eq!(before, 1, "B really was writing against a stale view");

    let v = converge(&mut a, &mut b);
    let nodes = &v.trees["comments"];
    assert_eq!(nodes.len(), 51, "no append was lost");
    assert!(
        nodes.iter().any(|n| n.value == b"from-b"),
        "the stale writer's comment is present and reachable"
    );
    // `converge` already asserted the two replicas project identically, which
    // is the half that matters: wherever the stale append landed, it landed
    // there for everyone.
}

/// Two people reply to the same comment at the same time. Both replies survive,
/// both hang off the comment they answered, and the two replicas agree on the
/// order — none of which a `parent` field over a flat list can promise.
#[test]
fn concurrent_replies_to_one_comment_both_survive_under_it() {
    let (mut a, mut b) = forked_pair();
    a.commit(req(vec![Op::TreeInsert {
        key: key(),
        path: "comments".into(),
        parent: None,
        after: None,
        value: b"question".to_vec(),
    }]))
    .unwrap();
    let export = a.export_body(&key()).unwrap();
    b.import_body(&key(), &export).unwrap();
    let root = a.read_collaborative(&key()).unwrap().trees["comments"][0]
        .node
        .clone();

    for (engine, body) in [(&mut a, &b"answer-a"[..]), (&mut b, &b"answer-b"[..])] {
        engine
            .commit(req(vec![Op::TreeInsert {
                key: key(),
                path: "comments".into(),
                parent: Some(root.clone()),
                after: None,
                value: body.to_vec(),
            }]))
            .unwrap();
    }

    let v = converge(&mut a, &mut b);
    let replies: Vec<&[u8]> = v.trees["comments"]
        .iter()
        .filter(|n| n.parent.as_deref() == Some(root.as_str()))
        .map(|n| n.value.as_slice())
        .collect();
    assert_eq!(replies.len(), 2, "neither reply was lost");
    assert!(replies.contains(&&b"answer-a"[..]) && replies.contains(&&b"answer-b"[..]));
}

/// Two replicas re-parent the same node at once. A `parent` field would keep
/// whichever write happened to be projected last, with no guarantee the two
/// replicas keep the *same* one; the tree converges on one hierarchy and says
/// so identically on both.
#[test]
fn concurrent_reparenting_converges_on_one_hierarchy() {
    let (mut a, mut b) = forked_pair();
    let seed = |engine: &mut Engine, value: &[u8]| {
        engine
            .commit(req(vec![Op::TreeInsert {
                key: key(),
                path: "threads".into(),
                parent: None,
                after: None,
                value: value.to_vec(),
            }]))
            .unwrap();
    };
    seed(&mut a, b"one");
    seed(&mut a, b"two");
    seed(&mut a, b"moved");
    b.import_body(&key(), &a.export_body(&key()).unwrap())
        .unwrap();
    let nodes = a.read_collaborative(&key()).unwrap().trees["threads"].clone();
    let (one, two, moved) = (
        nodes[0].node.clone(),
        nodes[1].node.clone(),
        nodes[2].node.clone(),
    );

    // A hangs `moved` under `one`; B hangs it under `two`, concurrently.
    for (engine, parent) in [(&mut a, &one), (&mut b, &two)] {
        engine
            .commit(req(vec![Op::TreeMove {
                key: key(),
                path: "threads".into(),
                node: moved.clone(),
                parent: Some(parent.clone()),
                after: None,
            }]))
            .unwrap();
    }

    let v = converge(&mut a, &mut b);
    let node = v.trees["threads"]
        .iter()
        .find(|n| n.node == moved)
        .expect("the moved node survived both moves");
    let parent = node.parent.as_deref().expect("it hangs under one of them");
    assert!(
        parent == one || parent == two,
        "it hangs under one of the two parents that were named"
    );
    assert_eq!(
        v.trees["threads"].len(),
        3,
        "a contested move keeps the node reachable — no detached subtree"
    );
}

/// The count is the part that has to survive concurrency exactly. Two replicas
/// appending while apart must not agree on a total that is either of their own
/// — a single number would have done exactly that, keeping one writer's tally
/// and discarding the other's.
#[test]
fn concurrent_log_appends_all_survive_and_the_count_is_exact() {
    let (mut a, mut b) = forked_pair();
    for (engine, tag) in [(&mut a, b'a'), (&mut b, b'b')] {
        for i in 0..3u8 {
            engine
                .commit(req(vec![Op::LogAppend {
                    key: key(),
                    path: "feed".into(),
                    value: vec![tag, i],
                    retain: 64,
                }]))
                .unwrap();
        }
    }
    let v = converge(&mut a, &mut b);
    let feed = &v.logs["feed"];
    assert_eq!(feed.appended, 6, "every append counted, from both replicas");
    assert_eq!(feed.entries.len(), 6, "and none was lost");
}

/// Trimming under concurrency: replicas drop different entries and still agree
/// afterwards. The retained window is approximate here — that is the declared
/// trade — while the count stays exact, which is what a reader relies on.
#[test]
fn replicas_that_trimmed_differently_still_converge() {
    let (mut a, mut b) = forked_pair();
    for i in 0..10u8 {
        a.commit(req(vec![Op::LogAppend {
            key: key(),
            path: "feed".into(),
            value: vec![b'a', i],
            retain: 4,
        }]))
        .unwrap();
    }
    for i in 0..6u8 {
        b.commit(req(vec![Op::LogAppend {
            key: key(),
            path: "feed".into(),
            value: vec![b'b', i],
            retain: 4,
        }]))
        .unwrap();
    }
    // `converge` asserts both replicas project identically, which is the
    // claim: two different trimming histories, one agreed state.
    let v = converge(&mut a, &mut b);
    assert_eq!(v.logs["feed"].appended, 16, "the count lost nothing");
    assert!(
        v.logs["feed"].entries.len() <= 8,
        "state stayed bounded rather than growing with the feed"
    );
}
