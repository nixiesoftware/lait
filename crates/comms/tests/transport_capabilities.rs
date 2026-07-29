//! Plan 14 S0 item 2 — what the pinned transport actually does.
//!
//! Every number plan 14 freezes is either a *lait policy* choice or an
//! observation of iroh's behaviour, and the two need telling apart: a policy
//! number is ours to change, an observed one has to be re-measured when the
//! dependency moves. `iroh = 1.0.0-rc.1` is a release candidate, so recording
//! which is which is the whole point of this file — a number nobody can tell
//! apart from a policy choice becomes unrevisable by accident.
//!
//! This is the only place in the workspace outside `crates/comms/src` that may
//! name iroh, and it does so deliberately: measuring a dependency's behaviour
//! means touching it.

use std::time::{Duration, Instant};

use iroh::endpoint::presets;
use iroh::Endpoint;

const ALPN: &[u8] = b"lait/s0-measure/1";

/// Bind two endpoints with no relay and connect them directly. The server
/// connection is returned rather than dropped — dropping it closes the
/// connection out from under the client, and every measurement below needs a
/// peer that is still there.
async fn connected() -> (
    Endpoint,
    Endpoint,
    iroh::endpoint::Connection,
    iroh::endpoint::Connection,
) {
    let accepter = Endpoint::bind(presets::N0).await.expect("bind accepter");
    accepter.set_alpns(vec![ALPN.to_vec()]);
    let addr = accepter.addr();

    let dialer = Endpoint::bind(presets::N0).await.expect("bind dialer");
    let accept = tokio::spawn({
        let accepter = accepter.clone();
        async move {
            let incoming = accepter.accept().await.expect("incoming");
            incoming.await.expect("accepted connection")
        }
    });
    let conn = dialer.connect(addr, ALPN).await.expect("connect");
    let server = accept.await.expect("accept task");
    (accepter, dialer, conn, server)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds real UDP sockets; run explicitly for S0 measurement"]
async fn record_transport_capabilities() {
    let (accepter, dialer, conn, server) = connected().await;

    // Drain the receiving side. Without a reader the peer's concurrent-uni
    // allowance runs out and `open_uni` blocks, which would measure flow
    // control rather than the cost of opening a stream.
    let drain = tokio::spawn(async move {
        while let Ok(mut recv) = server.accept_uni().await {
            let _ = recv.read_to_end(64).await;
        }
    });

    println!("\n--- observed (iroh 1.0.0-rc.1) ---");

    // Datagram capacity. Path-dependent, which is exactly why lait's own
    // ceiling is advisory and checked at send time rather than assumed.
    println!(
        "max_datagram_size            {:?}",
        conn.max_datagram_size()
    );
    println!(
        "datagram_send_buffer_space   {}",
        conn.datagram_send_buffer_space()
    );

    // Stream-open cost. The MoQ pattern — one short stream per unit of work —
    // only makes sense if opening one is genuinely cheap.
    let started = Instant::now();
    let opens = 64;
    for _ in 0..opens {
        let mut send = conn.open_uni().await.expect("open_uni");
        send.write_all(b"x").await.expect("write");
        send.finish().expect("finish");
    }
    let per_open = started.elapsed() / opens;
    println!("open_uni + write + finish    {per_open:?} each over {opens}");

    // Reset and stop, the two abort directions.
    let mut send = conn.open_uni().await.expect("open_uni");
    send.write_all(b"doomed").await.expect("write");
    let reset = send.reset(7u32.into());
    println!("SendStream::reset            {reset:?}");

    println!("--- end ---\n");

    conn.close(0u32.into(), b"measured");
    drain.abort();
    dialer.close().await;
    accepter.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds real UDP sockets; run explicitly for S0 measurement"]
async fn a_datagram_larger_than_the_path_allows_is_refused_not_truncated() {
    // The property lait's send path depends on. If an oversized datagram were
    // silently truncated, a cursor payload would arrive as a corrupt one rather
    // than not at all — and transient state has no retransmit to fix it.
    let (accepter, dialer, conn, _server) = connected().await;
    let max = conn.max_datagram_size().unwrap_or(1_200);
    let oversized = vec![0u8; max + 64];
    let outcome = conn.send_datagram(oversized.into());
    assert!(
        outcome.is_err(),
        "an oversized datagram must be refused rather than truncated"
    );

    // And one that fits is accepted.
    let ok = conn.send_datagram(vec![0u8; max.min(512)].into());
    assert!(ok.is_ok());

    conn.close(0u32.into(), b"done");
    dialer.close().await;
    accepter.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds real UDP sockets; run explicitly for S0 measurement"]
async fn a_reset_stream_surfaces_its_code_rather_than_a_short_read() {
    // What lets a receiver tell "the sender gave up on this" apart from "the
    // sender finished". Without it, an abandoned transfer looks like a
    // truncated one, and truncation is supposed to be loud.
    let accepter = Endpoint::bind(presets::N0).await.expect("bind");
    accepter.set_alpns(vec![ALPN.to_vec()]);
    let addr = accepter.addr();
    let dialer = Endpoint::bind(presets::N0).await.expect("bind");

    let server = tokio::spawn({
        let accepter = accepter.clone();
        async move {
            let conn = accepter
                .accept()
                .await
                .expect("incoming")
                .await
                .expect("connection");
            let mut recv = conn.accept_uni().await.expect("accept_uni");
            let outcome = recv.read_to_end(1024).await;
            outcome.is_err()
        }
    });

    let conn = dialer.connect(addr, ALPN).await.expect("connect");
    let mut send = conn.open_uni().await.expect("open_uni");
    send.write_all(b"partial").await.expect("write");
    let _ = send.reset(9u32.into());
    tokio::time::sleep(Duration::from_millis(200)).await;

    let saw_error = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server task")
        .expect("join");
    assert!(
        saw_error,
        "a reset must surface as an error, not a clean end"
    );

    conn.close(0u32.into(), b"done");
    dialer.close().await;
    accepter.close().await;
}

#[test]
fn the_upgrade_posture_is_written_down() {
    // Which frozen numbers are ours and which are observations of a release
    // candidate. This is a documentation gate, not a behaviour test: if the
    // pin moves and nobody re-measures, the observed column is stale and this
    // list is what says so.
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/PROTOCOL.md"),
    )
    .expect("PROTOCOL.md");
    for required in [
        "MAX_FLOW_READ_BYTES",
        "MAX_DATAGRAM_BYTES",
        "max_datagram_size",
        "1.0.0-rc.1",
    ] {
        assert!(
            doc.contains(required),
            "PROTOCOL.md must record `{required}` in the transport posture"
        );
    }
}
