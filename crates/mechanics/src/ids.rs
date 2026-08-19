//! Identifiers. Every id is exactly one kind of thing with one
//! stability guarantee. App-minted ids are `<prefix>_<ULID>`: a ULID is a
//! 128-bit, lexicographically-sortable, time-ordered identifier rendered in
//! Crockford base32 (26 chars), so ids sort by creation time and never collide
//! in practice. These are **content-independent**: an id is minted once and is
//! permanent, never derived from document or session internals.
//!
//! `DeviceId` is an ed25519 public key — the same bytes as the iroh `EndpointId`.
//! Since the `lait/actor/1` cutover it identifies a **device**, not a person: a
//! member is an [`ActorId`] over a set of device keys, so one human holds many
//! `DeviceId`s and rotates them under a stable identity. Read a `DeviceId` as "which
//! device", an `ActorId` as "who".

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// A monotonic-ish clock + randomness source for minting ULIDs. Injected so
/// tests are deterministic and never flake on wall-clock/RNG (per the plan's
/// "inject clocks/seeds" rule).
pub trait UlidSource {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
    /// 80 bits of randomness for the ULID's entropy section.
    fn rand80(&self) -> u128;
}

/// Production source: real wall clock + `getrandom`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemUlidSource;

impl UlidSource for SystemUlidSource {
    fn now_ms(&self) -> u64 {
        // Through `wallclock` so a frozen clock reaches ULID minting too. A
        // test that wants deterministic ids should still supply its own
        // `UlidSource` — that seam is narrower and does not touch a global —
        // but a test freezing the wall clock for other reasons should not find
        // identifiers still marching forward underneath it.
        crate::wallclock::now_millis()
    }
    fn rand80(&self) -> u128 {
        static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut buf = [0u8; 10];
        if let Err(error) = getrandom::fill(&mut buf) {
            let counter = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"lait/ulid-entropy-fallback/1");
            hasher.update(&self.now_ms().to_le_bytes());
            hasher.update(&u64::from(std::process::id()).to_le_bytes());
            hasher.update(&counter.to_le_bytes());
            let digest = hasher.finalize();
            if let Some(prefix) = digest.as_bytes().get(..buf.len()) {
                buf.copy_from_slice(prefix);
            }
            tracing::warn!(%error, "OS randomness unavailable; using process-local ULID entropy");
        }
        let mut v: u128 = 0;
        for b in buf {
            v = (v << 8) | u128::from(b);
        }
        v
    }
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";

/// Whether `s` is a well-formed 26-char Crockford-base32 ULID.
pub fn valid_ulid(s: &str) -> bool {
    s.len() == 26
        && s.bytes()
            .all(|b| CROCKFORD.contains(&b.to_ascii_uppercase()))
}

/// Render a 128-bit value as a 26-char Crockford base32 ULID string.
#[doc(hidden)]
pub fn encode_ulid(value: u128) -> String {
    // 128 bits → 26 base32 chars (the top char encodes only 2 bits).
    let mut out = [0u8; 26];
    let mut v = value;
    for i in (0..26).rev() {
        let alphabet_index = usize::try_from(v & 0x1f).unwrap_or(0);
        let Some(encoded) = CROCKFORD.get(alphabet_index).copied() else {
            return String::new();
        };
        let Some(target) = out.get_mut(i) else {
            return String::new();
        };
        *target = encoded;
        v >>= 5;
    }
    out.into_iter().map(char::from).collect()
}

/// Mint a fresh ULID string from a source: 48-bit ms timestamp + 80-bit random.
pub fn mint_ulid(src: &dyn UlidSource) -> String {
    let ts = u128::from(src.now_ms()) & ((1u128 << 48) - 1);
    let rand = src.rand80() & ((1u128 << 80) - 1);
    encode_ulid((ts << 80) | rand)
}

/// Declare a newtype id `$name` with textual prefix `$prefix` (e.g. `ws_`).
/// Exported so the product crate declares its own ids with the identical
/// grammar, minting, and display semantics.
#[macro_export]
macro_rules! prefixed_id {
    ($(#[$m:meta])* $name:ident, $prefix:literal) => {
        $(#[$m])*
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            ::serde::Serialize,
            ::serde::Deserialize,
        )]
        pub struct $name(String);

        impl $name {
            /// The textual prefix these ids carry (including the underscore).
            pub const PREFIX: &'static str = $prefix;

            /// Mint a fresh id: `<prefix><ULID>`.
            pub fn mint(src: &dyn $crate::ids::UlidSource) -> Self {
                Self(format!("{}{}", $prefix, $crate::ids::mint_ulid(src)))
            }

            /// Deterministically derive an id from a domain-separated 128-bit
            /// digest. Product batch planners use this for request+ordinal
            /// identities whose retry must name the same entity.
            pub fn from_digest(digest: [u8; 16]) -> Self {
                Self(format!("{}{}", $prefix, $crate::ids::encode_ulid(u128::from_be_bytes(digest))))
            }

            /// Wrap an existing string, validating the prefix + ULID shape.
            pub fn parse(s: &str) -> Option<Self> {
                let rest = s.strip_prefix($prefix)?;
                if $crate::ids::valid_ulid(rest) {
                    Some(Self(s.to_string()))
                } else {
                    None
                }
            }

            /// The full id string, prefix included.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// A short, git-style prefix of the id (prefix + first `n` ULID
            /// chars) — the canonical human handle. `n` counts
            /// ULID characters after the textual prefix.
            pub fn short(&self, n: usize) -> String {
                let ulid = self.0.strip_prefix($prefix).unwrap_or_default();
                let short: String = ulid.chars().take(n).collect();
                format!("{}{}", $prefix, short)
            }

            /// The bare ULID portion (no textual prefix).
            pub fn ulid(&self) -> &str {
                self.0.strip_prefix($prefix).unwrap_or_default()
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
    };
}

prefixed_id!(
    /// Space id — minted at `space init` and committed by genesis.
    SpaceId, "ws_"
);

/// A **device** id — an ed25519 public key, hex-encoded (64 lowercase hex
/// chars), the same bytes as the iroh `EndpointId`. Kept as a validated string
/// so Layer B can carry it without depending on iroh types.
///
/// Not a member id: membership is keyed on [`ActorId`], and a device speaks
/// *for* an actor only while the actor's key-event log binds it. Use this for
/// transport peers, signature authors, and `committedBy` stamps — never to
/// answer "who did this".
/// # Equality is by key, not by spelling
///
/// `Eq`, `Ord` and `Hash` compare the ASCII-lowercase fold, so `AABB…` and
/// `aabb…` are **one** `DeviceId`. That is not a convenience — it is the only
/// way the type's two roles can agree.
///
/// A device id is simultaneously a *value* (32 bytes of ed25519 public key,
/// compared by decoding) and a *name* (a `String`, compared by `Ord`). Every
/// decoder here is `HEXLOWER_PERMISSIVE`, so both spellings are one key
/// cryptographically; with derived `Eq` they were two members of a
/// `BTreeSet<DeviceId>`. The two comparisons disagreed, and the disagreement was
/// reachable: a bound device could author a binding for its own shouted
/// spelling, and a later `RevokeDevice` naming the canonical id — the only
/// spelling any surface displays, since every display path derives from key
/// bytes — removed one member and left the other. **A revoked device kept its
/// standing**, and `device_speaks_for` in `acl` still authorized its ops.
/// CWE-178, with the remedy CWE-180 prescribes.
///
/// Folding here rather than rejecting non-canonical input is deliberate, and the
/// reasoning is worth keeping. Rejection would have to happen at deserialization
/// or at signature verification, and `as_str` is inside every signing payload
/// (`sigdag::signing_payload`, `actor::consent_payload`) — so normalising a
/// spelling would change the bytes a stored signature covers, a stored node
/// would stop verifying, and `Authority::open` re-verifies every stored effect
/// and answers `Failure::corrupt`. Unlike a checkpoint, which is downgraded to a
/// cache miss precisely so a layout change cannot brick a store, an effect gets
/// no such grace. So: fold, never rewrite. No signature moves, no hash moves, no
/// serialized byte moves, and an honest history — every production constructor
/// emits `HEXLOWER` — is bit-identical.
///
/// The wire is a different layer with a different answer: a boundary that
/// *receives* an id should refuse a non-canonical spelling outright rather than
/// accept a second name for one key (see `lait-post`'s `device_key`). Replay
/// cannot, because it does not get to rewrite the past.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceId(String);

impl PartialEq for DeviceId {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Eq for DeviceId {}

impl PartialOrd for DeviceId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeviceId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .bytes()
            .map(|b| b.to_ascii_lowercase())
            .cmp(other.0.bytes().map(|b| b.to_ascii_lowercase()))
    }
}

impl std::hash::Hash for DeviceId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Byte-wise so no allocation, terminated so a tuple key cannot be
        // ambiguous. Only consistency with `eq` matters, not agreeing with
        // `str`'s own hash.
        for b in self.0.bytes() {
            state.write_u8(b.to_ascii_lowercase());
        }
        state.write_u8(0xff);
    }
}

impl DeviceId {
    /// Parse a 64-char lowercase-hex ed25519 public key.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(Self(s.to_ascii_lowercase()))
        } else {
            None
        }
    }

    /// Wrap a key string that is already known valid (e.g. an iroh
    /// `EndpointId`, 64-hex by construction). **Validates nothing** — the
    /// caller vouches for the shape.
    ///
    /// Only use this where the value's provenance guarantees a device key. It
    /// is not a parser: reaching for it on a string read back out of a document
    /// launders whatever is there into a `DeviceId`, and post-cutover those
    /// strings are often `ActorId`s — a type lie that then mis-attributes
    /// silently downstream. Use [`DeviceId::parse`] there instead.
    pub fn from_key_string(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A short, display-friendly prefix (first 8 hex chars).
    pub fn short(&self) -> String {
        self.0.chars().take(8).collect()
    }

    /// The raw 32 bytes of the ed25519 public key this id *is*, or `None` when
    /// the value is not one. Fallible because [`DeviceId::from_key_string`]
    /// validates nothing.
    ///
    /// This is the compact form: a device key is 32 bytes, and any wire that
    /// spells it as 64 hex characters pays double for the same identity.
    pub fn key_bytes(&self) -> Option<[u8; 32]> {
        let raw = data_encoding::HEXLOWER_PERMISSIVE
            .decode(self.0.as_bytes())
            .ok()?;
        <[u8; 32]>::try_from(raw.as_slice()).ok()
    }

    /// The device id of an ed25519 public key's raw bytes — the inverse of
    /// [`DeviceId::key_bytes`].
    pub fn from_key_bytes(raw: &[u8; 32]) -> Self {
        Self(data_encoding::HEXLOWER.encode(raw))
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An actor id — the **self-certifying** identity of a member (`lait/actor/1`):
/// `act_` + the blake3 content-address of the actor's `Incept` event, 64
/// lowercase hex chars. An actor is a *set of device keys under one
/// self-managed key-event log*; a `DeviceId` (device key) signs, an `ActorId`
/// *is someone*. Not an ed25519 key — it never verifies a signature — and
/// content-independent of any device key, so devices rotate under a stable
/// identity. Minted per-space (the `Incept` payload binds the space id
/// + a nonce), so the same human is unlinkable across spaces by default.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ActorId(String);

impl ActorId {
    /// The textual prefix these ids carry (including the underscore).
    pub const PREFIX: &'static str = "act_";

    /// Parse `act_` + 64 lowercase hex chars.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let rest = s.strip_prefix(Self::PREFIX)?;
        if rest.len() == 64 && rest.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(Self(format!(
                "{}{}",
                Self::PREFIX,
                rest.to_ascii_lowercase()
            )))
        } else {
            None
        }
    }

    /// Wrap the content-address of an `Incept` event (a 64-hex blake3 string,
    /// as produced by `SignedNode::hash`). The caller vouches the hash shape.
    pub fn from_incept_hash(hash: &str) -> Self {
        Self(format!("{}{}", Self::PREFIX, hash))
    }

    /// The bare incept-event hash (no textual prefix).
    pub fn incept_hash(&self) -> &str {
        self.0.strip_prefix(Self::PREFIX).unwrap_or_default()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A short, display-friendly handle: `act_` + first 8 hash chars.
    pub fn short(&self) -> String {
        self.0
            .chars()
            .take(Self::PREFIX.len().saturating_add(8))
            .collect()
    }
}

impl fmt::Display for ActorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Two spellings of one ed25519 key are one `DeviceId` — in a set, in a map,
    /// and under every comparison a collection uses.
    ///
    /// The three traits are checked together on purpose. A `BTreeSet` dedupes by
    /// `Ord` and a `HashMap` by `Hash` + `Eq`, so fixing `eq` alone would leave a
    /// set that still held two members and a map that still had two entries — the
    /// revocation bypass would survive in a different container.
    #[test]
    fn one_key_is_one_device_however_it_is_spelled() {
        let canonical = DeviceId::from_key_bytes(&[0xab; 32]);
        let shouted = DeviceId::from_key_string(canonical.as_str().to_ascii_uppercase());

        // The premise: same key, different string. Without this the test proves
        // nothing.
        assert_eq!(canonical.key_bytes(), shouted.key_bytes());
        assert_ne!(canonical.as_str(), shouted.as_str());

        assert_eq!(canonical, shouted, "equality is by key, not by spelling");
        assert_eq!(
            canonical.cmp(&shouted),
            std::cmp::Ordering::Equal,
            "Ord must agree with Eq or a BTreeSet holds both"
        );

        let mut set = std::collections::BTreeSet::new();
        set.insert(canonical.clone());
        assert!(
            !set.insert(shouted.clone()),
            "the second spelling is not new"
        );
        assert_eq!(set.len(), 1);
        assert!(
            set.remove(&shouted),
            "removing either spelling removes the key"
        );
        assert!(set.is_empty(), "…and leaves nothing behind");

        let mut map = std::collections::HashMap::new();
        map.insert(canonical.clone(), "first");
        map.insert(shouted.clone(), "second");
        assert_eq!(
            map.len(),
            1,
            "Hash must agree with Eq or a HashMap holds both"
        );
        assert_eq!(map.get(&canonical), Some(&"second"));

        // Serialization is untouched: the raw spelling round-trips, because
        // `as_str` is inside signing payloads and normalising it would move bytes
        // a stored signature covers.
        let json = serde_json::to_string(&shouted).expect("serialize");
        assert!(
            json.contains("AB"),
            "the stored spelling is preserved: {json}"
        );
        let back: DeviceId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.as_str(), shouted.as_str());
    }

    /// A fully deterministic source: fixed clock, counter entropy.
    struct FakeSource {
        ms: Cell<u64>,
        ctr: Cell<u128>,
    }
    impl FakeSource {
        fn new(ms: u64) -> Self {
            Self {
                ms: Cell::new(ms),
                ctr: Cell::new(0),
            }
        }
    }
    impl UlidSource for FakeSource {
        fn now_ms(&self) -> u64 {
            self.ms.get()
        }
        fn rand80(&self) -> u128 {
            let v = self.ctr.get();
            self.ctr.set(v + 1);
            v
        }
    }

    #[test]
    fn ulid_is_26_crockford_chars() {
        let s = FakeSource::new(1_700_000_000_000);
        let u = mint_ulid(&s);
        assert_eq!(u.len(), 26, "ULID is 26 chars");
        assert!(
            u.bytes().all(|b| CROCKFORD.contains(&b)),
            "crockford alphabet"
        );
    }

    #[test]
    fn prefixed_ids_roundtrip_sort_and_shorten() {
        // The macro-declared grammar, exercised through SpaceId (the one
        // prefixed id the kernel itself owns): parse round-trip, prefix
        // enforcement, time-ordering, and the short-handle contract.
        let s = FakeSource::new(1_700_000_000_000);
        let id = SpaceId::mint(&s);
        assert!(id.as_str().starts_with("ws_"));
        assert_eq!(SpaceId::parse(id.as_str()), Some(id.clone()));
        assert_eq!(SpaceId::parse("ws_short"), None, "bad ULID length rejected");
        assert_eq!(
            SpaceId::parse("xx_00000000000000000000000000"),
            None,
            "wrong prefix rejected"
        );
        let early = FakeSource::new(1_000);
        let late = FakeSource::new(2_000);
        let a = SpaceId::mint(&early);
        let b = SpaceId::mint(&late);
        assert!(a < b, "earlier ULID sorts before later: {a} !< {b}");
        let short = id.short(3);
        assert_eq!(short.len(), "ws_".len() + 3);
        assert!(
            id.as_str().starts_with(&short),
            "short is a genuine prefix of the full id"
        );
    }

    #[test]
    fn device_id_validates_ed25519_hex() {
        let key = "a".repeat(64);
        assert!(DeviceId::parse(&key).is_some());
        assert!(DeviceId::parse("tooshort").is_none());
        assert!(
            DeviceId::parse(&"g".repeat(64)).is_none(),
            "non-hex rejected"
        );
        assert_eq!(DeviceId::parse(&key).unwrap().short().len(), 8);
    }
}
