//! Assignment facts derived from membership standing.

use serde::{Deserialize, Serialize};

pub use crate::acl::{Assignment, GrantOrigin, PolicyPass};

/// Which kind of act put an assignment in force — the wire form of
/// [`GrantOrigin`]'s variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentOriginKind {
    /// Seeded for a founding actor by a World's founder policy at formation.
    Founder,
    /// Installed when an admission was redeemed — the joiner's role.
    Admission,
    /// A membership role change by an admin.
    Membership,
    /// Granted beyond membership by a policy admin or delegate.
    Grant,
    /// Minted with an agent's sponsorship.
    Sponsorship,
}

/// Why an assignment exists, as the grant op that installed it recorded.
///
/// `founder`, `admission`, `membership` and `sponsorship` are all "came with
/// being a member": the assignment is the base role, expanded. `grant` is the
/// one kind a surface may present as an extra on top of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssignmentOrigin {
    pub kind: AssignmentOriginKind,
    /// The opaque product role reference (hex) whose expansion this is, for
    /// the kinds that carry one. Only the World that minted it can read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_ref: Option<String>,
    /// The role id the owning World resolved `definition_ref` to. The host
    /// leaves this empty: the reference is opaque to it by design, and the
    /// World's own reply is where the name is filled in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl AssignmentOrigin {
    /// The wire form of a recorded origin.
    pub fn of(origin: &GrantOrigin) -> Self {
        let (kind, definition_ref) = match origin {
            GrantOrigin::Founder => (AssignmentOriginKind::Founder, None),
            GrantOrigin::Admission { definition_ref } => {
                (AssignmentOriginKind::Admission, Some(definition_ref))
            }
            GrantOrigin::Membership { definition_ref } => {
                (AssignmentOriginKind::Membership, Some(definition_ref))
            }
            GrantOrigin::Grant { definition_ref } => {
                (AssignmentOriginKind::Grant, Some(definition_ref))
            }
            GrantOrigin::Sponsorship => (AssignmentOriginKind::Sponsorship, None),
        };
        Self {
            kind,
            definition_ref: definition_ref.map(|bytes| data_encoding::HEXLOWER.encode(bytes)),
            role: None,
        }
    }
}

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
    /// Why the assignment exists, when its grant op said. Absent means *not
    /// recorded* — a grant authored before origins were — and is not any kind
    /// in particular; a reader must not fold it into membership or into
    /// "granted here".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<AssignmentOrigin>,
}
