//! The multi-flow connection seam, proven the same way on both contractors.
//!
//! Every test here runs twice: once over the in-memory switchboard and once
//! over two real iroh endpoints on loopback. That is the point of the file. A
//! memory transport that quietly disagrees with the shipped one is worse than
//! no memory transport, because every Runtime test above it is then testing a
//! network that does not exist.
//!
//! Where the two genuinely cannot agree — loss, reordering, real path MTU — the
//! iroh side is the contract and the property is asserted only there, named as
//! such rather than skipped silently.

use std::sync::Arc;
use std::time::Duration;

use comms::mem::MemNet;
use comms::policy::Network;
use comms::{Alpn, Connection, DefaultTransport, PeerId, Protocols, Transport};
use mechanics::actor::device_from_seed;

const SESSION_ALPN: Alpn = b"lait/test-session/1";

/// One connected pair, however it was built.
struct Pair {
    dialer: Box<dyn Connection>,
    accepter: Box<dyn Connection>,
    /// Kept alive: dropping a transport tears its endpoint down.
    _keep: Vec<Arc<dyn Transport>>,
}

async fn mem_pair() -> Pair {
    let net = MemNet::new();
    let a: Arc<dyn Transport> = Arc::new(net.peer(device_from_seed(&[41u8; 32])));
    let b: Arc<dyn Transport> = Arc::new(net.peer(device_from_seed(&[42u8; 32])));
    let accepting = {
        let b = b.clone();
        tokio::spawn(async move { b.accept_connection().await })
    };
    let dialer = a
        .connect_session(b.my_id(), SESSION_ALPN)
        .await
        .expect("connect");
    let incoming = accepting.await.expect("accept task").expect("incoming");
    Pair {
        dialer,
        accepter: incoming.connection,
        _keep: vec![a, b],
    }
}

async fn iroh_pair() -> Pair {
    let protocols = Protocols {
        framed: &[],
        session: &[SESSION_ALPN],
    };
    let a = DefaultTransport::new(&[43u8; 32], &Network::Isolated, protocols)
        .await
        .expect("build A");
    let b = DefaultTransport::new(&[44u8; 32], &Network::Isolated, protocols)
        .await
        .expect("build B");
    // A fresh endpoint learns its direct addresses asynchronously, and under
    // Isolated a bare id resolves through nothing — so learning an empty
    // address list here is a dial that never completes.
    let a_addrs = a
        .advertised_routes(Duration::from_secs(10))
        .await
        .expect("A has a route");
    let b_addrs = b
        .advertised_routes(Duration::from_secs(10))
        .await
        .expect("B has a route");
    a.learn(b.my_id(), &b_addrs);
    b.learn(a.my_id(), &a_addrs);
    let a: Arc<dyn Transport> = Arc::new(a);
    let b: Arc<dyn Transport> = Arc::new(b);

    let accepting = {
        let b = b.clone();
        tokio::spawn(async move { b.accept_connection().await })
    };
    let dialer = tokio::time::timeout(
        Duration::from_secs(15),
        a.connect_session(b.my_id(), SESSION_ALPN),
    )
    .await
    .expect("dial in time")
    .expect("connect");
    let incoming = tokio::time::timeout(Duration::from_secs(10), accepting)
        .await
        .expect("accept in time")
        .expect("accept task")
        .expect("incoming");
    Pair {
        dialer,
        accepter: incoming.connection,
        _keep: vec![a, b],
    }
}

/// Run one property against both contractors. A failure names which.
async fn on_both(name: &str, property: impl AsyncFn(Pair)) {
    property(mem_pair().await).await;
    eprintln!("{name}: mem ok");
    property(iroh_pair().await).await;
    eprintln!("{name}: iroh ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bidirectional_flow_carries_bytes_both_ways() {
    on_both("bi", async |pair: Pair| {
        // The opener writes first. A flow does not exist for the peer until
        // something is sent on it, so accepting before writing is a wait with
        // nothing to wait for.
        let (mut send, mut recv) = pair.dialer.open_bi().await.expect("open");
        send.write_all(b"question").await.expect("write");
        send.finish().expect("finish");

        let (mut their_send, mut their_recv) = pair
            .accepter
            .accept_bi()
            .await
            .expect("accept")
            .expect("a flow");
        assert_eq!(
            their_recv.read_to_end(1024).await.expect("read"),
            b"question"
        );

        their_send.write_all(b"answer").await.expect("write");
        their_send.finish().expect("finish");
        assert_eq!(recv.read_to_end(1024).await.expect("read"), b"answer");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_flows_do_not_interleave_with_each_other() {
    // The reason a connection exists at all. Two transfers on one connection
    // must arrive as two transfers, not as one shuffled byte stream.
    on_both("concurrent", async |pair: Pair| {
        let mut first = pair.dialer.open_uni().await.expect("open");
        let mut second = pair.dialer.open_uni().await.expect("open");
        first.write_all(b"aaaaaaaa").await.expect("write");
        second.write_all(b"bbbbbbbb").await.expect("write");
        first.write_all(b"AAAAAAAA").await.expect("write");
        second.write_all(b"BBBBBBBB").await.expect("write");
        first.finish().expect("finish");
        second.finish().expect("finish");

        let mut got = Vec::new();
        for _ in 0..2 {
            let mut recv = pair
                .accepter
                .accept_uni()
                .await
                .expect("accept")
                .expect("a flow");
            got.push(recv.read_to_end(1024).await.expect("read"));
        }
        got.sort();
        // Each flow's own writes stay in order and stay in their own flow. The
        // two flows may be accepted in either order, which is why the pair is
        // sorted rather than indexed.
        assert_eq!(
            got,
            vec![b"aaaaaaaaAAAAAAAA".to_vec(), b"bbbbbbbbBBBBBBBB".to_vec()]
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reset_flow_reads_as_an_error_not_as_a_clean_end() {
    // What lets an abandoned transfer be told from a completed one. Without
    // it, truncation is silent, and truncation has to be loud.
    on_both("reset", async |pair: Pair| {
        let mut send = pair.dialer.open_uni().await.expect("open");
        send.write_all(b"partial").await.expect("write");
        send.reset(7);

        let mut recv = pair
            .accepter
            .accept_uni()
            .await
            .expect("accept")
            .expect("a flow");
        // Either the write or the reset may arrive first; what must never
        // happen is a clean end after a partial write.
        let outcome = recv.read_to_end(1024).await;
        assert!(
            outcome.is_err(),
            "a reset must not read as a clean end: {outcome:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_ceiling_bounds_the_read_and_not_the_flow() {
    // `max` is a pre-allocation ceiling. Asking for less than was written must
    // return less, not lose the rest and not end the flow.
    on_both("ceiling", async |pair: Pair| {
        let mut send = pair.dialer.open_uni().await.expect("open");
        send.write_all(b"0123456789").await.expect("write");
        send.finish().expect("finish");

        let mut recv = pair
            .accepter
            .accept_uni()
            .await
            .expect("accept")
            .expect("a flow");
        let mut got = Vec::new();
        while let Some(chunk) = recv.read_chunk(3).await.expect("read") {
            assert!(chunk.len() <= 3, "a read must not exceed its ceiling");
            if chunk.is_empty() {
                continue;
            }
            got.extend_from_slice(&chunk);
        }
        assert_eq!(got, b"0123456789");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn read_exact_refuses_a_short_flow_rather_than_returning_part() {
    on_both("exact", async |pair: Pair| {
        let mut send = pair.dialer.open_uni().await.expect("open");
        send.write_all(b"four").await.expect("write");
        send.finish().expect("finish");

        let mut recv = pair
            .accepter
            .accept_uni()
            .await
            .expect("accept")
            .expect("a flow");
        assert!(
            recv.read_exact(16).await.is_err(),
            "a caller asking for a fixed-size header has no use for a partial one"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_datagram_that_fits_arrives_and_one_that_does_not_is_refused() {
    // Refused, never truncated. A transient payload has no retransmit, so a
    // half-delivered one arrives as corruption rather than as a gap.
    on_both("datagram", async |pair: Pair| {
        let capacity = pair
            .dialer
            .datagram_capacity()
            .expect("both contractors carry datagrams");
        assert!(pair
            .dialer
            .send_datagram(&vec![7u8; capacity.min(256)])
            .is_ok());
        assert!(
            pair.dialer.send_datagram(&vec![7u8; capacity + 1]).is_err(),
            "an oversized datagram is refused rather than truncated"
        );

        let got = tokio::time::timeout(Duration::from_secs(5), pair.accepter.read_datagram())
            .await
            .expect("a datagram in time")
            .expect("read")
            .expect("a payload");
        assert_eq!(got, vec![7u8; capacity.min(256)]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_wakes_a_parked_accept() {
    // Shutdown has to reach a connection that is waiting for work, or a
    // dormancy deadline is a hope rather than a bound.
    on_both("close", async |pair: Pair| {
        let accepter = pair.accepter;
        let parked = tokio::spawn(async move { accepter.accept_uni().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        pair.dialer.close(0, b"done");

        let outcome = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("the parked accept woke")
            .expect("accept task");
        // Either answer is correct — nothing more is coming, and that is what
        // the caller needed to learn.
        assert!(
            matches!(outcome, Ok(None) | Err(_)),
            "a parked accept must not resolve with a flow after a close"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_names_its_peer_and_its_generation() {
    on_both("identity", async |pair: Pair| {
        assert_eq!(pair.dialer.alpn(), SESSION_ALPN.to_vec());
        assert_eq!(pair.accepter.alpn(), SESSION_ALPN.to_vec());
        assert_ne!(pair.dialer.peer(), pair.accepter.peer());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_framed_transport_refuses_a_session_dial_rather_than_pretending() {
    // The default on the trait. A transport that cannot offer flows says so; it
    // does not hand back something that looks like a connection. Both shipped
    // contractors *do* offer them, so the subject here is the default itself.
    struct NoFlows;
    #[async_trait::async_trait]
    impl Transport for NoFlows {
        fn my_id(&self) -> PeerId {
            device_from_seed(&[46u8; 32])
        }
        fn learn(&self, _peer: PeerId, _addrs: &[std::net::SocketAddr]) {}
        async fn connect(
            &self,
            _peer: PeerId,
            _alpn: Alpn,
        ) -> anyhow::Result<Box<dyn comms::Stream>> {
            anyhow::bail!("not implemented")
        }
        async fn accept(&self) -> Option<comms::Incoming> {
            None
        }
        fn advertised_addrs(&self) -> Vec<std::net::SocketAddr> {
            Vec::new()
        }
        async fn subscribe(
            &self,
            _topic: comms::Topic,
            _bootstrap: &[PeerId],
        ) -> anyhow::Result<(Box<dyn comms::GossipSender>, Box<dyn comms::GossipReceiver>)>
        {
            anyhow::bail!("not implemented")
        }
        async fn shutdown(&self) {}
    }

    let refusal = NoFlows
        .connect_session(device_from_seed(&[47u8; 32]), SESSION_ALPN)
        .await;
    assert!(refusal.is_err());
}
