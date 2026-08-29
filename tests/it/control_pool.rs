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

/// A throwaway root that removes itself — see [`crate::head::temp_root`],
/// which is the one place that knows how.
fn home(tag: &str) -> crate::head::TempRoot {
    crate::head::temp_root(&format!("pool-{tag}"))
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

/// A World call carries its payload behind the header, and that second write is
/// where a reaped connection actually shows itself: the first write into a
/// closed pipe can succeed, so the failure surfaces on the payload rather than
/// on the header.
///
/// This is a regression test with a specific history. Treating that failure as
/// delivered — on the reasoning that the header was already sent — turned every
/// reaped connection into a failed request under load, intermittently, in
/// whichever test happened to be running. What makes it undelivered is the
/// receiver, not the ordering: a World call is dispatched only after its
/// declared bytes are read in full, so a header that arrives without its
/// payload runs nothing.
#[tokio::test]
async fn a_framed_call_survives_a_reaped_connection() {
    let home = home("framed");
    let (accepts, listener) = framed_fake_daemon(&home, 1);

    for _ in 0..3 {
        let call = runtime::world::call::Call::new(
            replica::body::WorldId::parse("com.example.notes").expect("world id"),
            "read",
            1,
            b"{}".to_vec(),
        )
        .expect("build call");
        let reply = control::call_world(
            &home,
            control::ControlRoute::World {
                address: control::OrbitAddress::for_store(
                    &home,
                    mechanics::ids::SpaceId::from_digest([9u8; 16]),
                ),
                world: "com.example.notes".into(),
            },
            call,
            None,
        )
        .await
        .expect("a reaped connection must not surface as a failed World call");
        assert_eq!(reply.into_result().expect("payload"), b"pong".to_vec());
    }

    assert_eq!(accepts.load(Ordering::SeqCst), 3);
    listener.abort();
}

/// A listener that speaks the framed World-call protocol: read the header, read
/// exactly the bytes it declared, answer in the same shape.
fn framed_fake_daemon(
    home: &std::path::Path,
    requests_per_connection: usize,
) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncReadExt;

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
                let mut header = String::new();
                match reader.read_line(&mut header).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(frame) = serde_json::from_str::<control::WorldCallFrame>(header.trim())
                else {
                    break;
                };
                let mut payload = vec![0u8; frame.call.len as usize];
                if reader.read_exact(&mut payload).await.is_err() {
                    break;
                }
                let body = b"pong";
                let reply = control::ReplyFrame {
                    world: frame.call.world.clone(),
                    operation: frame.call.operation.clone(),
                    version: frame.call.version,
                    outcome: control::ReplyFrameOutcome::Ok {
                        len: body.len() as u64,
                    },
                };
                let mut line = serde_json::to_string(&reply).expect("encode reply");
                line.push('\n');
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if write_half.write_all(body).await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            }
        }
    });
    (accepts, task)
}

/// A daemon that takes the request and then goes away without answering.
///
/// This is the shape a restart has from the client's side, and it does not
/// announce itself the same way twice: a Unix socket still holding bytes we sent
/// reports `ECONNRESET` on the read, a Windows pipe reports the write, and a
/// clean close reports end-of-file. All three mean the same thing — nothing was
/// answered, and since a request is dispatched only after being read in full,
/// nothing ran.
///
/// Keying the re-send rule on end-of-file alone passed on Windows and macOS and
/// failed on Linux, in a test that restarts its daemon. So this asserts the
/// property rather than the spelling.
#[tokio::test]
async fn a_daemon_that_takes_the_request_and_vanishes_is_retried() {
    let home = home("vanish");
    let name = control::control_name(&home).expect("control name");
    #[cfg(unix)]
    let _ = std::fs::remove_file(lait::config::socket_path(&home));
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
            let seen = counter.fetch_add(1, Ordering::SeqCst);
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            if reader.read_line(&mut line).await.is_err() {
                continue;
            }
            // The first connection answers once and is then parked by the
            // client. On its next request it reads the bytes and drops the
            // stream — the daemon went away mid-flight.
            if seen == 0 {
                let _ = write_half.write_all(ok_line().as_bytes()).await;
                let _ = write_half.flush().await;
                let mut swallowed = String::new();
                let _ = reader.read_line(&mut swallowed).await;
                continue;
            }
            let _ = write_half.write_all(ok_line().as_bytes()).await;
            let _ = write_half.flush().await;
        }
    });

    control::send(&home, &ClientRequest::plain(Request::Id))
        .await
        .expect("first send");
    let response = control::send(&home, &ClientRequest::plain(Request::Id))
        .await
        .expect("a daemon that vanished mid-request must be reconnected to, not reported");
    assert!(matches!(response, Response::Ok { .. }));
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
    task.abort();
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
