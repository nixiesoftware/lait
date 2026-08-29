//! lait's L3 packet layer — the boundary that carries IP packets between two
//! members' devices over lait's own transport. Linux-first.
//!
//! This is the sealed home of the L3 concern: **addressing** (a device's
//! address is a hash of its key), the **TUN** OS seam ([`tun`]), and the
//! **carry** ([`carry`]) that composes `comms` to move packets. It names
//! `comms` and never iroh — exactly as `lait-relay` fronts `comms::relay` —
//! and it keeps every IPv6/L3 notion out of the transport seam and the kernel.
//!
//! It is a prototype boundary: a standalone tunnel today, folding into a
//! `lait/exec/1` net plane inside the daemon at slice 3, at which point the
//! [`carry`] logic moves to `runtime` and this crate keeps only the addressing
//! and the TUN seam that the plane composes.

use std::net::Ipv6Addr;

use comms::PeerId;

/// Domain separator for the address KDF. A change here renumbers every device.
const ULA_DOMAIN: &str = "lait.net.ula.v1";

/// Derive a device's stable tunnel address from its ed25519 key bytes.
///
/// A Unique Local Address (`fd00::/8`): the high byte is forced to `0xfd`, the
/// remaining 120 bits are the key's hash. Deterministic, collision-resistant,
/// and requiring no allocator or authority — the address is a pure function of
/// the identity, which is the job Tailscale needs a coordinator for.
pub fn ula_from_key(key: &[u8; 32]) -> Ipv6Addr {
    let digest = blake3::derive_key(ULA_DOMAIN, key);
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&digest[..16]);
    octets[0] = 0xfd;
    Ipv6Addr::from(octets)
}

/// The tunnel address of a peer, when its id carries an endpoint key.
pub fn ula_for(peer: &PeerId) -> Option<Ipv6Addr> {
    peer.key_bytes().map(|key| ula_from_key(&key))
}

/// The destination address of an IPv6 packet, if `packet` begins with an IPv6
/// header; `None` for anything that is not IPv6 or is too short.
pub fn ipv6_destination(packet: &[u8]) -> Option<Ipv6Addr> {
    if packet.len() < 40 || (packet[0] >> 4) != 6 {
        return None;
    }
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&packet[24..40]);
    Some(Ipv6Addr::from(octets))
}

/// Parse 32 bytes of hex — a CLI/wire convenience for keys.
pub fn parse_key_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

pub mod carry;
pub mod tun;

/// The transport policy the carry runs under, re-exported so a thin front need
/// name no `comms` type of its own.
pub use comms::policy::{LocalNet, Network};

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> [u8; 32] {
        mechanics::actor::device_from_seed(&[seed; 32])
            .key_bytes()
            .expect("a seeded device carries an endpoint key")
    }

    #[test]
    fn an_address_is_a_deterministic_function_of_the_key() {
        let k = key(7);
        assert_eq!(ula_from_key(&k), ula_from_key(&k));
    }

    #[test]
    fn every_address_is_a_unique_local_address() {
        for seed in 0..32u8 {
            assert_eq!(ula_from_key(&key(seed)).octets()[0], 0xfd);
        }
    }

    #[test]
    fn different_keys_derive_different_addresses() {
        assert_ne!(ula_from_key(&key(1)), ula_from_key(&key(2)));
    }

    #[test]
    fn destination_is_read_only_from_a_well_formed_ipv6_packet() {
        assert_eq!(ipv6_destination(&[0u8; 10]), None);
        let mut packet = [0u8; 40];
        packet[0] = 0x60;
        packet[24..40].copy_from_slice(&ula_from_key(&key(3)).octets());
        assert_eq!(ipv6_destination(&packet), Some(ula_from_key(&key(3))));
        packet[0] = 0x40;
        assert_eq!(ipv6_destination(&packet), None);
    }

    #[test]
    fn hex_parses_only_thirty_two_bytes() {
        assert_eq!(parse_key_hex(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(parse_key_hex("zz"), None);
        assert_eq!(parse_key_hex(&"a".repeat(63)), None);
    }
}
