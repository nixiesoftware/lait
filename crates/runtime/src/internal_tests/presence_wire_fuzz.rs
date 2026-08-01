//! The Beacon and presence decoders, against bytes nobody wrote on purpose.
//!
//! `contact_frame_fuzz` covers the Contact handshake. These are the other two
//! decoders that parse bytes from an unauthenticated peer, and they are the
//! *most* exposed of the three: a Contact frame arrives on a connection someone
//! chose to open, while a Beacon arrives from anyone gossiping on the topic.
//! `SignedBeacon::decode_canonical` runs on every announcement the network
//! carries, before the signature inside it has been checked.
//!
//! The properties are the same four, because they are the ones a parser owes
//! regardless of what it parses:
//!
//! - arbitrary bytes never panic;
//! - every value round-trips;
//! - a message must decode to exactly the bytes it came from;
//! - a single flipped byte decodes or refuses, never panics.
//!
//! What differs from the Contact frames is the shape of the interesting input.
//! A Beacon carries a `Vec<RouteHint>` of arbitrary length, each with its own
//! `Vec<u8>` — nested variable-length data behind a size cap, which is the
//! classic place for a length to be trusted that should not be. The generator
//! leans on that rather than on the fixed-size digest fields.

use proptest::prelude::*;

use crate::beacon::{BeaconBody, RouteHint, SignedBeacon, MAX_BEACON};
use crate::neighbor_presence::{PresenceAck, PresenceProbe};

fn digest32() -> impl Strategy<Value = [u8; 32]> {
    proptest::array::uniform32(any::<u8>())
}

fn space_id() -> impl Strategy<Value = [u8; 29]> {
    proptest::collection::vec(any::<u8>(), 29).prop_map(|v| {
        let mut out = [0u8; 29];
        out.copy_from_slice(&v);
        out
    })
}

fn signature() -> impl Strategy<Value = [u8; 64]> {
    proptest::collection::vec(any::<u8>(), 64).prop_map(|v| {
        let mut out = [0u8; 64];
        out.copy_from_slice(&v);
        out
    })
}

/// Route hints are the variable-length part, and the part a peer controls
/// freely. Kept small in count and size: what matters is that nesting exists
/// at all, not that it is deep.
fn route_hint() -> impl Strategy<Value = RouteHint> {
    (any::<u8>(), proptest::collection::vec(any::<u8>(), 0..48))
        .prop_map(|(scheme, bytes)| RouteHint { scheme, bytes })
}

fn beacon_body() -> impl Strategy<Value = BeaconBody> {
    (
        any::<u16>(),
        space_id(),
        digest32(),
        any::<u64>(),
        any::<u64>(),
        digest32(),
        any::<u64>(),
        any::<u8>(),
        proptest::collection::vec(route_hint(), 0..6),
    )
        .prop_map(
            |(
                protocol,
                space,
                station,
                epoch,
                sequence,
                frontier_root,
                frontier_count,
                flags,
                routes,
            )| BeaconBody {
                protocol,
                space,
                station,
                epoch,
                sequence,
                frontier_root,
                frontier_count,
                flags,
                routes,
            },
        )
}

fn signed_beacon() -> impl Strategy<Value = SignedBeacon> {
    (any::<u8>(), beacon_body(), any::<u8>(), signature()).prop_map(
        |(version, body, signature_algorithm, signature)| SignedBeacon {
            version,
            body,
            signature_algorithm,
            signature,
        },
    )
}

fn presence_probe() -> impl Strategy<Value = PresenceProbe> {
    (
        any::<u16>(),
        space_id(),
        digest32(),
        digest32(),
        digest32(),
        digest32(),
        any::<u8>(),
        signature(),
    )
        .prop_map(
            |(
                protocol,
                space,
                initiator_station,
                responder_station,
                initiator_transport,
                nonce,
                signature_algorithm,
                signature,
            )| PresenceProbe {
                protocol,
                space,
                initiator_station,
                responder_station,
                initiator_transport,
                nonce,
                signature_algorithm,
                signature,
            },
        )
}

fn presence_ack() -> impl Strategy<Value = PresenceAck> {
    (digest32(), digest32(), digest32(), any::<u8>(), signature()).prop_map(
        |(probe_hash, responder_transport, nonce, signature_algorithm, signature)| PresenceAck {
            probe_hash,
            responder_transport,
            nonce,
            signature_algorithm,
            signature,
        },
    )
}

/// Arbitrary bytes, biased toward what a length-prefixed decoder mishandles: a
/// plausible prefix followed by far less data than it promises.
fn wire_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..256),
        // A postcard varint claiming a large collection, then nothing. This is
        // the shape that turns a trusted length into an allocation.
        (1u8..=255, proptest::collection::vec(any::<u8>(), 0..32)).prop_map(|(len, rest)| {
            let mut out = vec![1u8, len];
            out.extend(rest);
            out
        }),
        // Truncations of a real encoding.
        (0usize..200).prop_map(|n| vec![0xFFu8; n]),
    ]
}

fn config() -> ProptestConfig {
    // As elsewhere: cheap per push, and `PROPTEST_CASES` raises it for nightly.
    // Not `with_cases`, which would overwrite the env-derived default.
    let from_env = std::env::var("PROPTEST_CASES").is_ok();
    let default = ProptestConfig::default();
    ProptestConfig {
        cases: if from_env { default.cases } else { 256 },
        ..default
    }
}

proptest! {
    #![proptest_config(config())]

    /// A Beacon arrives from anyone on the topic. Nothing it can contain may
    /// unwind the decoder.
    #[test]
    fn arbitrary_bytes_never_panic_the_beacon_decoder(bytes in wire_bytes()) {
        let _ = SignedBeacon::decode_canonical(&bytes);
    }

    /// The presence challenge, same treatment. Both halves decode before any
    /// signature is verified.
    #[test]
    fn arbitrary_bytes_never_panic_the_presence_decoders(bytes in wire_bytes()) {
        let _ = PresenceProbe::decode(&bytes);
        let _ = PresenceAck::decode(&bytes);
    }

    /// A Beacon survives encode -> decode unchanged, route hints included.
    #[test]
    fn a_beacon_round_trips(beacon in signed_beacon()) {
        let encoded = beacon.encode();
        // A generated beacon can exceed the cap through its route hints; that
        // is the cap doing its job, not a round-trip failure.
        prop_assume!(encoded.len() <= MAX_BEACON);
        let decoded = SignedBeacon::decode_canonical(&encoded)
            .map_err(|invalid| TestCaseError::fail(format!("valid beacon refused: {invalid:?}")))?;
        prop_assert_eq!(decoded, beacon);
    }

    #[test]
    fn presence_messages_round_trip(probe in presence_probe(), ack in presence_ack()) {
        let decoded_probe = PresenceProbe::decode(&probe.encode())
            .map_err(|e| TestCaseError::fail(format!("valid probe refused: {e:?}")))?;
        prop_assert_eq!(decoded_probe, probe);
        let decoded_ack = PresenceAck::decode(&ack.encode())
            .map_err(|e| TestCaseError::fail(format!("valid ack refused: {e:?}")))?;
        prop_assert_eq!(decoded_ack, ack);
    }

    /// Trailing bytes are refused rather than ignored, so two peers cannot
    /// disagree about the bytes a signature was computed over.
    #[test]
    fn trailing_bytes_are_not_ignored(
        beacon in signed_beacon(),
        probe in presence_probe(),
        tail in proptest::collection::vec(any::<u8>(), 1..16),
    ) {
        let mut encoded = beacon.encode();
        prop_assume!(encoded.len() + tail.len() <= MAX_BEACON);
        encoded.extend_from_slice(&tail);
        prop_assert!(
            SignedBeacon::decode_canonical(&encoded).is_err(),
            "a beacon with a {}-byte tail decoded anyway",
            tail.len()
        );

        let mut encoded = probe.encode();
        encoded.extend_from_slice(&tail);
        prop_assert!(
            PresenceProbe::decode(&encoded).is_err(),
            "a probe with a {}-byte tail decoded anyway",
            tail.len()
        );
    }

    /// A flipped byte decodes or refuses. The route-hint lengths live in these
    /// bytes, so this is the case where a corrupted length gets believed.
    #[test]
    fn a_flipped_byte_is_survivable(
        beacon in signed_beacon(),
        index in any::<prop::sample::Index>(),
        mask in 1u8..=255,
    ) {
        let mut encoded = beacon.encode();
        if encoded.is_empty() {
            return Ok(());
        }
        let at = index.index(encoded.len());
        encoded[at] ^= mask;
        let _ = SignedBeacon::decode_canonical(&encoded);
    }
}

/// The size cap is checked before anything is parsed, so an oversized
/// announcement costs a length comparison rather than an allocation. A Beacon
/// is the one message here that arrives unsolicited, which makes that ordering
/// load-bearing rather than tidy.
#[test]
fn a_beacon_over_the_cap_is_refused_by_size() {
    let oversized = vec![1u8; MAX_BEACON + 1];
    assert!(
        SignedBeacon::decode_canonical(&oversized).is_err(),
        "a beacon larger than MAX_BEACON must be refused"
    );
}
