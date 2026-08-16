//! The Post over a real socket.
//!
//! The library tests prove the carrier's rules. This proves the thing an
//! operator actually starts: a bound port, JSON on the wire, and a status code
//! per refusal. A routing table and a status mapping are exactly the sort of
//! code that is obviously right and quietly wrong — a refusal returned as 200,
//! a handler wired to the wrong path — and none of that is visible from the
//! inside.

use std::sync::{Arc, Mutex};

use lait_post::http::{router, Shared};
use lait_post::{Challenge, Deposited, Envelope, FsStore, Post, SignedAck, SignedDeposit};
use mechanics::actor::{device_from_seed, sign_detached};

const SENDER_SEED: [u8; 32] = [41u8; 32];
const RECIPIENT_SEED: [u8; 32] = [42u8; 32];
const STRANGER_SEED: [u8; 32] = [43u8; 32];

/// Start the real service on an ephemeral port and answer with its base URL.
async fn serve() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a deposit root");
    let store = FsStore::open(dir.path()).expect("open");
    let shared: Shared = Arc::new(Mutex::new(Post::new(store)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(shared)).await;
    });
    (format!("http://127.0.0.1:{port}"), dir)
}

async fn get_json(url: &str) -> (u16, String) {
    let response = reqwest_lite::get(url).await;
    response
}

/// The smallest HTTP client that can drive this, so the test does not pull a
/// client stack in for four requests.
mod reqwest_lite {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn round_trip(url: &str, method: &str, body: Option<&str>) -> (u16, String) {
        let rest = url.strip_prefix("http://").expect("http url");
        let (authority, path) = rest
            .split_once('/')
            .map(|(a, p)| (a, format!("/{p}")))
            .expect("path");
        let mut socket = tokio::net::TcpStream::connect(authority)
            .await
            .expect("connect");
        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
        if let Some(body) = body {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        if let Some(body) = body {
            request.push_str(body);
        }
        socket.write_all(request.as_bytes()).await.expect("write");
        let mut raw = Vec::new();
        socket.read_to_end(&mut raw).await.expect("read");
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, payload) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status: u16 = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("a status line");
        // `Connection: close` means no chunked encoding to unpick.
        (status, payload.to_string())
    }

    pub async fn get(url: &str) -> (u16, String) {
        round_trip(url, "GET", None).await
    }

    pub async fn post(url: &str, body: &str) -> (u16, String) {
        round_trip(url, "POST", Some(body)).await
    }
}

#[tokio::test]
async fn a_letter_crosses_the_wire_and_only_its_recipient_collects_it() {
    let (base, _dir) = serve().await;
    let sender = device_from_seed(&SENDER_SEED);
    let recipient = device_from_seed(&RECIPIENT_SEED);

    let (status, body) = get_json(&format!("{base}/health")).await;
    assert_eq!((status, body.as_str()), (200, "ok"));

    // Deposit.
    let envelope = Envelope {
        recipient: recipient.clone(),
        sealed: b"a sealed invitation".to_vec(),
        expires_at: now() + 600,
        envelope_version: 1,
    };
    let deposit = SignedDeposit {
        signature: sign_detached(&SENDER_SEED, &preimage_deposit(&sender, &envelope)),
        sender: sender.clone(),
        envelope,
    };
    let (status, body) = reqwest_lite::post(
        &format!("{base}/deposit"),
        &serde_json::to_string(&deposit).expect("json"),
    )
    .await;
    assert_eq!(status, 200, "deposit refused: {body}");

    // A stranger answering the recipient's challenge with their own key gets a
    // 403 and no mail — the one property the whole design exists for.
    let (_, raw) = get_json(&format!("{base}/challenge?device={}", recipient.as_str())).await;
    let challenge: Challenge = serde_json::from_str(&raw).expect("a challenge");
    let forged = serde_json::json!({
        "device": recipient.as_str(),
        "nonce": data_encoding::HEXLOWER.encode(&challenge.nonce),
        "signature": data_encoding::HEXLOWER.encode(
            &sign_detached(&STRANGER_SEED, &preimage_fetch(&challenge))),
    });
    let (status, body) = reqwest_lite::post(&format!("{base}/fetch"), &forged.to_string()).await;
    assert_eq!(status, 403, "a forged fetch must be refused, got {body}");
    assert!(
        body.contains("bad_signature"),
        "and the refusal keeps its name: {body}"
    );

    // The recipient collects.
    let (_, raw) = get_json(&format!("{base}/challenge?device={}", recipient.as_str())).await;
    let challenge: Challenge = serde_json::from_str(&raw).expect("a challenge");
    let answered = serde_json::json!({
        "device": recipient.as_str(),
        "nonce": data_encoding::HEXLOWER.encode(&challenge.nonce),
        "signature": data_encoding::HEXLOWER.encode(
            &sign_detached(&RECIPIENT_SEED, &preimage_fetch(&challenge))),
    });
    let (status, body) = reqwest_lite::post(&format!("{base}/fetch"), &answered.to_string()).await;
    assert_eq!(status, 200, "fetch refused: {body}");
    let waiting: Vec<Deposited> = serde_json::from_str(&body).expect("deposits");
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].envelope.sealed, b"a sealed invitation");

    // And acknowledges, which empties the mailbox.
    let (_, raw) = get_json(&format!("{base}/challenge?device={}", recipient.as_str())).await;
    let challenge: Challenge = serde_json::from_str(&raw).expect("a challenge");
    let ack = SignedAck {
        device: recipient.clone(),
        nonce: challenge.nonce,
        deposits: vec![waiting[0].id.clone()],
        signature: [0u8; 64],
    };
    let ack = SignedAck {
        signature: sign_detached(&RECIPIENT_SEED, &preimage_ack(&ack)),
        ..ack
    };
    let (status, body) = reqwest_lite::post(
        &format!("{base}/acknowledge"),
        &serde_json::to_string(&ack).expect("json"),
    )
    .await;
    assert_eq!(status, 200, "ack refused: {body}");
    assert!(body.contains("\"dropped\":1"), "{body}");

    let (_, raw) = get_json(&format!("{base}/challenge?device={}", recipient.as_str())).await;
    let challenge: Challenge = serde_json::from_str(&raw).expect("a challenge");
    let answered = serde_json::json!({
        "device": recipient.as_str(),
        "nonce": data_encoding::HEXLOWER.encode(&challenge.nonce),
        "signature": data_encoding::HEXLOWER.encode(
            &sign_detached(&RECIPIENT_SEED, &preimage_fetch(&challenge))),
    });
    let (_, body) = reqwest_lite::post(&format!("{base}/fetch"), &answered.to_string()).await;
    let left: Vec<Deposited> = serde_json::from_str(&body).expect("deposits");
    assert!(left.is_empty(), "what was acknowledged is gone");
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// The preimages, rebuilt here on purpose. A client that reached into the
// service for them would agree with it by construction and prove nothing about
// the format being writable from outside.
fn framed(out: &mut Vec<u8>, part: &[u8]) {
    out.extend_from_slice(&(part.len() as u32).to_be_bytes());
    out.extend_from_slice(part);
}

fn preimage_deposit(sender: &mechanics::ids::DeviceId, envelope: &Envelope) -> Vec<u8> {
    let mut out = Vec::new();
    framed(&mut out, b"lait/post/1/deposit");
    framed(&mut out, sender.as_str().as_bytes());
    framed(&mut out, envelope.recipient.as_str().as_bytes());
    framed(&mut out, &envelope.expires_at.to_be_bytes());
    framed(&mut out, &envelope.envelope_version.to_be_bytes());
    framed(&mut out, &envelope.sealed);
    out
}

fn preimage_fetch(challenge: &Challenge) -> Vec<u8> {
    let mut out = Vec::new();
    framed(&mut out, b"lait/post/1/fetch");
    framed(&mut out, challenge.device.as_str().as_bytes());
    framed(&mut out, &challenge.nonce);
    out
}

fn preimage_ack(ack: &SignedAck) -> Vec<u8> {
    let mut out = Vec::new();
    framed(&mut out, b"lait/post/1/acknowledge");
    framed(&mut out, ack.device.as_str().as_bytes());
    framed(&mut out, &ack.nonce);
    out.extend_from_slice(&(ack.deposits.len() as u32).to_be_bytes());
    for id in &ack.deposits {
        framed(&mut out, id.as_bytes());
    }
    out
}
