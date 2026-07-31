//! The Engine maintains the **shared world**: collaborative documents,
//! persistence, history, convergence, and projection.
//!
//! The kernel determines **legitimacy** — identity, authority, custody,
//! recovery, and which transitions are valid given signed history. The Engine
//! and the kernel are separate crates because the dependency edge is a
//! correctness boundary: convergence cannot confer legitimacy. They ship, test,
//! and version together as lait's substrate.
//!
//! This crate is the substrate's Loro boundary. It owns container layouts, CRDT
//! mutations, import/export, and the collaborative-document seam the replica
//! drives ([`fabric::Engine`]); kernel replay adjudicates signed authority
//! inputs. Raw document handles never cross the boundary — everything outside
//! sees [`fabric::Op`] transactions and typed exports.

pub mod fabric;
mod loro_ext;
mod op;

/// The semantics-free durable commit protocol, extracted into the lower
/// `journal` crate (mechanics commits its authority ledger through the same
/// machinery). Re-exported here so Engine consumers keep one durability
/// namespace.
pub mod journal {
    pub use ::journal::*;
}

pub mod causal;
pub use causal::{
    Anchor, AnchorResolution, Artifact, ArtifactRef, CausalError, CausalRelation, CheckpointPolicy,
    ImportStatus, Material, OpHead, Version, CAUSAL_FORMAT_VERSION, MAX_HEADS,
};
pub use fabric::{is_implemented_type_tag, is_reserved_type_tag};
pub use fabric::{
    BodyExport, CausalToken, CollaborativeView, Engine, Error, Key, ListElement, MemoryEngine, Op,
    ProjectionError, Receipt, Transaction,
};
pub use journal::{
    CallerIndex, FaultInjector, JournaledStore, ObjectRef, StoreManifest, FAULT_POINTS,
};
