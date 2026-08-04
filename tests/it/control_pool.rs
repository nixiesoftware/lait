//! The control channel's connection reuse, from the client's side.
//!
//! A head used to pay a fresh connect for every request it carried — a board
//! read, a status, a member list, each one a connect. The daemon serves many
//! requests per connection now, and these tests pin the two halves of that
//! bargain: the client stops reconnecting when it does not have to, and it
//! still reconnects when it does.
//!
//! Both run against a hand-written listener rather than a real daemon. What is
//! under test is framing and connection lifetime, and a real daemon would bring
//! a store, an identity, and a placement to a question that involves none of
//! them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use interprocess::local_socket::{tokio::prelude::*, ListenerOptions};
use lait::control::{self, ClientRequest, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn home(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lait-pool-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create test home");
    dir
}

/// One `ok` response, exactly as the daemon writes it.
fn ok_line() -> String {
    let mut line = serde_json::to_string(&Response::Ok { message: None }).expect("encode response");
    line.push('\n');
    line
}

/// A listener that answers `requests_per_connection` requests and then hangs
/// up, counting the connections it accepted.
///
/// `usize::MAX` is the daemon's real behaviour: serve until the client goes.
fn fake_daemon(
    home: &std::path::Path,
    requests_per_connection: usize,
) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let name = control::control_name(home).expect("control name");
    #[cfg(unix)]
    let _ = std::fs::remove_file(lait::config::socket_path(home));
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .expect("bind fake daemon");
    let accepts = Arc::new(AtomicUsize::new(0));
    let counter = accepts.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok(stream) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut reader = BufReader::new(read_half);
            for _ in 0..requests_per_connection {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if write_half.write_all(ok_line().as_bytes()).await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            }
        }
    });
    (accepts, task)
}

/// The reason the pool exists: three requests, one connection.
#[tokio::test]
async fn one_connection_carries_many_requests() {
    let home = home("reuse");
    let (accepts, listener) = fake_daemon(&home, usize::MAX);

    for _ in 0..3 {
        let response = control::send(&home, &ClientRequest::plain(Request::Id))
            .await
            .expect("send");
        assert!(matches!(response, Response::Ok { .. }));
    }

    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "three requests should have shared one connection"
    );
    listener.abort();
}

/// The other half of the bargain, and the one that matters for an upgrade: a
/// daemon that hangs up after every request is still served correctly, because
/// a parked connection that turns out to be dead is re-opened rather than
/// reported.
///
/// This is exactly what a daemon from before this change looks like from here.
#[tokio::test]
async fn a_daemon_that_hangs_up_is_reconnected_to() {
    let home = home("hangup");
    let (accepts, listener) = fake_daemon(&home, 1);

    for _ in 0..3 {
        let response = control::send(&home, &ClientRequest::plain(Request::Id))
            .await
            .expect("a hung-up connection must not surface as a failure");
        assert!(matches!(response, Response::Ok { .. }));
    }

    assert_eq!(
        accepts.load(Ordering::SeqCst),
        3,
        "each request should have opened its own connection"
    );
    listener.abort();
}

/// The two timers must not race.
///
/// The client's licence to re-send a request rests entirely on "a connection I
/// parked and the daemon then closed never carried it". That reasoning holds
/// only while the client gives up on a parked connection well before the daemon
/// reaps it — so the margin between the two is load-bearing, and closing it is
/// the kind of edit that looks harmless.
#[test]
fn the_client_stops_reusing_long_before_the_daemon_hangs_up() {
    assert!(
        control::MAX_IDLE_AGE < control::IDLE_CONNECTION_TIMEOUT,
        "a client must not offer a connection the daemon may already have reaped"
    );
    assert!(
        control::IDLE_CONNECTION_TIMEOUT >= control::MAX_IDLE_AGE * 2,
        "the gap between the two should be a margin, not a coincidence"
    );
}
