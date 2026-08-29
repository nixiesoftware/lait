//! Versioned project specifications and their issued baselines.
//!
//! The module supplies the context, so the durable nouns stay sharp:
//! [`Spec`], [`Revision`], [`Baseline`], [`Link`], and the derived [`Packet`].
//! An Issue remains work. A Spec says why or what governs that work; a
//! Baseline pins exact revisions; a Packet is the effective read model for one
//! Issue and is never replicated truth.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_TITLE_BYTES: usize = 256;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_LINKS: usize = 256;
pub const MAX_MEMBERS: usize = 1024;
pub const MAX_PLAN_ROOTS: usize = 32;
pub const MAX_PREDECESSORS: usize = 8;

const SPEC_REVISION_CONTEXT: &str = "lait.issues.spec-revision.v1";
const BASELINE_REVISION_CONTEXT: &str = "lait.issues.baseline-revision.v1";

// `runtime::publication::PublicationId` deliberately has no dependency on
// schemars. This private mirror describes its serde shape for the product
// contract without creating a second durable coordinate type.
#[derive(JsonSchema)]
#[allow(dead_code, reason = "schema-only mirror for PublicationId")]
struct PublicationSchema {
    manifest_root: [u8; 32],
    implementation_digest: [u8; 32],
    extractor_schema_digest: [u8; 32],
}

/// What one Spec contributes to the lifecycle.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Goal,
    Requirement,
    Plan,
    Design,
    Order,
    Guide,
    Proof,
    Verdict,
    Waiver,
    Record,
}

impl Kind {
    pub const ALL: [Self; 10] = [
        Self::Goal,
        Self::Requirement,
        Self::Plan,
        Self::Design,
        Self::Order,
        Self::Guide,
        Self::Proof,
        Self::Verdict,
        Self::Waiver,
        Self::Record,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::Requirement => "requirement",
            Self::Plan => "plan",
            Self::Design => "design",
            Self::Order => "order",
            Self::Guide => "guide",
            Self::Proof => "proof",
            Self::Verdict => "verdict",
            Self::Waiver => "waiver",
            Self::Record => "record",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }

    /// Advice and evidence remain visible in a Packet but never enter its
    /// governing set merely because they were issued.
    pub const fn governs(self) -> bool {
        matches!(
            self,
            Self::Requirement | Self::Design | Self::Order | Self::Waiver
        )
    }
}

/// Review state of one immutable revision.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Draft,
    Review,
    Issued,
    Withdrawn,
}

impl State {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Review => "review",
            Self::Issued => "issued",
            Self::Withdrawn => "withdrawn",
        }
    }
}

/// The meaning of one exact directed relation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Rel {
    Derives,
    Decomposes,
    Implements,
    Governs,
    Amends,
    Supersedes,
    Clarifies,
    Incorporates,
    References,
    Verifies,
    Validates,
    Waives,
    Records,
    Conflicts,
    Depends,
}

impl Rel {
    pub const ALL: [Self; 15] = [
        Self::Derives,
        Self::Decomposes,
        Self::Implements,
        Self::Governs,
        Self::Amends,
        Self::Supersedes,
        Self::Clarifies,
        Self::Incorporates,
        Self::References,
        Self::Verifies,
        Self::Validates,
        Self::Waives,
        Self::Records,
        Self::Conflicts,
        Self::Depends,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Derives => "derives",
            Self::Decomposes => "decomposes",
            Self::Implements => "implements",
            Self::Governs => "governs",
            Self::Amends => "amends",
            Self::Supersedes => "supersedes",
            Self::Clarifies => "clarifies",
            Self::Incorporates => "incorporates",
            Self::References => "references",
            Self::Verifies => "verifies",
            Self::Validates => "validates",
            Self::Waives => "waives",
            Self::Records => "records",
            Self::Conflicts => "conflicts",
            Self::Depends => "depends",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rel| rel.as_str() == raw)
    }
}

impl State {
    pub const ALL: [Self; 4] = [Self::Draft, Self::Review, Self::Issued, Self::Withdrawn];

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == raw)
    }
}

/// An exact Spec revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecRef {
    pub spec: String,
    pub revision: String,
}

/// An exact Baseline revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineRef {
    pub baseline: String,
    pub revision: String,
}

/// What a Link names. Spec and Baseline targets always pin an exact revision;
/// an Issue is already a stable identity whose changing work state is not a
/// governing document revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Spec { spec: String, revision: String },
    Baseline { baseline: String, revision: String },
    Issue { issue: String },
}

/// One typed relation carried by the revision that asserts it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub rel: Rel,
    pub target: Target,
}

/// The seed of a Plan's derived Issue geometry.
///
/// A Plan does not serialize phases, membership, positions, or a drawing. Those
/// facts already exist in the Issue graph and its metadata. `roots` names the
/// few Issues from which the compiler discovers the connected morphology; an
/// empty set means the Plan's whole project. This keeps authoring as light as a
/// document plus an Issue reference while making every rendered position a
/// reproducible consequence of canonical facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PlanData {
    #[serde(default)]
    pub roots: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanDataWire {
    #[serde(default)]
    roots: Vec<String>,
    // Read-only decoder for the deterministic phase model shipped before Plan
    // morphology. A successor serializes only `roots`; old phase membership is
    // collapsed into seeds and the Issue graph supplies all ordering thereafter.
    #[serde(default)]
    phases: Vec<LegacyPlanPhase>,
    #[serde(default)]
    placement: Option<usize>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyPlanPhase {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    milestone: Option<String>,
    #[serde(default)]
    issues: Vec<String>,
}

impl From<PlanDataWire> for PlanData {
    fn from(wire: PlanDataWire) -> Self {
        let mut roots = wire.roots;
        if roots.is_empty() {
            roots.extend(wire.phases.into_iter().flat_map(|phase| phase.issues));
        }
        roots.sort();
        roots.dedup();
        let _ = wire.placement;
        Self { roots }
    }
}

impl<'de> Deserialize<'de> for PlanData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        PlanDataWire::deserialize(deserializer).map(Self::from)
    }
}

impl PlanData {
    pub fn validate(&self) -> Result<(), String> {
        if self.roots.len() > MAX_PLAN_ROOTS {
            return Err("too many Plan roots".into());
        }
        let mut roots = BTreeSet::new();
        for root in &self.roots {
            if crate::ids::DocId::parse(root).is_none() || !roots.insert(root.as_str()) {
                return Err("Plan roots are invalid or duplicated".into());
            }
        }
        Ok(())
    }
}

/// Canonical content of one Spec revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Body {
    pub spec: String,
    pub project: String,
    pub kind: Kind,
    /// Full portable World publication against which this revision was
    /// composed. A Manifest root alone cannot name the implementation or
    /// extractor semantics that gave the Plan its meaning.
    #[schemars(with = "PublicationSchema")]
    pub publication: runtime::publication::PublicationId,
    pub title: String,
    #[serde(default)]
    pub text: String,
    pub state: State,
    #[serde(default)]
    pub links: Vec<Link>,
    /// Present only for `Kind::Plan`. It is a seed, never stored layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanData>,
    pub author: String,
    pub ts: u64,
}

impl Body {
    pub fn canonicalize(&mut self) {
        self.links.sort();
        self.links.dedup();
    }

    pub fn validate(&self) -> Result<(), String> {
        if crate::ids::SpecId::parse(&self.spec).is_none() {
            return Err("invalid Spec id".into());
        }
        if crate::ids::ProjectId::parse(&self.project).is_none() {
            return Err("invalid Project id".into());
        }
        if self.publication.implementation_digest == [0; 32]
            || self.publication.extractor_schema_digest.digest() == [0; 32]
        {
            return Err("invalid World publication".into());
        }
        let title = self.title.trim();
        if title.is_empty() || self.title.len() > MAX_TITLE_BYTES {
            return Err("Spec title is empty or too long".into());
        }
        if self.text.len() > MAX_TEXT_BYTES {
            return Err("Spec text is too long".into());
        }
        if mechanics::ids::ActorId::parse(&self.author).is_none() {
            return Err("invalid Spec author".into());
        }
        if self.links.len() > MAX_LINKS {
            return Err("too many Spec links".into());
        }
        let mut canonical = self.links.clone();
        canonical.sort();
        canonical.dedup();
        if canonical != self.links {
            return Err("Spec links are not sorted and unique".into());
        }
        for link in &self.links {
            validate_target(&link.target)?;
        }
        if self.plan.is_some() && self.kind != Kind::Plan {
            return Err("structured Plan data belongs only on a Plan Spec".into());
        }
        if self.kind == Kind::Plan && self.plan.is_none() {
            return Err("Plan revision is missing its bounded root selection".into());
        }
        if let Some(plan) = &self.plan {
            plan.validate()?;
        }
        Ok(())
    }
}

/// One immutable Spec revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub revision: String,
    #[serde(default)]
    pub predecessors: Vec<String>,
    pub body: Body,
}

/// Parsed state of one Spec Body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spec {
    pub revisions: Vec<Revision>,
    /// Notes filed against this document, in a set beside the revision map
    /// rather than inside any revision. See `Observation`.
    pub observations: Vec<Observation>,
    /// V4 head coordinates live in a small add-wins head Body. When present,
    /// they are authoritative and let a write hydrate only current heads rather
    /// than anti-join the complete revision history.
    pub explicit_heads: Vec<String>,
    /// Effective issued coordinates maintained beside heads. Draft successors
    /// do not force an ancestry scan merely to discover the governing revision.
    pub explicit_issued: Vec<String>,
}

impl Spec {
    pub fn from_view(view: &fabric::CollaborativeView) -> Self {
        let mut revisions: Vec<Revision> = view
            .maps
            .get("revisions")
            .into_iter()
            .flat_map(|map| map.values())
            .filter_map(|raw| serde_json::from_slice(raw).ok())
            .collect();
        revisions.sort_by(|a, b| a.revision.cmp(&b.revision));
        let mut observations: Vec<Observation> = view
            .sets
            .get("observations")
            .into_iter()
            .flatten()
            .filter_map(|raw| serde_json::from_slice(raw).ok())
            .collect();
        // By when they were noticed, then by id so the order is total: a set is
        // unordered by construction, and a reader scrolling notes wants the
        // thread they were written in.
        observations.sort_by(|a, b| (a.ts, &a.observation).cmp(&(b.ts, &b.observation)));
        Self {
            revisions,
            observations,
            explicit_heads: Vec::new(),
            explicit_issued: Vec::new(),
        }
    }

    pub fn observation(&self, id: &str) -> Option<&Observation> {
        self.observations
            .iter()
            .find(|entry| entry.observation == id)
    }

    pub fn heads(&self) -> Vec<&Revision> {
        if !self.explicit_heads.is_empty() {
            return self
                .explicit_heads
                .iter()
                .filter_map(|id| self.revision(id))
                .collect();
        }
        heads(&self.revisions, |revision| {
            (&revision.revision, &revision.predecessors)
        })
    }

    pub fn one_head(&self) -> Option<&Revision> {
        let heads = self.heads();
        if heads.len() == 1 {
            heads.first().copied()
        } else {
            None
        }
    }

    /// The currently effective issued revision. Draft/review descendants do
    /// not invalidate it. A later issued revision supersedes it; a later
    /// withdrawal ends it. Concurrent controlling revisions are a conflict.
    pub fn issued(&self) -> Issued<'_> {
        // A physical v4 heads Body makes even an empty issued set
        // authoritative.  Falling back to DAG ancestry here would resurrect a
        // withdrawn issuance simply because the set is empty.
        if !self.explicit_heads.is_empty() {
            let controls = self
                .explicit_issued
                .iter()
                .filter_map(|id| self.revision(id))
                .collect::<Vec<_>>();
            return match controls.as_slice() {
                [] => Issued::None,
                [one] if one.body.state == State::Issued => Issued::One(one),
                [one] if one.body.state == State::Withdrawn => Issued::None,
                _ => Issued::Conflict(controls),
            };
        }
        let controls: Vec<&Revision> = self
            .revisions
            .iter()
            .filter(|revision| matches!(revision.body.state, State::Issued | State::Withdrawn))
            .collect();
        let maximal: Vec<&Revision> = controls
            .iter()
            .copied()
            .filter(|candidate| {
                !controls.iter().any(|other| {
                    other.revision != candidate.revision
                        && descends(
                            &self.revisions,
                            &other.revision,
                            &candidate.revision,
                            |revision| (&revision.revision, &revision.predecessors),
                        )
                })
            })
            .collect();
        match maximal.as_slice() {
            [] => Issued::None,
            [one] if one.body.state == State::Issued => Issued::One(one),
            [one] if one.body.state == State::Withdrawn => Issued::None,
            _ => Issued::Conflict(maximal),
        }
    }

    pub fn revision(&self, id: &str) -> Option<&Revision> {
        self.revisions
            .iter()
            .find(|revision| revision.revision == id)
    }
}

pub enum Issued<'a> {
    None,
    One(&'a Revision),
    Conflict(Vec<&'a Revision>),
}

/// Canonical content of one Baseline revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineBody {
    pub baseline: String,
    pub project: String,
    pub name: String,
    pub state: State,
    #[serde(default)]
    pub members: Vec<SpecRef>,
    pub author: String,
    pub ts: u64,
}

impl BaselineBody {
    pub fn canonicalize(&mut self) {
        self.members.sort();
        self.members.dedup();
    }

    pub fn validate(&self) -> Result<(), String> {
        if crate::ids::BaselineId::parse(&self.baseline).is_none() {
            return Err("invalid Baseline id".into());
        }
        if crate::ids::ProjectId::parse(&self.project).is_none() {
            return Err("invalid Project id".into());
        }
        if self.name.trim().is_empty() || self.name.len() > MAX_TITLE_BYTES {
            return Err("Baseline name is empty or too long".into());
        }
        if mechanics::ids::ActorId::parse(&self.author).is_none() {
            return Err("invalid Baseline author".into());
        }
        if self.members.len() > MAX_MEMBERS {
            return Err("too many Baseline members".into());
        }
        let mut canonical = self.members.clone();
        canonical.sort();
        canonical.dedup();
        if canonical != self.members {
            return Err("Baseline members are not sorted and unique".into());
        }
        for member in &self.members {
            if crate::ids::SpecId::parse(&member.spec).is_none()
                || decode_revision(&member.revision).is_none()
            {
                return Err("invalid Baseline member".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineRevision {
    pub revision: String,
    #[serde(default)]
    pub predecessors: Vec<String>,
    pub body: BaselineBody,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Baseline {
    pub revisions: Vec<BaselineRevision>,
    pub explicit_heads: Vec<String>,
    pub explicit_issued: Vec<String>,
}

impl Baseline {
    pub fn from_view(view: &fabric::CollaborativeView) -> Self {
        let mut revisions: Vec<BaselineRevision> = view
            .maps
            .get("revisions")
            .into_iter()
            .flat_map(|map| map.values())
            .filter_map(|raw| serde_json::from_slice(raw).ok())
            .collect();
        revisions.sort_by(|a, b| a.revision.cmp(&b.revision));
        Self {
            revisions,
            explicit_heads: Vec::new(),
            explicit_issued: Vec::new(),
        }
    }

    pub fn heads(&self) -> Vec<&BaselineRevision> {
        if !self.explicit_heads.is_empty() {
            return self
                .explicit_heads
                .iter()
                .filter_map(|id| self.revision(id))
                .collect();
        }
        heads(&self.revisions, |revision| {
            (&revision.revision, &revision.predecessors)
        })
    }

    pub fn one_head(&self) -> Option<&BaselineRevision> {
        let heads = self.heads();
        if heads.len() == 1 {
            heads.first().copied()
        } else {
            None
        }
    }

    pub fn issued(&self) -> BaselineIssued<'_> {
        // As for Specs, presence of physical heads means an empty issued set
        // is a real state (withdrawn), not a request for legacy DAG inference.
        if !self.explicit_heads.is_empty() {
            let controls = self
                .explicit_issued
                .iter()
                .filter_map(|id| self.revision(id))
                .collect::<Vec<_>>();
            return match controls.as_slice() {
                [] => BaselineIssued::None,
                [one] if one.body.state == State::Issued => BaselineIssued::One(one),
                [one] if one.body.state == State::Withdrawn => BaselineIssued::None,
                _ => BaselineIssued::Conflict(controls),
            };
        }
        let controls: Vec<&BaselineRevision> = self
            .revisions
            .iter()
            .filter(|revision| matches!(revision.body.state, State::Issued | State::Withdrawn))
            .collect();
        let maximal: Vec<&BaselineRevision> = controls
            .iter()
            .copied()
            .filter(|candidate| {
                !controls.iter().any(|other| {
                    other.revision != candidate.revision
                        && descends(
                            &self.revisions,
                            &other.revision,
                            &candidate.revision,
                            |revision| (&revision.revision, &revision.predecessors),
                        )
                })
            })
            .collect();
        match maximal.as_slice() {
            [] => BaselineIssued::None,
            [one] if one.body.state == State::Issued => BaselineIssued::One(one),
            [one] if one.body.state == State::Withdrawn => BaselineIssued::None,
            _ => BaselineIssued::Conflict(maximal),
        }
    }

    pub fn revision(&self, id: &str) -> Option<&BaselineRevision> {
        self.revisions
            .iter()
            .find(|revision| revision.revision == id)
    }
}

pub enum BaselineIssued<'a> {
    None,
    One(&'a BaselineRevision),
    Conflict(Vec<&'a BaselineRevision>),
}

/// Stable external view of one Spec and all current coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecView {
    pub spec: String,
    pub project: String,
    pub kind: Kind,
    pub title: String,
    pub state: State,
    pub revision: String,
    pub heads: Vec<String>,
    #[serde(default)]
    pub issued: Vec<String>,
    pub body: Body,
}

/// The one head of a register row, as its corpus row states it.
///
/// What a register draws about a document -- its title, where its head stands,
/// who last wrote it and when -- read from the row the revision posts rather
/// than from its Body. Text, links and topology are deliberately absent: they
/// are the document, and a reader opens one (`Spec`) or pages its history
/// (`SpecHistory`) to see them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecHead {
    pub revision: String,
    pub title: String,
    pub state: State,
    pub author: String,
    pub ts: u64,
}

/// Bounded collection row for one Spec/Plan. Revision text and topology are
/// deliberately absent: callers page those immutable records through
/// `SpecHistory`. Concurrent heads remain explicit rather than being flattened
/// into an invented current title or state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecSummary {
    pub spec: String,
    pub project: String,
    pub kind: Kind,
    pub heads: Vec<String>,
    pub issued: Vec<String>,
    pub conflicted: bool,
    /// Present only when exactly one head exists, so nothing here chooses among
    /// concurrent intent. Absent, never defaulted, when the corpus has not
    /// posted that head yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<SpecHead>,
}

/// Stable external view of one Baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineView {
    pub baseline: String,
    pub project: String,
    pub name: String,
    pub state: State,
    pub revision: String,
    pub heads: Vec<String>,
    #[serde(default)]
    pub issued: Vec<String>,
    pub body: BaselineBody,
}

/// The one head of a Baseline register row, as its corpus row states it.
/// Same shape as [`SpecHead`]; the members are the document, and a reader
/// opens one (`Baseline`) to see them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineHead {
    pub revision: String,
    pub name: String,
    pub state: State,
    pub author: String,
    pub ts: u64,
}

/// Bounded collection row for one Baseline. The immutable revision records are
/// a separate cursor page, preserving conflicts without revisiting the whole
/// document DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaselineSummary {
    pub baseline: String,
    pub project: String,
    pub heads: Vec<String>,
    pub issued: Vec<String>,
    pub conflicted: bool,
    /// Present only when exactly one head exists; absent when the corpus has
    /// not posted that head yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<BaselineHead>,
}

/// How an exact revision reached a Packet.
///
/// A typed fact rather than a sentence, because a client has to *act* on the
/// difference: material pinned by a Baseline, material a Spec pulled in by
/// incorporation, and material aimed at one Issue directly are three different
/// claims about why this governs, and the reader must be able to say which
/// without parsing prose that a later reword would silently break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "route", rename_all = "snake_case", deny_unknown_fields)]
pub enum PacketSource {
    /// Pinned by the Baseline the Issue binds.
    Baseline { baseline: String },
    /// An issued Spec that governs this Issue by its own `governs` Link.
    Direct,
    /// Pulled in by an exact `incorporates` Link on another revision in the set.
    Incorporated { spec: String, revision: String },
}

/// Why a Packet is not whole.
///
/// Each variant is a different remedy — a missing Body will arrive with a sync,
/// an unissued Baseline needs issuing, concurrent issued revisions need a
/// resolution — so a client that has to tell them apart cannot be handed one
/// string. `missing` and `not issued` in particular are the difference between
/// "wait" and "act".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum PacketConflict {
    MissingBaseline {
        baseline: String,
    },
    MissingBaselineRevision {
        baseline: String,
        revision: String,
    },
    BaselineNotIssued {
        baseline: String,
        revision: String,
    },
    MissingSpec {
        spec: String,
    },
    MissingSpecRevision {
        spec: String,
        revision: String,
    },
    /// Concurrent issued revisions of a Spec that governs this Issue. Nothing
    /// is effective until they are resolved.
    IssuedSpecConflict {
        spec: String,
    },
    MissingIncorporated {
        spec: String,
        revision: String,
    },
}

/// One typed assertion, as seen from the far end of it.
///
/// Links live on the revision that asserts them, so "what verifies this
/// requirement" is only answerable by looking at every other document — and
/// specifically at every *revision* of them, not just the heads. The revision
/// that governs need not be the head, so an edge asserted by an issued
/// predecessor is live truth that a head-only scan silently loses.
///
/// `head` and `issued` describe the asserting revision, which is what lets a
/// reader tell a current claim from one a superseded revision made and nobody
/// stands behind any more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecReference {
    /// The Spec asserting it.
    pub spec: String,
    /// The exact revision asserting it.
    pub revision: String,
    pub kind: Kind,
    pub title: String,
    pub link: Link,
    /// The asserting revision is a current head.
    pub head: bool,
    /// The asserting revision is the effective issued one.
    pub issued: bool,
}

/// One independently pageable link assertion. Head/issued standing is not
/// copied onto every edge; callers compare `revision` against the bounded head
/// coordinates in [`SpecSummary`], avoiding a second mutable truth per link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecReferenceFact {
    pub spec: String,
    pub revision: String,
    pub kind: Kind,
    pub title: String,
    pub link: Link,
}

/// One retractable note about the graph, bound to nobody's document.
///
/// A `Link` is something a document *says*: it lives inside a revision, it is
/// covered by that revision's hash, and issuing the document issues it. That is
/// the right shape for a claim its author is answerable for, and the wrong shape
/// for the other thing people need to record — that REQ-3 conflicts with REQ-7,
/// that this design depends on that one, that a Proof turned out to cover a
/// second requirement nobody had connected. Those are true *about* documents
/// rather than said *by* one, and laundering them through a document forces an
/// author to amend material they may not own and, on issued material, to raise a
/// draft successor that announces a change to governing truth that is not
/// happening.
///
/// So an Observation is deliberately the inverse of a Link on every axis that
/// matters:
///
/// - it carries its own observer, not the document's author;
/// - it is not in any revision, so it never enters the content hash and issuing
///   a document neither adopts nor freezes it;
/// - it lives in a CRDT set, so two observers adding different notes merge
///   instead of colliding on a compare-and-swap;
/// - it is retractable on its own, without writing a revision;
/// - **it never reaches a Packet, and never counts as verification coverage.**
///
/// That last one is the whole firewall. An observation that could quietly become
/// enforcing would be a `governs` Link that skipped issuance, which is precisely
/// the hole `Rel::References` is worded to keep shut.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub observation: String,
    /// The Spec this note is filed against — the set it lives in.
    pub spec: String,
    /// Who noticed. Not the subject document's author, and not its issuer.
    pub observer: String,
    pub ts: u64,
    pub rel: Rel,
    pub target: Target,
    /// Why they think so, in their words. An observation with no argument
    /// behind it is a claim nobody can weigh.
    #[serde(default)]
    pub note: String,
}

impl Observation {
    pub fn validate(&self) -> Result<(), String> {
        if crate::ids::ObservationId::parse(&self.observation).is_none() {
            return Err("invalid Observation id".into());
        }
        if crate::ids::SpecId::parse(&self.spec).is_none() {
            return Err("invalid Spec id".into());
        }
        if mechanics::ids::ActorId::parse(&self.observer).is_none() {
            return Err("invalid Observation observer".into());
        }
        if self.note.len() > MAX_TEXT_BYTES {
            return Err("Observation note is too long".into());
        }
        validate_target(&self.target)
    }
}

/// One exact Spec revision as it appears in an Issue Packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PacketSpec {
    pub spec: String,
    pub revision: String,
    pub kind: Kind,
    pub title: String,
    pub state: State,
    pub source: PacketSource,
    #[serde(default)]
    pub links: Vec<Link>,
}

/// Derived effective brief for one Issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    pub issue: String,
    #[serde(default)]
    pub baseline: Option<BaselineRef>,
    #[serde(default)]
    pub governing: Vec<PacketSpec>,
    #[serde(default)]
    pub guidance: Vec<PacketSpec>,
    #[serde(default)]
    pub proof: Vec<PacketSpec>,
    #[serde(default)]
    pub record: Vec<PacketSpec>,
    #[serde(default)]
    pub conflicts: Vec<PacketConflict>,
}

pub fn build_revision(mut body: Body, mut predecessors: Vec<[u8; 32]>) -> Result<Revision, String> {
    body.canonicalize();
    body.validate()?;
    canonical_predecessors(&mut predecessors)?;
    let body_json = canonical_json(&body)?;
    let revision = data_encoding::HEXLOWER.encode(&blake3::derive_key(
        SPEC_REVISION_CONTEXT,
        &preimage(&body.spec, &predecessors, &body_json)?,
    ));
    Ok(Revision {
        revision,
        predecessors: predecessors
            .iter()
            .map(|id| data_encoding::HEXLOWER.encode(id))
            .collect(),
        body,
    })
}

pub fn build_baseline_revision(
    mut body: BaselineBody,
    mut predecessors: Vec<[u8; 32]>,
) -> Result<BaselineRevision, String> {
    body.canonicalize();
    body.validate()?;
    canonical_predecessors(&mut predecessors)?;
    let body_json = canonical_json(&body)?;
    let revision = data_encoding::HEXLOWER.encode(&blake3::derive_key(
        BASELINE_REVISION_CONTEXT,
        &preimage(&body.baseline, &predecessors, &body_json)?,
    ));
    Ok(BaselineRevision {
        revision,
        predecessors: predecessors
            .iter()
            .map(|id| data_encoding::HEXLOWER.encode(id))
            .collect(),
        body,
    })
}

pub fn decode_revision(raw: &str) -> Option<[u8; 32]> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

fn validate_target(target: &Target) -> Result<(), String> {
    match target {
        Target::Spec { spec, revision } => {
            if crate::ids::SpecId::parse(spec).is_none() || decode_revision(revision).is_none() {
                return Err("invalid Spec link target".into());
            }
        }
        Target::Baseline { baseline, revision } => {
            if crate::ids::BaselineId::parse(baseline).is_none()
                || decode_revision(revision).is_none()
            {
                return Err("invalid Baseline link target".into());
            }
        }
        Target::Issue { issue } => {
            if crate::ids::DocId::parse(issue).is_none() {
                return Err("invalid Issue link target".into());
            }
        }
    }
    Ok(())
}

fn canonical_predecessors(predecessors: &mut Vec<[u8; 32]>) -> Result<(), String> {
    if predecessors.len() > MAX_PREDECESSORS {
        return Err("too many revision predecessors".into());
    }
    predecessors.sort();
    predecessors.dedup();
    Ok(())
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    let value = sort_json(value);
    serde_json::to_vec(&value).map_err(|error| error.to_string())
}

fn sort_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(sort_json).collect())
        }
        other => other,
    }
}

fn preimage(id: &str, predecessors: &[[u8; 32]], body: &[u8]) -> Result<Vec<u8>, String> {
    let id_len = u16::try_from(id.len()).map_err(|_| "document id is too long".to_string())?;
    let predecessor_count = u16::try_from(predecessors.len())
        .map_err(|_| "too many predecessor revisions".to_string())?;
    let body_len =
        u32::try_from(body.len()).map_err(|_| "revision body is too long".to_string())?;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&id_len.to_be_bytes());
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(&predecessor_count.to_be_bytes());
    for predecessor in predecessors {
        out.extend_from_slice(predecessor);
    }
    out.extend_from_slice(&body_len.to_be_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

/// Every revision, predecessors before successors.
///
/// The stored order is by revision id, which is stable but arbitrary — ids are
/// content hashes, so "sorted" says nothing about what came first. A rail that
/// listed them that way would show a document's history shuffled. Ties inside a
/// generation still fall back to the id, so concurrent branches interleave
/// deterministically rather than by whatever the map iterated.
pub fn ordered<'a, T, F>(items: &'a [T], parts: F) -> Vec<&'a T>
where
    F: Fn(&T) -> (&String, &Vec<String>),
{
    let known: BTreeSet<&str> = items.iter().map(|item| parts(item).0.as_str()).collect();
    let mut pending: Vec<&T> = items.iter().collect();
    pending.sort_by(|a, b| parts(a).0.cmp(parts(b).0));
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<&T> = Vec::with_capacity(items.len());
    while !pending.is_empty() {
        let mut deferred: Vec<&T> = Vec::new();
        let before = out.len();
        for item in pending {
            let (id, predecessors) = parts(item);
            // A predecessor this store does not hold cannot be waited for. It
            // is a partial replica, not a broken order.
            let ready = predecessors.iter().all(|predecessor| {
                !known.contains(predecessor.as_str()) || placed.contains(predecessor.as_str())
            });
            if ready {
                placed.insert(id.as_str());
                out.push(item);
            } else {
                deferred.push(item);
            }
        }
        if out.len() == before {
            // Predecessors are content hashes, so a cycle cannot be authored —
            // but a corrupt store must not spin here. Emit the rest in id order.
            out.extend(deferred);
            break;
        }
        pending = deferred;
    }
    out
}

fn heads<'a, T, F>(items: &'a [T], parts: F) -> Vec<&'a T>
where
    F: Fn(&T) -> (&String, &Vec<String>),
{
    let predecessors: BTreeSet<&str> = items
        .iter()
        .flat_map(|item| parts(item).1.iter().map(String::as_str))
        .collect();
    let mut result: Vec<&T> = items
        .iter()
        .filter(|item| !predecessors.contains(parts(item).0.as_str()))
        .collect();
    result.sort_by(|a, b| parts(a).0.cmp(parts(b).0));
    result
}

fn descends<T, F>(revisions: &[T], descendant: &str, ancestor: &str, parts: F) -> bool
where
    F: Fn(&T) -> (&String, &Vec<String>),
{
    let mut stack = vec![descendant];
    let mut seen = BTreeSet::new();
    while let Some(current) = stack.pop() {
        if !seen.insert(current) {
            continue;
        }
        let Some(revision) = revisions
            .iter()
            .find(|revision| parts(revision).0 == current)
        else {
            continue;
        };
        for predecessor in parts(revision).1 {
            if predecessor == ancestor {
                return true;
            }
            stack.push(predecessor);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> String {
        mechanics::ids::ActorId::from_incept_hash(&"07".repeat(32)).to_string()
    }

    fn body(spec: &str, state: State, ts: u64) -> Body {
        Body {
            spec: spec.into(),
            project: "prj_01k1k8q6c6t0g0000000000000".into(),
            kind: Kind::Requirement,
            publication: publication(),
            title: "A requirement".into(),
            text: "The system shall be deterministic.".into(),
            state,
            links: vec![],
            plan: None,
            author: actor(),
            ts,
        }
    }

    fn baseline_body(state: State, ts: u64) -> BaselineBody {
        BaselineBody {
            baseline: "bas_01k1k8q6c6t0g0000000000000".into(),
            project: "prj_01k1k8q6c6t0g0000000000000".into(),
            name: "Issued set".into(),
            state,
            members: vec![],
            author: actor(),
            ts,
        }
    }

    fn plan() -> PlanData {
        PlanData {
            roots: vec!["iss_01k1k8q6c6t0g0000000000000".into()],
        }
    }

    fn publication() -> runtime::publication::PublicationId {
        runtime::publication::PublicationId::new(
            [1; 32],
            [2; 32],
            runtime::publication::ExtractorSchemaDigest::from_digest([3; 32]),
        )
    }

    #[test]
    fn structured_plan_data_belongs_only_to_plan_specs() {
        let mut revision = body("spc_01k1k8q6c6t0g0000000000000", State::Draft, 1);
        revision.plan = Some(plan());

        assert_eq!(
            revision.validate(),
            Err("structured Plan data belongs only on a Plan Spec".into())
        );
        revision.kind = Kind::Plan;
        revision.publication = publication();
        assert_eq!(revision.validate(), Ok(()));
    }

    #[test]
    fn plan_roots_are_canonical_and_unique() {
        let mut value = plan();
        value.roots.push("iss_01k1k8q6c6t0g0000000000000".into());

        assert_eq!(
            value.validate(),
            Err("Plan roots are invalid or duplicated".into())
        );
    }

    #[test]
    fn legacy_plan_phases_collapse_to_sorted_unique_roots() {
        let plan: PlanData = serde_json::from_value(serde_json::json!({
            "placement": 3,
            "phases": [
                {"id": "b", "title": "Build", "issues": [
                    "iss_01k1k8q6c6t0g0000000000001",
                    "iss_01k1k8q6c6t0g0000000000000"
                ]},
                {"id": "s", "title": "Ship", "milestone": null, "issues": [
                    "iss_01k1k8q6c6t0g0000000000001"
                ]}
            ]
        }))
        .expect("legacy Plan");
        assert_eq!(
            plan.roots,
            [
                "iss_01k1k8q6c6t0g0000000000000",
                "iss_01k1k8q6c6t0g0000000000001",
            ]
        );
        assert_eq!(
            serde_json::to_value(plan).expect("new Plan"),
            serde_json::json!({
                "roots": [
                    "iss_01k1k8q6c6t0g0000000000000",
                    "iss_01k1k8q6c6t0g0000000000001"
                ]
            })
        );
    }

    #[test]
    fn structured_plan_changes_are_part_of_revision_identity() {
        let mut left = body("spc_01k1k8q6c6t0g0000000000000", State::Draft, 1);
        left.kind = Kind::Plan;
        left.publication = publication();
        left.plan = Some(plan());
        let mut right = left.clone();
        right.plan.as_mut().expect("Plan data").roots[0] = "iss_01k1k8q6c6t0g0000000000001".into();

        let left = build_revision(left, vec![]).expect("left Plan revision");
        let right = build_revision(right, vec![]).expect("right Plan revision");
        assert_ne!(left.revision, right.revision);
    }

    #[test]
    fn a_draft_successor_does_not_unissue_its_parent() {
        let first = build_revision(
            body("spc_01k1k8q6c6t0g0000000000000", State::Issued, 1),
            vec![],
        )
        .expect("issued revision");
        let second = build_revision(
            body("spc_01k1k8q6c6t0g0000000000000", State::Draft, 2),
            vec![decode_revision(&first.revision).expect("revision id")],
        )
        .expect("draft revision");
        let spec = Spec {
            revisions: vec![first.clone(), second],
            ..Spec::default()
        };
        assert!(matches!(spec.issued(), Issued::One(rev) if rev.revision == first.revision));
    }

    #[test]
    fn a_later_issue_supersedes_the_prior_issue() {
        let first = build_revision(
            body("spc_01k1k8q6c6t0g0000000000000", State::Issued, 1),
            vec![],
        )
        .expect("issued revision");
        let second = build_revision(
            body("spc_01k1k8q6c6t0g0000000000000", State::Issued, 2),
            vec![decode_revision(&first.revision).expect("revision id")],
        )
        .expect("issued revision");
        let spec = Spec {
            revisions: vec![first, second.clone()],
            ..Spec::default()
        };
        assert!(matches!(spec.issued(), Issued::One(rev) if rev.revision == second.revision));
    }

    #[test]
    fn canonical_links_make_equal_revisions() {
        let mut left = body("spc_01k1k8q6c6t0g0000000000000", State::Draft, 1);
        let target = Target::Issue {
            issue: "iss_01k1k8q6c6t0g0000000000000".into(),
        };
        left.links = vec![
            Link {
                rel: Rel::References,
                target: target.clone(),
            },
            Link {
                rel: Rel::Governs,
                target,
            },
        ];
        let mut right = left.clone();
        right.links.reverse();
        let left = build_revision(left, vec![]).expect("left");
        let right = build_revision(right, vec![]).expect("right");
        assert_eq!(left.revision, right.revision);
    }

    #[test]
    fn concurrent_issued_successors_are_a_visible_conflict() {
        let root = build_revision(
            body("spc_01k1k8q6c6t0g0000000000000", State::Draft, 1),
            vec![],
        )
        .expect("root");
        let predecessor = decode_revision(&root.revision).expect("revision id");
        let left = build_revision(
            body("spc_01k1k8q6c6t0g0000000000000", State::Issued, 2),
            vec![predecessor],
        )
        .expect("left");
        let right = build_revision(
            body("spc_01k1k8q6c6t0g0000000000000", State::Issued, 3),
            vec![predecessor],
        )
        .expect("right");
        let spec = Spec {
            revisions: vec![root, left, right],
            ..Spec::default()
        };
        assert!(matches!(spec.issued(), Issued::Conflict(heads) if heads.len() == 2));
    }

    #[test]
    fn baseline_draft_preserves_issue_and_withdrawal_ends_it() {
        let issued = build_baseline_revision(baseline_body(State::Issued, 1), vec![])
            .expect("issued Baseline");
        let predecessor = decode_revision(&issued.revision).expect("revision id");
        let draft = build_baseline_revision(baseline_body(State::Draft, 2), vec![predecessor])
            .expect("draft Baseline");
        let current = Baseline {
            revisions: vec![issued.clone(), draft.clone()],
            ..Baseline::default()
        };
        assert!(
            matches!(current.issued(), BaselineIssued::One(revision) if revision.revision == issued.revision)
        );

        let withdrawn = build_baseline_revision(
            baseline_body(State::Withdrawn, 3),
            vec![decode_revision(&draft.revision).expect("revision id")],
        )
        .expect("withdrawn Baseline");
        let current = Baseline {
            revisions: vec![issued, draft, withdrawn],
            ..Baseline::default()
        };
        assert!(matches!(current.issued(), BaselineIssued::None));
    }
}
