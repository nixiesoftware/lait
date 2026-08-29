//! A self-sovereign L3 tunnel for a lait Space, Linux-first.
//!
//! This is the first slice of lait's own packet layer — the thing that lets
//! IP packets between two members' devices ride lait's fabric instead of a
//! Tailscale tunnel, with no coordination server handing out addresses and no
//! identity provider deciding who you are.
//!
//! The one idea that makes it self-sovereign is here: **a device's address is
//! derived from its identity.** A lait `DeviceId` *is* an ed25519 key, and its
//! tunnel address is a hash of that key placed in the ULA range `fd00::/8`.
//! Two devices never collide (128-bit content address), nobody allocates
//! anything, and the address *is* the key — which is exactly what Tailscale
//! needs a coordinator to do and lait does with a hash.
//!
//! The transport in this prototype is plain UDP between configured peers: it
//! exists to prove the hard, OS-specific half — a TUN interface carrying real
//! packets on real hardware (a Raspberry Pi) — before that half is folded into
//! iroh and the `lait/exec/1` plane. It is **not encrypted yet**; run it on a
//! trusted link. The [`crate`] README carries the ladder from here to the real
//! plane.

use std::net::Ipv6Addr;

use mechanics::ids::DeviceId;

/// Domain separator for the address KDF. A change here renumbers every device.
const ULA_DOMAIN: &str = "lait.net.ula.v1";

/// Derive a device's stable tunnel address from its ed25519 key bytes.
///
/// The result is a Unique Local Address (`fd00::/8`): the high byte is forced
/// to `0xfd`, the remaining 120 bits are the key's hash. Deterministic,
/// collision-resistant, and requiring no allocator or authority — the address
/// is a pure function of the identity.
pub fn ula_from_key(key: &[u8; 32]) -> Ipv6Addr {
    let digest = blake3::derive_key(ULA_DOMAIN, key);
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&digest[..16]);
    octets[0] = 0xfd;
    Ipv6Addr::from(octets)
}

/// The tunnel address of a device, when its id carries an endpoint key.
pub fn ula_for(device: &DeviceId) -> Option<Ipv6Addr> {
    device.key_bytes().map(|key| ula_from_key(&key))
}

/// The destination address of an IPv6 packet, if `packet` begins with an IPv6
/// header. Returns `None` for anything that is not IPv6 (the version nibble is
/// not 6) or is too short to carry a header.
pub fn ipv6_destination(packet: &[u8]) -> Option<Ipv6Addr> {
    if packet.len() < 40 || (packet[0] >> 4) != 6 {
        return None;
    }
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&packet[24..40]);
    Some(Ipv6Addr::from(octets))
}

/// Parse 32 bytes of lowercase or uppercase hex.
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

/// Render 32 bytes as lowercase hex.
pub fn to_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in bytes {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

pub mod tun;

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
            let addr = ula_from_key(&key(seed));
            assert_eq!(addr.octets()[0], 0xfd, "not in fd00::/8: {addr}");
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
        packet[0] = 0x60; // version 6
        packet[24..40].copy_from_slice(&ula_from_key(&key(3)).octets());
        assert_eq!(ipv6_destination(&packet), Some(ula_from_key(&key(3))));
        packet[0] = 0x40; // version 4
        assert_eq!(ipv6_destination(&packet), None);
    }

    #[test]
    fn hex_round_trips() {
        let k = key(9);
        assert_eq!(parse_key_hex(&to_hex(&k)), Some(k));
        assert_eq!(parse_key_hex("zz"), None);
        assert_eq!(parse_key_hex(&"a".repeat(63)), None);
    }
}
