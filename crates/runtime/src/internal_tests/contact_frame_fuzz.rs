//! The Contact wire decoders, against bytes nobody wrote on purpose.
//!
//! Everything here decodes input from a peer that has not been authenticated
//! yet. `ContactFrame::decode`, `Offer::decode` and `Proof::decode` all run
//! *before* a signature is checked — they have to, because the signature is
//! inside the thing being decoded. So the parser is the outermost attack
//! surface in this codebase, reachable by anyone who can open a connection.
//!
//! The existing tests here drive well-formed frames through the state machines
//! and check the protocol. That is the right test for the protocol and says
//! nothing about the parser, because every input it supplies was produced by
//! the encoder.
//!
//! ## What is asserted
//!
//! - **Never panic.** A malformed frame must be a `Result::Err`, not an
//!   unwind. A panic in a pre-auth decoder reached over the network is a
//!   remote denial of service; on a `Station` driving many contacts it takes
//!   more than the one connection down with it.
//! - **Round-trip.** Every frame variant survives encode → decode unchanged,
//!   including the ones carrying a `BodyKey` and length-prefixed bytes.
//! - **Canonical.** A frame must decode to exactly the bytes it came from.
//!   `decode_canonical` re-encodes what it parsed and compares byte-for-byte,
//!   which is stronger than checking for a trailing tail: it also refuses a
//!   non-minimal varint, and two peers that disagree about the bytes of a
//!   frame do not agree about the transcript hash computed over it.
//! - **Bit flips are survivable.** A single corrupted byte in an otherwise
//!   valid frame decodes or refuses; it does not panic. This is the case that
//!   a naive length prefix gets wrong, by trusting a length the corruption
//!   just made enormous.
//!
//! ## Why proptest rather than cargo-fuzz
//!
//! `cargo-fuzz` needs a nightly toolchain and a separate CI job, and this
//! workspace pins stable with an MSRV floor it checks. A structure-aware
//! generator on stable buys most of the coverage for none of that: proptest
//! shrinks a failure to a minimal input and records its seed in
//! `proptest-regressions/`, which is the part that makes a finding permanent.
//! What it does not do is coverage-guided exploration — if this file ever
//! starts finding things it cannot explain, that is the argument for the
//! nightly job, and it should be made then rather than now.

use proptest::prelude::*;

use replica::body::{BodyId, BodyKey, WorldId};

use crate::plane::contact::{ContactFrame, ContactId, Offer, Proof, MAX_FRAME};

fn world() -> WorldId {
    WorldId::parse("com.example.notes").expect("static world id")
}

fn body_key() -> impl Strategy<Value = BodyKey> {
    proptest::array::uniform16(any::<u8>())
        .prop_map(|raw| BodyKey::new(world(), BodyId::from_bytes(raw)))
}

fn digest() -> impl Strategy<Value = [u8; 32]> {
    proptest::array::uniform32(any::<u8>())
}

/// Payload bytes, kept short. The interesting inputs are structural — an
/// impossible length, a truncated tail, an unknown tag — and megabyte payloads
/// only slow the generator down. `a_frame_over_the_cap_is_refused_by_size`
/// covers the one case where size itself is the point.
fn payload() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..64)
}

fn frame() -> impl Strategy<Value = ContactFrame> {
    prop_oneof![
        (payload(), any::<u32>(), any::<u64>(), digest()).prop_map(
            |(authority_frontier, record_count, total_bytes, set_hash)| {
                ContactFrame::AuthorityOffer {
                    authority_frontier,
                    record_count,
                    total_bytes,
                    set_hash,
                }
            }
        ),
        (any::<u32>(), digest(), payload()).prop_map(|(index, record_hash, bytes)| {
            ContactFrame::AuthorityChunk {
                index,
                record_hash,
                bytes,
            }
        }),
        (any::<u32>(), digest()).prop_map(|(record_count, set_hash)| ContactFrame::AuthorityEnd {
            record_count,
            set_hash
        }),
        payload().prop_map(|root_bytes| ContactFrame::ManifestOffer { root_bytes }),
        (digest(), proptest::collection::vec(digest(), 0..8))
            .prop_map(|(root, nodes)| ContactFrame::ManifestRequest { root, nodes }),
        (digest(), digest(), payload()).prop_map(|(root, node_hash, node_bytes)| {
            ContactFrame::ManifestNode {
                root,
                node_hash,
                node_bytes,
            }
        }),
        (digest(), body_key(), any::<u64>(), any::<u32>()).prop_map(
            |(transaction, body, offset, length)| ContactFrame::BodyRequest {
                transaction,
                body,
                offset,
                length,
            }
        ),
        (
            digest(),
            body_key(),
            any::<u64>(),
            any::<u64>(),
            digest(),
            payload()
        )
            .prop_map(|(transaction, body, offset, total, chunk_hash, bytes)| {
                ContactFrame::BodyChunk {
                    transaction,
                    body,
                    offset,
                    total,
                    chunk_hash,
                    bytes,
                }
            }),
        (digest(), body_key(), any::<u64>(), digest()).prop_map(
            |(transaction, body, total, content_commitment)| ContactFrame::BodyEnd {
                transaction,
                body,
                total,
                content_commitment,
            }
        ),
        (digest(), digest(), any::<u32>(), digest()).prop_map(
            |(authority_set_hash, manifest_root, body_count, transcript_hash)| {
                ContactFrame::TransferEnd {
                    authority_set_hash,
                    manifest_root,
                    body_count,
                    transcript_hash,
                }
            }
        ),
        (digest(), any::<u64>()).prop_map(|(transcript_hash, received_bytes)| {
            ContactFrame::TransferAck {
                transcript_hash,
                received_bytes,
            }
        }),
        any::<u16>().prop_map(|code| ContactFrame::Abort { code }),
        (any::<u32>(), payload())
            .prop_map(|(index, bytes)| ContactFrame::HoldingsChunk { index, bytes }),
        (any::<u32>(), digest())
            .prop_map(|(count, digest)| ContactFrame::HoldingsEnd { count, digest }),
    ]
}

fn contact_id() -> impl Strategy<Value = ContactId> {
    proptest::array::uniform16(any::<u8>()).prop_map(ContactId::from_bytes)
}

/// Arbitrary bytes, biased toward the shapes a decoder gets wrong: a valid tag
/// byte followed by nothing useful, and lengths right at the header boundary.
fn wire_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // Unstructured.
        proptest::collection::vec(any::<u8>(), 0..256),
        // A plausible tag, then garbage — the decoder has committed to a
        // variant before it has anything to fill it with.
        (1u8..=14, proptest::collection::vec(any::<u8>(), 0..128)).prop_map(|(tag, rest)| {
            let mut out = vec![tag];
            out.extend(rest);
            out
        }),
        // Exactly at, and one either side of, the 1 + 16 header boundary.
        (0usize..20).prop_map(|n| vec![7u8; n]),
    ]
}

fn config() -> ProptestConfig {
    // Matches `fabric`'s convergence laws: cheap on the per-push tier, and
    // PROPTEST_CASES raises it for the nightly one. Written this way rather
    // than `with_cases`, which overwrites the default config's `cases` and
    // would discard the environment variable.
    let from_env = std::env::var("PROPTEST_CASES").is_ok();
    let default = ProptestConfig::default();
    ProptestConfig {
        cases: if from_env { default.cases } else { 256 },
        ..default
    }
}

proptest! {
    #![proptest_config(config())]

    /// The headline property: no input makes the frame decoder panic.
    #[test]
    fn arbitrary_bytes_never_panic_the_frame_decoder(bytes in wire_bytes()) {
        // The result is deliberately ignored. What is asserted is that we get
        // one at all — reaching this line means no unwind.
        let _ = ContactFrame::decode(&bytes);
    }

    /// The same for the two pre-auth handshake decoders. These run on the very
    /// first bytes of a connection, before any identity is established.
    #[test]
    fn arbitrary_bytes_never_panic_the_handshake_decoders(bytes in wire_bytes()) {
        let _ = Offer::decode(&bytes);
        let _ = Proof::decode(&bytes);
    }

    /// Every variant survives the round trip, contact id included.
    #[test]
    fn every_frame_round_trips(frame in frame(), contact in contact_id()) {
        let encoded = frame.encode(&contact);
        let (decoded_contact, decoded) = ContactFrame::decode(&encoded)
            .map_err(|invalid| TestCaseError::fail(format!("valid frame refused: {invalid:?}")))?;
        prop_assert_eq!(decoded_contact.as_bytes(), contact.as_bytes());
        prop_assert_eq!(decoded, frame);
    }

    /// Trailing bytes are refused rather than ignored.
    ///
    /// `decode_canonical` earns this by re-encoding what it parsed and
    /// comparing byte-for-byte, so it does not depend on postcard reporting a
    /// leftover tail. Verified by mutation: delete that comparison and this
    /// case fails, shrunk to a `BodyEnd` with eight bytes appended.
    #[test]
    fn trailing_bytes_are_not_ignored(
        frame in frame(),
        contact in contact_id(),
        tail in proptest::collection::vec(any::<u8>(), 1..16),
    ) {
        let mut encoded = frame.encode(&contact);
        encoded.extend_from_slice(&tail);
        prop_assert!(
            ContactFrame::decode(&encoded).is_err(),
            "a frame with {} trailing bytes decoded anyway",
            tail.len()
        );
    }

    /// A single corrupted byte decodes or refuses — never panics. This is the
    /// case a naive length prefix gets wrong: the flipped byte lands in a
    /// length field, and the decoder trusts it.
    #[test]
    fn a_flipped_byte_is_survivable(
        frame in frame(),
        contact in contact_id(),
        index in any::<prop::sample::Index>(),
        mask in 1u8..=255,
    ) {
        let mut encoded = frame.encode(&contact);
        if encoded.is_empty() {
            return Ok(());
        }
        let at = index.index(encoded.len());
        encoded[at] ^= mask;
        let _ = ContactFrame::decode(&encoded);
    }
}

/// The size cap is enforced before anything is parsed, so an oversized frame
/// costs a length check rather than an allocation. Not a property — there is
/// one boundary and it is worth naming.
#[test]
fn a_frame_over_the_cap_is_refused_by_size() {
    let oversized = vec![1u8; MAX_FRAME + 1];
    assert!(
        ContactFrame::decode(&oversized).is_err(),
        "a frame larger than MAX_FRAME must be refused"
    );
}

/// A tag outside the assigned range is refused by name rather than guessed at.
/// The enum encodes variants by index, so a decoder that fell through to
/// postcard would interpret an unknown tag as some other variant's fields.
#[test]
fn an_unassigned_tag_is_refused() {
    for tag in [0u8, 15, 16, 200, 255] {
        let mut frame = vec![tag];
        frame.extend_from_slice(&[0u8; 16]);
        frame.extend_from_slice(&[0u8; 8]);
        assert!(
            ContactFrame::decode(&frame).is_err(),
            "tag {tag} is not assigned and must be refused"
        );
    }
}
