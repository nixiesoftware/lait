//! The Post as a [`Carrier`] — the hosted contractor behind the seam.
//!
//! `comms::iroh` is the shape: the one module that names the concrete thing, with
//! lait's own vocabulary on the near side. Here the concrete thing is `lait-post`
//! over HTTP, and everything above this file goes on speaking [`Sealed`],
//! [`Waiting`] and [`Missed`].
//!
//! # What this adapter is responsible for, and what it refuses
//!
//! It holds **no key**. Signing is a seed the caller supplies at construction, and
//! the seam above holds none either — so a carrier cannot sign as anybody and the
//! question "whose key was spent" stays answerable one layer up, where
//! [`mechanics::egress`] answers it.
//!
//! It does **not** decide who anybody is. The recipient arrives already resolved
//! to a device, because turning a person into a device set is the directory's
//! question and a carrier that could answer it would be one.
//!
//! # Every failure is "could not be asked", and that is not laziness
//!
//! A carrier reached over a network fails in ways a local one cannot, and the one
//! thing that must never happen is a network failure arriving as an empty mailbox.
//! So [`Carrier::collect`] answers [`Missed::Unasked`] for every transport fault,
//! and the reason travels with it. `Missed::Held(vec![])` is reserved for the case
//! the service actually answered and is holding nothing.

use std::time::Duration;

use mechanics::egress::Egress;
use mechanics::ids::DeviceId;

use crate::{admissible, Carrier, Missed, Refused, Sealed, Waiting};

/// How long any one request to a Post may take.
///
/// A carrier is on the path of a person pressing something, so a stall has to
/// become an answer rather than a wait. Short enough to notice, long enough for a
/// cold TLS handshake on a slow link.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The largest reply this adapter will read into memory.
///
/// A fetch returns whole envelopes, so the ceiling is the mailbox bound rather
/// than one envelope's: [`crate::MAX_SEALED`] times [`crate::mem::MAX_MAILBOX`],
/// with room for the JSON around them. Bounded at all because a body limit is the
/// only thing between a hostile or broken service and this process's memory.
pub fn max_reply() -> u64 {
    // `try_from` rather than `as`: this crate denies silent conversions, and a
    // `usize` that does not fit a `u64` is a platform where a byte ceiling is not
    // the interesting problem. Saturating keeps the bound a bound.
    u64::try_from(crate::MAX_SEALED.saturating_mul(64)).unwrap_or(u64::MAX)
}

/// Who this client can sign as.
///
/// A seed and the device it is, held together so the two cannot drift. The device
/// is derived rather than accepted: a caller that passed a seed and a mismatched
/// device would produce signatures that verify against neither.
pub struct Signer {
    seed: [u8; 32],
    device: DeviceId,
}

impl Signer {
    pub fn new(seed: [u8; 32]) -> Self {
        let device = mechanics::actor::device_from_seed(&seed);
        Self { seed, device }
    }

    /// The device these signatures are made by.
    pub fn device(&self) -> &DeviceId {
        &self.device
    }
}

impl std::fmt::Debug for Signer {
    /// Never the seed. A `Debug` that printed one would put a signing key in every
    /// log line that ever formatted a carrier.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

/// A [`Carrier`] backed by a `lait-post` service.
#[derive(Debug)]
pub struct PostCarrier {
    /// The service's base, with no trailing slash.
    base: String,
    signer: Signer,
}

impl PostCarrier {
    /// Point a carrier at a Post.
    ///
    /// The base is normalised once here rather than at every call: a trailing
    /// slash is a spelling difference nobody controls, and joining it wrong turns
    /// every request into a 404 that names nothing.
    pub fn new(base: impl Into<String>, signer: Signer) -> Self {
        let base = base.into();
        Self {
            base: base.trim_end_matches('/').to_owned(),
            signer,
        }
    }

    /// The device this carrier signs as, which is the mailbox it can collect.
    pub fn device(&self) -> &DeviceId {
        self.signer.device()
    }

    /// Ask for a challenge to answer.
    ///
    /// Separate from the operations that use it because it is the one request that
    /// proves nothing and costs nothing: anyone may ask for any device's challenge,
    /// and it is the *answer* that has to be signed.
    fn challenge(&self, device: &DeviceId) -> Result<lait_post::Challenge, Refused> {
        let url = format!(
            "{}/challenge?device={}",
            self.base,
            urlencode(device.as_str())
        );
        let response = ureq::get(&url)
            .timeout(REQUEST_TIMEOUT)
            .call()
            .map_err(|error| unreachable_or_refusal("ask for a challenge", error))?;
        read_json(response)
    }

    fn post<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, Refused> {
        let response = ureq::post(&format!("{}{path}", self.base))
            .timeout(REQUEST_TIMEOUT)
            .send_json(body)
            .map_err(|error| unreachable_or_refusal(path, error))?;
        read_json(response)
    }
}

impl Carrier for PostCarrier {
    fn deposit(&mut self, from: &Egress<'_>, sealed: &Sealed, now: u64) -> Result<String, Refused> {
        // Checked here, before a round trip. Every one of these is decidable
        // locally, and paying a network hop to be told what this process already
        // knew is the difference between a refusal and a delay.
        admissible(sealed, now)?;

        // The witness names the device whose key is being spent; this carrier can
        // only sign as one. Refusing the mismatch is the custody fence at the
        // seam — the alternative is silently depositing under a different sender
        // than the one that was authorized, which is exactly what `egress` exists
        // to make impossible.
        if from.device() != self.signer.device() {
            return Err(Refused::Unreachable(format!(
                "this carrier signs as {}, and the send was authorized for {}",
                self.signer.device().short(),
                from.device().short()
            )));
        }

        let signed = lait_post::sign::deposit(
            &self.signer.seed,
            lait_post::Envelope {
                recipient: sealed.recipient.clone(),
                sealed: sealed.bytes.clone(),
                expires_at: sealed.expires_at,
                envelope_version: sealed.construction,
            },
        );
        let deposited: DepositedId = self.post("/deposit", &signed)?;
        Ok(deposited.deposited)
    }

    fn collect(&mut self, device: &DeviceId, now: u64) -> Missed {
        // A carrier that signs as one device cannot read another's mailbox, and
        // this is not a policy choice — it holds one seed. Said as `Unasked`
        // rather than as an empty list, because the mailbox was never asked about.
        if device != self.signer.device() {
            return Missed::Unasked(format!(
                "this carrier can only collect for {}",
                self.signer.device().short()
            ));
        }
        let _ = now;

        let challenge = match self.challenge(device) {
            Ok(challenge) => challenge,
            Err(why) => return Missed::Unasked(format!("{why}")),
        };
        let answer = lait_post::sign::fetch(&self.signer.seed, &challenge);
        match self.post::<_, Vec<lait_post::Deposited>>("/fetch", &answer) {
            Ok(held) => Missed::Held(held.into_iter().map(waiting_from).collect()),
            Err(why) => Missed::Unasked(format!("{why}")),
        }
    }

    fn acknowledge(
        &mut self,
        device: &DeviceId,
        ids: &[String],
        _now: u64,
    ) -> Result<usize, Refused> {
        if device != self.signer.device() {
            return Err(Refused::Unreachable(format!(
                "this carrier can only acknowledge for {}",
                self.signer.device().short()
            )));
        }
        if ids.is_empty() {
            // Nothing to confirm is not a request worth making. Answering locally
            // keeps an empty acknowledgement from spending a challenge, which the
            // service remembers exactly once.
            return Ok(0);
        }
        let challenge = self.challenge(device)?;
        let signed = lait_post::sign::acknowledge(&self.signer.seed, &challenge, ids.to_vec());
        let dropped: DroppedCount = self.post("/acknowledge", &signed)?;
        Ok(dropped.dropped)
    }
}

/// The deposit reply, named as the service names it.
///
/// `deposited`, not `deposit`. The first draft guessed the field and the decode
/// failed *after* a successful deposit — the letter was stored and the client
/// reported an unreachable carrier, which is the worst shape a mismatch can take
/// here: a retry would have deposited nothing new (the id is content-addressed) and
/// still looked broken.
#[derive(serde::Deserialize)]
struct DepositedId {
    deposited: String,
}

#[derive(serde::Deserialize)]
struct DroppedCount {
    dropped: usize,
}

fn waiting_from(held: lait_post::Deposited) -> Waiting {
    Waiting {
        id: held.id,
        deposited_by: held.sender,
        sealed: Sealed {
            recipient: held.envelope.recipient,
            bytes: held.envelope.sealed,
            expires_at: held.envelope.expires_at,
            construction: held.envelope.envelope_version,
        },
        arrived_at: held.deposited_at,
    }
}

/// Turn a transport failure into the right refusal.
///
/// The service's own refusals arrive as HTTP statuses with a typed body, and they
/// are *answers* — the Post decided. Everything else is the service not having
/// been reached, and the two must not become one: a caller that cannot tell
/// "refused" from "unreachable" cannot tell "fix the request" from "try later".
fn unreachable_or_refusal(what: &str, error: ureq::Error) -> Refused {
    match error {
        ureq::Error::Status(status, response) => match response.into_json::<lait_post::Refusal>() {
            Ok(refusal) => translate(refusal),
            // A status we cannot decode is still an answer, but not one we can
            // act on — so it is reported as unusable rather than mapped to a
            // guess at which refusal it might have been.
            Err(error) => Refused::Unreachable(format!(
                "{what}: the carrier answered {status} and the body did not decode ({error})"
            )),
        },
        ureq::Error::Transport(transport) => Refused::Unreachable(format!("{what}: {transport}")),
    }
}

/// Map the Post's refusals onto the seam's.
///
/// Deliberately total rather than a catch-all: a new arm on the service must be a
/// compile error here, because the alternative is a refusal silently arriving as
/// something it is not.
fn translate(refusal: lait_post::Refusal) -> Refused {
    use lait_post::Refusal as Post;
    match refusal {
        Post::TooLarge => Refused::TooLarge,
        Post::UnusableExpiry => Refused::UnusableExpiry,
        Post::UnusableDevice => Refused::UnusableRecipient,
        Post::AtCapacity => Refused::AtCapacity,
        // These three are this client's own fault or the service's own state, and
        // none of them is a fact about the envelope. A caller can only try again.
        Post::BadSignature => Refused::Unreachable(
            "the carrier rejected this client's signature; the seed and the sender disagree".into(),
        ),
        Post::UnknownChallenge => {
            Refused::Unreachable("the challenge was unknown or already answered".into())
        }
        Post::ChallengeExpired => {
            Refused::Unreachable("the challenge expired before it was answered".into())
        }
        Post::Unavailable => Refused::Unreachable("the carrier's store could not answer".into()),
    }
}

fn read_json<R: serde::de::DeserializeOwned>(response: ureq::Response) -> Result<R, Refused> {
    let mut body = Vec::new();
    response
        .into_reader()
        .take(max_reply())
        .read_to_end(&mut body)
        .map_err(|error| Refused::Unreachable(format!("read the carrier's reply: {error}")))?;
    serde_json::from_slice(&body)
        .map_err(|error| Refused::Unreachable(format!("decode the carrier's reply: {error}")))
}

use std::io::Read as _;

/// Percent-encode a query value.
///
/// Hand-written for one reason: a device id is 64 hex characters, so the only
/// bytes that can appear are already safe — and anything else in that position is
/// a caller error this should not smuggle through. Encoding the general case would
/// make a malformed id into a valid request.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
