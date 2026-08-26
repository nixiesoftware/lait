//! The Live and Signals control requests, served over the real control socket.
//!
//! This exists for one reason the compiler cannot cover. Classifying a request
//! is compile-enforced — `control::classify` has no wildcard arm — but
//! *handling* it is not: every `dispatch_*` in the StationHost ends in
//! `unreachable!("misclassified …")`, so a variant that is classified and never
//! dispatched panics the connection task the first time anybody sends it. The
//! client sees a closed socket and no reason. Both are answered here, over the
//! same IPC channel the local app and MCP heads use.
//!
//! A lone node with no Live sessions is the interesting case rather than a
//! degenerate one: an empty transient table is the truth about a node nobody is
//! connected to, and it must arrive as an empty answer rather than as an error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::world_fixture::run_station_process;
use anyhow::Result;
use async_trait::async_trait;
use comms::mem::MemNet;
use comms::policy::Network;
use comms::{Transport, TransportFactory};
use lait::control::{request, Request, Response};

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
            self.0
                .peer(mechanics::actor::device_from_seed(identity_seed)),
        ))
    }
}

/// A throwaway root that removes itself — see [`crate::head::temp_root`],
/// which is the one place that knows how.
fn temp_home() -> crate::head::TempRoot {
    crate::head::temp_root("live")
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
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Live Space").unwrap();

    let daemon_home = home.to_path_buf();
    let daemon_net = net.clone();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            if let Err(e) = run_station_process(daemon_home, &MemFactory(daemon_net)).await {
                eprintln!("DAEMON ERR: {e:#}");
            }
        });
    });

    let client = tokio::runtime::Runtime::new().unwrap();
    let online = poll_until(Duration::from_secs(20), || {
        matches!(req(&client, &home, Request::Status), Response::Status(_)).then_some(())
    });
    assert!(online.is_some(), "the StationHost never answered Status");

    // A first read carries no generation, so it always gets the table — even
    // when the table is empty and the daemon's counter is still at zero.
    let first = req(
        &client,
        &home,
        Request::Live {
            world: issues::PRODUCT_WORLD.into(),
            since_generation: None,
            body: None,
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
            world: issues::PRODUCT_WORLD.into(),
            since_generation: Some(generation),
            body: None,
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
            world: issues::PRODUCT_WORLD.into(),
            since_generation: None,
            body: Some(
                issues::contract::issue_body_id("iss_01jz0000000000000000000000").as_bytes(),
            ),
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

    // Declaring what this node is looking at. The send side's entry point, and
    // the reason a facepile on somebody else's screen has anything to draw.
    //
    // **What these three assertions prove is narrow, and worth stating exactly.**
    // `watching` ends in an unconditional `Ok`, so `Ok` does not report that a
    // declaration was understood — it reports that the verb was *routed*: decoded
    // from the wire, classified to the Station owner, and dispatched to a handler
    // rather than falling through to an unknown-variant error or a misclassified
    // `unreachable!`. That is this file's subject and it is a real thing to break.
    //
    // They do not prove what the declaration did. The scope truncation is
    // asserted where it is decided, and that a peer sees the result is
    // `crates/runtime/tests/live_transient.rs::two_node_presence`, which is
    // mutation-checked against a stubbed publisher.
    let watching = req(
        &client,
        &home,
        Request::Watching {
            world: issues::PRODUCT_WORLD.into(),
            bodies: vec![
                issues::contract::issue_body_id("iss_01jz0000000000000000000000").as_bytes(),
            ],
            carets: Vec::new(),
            typing: Vec::new(),
            previews: Vec::new(),
        },
    );
    assert!(
        matches!(watching, Response::Ok { .. }),
        "expected the declaration to be served, got {watching:?}"
    );

    // A string this node cannot resolve is accepted rather than refused, and
    // "resolve" is the wrong word for what happens: the Body id is a hash of the
    // string as given, so every string is a legal input and a stale one names a
    // Body nothing publishes under. Refusing would need a lookup this verb does
    // not do — and a lookup that answered would let anyone probe for which
    // issues exist by watching them.
    let nonsense = req(
        &client,
        &home,
        Request::Watching {
            world: issues::PRODUCT_WORLD.into(),
            bodies: vec![[1; 16], [2; 16]],
            carets: Vec::new(),
            typing: Vec::new(),
            previews: Vec::new(),
        },
    );
    assert!(
        matches!(nonsense, Response::Ok { .. }),
        "expected unresolvable ids to be dropped, got {nonsense:?}"
    );

    // The empty declaration, which is how presence stops. Not a no-op and not an
    // error: a node looking at nothing is a real state — every tab closed — and
    // it has to be expressible, or presence could only ever grow.
    let nothing = req(
        &client,
        &home,
        Request::Watching {
            world: issues::PRODUCT_WORLD.into(),
            bodies: Vec::new(),
            carets: Vec::new(),
            typing: Vec::new(),
            previews: Vec::new(),
        },
    );
    assert!(
        matches!(nothing, Response::Ok { .. }),
        "expected an empty declaration to be served, got {nothing:?}"
    );

    let _ = req(&client, &home, Request::Stop);
    let _ = handle.join();
}
