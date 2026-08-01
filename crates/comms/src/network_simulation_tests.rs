//! A seeded network, and the proof that it is one.
//!
//! `mem.rs` has described itself as "the seed of the deterministic network
//! simulator (controllable delivery: drop/delay/partition)" since it was
//! written. This is the delivery half arriving, and the tests that keep it
//! honest.
//!
//! ## What a seeded simulator has to prove about itself
//!
//! The value of seeding is that a failure replays. That is a claim about the
//! harness, not about the system under test, and it is the claim most likely to
//! be quietly false — one `HashMap` iteration, one unseeded generator, one
//! thread, and the seed stops determining anything while every test still
//! passes.
//!
//! So the first test here does not test the network at all. It runs the same
//! seed twice and compares a trace of every delivery decision byte for byte.
//! S2 built the same thing for the same reason and called it a meta test; it is
//! the difference between a simulator and a random number generator with good
//! intentions.
//!
//! ## What is NOT modelled, and why
//!
//! **Delay.** Drop, duplicate and partition are decided synchronously at the
//! delivery point, which keeps the decision order deterministic. Delay needs a
//! timer per message, and a timer needs a task, and once deliveries are racing
//! on the runtime's scheduler the order stops being a function of the seed. It
//! is buildable — pair it with `tokio::time::pause` so the timers are virtual —
//! but it is a second mechanism rather than a knob on this one, and the
//! convergence properties this exists to test retry rather than depend on
//! arrival time. Left out on purpose rather than half-built.
//!
//! **Reordering** is likewise absent between two peers, because the channels
//! underneath are FIFO. What the simulation does reorder is *whose* message
//! arrives first, which is where the interesting interleavings live.

use std::time::Duration;

use crate::mem::{Faults, MemNet};
use crate::{GossipEvent, PeerId, Topic, Transport};

fn id(seed: u8) -> PeerId {
    mechanics::actor::device_from_seed(&[seed; 32])
}

fn topic() -> Topic {
    Topic([9u8; 32])
}

/// Every delivery decision one run made, in order. Comparing two of these is
/// how determinism gets asserted rather than assumed.
async fn trace(seed: u64, faults: Faults) -> Vec<String> {
    let net = MemNet::seeded(seed, faults);
    let peers: Vec<_> = (1..=4u8).map(|n| net.peer(id(n))).collect();

    let mut subscriptions = Vec::new();
    for peer in &peers {
        subscriptions.push(peer.subscribe(topic(), &[]).await.expect("subscribe"));
    }

    let mut log = Vec::new();
    for round in 0..8u8 {
        // A partition that comes and goes, so the trace covers both states.
        if round == 3 {
            net.partition(&id(1), &id(3));
            log.push("partition 1|3".to_owned());
        }
        if round == 6 {
            net.heal();
            log.push("heal".to_owned());
        }

        let sender = usize::from(round % 4);
        let (send, _) = &subscriptions[sender];
        send.broadcast(vec![round]).await.expect("broadcast");

        // Drain what each peer can see without parking: a receiver that would
        // block has, by definition, not been delivered anything.
        for (index, (_, recv)) in subscriptions.iter_mut().enumerate() {
            while let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(1), recv.next()).await
            {
                let line = match event {
                    GossipEvent::Received { from, bytes } => {
                        format!("peer{index} <- {from:?} {bytes:?}")
                    }
                    GossipEvent::NeighborUp(peer) => format!("peer{index} up {peer:?}"),
                    other => format!("peer{index} {other:?}"),
                };
                log.push(line);
            }
        }
    }

    let counts = net.delivered();
    log.push(format!(
        "sent={} dropped={} duplicated={} partitioned={}",
        counts.sent, counts.dropped, counts.duplicated, counts.partitioned
    ));
    log
}

/// **The meta test.** The same seed twice produces the same trace.
///
/// Without this every other seeded test is decoration: a seed that does not
/// determine the run cannot replay a failure, which is the only reason to have
/// one. Compared line by line rather than by length, because two runs that drop
/// the same NUMBER of messages and different messages are not the same run.
#[tokio::test]
async fn the_same_seed_replays_exactly() {
    let first = trace(0xA11CE, Faults::LOSSY).await;
    let second = trace(0xA11CE, Faults::LOSSY).await;
    assert_eq!(
        first, second,
        "the same seed produced two different runs — the network is not deterministic"
    );
    assert!(
        first.len() > 10,
        "a trace this short is not evidence of anything: {first:?}"
    );
}

/// And a different seed produces a different run — otherwise the seed is being
/// ignored rather than honoured, which the test above cannot distinguish.
#[tokio::test]
async fn a_different_seed_is_a_different_run() {
    let a = trace(0xA11CE, Faults::LOSSY).await;
    let b = trace(0xB0B, Faults::LOSSY).await;
    assert_ne!(
        a, b,
        "two seeds produced identical runs — the seed is inert"
    );
}

/// The perfect network stays perfect. This is the default every existing
/// MemNet caller gets, and the assertion that they were not silently handed a
/// lossy network when faults arrived.
#[tokio::test]
async fn the_default_network_loses_nothing() {
    let net = MemNet::new();
    let a = net.peer(id(1));
    let b = net.peer(id(2));
    let (a_send, _a_recv) = a.subscribe(topic(), &[]).await.expect("subscribe");
    let (_b_send, mut b_recv) = b.subscribe(topic(), &[]).await.expect("subscribe");

    for n in 0..32u8 {
        a_send.broadcast(vec![n]).await.expect("broadcast");
    }
    let mut received = 0;
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(5), b_recv.next()).await
    {
        if matches!(event, GossipEvent::Received { .. }) {
            received += 1;
        }
    }
    assert_eq!(received, 32, "the default network must deliver everything");

    let counts = net.delivered();
    assert_eq!(counts.dropped, 0);
    assert_eq!(counts.duplicated, 0);
    assert_eq!(counts.partitioned, 0);
}

/// A partition is symmetric and total: neither side hears the other, and
/// healing restores both directions.
///
/// Asserted in both directions on purpose. Storing the pair smallest-first is
/// what makes symmetry a property of the representation, and a test that only
/// checked one direction would pass just as well if it were not.
#[tokio::test]
async fn a_partition_is_symmetric_and_heals() {
    let net = MemNet::seeded(7, Faults::PERFECT);
    let a = net.peer(id(1));
    let b = net.peer(id(2));
    let (a_send, mut a_recv) = a.subscribe(topic(), &[]).await.expect("subscribe");
    let (b_send, mut b_recv) = b.subscribe(topic(), &[]).await.expect("subscribe");

    async fn drain(recv: &mut Box<dyn crate::GossipReceiver>) -> usize {
        let mut seen = 0;
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(5), recv.next()).await
        {
            if matches!(event, GossipEvent::Received { .. }) {
                seen += 1;
            }
        }
        seen
    }

    // Clear the NeighborUp traffic subscription produced.
    drain(&mut a_recv).await;
    drain(&mut b_recv).await;

    net.partition(&id(1), &id(2));
    a_send.broadcast(b"a to b".to_vec()).await.expect("send");
    b_send.broadcast(b"b to a".to_vec()).await.expect("send");
    assert_eq!(drain(&mut b_recv).await, 0, "b heard a across a partition");
    assert_eq!(drain(&mut a_recv).await, 0, "a heard b across a partition");

    net.heal();
    a_send.broadcast(b"a to b".to_vec()).await.expect("send");
    b_send.broadcast(b"b to a".to_vec()).await.expect("send");
    assert_eq!(drain(&mut b_recv).await, 1, "healing did not restore a->b");
    assert_eq!(drain(&mut a_recv).await, 1, "healing did not restore b->a");
}

/// The faults are actually injected. A chaos test that never dropped anything
/// is a slow way of testing the happy path, and the counters are how that gets
/// noticed rather than assumed.
#[tokio::test]
async fn a_lossy_network_actually_loses() {
    let log = trace(0xFACE, Faults::LOSSY).await;
    let summary = log.last().expect("trace ends with counters");
    let dropped: u64 = summary
        .split_whitespace()
        .find_map(|field| field.strip_prefix("dropped="))
        .and_then(|value| value.parse().ok())
        .expect("counters parse");
    let partitioned: u64 = summary
        .split_whitespace()
        .find_map(|field| field.strip_prefix("partitioned="))
        .and_then(|value| value.parse().ok())
        .expect("counters parse");
    assert!(dropped > 0, "a LOSSY network dropped nothing: {summary}");
    assert!(partitioned > 0, "the partition blocked nothing: {summary}");
}
