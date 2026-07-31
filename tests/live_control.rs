//! The Live and Signals control requests, served over the real control socket.
//!
//! This exists for one reason the compiler cannot cover. Classifying a request
//! is compile-enforced — `control::classify` has no wildcard arm — but
//! *handling* it is not: every `dispatch_*` in the SpaceBridge ends in
//! `unreachable!("misclassified …")`, so a variant that is classified and never
//! dispatched panics the connection task the first time anybody sends it. The
//! client sees a closed socket and no reason. Both new verbs are answered here,
//! over the same IPC channel the CLI, `lait serve`, and MCP use.
//!
//! A lone node with no Live sessions is the interesting case rather than a
//! degenerate one: an empty transient table is the truth about a node nobody is
//! connected to, and it must arrive as an empty answer rather than as an error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use lait::control::{request, Request, Response};
use lait::net::Network;
use lait::orbital::run_space_bridge;
use lait::transport::mem::MemNet;
use lait::transport::{Transport, TransportFactory};

const FOUNDER_SEED: [u8; 32] = [113u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct MemFactory(MemNet);

#[async_trait]
impl TransportFactory for MemFactory {
    async fn build(
        &self,
        identity_seed: &[u8; 32],
        _network: &Network,
        _protocols: comms::Protocols<'_>,
    ) -> Result<Arc<dyn Transport>> {
        Ok(Arc::new(
            self.0.peer(lait::crypto::device_from_seed(identity_seed)),
        ))
    }
}

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-live-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_identity(home: &Path, seed: &[u8; 32]) {
    std::env::set_var("LAIT_HOME", home);
    std::fs::write(
        home.join("secret.key"),
        data_encoding::HEXLOWER.encode(seed),
    )
    .unwrap();
}

fn req(rt: &tokio::runtime::Runtime, home: &Path, r: Request) -> Response {
    rt.block_on(async { request(home, &r).await })
        .unwrap_or_else(|e| Response::err(format!("{e:#}")))
}

fn poll_until<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = check() {
            return Some(v);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn the_live_view_and_the_signal_drain_are_served_rather_than_unreachable() {
    let home = temp_home();
    let net = MemNet::new();
    std::fs::create_dir_all(&home).unwrap();
    write_identity(&home, &FOUNDER_SEED);
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Live Space").unwrap();

    let daemon_home = home.clone();
    let daemon_net = net.clone();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            if let Err(e) = run_space_bridge(daemon_home, &MemFactory(daemon_net)).await {
                eprintln!("DAEMON ERR: {e:#}");
            }
        });
    });

    let client = tokio::runtime::Runtime::new().unwrap();
    let online = poll_until(Duration::from_secs(20), || {
        matches!(req(&client, &home, Request::Status), Response::Status(_)).then_some(())
    });
    assert!(online.is_some(), "the SpaceBridge never answered Status");

    // A first read carries no generation, so it always gets the table — even
    // when the table is empty and the daemon's counter is still at zero.
    let first = req(
        &client,
        &home,
        Request::Live {
            since_generation: None,
            issue: None,
        },
    );
    let Response::Live {
        generation,
        partial,
        entries,
    } = first
    else {
        panic!("expected a live view, got {first:?}");
    };
    assert!(entries.is_empty(), "nobody is connected: {entries:?}");
    assert!(
        !partial,
        "a node at no ceiling is hearing everything it can"
    );

    // Holding that generation buys the cheap answer.
    let again = req(
        &client,
        &home,
        Request::Live {
            since_generation: Some(generation),
            issue: None,
        },
    );
    assert!(
        matches!(again, Response::LiveUnchanged { generation: g } if g == generation),
        "expected the unchanged reply, got {again:?}"
    );

    // Naming an issue narrows the scope. An issue nobody is looking at — or one
    // that does not exist — is an empty answer, not a refusal: the Live plane
    // has no opinion about which Bodies exist, and answering "not found" here
    // would let anyone probe for them by guessing doc ids.
    let scoped = req(
        &client,
        &home,
        Request::Live {
            since_generation: None,
            issue: Some("iss_01jz0000000000000000000000".into()),
        },
    );
    assert!(
        matches!(&scoped, Response::Live { entries, .. } if entries.is_empty()),
        "expected an empty scoped view, got {scoped:?}"
    );

    // The drain answers on a node nobody has signalled, and answers that
    // nothing was lost.
    let drained = req(&client, &home, Request::Signals);
    let Response::Signals { signals, dropped } = drained else {
        panic!("expected a signal drain, got {drained:?}");
    };
    assert!(signals.is_empty(), "nothing was sent: {signals:?}");
    assert_eq!(dropped, 0, "an empty queue has dropped nothing");

    let _ = req(&client, &home, Request::Stop);
    let _ = handle.join();
}
