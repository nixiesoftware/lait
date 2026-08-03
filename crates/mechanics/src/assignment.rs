//! Assignment facts derived from membership standing.

use serde::{Deserialize, Serialize};

pub use crate::acl::{Assignment, PolicyPass};

// The doc comment below is NOT free text: `schemars` publishes it as the
// `description` of this type in the committed product policy schema
// (`product_policy_schema_bundle`, gated by `product_schema.rs`). Editing it
// rewrites a shipped artifact, so it is kept verbatim from where this type
// used to live.
//
// Why it lives here: this is the stringly-typed wire form of [`Assignment`] —
// `world` and `capability` flatten a `PolicyCapability`, `resource` flattens a
// `Resource` into its exact segments, and `grant_id` is the revocation handle
// (the key a grant is stored under, not a field of the fact). It previously sat
// in the Issues product's DTO module, which made the shell import a product
// crate to describe *Space* authority — authority shared by every World in the
// Space and owned by none of them. The `world` field is the tell: a type that
// names which World a capability belongs to cannot itself belong to one.
/// One effective scoped capability assignment (Mechanics authority history,
/// projected for `access ls`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssignmentDto {
    /// The grant id (64-hex) — the revocation handle.
    pub grant_id: String,
    /// The subject actor id.
    pub actor: String,
    /// The capability's World namespace.
    pub world: String,
    /// The capability name.
    pub capability: String,
    /// The exact resource segments (empty = the Space resource).
    pub resource: Vec<String>,
}
