//! Plan 13 F4 item 2 — reconciling by descent instead of by enumeration.
//!
//! The claim under test is a cost claim, so the tests measure cost: how many
//! nodes a peer has to fetch to learn what it is missing, and how that number
//! moves as the store grows and as divergence grows. If fetch count tracked the
//! store rather than the disagreement, the mechanism would be enumeration
//! wearing a descent's clothes.

use std::collections::BTreeMap;

use journal::index::{
    apply, build_index, node_hash, ChildRef, Failure, IndexChange, IndexEntry, IndexKey, NodeSink,
    NodeSource, Reconciliation, ReconciliationStep,
};

#[derive(Default, Clone)]
struct Nodes(BTreeMap<[u8; 32], Vec<u8>>);

impl Nodes {
    fn absorb(&mut self, sink: NodeSink) {
        for bytes in sink.written {
            self.0.insert(node_hash(&bytes), bytes);
        }
    }
}

impl NodeSource for Nodes {
    fn node(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.0.get(hash).cloned()
    }
}

fn key(n: u64) -> IndexKey {
    *blake3::hash(&n.to_be_bytes()).as_bytes()
}

fn entry(n: u64) -> IndexEntry {
    IndexEntry {
        key: key(n),
        value: n.to_be_bytes().to_vec(),
    }
}

fn built(range: std::ops::Range<u64>) -> (Nodes, Option<ChildRef>) {
    let mut nodes = Nodes::default();
    let mut sink = NodeSink::default();
    let root = build_index(range.map(entry).collect(), &mut sink).expect("build");
    nodes.absorb(sink);
    (nodes, root)
}

/// Drive a reconciliation to completion, reporting what was missing and how
/// many nodes had to be fetched to find out.
fn reconcile(
    local: &Nodes,
    local_root: Option<ChildRef>,
    remote: &Nodes,
    remote_root: Option<ChildRef>,
) -> (Vec<IndexEntry>, usize) {
    let mut session = Reconciliation::begin(local_root, remote_root, 100_000);
    let mut fetched = 0usize;
    loop {
        match session.step() {
            ReconciliationStep::Complete(missing) => return (missing, fetched),
            ReconciliationStep::Fetch(hashes) => {
                let supplied: BTreeMap<[u8; 32], Vec<u8>> = hashes
                    .iter()
                    .filter_map(|h| remote.node(h).map(|b| (*h, b)))
                    .collect();
                fetched += supplied.len();
                session.absorb(local, &supplied).expect("absorb");
            }
        }
    }
}

#[test]
fn identical_roots_cost_one_comparison_and_fetch_nothing() {
    // The steady-state case, and the one that has to be free: two converged
    // peers must not pay to discover they agree.
    let (local, root) = built(0..10_000);
    let remote = local.clone();
    let (missing, fetched) = reconcile(&local, root, &remote, root);
    assert!(missing.is_empty());
    assert_eq!(fetched, 0, "equal roots must fetch nothing at all");
}

#[test]
fn a_blank_peer_learns_the_whole_set() {
    let (remote, remote_root) = built(0..500);
    let local = Nodes::default();
    let (missing, _) = reconcile(&local, None, &remote, remote_root);
    assert_eq!(missing.len(), 500);
}

#[test]
fn fetch_cost_tracks_the_divergence_not_the_store() {
    // The load-bearing measurement. One entry of disagreement must cost about
    // the same whether the peers hold a thousand entries or fifty thousand.
    let mut costs = Vec::new();
    for size in [1_000u64, 10_000, 50_000] {
        let (local, local_root) = built(0..size);
        let mut remote = local.clone();
        let mut sink = NodeSink::default();
        let remote_root = apply(
            &remote,
            local_root,
            vec![IndexChange {
                key: key(size + 1),
                value: Some(b"new".to_vec()),
            }],
            &mut sink,
        )
        .expect("remote adds one");
        remote.absorb(sink);

        let (missing, fetched) = reconcile(&local, local_root, &remote, remote_root);
        assert_eq!(missing.len(), 1, "exactly the one new entry at {size}");
        costs.push((size, fetched));
    }
    for (size, fetched) in &costs {
        assert!(
            *fetched <= 8,
            "one entry of divergence at {size} entries cost {fetched} node fetches"
        );
    }
    let smallest = costs.first().expect("measured").1;
    let largest = costs.last().expect("measured").1;
    assert!(
        largest <= smallest + 2,
        "descent cost grew from {smallest} to {largest} across a fiftyfold store"
    );
}

#[test]
fn a_hundred_divergent_entries_cost_far_less_than_the_store() {
    let (local, local_root) = built(0..20_000);
    let mut remote = local.clone();
    let mut sink = NodeSink::default();
    let remote_root = apply(
        &remote,
        local_root,
        (20_000..20_100)
            .map(|n| IndexChange {
                key: key(n),
                value: Some(n.to_be_bytes().to_vec()),
            })
            .collect(),
        &mut sink,
    )
    .expect("remote adds a hundred");
    remote.absorb(sink);

    let (missing, fetched) = reconcile(&local, local_root, &remote, remote_root);
    assert_eq!(missing.len(), 100);
    assert!(
        (fetched as u64) * 20 < 20_000,
        "learning about 100 entries in a 20,000-entry store cost {fetched} fetches"
    );
}

#[test]
fn a_changed_value_at_a_known_key_is_reported() {
    // Divergence is not only about presence: a peer holding a *different* value
    // for a key we have must be reported, or a Body whose head moved would look
    // like agreement.
    let (local, local_root) = built(0..1_000);
    let mut remote = local.clone();
    let mut sink = NodeSink::default();
    let remote_root = apply(
        &remote,
        local_root,
        vec![IndexChange {
            key: key(7),
            value: Some(b"moved on".to_vec()),
        }],
        &mut sink,
    )
    .expect("remote updates one");
    remote.absorb(sink);

    let (missing, fetched) = reconcile(&local, local_root, &remote, remote_root);
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].value, b"moved on");
    assert!(fetched <= 8);
}

#[test]
fn what_only_the_local_side_holds_is_not_reported() {
    // The reconciliation answers one question — what does the remote have that
    // I do not — and the other direction is the remote's own descent. Mixing
    // them would make a peer fetch material it is already the source of.
    let (remote, remote_root) = built(0..500);
    let mut local = remote.clone();
    let mut sink = NodeSink::default();
    let local_root = apply(
        &local,
        remote_root,
        vec![IndexChange {
            key: key(9_999),
            value: Some(b"only mine".to_vec()),
        }],
        &mut sink,
    )
    .expect("local adds one");
    local.absorb(sink);

    let (missing, _) = reconcile(&local, local_root, &remote, remote_root);
    assert!(
        missing.is_empty(),
        "the remote has nothing we lack: {missing:?}"
    );
}

#[test]
fn a_node_that_is_not_what_was_asked_for_is_refused() {
    // A peer answers a request for one specific address. Anything else is not
    // a slow answer, it is a different answer, and trusting it would let a
    // remote steer the descent.
    let (local, local_root) = built(0..1_000);
    let (_, remote_root) = built(0..1_001);
    let mut session = Reconciliation::begin(local_root, remote_root, 100_000);
    let ReconciliationStep::Fetch(hashes) = session.step() else {
        panic!("expected a fetch");
    };
    let mut forged = BTreeMap::new();
    forged.insert(hashes[0], b"not the node you asked for".to_vec());
    assert_eq!(session.absorb(&local, &forged), Err(Failure::NonCanonical));
}

#[test]
fn a_remote_cannot_make_the_descent_unbounded() {
    let (local, local_root) = built(0..1_000);
    let (remote, remote_root) = built(0..20_000);
    let mut session = Reconciliation::begin(local_root, remote_root, 2);
    let mut refused = false;
    for _ in 0..10 {
        match session.step() {
            ReconciliationStep::Complete(_) => break,
            ReconciliationStep::Fetch(hashes) => {
                let supplied: BTreeMap<[u8; 32], Vec<u8>> = hashes
                    .iter()
                    .filter_map(|h| remote.node(h).map(|b| (*h, b)))
                    .collect();
                if session.absorb(&local, &supplied) == Err(Failure::Bounds) {
                    refused = true;
                    break;
                }
            }
        }
    }
    assert!(refused, "a low node budget must stop the descent");
}

#[test]
fn a_round_that_supplies_nothing_makes_no_progress_and_no_mess() {
    // A peer that stalls should leave the session asking for the same thing,
    // not silently completing with a partial answer.
    let (local, local_root) = built(0..1_000);
    let (_, remote_root) = built(0..2_000);
    let mut session = Reconciliation::begin(local_root, remote_root, 100_000);
    let first = session.step();
    session.absorb(&local, &BTreeMap::new()).expect("absorb");
    assert_eq!(session.step(), first, "an empty round changes nothing");
}

#[test]
fn a_resumed_descent_does_not_refetch_what_it_already_absorbed() {
    // Bootstrap is a resumable multi-session operation, so a session that is
    // interrupted and continued must make durable forward progress rather than
    // starting over.
    let (local, local_root) = built(0..5_000);
    let (remote, remote_root) = built(0..6_000);
    let mut session = Reconciliation::begin(local_root, remote_root, 100_000);
    let mut total = 0usize;
    let mut rounds = 0;
    loop {
        match session.step() {
            ReconciliationStep::Complete(missing) => {
                assert_eq!(missing.len(), 1_000);
                break;
            }
            ReconciliationStep::Fetch(hashes) => {
                rounds += 1;
                // Supply only half of what was asked for each round, as a
                // bounded session would.
                let supplied: BTreeMap<[u8; 32], Vec<u8>> = hashes
                    .iter()
                    .take(hashes.len().div_ceil(2))
                    .filter_map(|h| remote.node(h).map(|b| (*h, b)))
                    .collect();
                total += supplied.len();
                session.absorb(&local, &supplied).expect("absorb");
            }
        }
        assert!(rounds < 100, "the descent must terminate");
    }
    assert!(
        total < 1_000,
        "a partially-supplied descent still converged in {total} fetches"
    );
}
