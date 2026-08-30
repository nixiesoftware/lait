//! The subscription: hearing a publish the moment it happens.
//!
//! [`super::watch`] polls the channel on a period, and that period is the
//! floor — a machine with no route to anything but the bucket still updates.
//! This module is what makes the period irrelevant in the ordinary case. The
//! daemon holds one server-sent-events subscription to the notify relay
//! (`tools/feed-notify`), and when a pointer arrives under a key this machine
//! follows, it wakes the watcher, which then does exactly what it does on its
//! period: resolve the channel against the bucket, ratchet the publish time,
//! verify the manifest, stage the bytes.
//!
//! ## What a subscription may and may not do
//!
//! It shortens time-to-know and nothing else. A frame is opened with the same
//! pinned keys the feed is, so the relay cannot make this machine believe a
//! pointer the publisher never signed; an old pointer, or one already acted
//! on, does not wake anything; and nothing heard here is ever *used* — the
//! watcher goes to the bucket for its own copy. The relay is a doorbell, and
//! a doorbell that rings falsely costs one bounded check, spaced by the
//! watcher's floor between checks. It is not a second feed.
//!
//! The relay is named by `update.notify` (`LAIT_FEED_NOTIFY` overrides;
//! present-but-empty is offline), so an operator running their own channel
//! runs their own relay for their own key. Absent a relay this module does
//! not start and the daemon says so once, at the level a period-only machine
//! deserves: informational, not a warning.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::feed;

/// How long a read may sit silent before the connection is presumed dead. The
/// relay comments every fifteen seconds on an idle stream, so this is five
/// missed heartbeats — a proxy that has dropped us, not a quiet channel.
pub const READ_TIMEOUT: Duration = Duration::from_secs(75);

/// Reconnect backoff bounds. The floor is what a flapping relay costs; the
/// ceiling keeps a relay that is down for an hour from being forgotten.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A connection that lived this long resets the backoff: the relay is fine
/// and the drop was ordinary.
const STEADY_AFTER: Duration = Duration::from_secs(60);

/// The keys this machine listens for, asked each time a frame arrives so a
/// World installed after the subscription opened is heard without a restart.
pub type Relevant = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// The product's channel key, and each installed World's own.
pub fn relevant_keys(identity: &Path) -> Vec<String> {
    let mut keys = vec![feed::Channel::current().as_str().to_owned()];
    let worlds = crate::serve::head::installations_root(identity);
    for declaration in crate::world::installed::declarations(&worlds).unwrap_or_default() {
        let world = declaration.manifest.id;
        let channel = super::world::channel_for(&worlds, &world);
        keys.push(world_key(&world, channel));
    }
    keys
}

/// A World's key: the path its pointer lives at under `channels/`.
pub fn world_key(world: &str, channel: feed::Channel) -> String {
    format!("worlds/{world}/{}", channel.as_str())
}

/// One frame as the relay sends it: the key and the envelope, verbatim.
#[derive(serde::Deserialize)]
struct Frame {
    key: String,
    envelope: serde_json::Value,
}

/// What one frame amounted to. Each is its own fact because each is logged
/// at a different level: a refusal is worth a warning, a repeat is not.
#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    /// A verified pointer under a followed key, newer than any acted on.
    Newer { key: String, published_at: u64 },
    /// A key this machine does not follow.
    Irrelevant(String),
    /// A followed key, but nothing newer than what was already acted on.
    Repeat(String),
    /// Not a frame, or an envelope no pinned key opens, or an unstamped one.
    Refused(String),
}

/// Open one frame against the pinned keys and this subscription's memory.
///
/// `acted` is what this subscription has already woken the watcher for, so a
/// relay replaying its board on every reconnect wakes nothing the second time.
/// It is per-subscription rather than persisted: the persisted ratchet is the
/// watcher's own, and it is the one that decides.
pub fn hear(
    data: &str,
    pubkeys: &[[u8; 32]],
    relevant: &[String],
    acted: &mut BTreeMap<String, u64>,
) -> Heard {
    let frame: Frame = match serde_json::from_str(data) {
        Ok(frame) => frame,
        Err(error) => return Heard::Refused(format!("frame is not an announcement: {error}")),
    };
    if !relevant.contains(&frame.key) {
        return Heard::Irrelevant(frame.key);
    }
    let envelope = match serde_json::to_vec(&frame.envelope) {
        Ok(bytes) => bytes,
        Err(error) => return Heard::Refused(format!("envelope: {error}")),
    };
    let payload = match feed::open_envelope(&envelope, pubkeys) {
        Ok(payload) => payload,
        Err(error) => return Heard::Refused(format!("{}: {error}", frame.key)),
    };
    let pointer: feed::PointerPayload = match serde_json::from_slice(&payload) {
        Ok(pointer) => pointer,
        Err(error) => return Heard::Refused(format!("{}: pointer payload: {error}", frame.key)),
    };
    let Some(published_at) = pointer.published_at() else {
        return Heard::Refused(format!(
            "{}: the pointer carries no publish time",
            frame.key
        ));
    };
    if acted
        .get(&frame.key)
        .is_some_and(|seen| *seen >= published_at)
    {
        return Heard::Repeat(frame.key);
    }
    acted.insert(frame.key.clone(), published_at);
    Heard::Newer {
        key: frame.key,
        published_at,
    }
}

/// A server-sent-events parser over lines: `event:` names the frame, `data:`
/// lines accumulate, a blank line dispatches, and a leading `:` is a comment
/// (the relay's keep-alive). Exactly the subset of the format the relay emits.
#[derive(Default)]
struct Frames {
    event: String,
    data: Vec<String>,
}

impl Frames {
    /// Feed one line; `Some(data)` when it completed a `pointer` frame.
    fn line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            let complete =
                (self.event.is_empty() || self.event == "pointer") && !self.data.is_empty();
            let data = self.data.join("\n");
            self.event.clear();
            self.data.clear();
            return complete.then_some(data);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => value.clone_into(&mut self.event),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        None
    }
}

/// Hold one subscription open and wake the watcher for what it hears, until
/// the stream ends. Blocking; runs on the blocking pool.
fn listen(
    base: &str,
    pubkeys: &[[u8; 32]],
    relevant: &Relevant,
    wake: &Notify,
    acted: &Mutex<BTreeMap<String, u64>>,
) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(READ_TIMEOUT)
        .build();
    let response = agent
        .get(&format!("{base}/subscribe"))
        .set("Accept", "text/event-stream")
        .call()
        .map_err(|error| format!("subscribe: {error}"))?;
    tracing::info!(relay = %base, "subscribed to the notify relay");
    let mut frames = Frames::default();
    let mut lines = BufReader::new(response.into_reader()).lines();
    loop {
        let line = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(error)) => return Err(format!("read: {error}")),
            None => return Err("the relay closed the stream".into()),
        };
        let Some(data) = frames.line(&line) else {
            continue;
        };
        let keys = relevant();
        let heard = {
            let mut acted = acted
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            hear(&data, pubkeys, &keys, &mut acted)
        };
        match heard {
            Heard::Newer { key, published_at } => {
                tracing::info!(%key, published_at, "the relay announced a newer pointer; waking the watcher");
                wake.notify_one();
            }
            Heard::Repeat(key) => {
                tracing::debug!(%key, "the relay repeated a pointer already acted on")
            }
            Heard::Irrelevant(key) => {
                tracing::trace!(%key, "a pointer this machine does not follow")
            }
            Heard::Refused(why) => tracing::warn!(%why, "a frame from the relay was refused"),
        }
    }
}

/// Keep a subscription to `base` for the daemon's life, reconnecting with
/// backoff, and wake `wake` for every newer pointer under a followed key.
pub async fn serve(
    base: String,
    pubkeys: Vec<[u8; 32]>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    relevant: Relevant,
    wake: Arc<Notify>,
) {
    let acted = Arc::new(Mutex::new(BTreeMap::new()));
    let mut backoff = MIN_BACKOFF;
    loop {
        let started = Instant::now();
        let attempt = {
            let (base, pubkeys, relevant, wake, acted) = (
                base.clone(),
                pubkeys.clone(),
                relevant.clone(),
                wake.clone(),
                acted.clone(),
            );
            tokio::task::spawn_blocking(move || listen(&base, &pubkeys, &relevant, &wake, &acted))
        };
        tokio::select! {
            ended = attempt => match ended {
                Ok(Err(why)) => tracing::debug!(relay = %base, %why, "the subscription ended"),
                Ok(Ok(())) => {}
                Err(error) => tracing::warn!(%error, "the subscription panicked"),
            },
            _ = stop.changed() => return,
        }
        if *stop.borrow() {
            return;
        }
        if started.elapsed() >= STEADY_AFTER {
            backoff = MIN_BACKOFF;
        }
        let delay = backoff.mul_f64(0.5 + super::watch::draw());
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            _ = stop.changed() => return,
        }
        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair(seed_byte: u8) -> ([u8; 32], [u8; 32]) {
        let seed = [seed_byte; 32];
        let device = mechanics::actor::device_from_seed(&seed);
        let pubkey: [u8; 32] = data_encoding::HEXLOWER
            .decode(device.as_str().as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        (seed, pubkey)
    }

    fn sealed(seed: &[u8; 32], payload: &serde_json::Value) -> serde_json::Value {
        let bytes = serde_json::to_vec(payload).unwrap();
        let signature = mechanics::actor::sign_detached(seed, &bytes);
        serde_json::json!({
            "payload": data_encoding::BASE64.encode(&bytes),
            "signature": data_encoding::BASE64.encode(&signature),
        })
    }

    fn pointer(version: &str, published_at: Option<u64>) -> serde_json::Value {
        let mut value = serde_json::json!({
            "kind": "release",
            "version": version,
            "manifest": format!("https://feed.example/releases/{version}/manifest.json"),
        });
        if let Some(at) = published_at {
            value["published_at"] = at.into();
        }
        value
    }

    fn frame(key: &str, envelope: serde_json::Value) -> String {
        serde_json::json!({ "key": key, "envelope": envelope }).to_string()
    }

    /// The whole of what a frame may do: wake once, for a verified, newer,
    /// followed pointer. Every other case is named and wakes nothing.
    #[test]
    fn a_frame_wakes_only_for_a_verified_newer_pointer_on_a_followed_key() {
        let (seed, pubkey) = keypair(7);
        let (other_seed, _) = keypair(9);
        let keys = vec![pubkey];
        let relevant = vec![
            "stable".to_owned(),
            world_key("com.lait.signage", feed::Channel::Stable),
        ];
        let mut acted = BTreeMap::new();

        assert_eq!(
            hear(
                &frame("stable", sealed(&seed, &pointer("0.9.8", Some(100)))),
                &keys,
                &relevant,
                &mut acted
            ),
            Heard::Newer {
                key: "stable".into(),
                published_at: 100
            }
        );
        assert_eq!(
            hear(
                &frame("stable", sealed(&seed, &pointer("0.9.8", Some(100)))),
                &keys,
                &relevant,
                &mut acted
            ),
            Heard::Repeat("stable".into()),
            "a relay replaying its board on reconnect wakes nothing twice"
        );
        assert_eq!(
            hear(
                &frame("stable", sealed(&seed, &pointer("0.9.7", Some(50)))),
                &keys,
                &relevant,
                &mut acted
            ),
            Heard::Repeat("stable".into()),
            "an older pointer is not newer"
        );
        assert_eq!(
            hear(
                &frame("test", sealed(&seed, &pointer("0.9.9", Some(200)))),
                &keys,
                &relevant,
                &mut acted
            ),
            Heard::Irrelevant("test".into()),
            "a channel this machine does not follow is not its business"
        );
        assert!(
            matches!(
                hear(
                    &frame("stable", sealed(&other_seed, &pointer("0.9.9", Some(200)))),
                    &keys,
                    &relevant,
                    &mut acted
                ),
                Heard::Refused(_)
            ),
            "a relay cannot make this machine believe an unsigned pointer"
        );
        assert!(
            matches!(
                hear(
                    &frame("stable", sealed(&seed, &pointer("0.9.9", None))),
                    &keys,
                    &relevant,
                    &mut acted
                ),
                Heard::Refused(_)
            ),
            "an unstamped pointer is refused: nothing unratcheted wakes anything"
        );
        assert!(matches!(
            hear("{not json", &keys, &relevant, &mut acted),
            Heard::Refused(_)
        ));
        assert_eq!(
            hear(
                &frame(
                    "worlds/com.lait.signage/stable",
                    sealed(&seed, &pointer("0.1.3", Some(150)))
                ),
                &keys,
                &relevant,
                &mut acted
            ),
            Heard::Newer {
                key: "worlds/com.lait.signage/stable".into(),
                published_at: 150
            },
            "a World's key is its own ratchet"
        );
    }

    #[test]
    fn the_frame_parser_speaks_the_subset_the_relay_emits() {
        let mut frames = Frames::default();
        assert_eq!(
            frames.line(": hb"),
            None,
            "a keep-alive comment is not a frame"
        );
        assert_eq!(
            frames.line(""),
            None,
            "a blank line with nothing pending dispatches nothing"
        );
        assert_eq!(frames.line("event: pointer"), None);
        assert_eq!(frames.line("data: {\"a\":1}"), None);
        assert_eq!(frames.line(""), Some("{\"a\":1}".into()));
        // An event the relay does not send is dropped, and the parser is clean after.
        assert_eq!(frames.line("event: error"), None);
        assert_eq!(frames.line("data: boom"), None);
        assert_eq!(frames.line(""), None);
        assert_eq!(frames.line("data:bare"), None);
        assert_eq!(
            frames.line(""),
            Some("bare".into()),
            "an unnamed frame is a pointer frame"
        );
    }

    /// The composition, against the real relay in-process: a subscription
    /// opened against a live board hears an announcement and wakes, and a
    /// subscriber arriving after the announcement is caught up by the replay.
    /// This is the seam between two crates and two processes in production,
    /// so it is asserted as a chain rather than trusted as parts.
    #[tokio::test]
    async fn a_subscription_hears_an_announcement_and_a_late_one_is_replayed_to() {
        let (seed, pubkey) = keypair(7);
        let board = Arc::new(Mutex::new(
            lait_feed_notify::Board::open(vec![pubkey], None).unwrap(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let relay = tokio::spawn(async move {
            let _ = axum::serve(listener, lait_feed_notify::router(board)).await;
        });

        let (stop, receiver) = tokio::sync::watch::channel(false);
        let relevant: Relevant = Arc::new(|| vec!["stable".to_owned()]);
        let wake = Arc::new(Notify::new());
        let early = tokio::spawn(serve(
            base.clone(),
            vec![pubkey],
            receiver.clone(),
            relevant.clone(),
            wake.clone(),
        ));

        // Announce once the subscription is up. Nothing observable says it is
        // — the relay does not count subscribers — so announce until the wake
        // arrives, bounded; each repeat is a 409 the relay refuses cheaply.
        let envelope = sealed(&seed, &pointer("0.9.9", Some(1_788_000_000))).to_string();
        let announce = base.clone();
        let announced = tokio::task::spawn_blocking(move || {
            let response = ureq::post(&format!("{announce}/announce/stable"))
                .set("Content-Type", "application/json")
                .send_string(&envelope);
            response.map(|r| r.status()).map_err(|e| e.to_string())
        });
        assert_eq!(announced.await.unwrap(), Ok(202));
        tokio::time::timeout(Duration::from_secs(10), wake.notified())
            .await
            .expect("the subscription did not wake for an announced pointer within ten seconds");

        // A subscriber arriving after the fact hears the board's replay.
        let late_wake = Arc::new(Notify::new());
        let late = tokio::spawn(serve(
            base.clone(),
            vec![pubkey],
            receiver,
            relevant,
            late_wake.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), late_wake.notified())
            .await
            .expect("a late subscriber was not caught up by the replay");

        stop.send(true).unwrap();
        for task in [early, late] {
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("a subscription did not end on the stop signal")
                .unwrap();
        }
        relay.abort();
    }
}
