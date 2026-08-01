//! **Replica** — LAIT's durable-material and Convergence semantics.
//!
//! A Replica is an Orbit's durable local materialization of its Space: authority
//! material, World Bodies, semantic frontiers, locally held keys, and enough
//! metadata to distinguish unknown, partial, and corrupt material. Replica is a
//! LAIT semantic type — **not the CRDT engine**, which it never exposes. It applies
//! transaction, incorporation, and Convergence policy using [`mechanics`]
//! (mechanics) for legitimacy and [`Engine`] (Engine) for canonical
//! collaborative representation and durability.
//!
//! This crate is prefix-free from birth (the S8 renames do not touch it). It
//! names neither `loro` nor any product/consumer vocabulary — the dependency
//! edge is the seal, and the guard suite proves the vocabulary boundary.
//!
//! The sealed contract surface: Body identity ([`ids`]), Body schemas/
//! operations/descriptors ([`body`]), semantic/authority frontiers
//! ([`frontier`]), Convergence outcomes ([`convergence`]), signed transactions
//! and manifests ([`transaction`], [`manifest`]), persistent-idempotency
//! receipts ([`receipt`]), and the committing [`replica`] itself, which
//! translates validated Body operations into Engine operations and advances
//! only from durable Engine receipts.

pub mod algebra;
pub mod body;
/// The durability layer beneath Replica, re-exported so a consumer keeps one
/// namespace for objects, indexes, and the resident cache.
pub mod journal {
    pub use fabric::journal::*;
}

pub mod content;
pub mod convergence;
pub mod frontier;
pub mod ids;
pub mod manifest;
pub mod marker;
pub mod protected;
pub mod receipt;
mod replica;
/// Failures produced by Replica commit and incorporation operations.
pub mod commit {
    pub use crate::replica::Failure;
}
pub mod transaction;

pub use body::{CollaborativeSchema, ContentCommitment, MutationModel, Op, Schema};
pub use content::{
    ChunkLeaf, ChunkProof, ContentDescriptor, ContentRef, ProofStep, SealedContent,
    CHUNK_PLAINTEXT_LEN, CONTENT_FORMAT_VERSION, MAX_CONTENT_LEN, MAX_PROOF_DEPTH,
};
pub use convergence::{
    AuthorityBatchReceipt, AuthorityIncorporator, ConvergenceOutcome, IncorporationClass,
    StagedContactMaterial, ValidatedContactBundle,
};
pub use fabric::{
    Anchor, AnchorResolution, CollaborativeView, ListElement, OpHead, Version,
    CAUSAL_FORMAT_VERSION,
};
pub mod projection {
    pub use fabric::projection::Failure;
}
pub use frontier::{AuthorityFrontier, ReplicaFrontier};
pub use ids::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
pub use manifest::{
    AuthorizedRoot, ManifestBook, ManifestEntry, ManifestHead, ManifestRoot, RootObservation,
};
pub use marker::StoreMarker;
pub use protected::{
    BodyKeySource, Material, StaticBodyKeys, MAX_BODY_BYTES, MAX_PROTECTED_PLAINTEXT,
};
pub use receipt::{RequestReceipt, MAX_EFFECT_BYTES};
pub use replica::{
    operations_digest_of, ActionOutcome, BodyBinding, CommitAuthorization, CommitContext,
    ExportedMaterial, QuotaConfig, Replica, StaticAuthorizer, SupportedSchemas,
    TransactionAuthorizer, MUTATION_ATOMIC, MUTATION_COLLABORATIVE,
};
pub use transaction::{AuthoritySource, Core, Descriptor, Error, SeedSigner, Signer, Transaction};
