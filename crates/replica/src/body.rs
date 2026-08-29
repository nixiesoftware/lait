//! Body schemas, operations, and descriptors.
//!
//! A Body is a durable addressable World entity. A World declares its
//! [`Schema`]s and stages [`Op`]s; Replica validates them, mechanics
//! adjudicates authority, and Engine makes them durable. The operation algebra
//! is LAIT semantics — **not** a copy of the CRDT engine's API — and is frozen as an S1
//! fixture and implemented through Engine in S5. This module defines the sealed
//! contract shapes; S0 introduces no production routing.

use serde::{Deserialize, Serialize};

pub use crate::ids::{
    served_world, BodyId, BodyKey, EncodingId, SchemaId, WorldId, SERVED_WORLD_VAR,
};
pub use crate::protected::{
    BodyKeySource, StaticBodyKeys, MAX_BODY_BYTES, MAX_PROTECTED_PLAINTEXT, MUTATION_ATOMIC,
    MUTATION_COLLABORATIVE, MUTATION_IMMUTABLE_ATOMIC,
};
pub use crate::replica::{BodyBinding, QuotaConfig, SupportedSchemas};
pub use fabric::Material;

pub use crate::transaction::Descriptor;

/// Why a Body-owned operation could not be completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The operating system could not provide entropy for a new Body identity.
    Randomness,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Failure {}

/// Domain separator for the ciphertext-only content commitment.
pub const BODY_CONTENT_DOMAIN: &[u8] = b"lait/body-content/1";

/// Domain separator for a create-once atomic Body's content-derived identity.
///
/// The schema coordinate is part of the preimage, so the same canonical bytes
/// in two schemas are different objects. Length-prefixing makes the tuple
/// encoding unambiguous and independently reproducible by every peer.
pub const IMMUTABLE_BODY_ID_DOMAIN: &[u8] = b"lait/immutable-body-id/1";

/// Derive the only valid address for a create-once atomic value.
///
/// Unlike an ordinary [`BodyId`], this id is not random. That is the
/// convergence invariant for [`MutationModel::ImmutableAtomic`]: two different
/// canonical values cannot compete for one address and therefore can never be
/// reduced by arrival-order or last-writer-wins policy. Every live, recovery,
/// and peer path recomputes this value before admitting the Body.
pub fn immutable_body_id(
    world: &WorldId,
    schema: &SchemaId,
    schema_version: u32,
    encoding: &EncodingId,
    canonical_value: &[u8],
) -> BodyId {
    fn field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
        hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(bytes);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(IMMUTABLE_BODY_ID_DOMAIN);
    field(&mut hasher, world.as_bytes());
    field(&mut hasher, schema.as_bytes());
    hasher.update(&schema_version.to_be_bytes());
    field(&mut hasher, encoding.as_bytes());
    field(&mut hasher, canonical_value);
    let mut id = [0u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    BodyId::from_bytes(id)
}

/// Construct the canonical key for a create-once atomic value.
pub fn immutable_body_key(
    world: &WorldId,
    schema: &SchemaId,
    schema_version: u32,
    encoding: &EncodingId,
    canonical_value: &[u8],
) -> BodyKey {
    BodyKey::new(
        world.clone(),
        immutable_body_id(world, schema, schema_version, encoding, canonical_value),
    )
}

/// A commitment to a Body's protected payload: `BLAKE3(BODY_CONTENT_DOMAIN ||
/// protected_payload)`. It commits to the **ciphertext**, never the plaintext,
/// so it is not an equality oracle over decrypted content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentCommitment([u8; 32]);

impl ContentCommitment {
    /// Compute the commitment over an already-protected (encrypted) payload.
    pub fn over_protected_payload(protected_payload: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(BODY_CONTENT_DOMAIN);
        h.update(protected_payload);
        Self(*h.finalize().as_bytes())
    }

    pub fn from_bytes(raw: [u8; 32]) -> Self {
        Self(raw)
    }

    pub fn as_bytes(&self) -> [u8; 32] {
        self.0
    }
}

/// How a schema's Bodies mutate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationModel {
    /// The Body carries a single canonical value replaced atomically per
    /// transaction.
    Atomic,
    /// A single canonical value whose Body address is derived from the schema
    /// coordinate and value. It may be created or replayed identically, but it
    /// can never be replaced or tombstoned under the same address.
    ImmutableAtomic,
    /// The Body uses the versioned LAIT collaborative algebra.
    Collaborative(CollaborativeSchema),
}

/// The collaborative-schema declaration for a Body. The concrete path grammar,
/// stable element identity, concurrency winners, idempotence, type conflicts,
/// limits, and upgrade behavior are frozen as an S1 fixture; S0 reserves the
/// shape so registration and descriptors can name it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollaborativeSchema {
    /// Maximum encoded size, in bytes, of a single collaborative Body. `0` means
    /// "use the Replica default"; real limits are frozen in S1.
    pub max_encoded_bytes: u64,
}

/// A World's declaration of one Body schema it supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    pub id: SchemaId,
    pub version: u32,
    pub encoding: EncodingId,
    pub mutation: MutationModel,
    /// Earlier schema versions this version can read (deterministic upgrade
    /// declaration). Runtime rejects contradictory upgrade claims.
    pub readable_predecessors: Vec<u32>,
}

/// The LAIT-owned Body operation algebra. A World stages these; it cannot submit
/// raw CRDT updates or container ids. Stable element ids, paths, concurrency,
/// idempotency, limits, and errors are LAIT semantics. This enum is the sealed
/// S0 shape; the exact path grammar and element-identity rules are frozen as an
/// S1 fixture and implemented through Engine in S5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// Atomic replacement of a Body's canonical value.
    ReplaceAtomic {
        value: Vec<u8>,
    },
    /// Register set / clear.
    RegisterSet {
        path: String,
        value: Vec<u8>,
    },
    RegisterClear {
        path: String,
    },
    /// Map entry set / remove.
    MapSet {
        path: String,
        key: String,
        value: Vec<u8>,
    },
    MapRemove {
        path: String,
        key: String,
    },
    /// Ordered-list insert / remove / move with stable element identity.
    ListInsert {
        path: String,
        index: u64,
        value: Vec<u8>,
    },
    ListRemove {
        path: String,
        element: String,
    },
    ListMove {
        path: String,
        element: String,
        index: u64,
    },
    /// Text splice with declared coordinate semantics.
    TextSplice {
        path: String,
        index: u64,
        delete: u64,
        insert: String,
    },
    /// Set add / remove.
    SetAdd {
        path: String,
        value: Vec<u8>,
    },
    SetRemove {
        path: String,
        value: Vec<u8>,
    },
    /// Counter increment.
    CounterAdd {
        path: String,
        delta: i64,
    },
    /// Body create / tombstone (when the schema allows it).
    Create,
    Tombstone,
    /// Movable-hierarchy insert / move / remove with stable node identity, and
    /// per-node data entries. `parent: None` names a root of the forest;
    /// `after` names the sibling to follow and must be a child of `parent`.
    ///
    /// Appended after `Tombstone` rather than filed beside the other sequence
    /// operations: the postcard encoding is a frozen S1a fixture keyed by
    /// variant index, so where a variant sits is a wire fact, not a taste.
    TreeInsert {
        path: String,
        parent: Option<String>,
        after: Option<String>,
        value: Vec<u8>,
    },
    TreeMove {
        path: String,
        node: String,
        parent: Option<String>,
        after: Option<String>,
    },
    TreeRemove {
        path: String,
        node: String,
    },
    TreeSet {
        path: String,
        node: String,
        key: String,
        value: Vec<u8>,
    },
    TreeUnset {
        path: String,
        node: String,
        key: String,
    },
    /// Place the node carrying application anchor `anchor` under the one
    /// carrying `parent`, creating either if absent. The addressing mode for a
    /// hierarchy over records that have their own ids: idempotent, needs no
    /// prior read, and expressible before either node exists — which the
    /// node-id form is not, since a batch cannot name a node it is creating.
    TreeAnchor {
        path: String,
        anchor: String,
        parent: Option<String>,
    },
    /// Append to a log, keeping at most `retain` entries in state. The type for
    /// a feed: an exact count of everything appended, plus a bounded tail, so a
    /// checkpoint of a busy feed does not carry every entry it ever had.
    LogAppend {
        path: String,
        value: Vec<u8>,
        retain: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_commitment_covers_ciphertext_with_domain() {
        let c1 = ContentCommitment::over_protected_payload(b"cipher-a");
        let c2 = ContentCommitment::over_protected_payload(b"cipher-a");
        let c3 = ContentCommitment::over_protected_payload(b"cipher-b");
        assert_eq!(c1, c2, "deterministic over identical ciphertext");
        assert_ne!(c1, c3);
        // Domain-separated: a bare hash of the payload is not the commitment.
        assert_ne!(c1.as_bytes(), *blake3::hash(b"cipher-a").as_bytes());
    }

    #[test]
    fn body_op_and_schema_roundtrip_postcard() {
        let op = Op::TextSplice {
            path: "body".into(),
            index: 3,
            delete: 1,
            insert: "hi".into(),
        };
        let bytes = postcard::to_stdvec(&op).unwrap();
        let back: Op = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(op, back);

        let schema = Schema {
            id: SchemaId::parse("issue").unwrap(),
            version: 1,
            encoding: EncodingId::parse("lait.body.v1").unwrap(),
            mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
            readable_predecessors: vec![],
        };
        let sb = postcard::to_stdvec(&schema).unwrap();
        let sback: Schema = postcard::from_bytes(&sb).unwrap();
        assert_eq!(schema, sback);
    }

    #[test]
    fn immutable_body_identity_binds_coordinate_and_value() {
        let world = WorldId::parse("com.example.product").unwrap();
        let other_world = WorldId::parse("com.example.other").unwrap();
        let schema = SchemaId::parse("comment").unwrap();
        let encoding = EncodingId::parse("lait.comment.v1").unwrap();
        let first = immutable_body_key(&world, &schema, 1, &encoding, b"canonical");
        assert_eq!(
            first,
            immutable_body_key(&world, &schema, 1, &encoding, b"canonical"),
            "identical canonical material has one stable address"
        );
        assert_ne!(
            first,
            immutable_body_key(&world, &schema, 1, &encoding, b"different")
        );
        assert_ne!(
            first,
            immutable_body_key(&other_world, &schema, 1, &encoding, b"canonical")
        );
        assert_ne!(
            first,
            immutable_body_key(&world, &schema, 2, &encoding, b"canonical")
        );
    }
}
