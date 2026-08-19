//! What actually crosses: a letter, sealed and signed.
//!
//! The plane carries one shape with different payloads, which is the whole design
//! — "what is new is the payload and the source, not the mechanism". An invitation
//! and a message are the same envelope: a [`Letter`] sealed to the recipient's
//! device set for confidentiality and signed by the sender for authenticity, then
//! handed to a [`Carrier`](crate::Carrier).
//!
//! # Two guarantees, and why both
//!
//! **Sealing** ([`mechanics::authorization::seal_to_devices`]) makes the letter
//! readable only by the recipient's devices. **Signing** makes it provably from
//! the sender. Neither implies the other: a sealed-only letter could be forged by
//! anyone who knows the recipient's device key, and a signed-only one is readable
//! by the carrier. Secure correspondence needs both, so a [`Letter`] carries its
//! own signature *inside* the seal.
//!
//! The carrier's own record of who deposited a letter — `Waiting.deposited_by` —
//! is corroboration, not the authority. The letter's signature is what a reader
//! trusts, because the carrier could lie about a deposit and cannot forge a
//! signature. This is CORR-9 made concrete: provenance travels beside the content,
//! and authority lives in the seal and the signature, never in a claim.

use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};

use crate::{Refused, Sealed};

/// The domain a letter's signature is taken under.
const LETTER_DOMAIN: &[u8] = b"lait/correspondence/1/letter";

/// The sealing context. A leading part distinct from every other consumer's, per
/// `seal_to_bound`'s stated obligation — the kernel owns no vocabulary, and a
/// caller must own a prefix nobody else uses.
const LETTER_CONTEXT: &[&[u8]] = &[b"lait/correspondence/1/letter"];

/// What a letter carries. One envelope, and this is the only thing that differs
/// between an invitation and a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "content")]
pub enum Content {
    /// A self-authenticating invitation: `SignedCoordinates` bytes.
    ///
    /// Opaque here on purpose — this crate does not depend on `runtime` and must
    /// not, so it carries the bytes and the recipient verifies them with
    /// `SignedCoordinates::verify`, which is self-authenticating against the Space
    /// id and needs no prior state. That property is what lets an invitation ride
    /// any carrier at all.
    Invitation { coordinates: Vec<u8> },
    /// A flat message. Text only, which is v1's whole scope: no threads, no
    /// markup, no attachments beyond what the content plane gives free.
    Message { body: String },
}

/// A letter, before or after it is sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Letter {
    /// The device that composed it. Provenance, checked against the signature
    /// below — never read as authority on its own.
    pub from: DeviceId,
    /// When it was written, unix seconds. Inside the signature, so it cannot be
    /// altered after the fact.
    pub sent_at: u64,
    pub content: Content,
    #[serde(with = "hex_sig")]
    signature: [u8; 64],
}

impl Letter {
    /// Compose and sign a letter as `seed`'s device.
    ///
    /// The signature covers a framed preimage of the fields rather than their
    /// serialized form, so a re-encoding that produced the same fields cannot
    /// invalidate a signature and a different framing cannot collide with this
    /// one.
    pub fn compose(seed: &[u8; 32], content: Content, sent_at: u64) -> Self {
        let from = mechanics::actor::device_from_seed(seed);
        let mut letter = Self {
            from,
            sent_at,
            content,
            signature: [0u8; 64],
        };
        letter.signature = mechanics::actor::sign_detached(seed, &letter.preimage());
        letter
    }

    fn preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        framed(&mut out, LETTER_DOMAIN);
        framed(&mut out, self.from.as_str().as_bytes());
        framed(&mut out, &self.sent_at.to_be_bytes());
        match &self.content {
            Content::Invitation { coordinates } => {
                framed(&mut out, b"invitation");
                framed(&mut out, coordinates);
            }
            Content::Message { body } => {
                framed(&mut out, b"message");
                framed(&mut out, body.as_bytes());
            }
        }
        out
    }

    /// Whether the signature verifies against the claimed sender.
    ///
    /// A letter that does not verify is not from who it says, and every path that
    /// opens one checks this before believing the `from`. Returns `false` rather
    /// than erroring, because "not from them" is an answer, not a fault.
    pub fn verifies(&self) -> bool {
        let Some(key) = self.from.key_bytes() else {
            return false;
        };
        mechanics::actor::verify_detached(&key, &self.preimage(), &self.signature)
    }

    /// Seal this letter to one recipient device, ready to hand a carrier.
    ///
    /// One device today; the actor's whole set is the CORR-8 direction and the
    /// same call takes it — `seal_to_devices` fans out over a slice.
    pub fn seal_to(&self, recipient: &DeviceId, expires_at: u64) -> Result<Sealed, Refused> {
        self.seal_to_devices(std::slice::from_ref(recipient), recipient, expires_at)
    }

    /// Seal to a device set, addressed to one of them.
    ///
    /// `addressed` is which device the carrier is keyed on; `devices` are all the
    /// devices that can open it. For a single recipient the two coincide.
    pub fn seal_to_devices(
        &self,
        devices: &[DeviceId],
        addressed: &DeviceId,
        expires_at: u64,
    ) -> Result<Sealed, Refused> {
        let plaintext = serde_json::to_vec(self)
            .map_err(|error| Refused::Unreachable(format!("encode a letter to seal: {error}")))?;
        let sealed = mechanics::authorization::seal_to_devices(devices, LETTER_CONTEXT, &plaintext)
            .map_err(|error| Refused::Unreachable(format!("seal a letter: {error}")))?;
        let bytes = serde_json::to_vec(&sealed)
            .map_err(|error| Refused::Unreachable(format!("encode a sealed letter: {error}")))?;
        Ok(Sealed {
            recipient: addressed.clone(),
            bytes,
            expires_at,
            construction: 1,
        })
    }

    /// Open a sealed letter as one of its recipient devices, and verify it.
    ///
    /// `None` when this device cannot open it (not a recipient, wrong context, or
    /// tampered), or when it opens but the signature does not verify. The two are
    /// deliberately one answer at this boundary: a letter this device cannot
    /// trust is not a letter, and telling the two apart tells a caller nothing it
    /// can act on differently.
    pub fn open(seed: &[u8; 32], me: &DeviceId, sealed: &Sealed) -> Option<Self> {
        let device_sealed: mechanics::authorization::DeviceSealed =
            serde_json::from_slice(&sealed.bytes).ok()?;
        let plaintext =
            mechanics::authorization::open_as_device(seed, me, LETTER_CONTEXT, &device_sealed)?;
        let letter: Self = serde_json::from_slice(&plaintext).ok()?;
        letter.verifies().then_some(letter)
    }
}

/// Length-framed, so two different field splits can never make one preimage.
fn framed(out: &mut Vec<u8>, part: &[u8]) {
    // `try_from`, not `as`: this crate denies silent conversions, and a part longer
    // than `u64::MAX` cannot exist. Saturating keeps the length prefix a prefix.
    out.extend_from_slice(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
    out.extend_from_slice(part);
}

mod hex_sig {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::actor::device_from_seed;

    const NOW: u64 = 1_800_000_000;

    /// A message seals to its recipient, opens for them, and reads back intact.
    #[test]
    fn a_message_seals_signs_and_opens_for_its_recipient() {
        let alice = [1u8; 32];
        let bob_device = device_from_seed(&[2u8; 32]);

        let letter = Letter::compose(
            &alice,
            Content::Message {
                body: "the first message across a Space boundary".into(),
            },
            NOW,
        );
        assert!(letter.verifies(), "a freshly composed letter verifies");

        let sealed = letter.seal_to(&bob_device, NOW + 3600).expect("seal");

        let opened = Letter::open(&[2u8; 32], &bob_device, &sealed).expect("bob opens it");
        assert_eq!(opened, letter);
        assert_eq!(opened.from, device_from_seed(&alice));
        match opened.content {
            Content::Message { body } => {
                assert_eq!(body, "the first message across a Space boundary")
            }
            other => panic!("expected a message, got {other:?}"),
        }
    }

    /// Nobody but the recipient can open it.
    #[test]
    fn a_stranger_cannot_open_a_sealed_letter() {
        let letter = Letter::compose(
            &[1u8; 32],
            Content::Message {
                body: "private".into(),
            },
            NOW,
        );
        let sealed = letter
            .seal_to(&device_from_seed(&[2u8; 32]), NOW + 3600)
            .expect("seal");

        assert!(
            Letter::open(&[9u8; 32], &device_from_seed(&[9u8; 32]), &sealed).is_none(),
            "a device that holds no wrap must not open the letter"
        );
    }

    /// A tampered letter does not verify, so `open` refuses it.
    #[test]
    fn a_forged_sender_is_rejected_on_open() {
        // Compose honestly, then rewrite `from` to somebody else and re-seal. The
        // signature no longer matches the claimed sender.
        let mut letter = Letter::compose(
            &[1u8; 32],
            Content::Message {
                body: "not mine".into(),
            },
            NOW,
        );
        letter.from = device_from_seed(&[7u8; 32]);
        let bob = device_from_seed(&[2u8; 32]);
        let sealed = letter.seal_to(&bob, NOW + 3600).expect("seal");

        assert!(
            Letter::open(&[2u8; 32], &bob, &sealed).is_none(),
            "a letter whose signature does not match its sender is not opened"
        );
    }

    /// An invitation is carried as opaque bytes and comes back byte-identical.
    ///
    /// This crate cannot verify a `SignedCoordinates` — it does not depend on
    /// `runtime` — so it proves the carriage: the bytes a sender put in are the
    /// bytes a recipient gets out, which is all the letter owes an invitation. The
    /// self-authenticating verification happens above, and the hop test does it.
    #[test]
    fn an_invitation_is_carried_intact() {
        let coordinates = vec![7u8; 512];
        let letter = Letter::compose(
            &[1u8; 32],
            Content::Invitation {
                coordinates: coordinates.clone(),
            },
            NOW,
        );
        let bob = device_from_seed(&[2u8; 32]);
        let sealed = letter.seal_to(&bob, NOW + 3600).expect("seal");
        let opened = Letter::open(&[2u8; 32], &bob, &sealed).expect("open");
        match opened.content {
            Content::Invitation { coordinates: got } => assert_eq!(got, coordinates),
            other => panic!("expected an invitation, got {other:?}"),
        }
    }
}
