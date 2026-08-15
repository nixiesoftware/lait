#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "cryptographic operations use fixed-width arrays and deliberately wrapping finite-field and framing arithmetic"
)]
//! End-to-end encryption primitives. All pure Rust (RustCrypto/dalek), no C toolchain,
//! no `aws-lc` — respecting the portability + supply-chain bans.
//!
//! - **AEAD**: ChaCha20-Poly1305 with the 32-byte space symmetric key. Sync
//!   payloads (catalog + issue-doc `export()` bytes) are sealed with this, so a
//!   blind relay or a non-member sees only ciphertext (the "encryption *is* the
//!   access control" posture).
//! - **Sealed box**: an anonymous X25519 + AEAD box that distributes the
//!   space key to a member addressed by their ed25519 `DeviceId`. The member's
//!   ed25519 identity is converted to X25519 (libsodium's `*_to_curve25519`).
//!
//! # Security status
//!
//! This composition has not received an independent cryptographic audit. Do not
//! treat it as suitable for high-sensitivity production data until that review
//! is complete.

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use curve25519_dalek::edwards::CompressedEdwardsY;
use sha2::{Digest, Sha512};
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

use crate::ids::DeviceId;

/// The space symmetric key length (ChaCha20-Poly1305).
pub const KEY_LEN: usize = 32;
/// A space symmetric key.
pub type SpaceKey = [u8; KEY_LEN];
const NONCE_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Randomness,
    Encryption,
}

pub(crate) fn random_array<const N: usize>() -> Result<[u8; N], Failure> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|error| {
        tracing::error!(%error, "OS randomness unavailable");
        Failure::Randomness
    })?;
    Ok(bytes)
}

/// A fresh random 32-byte space key.
pub fn random_key() -> Result<SpaceKey, Failure> {
    random_array()
}

/// A fresh random 32-byte identity seed. A lait identity is just this seed; the
/// transport constructs its keypair from it (see [`device_from_seed`]).
pub fn random_seed() -> Result<[u8; 32], Failure> {
    random_array()
}

/// The lait [`DeviceId`] (device key) of an identity seed: the ed25519 public key
/// of the 32-byte seed, hex-encoded. A `DeviceId` *is* this public key,
/// and it equals the transport's node id for the same seed (see [`crate::ids`]) —
/// so identity is defined here, in lait's own terms, with no transport type.
pub fn device_from_seed(seed: &[u8; 32]) -> DeviceId {
    let pk = ed25519_dalek::SigningKey::from_bytes(seed).verifying_key();
    DeviceId::from_key_string(data_encoding::HEXLOWER.encode(pk.as_bytes()))
}

/// The `did:key` form of an ed25519 public key (the raw bytes a [`DeviceId`]
/// *is*). A pure, offline, self-certifying function of the key — the interop
/// lingua franca the agent-identity standards converge on (draft-duda / AIP /
/// KERI): lait presents *any* member's identity outward as a
/// `did` with no registry and no network. The multicodec prefix `0xed01` marks
/// ed25519-pub; the body is multibase base58btc (`z`-prefixed), per the W3C
/// did:key spec, so every ed25519 did:key begins `z6Mk`.
pub fn did_key_from_pubkey(pubkey: &[u8; 32]) -> String {
    let mut bytes = Vec::with_capacity(34);
    bytes.extend_from_slice(&[0xed, 0x01]);
    bytes.extend_from_slice(pubkey);
    format!("did:key:z{}", base58btc_encode(&bytes))
}

/// The `did:key` of a [`DeviceId`] (which *is* a hex ed25519 public key).
/// `None` if the id is not a well-formed 32-byte key.
pub fn did_key_from_device(device: &DeviceId) -> Option<String> {
    ed_pubkey_bytes(device).map(|pk| did_key_from_pubkey(&pk))
}

/// Bitcoin-alphabet base58 encoding (no external crate: the kernel lists no
/// scaffold, and this is ~20 lines of well-defined arithmetic). Used only to
/// render a `did:key` multibase body.
fn base58btc_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let zeros = input.iter().take_while(|&&b| b == 0).count();
    let mut buf: Vec<u8> = Vec::new();
    for &byte in input {
        let mut carry = byte as u32;
        for b in buf.iter_mut() {
            carry += (*b as u32) << 8;
            *b = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            buf.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::with_capacity(zeros + buf.len());
    for _ in 0..zeros {
        out.push('1');
    }
    for &b in buf.iter().rev() {
        out.push(ALPHABET[b as usize] as char);
    }
    out
}

/// Sign an **already-built preimage** with an identity seed's Ed25519 key,
/// returning the detached 64-byte signature. Mechanics owns key operations; a
/// higher layer (e.g. runtime's World-action envelope) builds the canonical
/// length-framed preimage and hands it here, so no upper crate names a signature
/// primitive. Domain separation and framing are the caller's responsibility.
pub fn sign_detached(seed: &[u8; 32], preimage: &[u8]) -> [u8; 64] {
    use ed25519_dalek::Signer;
    let sk = ed25519_dalek::SigningKey::from_bytes(seed);
    sk.sign(preimage).to_bytes()
}

/// Verify a detached Ed25519 signature over a preimage against a 32-byte public
/// key (the raw bytes a [`DeviceId`]/`Key` *is*). Never panics on a
/// malformed key or signature — a bad input is a failed verification, not a
/// crash.
pub fn verify_detached(public_key: &[u8; 32], preimage: &[u8], signature: &[u8; 64]) -> bool {
    use ed25519_dalek::Verifier;
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    let sig = ed25519_dalek::Signature::from_bytes(signature);
    vk.verify(preimage, &sig).is_ok()
}

fn random_nonce() -> Result<[u8; NONCE_LEN], Failure> {
    random_array()
}

/// AEAD-seal a payload with the space key. Output = `nonce(12) || ciphertext`.
pub fn aead_encrypt(key: &SpaceKey, plaintext: &[u8]) -> Result<Vec<u8>, Failure> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = random_nonce()?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| Failure::Encryption)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// AEAD-open a payload; `None` if the key is wrong or the blob is malformed (the
/// blind-relay property: without the key you get nothing).
pub fn aead_decrypt(key: &SpaceKey, blob: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < NONCE_LEN {
        return None;
    }
    let cipher = ChaCha20Poly1305::new(key.into());
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    cipher.decrypt(Nonce::from_slice(nonce), ct).ok()
}

/// Parse a hex `DeviceId` into raw ed25519 public-key bytes.
fn ed_pubkey_bytes(device: &DeviceId) -> Option<[u8; 32]> {
    let s = device.as_str();
    if s.len() != 64 {
        return None;
    }
    let decoded = data_encoding::HEXLOWER_PERMISSIVE
        .decode(s.as_bytes())
        .ok()?;
    <[u8; 32]>::try_from(decoded.as_slice()).ok()
}

/// ed25519 public → X25519 public (Edwards-Y → Montgomery-u).
fn ed_pk_to_x(ed_pub: &[u8; 32]) -> Option<XPublic> {
    let ed = CompressedEdwardsY(*ed_pub).decompress()?;
    Some(XPublic::from(ed.to_montgomery().to_bytes()))
}

/// ed25519 secret seed → X25519 static secret (libsodium `sk_to_curve25519`).
fn ed_seed_to_x(seed: &[u8; 32]) -> StaticSecret {
    let h = Sha512::digest(seed);
    let mut s = [0u8; 32];
    s.copy_from_slice(&h[..32]);
    s[0] &= 248;
    s[31] &= 127;
    s[31] |= 64;
    StaticSecret::from(s)
}

/// Seal `msg` to a member addressed by their ed25519 `DeviceId` (an anonymous
/// sealed box). Output = `eph_x_pub(32) || nonce(12) || ciphertext`. Used to
/// distribute the space key. Returns `None` if the recipient key is invalid.
pub fn seal_to(recipient: &DeviceId, msg: &[u8]) -> Result<Option<Vec<u8>>, Failure> {
    let Some(recip_ed) = ed_pubkey_bytes(recipient) else {
        return Ok(None);
    };
    let Some(recip_x) = ed_pk_to_x(&recip_ed) else {
        return Ok(None);
    };
    let eph_seed = random_array()?;
    let eph = StaticSecret::from(eph_seed);
    let eph_pub = XPublic::from(&eph);
    let shared = eph.diffie_hellman(&recip_x);
    let key = box_key(shared.as_bytes(), eph_pub.as_bytes(), recip_x.as_bytes());
    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce = random_nonce()?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), msg)
        .map_err(|_| Failure::Encryption)?;
    let mut out = Vec::with_capacity(32 + NONCE_LEN + ct.len());
    out.extend_from_slice(eph_pub.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(Some(out))
}

/// Open a sealed box addressed to us, given our ed25519 seed + `DeviceId`.
pub fn open_sealed(my_seed: &[u8; 32], me: &DeviceId, sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 32 + NONCE_LEN {
        return None;
    }
    let eph_pub = XPublic::from(<[u8; 32]>::try_from(&sealed[..32]).ok()?);
    let nonce = &sealed[32..32 + NONCE_LEN];
    let ct = &sealed[32 + NONCE_LEN..];
    let my_x = ed_seed_to_x(my_seed);
    let my_ed = ed_pubkey_bytes(me)?;
    let my_x_pub = ed_pk_to_x(&my_ed)?;
    let shared = my_x.diffie_hellman(&eph_pub);
    let key = box_key(shared.as_bytes(), eph_pub.as_bytes(), my_x_pub.as_bytes());
    let cipher = ChaCha20Poly1305::new((&key).into());
    cipher.decrypt(Nonce::from_slice(nonce), ct).ok()
}

/// A payload encrypted once, with its key wrapped separately for each device
/// permitted to read it.
///
/// The shape `custody.rs` already argues for, with device-of-the-actor
/// substituted for unlock slot: one random data key encrypts the payload, and
/// each wrap is an independent path to that key. Adding a reader adds a wrap and
/// re-encrypts nothing, which is the property that makes a person's device set
/// something that can grow without rewriting everything addressed to them.
///
/// A mailbox is the motivating case. Sealing to the Space epoch key would make
/// every member's correspondence readable by every other member; sealing
/// separately to each device would re-encrypt the payload per device and make
/// adding a phone an O(mailbox) operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceSealed {
    /// The payload under the data key.
    pub ciphertext: Vec<u8>,
    /// The data key, wrapped once per reader. Sorted by device, so the same set
    /// of readers always encodes the same way.
    pub wraps: Vec<(DeviceId, Vec<u8>)>,
}

/// Seal `plaintext` so that exactly `devices` can read it, bound to `context`.
///
/// Duplicate devices collapse; an unusable device key is skipped rather than
/// failing the whole seal, because one malformed entry in a device set must not
/// make a person unreachable at every other device they hold.
pub fn seal_to_devices(
    devices: &[DeviceId],
    context: &[&[u8]],
    plaintext: &[u8],
) -> Result<DeviceSealed, Failure> {
    let dek = random_key()?;
    let ciphertext = aead_encrypt(&dek, plaintext)?;
    let mut wraps: Vec<(DeviceId, Vec<u8>)> = Vec::new();
    for device in devices {
        if wraps.iter().any(|(held, _)| held == device) {
            continue;
        }
        if let Some(wrapped) = seal_to_bound(device, context, &dek)? {
            wraps.push((device.clone(), wrapped));
        }
    }
    wraps.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(DeviceSealed { ciphertext, wraps })
}

/// Read a payload as one of its devices. `None` when this device holds no wrap,
/// the context differs, or the payload has been tampered with.
pub fn open_as_device(
    my_seed: &[u8; 32],
    me: &DeviceId,
    context: &[&[u8]],
    sealed: &DeviceSealed,
) -> Option<Vec<u8>> {
    let (_, wrapped) = sealed.wraps.iter().find(|(device, _)| device == me)?;
    let dek = open_sealed_bound(my_seed, me, context, wrapped)?;
    let dek: SpaceKey = <[u8; KEY_LEN]>::try_from(dek.as_slice()).ok()?;
    aead_decrypt(&dek, &sealed.ciphertext)
}

/// Add a reader, using a device that can already read it.
///
/// The data key is recovered through `me`'s wrap and wrapped again for
/// `newcomer` — the ciphertext is never touched. Returns `false` when `me`
/// cannot read the payload, which is the only way to learn the key, and
/// therefore the only authority this operation can have.
pub fn add_device_to_sealed(
    my_seed: &[u8; 32],
    me: &DeviceId,
    context: &[&[u8]],
    sealed: &mut DeviceSealed,
    newcomer: &DeviceId,
) -> Result<bool, Failure> {
    let Some((_, wrapped)) = sealed.wraps.iter().find(|(device, _)| device == me) else {
        return Ok(false);
    };
    let Some(dek) = open_sealed_bound(my_seed, me, context, wrapped) else {
        return Ok(false);
    };
    if sealed.wraps.iter().any(|(held, _)| held == newcomer) {
        return Ok(true);
    }
    let Some(fresh) = seal_to_bound(newcomer, context, &dek)? else {
        return Ok(false);
    };
    sealed.wraps.push((newcomer.clone(), fresh));
    sealed.wraps.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(true)
}

/// Derive the box AEAD key from the DH shared secret + both public keys.
fn box_key(shared: &[u8], eph_pub: &[u8], recip_pub: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"lait/sealedbox/0");
    h.update(shared);
    h.update(eph_pub);
    h.update(recip_pub);
    *h.finalize().as_bytes()
}

/// The same box, with the caller's context mixed into the key.
///
/// [`seal_to`] binds the ephemeral key and the recipient and nothing else, so a
/// sealed blob carries no statement about what it is *for* — its meaning comes
/// entirely from where it happens to be filed, and a blob moved to the wrong
/// file opens perfectly well. Every place this kernel seals something, the
/// context matters: a space key belongs to one space and one epoch, a custody
/// package to one ceremony and one leaf, a mailbox payload to one actor.
///
/// Binding turns misfiling from a policy question into a decrypt failure. That
/// is the whole point: a check the caller can forget to write becomes one the
/// arithmetic makes for them.
///
/// The wire format is unchanged — `eph_x_pub(32) || nonce(12) || ciphertext` —
/// because the binding lives in the key rather than the bytes. So this is not a
/// new envelope generation and needs no discriminator: an envelope sealed under
/// the wrong context simply does not open, which is exactly the intended
/// behaviour and is indistinguishable from any other failed decryption.
fn bound_box_key(shared: &[u8], eph_pub: &[u8], recip_pub: &[u8], context: &[&[u8]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(BOUND_SEALED_BOX_DOMAIN);
    h.update(shared);
    h.update(eph_pub);
    h.update(recip_pub);
    // Length-framed, count first, exactly as the signature preimages in this
    // tree are framed. Concatenating the parts raw would make `["ab", "c"]` and
    // `["a", "bc"]` the same binding, so two different contexts would open each
    // other's envelopes — the binding would be decorative.
    h.update(&(context.len() as u32).to_be_bytes());
    for part in context {
        h.update(&(part.len() as u32).to_be_bytes());
        h.update(part);
    }
    *h.finalize().as_bytes()
}

/// Domain for the context-bound box. Distinct from the unbound one, so a bound
/// envelope and an unbound envelope over the same plaintext never share a key
/// and neither can be opened by the other's reader.
const BOUND_SEALED_BOX_DOMAIN: &[u8] = b"lait/sealedbox/1";

/// Seal `msg` to a member addressed by their ed25519 `DeviceId`, bound to
/// `context`. See [`bound_box_key`] for what binding buys and why the wire
/// format is unchanged. Returns `None` if the recipient key is invalid.
pub fn seal_to_bound(
    recipient: &DeviceId,
    context: &[&[u8]],
    msg: &[u8],
) -> Result<Option<Vec<u8>>, Failure> {
    let Some(recip_ed) = ed_pubkey_bytes(recipient) else {
        return Ok(None);
    };
    let Some(recip_x) = ed_pk_to_x(&recip_ed) else {
        return Ok(None);
    };
    let eph_seed = random_array()?;
    let eph = StaticSecret::from(eph_seed);
    let eph_pub = XPublic::from(&eph);
    let shared = eph.diffie_hellman(&recip_x);
    let key = bound_box_key(
        shared.as_bytes(),
        eph_pub.as_bytes(),
        recip_x.as_bytes(),
        context,
    );
    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce = random_nonce()?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), msg)
        .map_err(|_| Failure::Encryption)?;
    let mut out = Vec::with_capacity(32 + NONCE_LEN + ct.len());
    out.extend_from_slice(eph_pub.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(Some(out))
}

/// Open a context-bound sealed box addressed to us. `None` when the envelope is
/// malformed, addressed elsewhere, **or sealed under a different context** —
/// the three are deliberately indistinguishable to the caller, because a reader
/// that could tell "wrong context" from "wrong recipient" would be an oracle
/// over what a blob was for.
pub fn open_sealed_bound(
    my_seed: &[u8; 32],
    me: &DeviceId,
    context: &[&[u8]],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() < 32 + NONCE_LEN {
        return None;
    }
    let eph_pub = XPublic::from(<[u8; 32]>::try_from(&sealed[..32]).ok()?);
    let nonce = &sealed[32..32 + NONCE_LEN];
    let ct = &sealed[32 + NONCE_LEN..];
    let my_x = ed_seed_to_x(my_seed);
    let my_ed = ed_pubkey_bytes(me)?;
    let my_x_pub = ed_pk_to_x(&my_ed)?;
    let shared = my_x.diffie_hellman(&eph_pub);
    let key = bound_box_key(
        shared.as_bytes(),
        eph_pub.as_bytes(),
        my_x_pub.as_bytes(),
        context,
    );
    let cipher = ChaCha20Poly1305::new((&key).into());
    cipher.decrypt(Nonce::from_slice(nonce), ct).ok()
}

/// The key-epoch id length prefixed to every protected Body envelope.
pub const BODY_EPOCH_ID_LEN: usize = 16;
/// The fixed protected-Body envelope overhead:
/// `epoch_id(16) || nonce(12) || tag(16)` beyond the plaintext length.
pub const BODY_ENVELOPE_OVERHEAD: usize = BODY_EPOCH_ID_LEN + NONCE_LEN + 16;

/// An **opaque, non-serializable** capability authorizing Body protection under
/// one approved key epoch: the authorized epoch id plus its current key
/// material. Mechanics-side policy (the composition root, reading the
/// authorized epoch set) mints it; Replica selects it under Space policy and
/// passes it only to Engine seal/open. Engine never decides epoch legitimacy —
/// holding this capability *is* the legitimacy decision, made upstream. The
/// key material has no accessor, no serialization, and no `Debug` leak.
#[derive(Clone)]
pub struct AuthorizedBodyKey {
    epoch: [u8; BODY_EPOCH_ID_LEN],
    key: SpaceKey,
}

impl std::fmt::Debug for AuthorizedBodyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizedBodyKey")
            .field("epoch", &data_encoding::HEXLOWER.encode(&self.epoch))
            .finish_non_exhaustive()
    }
}

impl AuthorizedBodyKey {
    /// Mint the capability for an **authorized** epoch. The caller owes the
    /// authorization proof (a valid writer-signed epoch mint replayed from
    /// signed history); this constructor only packages the decision.
    pub fn for_authorized_epoch(epoch: [u8; BODY_EPOCH_ID_LEN], key: SpaceKey) -> Self {
        Self { epoch, key }
    }

    /// The authorized epoch id this capability speaks for.
    pub fn epoch_id(&self) -> &[u8; BODY_EPOCH_ID_LEN] {
        &self.epoch
    }
}

/// Seal a Body plaintext under an authorized key epoch. The persisted envelope
/// is exactly `epoch_id[16] || nonce[12] || ciphertext_and_tag` — the existing
/// construction; this completion pass introduces no new cryptography.
pub fn body_seal(key: &AuthorizedBodyKey, plaintext: &[u8]) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::with_capacity(BODY_EPOCH_ID_LEN + NONCE_LEN + plaintext.len() + 16);
    out.extend_from_slice(&key.epoch);
    out.extend_from_slice(&aead_encrypt(&key.key, plaintext)?);
    Ok(out)
}

/// Open a protected Body envelope with the capability for **its** epoch.
/// `None` when the envelope names a different epoch, the key is wrong, or the
/// blob is malformed — without the right epoch key you learn nothing.
pub fn body_open(key: &AuthorizedBodyKey, envelope: &[u8]) -> Option<Vec<u8>> {
    let (epoch, blob) = envelope.split_at_checked(BODY_EPOCH_ID_LEN)?;
    if epoch != key.epoch {
        return None;
    }
    aead_decrypt(&key.key, blob)
}

/// The epoch id a protected Body envelope names (no key required — this is the
/// lookup tag, deliberately public).
pub fn body_epoch_id(envelope: &[u8]) -> Option<[u8; BODY_EPOCH_ID_LEN]> {
    envelope.get(..BODY_EPOCH_ID_LEN)?.try_into().ok()
}

/// Domain for immutable-content chunk protection. Distinct from Body
/// protection so a chunk envelope can never be opened as a Body or the
/// reverse, even under the same epoch key.
const CONTENT_CHUNK_DOMAIN: &[u8] = b"lait/content-chunk/1";

/// The binding a content chunk is sealed under: everything about the chunk's
/// place in its content that must not be substitutable. A chunk lifted into a
/// different position or a different content fails to open — the associated
/// data is not decoration.
///
/// `content_nonce` rather than the final `ContentId` is bound, and that is
/// forced: the id contains the Merkle root over these very ciphertexts, so
/// binding it would be circular.
///
/// The content's total length and chunk count are deliberately **not** here,
/// though an earlier draft had them. They buy nothing the Merkle root does not
/// already give — the root commits the whole leaf set, each leaf commits its
/// index and ciphertext length, and the `ContentId` is the hash of the
/// descriptor carrying that root, so a re-shaped content is a different content
/// and a truncated one does not verify. What they cost is real: knowing the
/// total before sealing the first chunk makes streaming ingest impossible, and
/// ingest that must know the size up front is ingest that buffers the file.
#[derive(Debug, Clone, Copy)]
pub struct ContentChunkBinding<'a> {
    pub space: &'a str,
    pub content_nonce: &'a [u8; 16],
    pub chunk_index: u32,
}

impl ContentChunkBinding<'_> {
    /// The canonical associated data. Length-framed so no two distinct
    /// bindings share a preimage.
    fn associated_data(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(CONTENT_CHUNK_DOMAIN.len() + self.space.len() + 24);
        out.extend_from_slice(&(CONTENT_CHUNK_DOMAIN.len() as u16).to_be_bytes());
        out.extend_from_slice(CONTENT_CHUNK_DOMAIN);
        out.extend_from_slice(&(self.space.len() as u16).to_be_bytes());
        out.extend_from_slice(self.space.as_bytes());
        out.extend_from_slice(self.content_nonce);
        out.extend_from_slice(&self.chunk_index.to_be_bytes());
        out
    }
}

/// Seal one immutable-content chunk under an authorized key epoch, bound to its
/// position. The envelope is `epoch_id(16) || nonce(12) || ciphertext_and_tag`,
/// the same shape a Body envelope uses, under a different domain and with the
/// binding as associated data.
///
/// Nonces are random per chunk, so two ingests of identical bytes produce
/// unequal ciphertexts: there is no convergent encryption and no
/// plaintext-equality oracle for a relay that holds both.
pub fn content_chunk_seal(
    key: &AuthorizedBodyKey,
    binding: &ContentChunkBinding<'_>,
    plaintext: &[u8],
) -> Result<Vec<u8>, Failure> {
    let cipher = ChaCha20Poly1305::new((&key.key).into());
    let nonce = random_nonce()?;
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad: &binding.associated_data(),
            },
        )
        .map_err(|_| Failure::Encryption)?;
    let mut out = Vec::with_capacity(BODY_EPOCH_ID_LEN + NONCE_LEN + ct.len());
    out.extend_from_slice(&key.epoch);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open a content-chunk envelope with the capability for **its** epoch and the
/// binding it was sealed under. `None` when the epoch differs, the key is
/// wrong, the binding disagrees, or the blob is malformed — one answer for
/// every failure, so nothing is an oracle.
pub fn content_chunk_open(
    key: &AuthorizedBodyKey,
    binding: &ContentChunkBinding<'_>,
    envelope: &[u8],
) -> Option<Vec<u8>> {
    let (epoch, rest) = envelope.split_at_checked(BODY_EPOCH_ID_LEN)?;
    if epoch != key.epoch {
        return None;
    }
    let (nonce, ct) = rest.split_at_checked(NONCE_LEN)?;
    let cipher = ChaCha20Poly1305::new((&key.key).into());
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            chacha20poly1305::aead::Payload {
                msg: ct,
                aad: &binding.associated_data(),
            },
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aead_roundtrip_and_wrong_key_fails() {
        let k = random_key().expect("random key");
        let blob = aead_encrypt(&k, b"opaque loro export").expect("encrypt");
        assert_eq!(
            aead_decrypt(&k, &blob).as_deref(),
            Some(&b"opaque loro export"[..])
        );
        // wrong key ⇒ None (a blind relay / non-member learns nothing).
        assert!(aead_decrypt(&[0u8; 32], &blob).is_none());
        assert!(aead_decrypt(&k, b"tooshort").is_none());
    }

    #[test]
    fn did_key_is_a_deterministic_ed25519_multibase() {
        let seed = [9u8; 32];
        let device = device_from_seed(&seed);
        let did = did_key_from_device(&device).expect("a seed device is a valid ed25519 key");
        // Every ed25519 did:key begins `did:key:z6Mk` — the multibase base58btc
        // encoding of the `0xed01` multicodec prefix. This pins both the prefix
        // and the base58 alphabet.
        assert!(
            did.starts_with("did:key:z6Mk"),
            "ed25519 did:key must be z6Mk-prefixed, got {did}"
        );
        // Pure function of the key: same device → same did, every time.
        assert_eq!(did, did_key_from_device(&device).unwrap());
        // And it is a function of the key material, not the id string form.
        let other = device_from_seed(&[10u8; 32]);
        assert_ne!(did, did_key_from_device(&other).unwrap());
    }

    #[test]
    fn base58btc_matches_known_vectors() {
        // Bitcoin base58 reference vectors (leading-zero handling included).
        assert_eq!(base58btc_encode(&[0x00, 0x00, 0x01]), "112");
        assert_eq!(base58btc_encode(b"hello world"), "StV1DL6CwTryKyV");
        assert_eq!(base58btc_encode(&[]), "");
    }

    #[test]
    fn seals_to_a_seed_derived_device_and_opens() {
        // A member is addressed by their seed-derived DeviceId; the ed25519↔x25519
        // conversion must let a box sealed to it open with the seed. (The
        // agreement that the transport's key IS this ed25519 pair lives at the
        // net seam — see tests/identity_interop.rs.)
        let seed = [5u8; 32];
        let uid = device_from_seed(&seed);
        let key = random_key().expect("random key");
        let sealed = seal_to(&uid, &key)
            .expect("encrypt")
            .expect("seal to seed-derived key");
        assert_eq!(
            open_sealed(&seed, &uid, &sealed).as_deref(),
            Some(&key[..]),
            "seed-keyed sealed box must round-trip"
        );
    }

    /// Mint the golden fixtures frozen below. Ignored: it exists to be run by
    /// hand, once, and its output pasted in.
    ///
    /// It cannot be a normal test because a v1 envelope is not reproducible —
    /// the ephemeral key and the nonce are drawn from the CSPRNG, so sealing
    /// the same plaintext twice gives different bytes. That is exactly why the
    /// fixtures have to be *data* rather than something a test regenerates:
    /// once the sealing code changes, the old envelopes can never be produced
    /// again, and if none were kept there is nothing left to prove the new code
    /// can still read the old ones.
    #[test]
    #[ignore = "run by hand to mint fixtures; output is pasted into V1_GOLDEN"]
    fn mint_v1_golden_fixtures() {
        for (seed_byte, label, msg) in [
            (0x11u8, "empty", &b""[..]),
            (0x22, "space-key-sized", &[0xABu8; 32][..]),
            (0x33, "multi-block", &[0x5Au8; 200][..]),
        ] {
            let seed = [seed_byte; 32];
            let device = device_from_seed(&seed);
            let sealed = seal_to(&device, msg).unwrap().unwrap();
            println!(
                "(\n    // {label}\n    [0x{seed_byte:02x}; 32],\n    \"{}\",\n    \"{}\",\n),",
                hex_of(msg),
                hex_of(&sealed)
            );
        }
    }

    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("fixture hex"))
            .collect()
    }

    /// Real v1 sealed boxes, minted by [`mint_v1_golden_fixtures`] against the
    /// sealing code as it stood before any HPKE work, and frozen here.
    ///
    /// `(recipient seed, plaintext hex, sealed hex)`.
    ///
    /// These are durable-format evidence, not test data. Sealed epoch keys live
    /// as long as the Space that minted them, so whatever replaces `seal_to`
    /// has to keep opening bytes shaped like these forever. Do not regenerate
    /// them, do not "fix" them to match new output, and do not delete one
    /// because a new construction cannot read it — that last case is the
    /// finding, not the problem with the fixture.
    const V1_GOLDEN: &[([u8; 32], &str, &str)] = &[
        // empty plaintext: the ciphertext is nothing but the tag, which is the
        // shape most likely to be mishandled by a length check.
        (
            [0x11; 32],
            "",
            "191a2510565613a22f2fd42c2cf913762afe9faff13f7ab3bde5d44fbae5c442881853ad91c918c9a07da0bfdf6350034afc8b7ca20c06c075279a4c",
        ),
        // 32 bytes: a space key, which is what the dominant call site seals.
        (
            [0x22; 32],
            "abababababababababababababababababababababababababababababababab",
            "a4a4d4ef9db07ce55174fd2fede5a5f1b51f2141a2ff575ad0cacd442ae140625cd5d1a972deb92531a2aad33f0f6cb0d9e80afe2e1366314b87f45fc655290882d84525717fe07a5e7256dfacd1a80bde31758fb385cbbbbe240f33",
        ),
        // 200 bytes: spans more than one ChaCha20 block.
        (
            [0x33; 32],
            "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            "67a24b2e09de50649422c39cbde8d314a51b31045a2c1b8385b95144d27bdb36053ec9daef79bfeb60b02175d3166649b3d95f784769b974d3d93c0fe3f7d9a02392c40f96c496adc70b61e4f3b7d122d780e89fb9924e74c6a650a82e12e221e2c5cfa738406ab2c93c34b5033c76cf5ab078bb2aaa2c2d48dbce00830524db7875c278c6aca04ec578e08af8566a22e24088a5390a2ae87ad09fcb930a8ea794da337b30b1b800f33e9192568b20512e9d3f107afd49134fe9e3782ff505ab2697b6ddf196a4b66674fd339e73e58ad2de805ee77eae1226cc6886b22816c77285735a77aadb5d43cec56c3f2887700144d3ffb0ac214abe9efaa74d002daddbe40f18",
        ),
    ];

    /// Every frozen v1 envelope still opens, and yields exactly the plaintext
    /// it was sealed over.
    ///
    /// This is the guard the whole HPKE migration rests on: v2 cannot be
    /// self-describing in-band, because a v1 envelope opens with a uniformly
    /// random byte and any version tag collides with roughly one in 256 of
    /// them. So the version has to ride the record that holds the blob, and the
    /// v1 path has to stay live and correct while it does.
    #[test]
    fn every_frozen_v1_envelope_still_opens() {
        assert!(
            !V1_GOLDEN.is_empty(),
            "the v1 fixtures are the only evidence that old envelopes remain \
             readable; an empty set silently proves nothing"
        );
        for (seed, plaintext_hex, sealed_hex) in V1_GOLDEN {
            let device = device_from_seed(seed);
            let opened = open_sealed(seed, &device, &unhex(sealed_hex));
            assert_eq!(
                opened.as_deref(),
                Some(&unhex(plaintext_hex)[..]),
                "a frozen v1 envelope stopped opening"
            );
        }
    }

    /// The property the binding exists for: the same recipient, the same
    /// plaintext, a different context — and it does not open.
    #[test]
    fn a_bound_envelope_opens_only_under_the_context_it_was_sealed_for() {
        let seed = [21u8; 32];
        let me = device_from_seed(&seed);
        let space = b"ws_example";
        let epoch_three = 3u32.to_be_bytes();
        let epoch_four = 4u32.to_be_bytes();

        let sealed = seal_to_bound(&me, &[space, &epoch_three], b"the space key")
            .expect("encrypt")
            .expect("valid recipient");

        assert_eq!(
            open_sealed_bound(&seed, &me, &[space, &epoch_three], &sealed).as_deref(),
            Some(&b"the space key"[..]),
            "the sealing context must open it"
        );

        // The misfiling this exists to catch: right space, wrong epoch.
        assert!(
            open_sealed_bound(&seed, &me, &[space, &epoch_four], &sealed).is_none(),
            "an envelope from another epoch must not open"
        );
        // And right epoch, wrong space.
        assert!(
            open_sealed_bound(&seed, &me, &[b"ws_other", &epoch_three], &sealed).is_none(),
            "an envelope from another space must not open"
        );
        // A context of a different shape is a different context, not a prefix
        // match.
        assert!(
            open_sealed_bound(&seed, &me, &[space], &sealed).is_none(),
            "dropping a context element must not open it"
        );
    }

    /// Framing is load-bearing, not tidiness.
    ///
    /// With the parts concatenated raw, `["ab", "c"]` and `["a", "bc"]` hash
    /// identically, so two unrelated contexts would open each other's envelopes
    /// and the binding would be decoration. This is the test that fails if the
    /// length framing is ever removed as redundant.
    #[test]
    fn contexts_that_concatenate_alike_are_still_different_bindings() {
        let seed = [22u8; 32];
        let me = device_from_seed(&seed);
        let sealed = seal_to_bound(&me, &[b"ab", b"c"], b"payload")
            .expect("encrypt")
            .expect("valid recipient");
        assert!(open_sealed_bound(&seed, &me, &[b"ab", b"c"], &sealed).is_some());
        assert!(
            open_sealed_bound(&seed, &me, &[b"a", b"bc"], &sealed).is_none(),
            "the framing must distinguish contexts that concatenate to the same bytes"
        );
    }

    /// Bound and unbound envelopes never cross, even with an empty context,
    /// because the domains differ. A bound reader must not be satisfiable by a
    /// legacy envelope, or the binding could be stripped by re-filing.
    #[test]
    fn a_bound_envelope_and_an_unbound_one_never_open_each_other() {
        let seed = [23u8; 32];
        let me = device_from_seed(&seed);
        let msg = b"same plaintext";

        let bound = seal_to_bound(&me, &[], msg).unwrap().unwrap();
        let unbound = seal_to(&me, msg).unwrap().unwrap();

        assert!(open_sealed_bound(&seed, &me, &[], &bound).is_some());
        assert!(open_sealed(&seed, &me, &unbound).is_some());

        assert!(
            open_sealed(&seed, &me, &bound).is_none(),
            "the unbound reader must not open a bound envelope"
        );
        assert!(
            open_sealed_bound(&seed, &me, &[], &unbound).is_none(),
            "the bound reader must not open an unbound envelope"
        );
    }

    #[test]
    fn a_bound_envelope_still_refuses_the_wrong_recipient() {
        let seed = [24u8; 32];
        let me = device_from_seed(&seed);
        let context: &[&[u8]] = &[b"ws_example"];
        let sealed = seal_to_bound(&me, context, b"secret").unwrap().unwrap();

        let other_seed = [25u8; 32];
        let other = device_from_seed(&other_seed);
        assert!(
            open_sealed_bound(&other_seed, &other, context, &sealed).is_none(),
            "the right context must not rescue the wrong recipient"
        );
    }

    /// Every device a payload was sealed to reads it, and nothing else does.
    #[test]
    fn every_device_in_the_set_reads_the_payload_and_no_other_does() {
        let seeds = [[31u8; 32], [32u8; 32], [33u8; 32]];
        let devices: Vec<_> = seeds.iter().map(device_from_seed).collect();
        let context: &[&[u8]] = &[b"act_example", b"ws_example"];

        let sealed = seal_to_devices(&devices, context, b"a message").expect("seal");
        assert_eq!(sealed.wraps.len(), 3, "one wrap per device");

        for (seed, device) in seeds.iter().zip(&devices) {
            assert_eq!(
                open_as_device(seed, device, context, &sealed).as_deref(),
                Some(&b"a message"[..]),
                "each device in the set reads the same payload"
            );
        }

        let stranger_seed = [34u8; 32];
        let stranger = device_from_seed(&stranger_seed);
        assert!(
            open_as_device(&stranger_seed, &stranger, context, &sealed).is_none(),
            "a device with no wrap holds nothing"
        );
    }

    /// The property the whole shape exists for: gaining a device does not
    /// re-encrypt what was already addressed to you.
    ///
    /// If it did, adding a phone would be an O(mailbox) rewrite, and every
    /// correspondent's copy of those bytes would move.
    #[test]
    fn adding_a_device_adds_an_unlock_path_and_re_encrypts_nothing() {
        let seed = [41u8; 32];
        let me = device_from_seed(&seed);
        let context: &[&[u8]] = &[b"act_example"];

        let mut sealed = seal_to_devices(&[me.clone()], context, b"kept").expect("seal");
        let ciphertext_before = sealed.ciphertext.clone();
        let wrap_before = sealed.wraps.clone();

        let phone_seed = [42u8; 32];
        let phone = device_from_seed(&phone_seed);
        assert!(
            add_device_to_sealed(&seed, &me, context, &mut sealed, &phone).expect("add"),
            "a device that can read may add another"
        );

        assert_eq!(
            sealed.ciphertext, ciphertext_before,
            "the payload must not be re-encrypted"
        );
        assert_eq!(sealed.wraps.len(), 2, "exactly one wrap was added");
        assert!(
            wrap_before.iter().all(|old| sealed.wraps.contains(old)),
            "existing wraps are untouched"
        );
        assert_eq!(
            open_as_device(&phone_seed, &phone, context, &sealed).as_deref(),
            Some(&b"kept"[..]),
            "the new device reads it"
        );
        assert_eq!(
            open_as_device(&seed, &me, context, &sealed).as_deref(),
            Some(&b"kept"[..]),
            "and the old one still does"
        );
    }

    /// Only a device that can already read may extend the reader set. Knowing
    /// the ciphertext is not authority over it.
    #[test]
    fn a_device_that_cannot_read_cannot_add_a_reader() {
        let seed = [51u8; 32];
        let me = device_from_seed(&seed);
        let context: &[&[u8]] = &[b"act_example"];
        let mut sealed = seal_to_devices(&[me], context, b"secret").expect("seal");

        let outsider_seed = [52u8; 32];
        let outsider = device_from_seed(&outsider_seed);
        let accomplice = device_from_seed(&[53u8; 32]);
        assert!(
            !add_device_to_sealed(&outsider_seed, &outsider, context, &mut sealed, &accomplice)
                .expect("add"),
            "an outsider must not be able to enrol anybody"
        );
        assert_eq!(sealed.wraps.len(), 1, "the reader set is unchanged");
    }

    /// The binding reaches the wraps, not merely the payload: the whole item is
    /// unreadable under the wrong context.
    #[test]
    fn a_sealed_payload_is_unreadable_under_a_different_context() {
        let seed = [61u8; 32];
        let me = device_from_seed(&seed);
        let sealed = seal_to_devices(&[me.clone()], &[b"act_one"], b"private").expect("seal");
        assert!(
            open_as_device(&seed, &me, &[b"act_two"], &sealed).is_none(),
            "another actor's context must not open it"
        );
    }

    #[test]
    fn a_repeated_device_is_wrapped_once_and_the_order_is_stable() {
        let seed = [71u8; 32];
        let me = device_from_seed(&seed);
        let other = device_from_seed(&[72u8; 32]);
        let context: &[&[u8]] = &[b"act_example"];

        let sealed = seal_to_devices(
            &[other.clone(), me.clone(), other.clone()],
            context,
            b"once",
        )
        .expect("seal");
        assert_eq!(sealed.wraps.len(), 2, "a duplicate device collapses");

        let devices: Vec<_> = sealed.wraps.iter().map(|(d, _)| d.clone()).collect();
        let mut sorted = devices.clone();
        sorted.sort();
        assert_eq!(devices, sorted, "wraps are sorted, so encoding is stable");
    }

    #[test]
    fn sealed_box_only_opens_for_recipient() {
        let seed = [7u8; 32];
        let me = device_from_seed(&seed);
        let key = random_key().expect("random key");
        let sealed = seal_to(&me, &key)
            .expect("encrypt")
            .expect("valid recipient");
        assert_eq!(open_sealed(&seed, &me, &sealed).as_deref(), Some(&key[..]));
        // a different member cannot open it.
        let other_seed = [9u8; 32];
        let other = device_from_seed(&other_seed);
        assert!(open_sealed(&other_seed, &other, &sealed).is_none());
    }
}
