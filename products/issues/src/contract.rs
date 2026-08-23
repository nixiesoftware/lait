#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    reason = "the bundled contract is assembled from compile-time validated identifiers and bounded canonical DTOs"
)]
//! The product World contract (C4.2) — the frozen Rust mirror of
//! `docs/plans/04-product-world-contract.md`.
//!
//! The World is pure: the daemon adapter mints every id, stamps every
//! timestamp, and resolves every ref/alias **into** the intent before submit.
//! Intents, queries, and effects are canonical JSON (the product's Layer-B
//! convention). Membership authority is mechanics, never a product Body.

use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use serde::{Deserialize, Serialize};

/// The product World id.
pub const PRODUCT_WORLD: &str = "com.lait.issues";
/// The issue Body schema.
///
/// Version 3 adds the history log ([`EVENTS_PATH`]) to version 2's comment
/// hierarchy. Both are types an older algebra cannot project, so both are
/// declared; version 3 reads 2 and 1, which is what makes a Body written at any
/// of them one issue with one history.
///
/// Version 2 writes comment threads as a `tree:comments` hierarchy. The bump is
/// not bookkeeping: a build whose algebra does not implement the tree type
/// refuses to project the *whole* issue Body — that is the declared behavior
/// for material a build cannot interpret, and it is right, but it means an
/// older reader loses the title along with the thread. Declaring the version
/// puts that refusal where it belongs, at schema gating, instead of leaving it
/// to surface as an issue that mysteriously will not open.
///
/// This version reads its predecessor: comments written as `list:comments`
/// before the cutover are read forever, and a thread spanning it reads as one
/// thread. See [`crate::views`]' comment reader.
pub const ISSUE_SCHEMA: &str = "issue";
pub const ISSUE_SCHEMA_VERSION: u32 = 3;
pub const ISSUE_ENCODING: &str = "lait.issue.v1";
/// The specification Body schema (one Body per Spec).
pub const SPEC_SCHEMA: &str = "spec";
pub const SPEC_SCHEMA_VERSION: u32 = 1;
pub const SPEC_ENCODING: &str = "lait.spec.v1";
/// The baseline Body schema (one Body per Baseline).
pub const BASELINE_SCHEMA: &str = "baseline";
pub const BASELINE_SCHEMA_VERSION: u32 = 1;
pub const BASELINE_ENCODING: &str = "lait.baseline.v1";
/// Project-owned Issue topology (edges and hierarchy). New relation writes do
/// not invalidate the Space-wide Catalog.
pub const RELATION_SCHEMA: &str = "issue_relations";
pub const RELATION_SCHEMA_VERSION: u32 = 1;
pub const RELATION_ENCODING: &str = "lait.issue-relations.v1";
/// The catalog Body schema (one Body per Space).
pub const CATALOG_SCHEMA: &str = "catalog";
/// Version 2 holds the sub-issue hierarchy as a tree ([`HIERARCHY_PATH`]).
/// Same reasoning as [`ISSUE_SCHEMA_VERSION`], and it bites harder here: a
/// build that cannot project a tree cannot project the Catalog, and the Catalog
/// is every project, alias and board in the Space. The version is what turns
/// that into a refusal at schema gating rather than a Space that opens empty.
/// Version 1 is readable — its `map:parents` entries are still read.
pub const CATALOG_SCHEMA_VERSION: u32 = 2;
pub const CATALOG_ENCODING: &str = "lait.catalog.v1";

/// The legacy projection schema version carried by every view DTO.
pub const VIEW_SCHEMA_VERSION: u32 = 5;

/// The link kinds, frozen.
pub const LINK_KINDS: [&str; 3] = ["blocks", "relates", "duplicates"];
/// The default status a fresh issue carries.
pub const DEFAULT_STATUS: &str = "backlog";
/// The canonical, user-invisible document model written by current clients.
/// A missing/zero `document_schema` register identifies a legacy Markdown body.
pub const DOCUMENT_SCHEMA_VERSION: u32 = 1;
/// Internal discriminator on canonical issue source. Client renderers collapse
/// it; semantic APIs never ask callers to supply it.
pub const DOCUMENT_PREFIX: &str = "// lait-document:1\n";
/// Maximum UTF-8 bytes for a human-facing issue or triage title.
pub const MAX_TITLE_BYTES: usize = 4 * 1024;
/// Maximum UTF-8 bytes for a human-facing entity name.
pub const MAX_NAME_BYTES: usize = 4 * 1024;
/// Maximum UTF-8 bytes for one prose field extracted into the shared corpus.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Outer Intent/Query envelope: one maximal text value plus bounded JSON,
/// links, identities and command metadata. Runtime also enforces its 2 MiB
/// substrate ceiling before World decode.
pub const MAX_PAYLOAD_BYTES: u32 = 1_572_864;
/// Project and initiative summaries share a Body with hot scalar metadata.
/// Their deliberately small ceiling keeps a scalar edit's whole-Body copy
/// bounded; long-form planning prose belongs in a Spec/Plan revision Body.
pub const MAX_METADATA_DESCRIPTION_BYTES: usize = 16 * 1024;
/// Maximum UTF-8 bytes for compact color/icon-like presentation tokens.
pub const MAX_PRESENTATION_TOKEN_BYTES: usize = 256;
/// The longest reaction emoji accepted, in UTF-8 bytes (a ZWJ family sequence
/// fits; a paragraph does not).
pub const MAX_REACTION_EMOJI_BYTES: usize = 32;
/// Hard cardinality ceilings for an issue's notification audience. They make
/// activity fan-out a bounded publication cost and prevent one pathological
/// issue from turning a comment or status transition into a tracker-wide
/// write. Assignee/follower truth remains in independently addressed relation
/// Bodies; these bounds are validated before those records are staged.
pub const MAX_ISSUE_ASSIGNEES: usize = 128;
pub const MAX_ISSUE_LABELS: usize = 128;
pub const MAX_ISSUE_FOLLOWERS: usize = 128;
pub const MAX_ISSUE_AUDIENCE: usize = MAX_ISSUE_ASSIGNEES + MAX_ISSUE_FOLLOWERS;
/// The largest accepted estimate. Every scale humans use tops out far below
/// this; the cap exists so a typo cannot become a permanent register.
pub const MAX_ESTIMATE: u32 = 1000;
/// How many files one issue may carry.
///
/// Kept, and now enforced against the raw record map rather than the decoded
/// list. That is a real change: a record this build cannot decode used to
/// occupy a slot without counting toward the limit, could never be removed
/// through the product surface, and stayed fetchable by id. Counting the raw
/// map makes the cap mean what it says.
pub const MAX_ATTACHMENTS_PER_ISSUE: usize = 8;
/// How many durable verification Runs one issue may retain.
///
/// Counted against the raw `checks` map so an entry written by a newer build
/// still occupies a slot. Each record also retains its pinned source and,
/// once accepted, its report, so the map must have a product-owned ceiling.
pub const MAX_CHECKS_PER_ISSUE: usize = 32;

/// The largest inline attachment this build will still *read*.
///
/// The write path is gone: an attachment is a `ContentRef` now, and its bytes
/// live on the content plane. This bound remains because records written before
/// the cutover are still in Bodies in the field, and a reader that refused them
/// would lose files rather than migrate them.
///
/// It is not a policy an operator tunes. It is the shape of what was already
/// written, and it can only be removed when no such record can exist.
pub const MAX_LEGACY_ATTACHMENT_BYTES: usize = 256 * 1024;
/// The longest attachment display name, in UTF-8 bytes.
///
/// A display name is shown, synced, and — the reason for a bound rather than a
/// convention — offered as the file name when someone saves the attachment.
/// Sanitising at that moment is what keeps the write safe, but sanitising is
/// downstream of convergence: an unbounded name has already reached every peer
/// by then. The engine legitimizes, so the name is bounded where it enters.
///
/// Sits under [`world_interface::destination::MAX_DISPLAY_NAME_BYTES`] so a
/// name that was accepted here always survives sanitising with its extension
/// intact.
pub const MAX_ATTACHMENT_NAME_BYTES: usize = 180;
/// The triage outcomes, frozen.
pub const TRIAGE_OUTCOMES: [&str; 3] = ["accepted", "declined", "duplicate"];
/// The self-reported health labels (project updates, initiatives).
pub const HEALTH_LABELS: [&str; 3] = ["on_track", "at_risk", "off_track"];

pub fn valid_title(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_TITLE_BYTES
}

pub fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_NAME_BYTES
}

pub const fn valid_text(value: &str) -> bool {
    value.len() <= MAX_TEXT_BYTES
}

pub fn world_id() -> WorldId {
    WorldId::parse(PRODUCT_WORLD).expect("product world id")
}

// ---- Authorization demands (plan 04 policy vocabulary) --------------------
//
// The World declares a canonical non-empty demand for every mutation and
// query; Mechanics evaluates it at the pinned authority frontier. These are
// the frozen constructors from plan 04's routing table.

use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};

/// The Space-level resource of the Issues World.
fn space_resource() -> Resource {
    Resource::root(PRODUCT_WORLD)
}

/// A Space-scoped capability of the Issues World.
fn space_cap(name: &str) -> PolicyCapability {
    PolicyCapability::new(PRODUCT_WORLD, name)
}

/// `Require(space.admin, Space)` — the admin demand.
pub fn demand_admin() -> Vec<u8> {
    AuthorizationDemand::require(space_cap("space.admin"), space_resource())
        .encode_canonical()
        .expect("canonical admin demand")
}

/// `Any(Require(space.contributor, Space), Require(space.admin, Space))` — the
/// ordinary contributor demand, with admin as an explicit override.
pub fn demand_contributor() -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(space_cap("space.contributor"), space_resource()),
        AuthorizationDemand::require(space_cap("space.admin"), space_resource()),
    ])
    .encode_canonical()
    .expect("canonical contributor demand")
}

/// `Any(Require(<capability>, Space), Require(space.admin, Space))` — a
/// Space-scoped registry/policy mutation with the explicit admin override.
pub fn demand_space_any(capability: &str) -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(space_cap(capability), space_resource()),
        AuthorizationDemand::require(space_cap("space.admin"), space_resource()),
    ])
    .encode_canonical()
    .expect("canonical space-any demand")
}

/// `Any(Require(<capability>, Project(<id>)), Require(space.admin, Space))` —
/// a Project-scoped mutation with the explicit admin override (the shape
/// `project.delete` uses).
pub fn demand_project_any(capability: &str, project: &str) -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(
            space_cap(capability),
            Resource::segments(PRODUCT_WORLD, [project]).expect("validated project resource"),
        ),
        AuthorizationDemand::require(space_cap("space.admin"), space_resource()),
    ])
    .encode_canonical()
    .expect("canonical project-any demand")
}

/// `Any(Require(<capability>, Project), contributor, admin)` — draft and
/// evidence authoring. Issuing governing material deliberately uses
/// [`demand_project_any`] instead, so ordinary contribution cannot silently
/// become project direction.
pub fn demand_project_work(capability: &str, project: &str) -> Vec<u8> {
    AuthorizationDemand::Any(vec![
        AuthorizationDemand::require(
            space_cap(capability),
            Resource::segments(PRODUCT_WORLD, [project]).expect("validated project resource"),
        ),
        AuthorizationDemand::require(space_cap("space.contributor"), space_resource()),
        AuthorizationDemand::require(space_cap("space.admin"), space_resource()),
    ])
    .encode_canonical()
    .expect("canonical project-work demand")
}

/// `Require(space.issue.read, Space)` — every query's read demand.
pub fn demand_read() -> Vec<u8> {
    AuthorizationDemand::require(space_cap("space.issue.read"), space_resource())
        .encode_canonical()
        .expect("canonical read demand")
}

/// The one executable contract the Issues package currently adopts.
pub const VERIFY_SPEC: &str = "issue.verify";
pub const VERIFY_SPEC_VERSION: u32 = 1;
pub const VERIFY_INPUT_SCHEMA: &str = "issue.verify.input";
pub const VERIFY_OUTPUT_SCHEMA: &str = "issue.verify.output";

pub fn verify_spec_ref() -> runtime::exec::SchemaRef {
    runtime::exec::SchemaRef {
        name: SchemaId::parse(VERIFY_SPEC).expect("verification Spec id"),
        version: VERIFY_SPEC_VERSION,
    }
}

pub fn verify_output_ref() -> runtime::exec::SchemaRef {
    runtime::exec::SchemaRef {
        name: SchemaId::parse(VERIFY_OUTPUT_SCHEMA).expect("verification output schema"),
        version: 1,
    }
}

pub fn verify_limits() -> runtime::exec::Limits {
    runtime::exec::Limits {
        attempts: 3,
        events: 64,
        checkpoints: 0,
        child_runs: 0,
        progress_bytes: 4 * 1024,
        checkpoint_bytes: 0,
        wall_millis: 30 * 60 * 1_000,
    }
}

/// First-party runner-local verifier Build for `issue.verify/v1`.
///
/// Build publication is identity, not a dispatch gate. Runtime binds the
/// exact caller-selected Build id into the Run and still refuses to imply
/// that the Build was published or attested. The runner-local handler is selected
/// from the application package. Callers of `issues_verify` may name this id
/// explicitly or omit it so the application package fills it in.
pub fn verify_build(world_build: [u8; 32]) -> runtime::exec::Build {
    let seed = *blake3::hash(b"lait/issues/verify-build-seed/1").as_bytes();
    let publisher = mechanics::ids::ActorId::from_incept_hash(
        &data_encoding::HEXLOWER.encode(blake3::hash(b"lait/issues/verify-publisher/1").as_bytes()),
    );
    let handler = replica::content::ContentRef {
        content_id: *blake3::hash(b"lait/issues/verify-handler/1").as_bytes(),
    };
    runtime::exec::Build {
        id: runtime::exec::BuildId::from_bytes([0; 32]),
        world: replica::body::WorldId::parse(PRODUCT_WORLD).expect("product World id"),
        world_build,
        spec: verify_spec_ref(),
        handler,
        dependencies: None,
        environment: *blake3::hash(b"lait/issues/verify-environment/1").as_bytes(),
        config: Vec::new(),
        checkpoint: None,
        replay_commands: None,
        compatible_from: Vec::new(),
        publisher,
        signature: runtime::exec::Signature {
            signer: mechanics::actor::device_from_seed(&seed),
            algorithm: 1,
            bytes: [0; 64],
        },
    }
    .sign(&seed)
    .expect("bundled verify Build signs")
}

/// Canonical lowercase hex of [`verify_build`] for the given World implementation.
pub fn verify_build_hex(world_build: [u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(&verify_build(world_build).id.as_bytes())
}

pub fn verify_spec() -> runtime::exec::Spec {
    let contributor = demand_contributor();
    runtime::exec::Spec {
        name: verify_spec_ref().name,
        version: VERIFY_SPEC_VERSION,
        access: runtime::exec::Access {
            start: contributor.clone(),
            offer: contributor.clone(),
            control: contributor.clone(),
            accept: contributor,
        },
        input: runtime::exec::PayloadSpec {
            schema: runtime::exec::SchemaRef {
                name: SchemaId::parse(VERIFY_INPUT_SCHEMA).expect("verification input schema"),
                version: 1,
            },
            max_inline_bytes: 1_024,
            max_content_refs: 1,
            max_content_bytes: replica::content::MAX_CONTENT_LEN,
            read: demand_read(),
            max_additional_input_bytes: 0,
        },
        output: runtime::exec::PayloadSpec {
            schema: verify_output_ref(),
            max_inline_bytes: 64,
            max_content_refs: 1,
            max_content_bytes: replica::content::MAX_CONTENT_LEN,
            read: demand_read(),
            max_additional_input_bytes: 0,
        },
        mode: runtime::exec::Mode::Unary,
        resume: runtime::exec::Resume::Restart,
        effects: runtime::exec::Effects::Pure,
        accept: runtime::exec::AcceptRule::World,
        queries: Vec::new(),
        service: None,
        links: Vec::new(),
        limits: verify_limits(),
    }
}

/// Build the exact output material an `issue.verify/v1` handler returns.
///
/// A failed check is still a successfully executed verifier, so both verdicts
/// use [`runtime::exec::TerminalClass::Succeeded`]. The verdict itself is
/// canonical inline JSON and therefore enters the Runtime output digest beside
/// the report ContentRef. Acceptance can validate the decision without reading
/// either payload.
pub fn verify_candidate(
    verdict: &str,
    report: replica::content::ContentRef,
    report_bytes: u64,
) -> Option<runtime::exec::Candidate> {
    if !matches!(verdict, "pass" | "fail") {
        return None;
    }
    let inline = serde_json::to_vec(&VerifyOutput {
        verdict: verdict.to_owned(),
    })
    .ok()?;
    Some(runtime::exec::Candidate {
        output: verify_output_ref(),
        inline,
        content: vec![report],
        content_bytes: report_bytes,
        terminal: runtime::exec::TerminalClass::Succeeded,
        usage: Vec::new(),
        evidence: Vec::new(),
    })
}

// ---- Reliable signals -----------------------------------------------------

/// The signals this World declares, by name.
///
/// Each is a *nudge*: it says where to look and never what happened. That is not
/// minimalism, it is the rule that makes a signal safe to lose. The durable
/// record — the issue, its assignee, its comments — is already committed and
/// already reaches everyone through convergence; a signal only makes it timely.
/// A signal that carried the fact would be a second copy of it, on a plane whose
/// whole contract is that it keeps nothing.
/// Two, and only because two are emitted. A declared signal nothing sends is a
/// reviewed surface with nothing behind it — the same shape as a feature bit
/// advertised by a build that does not honour it — and it moves this World's
/// implementation id to say so. `mentioned` and `review-requested` were in the
/// first version of this list and are not here now: nothing parses a mention and
/// no verb requests a review, so both were promises.
pub mod signal {
    /// Somebody put an issue on you.
    pub const ASSIGNED: &str = "assigned";
    /// Somebody said something on an issue you are on.
    pub const COMMENTED: &str = "commented";
}

/// What every Issues signal carries.
///
/// A doc id and nothing else. The receiver already has, or can converge, the
/// issue itself — so the largest thing this ever needs to say is which one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssueNudge {
    /// The `iss_` doc id, never a project alias: an alias is a display form and
    /// the receiver would have nothing to resolve it against.
    pub issue: String,
}

impl IssueNudge {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard issue nudge")
    }

    /// Decode one nudge, in the order every other shape in this tree uses:
    /// postcard, then re-encode equality, so one value has one spelling.
    pub fn decode_canonical(bytes: &[u8]) -> Option<Self> {
        let nudge: Self = postcard::from_bytes(bytes).ok()?;
        (nudge.encode() == bytes).then_some(nudge)
    }
}

/// The ceiling every Issues signal declares.
///
/// A doc id is around thirty bytes and a nudge carries one. This is generous for
/// what it holds and tight against anything that wanted to become a message —
/// which is the point of a per-schema bound, since the plane's own ceiling is
/// sixteen kilobytes.
pub const MAX_NUDGE_BYTES: u32 = 128;

/// The signal declarations this World registers.
///
/// Every one demands `space.issue.read`. Being told about an issue is a read of
/// it: a signal naming something you may not open would tell you it exists, and
/// its assignee, and when somebody touched it.
pub fn signal_schemas() -> Vec<runtime::world::SignalSchema> {
    [signal::ASSIGNED, signal::COMMENTED]
        .into_iter()
        .map(|name| runtime::world::SignalSchema {
            name: SchemaId::parse(name).expect("declared signal name"),
            max_payload_bytes: MAX_NUDGE_BYTES,
            demand: demand_read(),
        })
        .collect()
}

/// The full Space capability set the founder is granted at formation:
/// `(capability, resource)` pairs, plus the Mechanics meta policy-admin grant.
pub fn founder_capabilities() -> Vec<(PolicyCapability, Resource)> {
    ["space.admin", "space.contributor", "space.issue.read"]
        .into_iter()
        .map(|c| (space_cap(c), space_resource()))
        .collect()
}

// ---- Capability registry v1 (plan 04) -------------------------------------
//
// The registry is part of the implementation descriptor's policy-table
// commitment, NOT editable Catalog state; changing an entry requires a new
// implementation id, and entries are never repurposed in place.

/// The Space-scoped capability ids, sorted.
pub const SPACE_CAPABILITIES: [&str; 8] = [
    "catalog.label.configure",
    "catalog.workflow.configure",
    "policy.assign",
    "policy.configure",
    "project.create",
    "space.admin",
    "space.contributor",
    "space.issue.read",
];

/// The Project-scoped capability ids, sorted. `workflow.transition.<id>` is a
/// qualified family validated by grammar, not enumerated here.
pub const PROJECT_CAPABILITIES: [&str; 20] = [
    "baseline.issue",
    "baseline.write",
    "comment.create",
    "issue.assign",
    "issue.bind",
    "issue.create",
    "issue.delete",
    "issue.edit",
    "issue.label",
    "issue.link",
    "issue.move_in",
    "issue.move_out",
    "issue.parent",
    "issue.restore",
    "issue.verify",
    "project.configure",
    "project.delete",
    "spec.issue",
    "spec.write",
    "workflow.transition",
];

/// The canonical exhaustive registry bytes: one line per entry,
/// `scope id delegable`, sorted. The `workflow.transition` row stands for
/// the qualified `workflow.transition.<TransitionId>` family.
pub fn capability_registry_bytes() -> Vec<u8> {
    let mut out = String::new();
    for id in SPACE_CAPABILITIES {
        out.push_str("space	");
        out.push_str(id);
        out.push_str(
            "	delegable
",
        );
    }
    for id in PROJECT_CAPABILITIES {
        out.push_str("project	");
        out.push_str(id);
        out.push_str(
            "	delegable
",
        );
    }
    out.into_bytes()
}

/// The policy-table commitment (plan 01): BLAKE3 derive-key, context
/// `lait.world-policy-table.v1`, over the exhaustive registry bytes. This is
/// the commitment the implementation descriptor embeds.
pub fn capability_registry_commitment() -> [u8; 32] {
    blake3::derive_key("lait.world-policy-table.v1", &capability_registry_bytes())
}

/// Whether `name` is a registered Space-scoped capability.
pub fn is_space_capability(name: &str) -> bool {
    SPACE_CAPABILITIES.contains(&name)
}

/// Whether `name` is a registered Project-scoped capability (including the
/// qualified `workflow.transition.<TransitionId>` family).
pub fn is_project_capability(name: &str) -> bool {
    if PROJECT_CAPABILITIES.contains(&name) {
        return true;
    }
    name.strip_prefix("workflow.transition.").is_some_and(|t| {
        !t.is_empty()
            && t.len() <= 64
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    })
}

pub fn issue_schema() -> SchemaId {
    SchemaId::parse(ISSUE_SCHEMA).expect("issue schema id")
}

pub fn catalog_schema() -> SchemaId {
    SchemaId::parse(CATALOG_SCHEMA).expect("catalog schema id")
}

pub fn spec_schema() -> SchemaId {
    SchemaId::parse(SPEC_SCHEMA).expect("spec schema id")
}

pub fn baseline_schema() -> SchemaId {
    SchemaId::parse(BASELINE_SCHEMA).expect("baseline schema id")
}

pub fn issue_encoding() -> EncodingId {
    EncodingId::parse(ISSUE_ENCODING).expect("issue encoding id")
}

pub fn catalog_encoding() -> EncodingId {
    EncodingId::parse(CATALOG_ENCODING).expect("catalog encoding id")
}

pub fn spec_encoding() -> EncodingId {
    EncodingId::parse(SPEC_ENCODING).expect("spec encoding id")
}

pub fn baseline_encoding() -> EncodingId {
    EncodingId::parse(BASELINE_ENCODING).expect("baseline encoding id")
}

pub fn relation_schema() -> SchemaId {
    SchemaId::parse(RELATION_SCHEMA).expect("relation schema id")
}

pub fn relation_encoding() -> EncodingId {
    EncodingId::parse(RELATION_ENCODING).expect("relation encoding id")
}

/// The ONE deterministic catalog Body per Space: the first 16 bytes of the
/// BLAKE3 derive-key digest, context `lait.issues.catalog.v1`, over the
/// canonical `(SpaceId, WorldId)` bytes (each length-prefixed big-endian).
/// Joiners adopt this Body through Manifest synchronization; nobody ever
/// creates it locally except the founder's one `InitializeTracker`.
pub fn catalog_body_id(space: &mechanics::ids::SpaceId) -> BodyId {
    let space_bytes = space.as_str().as_bytes();
    let world_bytes = PRODUCT_WORLD.as_bytes();
    let mut input = Vec::with_capacity(4 + space_bytes.len() + world_bytes.len());
    input.extend_from_slice(&(space_bytes.len() as u16).to_be_bytes());
    input.extend_from_slice(space_bytes);
    input.extend_from_slice(&(world_bytes.len() as u16).to_be_bytes());
    input.extend_from_slice(world_bytes);
    let digest = blake3::derive_key("lait.issues.catalog.v1", &input);
    let mut raw = [0u8; 16];
    raw.copy_from_slice(&digest[..16]);
    BodyId::from_bytes(raw)
}

/// The Body id of an issue: derived deterministically from its `iss_` DocId.
pub fn issue_body_id(doc: &str) -> BodyId {
    let mut h = blake3::Hasher::new();
    h.update(b"lait/issue-body/1");
    h.update(doc.as_bytes());
    let mut raw = [0u8; 16];
    raw.copy_from_slice(&h.finalize().as_bytes()[..16]);
    BodyId::from_bytes(raw)
}

/// The Body id of a Spec: derived deterministically from its `spc_` id.
pub fn spec_body_id(spec: &str) -> BodyId {
    named_body_id(b"lait/spec-body/1", spec)
}

/// The Body id of a Baseline: derived deterministically from its `bas_` id.
pub fn baseline_body_id(baseline: &str) -> BodyId {
    named_body_id(b"lait/baseline-body/1", baseline)
}

/// One topology Body per project. Its tree can enforce hierarchy acyclicity
/// locally because parentage is project-local.
pub fn relation_body_id(project: &str) -> BodyId {
    named_body_id(b"lait/issue-relations/1", project)
}

fn named_body_id(domain: &[u8], id: &str) -> BodyId {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(id.as_bytes());
    let mut raw = [0u8; 16];
    raw.copy_from_slice(&h.finalize().as_bytes()[..16]);
    BodyId::from_bytes(raw)
}

pub fn catalog_key(space: &mechanics::ids::SpaceId) -> BodyKey {
    BodyKey::new(world_id(), catalog_body_id(space))
}

pub fn issue_key(doc: &str) -> BodyKey {
    BodyKey::new(world_id(), issue_body_id(doc))
}

pub fn spec_key(spec: &str) -> BodyKey {
    BodyKey::new(world_id(), spec_body_id(spec))
}

pub fn baseline_key(baseline: &str) -> BodyKey {
    BodyKey::new(world_id(), baseline_body_id(baseline))
}

pub fn relation_key(project: &str) -> BodyKey {
    BodyKey::new(world_id(), relation_body_id(project))
}

/// The catalog board list path for a project.
pub fn board_path(project: &str) -> String {
    format!("board/{}", project.to_ascii_lowercase())
}

/// The reactions set path for one comment. Comment ids are canonically
/// lowercase (`cmt_` + lowercased ULID) precisely so they are path-legal —
/// the frozen path grammar admits only `[a-z0-9_]`.
pub fn reaction_path(comment_id: &str) -> String {
    format!("reactions/{comment_id}")
}

/// The one set every reaction on an issue lives in.
///
/// It used to be one set *per comment* — [`reaction_path`] — which is one root
/// container per reacted-to comment. Root containers are what the projection
/// walks, and it walks all of them on every read of the issue, including the
/// reads that only want a title: a long thread with a lot of reactions made
/// every unrelated read of that issue more expensive, without bound.
///
/// One set holds them all instead, with the comment named in the value rather
/// than in the path. The type is unchanged and so are its semantics — still an
/// observed-remove, add-wins set, so two actors reacting concurrently never
/// clobber and a reaction that raced its own removal survives. A map keyed by
/// `(emoji, actor)` would have collapsed the same containers and quietly turned
/// that race into last-writer-wins.
pub const REACTIONS_PATH: &str = "reactions";

/// The catalog hierarchy: sub-issue parentage, one node per issue that takes
/// part, anchored by doc id.
///
/// Not `parents` — that path is bound to a map in every catalog Body in the
/// field, and a path holds one collaborative type for its Body's lifetime, so
/// reusing the name would be a `TypeConflict` on every existing Space rather
/// than a migration.
///
/// The map it replaces stored child -> parent as an entry per child. Two peers
/// could then parent A under B and B under A concurrently, each passing its own
/// ancestry check against its own view, and the merge held a cycle nothing
/// afterwards rejected — `is_ancestor` still carries the loop guard that fact
/// required. A tree cannot hold one: the engine refuses the move that would
/// close it, wherever it is applied.
pub const HIERARCHY_PATH: &str = "hierarchy";

/// An issue's history feed. A log rather than the list it was, and not at the
/// `events` path the list holds — a path keeps one collaborative type for its
/// Body's lifetime, so the log needs a name of its own.
pub const EVENTS_PATH: &str = "history";

/// How many events an issue keeps in Body state.
///
/// The number is a trade with two sides and no free choice. Every retained
/// event is carried by every checkpoint of that issue, forever: an unbounded
/// feed made a busy issue's snapshot grow without limit, which is what sent
/// this type into the algebra. Every trimmed event is one nobody can read
/// again once a checkpoint compacts the history behind it.
///
/// 512 is chosen for what it makes impossible rather than for what it keeps: an
/// issue would have to be edited, assigned, commented and transitioned five
/// hundred times before it loses a row, which no issue in the corpus approaches
/// — while the runaway case that motivated the type is bounded absolutely. The
/// count survives trimming exactly, so a reader can always say how much
/// happened even where it can no longer say what.
pub const EVENTS_RETAINED: u64 = 512;

/// Whether `s` is a canonical comment id: `cmt_` + a 26-character lowercased
/// ULID in the kernel's base32 alphabet (`0-9` then `a-v`, the lowercase of
/// [`mechanics::ids`]' encoder). The daemon mints and lowercases; the World
/// re-validates because ids arrive inside the intent.
pub fn is_comment_id(s: &str) -> bool {
    s.strip_prefix("cmt_").is_some_and(|ulid| {
        ulid.len() == 26
            && ulid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'v').contains(&b))
    })
}

/// One reaction as stored in the issue's reactions set:
/// `comment \t emoji \t actor`.
///
/// The comment moved from the path into the value when the per-comment sets
/// were collapsed into one — see [`REACTIONS_PATH`]. Tab-separated as before,
/// and still safe to split on: [`is_reaction_emoji`] refuses whitespace, and a
/// comment id is `cmt_` plus base32.
pub fn reaction_value(comment: &str, emoji: &str, actor: &str) -> Vec<u8> {
    format!("{comment}\t{emoji}\t{actor}").into_bytes()
}

/// Parse a stored reaction value back into `(comment, emoji, actor)`.
pub fn parse_reaction_value(raw: &[u8]) -> Option<(String, String, String)> {
    let s = std::str::from_utf8(raw).ok()?;
    let mut parts = s.split('\t');
    let (comment, emoji, actor) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || comment.is_empty() || emoji.is_empty() || actor.is_empty() {
        return None;
    }
    Some((comment.to_string(), emoji.to_string(), actor.to_string()))
}

/// The value a legacy per-comment reactions set holds. Written by no encoder
/// any more; still constructed, because un-reacting has to be able to remove a
/// reaction that was added before the sets were collapsed.
pub fn reaction_value_legacy(emoji: &str, actor: &str) -> Vec<u8> {
    format!("{emoji}\t{actor}").into_bytes()
}

/// Parse a value from a legacy per-comment reactions set, which named only
/// `emoji \t actor` because the path carried the comment. Read forever: these
/// are in Bodies in the field, and nothing writes the shape any more.
pub fn parse_legacy_reaction_value(raw: &[u8]) -> Option<(String, String)> {
    let s = std::str::from_utf8(raw).ok()?;
    let (emoji, actor) = s.split_once('\t')?;
    if emoji.is_empty() || actor.is_empty() || actor.contains('\t') {
        return None;
    }
    Some((emoji.to_string(), actor.to_string()))
}

/// Whether `emoji` is acceptable as a reaction: non-empty, bounded, and free
/// of the control/whitespace bytes the storage encoding reserves.
pub fn is_reaction_emoji(emoji: &str) -> bool {
    !emoji.is_empty()
        && emoji.len() <= MAX_REACTION_EMOJI_BYTES
        && !emoji.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// A board position, resolved to DocIds by the daemon before submit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Pos {
    Top,
    Bottom,
    Before { doc: String },
    After { doc: String },
}

pub const CHANGE_SET_MAX_OPERATIONS: usize = 64;
pub const CHANGE_SET_MAX_BYTES: usize = 512 * 1_024;

/// A project coordinate inside one atomic Issues change set. Later operations
/// may address a project created by an earlier ordinal without a client-side
/// create/read/create loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ChangeProject {
    Existing { project: String },
    Created { operation: u16 },
}

/// A label coordinate inside one atomic Issues change set. A newly-created
/// label may be attached to a later Issue without an adapter create/read loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ChangeLabel {
    Existing { label: String },
    Created { operation: u16 },
}

fn default_priority() -> String {
    "none".into()
}

/// A Board target resolved inside the pinned World action. Keeping product
/// references here (rather than pre-querying in an adapter) lets a human drag
/// and an agent ChangeSet obey the same exact-publication rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum ChangePosition {
    Top,
    Bottom,
    Before { issue: String },
    After { issue: String },
}

/// The first canonical bounded batch vocabulary. New operation kinds extend
/// this product-owned planner; they never expose Runtime Body operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ChangeOperation {
    ProjectCreate {
        name: String,
        key: String,
        color: String,
    },
    SpecCreate {
        project: ChangeProject,
        kind: crate::spec::Kind,
        title: String,
        text: String,
        #[serde(default)]
        links: Vec<crate::spec::Link>,
    },
    IssueCreate {
        project: ChangeProject,
        title: String,
        #[serde(default = "default_priority")]
        priority: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        assignees: Vec<String>,
        #[serde(default)]
        labels: Vec<ChangeLabel>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        due: Option<u64>,
        #[serde(default)]
        estimate: Option<u32>,
    },
    /// Atomically change workflow state and/or Board position. A cross-column
    /// drag is one predecessor-bound transition and one durable operation, not
    /// an adapter loop of status then move.
    IssueBoard {
        issue: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        position: Option<ChangePosition>,
    },
    /// Replace any subset of the row-sized Issue facts. Set-valued fields are
    /// exact replacements so a multi-assignee/label edit remains one bounded
    /// durable operation rather than an adapter loop.
    IssuePatch {
        issue: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        due: Option<u64>,
        #[serde(default)]
        clear_due: bool,
        #[serde(default)]
        estimate: Option<u32>,
        #[serde(default)]
        clear_estimate: bool,
        #[serde(default)]
        assignees: Option<Vec<String>>,
        #[serde(default)]
        labels: Option<Vec<ChangeLabel>>,
    },
    /// Apply one work-state verb, including its authenticated self-assignment
    /// semantics, through the same planner used by human and agent batches.
    IssueWork {
        issue: String,
        action: WorkAction,
    },
    /// Tombstone or restore one Issue. The Boolean is explicit so retries and
    /// bulk plans remain byte-identical instead of inferring a toggle.
    IssueTombstone {
        issue: String,
        on: bool,
    },
    /// Add one immutable comment record. Its stable id is derived from the
    /// ChangeSet request id and this operation's ordinal.
    IssueComment {
        issue: String,
        body: String,
        #[serde(default)]
        parent: Option<String>,
    },
    /// Add an immutable comment anchored against one exact rendered source.
    /// The full station-local coordinate prevents a stale scalar offset from
    /// being reinterpreted against a rematerialized or newer Issue body.
    IssueCommentAt {
        issue: String,
        body: String,
        field: String,
        start: u64,
        #[serde(default)]
        end: Option<u64>,
        #[serde(default)]
        parent: Option<String>,
        source: runtime::publication::WorldPublicationId,
    },
    /// Replace one actor-owned reaction tuple with its explicit presence.
    IssueReaction {
        issue: String,
        comment: String,
        emoji: String,
        on: bool,
    },
    /// Replace one directed/symmetric Issue relation tuple.
    IssueLink {
        issue: String,
        kind: String,
        target: String,
        on: bool,
    },
    /// Replace the Issue's parent relation. `None` unparents.
    IssueParent {
        issue: String,
        #[serde(default)]
        parent: Option<String>,
    },
    /// Move an Issue between projects and/or to a bounded Board position.
    IssueMove {
        issue: String,
        #[serde(default)]
        project: Option<ChangeProject>,
        #[serde(default)]
        position: Option<ChangePosition>,
    },
    /// Replace the Issue's milestone membership within its current project.
    IssueMilestone {
        issue: String,
        #[serde(default)]
        milestone: Option<String>,
    },
    LabelCreate {
        name: String,
        color: String,
    },
    LabelEdit {
        label: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    LabelDelete {
        label: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeResult {
    pub operation: u16,
    pub kind: String,
    pub id: String,
}

/// A label minted by this transaction (create-on-first-use).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewLabel {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// Deserialize a present field (including an explicit `null`) as the OUTER
/// `Some` of a double option — absent stays `None` via `#[serde(default)]`.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// The work-state actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAction {
    Start,
    Done,
    Stop,
}

/// One source-preserving edit used while upgrading a legacy issue body.
///
/// Edits are expressed in the same Unicode-scalar coordinate system as the
/// live text CRDT and are applied in the order supplied. The adapter emits them
/// from the end of the document towards the beginning, which keeps unchanged
/// source material (and therefore range-comment anchors) alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSplice {
    pub index: u64,
    pub delete: u64,
    pub insert: String,
}

/// One exact, content-bound source window selected outside mutation admission.
/// The cursor is a compact product coordinate; source content never enters the
/// signed command or the host's opaque lifecycle record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct V4MigrationWindow {
    /// Stable phase vocabulary. The planner uses `catalog`, `issue`, the
    /// Catalog-backed `coordinates` join, `spec`, `baseline`, then the
    /// body-less `terminal` sentinel.
    pub phase: String,
    /// Exact frozen Body opened by the commit callback. Absent only for the
    /// terminal sentinel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyKey>,
    /// Phase-local continuation within that Body. It is an ordinal or a
    /// canonical map/set key, never source payload.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subitem: String,
    /// Domain-separated digest of the selected Body/window coordinate and its
    /// frozen source bytes. Commit recomputes it before staging any effect.
    pub digest: [u8; 32],
    /// Canonical durable cursor written with the migrated output.
    pub cursor: String,
}

impl V4MigrationWindow {
    pub const CURSOR_PREFIX: &str = "m1";
    pub const TERMINAL_PHASE: &str = "terminal";

    pub fn render_cursor(phase: &str, body: Option<&BodyKey>, subitem: &str) -> Option<String> {
        if !matches!(
            phase,
            "catalog" | "issue" | "coordinates" | "spec" | "baseline" | Self::TERMINAL_PHASE
        ) || subitem.len() > 256
            || (phase == Self::TERMINAL_PHASE) != body.is_none()
            || (phase == Self::TERMINAL_PHASE && !subitem.is_empty())
        {
            return None;
        }
        let body = body.map_or_else(|| "-".to_string(), |key| key.body.render());
        let subitem = data_encoding::BASE64URL_NOPAD.encode(subitem.as_bytes());
        Some(format!("{}:{phase}:{body}:{subitem}", Self::CURSOR_PREFIX))
    }

    pub fn valid(&self) -> bool {
        self.digest != [0; 32]
            && self
                .body
                .as_ref()
                .is_none_or(|body| body.world == world_id())
            && Self::render_cursor(&self.phase, self.body.as_ref(), &self.subitem)
                .is_some_and(|cursor| cursor == self.cursor)
            && self.cursor.len() <= 512
    }

    pub fn parse_cursor(cursor: &str) -> Option<(String, Option<BodyId>, String)> {
        let mut parts = cursor.splitn(4, ':');
        if parts.next()? != Self::CURSOR_PREFIX {
            return None;
        }
        let phase = parts.next()?.to_string();
        let body = match parts.next()? {
            "-" => None,
            rendered => Some(BodyId::parse(rendered)?),
        };
        let subitem = String::from_utf8(
            data_encoding::BASE64URL_NOPAD
                .decode(parts.next()?.as_bytes())
                .ok()?,
        )
        .ok()?;
        Self::render_cursor(
            &phase,
            body.as_ref()
                .map(|body| BodyKey::new(world_id(), body.clone()))
                .as_ref(),
            &subitem,
        )
        .filter(|canonical| canonical == cursor)?;
        Some((phase, body, subitem))
    }

    pub fn terminal(&self) -> bool {
        self.phase == Self::TERMINAL_PHASE
    }
}

/// Crash-stable coordinate for one bounded v3 -> v4 lifecycle batch.
///
/// The launcher persists the signed intent containing this value before it
/// submits. A large legacy description therefore remains in its frozen Body;
/// neither the opaque lifecycle record nor the signed command becomes a
/// tracker-sized data transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct V4MigrationPlan {
    pub version: u16,
    pub source: runtime::publication::PublicationId,
    pub source_frontier: replica::frontier::ReplicaFrontier,
    /// Last durably committed batch/cursor at preparation time. Zero plus an
    /// empty cursor names the first batch.
    pub previous_batch: u64,
    #[serde(default)]
    pub previous_cursor: String,
    /// Exact next frozen source window, prepared read-only before the action is
    /// admitted to the single mutation lane.
    pub window: V4MigrationWindow,
    /// Product fact timestamp chosen once when this plan is signed. Durable
    /// attribution still comes exclusively from authenticated Context.
    pub timestamp: u64,
}

impl V4MigrationPlan {
    pub const VERSION: u16 = 2;

    pub fn valid(&self) -> bool {
        self.version == Self::VERSION
            && self.source.implementation_digest != [0; 32]
            && self.source.extractor_schema_digest.digest() != [0; 32]
            && self.source_frontier != replica::frontier::ReplicaFrontier::EMPTY
            && self.previous_cursor.len() <= 512
            && (self.previous_batch > 0 || self.previous_cursor.is_empty())
            && self.window.valid()
            && self.window.cursor != self.previous_cursor
            && self.timestamp > 0
    }
}

/// The product intents (schema `issue` v1). Every id/timestamp is supplied by
/// the daemon; the World validates and stages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum IssueIntent {
    /// Preflight and lower a bounded, ordered product mutation as one Runtime
    /// action. Entity identities derive from this action's RequestId plus the
    /// operation ordinal, so retries are byte- and identity-idempotent.
    ChangeSet {
        operations: Vec<ChangeOperation>,
        /// Signed client fact used for authored revision ordering.
        ts: u64,
    },
    /// The ONE founder-only, crash-resumable formation intent: it atomically
    /// creates the deterministic Catalog with the captured display name,
    /// initialization timestamp, initial project, the built-in role
    /// definitions, the capability-registry commitment, and the default
    /// workflow revision. The World lifecycle adapter persists the complete
    /// signed action before submission and replays the exact bytes after a crash;
    /// the World is a deterministic pure validator/stager (no clock, no id
    /// generator). Joiners adopt the Catalog through Manifest synchronization
    /// and never synthesize it locally.
    InitializeTracker {
        name: String,
        ts: u64,
        project_id: String,
        project_name: String,
        project_key: String,
        device: String,
        /// `(role_id, revision_id hex, definition digest hex)` for the three
        /// built-ins — validated against this release's reviewed definitions.
        built_in_roles: Vec<(String, String, String)>,
        /// Hex of [`capability_registry_commitment`].
        capability_registry_commitment: String,
        /// Hex of the initial project's default workflow revision id.
        default_workflow_commitment: String,
    },
    /// Advance the decisive v3 -> v4 physical migration by one bounded atomic
    /// batch. A durable cursor and immutable audit entry ride the same
    /// transaction as the copied facts; callers repeat until the effect is an
    /// idempotent no-op. Batching is required by the substrate's 4,096-op /
    /// 1-MiB transaction ceiling and is not exposed as product truth.
    V4Migrate {
        /// Compact deterministic coordinate prepared against one exact frozen
        /// source publication by the launcher-owned lifecycle. Migrated
        /// Bodies and text remain in the frozen source rather than inflating
        /// the host's opaque lifecycle record.
        plan: V4MigrationPlan,
    },
    IssueNew {
        doc: String,
        project: String,
        title: String,
        priority: String,
        assignees: Vec<String>,
        labels: Vec<String>,
        new_labels: Vec<NewLabel>,
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duedate: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimate: Option<u32>,
        actor: String,
        device: String,
        ts: u64,
    },
    IssueEdit {
        doc: String,
        title: Option<String>,
        status: Option<String>,
        priority: Option<String>,
        description: Option<String>,
        /// Double-option: absent = untouched, `Some(None)` (JSON `null`) =
        /// clear, `Some(Some(ts))` = set (unix seconds). The custom
        /// deserializer is what keeps `null` distinct from absent — serde's
        /// default reads both as the outer `None`.
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        duedate: Option<Option<u64>>,
        /// Same shape as `duedate`; points on whatever scale the team reads.
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        estimate: Option<Option<u32>>,
        device: String,
        ts: u64,
    },
    /// One editor-local operation against the issue description's text CRDT.
    /// Offsets count Unicode scalar values, matching [`fabric::Op::TextSplice`].
    ///
    /// `base_len` is the scalar length of the document the offsets were
    /// computed against, and the World refuses when it disagrees with what it
    /// holds. Without it this is a bare positional write with no agreement
    /// about *which* document it applies to — and an editor whose coordinate
    /// space had drifted would silently overwrite unrelated text, which is
    /// exactly what happened to a document whose editor was measuring a
    /// re-serialized copy.
    IssueTextSplice {
        doc: String,
        index: u64,
        delete: u64,
        insert: String,
        #[serde(default)]
        base_len: Option<u64>,
    },
    /// Atomically replace one issue's whole description source.
    ///
    /// Two callers, one mechanism: moving a legacy (schema 0) body onto the
    /// document schema, and rewriting a schema-1 document into the canonical
    /// form an editor can address positionally. `expected` is a
    /// compare-and-swap over the exact source being replaced, so a concurrent
    /// edit refuses the rewrite instead of being overwritten.
    ///
    /// Normalization cannot use [`IssueIntent::IssueTextSplice`]: a document
    /// needing it is one whose offsets are not trustworthy, so repairing it
    /// with the primitive the mismatch breaks would be circular.
    IssueDocumentUpgrade {
        doc: String,
        expected: String,
        splices: Vec<DocumentSplice>,
        device: String,
        ts: u64,
    },
    /// A grouped activity marker for a completed burst of description edits.
    /// Text replication is intentionally not coupled to this bookkeeping op.
    IssueTextCheckpoint {
        doc: String,
        device: String,
        ts: u64,
    },
    IssueMove {
        doc: String,
        project: Option<String>,
        pos: Option<Pos>,
        device: String,
        ts: u64,
    },
    Assign {
        doc: String,
        who: Vec<String>,
        add: bool,
        device: String,
        ts: u64,
    },
    Label {
        doc: String,
        add: Vec<String>,
        new_labels: Vec<NewLabel>,
        remove: Vec<String>,
        device: String,
        ts: u64,
    },
    Comment {
        doc: String,
        body: String,
        /// Daemon-minted canonical comment id. Optional for wire compatibility
        /// with pre-identity intents; a comment stored without one cannot
        /// anchor reactions or replies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// The id of the comment being replied to, when this is a reply.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Comment on a span of an issue's collaborative text.
    ///
    /// Its own verb rather than an optional field on [`IssueIntent::Comment`]:
    /// it carries preconditions a plain comment has none of — the field must be
    /// collaborative text and the span must lie inside material that exists —
    /// and `Comment`'s field set is the wire form clients already write.
    ///
    /// The span arrives as offsets and NEVER as an encoded anchor. The World
    /// mints the anchor itself, which is what makes the stored anchor sound:
    /// `Anchor::decode_canonical` bounds neither `path` nor `offset`, and
    /// the `body` digest a correct anchor needs is computed by a substrate
    /// function no product can call — so a wire-supplied anchor is either
    /// unbounded or permanently drifted, and there is no third case.
    CommentAt {
        doc: String,
        body: String,
        /// The collaborative text path the span lies in.
        field: String,
        /// The span's start, in Unicode scalar offsets into `field` — the
        /// coordinates the convergence engine validates text ops in.
        start: u64,
        /// The span's end. Absent names a position rather than a span.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<u64>,
        /// Required here, unlike on [`IssueIntent::Comment`]. Reactions and
        /// replies already refuse to attach to an id-less comment; a span is
        /// the third thing a comment cannot carry without an identity of its
        /// own, because a reader that cannot name the comment cannot tell a
        /// caller which span moved.
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        /// Exact rendered publication whose scalar offsets are named.
        source: runtime::publication::WorldPublicationId,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Toggle one actor's emoji reaction on one comment. Deliberately writes
    /// **no history event**: a reaction is a social signal, not a change of
    /// record, and history rows for every 👍 would bury the changes that are.
    React {
        doc: String,
        /// The target comment's canonical id.
        comment: String,
        emoji: String,
        actor: String,
        /// `true` adds, `false` removes.
        on: bool,
        device: String,
        ts: u64,
    },
    SetTombstone {
        doc: String,
        on: bool,
        device: String,
        ts: u64,
    },
    Link {
        doc: String,
        kind: String,
        target: String,
        add: bool,
        device: String,
        ts: u64,
    },
    Parent {
        doc: String,
        parent: Option<String>,
        device: String,
        ts: u64,
    },
    WorkState {
        doc: String,
        action: WorkAction,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Start the product-owned verification of one pinned repository source.
    /// `run` is derived by the adapter from this action's persistent request
    /// coordinate and rechecked by the World against `Context::run_id(0)`.
    Verify {
        doc: String,
        run: String,
        source: String,
        build: String,
        /// True when the application package filled `build` because the caller
        /// omitted it. False means the caller named the Build.
        #[serde(default)]
        package_filled: bool,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Accept one returned verification Outcome into ordinary issue truth.
    AcceptCheck {
        doc: String,
        run: String,
        attempt: String,
        report: String,
        verdict: String,
        move_to_done: bool,
        id: String,
        actor: String,
        device: String,
        ts: u64,
    },
    ProjectNew {
        id: String,
        name: String,
        key: String,
        color: String,
        device: String,
        ts: u64,
    },
    LabelNew {
        id: String,
        name: String,
        color: String,
        device: String,
        ts: u64,
    },
    /// Rename and/or recolor a project in place. `key` is deliberately not
    /// editable — it seeds every alias. An in-place `map_set` over the same
    /// catalog key; LWW, `project.configure`-gated.
    ProjectEdit {
        id: String,
        name: Option<String>,
        color: Option<String>,
        description: Option<String>,
        lead: Option<String>,
        /// Outer `None` leaves the date untouched; inner `None` clears it.
        start_date: Option<Option<u64>>,
        target_date: Option<Option<u64>>,
        /// Soft-hide toggle: `None` leaves it, `Some(bool)` sets it (CUSTOM-9).
        archived: Option<bool>,
        /// Owning team id: `None` leaves it, `Some("")` clears (GOV-7).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        team: Option<String>,
        device: String,
        ts: u64,
    },
    /// Hard-delete an EMPTY project (CUSTOM-10 safe v1): refused while any
    /// issue — live or tombstoned — still carries its `projectid`, else every
    /// project-keyed catalog entry is removed. `project.delete`-gated (with
    /// the admin override).
    ProjectDelete { id: String, device: String, ts: u64 },
    /// Toggle one actor's subscription to an issue (INBOX-9). Like `React`,
    /// writes no history event — following is a personal signal, not a change
    /// of record.
    Follow {
        doc: String,
        actor: String,
        on: bool,
        device: String,
        ts: u64,
    },
    /// Create or edit a project milestone (SCOPE-1): the daemon mints the id
    /// on create; the whole record is rewritten so untouched fields survive.
    MilestoneSet {
        project_id: String,
        id: String,
        name: Option<String>,
        /// `None` leaves the body untouched; `Some("")` clears it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Outer `None` leaves the date; inner `None` clears it.
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        target_date: Option<Option<u64>>,
        /// Where to place it in the project's manual order. `None` leaves an
        /// existing milestone where it is and appends a new one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pos: Option<Pos>,
        tombstone: Option<bool>,
        device: String,
        ts: u64,
    },
    /// Point an issue at a milestone (or clear it).
    IssueMilestone {
        doc: String,
        milestone: Option<String>,
        device: String,
        ts: u64,
    },
    /// Create or edit a cycle (BOARD-11); same record shape as milestones.
    CycleSet {
        project_id: String,
        id: String,
        name: Option<String>,
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        start: Option<Option<u64>>,
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        end: Option<Option<u64>>,
        tombstone: Option<bool>,
        device: String,
        ts: u64,
    },
    /// Schedule an issue into a cycle (or clear it).
    IssueCycle {
        doc: String,
        cycle: Option<String>,
        device: String,
        ts: u64,
    },
    /// Create or edit an initiative (SCOPE-8). Membership changes are tuple
    /// deltas so one project edit never enumerates or rewrites the initiative's
    /// other memberships.
    InitiativeSet {
        id: String,
        name: Option<String>,
        description: Option<String>,
        owner: Option<String>,
        health: Option<String>,
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        target_date: Option<Option<u64>>,
        add_projects: Vec<String>,
        remove_projects: Vec<String>,
        tombstone: Option<bool>,
        device: String,
        ts: u64,
    },
    /// Create or edit a team (GOV-7). `key` binds at creation and is
    /// immutable after (it seeds nothing yet, but the project-key rule is the
    /// convention). Membership changes are independently addressed deltas.
    TeamSet {
        id: String,
        name: Option<String>,
        key: Option<String>,
        icon: Option<String>,
        lead: Option<String>,
        add_members: Vec<String>,
        remove_members: Vec<String>,
        tombstone: Option<bool>,
        device: String,
        ts: u64,
    },
    /// Report work into the triage intake queue (SCOPE-7) — outside every
    /// project workflow until reviewed.
    TriageSubmit {
        id: String,
        title: String,
        body: String,
        source: String,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Decide a pending triage item exactly once. `accepted` atomically
    /// creates the issue (`doc` = the daemon-minted DocId, `project`
    /// required) in the same transaction that stamps the outcome; `duplicate`
    /// names the existing issue in `doc`; `declined` needs neither.
    TriageDecide {
        id: String,
        outcome: String,
        project: Option<String>,
        doc: Option<String>,
        note: String,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Attach a bounded file to an issue (CREATE-5): a sealed record in the
    /// issue Body's `attachments` map, riding the existing sync and E2EE.
    Attach {
        doc: String,
        id: String,
        name: String,
        mime: String,
        /// The content this attachment *is*, already committed on the content
        /// plane.
        ///
        /// The engine never sees the bytes. It could not usefully: they are
        /// sealed under a content nonce it does not hold, and a World that
        /// handled plaintext would be a World that had to be trusted with it.
        /// What it does instead is name them, and the substrate refuses a
        /// declaration whose descriptor is not committed — which is what makes
        /// upload-then-attach an ordering the store enforces rather than a
        /// convention the product hopes for.
        content: String,
        /// Plaintext bytes, as the uploader saw them.
        ///
        /// Carried rather than derived because the engine has no way to ask the
        /// content plane anything. It is checked against the descriptor at the
        /// substrate boundary; here it is what the issue view reports, so a
        /// wrong value would be a wrong number on a screen, not a wrong file.
        size: u64,
        comment: Option<String>,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Remove an attachment record.
    Detach {
        doc: String,
        id: String,
        device: String,
        ts: u64,
    },
    /// Append an immutable status update to a project's feed (SCOPE-1). A
    /// grow-only `project_updates` log entry keyed `<project>/<id>`;
    /// `project.configure`-gated like the other project mutations.
    ProjectUpdatePost {
        project_id: String,
        id: String,
        author: String,
        body: String,
        health: String,
        device: String,
        ts: u64,
    },
    /// Rename and/or recolor a label in place. Issues reference labels by id,
    /// so a rename re-points every use for free. `catalog.label.configure`-gated.
    LabelEdit {
        id: String,
        name: Option<String>,
        color: Option<String>,
        device: String,
        ts: u64,
    },
    /// Remove a label from the registry. Ids left on issues resolve to the raw id
    /// (graceful degradation), so this is a hard `MapRemove`. `catalog.label.configure`-gated.
    LabelDelete { id: String, device: String, ts: u64 },
    /// Set the space's mutable display label. The genesis/seed id is
    /// name-independent, so this is a plain LWW `RegisterSet` on the catalog
    /// `name` — never touches identity. `demand_admin`-gated.
    SpaceRename {
        name: String,
        device: String,
        ts: u64,
    },
    /// Set (or clear, with an empty string) the space's overview description — a
    /// plain LWW `RegisterSet` on the catalog `description`, beside `name`
    /// (SCOPE-2). `demand_admin`-gated like the rename.
    SpaceDescribe {
        description: String,
        device: String,
        ts: u64,
    },
    /// Create a custom role definition (a grow-only Catalog revision with no
    /// predecessor). The daemon mints `role_id` (`role_<ULID>`); the World
    /// validates the registry membership of every capability for the declared
    /// scope.
    RoleCreate {
        role_id: String,
        /// `None` = a Space-scoped role; `Some(project)` = Project-scoped
        /// (the project must exist; capabilities must be Project-registered).
        scope_project: Option<String>,
        name: String,
        description: String,
        capabilities: Vec<String>,
        device: String,
        ts: u64,
    },
    /// Edit a custom role: a new revision whose predecessor is the exact
    /// expected head. Built-ins are immutable in every field.
    RoleEdit {
        role_id: String,
        expected_revision: String,
        name: Option<String>,
        description: Option<String>,
        capabilities: Option<Vec<String>>,
        device: String,
        ts: u64,
    },
    /// Tombstone a custom role (a complete revision; grow-only).
    RoleDelete {
        role_id: String,
        expected_revision: String,
        device: String,
        ts: u64,
    },
    /// Resolve concurrent role heads: a successor naming ALL current heads.
    RoleResolve {
        role_id: String,
        expected_heads: Vec<String>,
        /// The complete replacement body (product canonical JSON).
        body_json: String,
        device: String,
        ts: u64,
    },
    /// Replace a project's workflow: a new revision whose predecessors are
    /// exactly the current heads (also the conflict-resolution path).
    WorkflowReplace {
        project_id: String,
        expected_heads: Vec<String>,
        /// The complete replacement body (product canonical JSON).
        body_json: String,
        device: String,
        ts: u64,
    },
    /// Create one draft Spec Body with its first immutable revision.
    SpecCreate {
        spec: String,
        project: String,
        kind: crate::spec::Kind,
        title: String,
        text: String,
        links: Vec<crate::spec::Link>,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Create a draft successor to the one expected Spec head. An issued
    /// predecessor remains effective until another issued/withdrawn successor
    /// replaces it.
    SpecRevise {
        spec: String,
        expected: String,
        title: Option<String>,
        text: Option<String>,
        links: Option<Vec<crate::spec::Link>>,
        /// Outer `None` = preserve, `Some(None)` = remove, `Some(Some(_))` = replace.
        plan: Option<Option<crate::spec::PlanData>>,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Create a schema-only successor to one legacy Spec head while preserving
    /// its lifecycle state. This is separate from an ordinary revision because
    /// a storage migration must not turn issued truth back into a draft.
    SpecDocumentUpgrade {
        spec: String,
        expected: String,
        text: String,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Move the one expected Spec head through review/issue/withdraw. The
    /// transition itself is another immutable revision.
    SpecState {
        spec: String,
        expected: String,
        state: crate::spec::State,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Resolve concurrent Spec heads with one complete replacement body.
    SpecResolve {
        spec: String,
        expected_heads: Vec<String>,
        body_json: String,
        actor: String,
        device: String,
        ts: u64,
    },
    /// File one retractable note about the graph against a Spec.
    ///
    /// No `expected`: an Observation is not a revision and does not compete for
    /// the head, so two observers noting different things converge instead of
    /// refusing each other. See `spec::Observation` for why this is not a Link.
    SpecObserve {
        observation: String,
        spec: String,
        rel: crate::spec::Rel,
        target: crate::spec::Target,
        note: String,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Withdraw one Observation. The observer takes their own note back; anyone
    /// else needs the project's issuing capability.
    SpecRetract {
        spec: String,
        observation: String,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Create one draft Baseline pinning exact Spec revisions.
    BaselineCreate {
        baseline: String,
        project: String,
        name: String,
        members: Vec<crate::spec::SpecRef>,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Create a draft successor to the one expected Baseline head.
    BaselineRevise {
        baseline: String,
        expected: String,
        name: Option<String>,
        members: Option<Vec<crate::spec::SpecRef>>,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Move a Baseline through review/issue/withdraw.
    BaselineState {
        baseline: String,
        expected: String,
        state: crate::spec::State,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Resolve concurrent Baseline heads with one complete replacement body.
    BaselineResolve {
        baseline: String,
        expected_heads: Vec<String>,
        body_json: String,
        actor: String,
        device: String,
        ts: u64,
    },
    /// Pin one exact issued Baseline revision to an Issue, or clear the pin.
    IssueBaseline {
        doc: String,
        baseline: Option<crate::spec::BaselineRef>,
        device: String,
        ts: u64,
    },
}

/// Build the canonical `InitializeTracker` intent from captured formation
/// facts; the golden role/registry/workflow commitments come from this
/// release's reviewed definitions. The lifecycle adapter captures the inputs
/// ONCE and persists the signed action before submission.
pub fn initialize_tracker_intent(
    name: &str,
    ts: u64,
    project_id: &str,
    project_name: &str,
    project_key: &str,
    device: &str,
) -> IssueIntent {
    let mut built_in_roles = Vec::new();
    for id in crate::roles::BUILT_IN_ROLE_IDS {
        let rev = crate::roles::built_in(id).expect("built-in role");
        built_in_roles.push((
            id.to_string(),
            data_encoding::HEXLOWER.encode(&rev.revision_id),
            data_encoding::HEXLOWER.encode(&rev.body.definition_digest()),
        ));
    }
    IssueIntent::InitializeTracker {
        name: name.to_string(),
        ts,
        project_id: project_id.to_string(),
        project_name: project_name.to_string(),
        project_key: project_key.to_string(),
        device: device.to_string(),
        built_in_roles,
        capability_registry_commitment: data_encoding::HEXLOWER
            .encode(&capability_registry_commitment()),
        default_workflow_commitment: crate::workflow::default_workflow_revision(project_id)
            .revision_id,
    }
}

impl IssueIntent {
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("intent json")
    }
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

pub const DEFAULT_PAGE_SIZE: u32 = 100;
pub const MAX_PAGE_SIZE: u32 = 1_000;

/// One bounded continuation request. The cursor is an opaque Issues envelope
/// around Runtime's cursor and the full WorldPublicationId. The application
/// router uses that coordinate to enter the exact historical World before the
/// inner cursor is evaluated, so a continuation can never silently run under
/// a newer implementation or extractor declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    #[serde(default = "default_page_size")]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            limit: DEFAULT_PAGE_SIZE,
            cursor: None,
        }
    }
}

const fn default_page_size() -> u32 {
    DEFAULT_PAGE_SIZE
}

impl PageRequest {
    pub fn validate(&self) -> bool {
        (1..=MAX_PAGE_SIZE).contains(&self.limit)
            && self
                .cursor
                .as_ref()
                .is_none_or(|cursor| !cursor.is_empty() && cursor.len() <= 16 * 1_024)
    }
}

const PAGE_CURSOR_VERSION: u8 = 2;
const PAGE_CURSOR_BINDING_CONTEXT: &str = "lait.issues.page-continuation.v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageContinuation {
    version: u8,
    publication: runtime::publication::WorldPublicationId,
    cursor: String,
    binding: [u8; 32],
}

fn page_cursor_binding(
    publication: runtime::publication::WorldPublicationId,
    cursor: &str,
) -> Option<[u8; 32]> {
    let material = postcard::to_stdvec(&(PAGE_CURSOR_VERSION, publication, cursor)).ok()?;
    Some(blake3::derive_key(PAGE_CURSOR_BINDING_CONTEXT, &material))
}

/// Wrap one Runtime continuation in the exact portable publication that
/// produced it. The representation remains deliberately opaque to clients.
pub fn encode_page_cursor(
    publication: runtime::publication::WorldPublicationId,
    cursor: String,
) -> Option<String> {
    if cursor.is_empty() {
        return None;
    }
    let continuation = PageContinuation {
        version: PAGE_CURSOR_VERSION,
        publication,
        binding: page_cursor_binding(publication, &cursor)?,
        cursor,
    };
    let bytes = postcard::to_stdvec(&continuation).ok()?;
    (bytes.len() <= 12 * 1_024).then(|| data_encoding::BASE64URL_NOPAD.encode(&bytes))
}

/// Decode an Issues continuation and reject alternate encodings. The inner
/// Runtime cursor is returned only to product code; clients never learn its
/// transport representation.
pub fn decode_page_cursor(
    encoded: &str,
) -> Option<(runtime::publication::WorldPublicationId, String)> {
    let bytes = data_encoding::BASE64URL_NOPAD
        .decode(encoded.as_bytes())
        .ok()?;
    if data_encoding::BASE64URL_NOPAD.encode(&bytes) != encoded {
        return None;
    }
    let continuation: PageContinuation = postcard::from_bytes(&bytes).ok()?;
    if continuation.version != PAGE_CURSOR_VERSION
        || continuation.cursor.is_empty()
        || continuation.binding
            != page_cursor_binding(continuation.publication, &continuation.cursor)?
        || postcard::to_stdvec(&continuation).ok()? != bytes
    {
        return None;
    }
    Some((continuation.publication, continuation.cursor))
}

/// Exact coordinate selected by a continuation, for the application router.
pub fn page_publication(request: &PageRequest) -> Option<runtime::publication::WorldPublicationId> {
    request
        .cursor
        .as_deref()
        .and_then(decode_page_cursor)
        .map(|(publication, _)| publication)
}

/// Uniform response envelope for every collection whose cardinality is not
/// proven by a product singleton bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub publication: runtime::publication::WorldPublicationId,
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueDetailPages {
    pub comments: PageRequest,
    pub reactions: PageRequest,
    pub attachments: PageRequest,
    pub checks: PageRequest,
    pub outgoing_relations: PageRequest,
    pub incoming_relations: PageRequest,
}

impl Default for IssueDetailPages {
    fn default() -> Self {
        Self {
            comments: PageRequest::default(),
            reactions: PageRequest::default(),
            attachments: PageRequest::default(),
            checks: PageRequest::default(),
            outgoing_relations: PageRequest::default(),
            incoming_relations: PageRequest::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueDetailProjection {
    pub publication: runtime::publication::WorldPublicationId,
    pub issue: crate::dto::IssueView,
    pub comments: Page<crate::dto::CommentDto>,
    pub reactions: Page<crate::records::ReactionRecord>,
    pub attachments: Page<crate::dto::AttachmentMetaDto>,
    pub checks: Page<crate::dto::CheckDto>,
    pub outgoing_relations: Page<crate::dto::IssueRelationDto>,
    pub incoming_relations: Page<crate::dto::IssueRelationDto>,
}

/// One exact label record at the query publication. A tombstoned label is
/// represented by `None` so consumers can remove it without treating an
/// arbitrary first registry page as the complete label universe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelProjection {
    pub publication: runtime::publication::WorldPublicationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<crate::dto::LabelDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSummary {
    pub role_id: String,
    pub built_in: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_heads: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleProjection {
    pub summary: RoleSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<crate::views::StoredRoleRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowProjection {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<crate::workflow::WorkflowRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_heads: Vec<String>,
}

/// The product queries (read the committed snapshot; derive projections).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum IssueQuery {
    /// Migrator-package-only, constant-footprint lifecycle proof. This reads
    /// only the v4 migration marker and its retained audit tail; it never
    /// projects Catalog, Issues, Specs, or Baselines.
    V4MigrationStatus,
    /// Report which current Blueprint records still depend on compatibility
    /// readers instead of the structures this implementation writes natively.
    StructureStatus,
    /// Resolve one human selector through exact, publication-pinned Corpus
    /// facts. This is the shared selector primitive used by human adapters and
    /// agent ChangeSets; it never hydrates a tracker-wide catalog.
    Resolve {
        entity: ResolveEntity,
        selector: String,
        #[serde(default)]
        project: Option<String>,
    },
    View {
        doc: String,
        /// The viewer's actor (for `assignee_summary`), if known.
        me: Option<String>,
    },
    /// One bounded issue summary plus independently continued enrichment
    /// sections, all evaluated against this query's one exact publication.
    Detail {
        doc: String,
        me: Option<String>,
        pages: IssueDetailPages,
    },
    List {
        project: Option<String>,
        label: Option<String>,
        status: Option<String>,
        /// Already-resolved `mls_` id — the router resolves the name against
        /// the listed project, because only it holds the catalog.
        milestone: Option<String>,
        mine: Option<String>,
        all: bool,
        me: Option<String>,
        page: PageRequest,
    },
    Board {
        project: String,
        me: Option<String>,
        page: PageRequest,
    },
    /// Compile Plan morphology from canonical Issue facts at this query's one
    /// pinned World generation. `roots` are canonical `iss_` document ids; an
    /// empty vector compiles the whole project.
    Geometry {
        project: String,
        #[serde(default)]
        roots: Vec<String>,
        /// Optional bounded artifact page. Absence returns only readiness and
        /// summary; no query can accidentally serialize the full graph.
        #[serde(default)]
        page: Option<crate::geometry::GeometryPageRequest>,
    },
    History {
        doc: String,
        page: PageRequest,
    },
    /// One direction of the issue's durable relation/containment neighborhood.
    /// Directions page independently so a high-degree node remains bounded.
    Relations {
        doc: String,
        direction: crate::dto::RelationDirection,
        page: PageRequest,
    },
    /// Immutable comments, oldest first. Reactions are a separate tuple page.
    Comments {
        doc: String,
        page: PageRequest,
    },
    Reactions {
        doc: String,
        page: PageRequest,
    },
    Attachments {
        doc: String,
        page: PageRequest,
    },
    Checks {
        doc: String,
        page: PageRequest,
    },
    Projects {
        page: PageRequest,
    },
    /// A project's status-update feed, newest first (SCOPE-1).
    ProjectUpdates {
        project: String,
        page: PageRequest,
    },
    Labels {
        page: PageRequest,
    },
    /// Hydrate one label by stable id for operation-correlated terminal
    /// reconciliation. This is a unique indexed seek, never a registry scan.
    Label {
        label: String,
    },
    /// Every role definition: built-ins plus custom heads (with conflict
    /// head lists).
    Roles {
        page: PageRequest,
    },
    RoleShow {
        role: String,
    },
    /// The space-wide activity feed: every issue event across the tracker,
    /// ordered by `(ts, doc, entry id)`. `since` filters to rows the caller has
    /// not yet seen — pass back the `last` the previous pull returned, or
    /// `None` for the whole feed.
    ///
    /// The cursor is an opaque token naming a row, not a count of rows. It was
    /// a count: `seq` was a position in the feed and `since` was how far the
    /// caller had got. That only works while the feed is append-only, and the
    /// history log now trims — the moment an issue drops its oldest events,
    /// every position behind them shifts down, and a caller resuming from a
    /// remembered count silently skips exactly as many rows as were trimmed.
    /// A token built from `(ts, doc, entry id)` names the row itself, and an
    /// entry id survives trimming because it never described a position.
    Activity {
        page: PageRequest,
    },
    /// The authenticated principal's addressed-to-you inbox. Identity is
    /// derived from the outer Runtime action; clients cannot select another
    /// actor under the ordinary read demand.
    Inbox {
        exclude_device: Option<String>,
        page: PageRequest,
    },
    /// A project's workflow revision head(s).
    Workflow {
        project: String,
    },
    /// Every Spec, optionally restricted to one project.
    Specs {
        project: Option<String>,
        page: PageRequest,
    },
    /// One Spec including its current heads and issued coordinate.
    Spec {
        spec: String,
    },
    /// One bounded immutable-revision page of a Spec/Plan DAG.
    SpecHistory {
        spec: String,
        page: PageRequest,
    },
    /// Every Baseline, optionally restricted to one project.
    Baselines {
        project: Option<String>,
        page: PageRequest,
    },
    /// One Baseline including its current heads.
    Baseline {
        baseline: String,
    },
    /// One bounded immutable-revision page of a Baseline DAG.
    BaselineHistory {
        baseline: String,
        page: PageRequest,
    },
    /// One bounded page of typed Links asserted in scope.
    SpecReferences {
        project: Option<String>,
        page: PageRequest,
    },
    /// One bounded page of Observation assertions/retractions in scope.
    SpecObservations {
        project: Option<String>,
        page: PageRequest,
    },
    /// The deterministic effective brief for one Issue.
    Packet {
        doc: String,
    },
    /// A project's milestones with derived progress (SCOPE-1).
    Milestones {
        project: String,
        page: PageRequest,
    },
    /// A project's cycles with derived counts (BOARD-11).
    Cycles {
        project: String,
        page: PageRequest,
    },
    /// Every live initiative with its derived roll-up (SCOPE-8).
    Initiatives {
        page: PageRequest,
    },
    /// Every live team with its owned projects (GOV-7).
    Teams {
        page: PageRequest,
    },
    /// The triage intake queue, pending first (SCOPE-7).
    Triage {
        page: PageRequest,
    },
    /// One attachment's full record including the payload (CREATE-5).
    Attachment {
        doc: String,
        id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveEntity {
    Issue,
    Project,
    Label,
    Milestone,
    Cycle,
    Initiative,
    Team,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedEntity {
    pub id: String,
    /// Stable human rendering. For Issues this is the full collision-safe
    /// alias; for other entities it is their canonical id or key.
    pub display: String,
    #[serde(default)]
    pub record: serde_json::Value,
}

/// A bounded audit of Blueprint's current structural representation.
///
/// Historical Spec and Baseline revisions are deliberately absent. They are
/// immutable evidence, not pending current state, and revisions written before
/// generation coordinates remain readable under the documented live-morphology
/// rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureReport {
    pub generation: String,
    pub projects: u64,
    pub issues: u64,
    pub visible_edges: u64,
    pub visible_parents: u64,
    pub relation_bodies: u64,
    pub relation_projects_pending: u64,
    pub relation_edges_pending: u64,
    pub relation_parents_pending: u64,
    pub specs: u64,
    pub spec_heads_pending: u64,
    pub spec_conflicts: u64,
    pub plans_without_roots: u64,
    pub issue_documents_pending: u64,
    pub baselines: u64,
    /// Durable v3 -> v4 cursor and audit-tail verification. Present only in
    /// the separately installed migrator implementation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<MigrationVerification>,
    pub complete: bool,
}

/// Bounded proof that the migrator's mutable cursor and immutable audit tail
/// describe the same completed batch. The lifecycle host requires this proof
/// before it may activate the preferred v4 implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationVerification {
    pub batch: u64,
    pub cursor: String,
    pub marker_complete: bool,
    pub audit_records: u64,
    pub audit_tail_complete: bool,
    pub audit_tail_matches: bool,
    /// Every source admitted by the preferred extractor has a deterministic
    /// migrator phase. End-of-enumerator alone is never completion.
    pub source_coverage_complete: bool,
    /// The cursor enumerated one immutable source publication rather than the
    /// changing migration head. Until Runtime supplies that exact-source
    /// lifecycle capability, completion must remain pending.
    pub source_snapshot_pinned: bool,
    /// Portable semantic publication and causal frontier retained by the
    /// generic lifecycle host. A later Contact extension is compared against
    /// this cut and re-enters the consented migrator instead of becoming
    /// silently invisible under preferred v4.
    pub source_publication: runtime::publication::PublicationId,
    pub source_frontier: replica::frontier::ReplicaFrontier,
}

impl MigrationVerification {
    pub fn verified(&self) -> bool {
        self.marker_complete
            && self.audit_records > 0
            && self.audit_tail_complete
            && self.audit_tail_matches
            && self.source_coverage_complete
            && self.source_snapshot_pinned
    }
}

/// Publication-pinned, bounded geometry response. The compact artifact never
/// crosses the World boundary wholesale; callers receive its readiness,
/// constant-size summary, and at most one explicitly requested page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeometryProjection {
    pub key: crate::geometry::GeometryArtifactKey,
    pub source: runtime::publication::WorldPublicationId,
    pub estimate: crate::geometry::GeometryEstimate,
    pub readiness: crate::geometry::GeometryReadiness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<crate::geometry::GeometrySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<crate::geometry::GeometryPage>,
}

impl IssueQuery {
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("query json")
    }
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Inline semantic input committed into an `issue.verify/v1` Start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyInput {
    pub doc: String,
    pub source: String,
}

impl VerifyInput {
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Canonical inline result of an `issue.verify/v1` handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyOutput {
    pub verdict: String,
}

/// One issue-owned binding to Runtime lifecycle truth.
///
/// The map key is the Run id. This record deliberately repeats enough of the
/// Start coordinates for an older Issues build to explain the check even when
/// it cannot project the protected Run Body. Runtime remains authoritative for
/// every lifecycle fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRecord {
    pub spec: String,
    pub v: u32,
    pub build: String,
    pub source: String,
    pub state: String,
    pub by: String,
    pub ts: u64,
    /// The application package named this Build because the caller omitted it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub package_filled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
}

/// The effect every mutating intent returns: the DocId(s) it touched (the
/// daemon renders the canonical reff).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueEffect {
    pub doc: Option<String>,
    /// The stable Runtime Run bound by this product action, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Whether the intent was an idempotent no-op (nothing staged).
    #[serde(default)]
    pub unchanged: bool,
    /// Ordered per-operation effects for [`IssueIntent::ChangeSet`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<ChangeResult>,
}

impl IssueEffect {
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("effect json")
    }
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// One durable history event appended to an issue's `events` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueEvent {
    /// The request kind (`created`, `edited`, `assigned`, …).
    pub k: String,
    /// The committing device (advisory attribution).
    ///
    /// Kept, and not what a reader is shown: `IssueQuery::Inbox` filters on it to
    /// answer "what happened that was not me on this machine", which an actor
    /// cannot answer because a person's other device is still them.
    pub d: String,
    /// The acting actor.
    ///
    /// Absent-means-absent, so every event written before this field re-encodes
    /// byte-identically and reads back as "no name" — which the viewer already
    /// renders honestly rather than inventing one.
    ///
    /// This is what a person is shown. The device was, and a device id is not an
    /// actor id: the lookup that resolves a display name is keyed by actor, so
    /// it missed on every row and every author fell back to a hex prefix,
    /// coloured by hashing that hex — a different colour from the same person's
    /// roster chip.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub a: String,
    /// Unix seconds.
    pub t: u64,
    /// Field changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub c: Vec<EventChange>,
    /// Free text (comment body, link summary).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub x: String,
    /// The engine's entry id for this event, filled in by the projection and
    /// never stored — see [`StoredComment::node`] for why a `skip` and not a
    /// skipped-if-none field. It is what an activity cursor names: a position
    /// cannot be, because trimming the log renumbers every event behind it.
    #[serde(skip)]
    pub entry: String,
}

impl IssueEvent {
    /// Product notification class for events that enter an addressed inbox.
    /// Keeping this normalization beside the durable event prevents the
    /// writer and extractor from disagreeing about which events need bounded
    /// recipient facts.
    pub fn inbox_kind(&self) -> Option<&'static str> {
        match self.k.as_str() {
            "assigned" => Some("assigned"),
            "commented" => Some("comment"),
            "started" | "finished" | "stopped" => Some("status"),
            "edited" if self.c.iter().any(|change| change.f == "status") => Some("status"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventChange {
    pub f: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// A stored comment list element.
///
/// `id`/`parent` arrived after v0.6 comments shipped, and `at` after them, so
/// all three are optional with absent-means-absent serialization: pre-existing
/// comments keep their exact stored bytes, and older builds deserialize
/// enriched comments unchanged (serde ignores unknown fields). A comment
/// without an `id` predates identity and simply cannot anchor reactions or
/// replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredComment {
    pub a: String,
    pub t: u64,
    pub b: String,
    /// Canonical comment id (`cmt_…`, lowercase — see [`is_comment_id`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The comment this one replies to (one level; a reply to a reply names
    /// the same root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Where in the issue's collaborative text this comment is attached.
    /// Absent on an ordinary comment, which is most of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<StoredAnchor>,
    /// The engine's node id for this comment, filled in by the projection and
    /// never stored. `None` for a comment still living in the legacy
    /// `list:comments`, which has no node to name.
    ///
    /// `skip` rather than `skip_serializing_if`: this is not a field of the
    /// record at all, it is where the record was found. Serializing it would
    /// put an engine handle inside product bytes that outlive the engine
    /// position it names, and every stored comment's bytes would change.
    #[serde(skip)]
    pub node: Option<String>,
    /// The parent as the hierarchy actually holds it, filled in by the
    /// projection. Authoritative over [`Self::parent`], which is written
    /// alongside it so an older build reads the same thread out of the same
    /// bytes.
    #[serde(skip)]
    pub parent_node: Option<String>,
}

/// A comment's durable attachment to a span of collaborative text.
///
/// What is stored is the ANCHOR, never a resolved offset. An offset is true of
/// one version of the Body and of no other; the anchors are the only form that
/// survives the edits made after the comment. See
/// [`crate::dto::CommentAnchorState`] for the read half of that rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAnchor {
    /// The collaborative text path the span lies in — see
    /// [`crate::views::IssueState::anchorable_text`], which is the list of
    /// paths this build will anchor into and the reason there is a list.
    pub field: String,
    /// The span's start, a hex-encoded [`fabric::Anchor`].
    ///
    /// Hex rather than the raw bytes because the record is JSON and
    /// `serde_json` writes a `Vec<u8>` as a decimal array — about four bytes of
    /// record per byte of anchor.
    pub start: String,
    /// The span's end. Absent means the comment names a position rather than a
    /// span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
}

/// The default workflow, exactly the legacy seed.
pub fn default_workflow() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"id":"backlog","name":"Backlog","category":"backlog","color":"gray"}),
        serde_json::json!({"id":"in_progress","name":"In Progress","category":"active","color":"blue"}),
        serde_json::json!({"id":"in_review","name":"In Review","category":"active","color":"yellow"}),
        serde_json::json!({"id":"done","name":"Done","category":"done","color":"green"}),
    ]
}

#[cfg(test)]
mod verify_contract_tests {
    use super::*;

    fn report(byte: u8) -> replica::content::ContentRef {
        replica::content::ContentRef {
            content_id: [byte; 32],
        }
    }

    #[test]
    fn verdict_report_and_geometry_are_all_bound_into_the_output_digest() {
        let pass = verify_candidate("pass", report(1), 100).unwrap();
        let fail = verify_candidate("fail", report(1), 100).unwrap();
        let other_report = verify_candidate("pass", report(2), 100).unwrap();
        let other_size = verify_candidate("pass", report(1), 101).unwrap();

        assert_eq!(pass.inline, br#"{"verdict":"pass"}"#);
        assert_eq!(pass.terminal, runtime::exec::TerminalClass::Succeeded);
        assert_ne!(pass.digest().unwrap(), fail.digest().unwrap());
        assert_ne!(pass.digest().unwrap(), other_report.digest().unwrap());
        assert_ne!(pass.digest().unwrap(), other_size.digest().unwrap());
        assert!(verify_candidate("unknown", report(1), 100).is_none());
    }
}

#[cfg(test)]
mod product_bound_tests {
    use super::*;

    #[test]
    fn issue_and_entity_text_bounds_are_utf8_byte_bounds() {
        assert!(valid_title("a"));
        assert!(!valid_title("   "));
        assert!(valid_title(&"x".repeat(MAX_TITLE_BYTES)));
        assert!(!valid_title(&"é".repeat(MAX_TITLE_BYTES)));
        assert!(valid_name(&"n".repeat(MAX_NAME_BYTES)));
        assert!(!valid_name(&"n".repeat(MAX_NAME_BYTES + 1)));
        assert!(valid_text(&"x".repeat(MAX_TEXT_BYTES)));
        assert!(!valid_text(&"x".repeat(MAX_TEXT_BYTES + 1)));
    }

    #[test]
    fn page_continuation_pins_the_complete_world_publication() {
        let publication = runtime::publication::WorldPublicationId::new(
            runtime::publication::PublicationId::new(
                [1; 32],
                [2; 32],
                runtime::publication::ExtractorSchemaDigest::from_digest([3; 32]),
            ),
            runtime::publication::MaterializationId::from_u64(7).unwrap(),
        );
        let encoded = encode_page_cursor(publication, "runtime-cursor".into()).unwrap();
        assert_eq!(
            decode_page_cursor(&encoded),
            Some((publication, "runtime-cursor".into()))
        );
        assert_eq!(
            page_publication(&PageRequest {
                limit: 10,
                cursor: Some(encoded.clone()),
            }),
            Some(publication)
        );

        let mut tampered = encoded.into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        assert!(decode_page_cursor(std::str::from_utf8(&tampered).unwrap()).is_none());
    }

    #[test]
    fn migration_window_cursor_rejects_body_and_subitem_tampering() {
        let body = BodyKey::new(world_id(), BodyId::from_bytes([0x41; 16]));
        let cursor = V4MigrationWindow::render_cursor("issue", Some(&body), "21:comment:list:0")
            .expect("canonical migration cursor");
        let window = V4MigrationWindow {
            phase: "issue".into(),
            body: Some(body.clone()),
            subitem: "21:comment:list:0".into(),
            digest: [0x42; 32],
            cursor: cursor.clone(),
        };
        assert!(window.valid());

        let mut wrong_body = window.clone();
        wrong_body.body = Some(BodyKey::new(world_id(), BodyId::from_bytes([0x43; 16])));
        assert!(!wrong_body.valid());

        let mut wrong_subitem = window.clone();
        wrong_subitem.subitem = "21:comment:list:1".into();
        assert!(!wrong_subitem.valid());

        let mut wrong_digest = window;
        wrong_digest.digest = [0; 32];
        assert!(!wrong_digest.valid());

        let parsed = V4MigrationWindow::parse_cursor(&cursor).expect("round trip cursor");
        assert_eq!(parsed.0, "issue");
        assert_eq!(parsed.1, Some(body.body));
        assert_eq!(parsed.2, "21:comment:list:0");
    }
}

#[cfg(test)]
mod stored_comment_tests {
    use super::*;

    /// A comment written before spans existed decodes unchanged, and re-encodes
    /// to the same bytes.
    ///
    /// This is what "additive" has to mean for a record that is already in
    /// Bodies in the field: absent `at` is absent, not `null`, so a peer running
    /// this build and a peer running the previous one agree byte for byte about
    /// every comment neither of them attached to anything.
    #[test]
    fn a_comment_written_before_spans_existed_is_byte_neutral() {
        let stored = br#"{"a":"act_1","t":7,"b":"hello","id":"cmt_1"}"#;
        let comment: StoredComment = serde_json::from_slice(stored).expect("decode");
        assert!(comment.at.is_none());
        assert_eq!(serde_json::to_vec(&comment).expect("encode"), stored);
    }

    /// A point attachment does not serialize an `end`, so the two forms stay
    /// distinguishable on the wire.
    #[test]
    fn a_point_attachment_omits_the_end() {
        let comment = StoredComment {
            a: "act_1".into(),
            t: 7,
            b: "hello".into(),
            id: Some("cmt_1".into()),
            parent: None,
            at: Some(StoredAnchor {
                field: "description".into(),
                start: "ab".into(),
                end: None,
            }),
            node: None,
            parent_node: None,
        };
        let json = serde_json::to_string(&comment).expect("encode");
        assert_eq!(
            json,
            r#"{"a":"act_1","t":7,"b":"hello","id":"cmt_1","at":{"field":"description","start":"ab"}}"#
        );
        assert_eq!(
            serde_json::from_str::<StoredComment>(&json).expect("decode"),
            comment
        );
    }
}
