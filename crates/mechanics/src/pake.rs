#![allow(
    clippy::arithmetic_side_effects,
    reason = "curve arithmetic on edwards25519: the operators are group operations, not integer ones"
)]
//! SPAKE2 (RFC 9382) over edwards25519 — the proof a pairing code is entered
//! with.
//!
//! The threat this closes. A pairing code's first half derives the endpoint
//! the sponsor dials, and that endpoint's key is public: a stranger who
//! inverts it (2^20 guesses, offline) can stand up the same key and take the
//! sponsor's dial. Anything the sponsor then sends that is a *function of the
//! secret half alone* — a hash, a MAC keyed on it — hands the stranger an
//! offline oracle for the other 2^20, after which the stranger dials the real
//! joiner with a valid proof and, the joiner being headless, is adopted. With
//! a balanced PAKE the stranger's dial yields one online guess: the sponsor's
//! share commits the secret only through a group element blinded by a fresh
//! random scalar, and the sponsor confirms nothing until the other side has
//! proved the same secret. Wrong guesses are counted where they land.
//!
//! What is here is the RFC's protocol as specified — its edwards25519 `M`
//! and `N`, its transcript layout, its key schedule (`Ke || Ka = Hash(TT)`,
//! `KcA || KcB = HKDF(Ka, "ConfirmationKeys")`, `cA = MAC(KcA, TT)`) — with
//! the identities and password chosen by the caller. The RFC ships test
//! vectors for P-256 only; the schedule is pinned against those, because it
//! is the same bytes whatever the group, and the group half is pinned by the
//! RFC's own points and by both roles agreeing.

use curve25519_dalek::constants::ED25519_BASEPOINT_POINT as G;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::IsIdentity;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;

/// RFC 9382 §6, "edwards25519 point generation seed (M)".
const M: [u8; 32] = [
    0xd0, 0x48, 0x03, 0x2c, 0x6e, 0xa0, 0xb6, 0xd6, 0x97, 0xdd, 0xc2, 0xe8, 0x6b, 0xda, 0x85, 0xa3,
    0x3a, 0xda, 0xc9, 0x20, 0xf1, 0xbf, 0x18, 0xe1, 0xb0, 0xc6, 0xd1, 0x66, 0xa5, 0xce, 0xcd, 0xaf,
];
/// RFC 9382 §6, "edwards25519 point generation seed (N)".
const N: [u8; 32] = [
    0xd3, 0xbf, 0xb5, 0x18, 0xf4, 0x4f, 0x34, 0x30, 0xf2, 0x9d, 0x0c, 0x92, 0xaf, 0x50, 0x38, 0x65,
    0xa1, 0xed, 0x32, 0x81, 0xdc, 0x69, 0xb3, 0x5d, 0xd8, 0x68, 0xba, 0x85, 0xf8, 0x86, 0xc4, 0xab,
];
const CONFIRMATION_INFO: &[u8] = b"ConfirmationKeys";
const PASSWORD_DOMAIN: &[u8] = b"lait/pake/spake2-password/v1";

/// Why an exchange did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The other side's share is not a point, or is one that would make the
    /// shared key independent of the password.
    Share,
    /// The other side's confirmation did not verify: a different password,
    /// or a different transcript.
    Confirmation,
    /// No randomness for the blinding scalar.
    Randomness,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Share => f.write_str("the SPAKE2 share is not a usable point"),
            Self::Confirmation => f.write_str("the SPAKE2 confirmation did not verify"),
            Self::Randomness => f.write_str("no randomness for the SPAKE2 exchange"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Which side of the exchange this is. `A` sends first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    A,
    B,
}

/// One side of an exchange, between sending its share and receiving the
/// other's. Holds the blinding scalar; nothing here leaves the process.
pub struct Exchange {
    role: Role,
    a: Vec<u8>,
    b: Vec<u8>,
    w: Scalar,
    secret: Scalar,
    mine: [u8; 32],
}

/// What both sides hold once the shares have crossed: the session key and
/// the two confirmations. The key is not to be used before [`Session::confirm`]
/// has accepted the other side's confirmation.
pub struct Session {
    key: [u8; 16],
    mine: [u8; 32],
    theirs: [u8; 32],
}

/// The password as a scalar. Domain-separated so the same symbols in another
/// protocol are another scalar.
#[must_use]
pub fn password_scalar(password: &[u8]) -> Scalar {
    let mut wide = [0u8; 64];
    let digest = Sha512::new()
        .chain_update(PASSWORD_DOMAIN)
        .chain_update(password)
        .finalize();
    wide.copy_from_slice(&digest);
    Scalar::from_bytes_mod_order_wide(&wide)
}

impl Exchange {
    /// Begin as `role`, between identities `a` and `b`, over `password`.
    /// Returns the share to send.
    pub fn start(
        role: Role,
        a: &[u8],
        b: &[u8],
        password: &[u8],
    ) -> Result<(Self, [u8; 32]), Refusal> {
        let mut wide = [0u8; 64];
        getrandom::fill(&mut wide).map_err(|_| Refusal::Randomness)?;
        let secret = Scalar::from_bytes_mod_order_wide(&wide);
        let exchange = Self::with(role, a, b, password_scalar(password), secret);
        let share = exchange.mine;
        Ok((exchange, share))
    }

    fn with(role: Role, a: &[u8], b: &[u8], w: Scalar, secret: Scalar) -> Self {
        let blind = match role {
            Role::A => point(&M),
            Role::B => point(&N),
        };
        let mine = (w * blind + secret * G).compress().to_bytes();
        Self {
            role,
            a: a.to_vec(),
            b: b.to_vec(),
            w,
            secret,
            mine,
        }
    }

    /// Take the other side's share and derive the session.
    pub fn finish(self, theirs: &[u8; 32]) -> Result<Session, Refusal> {
        let their_point = CompressedEdwardsY(*theirs)
            .decompress()
            .ok_or(Refusal::Share)?;
        // A small-order share has no secret in it: the key would then be a
        // function of this side's scalar and the password alone, and the
        // confirmation sent back would be an offline oracle for the password
        // — exactly the leak the PAKE exists to close.
        if their_point.is_small_order() {
            return Err(Refusal::Share);
        }
        let unblind = match self.role {
            Role::A => point(&N),
            Role::B => point(&M),
        };
        // K = h * x * (theirs - w * (N or M)); the cofactor clears the small
        // subgroup, and a K at the identity is a share chosen to make the key
        // independent of the password — refused rather than confirmed.
        let k = ((their_point - self.w * unblind) * self.secret).mul_by_cofactor();
        if k.is_identity() {
            return Err(Refusal::Share);
        }
        let (pa, pb) = match self.role {
            Role::A => (self.mine, *theirs),
            Role::B => (*theirs, self.mine),
        };
        let mut w = self.w.to_bytes();
        w.reverse();
        let keys = schedule(&self.a, &self.b, &pa, &pb, &k.compress().to_bytes(), &w);
        let (mine, theirs) = match self.role {
            Role::A => (keys.mac_a, keys.mac_b),
            Role::B => (keys.mac_b, keys.mac_a),
        };
        Ok(Session {
            key: keys.ke,
            mine,
            theirs,
        })
    }
}

impl Session {
    /// This side's confirmation, for the other side to verify.
    #[must_use]
    pub fn confirmation(&self) -> [u8; 32] {
        self.mine
    }

    /// Verify the other side's confirmation, in constant time.
    pub fn confirm(&self, theirs: &[u8; 32]) -> Result<(), Refusal> {
        if bool::from(self.theirs.ct_eq(theirs)) {
            Ok(())
        } else {
            Err(Refusal::Confirmation)
        }
    }

    /// The session key `Ke`.
    #[must_use]
    pub fn key(&self) -> &[u8; 16] {
        &self.key
    }
}

/// The RFC's `M` and `N` are fixed valid points; a table that failed to
/// decompress would be a corrupted binary, answered with the identity so the
/// exchange refuses at `finish` rather than panicking here.
fn point(encoded: &[u8; 32]) -> EdwardsPoint {
    CompressedEdwardsY(*encoded)
        .decompress()
        .unwrap_or_default()
}

struct Keys {
    ke: [u8; 16],
    mac_a: [u8; 32],
    mac_b: [u8; 32],
}

/// RFC 9382 §3.3–§4: the transcript, `Ke || Ka = Hash(TT)`,
/// `KcA || KcB = HKDF(Ka, "ConfirmationKeys")`, `cA = MAC(KcA, TT)`,
/// `cB = MAC(KcB, TT)`. Group-independent, which is what lets the P-256
/// vectors pin it.
fn schedule(a: &[u8], b: &[u8], pa: &[u8], pb: &[u8], k: &[u8], w: &[u8]) -> Keys {
    let mut tt = Vec::new();
    for field in [a, b, pa, pb, k, w] {
        let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
        tt.extend_from_slice(&length.to_le_bytes());
        tt.extend_from_slice(field);
    }
    let digest = Sha256::digest(&tt);
    let mut ke = [0u8; 16];
    let mut ka = [0u8; 16];
    ke.copy_from_slice(digest.get(..16).unwrap_or(&[0u8; 16]));
    ka.copy_from_slice(digest.get(16..).unwrap_or(&[0u8; 16]));
    let mut confirmation = [0u8; 32];
    // The output length is fixed and valid for HKDF-SHA256; `expand` can
    // only refuse a length over 255 blocks.
    let _ = Hkdf::<Sha256>::new(None, &ka).expand(CONFIRMATION_INFO, &mut confirmation);
    let mac = |key: &[u8]| -> [u8; 32] {
        let mut out = [0u8; 32];
        if let Ok(mut hmac) = Hmac::<Sha256>::new_from_slice(key) {
            hmac.update(&tt);
            out.copy_from_slice(&hmac.finalize().into_bytes());
        }
        out
    };
    Keys {
        ke,
        mac_a: mac(confirmation.get(..16).unwrap_or(&[0u8; 16])),
        mac_b: mac(confirmation.get(16..).unwrap_or(&[0u8; 16])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        data_encoding::HEXLOWER
            .decode(text.as_bytes())
            .expect("hex")
    }

    /// RFC 9382 Appendix B, the first P-256 vector (`A='server'`,
    /// `B='client'`): the transcript layout, the hash split and the
    /// confirmation keys and MACs are the same bytes whatever the group.
    #[test]
    fn the_key_schedule_matches_the_rfc_9382_vector() {
        let w = hex("2ee57912099d31560b3a44b1184b9b4866e904c49d12ac5042c97dca461b1a5f");
        let pa = hex("04a56fa807caaa53a4d28dbb9853b9815c61a411118a6fe516a8798434751470f9010153ac33d0d5f2047ffdb1a3e42c9b4e6be662766e1eeb4116988ede5f912c");
        let pb = hex("0406557e482bd03097ad0cbaa5df82115460d951e3451962f1eaf4367a420676d09857ccbc522686c83d1852abfa8ed6e4a1155cf8f1543ceca528afb591a1e0b7");
        let k = hex("0412af7e89717850671913e6b469ace67bd90a4df8ce45c2af19010175e37eed69f75897996d539356e2fa6a406d528501f907e04d97515fbe83db277b715d3325");
        let keys = schedule(b"server", b"client", &pa, &pb, &k, &w);
        assert_eq!(keys.ke.to_vec(), hex("0e0672dc86f8e45565d338b0540abe69"));
        assert_eq!(
            keys.mac_a.to_vec(),
            hex("58ad4aa88e0b60d5061eb6b5dd93e80d9c4f00d127c65b3b35b1b5281fee38f0")
        );
        assert_eq!(
            keys.mac_b.to_vec(),
            hex("d3e2e547f1ae04f2dbdbf0fc4b79f8ecff2dff314b5d32fe9fcef2fb26dc459b")
        );
    }

    /// The RFC's points decompress and are not small-order: a constant that
    /// had drifted by a byte would fail one or the other.
    #[test]
    fn the_edwards25519_points_are_the_rfcs() {
        for encoded in [&M, &N] {
            let point = CompressedEdwardsY(*encoded)
                .decompress()
                .expect("a valid point");
            assert!(!point.is_small_order());
            assert!(point.is_torsion_free());
        }
    }

    #[test]
    fn both_roles_agree_and_confirm_each_other() {
        let (alice, share_a) = Exchange::start(Role::A, b"alice", b"bob", b"7K3Q").expect("start");
        let (bob, share_b) = Exchange::start(Role::B, b"alice", b"bob", b"7K3Q").expect("start");
        let alice = alice.finish(&share_b).expect("finish");
        let bob = bob.finish(&share_a).expect("finish");
        assert_eq!(alice.key(), bob.key());
        alice
            .confirm(&bob.confirmation())
            .expect("bob proves the password");
        bob.confirm(&alice.confirmation())
            .expect("alice proves the password");
        assert_ne!(alice.confirmation(), bob.confirmation());
        assert_ne!(share_a, share_b);
    }

    /// One wrong symbol: neither confirmation verifies, and the keys differ.
    /// The other side learns that its guess was wrong, and nothing else.
    #[test]
    fn a_wrong_password_confirms_nothing() {
        let (alice, share_a) = Exchange::start(Role::A, b"alice", b"bob", b"7K3Q").expect("start");
        let (bob, share_b) = Exchange::start(Role::B, b"alice", b"bob", b"7K3R").expect("start");
        let alice = alice.finish(&share_b).expect("finish");
        let bob = bob.finish(&share_a).expect("finish");
        assert_ne!(alice.key(), bob.key());
        assert_eq!(
            alice.confirm(&bob.confirmation()),
            Err(Refusal::Confirmation)
        );
        assert_eq!(
            bob.confirm(&alice.confirmation()),
            Err(Refusal::Confirmation)
        );
        // The identities are part of the transcript too.
        let (carol, share_c) =
            Exchange::start(Role::B, b"alice", b"carol", b"7K3Q").expect("start");
        let (alice, share_a) = Exchange::start(Role::A, b"alice", b"bob", b"7K3Q").expect("start");
        let alice = alice.finish(&share_c).expect("finish");
        let carol = carol.finish(&share_a).expect("finish");
        assert!(alice.confirm(&carol.confirmation()).is_err());
    }

    /// A share at a small-order point carries no secret: the key would be a
    /// function of this side's scalar and the password, and the confirmation
    /// an offline oracle for it. Refused before any key exists — the
    /// identity, and the point of order two, (0, -1).
    #[test]
    fn a_small_order_share_is_refused() {
        let (alice, _) = Exchange::start(Role::A, b"a", b"b", b"7K3Q").expect("start");
        let identity = EdwardsPoint::default().compress().to_bytes();
        assert_eq!(alice.finish(&identity).err(), Some(Refusal::Share));
        let mut order_two = [0xffu8; 32];
        order_two[0] = 0xec;
        order_two[31] = 0x7f;
        assert!(CompressedEdwardsY(order_two)
            .decompress()
            .expect("a valid encoding")
            .is_small_order());
        let (alice, _) = Exchange::start(Role::A, b"a", b"b", b"7K3Q").expect("start");
        assert_eq!(alice.finish(&order_two).err(), Some(Refusal::Share));
    }
}
