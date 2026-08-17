//! The Post — the carrier that holds sealed correspondence for a recipient who
//! is not online.
//!
//! # What it is, and the two questions it deliberately does not conflate
//!
//! *"Who is this person, and which devices do they hold?"* is the **directory's**
//! question, and it is asked by the sender before anything reaches here. The
//! Post is keyed by [`DeviceId`] and holds no human-facing name at all.
//!
//! *"May you read this?"* is answered by **a signature from the recipient
//! device, and nothing else** — not an account session, not a token this service
//! minted. That distinction costs one signature verification and buys the whole
//! security posture: a carrier that authorizes fetch by its own session can hand
//! a mailbox to whoever it likes, while one that authorizes by device signature
//! can *withhold* a mailbox and can never *impersonate* its owner. It also keeps
//! receiving free of any account requirement, which is what preserves the
//! optionality the identity project promises.
//!
//! # What it cannot do
//!
//! It cannot read what it holds — envelopes arrive sealed to the recipient's
//! devices and leave the same way. It cannot grant standing: an invitation
//! delivered through here is still signed by a Space admin, still redeemed
//! against convergent revocation, and still refused if its window has passed.
//! **Delivery is not admission**, and the Post never learns the difference.
//!
//! # Freshness is a challenge, not a clock
//!
//! A fetch is a signature over a nonce this service issued and remembers once.
//! The alternative — signing a timestamp inside a window — needs the two sides
//! to agree about the time, and turns every clock skew into either a refusal or
//! a replay window. A challenge needs no agreement about anything.

use std::collections::HashMap;

use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};

pub mod http;
pub mod store;

pub use store::{Deposited, FsStore, MemStore, Store};

/// Signature domains. Distinct so a signature gathered for one operation can
/// never be replayed as another — the same discipline every other signed
/// preimage in this tree follows.
const DEPOSIT_DOMAIN: &[u8] = b"lait/post/1/deposit";
const FETCH_DOMAIN: &[u8] = b"lait/post/1/fetch";
const ACK_DOMAIN: &[u8] = b"lait/post/1/acknowledge";

/// The largest envelope the Post will hold. An invitation is ~1.5 KB and a
/// first message is small; this is generous by three orders of magnitude and
/// still bounds what one deposit can cost.
pub const MAX_ENVELOPE: usize = 256 * 1024;

/// How long an issued challenge stays answerable, in seconds. Short, because the
/// only thing it has to survive is one round trip.
pub const CHALLENGE_TTL: u64 = 60;

/// The longest a deposit may be held regardless of what it asks for. A payload
/// carries its own validity window; this bounds the carrier's exposure to one
/// that asks for a century.
pub const MAX_RETENTION: u64 = 30 * 24 * 60 * 60;

/// Why the Post refused.
///
/// Each arm names a different remedy, which is the reason they are not one
/// "denied": a caller that cannot tell a bad signature from an expired challenge
/// cannot tell "sign it properly" from "ask again".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "refused")]
pub enum Refusal {
    /// The signature did not verify against the device that claims to have
    /// made it.
    BadSignature,
    /// The named device is not a well-formed key, or is spelled
    /// non-canonically. Both are one remedy — send the 64 lower-case hex
    /// characters and nothing else — and admitting a second spelling would
    /// split one mailbox in two.
    UnusableDevice,
    /// No such challenge, or it has already been answered. Challenges are
    /// single-use; a second answer to one is a replay whether or not it meant
    /// to be.
    UnknownChallenge,
    /// The challenge was issued too long ago.
    ChallengeExpired,
    /// The envelope is larger than [`MAX_ENVELOPE`].
    TooLarge,
    /// The deposit's own expiry is in the past, or beyond [`MAX_RETENTION`].
    UnusableExpiry,
    /// The store could not answer. Never rendered as "nothing waiting" — an
    /// absence that could not be measured is not an absence.
    Unavailable,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Refusal {}

/// One sealed envelope, addressed to one device.
///
/// The Post never opens it. `version` rides the record rather than the bytes,
/// because the sealed-box work already established that a version which cannot
/// be sniffed in band has to be carried by its holder — an envelope begins with
/// a uniformly random byte, so any in-band tag collides with one in 256 of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// The device this copy is sealed to.
    pub recipient: DeviceId,
    /// The sealed bytes. Opaque here, by construction.
    pub sealed: Vec<u8>,
    /// When this stops being worth holding, unix seconds. Taken from the
    /// payload's own validity window rather than invented as a carrier policy.
    pub expires_at: u64,
    /// Which sealing construction produced `sealed`.
    pub envelope_version: u16,
}

impl Envelope {
    fn preimage(&self, sender: &DeviceId) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.sealed.len() + 128);
        framed(&mut out, DEPOSIT_DOMAIN);
        framed(&mut out, sender.as_str().as_bytes());
        framed(&mut out, self.recipient.as_str().as_bytes());
        framed(&mut out, &self.expires_at.to_be_bytes());
        framed(&mut out, &self.envelope_version.to_be_bytes());
        framed(&mut out, &self.sealed);
        out
    }
}

/// A deposit, signed by whoever is making it.
///
/// The sender's signature is not what authorizes the deposit — anyone may write
/// to anyone, which is what makes a first contact possible at all. It is what
/// gives abuse control something to name: a rate limit or a block needs a
/// subject, and an unsigned deposit has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedDeposit {
    pub sender: DeviceId,
    pub envelope: Envelope,
    #[serde(with = "hex_signature")]
    pub signature: [u8; 64],
}

/// A challenge this service issued, and remembers exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub device: DeviceId,
    #[serde(with = "hex_nonce")]
    pub nonce: [u8; 32],
    pub issued_at: u64,
}

impl Challenge {
    fn preimage(&self, domain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        framed(&mut out, domain);
        framed(&mut out, self.device.as_str().as_bytes());
        framed(&mut out, &self.nonce);
        out
    }
}

/// An answered challenge: read what is waiting for this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedFetch {
    pub device: DeviceId,
    #[serde(with = "hex_nonce")]
    pub nonce: [u8; 32],
    #[serde(with = "hex_signature")]
    pub signature: [u8; 64],
}

/// Drop what the recipient confirms it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAck {
    pub device: DeviceId,
    #[serde(with = "hex_nonce")]
    pub nonce: [u8; 32],
    /// The deposits being confirmed, by id.
    pub deposits: Vec<String>,
    #[serde(with = "hex_signature")]
    pub signature: [u8; 64],
}

impl SignedAck {
    fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128 + self.deposits.len() * 40);
        framed(&mut out, ACK_DOMAIN);
        framed(&mut out, self.device.as_str().as_bytes());
        framed(&mut out, &self.nonce);
        out.extend_from_slice(&(self.deposits.len() as u32).to_be_bytes());
        for id in &self.deposits {
            framed(&mut out, id.as_bytes());
        }
        out
    }
}

/// Length-framed, so two different field splits can never produce one preimage.
fn framed(out: &mut Vec<u8>, part: &[u8]) {
    out.extend_from_slice(&(part.len() as u32).to_be_bytes());
    out.extend_from_slice(part);
}

fn verify(device: &DeviceId, preimage: &[u8], signature: &[u8; 64]) -> Result<(), Refusal> {
    let key = device_key(device)?;
    if mechanics::actor::verify_detached(&key, preimage, signature) {
        Ok(())
    } else {
        Err(Refusal::BadSignature)
    }
}

/// The ed25519 key a device id names, and the one gate that decides a device id
/// is addressable here at all.
///
/// **A non-canonical spelling is refused rather than normalised**, and that is
/// the whole point of this function. `DeviceId` is a newtype over `String` whose
/// `Deserialize` is derived, so an id arriving on the wire has been through no
/// parser: `DeviceId::from_key_string` says in its own doc that it validates
/// nothing. Meanwhile a key decoded permissively accepts either case and yields
/// the same 32 bytes. Put those together and `AABB…` and `aabb…` verify against
/// one key while remaining two different `BTreeMap` keys, two different
/// `box_dir` hashes, and unequal in `take_challenge` — so a deposit addressed in
/// upper case lands in a mailbox no lower-case fetch will ever see, and nothing
/// anywhere reports a problem.
///
/// Normalising here would close that. Refusing closes it *and* leaves exactly one
/// spelling on the wire, which is the discipline the rest of the tree already
/// keeps: `SignedCoordinates::decode_canonical` and `correspondence::Hello::decode`
/// both reject a non-canonical encoding rather than repairing it. It also keeps
/// every downstream use of the id *as a string* — store key, directory hash,
/// challenge comparison, signature preimage — safe without any of them having to
/// know why.
fn device_key(device: &DeviceId) -> Result<[u8; 32], Refusal> {
    // `parse` lower-cases and trims, so comparing its output against the input
    // is what turns a normaliser into a canonicality check.
    let parsed = DeviceId::parse(device.as_str()).ok_or(Refusal::UnusableDevice)?;
    if parsed.as_str() != device.as_str() {
        return Err(Refusal::UnusableDevice);
    }
    parsed.key_bytes().ok_or(Refusal::UnusableDevice)
}

/// The carrier.
pub struct Post<S: Store> {
    store: S,
    /// Issued challenges, by nonce. Single-use: answering removes it.
    outstanding: HashMap<[u8; 32], Challenge>,
}

impl<S: Store> Post<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            outstanding: HashMap::new(),
        }
    }

    /// Issue a challenge for a device to answer.
    ///
    /// Costs nothing and proves nothing — anyone may ask for one, for any
    /// device. It is the *answer* that has to be signed, so an attacker
    /// collecting challenges for someone else's device collects only nonces.
    pub fn challenge(&mut self, device: &DeviceId, now: u64) -> Result<Challenge, Refusal> {
        device_key(device)?;
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|error| {
            tracing::error!(%error, "OS randomness unavailable");
            Refusal::Unavailable
        })?;
        let challenge = Challenge {
            device: device.clone(),
            nonce,
            issued_at: now,
        };
        self.expire_challenges(now);
        self.outstanding.insert(nonce, challenge.clone());
        Ok(challenge)
    }

    /// Accept an envelope for later collection.
    pub fn deposit(&mut self, request: &SignedDeposit, now: u64) -> Result<String, Refusal> {
        if request.envelope.sealed.len() > MAX_ENVELOPE {
            return Err(Refusal::TooLarge);
        }
        if request.envelope.expires_at <= now
            || request.envelope.expires_at.saturating_sub(now) > MAX_RETENTION
        {
            return Err(Refusal::UnusableExpiry);
        }
        device_key(&request.envelope.recipient)?;
        verify(
            &request.sender,
            &request.envelope.preimage(&request.sender),
            &request.signature,
        )?;
        self.store
            .put(&request.sender, &request.envelope, now)
            .map_err(|_| Refusal::Unavailable)
    }

    /// Return what is waiting, to a device that proves it is that device.
    pub fn fetch(&mut self, request: &SignedFetch, now: u64) -> Result<Vec<Deposited>, Refusal> {
        // Before the challenge is spent: a malformed or non-canonical id is a
        // malformed request, not the wrong-device probe `take_challenge` burns a
        // nonce to discourage, and it deserves the refusal that names it.
        device_key(&request.device)?;
        let challenge = self.take_challenge(&request.nonce, &request.device, now)?;
        verify(
            &request.device,
            &challenge.preimage(FETCH_DOMAIN),
            &request.signature,
        )?;
        self.store
            .list(&request.device, now)
            .map_err(|_| Refusal::Unavailable)
    }

    /// Drop what the recipient confirms.
    pub fn acknowledge(&mut self, request: &SignedAck, now: u64) -> Result<usize, Refusal> {
        device_key(&request.device)?;
        let challenge = self.take_challenge(&request.nonce, &request.device, now)?;
        // The ack signs the deposit ids too, so a captured signature cannot be
        // replayed against a different set — even though the challenge already
        // makes it single-use.
        let _ = &challenge;
        verify(&request.device, &request.preimage(), &request.signature)?;
        self.store
            .drop_all(&request.device, &request.deposits)
            .map_err(|_| Refusal::Unavailable)
    }

    /// Collect what nobody came for. Returns how many went.
    pub fn sweep(&mut self, now: u64) -> usize {
        self.expire_challenges(now);
        self.store.sweep(now).unwrap_or(0)
    }

    fn take_challenge(
        &mut self,
        nonce: &[u8; 32],
        device: &DeviceId,
        now: u64,
    ) -> Result<Challenge, Refusal> {
        let challenge = self
            .outstanding
            .remove(nonce)
            .ok_or(Refusal::UnknownChallenge)?;
        if challenge.device != *device {
            // Issued for somebody else. Removed anyway: a nonce someone else
            // answered is spent either way, and leaving it would let a probe
            // grind through outstanding challenges for free.
            return Err(Refusal::UnknownChallenge);
        }
        if now.saturating_sub(challenge.issued_at) > CHALLENGE_TTL {
            return Err(Refusal::ChallengeExpired);
        }
        Ok(challenge)
    }

    fn expire_challenges(&mut self, now: u64) {
        self.outstanding
            .retain(|_, held| now.saturating_sub(held.issued_at) <= CHALLENGE_TTL);
    }

    /// How many challenges are outstanding. For the operator's gauge, and for
    /// the test that says they do not accumulate.
    pub fn outstanding_challenges(&self) -> usize {
        self.outstanding.len()
    }
}

mod hex_signature {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let text = String::deserialize(d)?;
        let bytes = data_encoding::HEXLOWER_PERMISSIVE
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)?;
        <[u8; 64]>::try_from(bytes.as_slice())
            .map_err(|_| serde::de::Error::custom("a signature is 64 bytes"))
    }
}

mod hex_nonce {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let bytes = data_encoding::HEXLOWER_PERMISSIVE
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)?;
        <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| serde::de::Error::custom("a nonce is 32 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::actor::{device_from_seed, sign_detached};

    const SENDER_SEED: [u8; 32] = [11u8; 32];
    const RECIPIENT_SEED: [u8; 32] = [22u8; 32];
    const STRANGER_SEED: [u8; 32] = [33u8; 32];
    const NOW: u64 = 1_800_000_000;

    fn envelope_to(recipient: &DeviceId, body: &[u8]) -> Envelope {
        Envelope {
            recipient: recipient.clone(),
            sealed: body.to_vec(),
            expires_at: NOW + 3600,
            envelope_version: 1,
        }
    }

    fn signed_deposit(seed: &[u8; 32], envelope: Envelope) -> SignedDeposit {
        let sender = device_from_seed(seed);
        let signature = sign_detached(seed, &envelope.preimage(&sender));
        SignedDeposit {
            sender,
            envelope,
            signature,
        }
    }

    fn answer(seed: &[u8; 32], challenge: &Challenge) -> SignedFetch {
        SignedFetch {
            device: challenge.device.clone(),
            nonce: challenge.nonce,
            signature: sign_detached(seed, &challenge.preimage(FETCH_DOMAIN)),
        }
    }

    #[test]
    fn a_deposit_is_collected_by_the_device_it_was_addressed_to() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());

        post.deposit(
            &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"sealed bytes")),
            NOW,
        )
        .expect("deposit");

        let challenge = post.challenge(&recipient, NOW).expect("challenge");
        let waiting = post
            .fetch(&answer(&RECIPIENT_SEED, &challenge), NOW)
            .expect("fetch");

        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].envelope.sealed, b"sealed bytes");
        assert_eq!(waiting[0].sender, device_from_seed(&SENDER_SEED));
    }

    #[test]
    fn a_device_spelled_in_upper_case_is_refused_rather_than_given_a_second_mailbox() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let shouted = DeviceId::from_key_string(recipient.as_str().to_ascii_uppercase());

        // The premise, and the reason this was silent rather than merely wrong:
        // both spellings name the *same* ed25519 key, so a signature made by
        // that device verifies under either. Only the string differs — and the
        // string is the store key, the mailbox directory hash, and the equality
        // `take_challenge` tests.
        assert_eq!(
            shouted.key_bytes(),
            recipient.key_bytes(),
            "the two spellings must name one key, or this test is about nothing"
        );
        assert_ne!(shouted, recipient);

        let mut post = Post::new(MemStore::default());

        assert_eq!(
            post.deposit(
                &signed_deposit(&SENDER_SEED, envelope_to(&shouted, b"shouted")),
                NOW
            ),
            Err(Refusal::UnusableDevice),
            "an upper-case address must be refused, not filed where no canonical fetch will look"
        );
        assert_eq!(
            post.challenge(&shouted, NOW),
            Err(Refusal::UnusableDevice),
            "and a challenge must not be issued for a spelling that cannot be fetched under"
        );

        // The device is addressable; it was the spelling that was refused.
        post.deposit(
            &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"canonical")),
            NOW,
        )
        .expect("the canonical spelling is accepted");
        let challenge = post.challenge(&recipient, NOW).expect("challenge");
        let waiting = post
            .fetch(&answer(&RECIPIENT_SEED, &challenge), NOW)
            .expect("fetch");
        assert_eq!(waiting.len(), 1, "one spelling, one mailbox, one letter");
        assert_eq!(waiting[0].envelope.sealed, b"canonical");
    }

    #[test]
    fn a_shouted_fetch_is_refused_without_spending_the_challenge() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        post.deposit(
            &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"waiting")),
            NOW,
        )
        .expect("deposit");

        let challenge = post.challenge(&recipient, NOW).expect("challenge");
        let mut shouted = answer(&RECIPIENT_SEED, &challenge);
        shouted.device = DeviceId::from_key_string(recipient.as_str().to_ascii_uppercase());

        assert_eq!(
            post.fetch(&shouted, NOW),
            Err(Refusal::UnusableDevice),
            "a malformed id is named as one, not reported as an unknown challenge"
        );

        // The nonce is deliberately spent for a *wrong-device* answer, to stop a
        // probe grinding through outstanding challenges. A malformed id is not
        // that probe, so the real device can still answer the same challenge.
        let waiting = post
            .fetch(&answer(&RECIPIENT_SEED, &challenge), NOW)
            .expect("the challenge survived a malformed answer");
        assert_eq!(waiting.len(), 1);
    }

    #[test]
    fn a_fetch_without_the_devices_signature_gets_nothing() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        post.deposit(
            &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"not yours")),
            NOW,
        )
        .expect("deposit");

        // A stranger answering the recipient's challenge with their own key.
        // This is the whole security posture in one assertion: the carrier can
        // withhold, and cannot hand a mailbox to anyone else.
        let challenge = post.challenge(&recipient, NOW).expect("challenge");
        let forged = SignedFetch {
            device: recipient.clone(),
            nonce: challenge.nonce,
            signature: sign_detached(&STRANGER_SEED, &challenge.preimage(FETCH_DOMAIN)),
        };
        assert_eq!(post.fetch(&forged, NOW), Err(Refusal::BadSignature));
    }

    #[test]
    fn a_challenge_answers_once() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        let challenge = post.challenge(&recipient, NOW).expect("challenge");
        let reply = answer(&RECIPIENT_SEED, &challenge);

        assert!(post.fetch(&reply, NOW).is_ok());
        assert_eq!(
            post.fetch(&reply, NOW),
            Err(Refusal::UnknownChallenge),
            "a replayed answer is refused even though its signature is genuine"
        );
    }

    #[test]
    fn a_stale_challenge_is_refused_and_outstanding_ones_do_not_accumulate() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        let challenge = post.challenge(&recipient, NOW).expect("challenge");

        assert_eq!(
            post.fetch(
                &answer(&RECIPIENT_SEED, &challenge),
                NOW + CHALLENGE_TTL + 1
            ),
            Err(Refusal::ChallengeExpired)
        );

        for _ in 0..5 {
            post.challenge(&recipient, NOW).expect("challenge");
        }
        assert_eq!(post.outstanding_challenges(), 5);
        post.sweep(NOW + CHALLENGE_TTL + 1);
        assert_eq!(
            post.outstanding_challenges(),
            0,
            "an unanswered challenge is not a leak"
        );
    }

    #[test]
    fn an_expired_deposit_is_neither_returned_nor_kept() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        post.deposit(
            &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"stale")),
            NOW,
        )
        .expect("deposit");

        let later = NOW + 3601;
        let challenge = post.challenge(&recipient, later).expect("challenge");
        let waiting = post
            .fetch(&answer(&RECIPIENT_SEED, &challenge), later)
            .expect("fetch");
        assert!(waiting.is_empty(), "past its window, it is not delivered");
        assert_eq!(post.sweep(later), 1, "and it is collected");
    }

    #[test]
    fn a_deposit_that_asks_to_be_kept_forever_is_refused() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());

        let mut forever = envelope_to(&recipient, b"eternal");
        forever.expires_at = NOW + MAX_RETENTION + 1;
        assert_eq!(
            post.deposit(&signed_deposit(&SENDER_SEED, forever), NOW),
            Err(Refusal::UnusableExpiry)
        );

        let mut past = envelope_to(&recipient, b"already gone");
        past.expires_at = NOW - 1;
        assert_eq!(
            post.deposit(&signed_deposit(&SENDER_SEED, past), NOW),
            Err(Refusal::UnusableExpiry)
        );
    }

    #[test]
    fn a_tampered_deposit_does_not_verify() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        let mut request = signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"original"));
        request.envelope.sealed = b"substituted".to_vec();
        assert_eq!(post.deposit(&request, NOW), Err(Refusal::BadSignature));
    }

    #[test]
    fn acknowledging_drops_exactly_what_was_named() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        let first = post
            .deposit(
                &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"first")),
                NOW,
            )
            .expect("deposit");
        post.deposit(
            &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"second")),
            NOW,
        )
        .expect("deposit");

        let challenge = post.challenge(&recipient, NOW).expect("challenge");
        let ack = SignedAck {
            device: recipient.clone(),
            nonce: challenge.nonce,
            deposits: vec![first],
            signature: [0u8; 64],
        };
        let signature = sign_detached(&RECIPIENT_SEED, &ack.preimage());
        let ack = SignedAck { signature, ..ack };

        assert_eq!(post.acknowledge(&ack, NOW).expect("ack"), 1);

        let challenge = post.challenge(&recipient, NOW).expect("challenge");
        let left = post
            .fetch(&answer(&RECIPIENT_SEED, &challenge), NOW)
            .expect("fetch");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].envelope.sealed, b"second");
    }

    #[test]
    fn one_persons_mailbox_is_not_anothers() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let stranger = device_from_seed(&STRANGER_SEED);
        let mut post = Post::new(MemStore::default());
        post.deposit(
            &signed_deposit(&SENDER_SEED, envelope_to(&recipient, b"for the recipient")),
            NOW,
        )
        .expect("deposit");

        let challenge = post.challenge(&stranger, NOW).expect("challenge");
        let waiting = post
            .fetch(&answer(&STRANGER_SEED, &challenge), NOW)
            .expect("fetch");
        assert!(
            waiting.is_empty(),
            "a correctly signed fetch still only reaches its own device's deposits"
        );
    }

    #[test]
    fn an_envelope_beyond_the_bound_is_refused_before_it_is_stored() {
        let recipient = device_from_seed(&RECIPIENT_SEED);
        let mut post = Post::new(MemStore::default());
        let oversized = envelope_to(&recipient, &vec![0u8; MAX_ENVELOPE + 1]);
        assert_eq!(
            post.deposit(&signed_deposit(&SENDER_SEED, oversized), NOW),
            Err(Refusal::TooLarge)
        );
    }
}
