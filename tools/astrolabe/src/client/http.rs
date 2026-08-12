//! One JSON request to a head on loopback.
//!
//! A head is the only party that can mint a launch credential — redemption has
//! to *consume*, so the store lives in the process that will be presented with
//! the ticket. That store is reachable over HTTP and nothing else, which is why
//! there is a socket in a program that otherwise has no boundaries in it.
//!
//! It is hand-written rather than a client crate, and the reason is the same one
//! that keeps `sidecar`'s version comparison hand-written: the whole job is one
//! POST to `127.0.0.1`, with a bearer token, expecting JSON back. A general HTTP
//! client would bring redirects, proxies, TLS, connection pooling, cookie jars
//! and a transitive closure to audit and to carry in the third-party notices —
//! for a request that never leaves the machine and never varies in shape.
//!
//! `Connection: close` is what makes the reply unambiguous: the body ends when
//! the socket does, so nothing here has to agree with the server about framing.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{ClientError, ClientResult};

/// How long a loopback request may take before it is reported rather than
/// waited on.
///
/// Generous for a process on the same machine, and short enough that a wedged
/// head is a message on screen rather than a client that has stopped answering.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A head, addressed.
///
/// Carried as a pair rather than as one URL because every call needs both, and
/// re-parsing the token out of a URL at each use is how a credential ends up in
/// a log line that was only meant to say where the head is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// `http://127.0.0.1:PORT`, with no trailing slash.
    pub base: String,
    /// The run credential for this head. Every `/api` route wants it.
    pub token: String,
}

impl Head {
    /// Split a readiness URL — `http://127.0.0.1:PORT/?token=…` — into the two
    /// halves a caller actually uses.
    ///
    /// A head that announced no token is refused rather than addressed without
    /// one: every route this client needs is behind the token, so a `Head` with
    /// an empty one is a value whose every use fails later and further away.
    pub fn from_ready_url(url: &str) -> ClientResult<Self> {
        let (base, query) = url
            .split_once('?')
            .ok_or_else(|| ClientError::internal(format!("head announced no credential: {url}")))?;
        let token = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
            .filter(|token| !token.is_empty())
            .ok_or_else(|| ClientError::internal(format!("head announced no credential: {url}")))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
        })
    }

    /// `127.0.0.1:PORT`, which is what a socket needs.
    fn authority(&self) -> ClientResult<&str> {
        self.base.strip_prefix("http://").ok_or_else(|| {
            ClientError::internal(format!("a head is not on loopback: {}", self.base))
        })
    }
}

/// POST `body` as JSON to `path` on `head`, and decode the JSON reply.
pub async fn post_json(
    head: &Head,
    path: &str,
    body: &serde_json::Value,
) -> ClientResult<serde_json::Value> {
    let authority = head.authority()?;
    let encoded = serde_json::to_vec(body)
        .map_err(|error| ClientError::internal(format!("encode request: {error}")))?;
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {length}\r\n\
         Connection: close\r\n\
         \r\n",
        token = head.token,
        length = encoded.len(),
    );

    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(authority)
            .await
            .map_err(|error| {
                ClientError::unreachable(format!("reach the head at {authority}: {error}"))
            })?;
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| ClientError::unreachable(format!("write to the head: {error}")))?;
        stream
            .write_all(&encoded)
            .await
            .map_err(|error| ClientError::unreachable(format!("write to the head: {error}")))?;
        let mut raw = Vec::new();
        // To end of stream. `Connection: close` is what makes that the whole
        // reply and not a guess about framing.
        stream
            .read_to_end(&mut raw)
            .await
            .map_err(|error| ClientError::unreachable(format!("read from the head: {error}")))?;
        Ok::<_, ClientError>(raw)
    };

    let raw = tokio::time::timeout(TIMEOUT, exchange)
        .await
        .map_err(|_| {
            ClientError::unreachable(format!("the head at {authority} did not answer"))
        })??;
    decode(&raw)
}

/// Status line, headers, body — and what each of them means for the caller.
fn decode(raw: &[u8]) -> ClientResult<serde_json::Value> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| ClientError::internal("the head's reply had no header block"))?;
    let (head, rest) = raw.split_at(split);
    let body = rest.get(4..).unwrap_or_default();
    let head = String::from_utf8_lossy(head);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| ClientError::internal("the head's reply had no status"))?;

    if !(200..300).contains(&status) {
        let message = String::from_utf8_lossy(body).trim().to_owned();
        let message = if message.is_empty() {
            format!("the head refused this request ({status})")
        } else {
            message
        };
        // A refusal is the head's answer and asking again changes nothing; a
        // 5xx is the one class where the same request might work next time.
        return Err(if status >= 500 {
            ClientError::unreachable(message)
        } else {
            ClientError::refused(message)
        });
    }

    serde_json::from_slice(body).map_err(|error| {
        ClientError::internal(format!("the head answered something unreadable: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The readiness line is where a client learns both halves. Losing the
    /// token half would leave every later call to be refused by the head with
    /// an error that names authorisation rather than the parse that dropped it.
    #[test]
    fn a_readiness_url_splits_into_an_address_and_a_credential() {
        let head = Head::from_ready_url("http://127.0.0.1:7717/?token=abc123").expect("a head");
        assert_eq!(head.base, "http://127.0.0.1:7717");
        assert_eq!(head.token, "abc123");

        for wrong in [
            "http://127.0.0.1:7717/",
            "http://127.0.0.1:7717/?token=",
            "http://127.0.0.1:7717/?other=1",
        ] {
            assert!(
                Head::from_ready_url(wrong).is_err(),
                "'{wrong}' was accepted as an addressable head"
            );
        }
    }

    /// A refusal and a hiccup are different answers, and the surface offers a
    /// retry for exactly one of them.
    #[test]
    fn a_refusal_is_not_retryable_and_a_server_fault_is() {
        let refused = decode(b"HTTP/1.1 401 Unauthorized\r\n\r\nthis link has been used already")
            .expect_err("a 401 was accepted");
        assert!(!refused.retryable);
        assert!(refused.message.contains("used already"), "{refused}");

        let faulted = decode(b"HTTP/1.1 500 Internal Server Error\r\n\r\nnope")
            .expect_err("a 500 was accepted");
        assert!(faulted.retryable, "a server fault was reported as final");
    }

    #[test]
    fn a_json_body_survives_the_header_block() {
        let decoded =
            decode(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ticket\":\"t\"}")
                .expect("a body");
        assert_eq!(decoded["ticket"], "t");
    }

    /// Two round trips against a real socket: the request this module writes is
    /// well formed enough for a server to read, and the reply is decoded from
    /// the bytes rather than from an assumption about framing.
    #[tokio::test]
    async fn a_request_and_its_reply_survive_a_real_socket() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let served = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut seen = Vec::new();
            let mut buffer = [0u8; 1024];
            // Read until the declared body has arrived. The request is small
            // enough to be one read in practice; the loop is what makes the
            // test independent of that.
            loop {
                let read = stream.read(&mut buffer).await.expect("read");
                if read == 0 {
                    break;
                }
                seen.extend_from_slice(buffer.get(..read).unwrap_or_default());
                if seen.windows(4).any(|window| window == b"\r\n\r\n") && seen.ends_with(b"}") {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"ticket\":\"t\"}")
                .await
                .expect("write");
            stream.shutdown().await.ok();
            String::from_utf8_lossy(&seen).into_owned()
        });

        let head = Head {
            base: format!("http://{address}"),
            token: "secret".into(),
        };
        let reply = post_json(
            &head,
            "/api/launch",
            &serde_json::json!({ "orbit": "orb_one" }),
        )
        .await
        .expect("a reply");
        assert_eq!(reply["ticket"], "t");

        let request = served.await.expect("the server task");
        assert!(
            request.contains("Authorization: Bearer secret"),
            "the credential did not travel: {request}"
        );
        assert!(
            request.contains("POST /api/launch HTTP/1.1"),
            "the request line is wrong: {request}"
        );
        assert!(
            request.contains(r#"{"orbit":"orb_one"}"#),
            "the body did not travel: {request}"
        );
    }
}
