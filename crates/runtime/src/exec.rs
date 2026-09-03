//! Durable work declared by Worlds and performed by Stations.
//!
//! This module owns the substrate vocabulary for execution. The module is the
//! qualifier: public types are `Spec`, `Run`, `Attempt`, `Outcome`, `Build`,
//! `Service`, and `Offer`, never names that repeat `Exec` or `Execution`.
//!
//! A World remains a pure semantic authority. It declares what may run and,
//! through its ordinary [`World::submit`](crate::world::World::submit)
//! callback, asks Runtime to commit work beside the product mutation that
//! requested it. Runtime commits that durable truth before a Station may
//! perform an Attempt. There is deliberately no `Session::exec` or
//! `Session::start` shortcut around [`Session::submit`](crate::Session::submit).
//!
//! ## Package access
//!
//! The ordinary package boundary has one ambient Session coordinate and two
//! operation classes:
//!
//! | Package need | Ordinary entrypoint | World callback | Durable consequence |
//! | --- | --- | --- | --- |
//! | bounded retrieval | `Session::find(Query)` | none | none; returns a coordinate-stamped answer |
//! | start or change durable work | [`Session::submit`](crate::Session::submit) | [`World::submit`](crate::world::World::submit) returns command values | Run events commit atomically with ordinary World effects |
//! | retrieval inside an Attempt | bounded Attempt query facade | none | none unless retained as Outcome evidence |
//! | child work inside an Attempt | bounded child-Run sink | none | commits the child Run before dispatch |
//!
//! `Session::find` and the Attempt facades are contracts for their later
//! slices, not claims about the current tree. Every operation begins from the
//! same live Station, Space, World, implementation, principal, authority, and
//! policy envelope. Read-only retrieval may then pin a root and release the
//! writer. Durable work retains the writer through semantic validation and
//! atomic containment. Admission failure is synchronous and leaves neither a
//! partial answer nor a partial commit.
//!
//! Product surfaces retain product language. A product that already owns a
//! flat "spec" noun can call the durable product record a "check" rather than
//! exporting two unrelated meanings under one unqualified name.
//!
//! The contract types land in the following E0 slices. Keeping this module
//! present first gives those slices one reviewed namespace and lets the
//! workspace naming gate reject stuttering types as soon as they are declared.

use mechanics::{
    authorization::AuthorizationDemand,
    ids::{ActorId, DeviceId},
    station::{Epoch as StationEpoch, Key as StationKey},
};
use replica::{
    body::{
        BodyId, CollaborativeSchema, EncodingId, MutationModel, Schema as BodySchema, SchemaId,
        WorldId, MAX_BODY_BYTES, MUTATION_ATOMIC, MUTATION_COLLABORATIVE,
    },
    content::{ContentRef, MAX_CONTENT_LEN},
    frontier::AuthorityFrontier,
    manifest::MAX_CONTENT_REFS_PER_BODY,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Standalone canonical encoding generation for [`Spec`].
const SPEC_VERSION: u8 = 2;
/// Standalone canonical encoding generation for [`Build`].
const BUILD_VERSION: u8 = 1;
/// Standalone canonical encoding generation for [`Offer`].
const OFFER_VERSION: u8 = 1;
/// Signature algorithm carried by [`Signature`] in generation 1.
const SIGNATURE_ED25519: u8 = 1;
/// Domain for [`BuildId`] derivation over canonical identity-bearing material.
const BUILD_ID_DOMAIN: &[u8] = b"lait/exec/build/1\0";
/// Domain for the signed Build publication envelope.
const BUILD_SIGNATURE_DOMAIN: &[u8] = b"lait/exec/build/signature/1\0";
/// Domain for [`OfferId`] derivation over canonical identity-bearing material.
const OFFER_ID_DOMAIN: &[u8] = b"lait/exec/offer/1\0";
/// Domain for the signed Offer news envelope.
const OFFER_SIGNATURE_DOMAIN: &[u8] = b"lait/exec/offer/signature/1\0";
/// Standalone canonical encoding generation for [`Challenge`].
const CHALLENGE_VERSION: u8 = 1;
/// Domain for the signed Ready answer over one Challenge.
const READY_SIGNATURE_DOMAIN: &[u8] = b"lait/exec/ready/signature/1\0";
/// Maximum bytes in one canonical standalone [`Spec`].
pub const MAX_SPEC_BYTES: usize = 2 * 1024 * 1024;
/// Maximum bytes in one canonical standalone [`Build`].
pub const MAX_BUILD_BYTES: usize = 256 * 1024;
/// Maximum bytes in one canonical standalone [`Offer`].
pub const MAX_OFFER_BYTES: usize = 64 * 1024;
/// Maximum exact Builds one Offer may claim.
pub const MAX_BUILDS_PER_OFFER: usize = 64;
/// Maximum resident ContentRef hints one Offer may carry.
pub const MAX_RESIDENT_HINTS_PER_OFFER: usize = 64;
/// Maximum live Offers one Station activation may retain as news.
pub const MAX_OFFERS_PER_STATION: usize = 256;
/// Maximum outstanding readiness challenges one Station activation may retain.
pub const MAX_CHALLENGES_PER_STATION: usize = 256;
/// Default lifetime of one readiness challenge, in unix milliseconds.
pub const CHALLENGE_TTL_MILLIS: u64 = 30_000;
/// Canonical CPU quantity name.
pub const CPU_MILLIS: &str = "cpu.millis";
/// Canonical memory quantity name.
pub const MEMORY_BYTES: &str = "memory.bytes";
/// Canonical storage quantity name.
pub const STORAGE_BYTES: &str = "storage.bytes";
/// Canonical wall-time quantity name.
pub const WALL_TIME_MS: &str = "wall_time.ms";
/// Canonical ingress bandwidth quantity name.
pub const NETWORK_INGRESS_BPS: &str = "network.ingress_bps";
/// Canonical egress bandwidth quantity name.
pub const NETWORK_EGRESS_BPS: &str = "network.egress_bps";
/// Maximum Find envelopes one Spec may expose to an Attempt.
pub const MAX_QUERIES_PER_SPEC: usize = 16;
/// Maximum declared Links in one Spec or Set Service.
pub const MAX_LINKS_PER_SPEC: usize = 256;
/// Maximum Roles in one Set Service.
pub const MAX_ROLES_PER_SERVICE: usize = 256;
/// Maximum deterministic commands retained for replay.
pub const MAX_REPLAY_COMMANDS: u32 = 1_000_000;
/// Maximum immutable configuration objects one Build may reference.
pub const MAX_CONFIG_REFS_PER_BUILD: usize = 256;
/// Maximum predecessor Builds one Build may claim checkpoint compatibility with.
pub const MAX_COMPATIBLE_BUILDS: usize = 256;
/// Maximum canonical Resources one Start or Try may carry.
pub const MAX_RESOURCES_PER_INTENT: usize = 64;
/// Maximum canonical bytes retained in one Run event value.
pub const MAX_RUN_EVENT_BYTES: usize = 64 * 1024;
/// Maximum immediate causal parents one Run event may join.
pub const MAX_RUN_EVENT_PREDECESSORS: usize = 256;
/// Maximum command-material bytes retained in one Run Body map entry.
pub const MAX_RUN_COMMAND_CHUNK_BYTES: usize = 64 * 1024;
/// Collaborative list path containing predecessor-bound Run events.
pub const RUN_EVENTS_PATH: &str = "events";
/// Collaborative map path containing canonical Start command chunks.
pub const RUN_COMMAND_PATH: &str = "command";
/// Collaborative map path containing one published Build envelope.
pub const BUILD_ENVELOPE_PATH: &str = "envelope";
/// Map key of the canonical Build bytes under [`BUILD_ENVELOPE_PATH`].
pub const BUILD_ENVELOPE_KEY: &str = "bytes";
/// Domain for deriving a reserved Build Body id from a [`BuildId`].
const BUILD_BODY_ID_DOMAIN: &[u8] = b"lait/exec/build-body/1\0";

const RUN_EVENT_VERSION: u8 = 3;
const RUN_EVENT_ID_CONTEXT: &str = "lait.exec.run-event.v1";
const RUN_ID_DOMAIN: &[u8] = b"lait/exec/run-id/1\0";
const ACTIVE_RUN_BODY_DOMAIN: &[u8] = b"lait/exec/active-body/1\0";
const INPUT_DIGEST_CONTEXT: &str = "lait.exec.input.v1";
const QUERY_GRANTS_DIGEST_DOMAIN: &[u8] = b"lait/exec/query-grants/1\0";
const COMMAND_DIGEST_CONTEXT: &str = "lait.exec.command.v1";
const OUTPUT_DIGEST_CONTEXT: &str = "lait.exec.output.v1";

/// Runtime-owned Body Schema ids reserved under every hosted World.
///
/// They are not World declarations and do not enter that World's implementation
/// descriptor. Runtime adds the exact schemas to Replica's supported set at
/// activation. Package composition refuses a World that declares any id in
/// this list, at any version, so a World can request a semantic [`Cmd`] but can
/// never write the durable truth produced by lowering one.
/// Runtime-owned Run Body Schema id.
pub const RUN_BODY_SCHEMA: &str = "lait.exec.run";
/// Runtime-owned Build Body Schema id.
pub const BUILD_BODY_SCHEMA: &str = "lait.exec.build";
/// Runtime-owned Service Body Schema id.
pub const SERVICE_BODY_SCHEMA: &str = "lait.exec.service";
/// Runtime-owned sparse directory of Runs that still require control work.
/// A deterministic marker Body exists only while its Run is unresolved;
/// terminal history stays in the Run Body without occupying this posting.
pub const ACTIVE_RUN_BODY_SCHEMA: &str = "lait.exec.active";
pub const RESERVED_SCHEMAS: [&str; 4] = [
    RUN_BODY_SCHEMA,
    BUILD_BODY_SCHEMA,
    SERVICE_BODY_SCHEMA,
    ACTIVE_RUN_BODY_SCHEMA,
];
/// Version of the Runtime-owned Run Body schema.
pub const RUN_BODY_SCHEMA_VERSION: u32 = 2;
/// Version of the Runtime-owned Build Body schema.
pub const BUILD_BODY_SCHEMA_VERSION: u32 = 1;
/// Version of the Runtime-owned Service Body schema.
pub const SERVICE_BODY_SCHEMA_VERSION: u32 = 1;
/// Version of the sparse active-Run marker schema.
pub const ACTIVE_RUN_BODY_SCHEMA_VERSION: u32 = 1;
/// Encoding contract shared by the generation-1 Runtime event Bodies.
pub const BODY_ENCODING: &str = "lait.exec.body.v1";

/// Whether a Body Schema id belongs exclusively to Runtime's Exec lowering.
pub fn is_reserved_schema(schema: &SchemaId) -> bool {
    RESERVED_SCHEMAS.contains(&schema.as_str())
}

/// Exact Runtime-owned Body schemas installed under every hosted World.
///
/// The literals are tests and compatibility fixtures. Localized
/// `expect` is intentional: an invalid checked-in literal is a broken build,
/// not a runtime input that can be recovered by silently dropping a protected
/// schema.
#[allow(
    clippy::expect_used,
    reason = "checked-in reserved schema and encoding literals are compatibility fixtures"
)]
pub fn body_schemas() -> [BodySchema; 4] {
    let encoding = EncodingId::parse(BODY_ENCODING).expect("reserved Exec Body encoding");
    let schema = |name: &str, version| BodySchema {
        id: SchemaId::parse(name).expect("reserved Exec Body schema"),
        version,
        encoding: encoding.clone(),
        mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
        readable_predecessors: Vec::new(),
    };
    let active = BodySchema {
        id: SchemaId::parse(ACTIVE_RUN_BODY_SCHEMA).expect("reserved active Run Body schema"),
        version: ACTIVE_RUN_BODY_SCHEMA_VERSION,
        encoding: encoding.clone(),
        mutation: MutationModel::Atomic,
        readable_predecessors: Vec::new(),
    };
    [
        schema(RUN_BODY_SCHEMA, RUN_BODY_SCHEMA_VERSION),
        schema(BUILD_BODY_SCHEMA, BUILD_BODY_SCHEMA_VERSION),
        schema(SERVICE_BODY_SCHEMA, SERVICE_BODY_SCHEMA_VERSION),
        active,
    ]
}

/// One version of a World-owned payload, checkpoint, effect, or Link codec.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaRef {
    pub name: SchemaId,
    pub version: u32,
}

/// Which Specs this Station performs for a device other than its own.
///
/// Consent is a device's fact, never a Space's: membership and invites confer
/// no right to run on a device. A Station always performs Runs its own device
/// started; for anyone else's it must have said so here, per Spec. The
/// default says nothing, so a Station that never configured consent performs
/// only its own work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Consent {
    specs: Vec<SchemaRef>,
}

impl Consent {
    /// Perform nothing another device started.
    pub fn none() -> Self {
        Self::default()
    }

    /// Perform exactly these Specs for other devices.
    pub fn for_specs(specs: impl IntoIterator<Item = SchemaRef>) -> Self {
        let mut specs: Vec<SchemaRef> = specs.into_iter().collect();
        specs.sort();
        specs.dedup();
        Self { specs }
    }

    /// Whether a Run of `spec` started elsewhere may be performed here.
    pub fn allows(&self, spec: &SchemaRef) -> bool {
        self.specs.binary_search(spec).is_ok()
    }
}

/// The independent authority demanded by every way a Spec can be acted on.
///
/// These are canonical, non-empty [`AuthorizationDemand`] bytes. They are
/// declarations only: Runtime evaluates the appropriate field at the pinned
/// authority frontier when the corresponding action is admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    pub start: Vec<u8>,
    pub offer: Vec<u8>,
    pub control: Vec<u8>,
    pub accept: Vec<u8>,
    /// Standing to discover and attach to a live terminal session. Byte
    /// observation additionally requires the output payload's read demand;
    /// driving additionally requires `control`. Absence is deliberately
    /// fail-closed: the Spec is not attachable.
    pub attach: Option<Vec<u8>>,
}

/// One bounded payload contract.
///
/// Inline and additional input use Body-sized messages. Content is immutable
/// and named separately so a handler receives only pinned, authorized readers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadSpec {
    pub schema: SchemaRef,
    pub max_inline_bytes: u32,
    pub max_content_refs: u32,
    pub max_content_bytes: u64,
    /// Canonical non-empty demand for reading payload material.
    pub read: Vec<u8>,
    /// Total additional committed input accepted after Start. Zero disables it.
    pub max_additional_input_bytes: u64,
}

/// Shape of interaction between one Run and its handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Unary,
    Stream,
    Interactive,
}

/// Whether a terminal Attempt leaves a durable, host-authored recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Transcript {
    /// Live bytes remain activation-local and disappear with the session.
    #[default]
    Never,
    /// Runtime seals a bounded transcript beside a successful Returned event.
    OnReturn,
}

/// Terminal-specific ceilings and durability posture.
///
/// This block is present for every Spec so the Mode matrix remains explicit.
/// Non-interactive Specs use zero live input; transcript recording is also
/// opt-in rather than inferred from a frontend attaching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TerminalSpec {
    pub transcript: Transcript,
    pub max_transcript_bytes: u64,
    pub max_live_input_bytes: u64,
}

impl TerminalSpec {
    pub const fn disabled() -> Self {
        Self {
            transcript: Transcript::Never,
            max_transcript_bytes: 0,
            max_live_input_bytes: 0,
        }
    }

    fn validate(self) -> Result<(), Invalid> {
        if self.max_transcript_bytes == u64::MAX
            || self.max_live_input_bytes == u64::MAX
            || self.max_transcript_bytes > MAX_CONTENT_LEN
            || matches!(self.transcript, Transcript::Never) && self.max_transcript_bytes != 0
            || matches!(self.transcript, Transcript::OnReturn) && self.max_transcript_bytes == 0
        {
            return Err(Invalid::InvalidSpec("terminal"));
        }
        Ok(())
    }
}

/// How a later Attempt reconstructs one Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resume {
    Restart,
    Checkpoint { codec: SchemaRef },
    Replay { commands: u32 },
    Never,
}

/// Repeatability contract for effects outside durable child Runs and inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effects {
    /// Safe to repeat; an output digest can be checked.
    Pure,
    /// Safe to repeat only under the declared World-owned external key codec.
    Idempotent { key: SchemaRef },
    /// Duplication remains possible and must stay visible.
    ExternalAtLeastOnce,
}

/// Who may turn a returned Attempt into accepted durable truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcceptRule {
    /// Only a World-returned [`Cmd::Accept`] may accept. Operator routes refuse.
    World,
    /// Either route may accept after satisfying [`Access::accept`].
    Authorized,
}

/// One logical part of a reusable Service.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoleSpec {
    pub name: SchemaId,
    /// Callable contract served by this Role. A Service activation later pins
    /// the exact compatible Build.
    pub spec: SchemaRef,
}

/// Delivery semantics for one declared Link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Send {
    Store,
    Direct,
    Signal,
    Stream,
    Fetch,
}

/// How a child message derives rank from its parent operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RankRule {
    Inherit,
    Cap(u32),
    Reset,
    Recompute,
}

/// One bounded, permitted Role-to-Role exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkSpec {
    pub name: SchemaId,
    pub from: SchemaId,
    pub to: SchemaId,
    pub codec: SchemaRef,
    pub send: Send,
    pub rank: RankRule,
    pub max_messages: u32,
    pub max_bytes: u64,
}

/// Barrier applied before a Set's entry Role may be used.
///
/// The first contract deliberately has one honest rule: every declared Role
/// and Link must be ready. Quorum or partial-readiness policy belongs to a
/// later explicit generation rather than an ambiguous number here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadyRule {
    All,
}

/// Reusable live-host shape declared by a Spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceSpec {
    Warm {
        role: RoleSpec,
        max_runs: u32,
    },
    Pool {
        role: RoleSpec,
        min: u16,
        max: u16,
    },
    Set {
        roles: Vec<RoleSpec>,
        links: Vec<LinkSpec>,
        ready: ReadyRule,
    },
}

/// Finite ceilings applied across one Run and all of its Attempts.
///
/// Zero disables checkpoints, child Runs, progress, or checkpoint bytes. The
/// Run itself must still permit at least one Attempt and one event, and must
/// carry a finite wall-clock admission ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub attempts: u32,
    pub events: u32,
    pub checkpoints: u32,
    pub child_runs: u32,
    pub progress_bytes: u64,
    pub checkpoint_bytes: u64,
    pub wall_millis: u64,
}

/// One World-declared callable contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub name: SchemaId,
    pub version: u32,
    pub access: Access,
    pub input: PayloadSpec,
    pub output: PayloadSpec,
    pub mode: Mode,
    pub terminal: TerminalSpec,
    pub resume: Resume,
    pub effects: Effects,
    pub accept: AcceptRule,
    pub queries: Vec<crate::find::Grant>,
    pub service: Option<ServiceSpec>,
    pub links: Vec<LinkSpec>,
    pub limits: Limits,
}

/// The content identity of one immutable executable Build.
///
/// Identity commits to the Build material from `world` through
/// `compatible_from`. Publisher and signature are deliberately outside the
/// identity: they attest those exact bytes and are evaluated independently by
/// Mechanics at publication time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BuildId([u8; 32]);

impl BuildId {
    /// Wrap one canonical 256-bit Build identity.
    pub const fn from_bytes(raw: [u8; 32]) -> Self {
        Self(raw)
    }

    /// Return this identity's canonical bytes.
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Detached signature over one Build publication envelope.
///
/// The signer is a Device, not an Actor. Local validation proves the device
/// signed these bytes; Mechanics separately proves that device represented
/// [`Build::publisher`] at the pinned authority frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub signer: DeviceId,
    pub algorithm: u8,
    #[serde(with = "serde_byte_array")]
    pub bytes: [u8; 64],
}

/// One immutable implementation of a World-declared [`Spec`].
///
/// `handler`, optional dependencies, configuration, and environment name
/// immutable content. Secret values are never Build content; configuration
/// may only name an immutable secret-contract reference that a Station later
/// resolves under authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub id: BuildId,
    pub world: WorldId,
    /// Canonical World implementation identity active when this Build was
    /// published. This is trust context, not native-code attestation.
    pub world_build: [u8; 32],
    pub spec: SchemaRef,
    pub handler: ContentRef,
    pub dependencies: Option<ContentRef>,
    pub environment: [u8; 32],
    pub config: Vec<ContentRef>,
    pub checkpoint: Option<SchemaRef>,
    pub replay_commands: Option<u32>,
    pub compatible_from: Vec<BuildId>,
    pub publisher: ActorId,
    pub signature: Signature,
}

/// Borrowed identity-bearing portion of [`Build`]. The field order is the
/// generation-1 BuildId grammar and must not be changed in place.
#[derive(Serialize)]
struct BuildMaterial<'a> {
    world: &'a WorldId,
    world_build: &'a [u8; 32],
    spec: &'a SchemaRef,
    handler: &'a ContentRef,
    dependencies: &'a Option<ContentRef>,
    environment: &'a [u8; 32],
    config: &'a [ContentRef],
    checkpoint: &'a Option<SchemaRef>,
    replay_commands: Option<u32>,
    compatible_from: &'a [BuildId],
}

fn valid_schema(reference: &SchemaRef) -> bool {
    reference.version != 0 && SchemaId::parse(reference.name.as_str()).is_some()
}

fn valid_demand(bytes: &[u8]) -> bool {
    !bytes.is_empty() && AuthorizationDemand::decode_canonical(bytes).is_ok()
}

fn canonical_names<T>(
    values: &[T],
    max: usize,
    name: &'static str,
    key: impl Fn(&T) -> &SchemaId,
) -> Result<(), Invalid> {
    if values.len() > max {
        return Err(Invalid::InvalidSpec(name));
    }
    let mut previous: Option<&SchemaId> = None;
    for value in values {
        let current = key(value);
        if SchemaId::parse(current.as_str()).is_none()
            || previous.is_some_and(|prior| prior >= current)
        {
            return Err(Invalid::InvalidSpec(name));
        }
        previous = Some(current);
    }
    Ok(())
}

impl Access {
    fn validate(&self) -> Result<(), Invalid> {
        for (name, bytes) in [
            ("access.start", self.start.as_slice()),
            ("access.offer", self.offer.as_slice()),
            ("access.control", self.control.as_slice()),
            ("access.accept", self.accept.as_slice()),
        ] {
            if !valid_demand(bytes) {
                return Err(Invalid::InvalidSpec(name));
            }
        }
        if self
            .attach
            .as_deref()
            .is_some_and(|bytes| !valid_demand(bytes))
        {
            return Err(Invalid::InvalidSpec("access.attach"));
        }
        Ok(())
    }
}

impl PayloadSpec {
    fn validate(&self, name: &'static str) -> Result<(), Invalid> {
        if !valid_schema(&self.schema) {
            return Err(Invalid::InvalidSpec(name));
        }
        if usize::try_from(self.max_inline_bytes).map_or(true, |value| value > MAX_BODY_BYTES)
            || usize::try_from(self.max_content_refs)
                .map_or(true, |value| value > MAX_CONTENT_REFS_PER_BODY)
            || self.max_additional_input_bytes > MAX_CONTENT_LEN
            || !valid_demand(&self.read)
        {
            return Err(Invalid::InvalidSpec(name));
        }
        if (self.max_content_refs == 0) != (self.max_content_bytes == 0) {
            return Err(Invalid::InvalidSpec(name));
        }
        let aggregate_ceiling = MAX_CONTENT_LEN.saturating_mul(self.max_content_refs.into());
        if self.max_content_bytes > aggregate_ceiling {
            return Err(Invalid::InvalidSpec(name));
        }
        Ok(())
    }
}

impl RoleSpec {
    fn validate(&self) -> Result<(), Invalid> {
        if SchemaId::parse(self.name.as_str()).is_none() || !valid_schema(&self.spec) {
            return Err(Invalid::InvalidSpec("service role"));
        }
        Ok(())
    }
}

impl LinkSpec {
    fn validate(&self) -> Result<(), Invalid> {
        if SchemaId::parse(self.name.as_str()).is_none()
            || SchemaId::parse(self.from.as_str()).is_none()
            || SchemaId::parse(self.to.as_str()).is_none()
            || !valid_schema(&self.codec)
            || self.max_messages == 0
            || self.max_messages == u32::MAX
            || self.max_bytes == 0
            || self.max_bytes == u64::MAX
            || matches!(self.rank, RankRule::Cap(0 | u32::MAX))
        {
            return Err(Invalid::InvalidSpec("link"));
        }
        Ok(())
    }
}

impl ServiceSpec {
    fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Warm { role, max_runs } => {
                role.validate()?;
                if *max_runs == 0 || *max_runs == u32::MAX {
                    return Err(Invalid::InvalidSpec("warm service"));
                }
            }
            Self::Pool { role, min, max } => {
                role.validate()?;
                if *min == 0 || min > max || *max == u16::MAX {
                    return Err(Invalid::InvalidSpec("pool service"));
                }
            }
            Self::Set { roles, links, .. } => {
                if roles.is_empty() {
                    return Err(Invalid::InvalidSpec("service roles"));
                }
                canonical_names(roles, MAX_ROLES_PER_SERVICE, "service roles", |role| {
                    &role.name
                })?;
                for role in roles {
                    role.validate()?;
                }
                canonical_names(links, MAX_LINKS_PER_SPEC, "service links", |link| {
                    &link.name
                })?;
                for link in links {
                    link.validate()?;
                    for endpoint in [&link.from, &link.to] {
                        if roles
                            .binary_search_by(|role| role.name.cmp(endpoint))
                            .is_err()
                        {
                            return Err(Invalid::InvalidSpec("service link role"));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Limits {
    fn validate(self) -> Result<(), Invalid> {
        if self.attempts == 0
            || self.attempts == u32::MAX
            || self.events == 0
            || self.events == u32::MAX
            || self.checkpoints == u32::MAX
            || self.child_runs == u32::MAX
            || self.progress_bytes == u64::MAX
            || self.checkpoint_bytes == u64::MAX
            || self.wall_millis == 0
            || self.wall_millis == u64::MAX
            || (self.checkpoints == 0) != (self.checkpoint_bytes == 0)
        {
            return Err(Invalid::InvalidSpec("limits"));
        }
        Ok(())
    }
}

impl Spec {
    /// Validate one canonical contract without consulting mutable Runtime state.
    ///
    /// Registration additionally intersects each Find Grant with the active
    /// World declaration; this method establishes that every embedded Grant is
    /// independently valid and canonically ordered first.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.version == 0 || SchemaId::parse(self.name.as_str()).is_none() {
            return Err(Invalid::InvalidSpec("spec reference"));
        }
        self.access.validate()?;
        self.input.validate("input")?;
        self.output.validate("output")?;
        self.limits.validate()?;
        self.terminal.validate()?;

        if self.output.max_additional_input_bytes != 0 {
            return Err(Invalid::InvalidSpec("output additional input"));
        }
        match self.mode {
            Mode::Interactive
                if self.input.max_additional_input_bytes == 0
                    && self.terminal.max_live_input_bytes == 0 =>
            {
                return Err(Invalid::InvalidSpec("interactive input"));
            }
            Mode::Unary | Mode::Stream
                if self.input.max_additional_input_bytes != 0
                    || self.terminal.max_live_input_bytes != 0 =>
            {
                return Err(Invalid::InvalidSpec("additional input mode"));
            }
            Mode::Unary | Mode::Stream | Mode::Interactive => {}
        }

        match &self.resume {
            Resume::Checkpoint { codec } => {
                if !valid_schema(codec)
                    || self.limits.checkpoints == 0
                    || self.limits.checkpoint_bytes == 0
                {
                    return Err(Invalid::InvalidSpec("checkpoint resume"));
                }
            }
            Resume::Replay { commands } => {
                if *commands == 0
                    || *commands > MAX_REPLAY_COMMANDS
                    || *commands > self.limits.events
                {
                    return Err(Invalid::InvalidSpec("replay commands"));
                }
            }
            Resume::Restart | Resume::Never => {}
        }
        if let Effects::Idempotent { key } = &self.effects {
            if !valid_schema(key) {
                return Err(Invalid::InvalidSpec("effect key"));
            }
        }

        if self.queries.len() > MAX_QUERIES_PER_SPEC {
            return Err(Invalid::InvalidSpec("queries"));
        }
        let mut previous: Option<Vec<u8>> = None;
        for query in &self.queries {
            let current = query.encode().map_err(|_| Invalid::InvalidSpec("query"))?;
            if previous.as_ref().is_some_and(|prior| prior >= &current) {
                return Err(Invalid::InvalidSpec("queries"));
            }
            previous = Some(current);
        }

        if let Some(service) = &self.service {
            service.validate()?;
        }
        canonical_names(&self.links, MAX_LINKS_PER_SPEC, "links", |link| &link.name)?;
        for link in &self.links {
            link.validate()?;
        }
        Ok(())
    }

    /// Prove every maximum Find Grant stays inside the active World Find
    /// declaration. Descriptor/package composition calls this after resolving
    /// the exact active implementation.
    pub fn validate_with_find(&self, schemas: &[crate::find::Schema]) -> Result<(), Invalid> {
        self.validate()?;
        for query in &self.queries {
            query
                .validate_within_schemas(schemas)
                .map_err(|_| Invalid::InvalidSpec("query declaration"))?;
        }
        Ok(())
    }

    /// Encode this contract to canonical standalone bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes = postcard::to_stdvec(&(SPEC_VERSION, self))
            .map_err(|_| Invalid::InvalidSpec("encoding"))?;
        if bytes.len() > MAX_SPEC_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    /// Decode exact canonical standalone bytes.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_SPEC_BYTES {
            return Err(Invalid::TooLarge);
        }
        let (version, spec): (u8, Self) =
            postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        if version != SPEC_VERSION {
            return Err(Invalid::UnsupportedVersion(version));
        }
        spec.validate()?;
        if spec.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(spec)
    }
}

impl Build {
    fn material(&self) -> BuildMaterial<'_> {
        BuildMaterial {
            world: &self.world,
            world_build: &self.world_build,
            spec: &self.spec,
            handler: &self.handler,
            dependencies: &self.dependencies,
            environment: &self.environment,
            config: &self.config,
            checkpoint: &self.checkpoint,
            replay_commands: self.replay_commands,
            compatible_from: &self.compatible_from,
        }
    }

    fn validate_material(&self) -> Result<(), Invalid> {
        if WorldId::parse(self.world.as_str()).as_ref() != Some(&self.world) {
            return Err(Invalid::InvalidBuild("world"));
        }
        if !valid_schema(&self.spec) {
            return Err(Invalid::InvalidBuild("spec"));
        }
        if self.config.len() > MAX_CONFIG_REFS_PER_BUILD
            || self
                .config
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return Err(Invalid::InvalidBuild("config"));
        }
        if let Some(checkpoint) = &self.checkpoint {
            if !valid_schema(checkpoint) {
                return Err(Invalid::InvalidBuild("checkpoint"));
            }
        }
        if let Some(commands) = self.replay_commands {
            if commands == 0 || commands > MAX_REPLAY_COMMANDS {
                return Err(Invalid::InvalidBuild("replay commands"));
            }
        }
        if self.checkpoint.is_some() && self.replay_commands.is_some() {
            return Err(Invalid::InvalidBuild("resume artifacts"));
        }
        if self.compatible_from.len() > MAX_COMPATIBLE_BUILDS
            || self
                .compatible_from
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return Err(Invalid::InvalidBuild("compatible builds"));
        }
        Ok(())
    }

    fn material_bytes(&self) -> Result<Vec<u8>, Invalid> {
        self.validate_material()?;
        postcard::to_stdvec(&(BUILD_VERSION, self.material()))
            .map_err(|_| Invalid::InvalidBuild("encoding"))
    }

    /// Derive the stable identity of this Build's immutable executable
    /// material. Publisher and signature do not participate.
    pub fn derived_id(&self) -> Result<BuildId, Invalid> {
        let material = self.material_bytes()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(BUILD_ID_DOMAIN);
        hasher.update(&material);
        Ok(BuildId(*hasher.finalize().as_bytes()))
    }

    fn signature_preimage(&self) -> Result<Vec<u8>, Invalid> {
        let body = postcard::to_stdvec(&(
            BUILD_VERSION,
            self.id,
            self.material(),
            &self.publisher,
            &self.signature.signer,
            self.signature.algorithm,
        ))
        .map_err(|_| Invalid::InvalidBuild("signature encoding"))?;
        let mut preimage = Vec::with_capacity(
            2usize
                .saturating_add(BUILD_SIGNATURE_DOMAIN.len())
                .saturating_add(4)
                .saturating_add(body.len()),
        );
        let domain_len = u16::try_from(BUILD_SIGNATURE_DOMAIN.len())
            .map_err(|_| Invalid::InvalidBuild("signature domain"))?;
        let body_len =
            u32::try_from(body.len()).map_err(|_| Invalid::InvalidBuild("signature encoding"))?;
        preimage.extend_from_slice(&domain_len.to_be_bytes());
        preimage.extend_from_slice(BUILD_SIGNATURE_DOMAIN);
        preimage.extend_from_slice(&body_len.to_be_bytes());
        preimage.extend_from_slice(&body);
        Ok(preimage)
    }

    /// Replace the identity and signature placeholders with a canonical
    /// publication envelope signed by `device_seed`.
    ///
    /// The publisher must already be set. Mechanics later proves that the
    /// derived signer belonged to that Actor at the publication frontier.
    pub fn sign(mut self, device_seed: &[u8; 32]) -> Result<Self, Invalid> {
        if ActorId::parse(self.publisher.as_str()).as_ref() != Some(&self.publisher) {
            return Err(Invalid::InvalidBuild("publisher"));
        }
        self.id = self.derived_id()?;
        self.signature.signer = mechanics::actor::device_from_seed(device_seed);
        self.signature.algorithm = SIGNATURE_ED25519;
        self.signature.bytes = [0; 64];
        let preimage = self.signature_preimage()?;
        self.signature.bytes = mechanics::actor::sign_detached(device_seed, &preimage);
        self.validate()?;
        Ok(self)
    }

    /// Validate the complete self-contained Build envelope.
    ///
    /// This proves byte shape, content identity, and the device signature. It
    /// intentionally cannot prove that the signer represented `publisher` or
    /// that publication satisfied the Spec's Build demand; those are Mechanics
    /// decisions at a pinned authority frontier.
    pub fn validate(&self) -> Result<(), Invalid> {
        self.validate_material()?;
        if ActorId::parse(self.publisher.as_str()).as_ref() != Some(&self.publisher) {
            return Err(Invalid::InvalidBuild("publisher"));
        }
        if DeviceId::parse(self.signature.signer.as_str()).as_ref() != Some(&self.signature.signer)
        {
            return Err(Invalid::InvalidBuild("signer"));
        }
        if self.signature.algorithm != SIGNATURE_ED25519 {
            return Err(Invalid::UnsupportedSignatureAlgorithm(
                self.signature.algorithm,
            ));
        }
        let derived = self.derived_id()?;
        if self.id != derived {
            return Err(Invalid::BuildIdMismatch);
        }
        if self.compatible_from.binary_search(&self.id).is_ok() {
            return Err(Invalid::InvalidBuild("self compatibility"));
        }
        let public_key = self
            .signature
            .signer
            .key_bytes()
            .ok_or(Invalid::InvalidBuild("signer"))?;
        let preimage = self.signature_preimage()?;
        if !mechanics::actor::verify_detached(&public_key, &preimage, &self.signature.bytes) {
            return Err(Invalid::BadBuildSignature);
        }
        Ok(())
    }

    /// Encode this complete Build publication to canonical standalone bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes = postcard::to_stdvec(&(BUILD_VERSION, self))
            .map_err(|_| Invalid::InvalidBuild("encoding"))?;
        if bytes.len() > MAX_BUILD_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    /// Decode and validate exact canonical standalone Build bytes.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_BUILD_BYTES {
            return Err(Invalid::TooLarge);
        }
        let (version, build): (u8, Self) =
            postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        if version != BUILD_VERSION {
            return Err(Invalid::UnsupportedVersion(version));
        }
        build.validate()?;
        if build.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(build)
    }
}

/// Exact local coordinates implemented by one package handler.
///
/// The binding is descriptive metadata used during package composition. It is
/// not durable Exec state and it cannot select a different Build at call time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandlerBinding {
    pub spec: SchemaRef,
    pub build: BuildId,
    pub artifact: ContentRef,
    pub role: Option<SchemaId>,
    pub links: Vec<SchemaId>,
}

/// Product code for one exact, locally installed Build.
///
/// Implementations receive only [`Context`]. Runtime, rather than the handler,
/// turns a successful [`Candidate`] into canonical, attributed Run events.
pub trait Handler: std::marker::Send + Sync {
    fn binding(&self) -> &HandlerBinding;

    /// The strongest boundary this performer actually supplies. Existing
    /// in-process handlers remain Advisory; non-Unary dispatch requires a
    /// process boundary or stronger.
    fn enforcement(&self) -> Enforcement {
        Enforcement::Advisory
    }

    fn handle(&self, context: &mut dyn HandlerContext) -> Result<Candidate, Failure>;
}

/// The executable half of one installed World package.
///
/// Specs deliberately appear both here and in the reviewed World descriptor:
/// composition requires exact equality, preventing the application shell from
/// pairing reviewed semantic code with a different callable contract.
#[derive(Clone, Default)]
pub struct Package {
    specs: Vec<Spec>,
    builds: Vec<Build>,
    handlers: Vec<Arc<dyn Handler>>,
}

impl std::fmt::Debug for Package {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Package")
            .field("specs", &self.specs)
            .field("builds", &self.builds)
            .field(
                "handlers",
                &self
                    .handlers
                    .iter()
                    .map(|handler| handler.binding())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Package {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_spec(mut self, spec: Spec) -> Self {
        self.specs.push(spec);
        self
    }

    pub fn with_build(mut self, build: Build) -> Self {
        self.builds.push(build);
        self
    }

    pub fn with_handler(mut self, handler: Arc<dyn Handler>) -> Self {
        self.handlers.push(handler);
        self
    }

    pub fn specs(&self) -> &[Spec] {
        &self.specs
    }

    pub fn builds(&self) -> &[Build] {
        &self.builds
    }

    pub fn handlers(&self) -> &[Arc<dyn Handler>] {
        &self.handlers
    }

    /// Resolve the exact local implementation pinned by one committed Attempt.
    ///
    /// Selection never falls back to another version or Build. In particular,
    /// a retry whose Build differs from `Started` is a coordinate failure even
    /// when the package contains another handler for the same Spec name.
    pub fn select<'a>(
        &'a self,
        run: &Run,
        attempt: &Attempt,
    ) -> Result<Selection<'a>, SelectionInvalid> {
        if run.id != run.started.run || attempt.run != run.id || attempt.build != run.started.build
        {
            return Err(SelectionInvalid::Coordinates);
        }
        let spec = self
            .specs
            .iter()
            .find(|spec| {
                spec.name == run.started.spec.name && spec.version == run.started.spec.version
            })
            .ok_or_else(|| SelectionInvalid::Spec(run.started.spec.clone()))?;
        let build = self
            .builds
            .iter()
            .find(|build| build.id == attempt.build)
            .ok_or(SelectionInvalid::Build(attempt.build))?;
        if build.spec != run.started.spec
            || build.world != run.started.world
            || build.world_build != run.started.world_implementation
        {
            return Err(SelectionInvalid::Coordinates);
        }
        let role = attempt.lease.as_ref().map(|lease| &lease.role);
        let handler = self
            .handlers
            .iter()
            .find(|handler| {
                let binding = handler.binding();
                binding.spec == run.started.spec
                    && binding.build == attempt.build
                    && binding.artifact == build.handler
                    && binding.role.as_ref() == role
            })
            .ok_or_else(|| SelectionInvalid::Handler {
                spec: run.started.spec.clone(),
                build: attempt.build,
                role: role.cloned(),
            })?;
        Ok(Selection {
            spec,
            build,
            handler: handler.as_ref(),
        })
    }

    /// Validate an executable package against one exact reviewed World.
    ///
    /// This is a composition check, not publication or dispatch admission.
    /// Mechanics still proves Build authority at its pinned frontier, and the
    /// dispatcher still commits an Attempt before calling a handler.
    pub fn validate(
        &self,
        world: &WorldId,
        reviewed_implementation: &[u8; 32],
        descriptor: &crate::world::Descriptor,
    ) -> Result<(), PackageInvalid> {
        if &descriptor.id != world {
            return Err(PackageInvalid::DescriptorWorld(descriptor.id.clone()));
        }
        let mut spec_names = BTreeSet::new();
        for spec in &self.specs {
            let reference = SchemaRef {
                name: spec.name.clone(),
                version: spec.version,
            };
            spec.validate_with_find(&descriptor.find_schemas)
                .map_err(|source| PackageInvalid::InvalidSpec {
                    spec: reference.clone(),
                    source,
                })?;
            if !spec_names.insert(spec.name.clone()) {
                return Err(PackageInvalid::DuplicateSpecName(spec.name.clone()));
            }
        }
        if self.specs != descriptor.exec_specs {
            return Err(PackageInvalid::SpecRegistrationMismatch);
        }

        let mut builds = BTreeMap::new();
        for build in &self.builds {
            build
                .validate()
                .map_err(|source| PackageInvalid::InvalidBuild {
                    build: build.id,
                    source,
                })?;
            if &build.world != world {
                return Err(PackageInvalid::BuildWorld {
                    build: build.id,
                    actual: build.world.clone(),
                });
            }
            if &build.world_build != reviewed_implementation {
                return Err(PackageInvalid::BuildImplementation(build.id));
            }
            let Some(spec) = self
                .specs
                .iter()
                .find(|spec| spec.name == build.spec.name && spec.version == build.spec.version)
            else {
                return Err(PackageInvalid::UnknownBuildSpec {
                    build: build.id,
                    spec: build.spec.clone(),
                });
            };
            if !build_resume_matches_spec(build, spec) {
                return Err(PackageInvalid::BuildResume(build.id));
            }
            if builds.insert(build.id, build).is_some() {
                return Err(PackageInvalid::DuplicateBuild(build.id));
            }
        }

        let mut handlers = BTreeSet::new();
        for handler in &self.handlers {
            let binding = handler.binding();
            validate_handler_binding(binding)?;
            let Some(spec) = self.specs.iter().find(|spec| {
                spec.name == binding.spec.name && spec.version == binding.spec.version
            }) else {
                return Err(PackageInvalid::UnknownHandlerSpec(binding.spec.clone()));
            };
            let Some(build) = builds.get(&binding.build) else {
                return Err(PackageInvalid::UnknownHandlerBuild(binding.build));
            };
            let spec_matches = build.spec == binding.spec;
            let artifact_matches = build.handler == binding.artifact;
            if !spec_matches || !artifact_matches {
                return Err(PackageInvalid::HandlerBuild(binding.build));
            }
            if !handlers.insert((binding.spec.clone(), binding.build, binding.role.clone())) {
                return Err(PackageInvalid::DuplicateHandler {
                    spec: binding.spec.clone(),
                    build: binding.build,
                    role: binding.role.clone(),
                });
            }
            validate_handler_role_and_links(binding, spec)?;
        }
        Ok(())
    }
}

/// One exact, locally available Spec/Build/handler resolution.
pub struct Selection<'a> {
    spec: &'a Spec,
    build: &'a Build,
    handler: &'a dyn Handler,
}

impl std::fmt::Debug for Selection<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Selection")
            .field("spec", &self.spec)
            .field("build", &self.build.id)
            .field("handler", self.handler.binding())
            .finish()
    }
}

impl<'a> Selection<'a> {
    pub const fn spec(&self) -> &'a Spec {
        self.spec
    }

    pub const fn build(&self) -> &'a Build {
        self.build
    }
}

/// Why exact local handler selection failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionInvalid {
    Coordinates,
    Spec(SchemaRef),
    Build(BuildId),
    Handler {
        spec: SchemaRef,
        build: BuildId,
        role: Option<SchemaId>,
    },
}

impl std::fmt::Display for SelectionInvalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for SelectionInvalid {}

fn build_resume_matches_spec(build: &Build, spec: &Spec) -> bool {
    match &spec.resume {
        Resume::Restart | Resume::Never => {
            build.checkpoint.is_none() && build.replay_commands.is_none()
        }
        Resume::Checkpoint { codec } => {
            build.checkpoint.as_ref() == Some(codec) && build.replay_commands.is_none()
        }
        Resume::Replay { commands } => {
            build.checkpoint.is_none() && build.replay_commands == Some(*commands)
        }
    }
}

fn validate_handler_binding(binding: &HandlerBinding) -> Result<(), PackageInvalid> {
    if !valid_schema(&binding.spec)
        || binding
            .role
            .as_ref()
            .is_some_and(|role| SchemaId::parse(role.as_str()).as_ref() != Some(role))
        || binding
            .links
            .iter()
            .any(|link| SchemaId::parse(link.as_str()).as_ref() != Some(link))
        || binding
            .links
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left >= right))
    {
        return Err(PackageInvalid::InvalidHandler(binding.build));
    }
    Ok(())
}

fn validate_handler_role_and_links(
    binding: &HandlerBinding,
    spec: &Spec,
) -> Result<(), PackageInvalid> {
    if let Some(role) = &binding.role {
        let role_exists = match &spec.service {
            Some(ServiceSpec::Warm { role: declared, .. })
            | Some(ServiceSpec::Pool { role: declared, .. }) => &declared.name == role,
            Some(ServiceSpec::Set { roles, .. }) => roles.iter().any(|item| &item.name == role),
            None => false,
        };
        if !role_exists {
            return Err(PackageInvalid::UnknownHandlerRole {
                spec: binding.spec.clone(),
                role: role.clone(),
            });
        }
    }

    for link in &binding.links {
        let ordinary = spec
            .links
            .iter()
            .filter(|declared| {
                &declared.name == link
                    && binding
                        .role
                        .as_ref()
                        .is_none_or(|role| &declared.from == role)
            })
            .count();
        let service = match (&spec.service, &binding.role) {
            (Some(ServiceSpec::Set { links, .. }), Some(role)) => links
                .iter()
                .filter(|declared| &declared.name == link && &declared.from == role)
                .count(),
            _ => 0,
        };
        match (ordinary, service) {
            (0, 0) => {
                return Err(PackageInvalid::UndeclaredHandlerLink {
                    spec: binding.spec.clone(),
                    link: link.clone(),
                });
            }
            (1, 0) | (0, 1) => {}
            _ => return Err(PackageInvalid::InvalidHandler(binding.build)),
        }
    }
    Ok(())
}

/// Why an executable World package was refused during composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageInvalid {
    DescriptorWorld(WorldId),
    InvalidSpec {
        spec: SchemaRef,
        source: Invalid,
    },
    DuplicateSpecName(SchemaId),
    SpecRegistrationMismatch,
    InvalidBuild {
        build: BuildId,
        source: Invalid,
    },
    DuplicateBuild(BuildId),
    BuildWorld {
        build: BuildId,
        actual: WorldId,
    },
    BuildImplementation(BuildId),
    UnknownBuildSpec {
        build: BuildId,
        spec: SchemaRef,
    },
    BuildResume(BuildId),
    InvalidHandler(BuildId),
    UnknownHandlerSpec(SchemaRef),
    UnknownHandlerBuild(BuildId),
    HandlerBuild(BuildId),
    DuplicateHandler {
        spec: SchemaRef,
        build: BuildId,
        role: Option<SchemaId>,
    },
    UnknownHandlerRole {
        spec: SchemaRef,
        role: SchemaId,
    },
    UndeclaredHandlerLink {
        spec: SchemaRef,
        link: SchemaId,
    },
}

impl std::fmt::Display for PackageInvalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PackageInvalid {}

/// Handler-produced output before Runtime attributes and commits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub output: SchemaRef,
    pub inline: Vec<u8>,
    pub content: Vec<ContentRef>,
    pub content_bytes: u64,
    pub terminal: TerminalClass,
    pub usage: Vec<Resource>,
    pub evidence: Vec<ContentRef>,
}

impl Candidate {
    pub(crate) fn validate_with_spec(&self, spec: &Spec) -> Result<(), Failure> {
        let inline_bytes = u64::try_from(self.inline.len()).map_err(|_| Failure::InvalidOutcome)?;
        let content_count =
            u64::try_from(self.content.len()).map_err(|_| Failure::InvalidOutcome)?;
        if self.output != spec.output.schema
            || inline_bytes > u64::from(spec.output.max_inline_bytes)
            || content_count > u64::from(spec.output.max_content_refs)
            || self.content_bytes > spec.output.max_content_bytes
            || !valid_content_geometry(&self.content, self.content_bytes)
            || !canonical_content_refs(&self.evidence)
            || validate_resources(&self.usage, "event").is_err()
        {
            return Err(Failure::InvalidOutcome);
        }
        Ok(())
    }

    /// Commit to the exact typed output material without attributing it to an
    /// Attempt. Runtime adds those durable coordinates in `Returned`.
    pub fn digest(&self) -> Result<[u8; 32], Failure> {
        let bytes = postcard::to_stdvec(&(
            &self.output,
            &self.inline,
            &self.content,
            self.content_bytes,
        ))
        .map_err(|_| Failure::InvalidOutcome)?;
        Ok(blake3::derive_key(OUTPUT_DIGEST_CONTEXT, &bytes))
    }
}

/// One bounded read delegated to the ordinary Runtime Find evaluator.
///
/// The dispatching host implements this over the Session that admitted the
/// Attempt; the handler never holds the Session. Every query reaches the
/// delegate already proven inside one instantiated Grant and within the
/// Attempt's declared wall budget, and the delegate pins evaluation to the
/// Run's parent Manifest root — a handler cannot select another
/// interpretation, and "latest" never enters a Run.
pub trait FindDelegate {
    fn find(&self, query: crate::find::Query) -> Result<crate::find::Answer, crate::find::Failure>;
}

/// One bounded plaintext-range read delegated to the host's content plane.
///
/// The dispatching host implements this over its own content authority; the
/// handler never holds it. Every read reaches the delegate only after
/// [`Context::read_content`] proved the ref is named by the Run's committed
/// truth — a handler cannot browse the content plane, only open what its
/// Started inputs and its own resume checkpoint already name.
pub trait ContentDelegate {
    fn read(&self, content: &ContentRef, offset: u64, len: usize) -> Result<Vec<u8>, Failure>;
}

/// One channel of transient performer output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputChannel {
    /// A PTY's single byte stream. Runtime never fabricates stderr separation.
    Merged,
    Stdout,
    Stderr,
}

/// One controller action delivered to an interactive performer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Stdin(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Interrupt,
    StdinEof,
}

/// Typed absence or refusal at the activation-local terminal boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFailure {
    Unsupported,
    Unavailable,
    MismatchedAttempt,
    Detached,
    Ended,
    InputLimit,
    OutputLimit,
    Os,
}

/// Host-owned live output and transcript facade lent to a non-Unary handler.
pub trait OutputSink: std::marker::Send + Sync {
    /// Append exact bytes and return the exclusive byte cursor after them.
    fn write(&self, channel: OutputChannel, bytes: &[u8]) -> Result<u64, TerminalFailure>;
    /// Record the exact output cursor at which geometry took effect.
    fn resized(&self, cols: u16, rows: u16) -> Result<u64, TerminalFailure>;
    /// Observe performer exit without claiming a durable Returned fact.
    fn exited(&self, code: Option<i32>) -> Result<(), TerminalFailure>;
    /// Return the bounded transcript batch when the Spec opted into one.
    fn transcript(&self) -> Result<Option<Vec<u8>>, TerminalFailure>;
}

/// Host-owned controller facade lent only to an Interactive handler.
pub trait InputSource: std::marker::Send + Sync {
    /// Wait for one already-bounded controller action. `None` is an ordinary
    /// timeout so a performer can continue checking cancellation and exit.
    fn receive(&self, timeout: std::time::Duration) -> Result<Option<InputEvent>, TerminalFailure>;
}

/// One activation-local terminal record. Output ranges use half-open byte
/// cursors; geometry and endings are stamped at the exact current cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalRecord {
    Output {
        start: u64,
        end: u64,
        channel: OutputChannel,
        bytes: Vec<u8>,
    },
    Resized {
        at: u64,
        cols: u16,
        rows: u16,
    },
    ProcessExited {
        at: u64,
        code: Option<i32>,
    },
    AttemptEnded {
        at: u64,
        event: EventId,
    },
    /// Counted live-ring loss. This is emitted to a reader that asks before
    /// the retained base; it is never synthesized as terminal state.
    Gap {
        after_seq: u64,
        dropped_bytes: u64,
    },
    Suppressed {
        at: u64,
        dropped_bytes: u64,
    },
}

/// Bounded late-attach replay followed by the current splice cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRead {
    pub base_offset: u64,
    pub next_offset: u64,
    pub dropped_bytes: u64,
    pub records: Vec<TerminalRecord>,
    pub ended: bool,
}

/// Versioned durable recording sealed by Runtime, never by the handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptBatch {
    pub version: u8,
    pub run: RunId,
    pub attempt: AttemptId,
    pub records: Vec<TerminalRecord>,
}

impl TranscriptBatch {
    pub const VERSION: u8 = 1;

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, TerminalFailure> {
        let batch: Self = postcard::from_bytes(bytes).map_err(|_| TerminalFailure::Unavailable)?;
        if batch.version != Self::VERSION
            || postcard::to_stdvec(&batch).map_err(|_| TerminalFailure::Unavailable)? != bytes
        {
            return Err(TerminalFailure::Unsupported);
        }
        Ok(batch)
    }
}

struct TerminalState {
    base_offset: u64,
    next_offset: u64,
    ring_bytes: usize,
    ring: VecDeque<TerminalRecord>,
    input_bytes: u64,
    input: VecDeque<InputEvent>,
    input_ended: bool,
    process_exited: bool,
    attempt_ended: bool,
    attached: u32,
    transcript: Vec<TerminalRecord>,
    transcript_suppressed: u64,
}

/// Station-activation state for one exact `(RunId, AttemptId)`.
///
/// This is the concrete host behind the OutputSink/InputSource facets. It is
/// intentionally not serializable and never becomes a second Run model.
pub(crate) struct TerminalSession {
    run: RunId,
    attempt: AttemptId,
    ring_limit: usize,
    output_limit: u64,
    input_limit: u64,
    transcript_limit: usize,
    transcript_enabled: bool,
    state: std::sync::Mutex<TerminalState>,
    input_ready: std::sync::Condvar,
}

impl TerminalSession {
    pub(crate) fn new(
        run: RunId,
        attempt: AttemptId,
        progress_bytes: u64,
        terminal: TerminalSpec,
    ) -> Self {
        const MAX_RING_BYTES: u64 = 1024 * 1024;
        let ring_limit = usize::try_from(progress_bytes.min(MAX_RING_BYTES)).unwrap_or(usize::MAX);
        let transcript_limit = usize::try_from(terminal.max_transcript_bytes).unwrap_or(usize::MAX);
        Self {
            run,
            attempt,
            ring_limit,
            output_limit: progress_bytes,
            input_limit: terminal.max_live_input_bytes,
            transcript_limit,
            transcript_enabled: matches!(terminal.transcript, Transcript::OnReturn),
            state: std::sync::Mutex::new(TerminalState {
                base_offset: 0,
                next_offset: 0,
                ring_bytes: 0,
                ring: VecDeque::new(),
                input_bytes: 0,
                input: VecDeque::new(),
                input_ended: false,
                process_exited: false,
                attempt_ended: false,
                attached: 0,
                transcript: Vec::new(),
                transcript_suppressed: 0,
            }),
            input_ready: std::sync::Condvar::new(),
        }
    }

    pub(crate) const fn coordinates(&self) -> (RunId, AttemptId) {
        (self.run, self.attempt)
    }

    pub(crate) fn attach(self: &Arc<Self>) -> TerminalAttachment {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.attached = state.attached.saturating_add(1);
        drop(state);
        TerminalAttachment {
            session: self.clone(),
            detached: false,
        }
    }

    pub(crate) fn attempt_ended(&self, event: EventId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.attempt_ended {
            return;
        }
        state.attempt_ended = true;
        let record = TerminalRecord::AttemptEnded {
            at: state.next_offset,
            event,
        };
        Self::push_metadata(&mut state, record.clone());
        self.push_transcript(&mut state, record, 0);
        self.input_ready.notify_all();
    }

    fn push_metadata(state: &mut TerminalState, record: TerminalRecord) {
        const MAX_METADATA_RECORDS: usize = 1_024;
        state.ring.push_back(record);
        while state.ring.len() > MAX_METADATA_RECORDS {
            let Some(dropped) = state.ring.pop_front() else {
                break;
            };
            if let TerminalRecord::Output { end, bytes, .. } = dropped {
                state.ring_bytes = state.ring_bytes.saturating_sub(bytes.len());
                state.base_offset = state.base_offset.max(end);
            }
        }
    }

    fn push_transcript(&self, state: &mut TerminalState, record: TerminalRecord, bytes: u64) {
        if !self.transcript_enabled || self.transcript_limit == 0 {
            return;
        }
        let mut candidate = state.transcript.clone();
        candidate.push(record);
        let batch = TranscriptBatch {
            version: TranscriptBatch::VERSION,
            run: self.run,
            attempt: self.attempt,
            records: candidate,
        };
        if postcard::to_stdvec(&batch).is_ok_and(|encoded| encoded.len() <= self.transcript_limit) {
            state.transcript = batch.records;
        } else {
            state.transcript_suppressed = state.transcript_suppressed.saturating_add(bytes);
        }
    }

    fn enqueue(&self, event: InputEvent) -> Result<(), TerminalFailure> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.attempt_ended || state.process_exited {
            return Err(TerminalFailure::Ended);
        }
        if state.input_ended {
            return Err(TerminalFailure::Ended);
        }
        if let InputEvent::Stdin(bytes) = &event {
            let added = u64::try_from(bytes.len()).map_err(|_| TerminalFailure::InputLimit)?;
            let next = state
                .input_bytes
                .checked_add(added)
                .ok_or(TerminalFailure::InputLimit)?;
            if added == 0 || next > self.input_limit {
                return Err(TerminalFailure::InputLimit);
            }
            state.input_bytes = next;
        }
        if matches!(event, InputEvent::StdinEof) {
            state.input_ended = true;
        }
        state.input.push_back(event);
        self.input_ready.notify_one();
        Ok(())
    }
}

impl OutputSink for TerminalSession {
    fn write(&self, channel: OutputChannel, bytes: &[u8]) -> Result<u64, TerminalFailure> {
        if bytes.is_empty() {
            return Ok(self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .next_offset);
        }
        let added = u64::try_from(bytes.len()).map_err(|_| TerminalFailure::OutputLimit)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.process_exited || state.attempt_ended {
            return Err(TerminalFailure::Ended);
        }
        let start = state.next_offset;
        let end = start
            .checked_add(added)
            .ok_or(TerminalFailure::OutputLimit)?;
        if end > self.output_limit {
            return Err(TerminalFailure::OutputLimit);
        }
        state.next_offset = end;
        let record = TerminalRecord::Output {
            start,
            end,
            channel,
            bytes: bytes.to_vec(),
        };
        self.push_transcript(&mut state, record.clone(), added);
        state.ring.push_back(record);
        state.ring_bytes = state.ring_bytes.saturating_add(bytes.len());
        while state.ring_bytes > self.ring_limit {
            let Some(front) = state.ring.pop_front() else {
                break;
            };
            match front {
                TerminalRecord::Output {
                    start,
                    end,
                    channel,
                    bytes,
                } => {
                    let excess = state.ring_bytes.saturating_sub(self.ring_limit);
                    if excess < bytes.len() {
                        let tail = bytes
                            .get(excess..)
                            .ok_or(TerminalFailure::OutputLimit)?
                            .to_vec();
                        let shifted =
                            start.saturating_add(u64::try_from(excess).unwrap_or(u64::MAX));
                        state.ring.push_front(TerminalRecord::Output {
                            start: shifted,
                            end,
                            channel,
                            bytes: tail,
                        });
                        state.ring_bytes = self.ring_limit;
                        state.base_offset = shifted;
                        break;
                    }
                    state.ring_bytes = state.ring_bytes.saturating_sub(bytes.len());
                    state.base_offset = end;
                }
                _ => {}
            }
        }
        Ok(end)
    }

    fn resized(&self, cols: u16, rows: u16) -> Result<u64, TerminalFailure> {
        if cols == 0 || rows == 0 {
            return Err(TerminalFailure::Unsupported);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.process_exited || state.attempt_ended {
            return Err(TerminalFailure::Ended);
        }
        let at = state.next_offset;
        let record = TerminalRecord::Resized { at, cols, rows };
        Self::push_metadata(&mut state, record.clone());
        self.push_transcript(&mut state, record, 0);
        Ok(at)
    }

    fn exited(&self, code: Option<i32>) -> Result<(), TerminalFailure> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.process_exited {
            return Ok(());
        }
        state.process_exited = true;
        let record = TerminalRecord::ProcessExited {
            at: state.next_offset,
            code,
        };
        Self::push_metadata(&mut state, record.clone());
        self.push_transcript(&mut state, record, 0);
        self.input_ready.notify_all();
        Ok(())
    }

    fn transcript(&self) -> Result<Option<Vec<u8>>, TerminalFailure> {
        if !self.transcript_enabled {
            return Ok(None);
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut records = state.transcript.clone();
        if state.transcript_suppressed != 0 {
            records.push(TerminalRecord::Suppressed {
                at: state.next_offset,
                dropped_bytes: state.transcript_suppressed,
            });
        }
        let batch = TranscriptBatch {
            version: TranscriptBatch::VERSION,
            run: self.run,
            attempt: self.attempt,
            records,
        };
        let mut encoded = postcard::to_stdvec(&batch).map_err(|_| TerminalFailure::Unavailable)?;
        if encoded.len() > self.transcript_limit {
            let fallback = TranscriptBatch {
                version: TranscriptBatch::VERSION,
                run: self.run,
                attempt: self.attempt,
                records: vec![TerminalRecord::Suppressed {
                    at: state.next_offset,
                    dropped_bytes: state.next_offset,
                }],
            };
            encoded = postcard::to_stdvec(&fallback).map_err(|_| TerminalFailure::Unavailable)?;
        }
        if encoded.len() > self.transcript_limit {
            return Err(TerminalFailure::OutputLimit);
        }
        Ok(Some(encoded))
    }
}

impl InputSource for TerminalSession {
    fn receive(&self, timeout: std::time::Duration) -> Result<Option<InputEvent>, TerminalFailure> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.input.is_empty() && !state.attempt_ended && !state.process_exited {
            let waited = self
                .input_ready
                .wait_timeout(state, timeout)
                .unwrap_or_else(|poison| poison.into_inner());
            state = waited.0;
        }
        if let Some(event) = state.input.pop_front() {
            return Ok(Some(event));
        }
        if state.attempt_ended || state.process_exited {
            return Err(TerminalFailure::Ended);
        }
        Ok(None)
    }
}

/// Disposable view/controller handle. Dropping or detaching it never cancels
/// the durable Run and never closes the performer.
pub(crate) struct TerminalAttachment {
    session: Arc<TerminalSession>,
    detached: bool,
}

impl TerminalAttachment {
    pub(crate) fn read(
        &self,
        cursor: u64,
        max_bytes: usize,
    ) -> Result<TerminalRead, TerminalFailure> {
        if self.detached {
            return Err(TerminalFailure::Detached);
        }
        let state = self
            .session
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if cursor > state.next_offset {
            return Err(TerminalFailure::Unavailable);
        }
        let mut remaining = max_bytes;
        let mut splice = cursor.max(state.base_offset);
        let mut records = Vec::new();
        let dropped_bytes = state.base_offset.saturating_sub(cursor);
        if dropped_bytes != 0 {
            records.push(TerminalRecord::Gap {
                after_seq: state.base_offset,
                dropped_bytes,
            });
        }
        for record in &state.ring {
            match record {
                TerminalRecord::Output {
                    start,
                    end,
                    channel,
                    bytes,
                } if *end > cursor && remaining != 0 => {
                    let skip = if cursor > *start {
                        usize::try_from(cursor.saturating_sub(*start)).unwrap_or(usize::MAX)
                    } else {
                        0
                    };
                    let Some(tail) = bytes.get(skip..) else {
                        continue;
                    };
                    let take = remaining.min(tail.len());
                    let Some(part) = tail.get(..take) else {
                        continue;
                    };
                    let part_start = start.saturating_add(u64::try_from(skip).unwrap_or(u64::MAX));
                    let part_end =
                        part_start.saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
                    records.push(TerminalRecord::Output {
                        start: part_start,
                        end: part_end,
                        channel: *channel,
                        bytes: part.to_vec(),
                    });
                    splice = splice.max(part_end);
                    remaining = remaining.saturating_sub(take);
                }
                TerminalRecord::Resized { at, .. }
                | TerminalRecord::ProcessExited { at, .. }
                | TerminalRecord::AttemptEnded { at, .. }
                | TerminalRecord::Suppressed { at, .. }
                    if *at >= cursor =>
                {
                    records.push(record.clone());
                }
                _ => {}
            }
        }
        Ok(TerminalRead {
            base_offset: state.base_offset,
            next_offset: splice,
            dropped_bytes,
            records,
            ended: state.attempt_ended || state.process_exited,
        })
    }

    pub(crate) fn stdin(&self, bytes: &[u8]) -> Result<(), TerminalFailure> {
        if self.detached {
            return Err(TerminalFailure::Detached);
        }
        self.session.enqueue(InputEvent::Stdin(bytes.to_vec()))
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16) -> Result<(), TerminalFailure> {
        if self.detached || cols == 0 || rows == 0 {
            return Err(if self.detached {
                TerminalFailure::Detached
            } else {
                TerminalFailure::Unsupported
            });
        }
        self.session.enqueue(InputEvent::Resize { cols, rows })
    }

    pub(crate) fn interrupt(&self) -> Result<(), TerminalFailure> {
        if self.detached {
            return Err(TerminalFailure::Detached);
        }
        self.session.enqueue(InputEvent::Interrupt)
    }

    pub(crate) fn end_input(&self) -> Result<(), TerminalFailure> {
        if self.detached {
            return Err(TerminalFailure::Detached);
        }
        self.session.enqueue(InputEvent::StdinEof)
    }

    pub(crate) fn detach(&mut self) {
        if self.detached {
            return;
        }
        self.detached = true;
        let mut state = self
            .session
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.attached = state.attached.saturating_sub(1);
    }
}

impl Drop for TerminalAttachment {
    fn drop(&mut self) {
        self.detach();
    }
}

/// The capability facets a host lends one dispatched Attempt.
///
/// Every field is optional on purpose: absence is the refusal, and a facet
/// joins this struct in the same commit as the bounded facade that honours
/// it — never before.
#[derive(Default, Clone, Copy)]
pub struct Facets<'a> {
    pub find: Option<&'a dyn FindDelegate>,
    pub content: Option<&'a dyn ContentDelegate>,
    pub output: Option<&'a dyn OutputSink>,
    pub input: Option<&'a dyn InputSource>,
}

/// Activation-owned control for one locally begun Attempt.
///
/// The handle is registered before invocation and remains addressable until
/// completion is durably recorded. Keeping the flag in this owned handle is
/// what lets a later `CancelAsked` reach a performer already running on a
/// different thread. Interactive input, resize, and signal controls extend
/// this same handle rather than introducing another Attempt registry.
#[derive(Clone, Default)]
pub struct AttemptControl {
    cancel_asked: Arc<AtomicBool>,
}

impl AttemptControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel_asked.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_asked.load(Ordering::SeqCst)
    }

    pub(crate) fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel_asked
    }
}

/// A bounded, authenticated view of one admitted local Attempt.
///
/// There is intentionally no World, Replica, Engine, Mechanics, device-key,
/// transport, filesystem, environment, or unrestricted query handle here.
/// Capability facets such as content reads and Find are added only with their
/// own bounded facade; absence from this type means the handler has no access.
pub struct Context<'a> {
    run: &'a Run,
    start: &'a Start,
    attempt: &'a Attempt,
    links: &'a [LinkSpec],
    cancel_asked: &'a AtomicBool,
    committed_cancel: bool,
    checkpoints: Vec<CheckpointRef>,
    children: Vec<Start>,
    output_blobs: Vec<Vec<u8>>,
    find: Option<&'a dyn FindDelegate>,
    content: Option<&'a dyn ContentDelegate>,
    output: Option<&'a dyn OutputSink>,
    input: Option<&'a dyn InputSource>,
    /// Declared query work already admitted, in wall milliseconds. Charged
    /// from each admitted query's own bound before evaluation, so the sum a
    /// handler can ask for never exceeds the Attempt's wall budget even
    /// though the first backend's enforcement is advisory.
    query_wall_charged: u64,
    /// Checkpoint bytes staged for Runtime to seal after the callback. Each
    /// entry carries its minted coordinates with a placeholder ContentRef
    /// that binding replaces once the host has sealed the blob.
    checkpoint_blobs: Vec<(CheckpointRef, Vec<u8>)>,
}

/// The exact bounded capability available to one Exec handler.
///
/// Runtime's native [`Context`] and a generation-pinned World process expose
/// the same surface. Admission, content containment, and Find grants remain
/// host-authoritative; moving product code out of process does not move those
/// decisions with it.
pub trait HandlerContext {
    fn resume_checkpoint(&self) -> Option<&CheckpointRef>;
    fn read_content(
        &self,
        content: &ContentRef,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, Failure>;
    fn query(&mut self, query: crate::find::Query) -> Result<crate::find::Answer, Failure>;
    fn world(&self) -> &WorldId;
    fn run(&self) -> RunId;
    fn attempt(&self) -> AttemptId;
    fn spec(&self) -> &SchemaRef;
    fn build(&self) -> BuildId;
    fn input_schema(&self) -> &SchemaRef;
    fn input_inline(&self) -> &[u8];
    fn input_content(&self) -> &[ContentRef];
    fn accepted_resources(&self) -> &[Resource];
    fn enforcement_evidence(&self) -> Option<ContentRef>;
    fn limits(&self) -> AttemptLimits;
    fn links(&self) -> &[LinkSpec];
    fn cancel_asked(&self) -> bool;
    /// Number of checkpoints already committed before this handler turn.
    fn committed_checkpoint_count(&self) -> u32;
    fn save_checkpoint(&mut self, checkpoint: CheckpointRef) -> Result<(), Failure>;
    fn save_checkpoint_bytes(&mut self, bytes: Vec<u8>) -> Result<(), Failure>;
    fn start_child(&mut self, child: Start) -> Result<(), Failure>;
    fn stage_output(&mut self, bytes: Vec<u8>) -> Result<(), Failure>;
    fn mode(&self) -> Mode {
        Mode::Unary
    }
    fn terminal(&self) -> TerminalSpec {
        TerminalSpec::disabled()
    }
    fn write_output(&self, _channel: OutputChannel, _bytes: &[u8]) -> Result<u64, Failure> {
        Err(Failure::OutputUnavailable)
    }
    fn output_resized(&self, _cols: u16, _rows: u16) -> Result<u64, Failure> {
        Err(Failure::OutputUnavailable)
    }
    fn observe_exit(&self, _code: Option<i32>) -> Result<(), Failure> {
        Err(Failure::OutputUnavailable)
    }
    fn receive_input(&self, _timeout: std::time::Duration) -> Result<Option<InputEvent>, Failure> {
        Err(Failure::InputUnavailable)
    }
    fn continuation_input(&self) -> Option<&ContinuationInput> {
        None
    }
}

impl HandlerContext for Context<'_> {
    fn resume_checkpoint(&self) -> Option<&CheckpointRef> {
        Context::resume_checkpoint(self)
    }

    fn read_content(
        &self,
        content: &ContentRef,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, Failure> {
        Context::read_content(self, content, offset, len)
    }

    fn query(&mut self, query: crate::find::Query) -> Result<crate::find::Answer, Failure> {
        Context::query(self, query)
    }

    fn world(&self) -> &WorldId {
        Context::world(self)
    }

    fn run(&self) -> RunId {
        Context::run(self)
    }

    fn attempt(&self) -> AttemptId {
        Context::attempt(self)
    }

    fn spec(&self) -> &SchemaRef {
        Context::spec(self)
    }

    fn build(&self) -> BuildId {
        Context::build(self)
    }

    fn input_schema(&self) -> &SchemaRef {
        Context::input_schema(self)
    }

    fn input_inline(&self) -> &[u8] {
        Context::input_inline(self)
    }

    fn input_content(&self) -> &[ContentRef] {
        Context::input_content(self)
    }

    fn accepted_resources(&self) -> &[Resource] {
        Context::accepted_resources(self)
    }

    fn enforcement_evidence(&self) -> Option<ContentRef> {
        Context::enforcement_evidence(self)
    }

    fn limits(&self) -> AttemptLimits {
        Context::limits(self)
    }

    fn links(&self) -> &[LinkSpec] {
        Context::links(self)
    }

    fn cancel_asked(&self) -> bool {
        Context::cancel_asked(self)
    }

    fn committed_checkpoint_count(&self) -> u32 {
        u32::try_from(self.attempt.checkpoints.len()).unwrap_or(u32::MAX)
    }

    fn save_checkpoint(&mut self, checkpoint: CheckpointRef) -> Result<(), Failure> {
        Context::save_checkpoint(self, checkpoint)
    }

    fn save_checkpoint_bytes(&mut self, bytes: Vec<u8>) -> Result<(), Failure> {
        Context::save_checkpoint_bytes(self, bytes)
    }

    fn start_child(&mut self, child: Start) -> Result<(), Failure> {
        Context::start_child(self, child)
    }

    fn stage_output(&mut self, bytes: Vec<u8>) -> Result<(), Failure> {
        Context::stage_output(self, bytes)
    }

    fn mode(&self) -> Mode {
        self.run.started.mode
    }

    fn terminal(&self) -> TerminalSpec {
        self.run.started.terminal
    }

    fn write_output(&self, channel: OutputChannel, bytes: &[u8]) -> Result<u64, Failure> {
        self.output
            .ok_or(Failure::OutputUnavailable)?
            .write(channel, bytes)
            .map_err(Failure::Terminal)
    }

    fn output_resized(&self, cols: u16, rows: u16) -> Result<u64, Failure> {
        self.output
            .ok_or(Failure::OutputUnavailable)?
            .resized(cols, rows)
            .map_err(Failure::Terminal)
    }

    fn observe_exit(&self, code: Option<i32>) -> Result<(), Failure> {
        self.output
            .ok_or(Failure::OutputUnavailable)?
            .exited(code)
            .map_err(Failure::Terminal)
    }

    fn receive_input(&self, timeout: std::time::Duration) -> Result<Option<InputEvent>, Failure> {
        self.input
            .ok_or(Failure::InputUnavailable)?
            .receive(timeout)
            .map_err(Failure::Terminal)
    }

    fn continuation_input(&self) -> Option<&ContinuationInput> {
        self.attempt.continuation.as_ref()
    }
}

impl std::fmt::Debug for Context<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Context")
            .field("run", &self.run.id)
            .field("attempt", &self.attempt.id)
            .field("links", &self.links)
            .field("cancel_asked", &self.cancel_asked())
            .finish()
    }
}

impl<'a> Context<'a> {
    pub(crate) fn new(
        run: &'a Run,
        start: &'a Start,
        attempt: &'a Attempt,
        links: &'a [LinkSpec],
        cancel_asked: &'a AtomicBool,
    ) -> Result<Self, Failure> {
        if run.id != run.started.run
            || attempt.run != run.id
            || attempt.build != run.started.build
            || start.spec != run.started.spec
            || start.build != run.started.build
            || start.input.content != run.started.input_content
            || start.input.content_bytes != run.started.input_content_bytes
        {
            return Err(Failure::InvalidContext);
        }
        Ok(Self {
            run,
            start,
            attempt,
            links,
            cancel_asked,
            committed_cancel: !run.cancel_asked.is_empty(),
            checkpoints: Vec::new(),
            children: Vec::new(),
            output_blobs: Vec::new(),
            find: None,
            content: None,
            output: None,
            input: None,
            query_wall_charged: 0,
            checkpoint_blobs: Vec::new(),
        })
    }

    /// Attach the host's bounded Find delegate. Without one, every
    /// [`Context::query`] refuses as unavailable rather than pretending an
    /// empty corpus answered.
    pub(crate) fn with_find(mut self, delegate: &'a dyn FindDelegate) -> Self {
        self.find = Some(delegate);
        self
    }

    /// Attach the host's bounded content delegate. Without one, every
    /// [`Context::read_content`] refuses as unavailable rather than
    /// pretending empty bytes answered.
    pub(crate) fn with_content(mut self, delegate: &'a dyn ContentDelegate) -> Self {
        self.content = Some(delegate);
        self
    }

    pub(crate) fn with_output(mut self, sink: &'a dyn OutputSink) -> Self {
        self.output = Some(sink);
        self
    }

    pub(crate) fn with_input(mut self, source: &'a dyn InputSource) -> Self {
        self.input = Some(source);
        self
    }

    /// The checkpoint this Attempt was admitted to resume from, when the
    /// authorized `Try` carried one. `None` means a fresh start — a handler
    /// must not guess a checkpoint the lease did not bind.
    pub fn resume_checkpoint(&self) -> Option<&CheckpointRef> {
        self.attempt.checkpoint.as_ref()
    }

    /// Read one bounded plaintext range of content this Run's truth names.
    ///
    /// Fail-closed before the host is asked: the ref must be one of the
    /// `Started` input ContentRefs or this Attempt's resume checkpoint.
    /// Everything else refuses — an Attempt reads what it was commissioned
    /// with, never what the Station happens to hold.
    pub fn read_content(
        &self,
        content: &ContentRef,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, Failure> {
        let Some(delegate) = self.content else {
            return Err(Failure::ContentUnavailable);
        };
        let named = self.run.started.input_content.contains(content)
            || self
                .attempt
                .checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.content == *content)
            || self
                .attempt
                .continuation
                .as_ref()
                .is_some_and(|continuation| continuation.input.content.contains(content));
        if !named {
            return Err(Failure::ContentRefused);
        }
        delegate.read(content, offset, len)
    }

    /// Run one bounded Find query inside this Attempt.
    ///
    /// Admission is fail-closed and happens before any evaluator work: the
    /// whole query must sit inside one Grant instantiated at `Started` (a
    /// widened op set, field, edge, gate, feature, mode, or bound rejects);
    /// the query may not name its own historical interpretation — the
    /// delegate pins the Run's parent Manifest root; and the query's declared
    /// wall ceiling is charged against the Attempt's remaining wall budget
    /// so the admitted total can never exceed it. A child Run receives none
    /// of these Grants: its own `Started` carries its own independently
    /// authorized set.
    pub fn query(&mut self, query: crate::find::Query) -> Result<crate::find::Answer, Failure> {
        let Some(delegate) = self.find else {
            return Err(Failure::QueryUnavailable);
        };
        if query.publication.is_some() {
            return Err(Failure::QueryRefused(crate::find::Invalid::Widening(
                "publication",
            )));
        }
        let mut admission = Err(Failure::QueryRefused(crate::find::Invalid::NotGranted(
            "no instantiated grant",
        )));
        for granted in &self.start.queries {
            match query.validate_within(&granted.grant) {
                Ok(()) => {
                    admission = Ok(());
                    break;
                }
                Err(refusal) => admission = Err(Failure::QueryRefused(refusal)),
            }
        }
        admission?;
        let charged = self
            .query_wall_charged
            .checked_add(query.bound.wall_millis)
            .ok_or(Failure::QueryBudget)?;
        if charged > self.attempt.limits.wall_millis {
            return Err(Failure::QueryBudget);
        }
        self.query_wall_charged = charged;
        delegate.find(query).map_err(|_| Failure::Handler)
    }

    pub fn world(&self) -> &WorldId {
        &self.run.started.world
    }

    pub const fn run(&self) -> RunId {
        self.run.id
    }

    pub const fn attempt(&self) -> AttemptId {
        self.attempt.id
    }

    pub fn spec(&self) -> &SchemaRef {
        &self.run.started.spec
    }

    pub const fn build(&self) -> BuildId {
        self.attempt.build
    }

    pub fn input_schema(&self) -> &SchemaRef {
        &self.run.started.input
    }

    pub fn input_inline(&self) -> &[u8] {
        &self.start.input.inline
    }

    pub fn input_content(&self) -> &[ContentRef] {
        &self.start.input.content
    }

    pub fn accepted_resources(&self) -> &[Resource] {
        &self.attempt.resources
    }

    /// Isolation evidence when a backend produced some. Advisory is absence.
    pub const fn enforcement_evidence(&self) -> Option<ContentRef> {
        self.attempt.enforcement
    }

    pub const fn limits(&self) -> AttemptLimits {
        self.attempt.limits
    }

    pub fn links(&self) -> &[LinkSpec] {
        self.links
    }

    pub fn cancel_asked(&self) -> bool {
        self.committed_cancel || self.cancel_asked.load(Ordering::Acquire)
    }

    /// Stage an immutable checkpoint reference for Runtime to validate,
    /// attribute, and commit after the handler returns.
    pub fn save_checkpoint(&mut self, checkpoint: CheckpointRef) -> Result<(), Failure> {
        let expected = self.next_checkpoint_sequence()?;
        if checkpoint.build != self.attempt.build
            || checkpoint.sequence != expected
            || expected > self.attempt.limits.checkpoints
        {
            return Err(Failure::InvalidCheckpoint);
        }
        self.checkpoints.push(checkpoint);
        Ok(())
    }

    /// Stage checkpoint bytes for Runtime to seal, attribute, and commit
    /// after the handler returns.
    ///
    /// The in-process backend has no content authority — that is the point of
    /// the boundary — so the bytes travel out with the completion, the host
    /// seals them exactly as it seals output blobs, and the `Saved` event is
    /// committed only once the sealed ContentRef is bound. The staged
    /// coordinates are minted here so the sequence chain stays the handler's
    /// declared order.
    pub fn save_checkpoint_bytes(&mut self, bytes: Vec<u8>) -> Result<(), Failure> {
        let sequence = self.next_checkpoint_sequence()?;
        if sequence > self.attempt.limits.checkpoints {
            return Err(Failure::CheckpointLimit);
        }
        let mut staged_bytes: u64 = 0;
        for (_, blob) in &self.checkpoint_blobs {
            let held = u64::try_from(blob.len()).map_err(|_| Failure::CheckpointLimit)?;
            staged_bytes = staged_bytes.saturating_add(held);
        }
        let added = u64::try_from(bytes.len()).map_err(|_| Failure::CheckpointLimit)?;
        if staged_bytes.saturating_add(added) > self.attempt.limits.checkpoint_bytes {
            return Err(Failure::CheckpointLimit);
        }
        self.checkpoint_blobs.push((
            CheckpointRef {
                content: ContentRef {
                    content_id: [0; 32],
                },
                build: self.attempt.build,
                sequence,
            },
            bytes,
        ));
        Ok(())
    }

    fn next_checkpoint_sequence(&self) -> Result<u32, Failure> {
        let committed =
            u32::try_from(self.attempt.checkpoints.len()).map_err(|_| Failure::CheckpointLimit)?;
        let staged = u32::try_from(
            self.checkpoints
                .len()
                .saturating_add(self.checkpoint_blobs.len()),
        )
        .map_err(|_| Failure::CheckpointLimit)?;
        committed
            .checked_add(staged)
            .and_then(|count| count.checked_add(1))
            .ok_or(Failure::CheckpointLimit)
    }

    /// Stage one independently bounded child Run.
    ///
    /// A child's query Grants are not inherited privilege: they are validated
    /// against the child's own Spec at lowering and admitted under the child
    /// Start's own demand, so a parent can only propose what the child's
    /// declaration already permits — never lend its own instantiated set.
    pub fn start_child(&mut self, child: Start) -> Result<(), Failure> {
        let staged = u32::try_from(self.children.len()).map_err(|_| Failure::ChildLimit)?;
        if staged >= self.attempt.limits.child_runs {
            return Err(Failure::ChildLimit);
        }
        child.validate().map_err(|_| Failure::InvalidChild)?;
        if child.parent != Some(self.run.id)
            || child.limits.events > self.attempt.limits.events
            || child.limits.checkpoints > self.attempt.limits.checkpoints
            || child.limits.child_runs > self.attempt.limits.child_runs
            || child.limits.progress_bytes > self.attempt.limits.progress_bytes
            || child.limits.checkpoint_bytes > self.attempt.limits.checkpoint_bytes
            || child.limits.wall_millis > self.attempt.limits.wall_millis
        {
            return Err(Failure::InvalidChild);
        }
        self.children.push(child);
        Ok(())
    }

    /// Stage one output blob for Runtime to ingest and bind as a ContentRef.
    ///
    /// The handler never receives a content-plane handle. Runtime seals the
    /// bytes after the callback returns and only then attributes them on
    /// `Returned`.
    pub fn stage_output(&mut self, bytes: Vec<u8>) -> Result<(), Failure> {
        let staged = u64::try_from(self.output_blobs.len()).map_err(|_| Failure::InvalidOutcome)?;
        let next = staged.checked_add(1).ok_or(Failure::InvalidOutcome)?;
        if next > u64::from(self.run.started.limits.events.max(1)) {
            return Err(Failure::InvalidOutcome);
        }
        self.output_blobs.push(bytes);
        Ok(())
    }

    fn validate_staged(&self, spec: &Spec, build: &Build) -> Result<(), Failure> {
        let staged_any = !self.checkpoints.is_empty() || !self.checkpoint_blobs.is_empty();
        if staged_any
            && !matches!(
                &spec.resume,
                Resume::Checkpoint { codec }
                    if build.checkpoint.as_ref() == Some(codec)
                        && self.checkpoints.iter().all(|checkpoint| checkpoint.build == build.id)
                        && self
                            .checkpoint_blobs
                            .iter()
                            .all(|(checkpoint, _)| checkpoint.build == build.id)
            )
        {
            return Err(Failure::InvalidCheckpoint);
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn take_staged(
        &mut self,
    ) -> (
        Vec<CheckpointRef>,
        Vec<(CheckpointRef, Vec<u8>)>,
        Vec<Start>,
        Vec<Vec<u8>>,
    ) {
        (
            std::mem::take(&mut self.checkpoints),
            std::mem::take(&mut self.checkpoint_blobs),
            std::mem::take(&mut self.children),
            std::mem::take(&mut self.output_blobs),
        )
    }
}

/// A validated handler return plus Runtime-owned lifecycle material staged
/// through the bounded Context. No canonical event bytes cross this seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    candidate: Candidate,
    checkpoints: Vec<CheckpointRef>,
    checkpoint_blobs: Vec<(CheckpointRef, Vec<u8>)>,
    children: Vec<Start>,
    output_blobs: Vec<Vec<u8>>,
    transcript_blob: Option<Vec<u8>>,
    transcript: Option<ContentRef>,
}

impl Completion {
    pub fn candidate(&self) -> &Candidate {
        &self.candidate
    }

    pub fn checkpoints(&self) -> &[CheckpointRef] {
        &self.checkpoints
    }

    pub fn children(&self) -> &[Start] {
        &self.children
    }

    pub fn output_blobs(&self) -> &[Vec<u8>] {
        &self.output_blobs
    }

    pub fn transcript_blob(&self) -> Option<&[u8]> {
        self.transcript_blob.as_deref()
    }

    /// Bind the one host-sealed transcript artifact. It is deliberately
    /// separate from handler output content and its payload contract.
    pub fn bind_transcript_content(&mut self, content: ContentRef) -> Result<(), Failure> {
        if self.transcript_blob.is_none() || self.transcript.is_some() {
            return Err(Failure::InvalidOutcome);
        }
        self.transcript = Some(content);
        Ok(())
    }

    /// Bind ingested output ContentRefs after Runtime seals staged blobs.
    pub fn bind_output_content(
        &mut self,
        content: Vec<ContentRef>,
        content_bytes: u64,
    ) -> Result<(), Failure> {
        self.candidate.content = content;
        self.candidate.content_bytes = content_bytes;
        Ok(())
    }

    /// Checkpoint bytes the handler staged for the host to seal, in the
    /// order their coordinates were minted.
    pub fn checkpoint_blobs(&self) -> impl Iterator<Item = &[u8]> {
        self.checkpoint_blobs
            .iter()
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// Bind sealed ContentRefs onto the staged checkpoint blobs, in order.
    ///
    /// After binding, the checkpoints are one sequence-ordered set — a
    /// `Saved` event can only ever name content the host actually sealed.
    pub fn bind_checkpoint_content(&mut self, content: Vec<ContentRef>) -> Result<(), Failure> {
        if content.len() != self.checkpoint_blobs.len() {
            return Err(Failure::InvalidCheckpoint);
        }
        for ((checkpoint, _), sealed) in self.checkpoint_blobs.drain(..).zip(content) {
            self.checkpoints.push(CheckpointRef {
                content: sealed,
                ..checkpoint
            });
        }
        self.checkpoints
            .sort_by_key(|checkpoint| checkpoint.sequence);
        Ok(())
    }

    /// Build predecessor-bound Saved/Returned facts. Runtime remains the only
    /// author of canonical lifecycle bytes and commits these in order.
    pub fn events(
        &self,
        run: RunId,
        attempt: AttemptId,
        predecessors: Vec<EventId>,
    ) -> Result<Vec<RunEvent>, Invalid> {
        let mut events = Vec::with_capacity(self.checkpoints.len().saturating_add(1));
        let mut prior = predecessors;
        for checkpoint in &self.checkpoints {
            let event = RunEvent::new(
                prior,
                RunEventKind::Saved(Saved {
                    run,
                    attempt,
                    checkpoint: checkpoint.clone(),
                }),
            )?;
            prior = vec![event.id()?];
            events.push(event);
        }
        let output_inline_bytes = u32::try_from(self.candidate.inline.len())
            .map_err(|_| Invalid::InvalidEvent("returned"))?;
        events.push(RunEvent::new(
            prior,
            RunEventKind::Returned(Returned {
                run,
                attempt,
                output: self.candidate.output.clone(),
                output_digest: self
                    .candidate
                    .digest()
                    .map_err(|_| Invalid::InvalidEvent("returned"))?,
                output_inline_bytes,
                output_content: self.candidate.content.clone(),
                output_content_bytes: self.candidate.content_bytes,
                terminal: self.candidate.terminal,
                usage: self.candidate.usage.clone(),
                evidence: self.candidate.evidence.clone(),
                transcript: self.transcript,
            }),
        )?);
        Ok(events)
    }
}

/// A local handler refusal. Runtime maps this typed value to durable Attempt
/// failure; the handler never writes canonical failure bytes itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Cancelled,
    Handler,
    /// The Attempt's wall budget expired and the backend stopped the work.
    /// Distinct from [`Failure::Handler`]: the handler did nothing wrong.
    Wall,
    /// The operating system refused process control — spawn, pipe, or wait.
    /// Distinct from [`Failure::Handler`]: the handler never ran.
    Os,
    /// Unary stdout crossed the Attempt's declared progress-byte ceiling.
    /// The child is stopped and its partial prefix is discarded: bounded
    /// output must never be presented as a complete successful Outcome.
    OutputLimit,
    /// A non-Unary handler was invoked without the host output facade.
    OutputUnavailable,
    /// An Interactive handler was invoked without the host input facade.
    InputUnavailable,
    /// A typed terminal boundary refusal.
    Terminal(TerminalFailure),
    InvalidContext,
    InvalidOutcome,
    InvalidCheckpoint,
    CheckpointLimit,
    InvalidChild,
    ChildLimit,
    /// This Attempt was dispatched without a Find delegate, so the handler
    /// has no query access at all. Distinct from a refused query: nothing
    /// could have been asked.
    QueryUnavailable,
    /// The query escaped every instantiated Grant, named its own historical
    /// interpretation, or was malformed. Admission fails before any
    /// evaluator work.
    QueryRefused(crate::find::Invalid),
    /// Admitting this query's declared work ceiling would exceed the
    /// Attempt's remaining wall budget.
    QueryBudget,
    /// This Attempt was dispatched without a content delegate, so the handler
    /// can read no bytes at all — distinct from a refused ref: nothing could
    /// have been asked.
    ContentUnavailable,
    /// The named ContentRef is not one this Run's committed truth entitles
    /// the Attempt to read: not a Started input and not this Attempt's
    /// resume checkpoint. Admission fails before the host is asked.
    ContentRefused,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Failure {}

/// What a backend actually enforces for declared resource ceilings.
///
/// Requested and reserved resources remain scheduling and accounting evidence;
/// this statement is separate so neither can be mistaken for isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Enforcement {
    Advisory,
    Measured,
    Process,
    Container,
    ExternallyAttested,
}

/// The generation-one trusted in-process Rust backend.
///
/// It contains handler panics and validates candidate output, but it supplies
/// no kernel or process boundary. A different backend can change enforcement
/// without changing durable Run truth or the handler contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct InProcess;

/// A performer that realizes one Build as a local child process.
///
/// Deliberately not a new dispatch seam: the [`Handler`] trait is already
/// the performer contract, and what distinguishes a backend is what it
/// enforces and claims — which is exactly what [`Enforcement`] states.
/// Input rides stdin; output rides stdout, bounded by the Attempt's
/// progress budget before the Spec's own output ceiling judges it; exit
/// zero is `Succeeded` and nonzero `ApplicationFailed` — a child that runs
/// and fails has *returned*, and acceptance judges it like any Outcome.
/// The wall limit is enforced by killing the child, and a committed
/// cancellation is honored both before spawn and mid-run through the
/// cancellation watch.
///
/// Two boundaries stated plainly. This build spawns only the program named
/// at composition, keyed to the Build's artifact identity — the same trust
/// as a compiled-in handler, no new surface; content-addressed artifact
/// materialization is the later general path. And a subprocess is a process
/// boundary, never a sandbox: the enforcement claim is [`Enforcement::Process`]
/// where the platform can kill and reap, and it does not make the child's
/// behavior trustworthy.
pub struct Subprocess {
    binding: HandlerBinding,
    output: SchemaRef,
    program: std::path::PathBuf,
    args: Vec<String>,
    working_directory: std::path::PathBuf,
    /// Composition-declared client environment for the outer process only.
    /// The ambient daemon environment is always cleared first.
    environment: Vec<(String, String)>,
}

impl Subprocess {
    fn consume_process_output(
        context: &dyn HandlerContext,
        mode: Mode,
        ceiling: usize,
        observed: &mut usize,
        stdout: &mut Vec<u8>,
        channel: OutputChannel,
        bytes: &[u8],
    ) -> Result<(), Failure> {
        if matches!(mode, Mode::Stream) {
            *observed = observed.saturating_add(bytes.len());
            if *observed > ceiling {
                return Err(Failure::OutputLimit);
            }
            context.write_output(channel, bytes)?;
        } else if matches!(channel, OutputChannel::Stdout) {
            if stdout.len().saturating_add(bytes.len()) > ceiling {
                return Err(Failure::OutputLimit);
            }
            stdout.extend_from_slice(bytes);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn stop_pty_child(
        child: &mut std::process::Child,
        writer: std::fs::File,
        output: &std::sync::mpsc::Receiver<Vec<u8>>,
        reader: std::thread::JoinHandle<()>,
    ) {
        Self::stop_tree(child);
        let _ = child.wait();
        drop(writer);
        while !reader.is_finished() {
            match output.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        while output.try_recv().is_ok() {}
        let _ = reader.join();
    }

    pub fn new(
        spec: &Spec,
        build: &Build,
        program: std::path::PathBuf,
        args: Vec<String>,
        working_directory: std::path::PathBuf,
    ) -> Self {
        Self {
            binding: HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: build.handler,
                role: None,
                links: Vec::new(),
            },
            output: spec.output.schema.clone(),
            program,
            args,
            working_directory,
            environment: Vec::new(),
        }
    }

    /// Add the exact bounded environment required by a composition-owned
    /// outer runtime client (for example rootless Podman). These values are
    /// not ambient inheritance and do not imply container environment.
    pub fn with_environment(mut self, environment: Vec<(String, String)>) -> Self {
        self.environment = environment;
        self
    }

    /// What this performer actually enforces: a real process boundary the
    /// host can kill and reap. Never claimed as isolation.
    pub const fn enforcement() -> Enforcement {
        Enforcement::Process
    }
}

impl Subprocess {
    /// The child leads a group, so the group is addressable by the pid we hold.
    ///
    /// Without this there is nothing to signal but the child itself, and a
    /// Handler that runs `sh -c` is the shape that forks: killing the shell
    /// leaves its child running *and holding the stdout pipe*, so the collector
    /// thread below blocks until that grandchild exits on its own. The wall
    /// limit then measures the child's patience rather than bounding it — a
    /// 400ms budget took 30.3s, the full length of what it was meant to stop.
    #[cfg(unix)]
    fn own_process_group(command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt as _;
        // `0` means "a new group whose id is the child's pid", which is what
        // makes the group addressable by the pid we already hold.
        command.process_group(0);
    }

    #[cfg(not(unix))]
    fn own_process_group(_command: &mut std::process::Command) {}

    /// Hand the child none of this process's capabilities.
    ///
    /// A daemon under the service unit holds `CAP_NET_ADMIN` **ambiently**, so the
    /// net plane can open its interface without being root — and ambient is
    /// precisely the set that survives `execve`. `CapabilityBoundingSet=` bounds
    /// what may ever be gained; it drops nothing. So without this, every program an Attempt names
    /// starts holding the daemon's authority over the machine's network.
    ///
    /// Cleared in the child rather than in the daemon: `netstack::tun`'s route
    /// changes shell out to `ip`, which needs the capability in *its* child, and
    /// clearing it process-wide around a spawn would be a race every other thread
    /// could lose. `pre_exec` runs after the fork, so it reaches this child only.
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::as_conversions,
        reason = "prctl's variadic arguments are `unsigned long` by kernel contract"
    )]
    fn disinherit_capabilities(command: &mut std::process::Command) {
        {
            use std::os::unix::process::CommandExt as _;
            // SAFETY: the closure runs between fork and exec and must be
            // async-signal-safe; a bare `prctl` syscall is.
            unsafe {
                {
                    command.pre_exec(|| {
                        {
                            // SAFETY: a documented prctl option taking no pointers.
                            let cleared = libc::prctl(
                                libc::PR_CAP_AMBIENT,
                                libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
                                0 as libc::c_ulong,
                                0 as libc::c_ulong,
                                0 as libc::c_ulong,
                            );
                            if cleared < 0 {
                                {
                                    let error = std::io::Error::last_os_error();
                                    // A kernel too old for ambient capabilities has none to
                                    // clear — `PR_CAP_AMBIENT` and the unit option that fills it
                                    // arrived together, in 4.3. Any other failure refuses the
                                    // spawn: a child that kept the daemon's authority is worse
                                    // than a child that never started.
                                    if error.raw_os_error() != Some(libc::EINVAL) {
                                        {
                                            return Err(error);
                                        }
                                    }
                                }
                            }
                            Ok(())
                        }
                    });
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn disinherit_capabilities(_command: &mut std::process::Command) {
        {}
    }

    /// Kill the child and everything it started.
    ///
    /// The group first, then the process alone. The fallback is not
    /// belt-and-braces: `kill(-pid, …)` resolves only if the child actually
    /// leads a group, so a platform without one still gets the child killed —
    /// what it misses is the child's own children, which is worth saying rather
    /// than worth failing over.
    #[cfg(unix)]
    fn stop_tree(child: &std::process::Child) {
        let killed = i32::try_from(child.id())
            .ok()
            .and_then(i32::checked_neg)
            .filter(|group| *group != 0)
            .is_some_and(|group| {
                // SAFETY: the documented POSIX form. `ESRCH` for a group that
                // has already gone is a value, not undefined behaviour. The
                // negation is checked because `kill(0, …)` would signal every
                // process in *our* group, which includes the test runner.
                unsafe { libc::kill(group, libc::SIGKILL) == 0 }
            });
        if !killed {
            Self::stop_process(child);
        }
    }

    #[cfg(not(unix))]
    fn stop_tree(child: &std::process::Child) {
        Self::stop_process(child);
    }

    /// `Child::kill` takes `&mut`, and every caller here holds the child behind
    /// a shared borrow while its reader threads run.
    fn stop_process(child: &std::process::Child) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: as above.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        #[cfg(not(unix))]
        {
            let _ = child;
        }
    }

    /// Run one Interactive Attempt behind a real local PTY.
    #[cfg(unix)]
    fn handle_pty(&self, context: &mut dyn HandlerContext) -> Result<Candidate, Failure> {
        use std::io::{Read as _, Write as _};
        use std::os::fd::{AsRawFd as _, FromRawFd as _, RawFd};
        use std::os::unix::process::CommandExt as _;

        if context.cancel_asked() {
            return Err(Failure::Cancelled);
        }
        let mut master: RawFd = -1;
        let mut slave: RawFd = -1;
        let mut initial = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: all pointers name initialized storage for the documented
        // openpty call; null termios requests the platform defaults.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut initial,
            )
        } != 0
        {
            return Err(Failure::Os);
        }
        // SAFETY: openpty returned owned descriptors. Each successful dup is
        // transferred exactly once into a File/Stdio owner below.
        let stdin_fd = unsafe { libc::dup(slave) };
        let stdout_fd = unsafe { libc::dup(slave) };
        if stdin_fd < 0 || stdout_fd < 0 {
            // SAFETY: these are live descriptors returned above; close is the
            // only owner action on this error path.
            unsafe {
                libc::close(master);
                libc::close(slave);
                if stdin_fd >= 0 {
                    libc::close(stdin_fd);
                }
                if stdout_fd >= 0 {
                    libc::close(stdout_fd);
                }
            }
            return Err(Failure::Os);
        }
        // SAFETY: ownership of the three distinct descriptors moves to File.
        let stdin_file = unsafe { std::fs::File::from_raw_fd(stdin_fd) };
        let stdout_file = unsafe { std::fs::File::from_raw_fd(stdout_fd) };
        let stderr_file = unsafe { std::fs::File::from_raw_fd(slave) };
        let mut command = std::process::Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(self.environment.iter().map(|(key, value)| (key, value)))
            .current_dir(&self.working_directory)
            .stdin(std::process::Stdio::from(stdin_file))
            .stdout(std::process::Stdio::from(stdout_file))
            .stderr(std::process::Stdio::from(stderr_file));
        Self::disinherit_capabilities(&mut command);
        // SAFETY: setsid/ioctl are async-signal-safe syscalls in the forked
        // child. stdin has already been wired to the slave side of this PTY.
        unsafe {
            command.pre_exec(|| {
                let request: libc::c_ulong = libc::TIOCSCTTY.into();
                if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, request, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                // SAFETY: parent still owns the master descriptor.
                unsafe { libc::close(master) };
                return Err(Failure::Os);
            }
        };
        // SAFETY: master is the sole remaining raw owner after spawn.
        let reader_file = unsafe { std::fs::File::from_raw_fd(master) };
        let mut writer_file = match reader_file.try_clone() {
            Ok(file) => file,
            Err(_) => {
                Self::stop_tree(&child);
                let _ = child.wait();
                drop(reader_file);
                return Err(Failure::Os);
            }
        };
        let control_fd = writer_file.as_raw_fd();
        let (output_tx, output_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(16);
        let reader = std::thread::spawn(move || {
            let mut reader_file = reader_file;
            let mut chunk = [0u8; 8 * 1024];
            loop {
                match reader_file.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let Some(bytes) = chunk.get(..read) else {
                            break;
                        };
                        if output_tx.send(bytes.to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let began = std::time::Instant::now();
        let wall = std::time::Duration::from_millis(context.limits().wall_millis);
        let status = loop {
            while let Ok(bytes) = output_rx.try_recv() {
                if let Err(failure) = context.write_output(OutputChannel::Merged, &bytes) {
                    Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                    return Err(failure);
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => {
                    Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                    return Err(Failure::Os);
                }
            }
            match context.receive_input(std::time::Duration::from_millis(10)) {
                Ok(Some(InputEvent::Stdin(bytes))) => {
                    if writer_file.write_all(&bytes).is_err() || writer_file.flush().is_err() {
                        Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                        return Err(Failure::Os);
                    }
                }
                Ok(Some(InputEvent::Resize { cols, rows })) => {
                    let size = libc::winsize {
                        ws_row: rows,
                        ws_col: cols,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    };
                    // SAFETY: control_fd is this live PTY master and size is a
                    // valid winsize value for TIOCSWINSZ.
                    if unsafe { libc::ioctl(control_fd, libc::TIOCSWINSZ, &size) } < 0 {
                        Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                        return Err(Failure::Terminal(TerminalFailure::Unsupported));
                    }
                    if let Err(failure) = context.output_resized(cols, rows) {
                        Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                        return Err(failure);
                    }
                }
                Ok(Some(InputEvent::Interrupt)) => {
                    // Address the PTY's foreground process group, not a pid.
                    // SAFETY: tcgetpgrp/kill take scalar descriptors/ids.
                    let group = unsafe { libc::tcgetpgrp(control_fd) };
                    if group <= 0 || unsafe { libc::kill(-group, libc::SIGINT) } < 0 {
                        Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                        return Err(Failure::Terminal(TerminalFailure::Unavailable));
                    }
                }
                Ok(Some(InputEvent::StdinEof)) => {
                    // POSIX canonical PTYs interpret VEOF at the terminal
                    // discipline. The output side stays open for final bytes.
                    if writer_file.write_all(&[0x04]).is_err() || writer_file.flush().is_err() {
                        Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                        return Err(Failure::Os);
                    }
                }
                Ok(None) => {}
                Err(Failure::Terminal(TerminalFailure::Ended)) => {}
                Err(failure) => {
                    Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                    return Err(failure);
                }
            }
            if context.cancel_asked() {
                Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                return Err(Failure::Cancelled);
            }
            if began.elapsed() >= wall {
                Self::stop_pty_child(&mut child, writer_file, &output_rx, reader);
                return Err(Failure::Wall);
            }
        };
        drop(writer_file);
        let mut output_failure = None;
        while !reader.is_finished() {
            match output_rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(bytes) => {
                    if output_failure.is_none() {
                        if let Err(failure) = context.write_output(OutputChannel::Merged, &bytes) {
                            output_failure = Some(failure);
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        while let Ok(bytes) = output_rx.try_recv() {
            if output_failure.is_none() {
                if let Err(failure) = context.write_output(OutputChannel::Merged, &bytes) {
                    output_failure = Some(failure);
                }
            }
        }
        let _ = reader.join();
        if let Some(failure) = output_failure {
            return Err(failure);
        }
        context.observe_exit(status.code())?;
        Ok(Candidate {
            output: self.output.clone(),
            inline: Vec::new(),
            content: Vec::new(),
            content_bytes: 0,
            terminal: if status.success() {
                TerminalClass::Succeeded
            } else {
                TerminalClass::ApplicationFailed
            },
            usage: SchemaId::parse(WALL_TIME_MS)
                .map(|name| {
                    vec![Resource {
                        name,
                        amount: wall_claim(began.elapsed()),
                    }]
                })
                .unwrap_or_default(),
            evidence: Vec::new(),
        })
    }

    #[cfg(not(unix))]
    fn handle_pty(&self, _context: &mut dyn HandlerContext) -> Result<Candidate, Failure> {
        Err(Failure::Terminal(TerminalFailure::Unsupported))
    }
}

impl Handler for Subprocess {
    fn binding(&self) -> &HandlerBinding {
        &self.binding
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::Process
    }

    fn handle(&self, context: &mut dyn HandlerContext) -> Result<Candidate, Failure> {
        use std::io::{Read, Write};
        if matches!(context.mode(), Mode::Interactive) {
            return self.handle_pty(context);
        }
        if context.cancel_asked() {
            return Err(Failure::Cancelled);
        }
        let wall = std::time::Duration::from_millis(context.limits().wall_millis);
        let mut command = std::process::Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(self.environment.iter().map(|(key, value)| (key, value)))
            .current_dir(&self.working_directory)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        Self::own_process_group(&mut command);
        Self::disinherit_capabilities(&mut command);
        let mut child = command.spawn().map_err(|_| Failure::Os)?;
        let mut stdin = child.stdin.take().ok_or(Failure::Os)?;
        let input = context.input_inline().to_vec();
        let feeder = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let mut stdout = child.stdout.take().ok_or(Failure::Os)?;
        let mut stderr = child.stderr.take().ok_or(Failure::Os)?;
        let ceiling = usize::try_from(context.limits().progress_bytes).unwrap_or(usize::MAX);
        let (output_tx, output_rx) = std::sync::mpsc::sync_channel::<(OutputChannel, Vec<u8>)>(32);
        let stdout_tx = output_tx.clone();
        let stdout_collector = std::thread::spawn(move || {
            let mut chunk = [0u8; 8 * 1024];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let Some(filled) = chunk.get(..read) else {
                            break;
                        };
                        if stdout_tx
                            .send((OutputChannel::Stdout, filled.to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr_collector = std::thread::spawn(move || {
            let mut chunk = [0u8; 8 * 1024];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let Some(filled) = chunk.get(..read) else {
                            break;
                        };
                        if output_tx
                            .send((OutputChannel::Stderr, filled.to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let began = std::time::Instant::now();
        let mode = context.mode();
        let mut observed = 0usize;
        let mut collected = Vec::new();
        let status = loop {
            while let Ok((channel, bytes)) = output_rx.try_recv() {
                if let Err(failure) = Self::consume_process_output(
                    context,
                    mode,
                    ceiling,
                    &mut observed,
                    &mut collected,
                    channel,
                    &bytes,
                ) {
                    Self::stop_tree(&child);
                    let _ = child.wait();
                    let _ = feeder.join();
                    let _ = stdout_collector.join();
                    let _ = stderr_collector.join();
                    return Err(failure);
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => {
                    Self::stop_tree(&child);
                    let _ = child.wait();
                    let _ = feeder.join();
                    let _ = stdout_collector.join();
                    let _ = stderr_collector.join();
                    return Err(Failure::Os);
                }
            }
            if context.cancel_asked() {
                Self::stop_tree(&child);
                let _ = child.wait();
                let _ = feeder.join();
                let _ = stdout_collector.join();
                let _ = stderr_collector.join();
                return Err(Failure::Cancelled);
            }
            if began.elapsed() >= wall {
                Self::stop_tree(&child);
                let _ = child.wait();
                let _ = feeder.join();
                let _ = stdout_collector.join();
                let _ = stderr_collector.join();
                return Err(Failure::Wall);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let _ = feeder.join();
        let mut output_failure = None;
        while !stdout_collector.is_finished() || !stderr_collector.is_finished() {
            match output_rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok((channel, bytes)) => {
                    if output_failure.is_none() {
                        if let Err(failure) = Self::consume_process_output(
                            context,
                            mode,
                            ceiling,
                            &mut observed,
                            &mut collected,
                            channel,
                            &bytes,
                        ) {
                            output_failure = Some(failure);
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        while let Ok((channel, bytes)) = output_rx.try_recv() {
            if output_failure.is_none() {
                if let Err(failure) = Self::consume_process_output(
                    context,
                    mode,
                    ceiling,
                    &mut observed,
                    &mut collected,
                    channel,
                    &bytes,
                ) {
                    output_failure = Some(failure);
                }
            }
        }
        let _ = stdout_collector.join();
        let _ = stderr_collector.join();
        if let Some(failure) = output_failure {
            return Err(failure);
        }
        if matches!(mode, Mode::Stream) {
            context.observe_exit(status.code())?;
        }
        let inline = if matches!(mode, Mode::Stream) {
            Vec::new()
        } else {
            collected
        };
        let usage = SchemaId::parse(WALL_TIME_MS)
            .map(|name| {
                vec![Resource {
                    name,
                    amount: wall_claim(began.elapsed()),
                }]
            })
            .unwrap_or_default();
        Ok(Candidate {
            output: self.output.clone(),
            inline,
            content: Vec::new(),
            content_bytes: 0,
            terminal: if status.success() {
                TerminalClass::Succeeded
            } else {
                TerminalClass::ApplicationFailed
            },
            usage,
            evidence: Vec::new(),
        })
    }
}

/// The wall-time claim for a child that ran. A run shorter than the
/// resolution claims one unit, because a zero-amount Resource is refused as
/// a non-claim — and a child the scheduler let finish before its parent
/// could look at the clock still ran.
pub(crate) fn wall_claim(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

impl InProcess {
    pub const fn new() -> Self {
        Self
    }

    pub const fn enforcement(self) -> Enforcement {
        Enforcement::Advisory
    }

    pub fn invoke(
        self,
        selection: &Selection<'_>,
        context: &mut Context<'_>,
    ) -> Result<Completion, Failure> {
        if selection.spec.name != context.spec().name
            || selection.spec.version != context.spec().version
            || selection.build.id != context.build()
        {
            return Err(Failure::InvalidContext);
        }
        if context.cancel_asked() {
            return Err(Failure::Cancelled);
        }
        let candidate = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            selection.handler.handle(context)
        }))
        .map_err(|_| Failure::Handler)??;
        candidate.validate_with_spec(selection.spec)?;
        context.validate_staged(selection.spec, selection.build)?;
        let (checkpoints, checkpoint_blobs, children, output_blobs) = context.take_staged();
        let transcript_blob = match context.output {
            Some(output) => output.transcript().map_err(Failure::Terminal)?,
            None => None,
        };
        Ok(Completion {
            candidate,
            checkpoints,
            checkpoint_blobs,
            children,
            output_blobs,
            transcript_blob,
            transcript: None,
        })
    }
}

/// Runtime-private exact Body capability for protected Exec state.
///
/// Implementations must be pinned to one immutable read publication and must
/// enter the same Body-image cache, singleflight, and memory governor as World
/// reads. Exec deliberately does not accept a raw `ReadSnapshot`: protected
/// Run state can be cold and encrypted, and bypassing this capability would
/// both defeat admission and collapse key/capacity failures into corruption.
pub(crate) trait ReservedBodyReader {
    fn binding(&self, key: &replica::body::BodyKey) -> Option<replica::body::BodyBinding>;

    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&replica::body::BodyKey>,
        limit: usize,
    ) -> Vec<replica::body::BodyKey>;

    fn read_atomic(
        &self,
        key: &replica::body::BodyKey,
    ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure>;

    fn read_collaborative(
        &self,
        key: &replica::body::BodyKey,
    ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure>;

    fn content_descriptor(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<replica::content::ContentDescriptor>;
}

// Unit tests build small in-memory generations directly. Production has no
// raw-snapshot implementation: Session supplies the governed exact reader.
#[cfg(test)]
impl ReservedBodyReader for replica::ReadSnapshot {
    fn content_descriptor(
        &self,
        content: &replica::content::ContentRef,
    ) -> Option<replica::content::ContentDescriptor> {
        replica::ReadSnapshot::content_descriptor(self, content)
    }

    fn binding(&self, key: &replica::body::BodyKey) -> Option<replica::body::BodyBinding> {
        replica::ReadSnapshot::binding(self, key).cloned()
    }

    fn body_keys_page_with_schema(
        &self,
        world: &WorldId,
        schema: &SchemaId,
        after: Option<&replica::body::BodyKey>,
        limit: usize,
    ) -> Vec<replica::body::BodyKey> {
        replica::ReadSnapshot::body_keys_page_with_schema(self, world, schema, after, limit)
    }

    fn read_atomic(
        &self,
        key: &replica::body::BodyKey,
    ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure> {
        Ok(replica::ReadSnapshot::read(self, key).map(crate::world::BodyBytes::owned))
    }

    fn read_collaborative(
        &self,
        key: &replica::body::BodyKey,
    ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure> {
        let Some(binding) = replica::ReadSnapshot::binding(self, key) else {
            return Ok(None);
        };
        if binding.mutation_model != replica::body::MUTATION_COLLABORATIVE {
            return Err(crate::world::BodyReadFailure::NotCollaborative(
                crate::world::BodyReadCoordinate::new(key.clone(), None),
            ));
        }
        replica::ReadSnapshot::read_collaborative(self, key)
            .map(crate::world::CollaborativeBody::owned)
            .map(Some)
            .map_err(|failure| match failure {
                fabric::projection::Failure::NotCollaborative => {
                    crate::world::BodyReadFailure::NotCollaborative(
                        crate::world::BodyReadCoordinate::new(key.clone(), None),
                    )
                }
                fabric::projection::Failure::SchemaAhead => {
                    crate::world::BodyReadFailure::SchemaAhead(
                        crate::world::BodyReadCoordinate::new(key.clone(), None),
                    )
                }
                fabric::projection::Failure::Malformed => crate::world::BodyReadFailure::Corrupt(
                    crate::world::BodyReadCoordinate::new(key.clone(), None),
                ),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadFailure {
    Invalid(Invalid),
    Body(crate::world::BodyReadFailure),
    /// Durable Run truth belongs to an intentionally unsupported schema
    /// generation. This is a compatibility boundary, not corruption.
    UnsupportedRunSchema {
        found: u32,
    },
}

impl From<Invalid> for ReadFailure {
    fn from(value: Invalid) -> Self {
        Self::Invalid(value)
    }
}

impl From<crate::world::BodyReadFailure> for ReadFailure {
    fn from(value: crate::world::BodyReadFailure) -> Self {
        Self::Body(value)
    }
}

/// Local dispatch that derives every invocation from one immutable committed
/// Replica generation.
///
/// Observing and invoking both enter through [`scan_unresolved`]. Callers
/// provide semantic ids, never an in-memory Run to trust, so incomplete staged
/// material has no route to a backend.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dispatcher<'a> {
    package: &'a Package,
    backend: InProcess,
}

impl<'a> Dispatcher<'a> {
    pub(crate) const fn new(package: &'a Package, backend: InProcess) -> Self {
        Self { package, backend }
    }

    pub(crate) fn observe(
        &self,
        snapshot: &impl ReservedBodyReader,
        world: &WorldId,
    ) -> Result<Vec<Unresolved>, DispatchFailure> {
        scan_unresolved(snapshot, world).map_err(DispatchFailure::from)
    }

    pub(crate) fn invoke(
        &self,
        snapshot: &impl ReservedBodyReader,
        world: &WorldId,
        run: RunId,
        attempt: AttemptId,
        cancel_asked: &AtomicBool,
        facets: Facets<'_>,
    ) -> Result<Completion, DispatchFailure> {
        let unresolved = self.observe(snapshot, world)?;
        let unresolved = unresolved
            .iter()
            .find(|candidate| candidate.run.id == run)
            .ok_or(DispatchFailure::Run(run))?;
        let attempt = unresolved
            .run
            .attempts
            .iter()
            .find(|candidate| candidate.id == attempt)
            .ok_or(DispatchFailure::Attempt(attempt))?;
        if !matches!(
            attempt.began.as_slice(),
            [began] if began.predecessors.contains(&attempt.leased_event)
        ) {
            return Err(DispatchFailure::NotBegan(attempt.id));
        }
        if !attempt.outcomes.is_empty()
            || !attempt.failures.is_empty()
            || !attempt.cancellations.is_empty()
        {
            return Err(DispatchFailure::Terminal(attempt.id));
        }
        let selection = self
            .package
            .select(&unresolved.run, attempt)
            .map_err(DispatchFailure::Selection)?;
        if !matches!(selection.spec.mode, Mode::Unary)
            && matches!(
                selection.handler.enforcement(),
                Enforcement::Advisory | Enforcement::Measured
            )
        {
            return Err(DispatchFailure::Backend(Failure::Terminal(
                TerminalFailure::Unsupported,
            )));
        }
        if !matches!(selection.spec.mode, Mode::Unary) && facets.output.is_none() {
            return Err(DispatchFailure::Backend(Failure::OutputUnavailable));
        }
        if matches!(selection.spec.mode, Mode::Interactive) && facets.input.is_none() {
            return Err(DispatchFailure::Backend(Failure::InputUnavailable));
        }
        let links = selected_links(&selection);
        let mut context = Context::new(
            &unresolved.run,
            &unresolved.start,
            attempt,
            &links,
            cancel_asked,
        )
        .map_err(DispatchFailure::Backend)?;
        if let Some(delegate) = facets.find {
            context = context.with_find(delegate);
        }
        if let Some(delegate) = facets.content {
            context = context.with_content(delegate);
        }
        if let Some(output) = facets.output {
            context = context.with_output(output);
        }
        if let Some(input) = facets.input {
            context = context.with_input(input);
        }
        let completion = self
            .backend
            .invoke(&selection, &mut context)
            .map_err(DispatchFailure::Backend)?;
        for child in completion.children() {
            let spec = self
                .package
                .specs
                .iter()
                .find(|spec| spec.name == child.spec.name && spec.version == child.spec.version)
                .ok_or(DispatchFailure::Backend(Failure::InvalidChild))?;
            let build = self
                .package
                .builds
                .iter()
                .find(|build| build.id == child.build)
                .ok_or(DispatchFailure::Backend(Failure::InvalidChild))?;
            child
                .validate_with(spec, build)
                .map_err(|_| DispatchFailure::Backend(Failure::InvalidChild))?;
            if build.world != unresolved.run.started.world
                || build.world_build != unresolved.run.started.world_implementation
            {
                return Err(DispatchFailure::Backend(Failure::InvalidChild));
            }
        }
        Ok(completion)
    }
}

fn selected_links(selection: &Selection<'_>) -> Vec<LinkSpec> {
    let binding = selection.handler.binding();
    let mut selected = Vec::with_capacity(binding.links.len());
    for name in &binding.links {
        let ordinary = selection.spec.links.iter().find(|declared| {
            &declared.name == name
                && binding
                    .role
                    .as_ref()
                    .is_none_or(|role| &declared.from == role)
        });
        let service = match (&selection.spec.service, &binding.role) {
            (Some(ServiceSpec::Set { links, .. }), Some(role)) => links
                .iter()
                .find(|declared| &declared.name == name && &declared.from == role),
            _ => None,
        };
        if let Some(declared) = ordinary.or(service) {
            selected.push(declared.clone());
        }
    }
    selected
}

/// What the local outbox did after observing committed Runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerformStep {
    Tried {
        run: RunId,
        attempt: AttemptId,
    },
    Began {
        run: RunId,
        attempt: AttemptId,
    },
    Returned {
        run: RunId,
        attempt: AttemptId,
    },
    Failed {
        run: RunId,
        attempt: AttemptId,
        class: FailureClass,
    },
}

/// Bounded record of one local perform pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PerformReport {
    pub steps: Vec<PerformStep>,
}

/// Why committed local dispatch could not enter a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchFailure {
    Invalid(Invalid),
    BodyRead(crate::world::BodyReadFailure),
    UnsupportedRunSchema { found: u32 },
    Run(RunId),
    Attempt(AttemptId),
    NotBegan(AttemptId),
    Terminal(AttemptId),
    Selection(SelectionInvalid),
    Backend(Failure),
}

impl std::fmt::Display for DispatchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for DispatchFailure {}

impl From<ReadFailure> for DispatchFailure {
    fn from(value: ReadFailure) -> Self {
        match value {
            ReadFailure::Invalid(invalid) => Self::Invalid(invalid),
            ReadFailure::Body(failure) => Self::BodyRead(failure),
            ReadFailure::UnsupportedRunSchema { found } => Self::UnsupportedRunSchema { found },
        }
    }
}

/// Maximum canonical size of one command, checked before postcard allocates.
pub const MAX_CMD_BYTES: usize = 4 * 1024 * 1024;

macro_rules! opaque_id {
    ($name:ident, $summary:literal) => {
        #[doc = $summary]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name([u8; 16]);

        impl $name {
            /// Wrap the canonical 128-bit identity.
            pub const fn from_bytes(raw: [u8; 16]) -> Self {
                Self(raw)
            }

            /// Return the canonical 128-bit identity.
            pub const fn as_bytes(self) -> [u8; 16] {
                self.0
            }
        }
    };
}

opaque_id!(RunId, "The stable identity of one durable logical Run.");
opaque_id!(
    AttemptId,
    "The stable identity of one physical Attempt under a Run."
);

impl AttemptId {
    /// An Attempt is named by its own signed `Leased` event: the event's
    /// identity, truncated to 128 bits, is the Attempt id. Two Stations that
    /// race the same Run sign different `Leased` events — different
    /// predecessors, station, and device — so they mint different Attempts
    /// with no shared counter to collide on, and a retry differs from its
    /// predecessor by the heads it advances from. This is the fencing token,
    /// the idempotency key, and the incarnation at once.
    pub fn of_event(event: EventId) -> Self {
        let mut id = [0u8; 16];
        id.copy_from_slice(&event.as_bytes()[..16]);
        Self(id)
    }
}

/// Deterministic sparse marker key for one unresolved Run. The marker value
/// carries the canonical RunId; the derived BodyId prevents it from colliding
/// with the Run's immutable identity Body in the same World namespace.
pub(crate) fn active_run_body_key(world: &WorldId, run: RunId) -> replica::body::BodyKey {
    let digest = blake3::keyed_hash(
        &blake3::derive_key("lait.exec.active-body.key.v1", ACTIVE_RUN_BODY_DOMAIN),
        &run.as_bytes(),
    );
    let mut body = [0u8; 16];
    body.copy_from_slice(&digest.as_bytes()[..16]);
    replica::body::BodyKey {
        world: world.clone(),
        body: BodyId::from_bytes(body),
    }
}
opaque_id!(
    ServiceId,
    "The stable identity of one reusable live Service."
);
opaque_id!(OfferId, "The stable identity of one signed advisory Offer.");
opaque_id!(
    LeaseId,
    "The stable identity of one committed Service Role lease."
);

/// Initial material committed by one [`Start`] intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub inline: Vec<u8>,
    pub content: Vec<ContentRef>,
    /// Claimed aggregate plaintext bytes of `content`. Runtime verifies this
    /// against the immutable descriptors before dispatch.
    pub content_bytes: u64,
}

/// One exact instantiation of a declared maximum Find Grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryGrant {
    pub parent: crate::find::GrantDigest,
    pub grant: crate::find::Grant,
}

/// One canonical requested or accepted resource quantity.
///
/// Core quantities use names such as `cpu.millis` and `memory.bytes`; Worlds
/// may add namespaced logical quantities. Request, Offer, accepted reservation,
/// measured use, and enforced limit remain different records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Resource {
    pub name: SchemaId,
    pub amount: u64,
}

/// World-declared semantic intent to create one durable Run.
///
/// Runtime derives the Run id and every ambient Session coordinate while
/// lowering this value. The mandatory Build id deliberately has no "latest"
/// representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    pub spec: SchemaRef,
    pub build: BuildId,
    pub input: Input,
    pub parent: Option<RunId>,
    pub source: Option<RunId>,
    pub service: Option<ServiceId>,
    pub resources: Vec<Resource>,
    pub limits: Limits,
    pub queries: Vec<QueryGrant>,
    /// The one Station this Run must run on, when the requester named it.
    ///
    /// `None` leaves placement open: any consenting Station that holds the
    /// Build may lease it. `Some(station)` is a directed Start — "run it
    /// there" — and only that Station's own activation may lease it, in
    /// addition to the consent every foreign Run needs. The target is fixed
    /// at Start and is not part of the Run id, so it never forks the Run.
    #[serde(default)]
    pub target: Option<StationKey>,
}

/// The content identity of one immutable predecessor-bound Run event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId([u8; 32]);

impl EventId {
    pub const fn from_bytes(raw: [u8; 32]) -> Self {
        Self(raw)
    }

    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Runtime-derived durable coordinates for the first event in a Run.
///
/// The complete canonical [`Cmd::Start`] material is chunked separately in the
/// same protected Body. This record binds its digest and geometry so recovery
/// can reject missing, reordered, or substituted chunks before dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub space: mechanics::ids::SpaceId,
    pub world: WorldId,
    pub run: RunId,
    pub spec: SchemaRef,
    /// Interaction shape and terminal bounds copied from the exact admitted
    /// Spec so a Run remains interpretable after descriptor movement.
    pub mode: Mode,
    pub terminal: TerminalSpec,
    pub world_implementation: [u8; 32],
    pub build: BuildId,
    pub invoker: ActorId,
    pub device: DeviceId,
    pub authority_frontier: AuthorityFrontier,
    pub parent_manifest_root: [u8; 32],
    pub input: SchemaRef,
    pub input_digest: [u8; 32],
    pub input_content: Vec<ContentRef>,
    pub input_content_bytes: u64,
    pub max_additional_input_bytes: u64,
    pub resources: Vec<Resource>,
    pub limits: Limits,
    pub request: [u8; 16],
    pub command: u32,
    pub parent: Option<RunId>,
    pub source: Option<RunId>,
    pub service: Option<ServiceId>,
    pub query_grants_digest: [u8; 32],
    pub command_digest: [u8; 32],
    pub command_bytes: u32,
    pub command_chunks: u32,
    /// The Station this Run was directed to, carried from its [`Start`].
    #[serde(default)]
    pub target: Option<StationKey>,
}

/// Exact Attempt coordinates committed before an executor may begin work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leased {
    pub run: RunId,
    pub station: StationKey,
    pub station_epoch: StationEpoch,
    pub executor: ActorId,
    pub device: DeviceId,
    pub build: BuildId,
    pub offer: Option<OfferId>,
    pub offer_epoch: u64,
    pub resources: Vec<Resource>,
    pub enforcement: Option<ContentRef>,
    pub limits: AttemptLimits,
    pub lease: Option<RoleLease>,
    pub checkpoint: Option<CheckpointRef>,
    /// Bounded, root-stamped input committed by an explicit continuation Try.
    pub continuation: Option<ContinuationInput>,
}

/// An executor's durable claim that one admitted Attempt began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Began {
    pub run: RunId,
    pub attempt: AttemptId,
    pub executor: ActorId,
    pub device: DeviceId,
}

/// One immutable checkpoint produced by an Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub run: RunId,
    pub attempt: AttemptId,
    pub checkpoint: CheckpointRef,
}

/// The terminal class claimed by a handler that returned an Outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalClass {
    Succeeded,
    ApplicationFailed,
}

/// One Station's returned Outcome claim. It does not accept the Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Returned {
    pub run: RunId,
    pub attempt: AttemptId,
    pub output: SchemaRef,
    pub output_digest: [u8; 32],
    pub output_inline_bytes: u32,
    pub output_content: Vec<ContentRef>,
    pub output_content_bytes: u64,
    pub terminal: TerminalClass,
    /// Canonical accounting quantities claimed by the returning Station.
    pub usage: Vec<Resource>,
    /// Immutable validation or attestation artifacts, sorted by ContentRef.
    pub evidence: Vec<ContentRef>,
    /// Host-authored terminal recording. This is deliberately not handler
    /// output and is validated against `TerminalSpec`, not `PayloadSpec`.
    pub transcript: Option<ContentRef>,
}

/// Why an Attempt terminated without returning an Outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureClass {
    Handler,
    Backend,
    Protocol,
    Deadline,
    Fence,
    /// Completion is unknown: this Attempt is dead and must not be retried
    /// under the same id. Used for an inherited `Began` and for a prior-epoch
    /// `Leased` that never began.
    Unknown,
}

/// A durable Attempt failure, distinct from a returned application failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failed {
    pub run: RunId,
    pub attempt: AttemptId,
    pub class: FailureClass,
    /// Immutable evidence supporting the failure claim, sorted by ContentRef.
    pub evidence: Vec<ContentRef>,
}

/// A committed request to cancel a Run; it does not claim work has stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAsked {
    pub run: RunId,
    pub actor: ActorId,
    pub device: DeviceId,
}

/// A durable claim that cancellation completed for the named scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancelled {
    pub run: RunId,
    pub attempt: Option<AttemptId>,
    pub actor: ActorId,
    pub device: DeviceId,
}

/// An authorized choice of one returned Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub run: RunId,
    pub attempt: AttemptId,
    pub actor: ActorId,
    pub device: DeviceId,
}

/// An authorized rejection of one returned Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub run: RunId,
    pub attempt: AttemptId,
    pub actor: ActorId,
    pub device: DeviceId,
}

/// The evidence a third party cites when it presumes an Attempt dead.
///
/// A presumption is a suspicion, not a verdict: it names why the presumer
/// stopped waiting, never that it observed the Attempt stop. Unreachability
/// alone is never here — "could not be asked" is not "is not running".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresumedEvidence {
    /// The Attempt's admission-to-return budget elapsed on the presumer's
    /// own clock, measured from when it first observed the lease.
    Deadline,
    /// A heartbeat the Spec led the presumer to expect did not arrive in time.
    MissedHeartbeat,
    /// The performing Station activation is known to have ended (its epoch is
    /// gone) without a terminal fact.
    DeadEpoch,
}

/// A third party's durable, **non-terminal** claim that an Attempt is presumed
/// dead. It never resolves the Run: the original executor may still return,
/// and a later `Returned` or `Failed` on the same Attempt reconciles over it.
/// Written by a holder of the Spec's `control` demand other than the executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presumed {
    pub run: RunId,
    pub attempt: AttemptId,
    pub evidence: PresumedEvidence,
    pub presumer: ActorId,
    pub device: DeviceId,
}

/// The executor's durable refresh of its own liveness: proof it is still on
/// the Attempt, resetting the deadline a third party measures against. Written
/// by the executor device only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renewed {
    pub run: RunId,
    pub attempt: AttemptId,
    pub executor: ActorId,
    pub device: DeviceId,
}

/// Generation-1 predecessor-bound Run event kinds.
///
/// Declaration order is wire order and therefore part of the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunEventKind {
    Started(Started),
    Leased(Leased),
    Began(Began),
    Saved(Saved),
    Returned(Returned),
    Failed(Failed),
    CancelAsked(CancelAsked),
    Cancelled(Cancelled),
    Accepted(Accepted),
    Rejected(Rejected),
    Presumed(Presumed),
    Renewed(Renewed),
}

/// One immutable Run event and the exact prior events it advances from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    pub predecessors: Vec<EventId>,
    pub kind: RunEventKind,
}

impl RunEvent {
    pub fn started(started: Started) -> Result<Self, Invalid> {
        let event = Self {
            predecessors: Vec::new(),
            kind: RunEventKind::Started(started),
        };
        event.validate()?;
        Ok(event)
    }

    /// Construct and validate a non-root predecessor-bound lifecycle event.
    pub fn new(predecessors: Vec<EventId>, kind: RunEventKind) -> Result<Self, Invalid> {
        let event = Self { predecessors, kind };
        event.validate()?;
        Ok(event)
    }

    pub fn as_started(&self) -> Option<&Started> {
        match &self.kind {
            RunEventKind::Started(started) => Some(started),
            _ => None,
        }
    }

    /// The durable Run this event belongs to.
    pub const fn run(&self) -> RunId {
        match &self.kind {
            RunEventKind::Started(event) => event.run,
            RunEventKind::Leased(event) => event.run,
            RunEventKind::Began(event) => event.run,
            RunEventKind::Saved(event) => event.run,
            RunEventKind::Returned(event) => event.run,
            RunEventKind::Failed(event) => event.run,
            RunEventKind::CancelAsked(event) => event.run,
            RunEventKind::Cancelled(event) => event.run,
            RunEventKind::Accepted(event) => event.run,
            RunEventKind::Rejected(event) => event.run,
            RunEventKind::Presumed(event) => event.run,
            RunEventKind::Renewed(event) => event.run,
        }
    }

    /// The physical Attempt named by this event, when it names one. A
    /// `Leased` event names the Attempt it creates: its own identity.
    pub fn attempt(&self) -> Option<AttemptId> {
        match &self.kind {
            RunEventKind::Started(_) | RunEventKind::CancelAsked(_) => None,
            RunEventKind::Leased(_) => self.id().ok().map(AttemptId::of_event),
            RunEventKind::Began(event) => Some(event.attempt),
            RunEventKind::Saved(event) => Some(event.attempt),
            RunEventKind::Returned(event) => Some(event.attempt),
            RunEventKind::Failed(event) => Some(event.attempt),
            RunEventKind::Cancelled(event) => event.attempt,
            RunEventKind::Accepted(event) => Some(event.attempt),
            RunEventKind::Rejected(event) => Some(event.attempt),
            RunEventKind::Presumed(event) => Some(event.attempt),
            RunEventKind::Renewed(event) => Some(event.attempt),
        }
    }

    pub fn validate(&self) -> Result<(), Invalid> {
        if self.predecessors.len() > MAX_RUN_EVENT_PREDECESSORS
            || self
                .predecessors
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return Err(Invalid::InvalidEvent("predecessors"));
        }
        match &self.kind {
            RunEventKind::Started(started) => {
                if !self.predecessors.is_empty()
                    || mechanics::ids::SpaceId::parse(started.space.as_str()).as_ref()
                        != Some(&started.space)
                    || WorldId::parse(started.world.as_str()).as_ref() != Some(&started.world)
                    || ActorId::parse(started.invoker.as_str()).as_ref() != Some(&started.invoker)
                    || DeviceId::parse(started.device.as_str()).as_ref() != Some(&started.device)
                    || !valid_schema(&started.spec)
                    || !valid_schema(&started.input)
                    || started.command_bytes == 0
                    || usize::try_from(started.command_bytes)
                        .map_or(true, |bytes| bytes > MAX_CMD_BYTES)
                    || started.command_chunks
                        != command_chunk_count(started.command_bytes)
                            .ok_or(Invalid::InvalidEvent("command chunks"))?
                    || !valid_content_geometry(&started.input_content, started.input_content_bytes)
                    || started.parent == Some(started.run)
                    || started.source == Some(started.run)
                    || derive_run_id(
                        &started.space,
                        &started.world,
                        &started.device,
                        started.request,
                        started.command,
                    ) != started.run
                {
                    return Err(Invalid::InvalidEvent("started"));
                }
                validate_resources(&started.resources, "start")?;
                started
                    .limits
                    .validate()
                    .map_err(|_| Invalid::InvalidEvent("limits"))?;
                started
                    .terminal
                    .validate()
                    .map_err(|_| Invalid::InvalidEvent("terminal"))?;
                if started.max_additional_input_bytes > MAX_CONTENT_LEN
                    || matches!(started.mode, Mode::Unary | Mode::Stream)
                        && (started.max_additional_input_bytes != 0
                            || started.terminal.max_live_input_bytes != 0)
                {
                    return Err(Invalid::InvalidEvent("mode"));
                }
            }
            RunEventKind::Leased(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.executor, &event.device)?;
                Try {
                    run: event.run,
                    build: event.build,
                    offer: event.offer.map(|id| OfferRef {
                        id,
                        station: event.station.clone(),
                        station_epoch: event.station_epoch,
                        epoch: event.offer_epoch,
                    }),
                    resources: event.resources.clone(),
                    enforcement: event.enforcement,
                    limits: event.limits,
                    lease: event.lease.clone(),
                    checkpoint: event.checkpoint.clone(),
                    continuation: event.continuation.clone(),
                }
                .validate()
                .map_err(|_| Invalid::InvalidEvent("leased"))?;
                if event.offer.is_none() != (event.offer_epoch == 0) {
                    return Err(Invalid::InvalidEvent("leased"));
                }
            }
            RunEventKind::Began(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.executor, &event.device)?;
            }
            RunEventKind::Saved(event) => {
                require_predecessors(&self.predecessors)?;
                if event.checkpoint.sequence == 0 {
                    return Err(Invalid::InvalidEvent("saved"));
                }
            }
            RunEventKind::Returned(event) => {
                require_predecessors(&self.predecessors)?;
                if !valid_schema(&event.output)
                    || usize::try_from(event.output_inline_bytes)
                        .map_or(true, |bytes| bytes > MAX_BODY_BYTES)
                    || !valid_content_geometry(&event.output_content, event.output_content_bytes)
                    || !canonical_content_refs(&event.evidence)
                {
                    return Err(Invalid::InvalidEvent("returned"));
                }
                validate_resources(&event.usage, "event")
                    .map_err(|_| Invalid::InvalidEvent("returned"))?;
            }
            RunEventKind::Failed(event) => {
                require_predecessors(&self.predecessors)?;
                if !canonical_content_refs(&event.evidence) {
                    return Err(Invalid::InvalidEvent("failed"));
                }
            }
            RunEventKind::CancelAsked(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.actor, &event.device)?;
            }
            RunEventKind::Cancelled(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.actor, &event.device)?;
            }
            RunEventKind::Accepted(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.actor, &event.device)?;
            }
            RunEventKind::Rejected(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.actor, &event.device)?;
            }
            RunEventKind::Presumed(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.presumer, &event.device)?;
            }
            RunEventKind::Renewed(event) => {
                require_predecessors(&self.predecessors)?;
                valid_actor_device(&event.executor, &event.device)?;
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes = postcard::to_stdvec(&(RUN_EVENT_VERSION, self))
            .map_err(|_| Invalid::InvalidEvent("encoding"))?;
        if bytes.len() > MAX_RUN_EVENT_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_RUN_EVENT_BYTES {
            return Err(Invalid::TooLarge);
        }
        let (version, event): (u8, Self) =
            postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        if version != RUN_EVENT_VERSION {
            return Err(Invalid::UnsupportedVersion(version));
        }
        event.validate()?;
        if event.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(event)
    }

    pub fn id(&self) -> Result<EventId, Invalid> {
        Ok(EventId(blake3::derive_key(
            RUN_EVENT_ID_CONTEXT,
            &self.encode()?,
        )))
    }
}

fn require_predecessors(predecessors: &[EventId]) -> Result<(), Invalid> {
    if predecessors.is_empty() {
        Err(Invalid::InvalidEvent("predecessors"))
    } else {
        Ok(())
    }
}

fn valid_actor_device(actor: &ActorId, device: &DeviceId) -> Result<(), Invalid> {
    if ActorId::parse(actor.as_str()).as_ref() != Some(actor)
        || DeviceId::parse(device.as_str()).as_ref() != Some(device)
    {
        return Err(Invalid::InvalidEvent("principal"));
    }
    Ok(())
}

fn valid_content_geometry(content: &[ContentRef], bytes: u64) -> bool {
    if content.len() > MAX_CONTENT_REFS_PER_BODY || (content.is_empty() != (bytes == 0)) {
        return false;
    }
    u64::try_from(content.len()).is_ok_and(|count| bytes <= MAX_CONTENT_LEN.saturating_mul(count))
}

fn canonical_content_refs(content: &[ContentRef]) -> bool {
    content.len() <= MAX_CONTENT_REFS_PER_BODY
        && !content
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left >= right))
}

/// Validate one complete Run event DAG and return every visible causal head.
///
/// Input order has no meaning. Every event must belong to one Run, exactly one
/// predecessor-free `Started` root must exist, and every other event must be
/// reachable from that root. Concurrent heads stay separate until a later
/// event explicitly names each of them as a predecessor.
pub fn run_event_heads(events: &[RunEvent]) -> Result<Vec<EventId>, Invalid> {
    if events.is_empty() {
        return Err(Invalid::InvalidEvent("history"));
    }

    let run = events
        .first()
        .ok_or(Invalid::InvalidEvent("history"))?
        .run();
    let mut by_id = BTreeMap::new();
    let mut started = None;
    for event in events {
        event.validate()?;
        if event.run() != run {
            return Err(Invalid::InvalidEvent("run"));
        }
        let id = event.id()?;
        if by_id.insert(id, event).is_some() {
            return Err(Invalid::InvalidEvent("duplicate event"));
        }
        if matches!(event.kind, RunEventKind::Started(_)) {
            if started.replace(id).is_some() {
                return Err(Invalid::InvalidEvent("started roots"));
            }
        }
    }
    let started = started.ok_or(Invalid::InvalidEvent("started root"))?;

    let mut referenced = BTreeSet::new();
    let mut remaining = BTreeMap::new();
    let mut children: BTreeMap<EventId, Vec<EventId>> = BTreeMap::new();
    for (id, event) in &by_id {
        remaining.insert(*id, event.predecessors.len());
        for predecessor in &event.predecessors {
            if !by_id.contains_key(predecessor) {
                return Err(Invalid::InvalidEvent("missing predecessor"));
            }
            referenced.insert(*predecessor);
            children.entry(*predecessor).or_default().push(*id);
        }
    }

    let mut reachable = BTreeSet::from([started]);
    let mut queue = VecDeque::from([started]);
    while let Some(parent) = queue.pop_front() {
        if let Some(successors) = children.get(&parent) {
            for successor in successors {
                let count = remaining
                    .get_mut(successor)
                    .ok_or(Invalid::InvalidEvent("history"))?;
                *count = count
                    .checked_sub(1)
                    .ok_or(Invalid::InvalidEvent("history"))?;
                if *count == 0 && reachable.insert(*successor) {
                    queue.push_back(*successor);
                }
            }
        }
    }
    if reachable.len() != by_id.len() {
        return Err(Invalid::InvalidEvent("unreachable event"));
    }

    Ok(by_id
        .keys()
        .filter(|id| !referenced.contains(id))
        .copied()
        .collect())
}

/// One attributed immutable event retained by a derived projection.
///
/// `event` is storage/event identity, not a new semantic identity for `T`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact<T> {
    pub event: EventId,
    pub predecessors: Vec<EventId>,
    pub value: T,
}

/// What one Attempt returned, addressed semantically by its [`AttemptId`].
///
/// This projection deliberately carries only Runtime-owned facts. The output
/// bytes remain opaque World payload and are reached through their separately
/// authorized content references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub event: EventId,
    pub predecessors: Vec<EventId>,
    pub attempt: AttemptId,
    pub output: SchemaRef,
    pub output_digest: [u8; 32],
    pub output_inline_bytes: u32,
    pub output_content: Vec<ContentRef>,
    pub output_content_bytes: u64,
    pub terminal: TerminalClass,
    pub usage: Vec<Resource>,
    pub evidence: Vec<ContentRef>,
    pub transcript: Option<ContentRef>,
}

impl Outcome {
    pub(crate) fn validate_with_spec(&self, spec: &Spec) -> Result<(), Invalid> {
        let inline = u64::from(self.output_inline_bytes);
        let content = u64::try_from(self.output_content.len())
            .map_err(|_| Invalid::InvalidEvent("returned"))?;
        if self.output != spec.output.schema
            || inline > u64::from(spec.output.max_inline_bytes)
            || content > u64::from(spec.output.max_content_refs)
            || self.output_content_bytes > spec.output.max_content_bytes
            || !valid_content_geometry(&self.output_content, self.output_content_bytes)
        {
            return Err(Invalid::InvalidEvent("outcome contract"));
        }
        Ok(())
    }
}

/// Derived view of one physical try by one Station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub run: RunId,
    pub id: AttemptId,
    pub leased_event: EventId,
    pub leased_predecessors: Vec<EventId>,
    pub station: StationKey,
    pub station_epoch: StationEpoch,
    pub executor: ActorId,
    pub device: DeviceId,
    pub build: BuildId,
    pub offer: Option<OfferId>,
    pub offer_epoch: u64,
    pub resources: Vec<Resource>,
    pub enforcement: Option<ContentRef>,
    pub limits: AttemptLimits,
    pub lease: Option<RoleLease>,
    pub checkpoint: Option<CheckpointRef>,
    pub continuation: Option<ContinuationInput>,
    pub began: Vec<Fact<Began>>,
    pub checkpoints: Vec<Fact<Saved>>,
    pub outcomes: Vec<Outcome>,
    pub failures: Vec<Fact<Failed>>,
    pub cancellations: Vec<Fact<Cancelled>>,
    /// Third-party suspicions this Attempt is dead. Non-terminal: their
    /// presence never resolves the Run, and an Outcome reconciles over them.
    pub presumptions: Vec<Fact<Presumed>>,
    /// The executor's own liveness refreshes.
    pub renewals: Vec<Fact<Renewed>>,
}

/// Derived view of one durable logical request.
///
/// There is intentionally no scalar lifecycle status. Concurrent Attempt and
/// acceptance facts remain separate collections, while `heads` exposes the
/// unresolved causal frontier exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: RunId,
    pub started: Started,
    pub heads: Vec<EventId>,
    pub attempts: Vec<Attempt>,
    pub cancel_asked: Vec<Fact<CancelAsked>>,
    pub cancellations: Vec<Fact<Cancelled>>,
    pub accepted: Vec<Fact<Accepted>>,
    pub rejected: Vec<Fact<Rejected>>,
}

/// A product-neutral request over durable Exec lifecycle state.
///
/// There is deliberately no `Start` arm. Creating work is a semantic World
/// action and enters Runtime only through [`crate::world::Effect::exec`]. These
/// operations either inspect protected lifecycle facts or request a transition
/// whose meaning Runtime owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkRequest {
    Inspect {
        world: WorldId,
        run: RunId,
    },
    Watch {
        world: WorldId,
        run: RunId,
        known_heads: Vec<EventId>,
    },
    Cancel {
        world: WorldId,
        run: RunId,
    },
    Retry {
        world: WorldId,
        run: RunId,
    },
    Resume {
        world: WorldId,
        run: RunId,
        checkpoint: ContentRef,
    },
}

impl WorkRequest {
    pub fn world(&self) -> &WorldId {
        match self {
            Self::Inspect { world, .. }
            | Self::Watch { world, .. }
            | Self::Cancel { world, .. }
            | Self::Retry { world, .. }
            | Self::Resume { world, .. } => world,
        }
    }

    pub const fn run(&self) -> RunId {
        match self {
            Self::Inspect { run, .. }
            | Self::Watch { run, .. }
            | Self::Cancel { run, .. }
            | Self::Retry { run, .. }
            | Self::Resume { run, .. } => *run,
        }
    }

    pub const fn is_command(&self) -> bool {
        matches!(
            self,
            Self::Cancel { .. } | Self::Retry { .. } | Self::Resume { .. }
        )
    }

    pub fn validate(&self) -> Result<(), Invalid> {
        if let Self::Watch { known_heads, .. } = self {
            if known_heads.len() > MAX_RUN_EVENT_PREDECESSORS
                || known_heads
                    .windows(2)
                    .any(|pair| matches!(pair, [left, right] if left >= right))
            {
                return Err(Invalid::InvalidEvent("work watch heads"));
            }
        }
        Ok(())
    }

    /// Stable receipt commitment for a generic Work operation.
    pub fn digest(&self) -> Result<[u8; 32], Invalid> {
        self.validate()?;
        let bytes = postcard::to_stdvec(self).map_err(|_| Invalid::NonCanonical)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lait.exec.work-request.v1");
        hasher.update(&bytes);
        Ok(*hasher.finalize().as_bytes())
    }
}

/// One checkpoint fact in a generic lifecycle projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCheckpoint {
    pub event: EventId,
    pub checkpoint: CheckpointRef,
}

/// One returned fact without the product payload it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkReturn {
    pub event: EventId,
    pub terminal: TerminalClass,
    /// Content identities of returned output. Bytes stay behind the World.
    #[serde(default)]
    pub output_content: Vec<ContentRef>,
    /// Host-authored terminal recording, separate from handler output.
    #[serde(default)]
    pub transcript: Option<ContentRef>,
}

/// One Attempt failure fact an actor can branch on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkFailure {
    pub event: EventId,
    pub class: FailureClass,
}

/// One acceptance or rejection fact. Concurrent choices remain separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkChoice {
    pub event: EventId,
    pub attempt: AttemptId,
}

/// Lifecycle-only projection of one physical Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkAttempt {
    pub attempt: AttemptId,
    pub station: StationKey,
    pub build: BuildId,
    #[serde(default)]
    pub offer: Option<OfferId>,
    /// The committed checkpoint this Attempt was admitted to resume from,
    /// when its lease bound one.
    #[serde(default)]
    pub checkpoint: Option<CheckpointRef>,
    pub began: Vec<EventId>,
    pub checkpoints: Vec<WorkCheckpoint>,
    pub returned: Vec<WorkReturn>,
    pub failed: Vec<WorkFailure>,
    pub cancelled: Vec<EventId>,
}

/// Product-neutral durable state for one Run.
///
/// Input and output payloads, digests, evidence, resources, and World Bodies
/// are intentionally absent. Applications get exact ids and lifecycle facts;
/// product meaning remains behind their own World actions and projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkState {
    pub world: WorldId,
    pub run: RunId,
    pub spec: SchemaRef,
    pub build: BuildId,
    /// The Actor/device that authored the durable Start. Execution services
    /// act as themselves; these facts make impersonation auditable.
    pub invoker: ActorId,
    pub device: DeviceId,
    pub heads: Vec<EventId>,
    pub event_count: u64,
    pub unresolved: bool,
    pub cancel_asked: Vec<EventId>,
    pub attempts: Vec<WorkAttempt>,
    pub accepted: Vec<WorkChoice>,
    pub rejected: Vec<WorkChoice>,
}

impl WorkState {
    fn from_run(run: &Run, event_count: u64) -> Self {
        let attempts = run
            .attempts
            .iter()
            .map(|attempt| WorkAttempt {
                attempt: attempt.id,
                station: attempt.station.clone(),
                build: attempt.build,
                offer: attempt.offer,
                checkpoint: attempt.checkpoint.clone(),
                began: attempt.began.iter().map(|fact| fact.event).collect(),
                checkpoints: attempt
                    .checkpoints
                    .iter()
                    .map(|fact| WorkCheckpoint {
                        event: fact.event,
                        checkpoint: fact.value.checkpoint.clone(),
                    })
                    .collect(),
                returned: attempt
                    .outcomes
                    .iter()
                    .map(|outcome| WorkReturn {
                        event: outcome.event,
                        terminal: outcome.terminal,
                        output_content: outcome.output_content.clone(),
                        transcript: outcome.transcript.clone(),
                    })
                    .collect(),
                failed: attempt
                    .failures
                    .iter()
                    .map(|fact| WorkFailure {
                        event: fact.event,
                        class: fact.value.class,
                    })
                    .collect(),
                cancelled: attempt
                    .cancellations
                    .iter()
                    .map(|fact| fact.event)
                    .collect(),
            })
            .collect();
        Self {
            world: run.started.world.clone(),
            run: run.id,
            spec: run.started.spec.clone(),
            build: run.started.build,
            invoker: run.started.invoker.clone(),
            device: run.started.device.clone(),
            heads: run.heads.clone(),
            event_count,
            unresolved: run.is_unresolved(),
            cancel_asked: run.cancel_asked.iter().map(|fact| fact.event).collect(),
            attempts,
            accepted: run
                .accepted
                .iter()
                .map(|fact| WorkChoice {
                    event: fact.event,
                    attempt: fact.value.attempt,
                })
                .collect(),
            rejected: run
                .rejected
                .iter()
                .map(|fact| WorkChoice {
                    event: fact.event,
                    attempt: fact.value.attempt,
                })
                .collect(),
        }
    }
}

/// Answer from the generic Work capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkReply {
    State(WorkState),
    Unchanged {
        world: WorldId,
        run: RunId,
        heads: Vec<EventId>,
    },
}

/// Why a generic Work request could not produce a lifecycle projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkRefusal {
    Invalid(Invalid),
    /// The exact protected Run/active image could not be obtained under the
    /// Station's key, integrity, or memory policy. This is never `NotFound`.
    BodyRead(crate::world::BodyReadFailure),
    /// The Run exists, but was written by a deliberately unsupported durable
    /// schema generation. It is quarantined from dispatch and not corruption.
    UnsupportedRunSchema {
        found: u32,
    },
    Session(crate::world::Failure),
    NotFound(RunId),
    Unsupported(&'static str),
}

impl std::fmt::Display for WorkRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for WorkRefusal {}

impl From<Invalid> for WorkRefusal {
    fn from(value: Invalid) -> Self {
        Self::Invalid(value)
    }
}

impl From<ReadFailure> for WorkRefusal {
    fn from(value: ReadFailure) -> Self {
        match value {
            ReadFailure::Invalid(invalid) => Self::Invalid(invalid),
            ReadFailure::Body(failure) => Self::BodyRead(failure),
            ReadFailure::UnsupportedRunSchema { found } => Self::UnsupportedRunSchema { found },
        }
    }
}

impl From<crate::world::Failure> for WorkRefusal {
    fn from(value: crate::world::Failure) -> Self {
        Self::Session(value)
    }
}

/// One complete committed Run that still requires local control.
///
/// This is a read projection, not a dispatch request. In particular, producing
/// it while incorporating remote material never calls a handler or launches an
/// Attempt. A local controller must explicitly consume the projection and
/// commit the next event before any executor may act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub run: Run,
    pub start: Start,
}

fn fact<T: Clone>(event: &RunEvent, value: &T) -> Result<Fact<T>, Invalid> {
    Ok(Fact {
        event: event.id()?,
        predecessors: event.predecessors.clone(),
        value: value.clone(),
    })
}

impl Attempt {
    fn from_leased(event: &RunEvent, leased: &Leased) -> Result<Self, Invalid> {
        Ok(Self {
            run: leased.run,
            id: AttemptId::of_event(event.id()?),
            leased_event: event.id()?,
            leased_predecessors: event.predecessors.clone(),
            station: leased.station.clone(),
            station_epoch: leased.station_epoch,
            executor: leased.executor.clone(),
            device: leased.device.clone(),
            build: leased.build,
            offer: leased.offer,
            offer_epoch: leased.offer_epoch,
            resources: leased.resources.clone(),
            enforcement: leased.enforcement,
            limits: leased.limits,
            lease: leased.lease.clone(),
            checkpoint: leased.checkpoint.clone(),
            continuation: leased.continuation.clone(),
            began: Vec::new(),
            checkpoints: Vec::new(),
            outcomes: Vec::new(),
            failures: Vec::new(),
            cancellations: Vec::new(),
            presumptions: Vec::new(),
            renewals: Vec::new(),
        })
    }

    fn sort_facts(&mut self) {
        self.began.sort_by_key(|fact| fact.event);
        self.checkpoints.sort_by_key(|fact| fact.event);
        self.outcomes.sort_by_key(|outcome| outcome.event);
        self.failures.sort_by_key(|fact| fact.event);
        self.cancellations.sort_by_key(|fact| fact.event);
        self.presumptions.sort_by_key(|fact| fact.event);
        self.renewals.sort_by_key(|fact| fact.event);
    }
}

impl Run {
    /// Rebuild a Run entirely from protected Body event material.
    ///
    /// Input list order is ignored. Structural DAG failures, duplicate Attempt
    /// identities, unbound Attempt facts, contradictory executor coordinates,
    /// repeated returns, and declared event/Attempt/checkpoint limit widening
    /// all reject rather than producing a partial projection.
    pub fn project(events: &[RunEvent]) -> Result<Self, Invalid> {
        let started_event = events
            .iter()
            .find(|event| event.as_started().is_some())
            .ok_or(Invalid::InvalidEvent("started root"))?;
        started_event.validate()?;
        let started = started_event
            .as_started()
            .cloned()
            .ok_or(Invalid::InvalidEvent("started root"))?;
        let event_count =
            u32::try_from(events.len()).map_err(|_| Invalid::InvalidEvent("events"))?;
        if event_count > started.limits.events {
            return Err(Invalid::InvalidEvent("events"));
        }
        let attempt_count = u32::try_from(
            events
                .iter()
                .filter(|event| matches!(event.kind, RunEventKind::Leased(_)))
                .count(),
        )
        .map_err(|_| Invalid::InvalidEvent("attempts"))?;
        if attempt_count > started.limits.attempts {
            return Err(Invalid::InvalidEvent("attempts"));
        }
        let saved_count = u32::try_from(
            events
                .iter()
                .filter(|event| matches!(event.kind, RunEventKind::Saved(_)))
                .count(),
        )
        .map_err(|_| Invalid::InvalidEvent("checkpoints"))?;
        if saved_count > started.limits.checkpoints {
            return Err(Invalid::InvalidEvent("checkpoints"));
        }
        let heads = run_event_heads(events)?;

        let mut attempts = BTreeMap::new();
        for event in events {
            if let RunEventKind::Leased(leased) = &event.kind {
                let attempt = Attempt::from_leased(event, leased)?;
                if attempts.insert(attempt.id, attempt).is_some() {
                    return Err(Invalid::InvalidEvent("duplicate attempt"));
                }
            }
        }
        let mut cancel_asked = Vec::new();
        let mut cancellations = Vec::new();
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for event in events {
            match &event.kind {
                RunEventKind::Started(_) | RunEventKind::Leased(_) => {}
                RunEventKind::Began(value) => {
                    let attempt = attempts
                        .get_mut(&value.attempt)
                        .ok_or(Invalid::InvalidEvent("unbound began"))?;
                    if value.executor != attempt.executor || value.device != attempt.device {
                        return Err(Invalid::InvalidEvent("began executor"));
                    }
                    attempt.began.push(fact(event, value)?);
                }
                RunEventKind::Saved(value) => {
                    let attempt = attempts
                        .get_mut(&value.attempt)
                        .ok_or(Invalid::InvalidEvent("unbound checkpoint"))?;
                    if value.checkpoint.build != attempt.build {
                        return Err(Invalid::InvalidEvent("checkpoint build"));
                    }
                    attempt.checkpoints.push(fact(event, value)?);
                }
                RunEventKind::Returned(value) => {
                    let attempt = attempts
                        .get_mut(&value.attempt)
                        .ok_or(Invalid::InvalidEvent("unbound outcome"))?;
                    attempt.outcomes.push(Outcome {
                        event: event.id()?,
                        predecessors: event.predecessors.clone(),
                        attempt: value.attempt,
                        output: value.output.clone(),
                        output_digest: value.output_digest,
                        output_inline_bytes: value.output_inline_bytes,
                        output_content: value.output_content.clone(),
                        output_content_bytes: value.output_content_bytes,
                        terminal: value.terminal,
                        usage: value.usage.clone(),
                        evidence: value.evidence.clone(),
                        transcript: value.transcript,
                    });
                }
                RunEventKind::Failed(value) => {
                    attempts
                        .get_mut(&value.attempt)
                        .ok_or(Invalid::InvalidEvent("unbound failure"))?
                        .failures
                        .push(fact(event, value)?);
                }
                RunEventKind::CancelAsked(value) => cancel_asked.push(fact(event, value)?),
                RunEventKind::Cancelled(value) => {
                    if let Some(attempt_id) = value.attempt {
                        attempts
                            .get_mut(&attempt_id)
                            .ok_or(Invalid::InvalidEvent("unbound cancellation"))?
                            .cancellations
                            .push(fact(event, value)?);
                    } else {
                        cancellations.push(fact(event, value)?);
                    }
                }
                RunEventKind::Accepted(value) => {
                    if !attempts.contains_key(&value.attempt) {
                        return Err(Invalid::InvalidEvent("unbound acceptance"));
                    }
                    accepted.push(fact(event, value)?);
                }
                RunEventKind::Rejected(value) => {
                    if !attempts.contains_key(&value.attempt) {
                        return Err(Invalid::InvalidEvent("unbound rejection"));
                    }
                    rejected.push(fact(event, value)?);
                }
                RunEventKind::Presumed(value) => {
                    let attempt = attempts
                        .get_mut(&value.attempt)
                        .ok_or(Invalid::InvalidEvent("unbound presumption"))?;
                    // A presumption is a third party's: the executor refreshes
                    // its own liveness with a Renewal, it does not presume
                    // itself dead.
                    if value.device == attempt.device {
                        return Err(Invalid::InvalidEvent("self presumption"));
                    }
                    attempt.presumptions.push(fact(event, value)?);
                }
                RunEventKind::Renewed(value) => {
                    let attempt = attempts
                        .get_mut(&value.attempt)
                        .ok_or(Invalid::InvalidEvent("unbound renewal"))?;
                    if value.executor != attempt.executor || value.device != attempt.device {
                        return Err(Invalid::InvalidEvent("renewal executor"));
                    }
                    attempt.renewals.push(fact(event, value)?);
                }
            }
        }

        let mut checkpoint_count = 0u32;
        for attempt in attempts.values_mut() {
            let count = u32::try_from(attempt.checkpoints.len())
                .map_err(|_| Invalid::InvalidEvent("checkpoints"))?;
            if count > attempt.limits.checkpoints || attempt.outcomes.len() > 1 {
                return Err(Invalid::InvalidEvent(if attempt.outcomes.len() > 1 {
                    "repeated return"
                } else {
                    "checkpoints"
                }));
            }
            checkpoint_count = checkpoint_count
                .checked_add(count)
                .ok_or(Invalid::InvalidEvent("checkpoints"))?;
            if let Some(continuation) = &attempt.continuation {
                let inline = u64::try_from(continuation.input.inline.len())
                    .map_err(|_| Invalid::InvalidEvent("continuation input"))?;
                let total = inline
                    .checked_add(continuation.input.content_bytes)
                    .ok_or(Invalid::InvalidEvent("continuation input"))?;
                if total == 0
                    || total > started.max_additional_input_bytes
                    || !matches!(started.mode, Mode::Interactive)
                {
                    return Err(Invalid::InvalidEvent("continuation input"));
                }
            }
            if attempt.outcomes.iter().any(|outcome| {
                outcome.transcript.is_some()
                    && !matches!(started.terminal.transcript, Transcript::OnReturn)
            }) {
                return Err(Invalid::InvalidEvent("transcript"));
            }
            attempt.sort_facts();
        }
        if checkpoint_count > started.limits.checkpoints {
            return Err(Invalid::InvalidEvent("checkpoints"));
        }
        for attempt_id in accepted
            .iter()
            .map(|fact| fact.value.attempt)
            .chain(rejected.iter().map(|fact| fact.value.attempt))
        {
            if attempts
                .get(&attempt_id)
                .is_none_or(|attempt| attempt.outcomes.len() != 1)
            {
                return Err(Invalid::InvalidEvent("choice without outcome"));
            }
        }

        cancel_asked.sort_by_key(|fact| fact.event);
        cancellations.sort_by_key(|fact| fact.event);
        accepted.sort_by_key(|fact| fact.event);
        rejected.sort_by_key(|fact| fact.event);
        Ok(Self {
            id: started.run,
            started,
            heads,
            attempts: attempts.into_values().collect(),
            cancel_asked,
            cancellations,
            accepted,
            rejected,
        })
    }

    /// Whether the Run has no Run-level terminal fact.
    ///
    /// A returned or failed Attempt is deliberately not terminal for the Run:
    /// local control may still accept, reject, retry, or resume it. An
    /// Attempt-scoped cancellation likewise leaves the Run available for a
    /// later Attempt.
    pub fn is_unresolved(&self) -> bool {
        self.accepted.is_empty() && self.cancellations.is_empty()
    }
}

/// Read the sparse active directory in one committed generation and project
/// its complete unresolved Runs.
///
/// The scan is deliberately inert. It reads an immutable Replica snapshot and
/// returns projections; it has no executor, callback, or message-sending seam.
/// This lets a local controller resume work after a crash while remote
/// incorporation remains data-only.
///
/// Terminal history is not visited: Start atomically creates one deterministic
/// `exec-active` marker and Run-level terminal acceptance/rejection atomically
/// tombstones it. Every returned marker and referenced protected Run Body is
/// validated in full. Malformed or stale active truth fails closed rather than
/// being skipped and potentially dispatched from partial state.
pub(crate) fn scan_unresolved(
    snapshot: &impl ReservedBodyReader,
    world: &WorldId,
) -> Result<Vec<Unresolved>, ReadFailure> {
    let mut unresolved = Vec::new();
    let active_schema = SchemaId::parse(ACTIVE_RUN_BODY_SCHEMA)
        .ok_or(Invalid::InvalidEvent("active run schema"))?;
    let mut after = None;
    const ACTIVE_PAGE: usize = 4_096;
    loop {
        let page =
            snapshot.body_keys_page_with_schema(world, &active_schema, after.as_ref(), ACTIVE_PAGE);
        for key in &page {
            let binding = snapshot
                .binding(key)
                .ok_or(Invalid::InvalidEvent("active run binding"))?;
            if binding.schema != active_schema
                || binding.schema_version != ACTIVE_RUN_BODY_SCHEMA_VERSION
                || binding.encoding.as_str() != BODY_ENCODING
                || binding.mutation_model != MUTATION_ATOMIC
            {
                return Err(Invalid::InvalidEvent("active run binding").into());
            }
            let value = snapshot
                .read_atomic(key)?
                .ok_or(Invalid::InvalidEvent("active run body"))?;
            let raw: [u8; 16] = value
                .as_ref()
                .try_into()
                .map_err(|_| Invalid::InvalidEvent("active run body"))?;
            let id = RunId::from_bytes(raw);
            if active_run_body_key(world, id) != *key {
                return Err(Invalid::InvalidEvent("active run identity").into());
            }
            let committed = match read_committed_run(snapshot, world, id) {
                // Generation 1 Runs cannot be losslessly re-identified after
                // the continuation/transcript schema cutover. Keep their
                // active marker as inert historical truth and allow newer
                // Runs to dispatch; direct inspection remains a typed refusal.
                Err(ReadFailure::UnsupportedRunSchema { found })
                    if found < RUN_BODY_SCHEMA_VERSION =>
                {
                    continue;
                }
                result => result?,
            };
            let Some((run, start, _)) = committed else {
                return Err(Invalid::InvalidEvent("active run target").into());
            };
            if !run.is_unresolved() {
                return Err(Invalid::InvalidEvent("terminal active run").into());
            }
            unresolved.push(Unresolved { run, start });
        }
        after = page.last().cloned();
        if page.len() < ACTIVE_PAGE {
            break;
        }
    }
    unresolved.sort_by_key(|candidate| candidate.run.id);
    Ok(unresolved)
}

/// Re-project one exact Run from its protected committed Body.
///
/// The event count is returned for a caller that will append a Runtime-owned
/// event at this same pinned generation. Absence is distinct from malformed
/// protected truth; malformed material always fails closed.
pub(crate) fn read_committed_run(
    snapshot: &impl ReservedBodyReader,
    world: &WorldId,
    id: RunId,
) -> Result<Option<(Run, Start, u64)>, ReadFailure> {
    let key = replica::body::BodyKey {
        world: world.clone(),
        body: BodyId::from_bytes(id.as_bytes()),
    };
    let Some(binding) = snapshot.binding(&key) else {
        return Ok(None);
    };
    if binding.schema.as_str() != RUN_BODY_SCHEMA
        || binding.encoding.as_str() != BODY_ENCODING
        || binding.mutation_model != MUTATION_COLLABORATIVE
    {
        return Err(Invalid::InvalidEvent("run binding").into());
    }
    if binding.schema_version != RUN_BODY_SCHEMA_VERSION {
        return Err(ReadFailure::UnsupportedRunSchema {
            found: binding.schema_version,
        });
    }
    let view = snapshot
        .read_collaborative(&key)?
        .ok_or(Invalid::InvalidEvent("run body"))?;
    let values = view
        .lists
        .get(RUN_EVENTS_PATH)
        .ok_or(Invalid::InvalidEvent("run events"))?;
    let event_count = u64::try_from(values.len()).map_err(|_| Invalid::InvalidEvent("events"))?;
    let events = values
        .iter()
        .map(|element| RunEvent::decode_canonical(&element.value))
        .collect::<Result<Vec<_>, _>>()?;
    let run = Run::project(&events)?;
    if run.id != id || run.started.world != *world || key.body.as_bytes() != run.id.as_bytes() {
        return Err(Invalid::InvalidEvent("run body identity").into());
    }
    let start = start_from_body(&view, &run.started)?;
    Ok(Some((run, start, event_count)))
}

pub(crate) fn work_state(
    snapshot: &impl ReservedBodyReader,
    world: &WorldId,
    run: RunId,
) -> Result<Option<WorkState>, ReadFailure> {
    Ok(read_committed_run(snapshot, world, run)?
        .map(|(run, _, event_count)| WorkState::from_run(&run, event_count)))
}

/// Derive the narrow World-facing Outcome facade from one committed snapshot.
pub(crate) fn outcome_facts(
    snapshot: &impl ReservedBodyReader,
    world: &WorldId,
    run: RunId,
    attempt: AttemptId,
) -> Result<Option<crate::world::OutcomeFacts>, ReadFailure> {
    let Some((projection, _, _)) = read_committed_run(snapshot, world, run)? else {
        return Ok(None);
    };
    let Some(attempt) = projection
        .attempts
        .iter()
        .find(|candidate| candidate.id == attempt)
    else {
        return Ok(None);
    };
    let [outcome] = attempt.outcomes.as_slice() else {
        return Ok(None);
    };
    Ok(Some(crate::world::OutcomeFacts {
        run,
        attempt: attempt.id,
        spec: projection.started.spec,
        build: attempt.build,
        station: attempt.station.clone(),
        terminal: outcome.terminal,
        output: outcome.output.clone(),
        output_digest: outcome.output_digest,
        output_inline_bytes: outcome.output_inline_bytes,
        output_content: outcome.output_content.clone(),
        output_content_bytes: outcome.output_content_bytes,
        returned_exactly_once: true,
    }))
}

fn start_from_body(view: &fabric::CollaborativeView, started: &Started) -> Result<Start, Invalid> {
    let chunks = view
        .maps
        .get(RUN_COMMAND_PATH)
        .ok_or(Invalid::InvalidEvent("run command"))?;
    let chunk_count = usize::try_from(started.command_chunks)
        .map_err(|_| Invalid::InvalidEvent("run command"))?;
    if chunks.len() != chunk_count {
        return Err(Invalid::InvalidEvent("run command"));
    }
    let command_len =
        usize::try_from(started.command_bytes).map_err(|_| Invalid::InvalidEvent("run command"))?;
    let mut bytes = Vec::with_capacity(command_len);
    for index in 0..chunk_count {
        let key = format!("{index:08x}");
        let chunk = chunks
            .get(&key)
            .ok_or(Invalid::InvalidEvent("run command"))?;
        let remaining = command_len
            .checked_sub(bytes.len())
            .ok_or(Invalid::InvalidEvent("run command"))?;
        let expected = remaining.min(MAX_RUN_COMMAND_CHUNK_BYTES);
        if chunk.len() != expected {
            return Err(Invalid::InvalidEvent("run command"));
        }
        bytes.extend_from_slice(chunk);
    }
    if bytes.len() != command_len {
        return Err(Invalid::InvalidEvent("run command"));
    }
    let command = Cmd::decode_canonical(&bytes)?;
    if command.digest()? != started.command_digest {
        return Err(Invalid::InvalidEvent("run command digest"));
    }
    let Cmd::Start(start) = command else {
        return Err(Invalid::InvalidEvent("run command kind"));
    };
    if start.spec != started.spec
        || start.build != started.build
        || start.input.content != started.input_content
        || start.input.content_bytes != started.input_content_bytes
        || start.resources != started.resources
        || start.limits != started.limits
        || start.parent != started.parent
        || start.source != started.source
        || start.service != started.service
        || start.query_grants_digest()? != started.query_grants_digest
    {
        return Err(Invalid::InvalidEvent("run command coordinates"));
    }
    Ok(start)
}

/// Derive one stable Run identity from the complete persistent-idempotency
/// scope and the command's ordinal within the World effect.
#[allow(
    clippy::indexing_slicing,
    reason = "BLAKE3 output is a fixed 32-byte array and RunId deliberately retains its first 16 bytes"
)]
pub fn derive_run_id(
    space: &mechanics::ids::SpaceId,
    world: &WorldId,
    device: &DeviceId,
    request: [u8; 16],
    command: u32,
) -> RunId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RUN_ID_DOMAIN);
    for field in [
        space.as_str().as_bytes(),
        world.as_str().as_bytes(),
        device.as_str().as_bytes(),
    ] {
        hasher.update(&u32::try_from(field.len()).unwrap_or(u32::MAX).to_be_bytes());
        hasher.update(field);
    }
    hasher.update(&request);
    hasher.update(&command.to_be_bytes());
    let digest = hasher.finalize();
    let mut run = [0u8; 16];
    run.copy_from_slice(&digest.as_bytes()[..16]);
    RunId::from_bytes(run)
}

/// Derive the reserved Body identity that holds one published Build envelope.
#[allow(
    clippy::indexing_slicing,
    reason = "BLAKE3 output is a fixed 32-byte array and BodyId is the first 16 bytes"
)]
pub fn derive_build_body_id(id: BuildId) -> BodyId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(BUILD_BODY_ID_DOMAIN);
    hasher.update(&id.as_bytes());
    let digest = hasher.finalize();
    let mut body = [0u8; 16];
    body.copy_from_slice(&digest.as_bytes()[..16]);
    BodyId::from_bytes(body)
}

fn command_chunk_count(command_bytes: u32) -> Option<u32> {
    let bytes = usize::try_from(command_bytes).ok()?;
    u32::try_from(bytes.div_ceil(MAX_RUN_COMMAND_CHUNK_BYTES)).ok()
}

/// One exact Build this Offer claims to serve.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OfferedBuild {
    pub id: BuildId,
    pub spec: SchemaRef,
}

/// Whether a Station claims it can accept new Attempts now.
///
/// This is news, not a rank. Runtime may drop an impossible Offer; it must not
/// order Offers by this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Availability {
    Ready,
    Busy,
    Draining,
}

/// Borrowed identity-bearing portion of [`Offer`]. The field order is the
/// generation-1 OfferId grammar and must not be changed in place.
#[derive(Serialize)]
struct OfferMaterial<'a> {
    space: &'a mechanics::ids::SpaceId,
    station: &'a StationKey,
    station_epoch: StationEpoch,
    actor: &'a ActorId,
    device: &'a DeviceId,
    world: &'a WorldId,
    world_build: &'a [u8; 32],
    builds: &'a [OfferedBuild],
    resources: &'a [Resource],
    backend: &'a SchemaId,
    enforcement: Enforcement,
    resident: &'a [ContentRef],
    availability: Availability,
    epoch: u64,
    expiry: u64,
}

/// Signed, lossy, expiring news that a Station is willing to run exact Builds.
///
/// An Offer confers no authority and reserves no Run. Publisher and signature
/// attest those exact bytes and sit outside [`OfferId`]. The envelope reveals
/// no local path and no Space catalog: resident hints are ContentRefs only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub id: OfferId,
    pub space: mechanics::ids::SpaceId,
    pub station: StationKey,
    pub station_epoch: StationEpoch,
    pub actor: ActorId,
    pub device: DeviceId,
    pub world: WorldId,
    pub world_build: [u8; 32],
    pub builds: Vec<OfferedBuild>,
    pub resources: Vec<Resource>,
    pub backend: SchemaId,
    pub enforcement: Enforcement,
    pub resident: Vec<ContentRef>,
    pub availability: Availability,
    pub epoch: u64,
    pub expiry: u64,
    pub publisher: ActorId,
    pub signature: Signature,
}

impl Offer {
    fn material(&self) -> OfferMaterial<'_> {
        OfferMaterial {
            space: &self.space,
            station: &self.station,
            station_epoch: self.station_epoch,
            actor: &self.actor,
            device: &self.device,
            world: &self.world,
            world_build: &self.world_build,
            builds: &self.builds,
            resources: &self.resources,
            backend: &self.backend,
            enforcement: self.enforcement,
            resident: &self.resident,
            availability: self.availability,
            epoch: self.epoch,
            expiry: self.expiry,
        }
    }

    fn validate_material(&self) -> Result<(), Invalid> {
        if mechanics::ids::SpaceId::parse(self.space.as_str()).as_ref() != Some(&self.space) {
            return Err(Invalid::InvalidOffer("space"));
        }
        if self.station_epoch == StationEpoch::ZERO {
            return Err(Invalid::InvalidOffer("station"));
        }
        if ActorId::parse(self.actor.as_str()).as_ref() != Some(&self.actor) {
            return Err(Invalid::InvalidOffer("actor"));
        }
        if DeviceId::parse(self.device.as_str()).as_ref() != Some(&self.device) {
            return Err(Invalid::InvalidOffer("device"));
        }
        if WorldId::parse(self.world.as_str()).as_ref() != Some(&self.world) {
            return Err(Invalid::InvalidOffer("world"));
        }
        if self.builds.is_empty()
            || self.builds.len() > MAX_BUILDS_PER_OFFER
            || self
                .builds
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
            || self.builds.iter().any(|build| !valid_schema(&build.spec))
        {
            return Err(Invalid::InvalidOffer("builds"));
        }
        if self.resources.len() > MAX_RESOURCES_PER_INTENT
            || self.resources.iter().any(|resource| {
                SchemaId::parse(resource.name.as_str()).as_ref() != Some(&resource.name)
                    || !resource.name.as_str().contains('.')
                    || resource.amount == 0
                    || resource.amount == u64::MAX
            })
            || self
                .resources
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left.name >= right.name))
        {
            return Err(Invalid::InvalidOffer("resources"));
        }
        if SchemaId::parse(self.backend.as_str()).as_ref() != Some(&self.backend) {
            return Err(Invalid::InvalidOffer("backend"));
        }
        if self.resident.len() > MAX_RESIDENT_HINTS_PER_OFFER
            || self
                .resident
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return Err(Invalid::InvalidOffer("resident"));
        }
        if self.epoch == 0 {
            return Err(Invalid::InvalidOffer("epoch"));
        }
        if self.expiry == 0 || self.expiry == u64::MAX {
            return Err(Invalid::InvalidOffer("expiry"));
        }
        Ok(())
    }

    fn material_bytes(&self) -> Result<Vec<u8>, Invalid> {
        self.validate_material()?;
        postcard::to_stdvec(&(OFFER_VERSION, self.material()))
            .map_err(|_| Invalid::InvalidOffer("encoding"))
    }

    /// Derive the stable identity of this Offer's news. Publisher and
    /// signature do not participate.
    #[allow(
        clippy::indexing_slicing,
        reason = "BLAKE3 output is a fixed 32-byte array and OfferId deliberately retains its first 16 bytes"
    )]
    pub fn derived_id(&self) -> Result<OfferId, Invalid> {
        let material = self.material_bytes()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(OFFER_ID_DOMAIN);
        hasher.update(&material);
        let digest = hasher.finalize();
        let mut id = [0u8; 16];
        id.copy_from_slice(&digest.as_bytes()[..16]);
        Ok(OfferId::from_bytes(id))
    }

    fn signature_preimage(&self) -> Result<Vec<u8>, Invalid> {
        let body = postcard::to_stdvec(&(
            OFFER_VERSION,
            self.id,
            self.material(),
            &self.publisher,
            &self.signature.signer,
            self.signature.algorithm,
        ))
        .map_err(|_| Invalid::InvalidOffer("signature encoding"))?;
        let mut preimage = Vec::with_capacity(
            2usize
                .saturating_add(OFFER_SIGNATURE_DOMAIN.len())
                .saturating_add(4)
                .saturating_add(body.len()),
        );
        let domain_len = u16::try_from(OFFER_SIGNATURE_DOMAIN.len())
            .map_err(|_| Invalid::InvalidOffer("signature domain"))?;
        let body_len =
            u32::try_from(body.len()).map_err(|_| Invalid::InvalidOffer("signature encoding"))?;
        preimage.extend_from_slice(&domain_len.to_be_bytes());
        preimage.extend_from_slice(OFFER_SIGNATURE_DOMAIN);
        preimage.extend_from_slice(&body_len.to_be_bytes());
        preimage.extend_from_slice(&body);
        Ok(preimage)
    }

    /// Replace the identity and signature placeholders with a canonical news
    /// envelope signed by `device_seed`.
    ///
    /// The publisher must already be set. Mechanics later proves that the
    /// derived signer belonged to that Actor at the referenced frontier.
    pub fn sign(mut self, device_seed: &[u8; 32]) -> Result<Self, Invalid> {
        if ActorId::parse(self.publisher.as_str()).as_ref() != Some(&self.publisher) {
            return Err(Invalid::InvalidOffer("publisher"));
        }
        self.id = self.derived_id()?;
        self.signature.signer = mechanics::actor::device_from_seed(device_seed);
        self.signature.algorithm = SIGNATURE_ED25519;
        self.signature.bytes = [0; 64];
        let preimage = self.signature_preimage()?;
        self.signature.bytes = mechanics::actor::sign_detached(device_seed, &preimage);
        self.validate()?;
        Ok(self)
    }

    /// Validate the complete self-contained Offer envelope.
    ///
    /// This proves byte shape, content identity, and the device signature. It
    /// does not consult a clock, so an expired Offer still validates as well-
    /// formed news. [`Offer::usable_at`] applies expiry. It also cannot prove
    /// that the signer represented `publisher` or that publication satisfied
    /// the Spec's Offer demand.
    pub fn validate(&self) -> Result<(), Invalid> {
        self.validate_material()?;
        if ActorId::parse(self.publisher.as_str()).as_ref() != Some(&self.publisher) {
            return Err(Invalid::InvalidOffer("publisher"));
        }
        if DeviceId::parse(self.signature.signer.as_str()).as_ref() != Some(&self.signature.signer)
        {
            return Err(Invalid::InvalidOffer("signer"));
        }
        if self.signature.algorithm != SIGNATURE_ED25519 {
            return Err(Invalid::UnsupportedSignatureAlgorithm(
                self.signature.algorithm,
            ));
        }
        if self.id != self.derived_id()? {
            return Err(Invalid::OfferIdMismatch);
        }
        let public_key = self
            .signature
            .signer
            .key_bytes()
            .ok_or(Invalid::InvalidOffer("signer"))?;
        let preimage = self.signature_preimage()?;
        if !mechanics::actor::verify_detached(&public_key, &preimage, &self.signature.bytes) {
            return Err(Invalid::BadOfferSignature);
        }
        Ok(())
    }

    /// Prove the news is still live at `now` unix milliseconds.
    pub fn usable_at(&self, now: u64) -> Result<(), Invalid> {
        self.validate()?;
        if now >= self.expiry {
            return Err(Invalid::InvalidOffer("expiry"));
        }
        Ok(())
    }

    /// Coordinates a [`Try`] may cite as optional evidence.
    pub fn reference(&self) -> OfferRef {
        OfferRef {
            id: self.id,
            station: self.station.clone(),
            station_epoch: self.station_epoch,
            epoch: self.epoch,
        }
    }

    /// Whether this news claims the named Build.
    pub fn covers(&self, build: BuildId) -> bool {
        self.builds.iter().any(|offered| offered.id == build)
    }

    /// Encode this complete Offer envelope to canonical standalone bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let bytes = postcard::to_stdvec(&(OFFER_VERSION, self))
            .map_err(|_| Invalid::InvalidOffer("encoding"))?;
        if bytes.len() > MAX_OFFER_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    /// Decode and validate exact canonical standalone Offer bytes.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_OFFER_BYTES {
            return Err(Invalid::TooLarge);
        }
        let (version, offer): (u8, Self) =
            postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        if version != OFFER_VERSION {
            return Err(Invalid::UnsupportedVersion(version));
        }
        offer.validate()?;
        if offer.encode()? != bytes {
            return Err(Invalid::NonCanonical);
        }
        Ok(offer)
    }
}

/// One nonce-bound readiness challenge against a live Offer.
///
/// Answering it does not prove intent and does not reserve a Run. A Station
/// that answers and then stalls cannot block another authorized Attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub offer: OfferId,
    pub nonce: [u8; 16],
    pub station: StationKey,
    pub station_epoch: StationEpoch,
    pub issued_at: u64,
    pub expiry: u64,
}

impl Challenge {
    /// Validate shape without consulting a clock.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.nonce == [0; 16] {
            return Err(Invalid::InvalidChallenge("nonce"));
        }
        if self.station_epoch == StationEpoch::ZERO {
            return Err(Invalid::InvalidChallenge("station"));
        }
        if self.issued_at == 0 || self.expiry == 0 || self.expiry == u64::MAX {
            return Err(Invalid::InvalidChallenge("expiry"));
        }
        if self.expiry <= self.issued_at {
            return Err(Invalid::InvalidChallenge("expiry"));
        }
        Ok(())
    }

    /// Prove the challenge is still live at `now` unix milliseconds.
    pub fn usable_at(&self, now: u64) -> Result<(), Invalid> {
        self.validate()?;
        if now < self.issued_at || now >= self.expiry {
            return Err(Invalid::InvalidChallenge("expiry"));
        }
        Ok(())
    }

    fn signature_body(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        postcard::to_stdvec(&(
            CHALLENGE_VERSION,
            self.offer,
            self.nonce,
            &self.station,
            self.station_epoch,
            self.issued_at,
            self.expiry,
        ))
        .map_err(|_| Invalid::InvalidChallenge("encoding"))
    }
}

/// A Station's signed answer to one [`Challenge`].
///
/// Readiness is not intent. The answer expires with its challenge and never
/// owns the Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ready {
    pub offer: OfferId,
    pub nonce: [u8; 16],
    pub signature: Signature,
}

impl Ready {
    fn signature_preimage(
        challenge: &Challenge,
        signer: &DeviceId,
        algorithm: u8,
    ) -> Result<Vec<u8>, Invalid> {
        let body = challenge.signature_body()?;
        let envelope = postcard::to_stdvec(&(
            CHALLENGE_VERSION,
            challenge.offer,
            challenge.nonce,
            signer,
            algorithm,
            body,
        ))
        .map_err(|_| Invalid::InvalidReady("signature encoding"))?;
        let mut preimage = Vec::with_capacity(
            2usize
                .saturating_add(READY_SIGNATURE_DOMAIN.len())
                .saturating_add(4)
                .saturating_add(envelope.len()),
        );
        let domain_len = u16::try_from(READY_SIGNATURE_DOMAIN.len())
            .map_err(|_| Invalid::InvalidReady("signature domain"))?;
        let body_len = u32::try_from(envelope.len())
            .map_err(|_| Invalid::InvalidReady("signature encoding"))?;
        preimage.extend_from_slice(&domain_len.to_be_bytes());
        preimage.extend_from_slice(READY_SIGNATURE_DOMAIN);
        preimage.extend_from_slice(&body_len.to_be_bytes());
        preimage.extend_from_slice(&envelope);
        Ok(preimage)
    }

    /// Sign an answer to `challenge` with `device_seed`.
    pub fn sign(challenge: &Challenge, device_seed: &[u8; 32]) -> Result<Self, Invalid> {
        challenge.validate()?;
        let signer = mechanics::actor::device_from_seed(device_seed);
        let mut ready = Self {
            offer: challenge.offer,
            nonce: challenge.nonce,
            signature: Signature {
                signer: signer.clone(),
                algorithm: SIGNATURE_ED25519,
                bytes: [0; 64],
            },
        };
        let preimage = Self::signature_preimage(challenge, &signer, SIGNATURE_ED25519)?;
        ready.signature.bytes = mechanics::actor::sign_detached(device_seed, &preimage);
        ready.validate_for(challenge)?;
        Ok(ready)
    }

    /// Prove this answer is the signed response to `challenge`.
    pub fn validate_for(&self, challenge: &Challenge) -> Result<(), Invalid> {
        challenge.validate()?;
        if self.offer != challenge.offer || self.nonce != challenge.nonce {
            return Err(Invalid::InvalidReady("challenge"));
        }
        if DeviceId::parse(self.signature.signer.as_str()).as_ref() != Some(&self.signature.signer)
        {
            return Err(Invalid::InvalidReady("signer"));
        }
        if self.signature.algorithm != SIGNATURE_ED25519 {
            return Err(Invalid::UnsupportedSignatureAlgorithm(
                self.signature.algorithm,
            ));
        }
        let public_key = self
            .signature
            .signer
            .key_bytes()
            .ok_or(Invalid::InvalidReady("signer"))?;
        let preimage =
            Self::signature_preimage(challenge, &self.signature.signer, self.signature.algorithm)?;
        if !mechanics::actor::verify_detached(&public_key, &preimage, &self.signature.bytes) {
            return Err(Invalid::BadReadySignature);
        }
        Ok(())
    }
}

/// Exact signed-Offer and Station-activation coordinates selected by [`Try`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferRef {
    pub id: OfferId,
    pub station: StationKey,
    pub station_epoch: StationEpoch,
    pub epoch: u64,
}

/// An existing committed Service Role lease selected by [`Try`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleLease {
    pub service: ServiceId,
    pub role: SchemaId,
    pub lease: LeaseId,
    pub epoch: u64,
}

/// Exact saved-state coordinates selected by [`Try`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRef {
    pub content: ContentRef,
    pub build: BuildId,
    pub sequence: u32,
}

/// Additional input committed by an explicit continuation Try.
///
/// The manifest root is the immutable interpretation coordinate at which the
/// input was admitted. Live keystrokes never enter this value and never spend
/// its durable byte budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationInput {
    pub manifest_root: [u8; 32],
    pub input: Input,
}

/// Finite ceilings for one physical Attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptLimits {
    pub events: u32,
    pub checkpoints: u32,
    pub child_runs: u32,
    pub progress_bytes: u64,
    pub checkpoint_bytes: u64,
    pub wall_millis: u64,
}

/// Authorized intent to create one bounded physical Attempt.
///
/// Runtime derives the Attempt id. Readiness and the referenced Offer remain
/// evidence for admission, never ownership of the Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Try {
    pub run: RunId,
    pub build: BuildId,
    /// Signed published news, when E4 has one. Local and explicit-Station
    /// Tries omit it.
    pub offer: Option<OfferRef>,
    pub resources: Vec<Resource>,
    /// Isolation evidence, when a backend produced some. Advisory is absence.
    pub enforcement: Option<ContentRef>,
    pub limits: AttemptLimits,
    pub lease: Option<RoleLease>,
    pub checkpoint: Option<CheckpointRef>,
    pub continuation: Option<ContinuationInput>,
}

fn validate_resources(resources: &[Resource], owner: &'static str) -> Result<(), Invalid> {
    if resources.len() > MAX_RESOURCES_PER_INTENT {
        return Err(if owner == "start" {
            Invalid::InvalidStart("resources")
        } else {
            Invalid::InvalidTry("resources")
        });
    }
    let invalid = resources.iter().any(|resource| {
        SchemaId::parse(resource.name.as_str()).as_ref() != Some(&resource.name)
            || !resource.name.as_str().contains('.')
            || resource.amount == 0
            || resource.amount == u64::MAX
    }) || resources
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left.name >= right.name));
    if invalid {
        return Err(if owner == "start" {
            Invalid::InvalidStart("resources")
        } else {
            Invalid::InvalidTry("resources")
        });
    }
    Ok(())
}

impl Input {
    pub(crate) fn validate(&self) -> Result<(), Invalid> {
        if self.inline.len() > MAX_BODY_BYTES
            || self.content.len() > MAX_CONTENT_REFS_PER_BODY
            || (self.content.is_empty() != (self.content_bytes == 0))
        {
            return Err(Invalid::InvalidStart("input"));
        }
        let count =
            u64::try_from(self.content.len()).map_err(|_| Invalid::InvalidStart("input"))?;
        if self.content_bytes > MAX_CONTENT_LEN.saturating_mul(count) {
            return Err(Invalid::InvalidStart("input"));
        }
        Ok(())
    }

    pub(crate) fn validate_with(&self, payload: &PayloadSpec) -> Result<(), Invalid> {
        self.validate()?;
        let inline =
            u64::try_from(self.inline.len()).map_err(|_| Invalid::InvalidStart("input"))?;
        let content =
            u64::try_from(self.content.len()).map_err(|_| Invalid::InvalidStart("input"))?;
        if inline > u64::from(payload.max_inline_bytes)
            || content > u64::from(payload.max_content_refs)
            || self.content_bytes > payload.max_content_bytes
        {
            return Err(Invalid::InvalidStart("input widening"));
        }
        Ok(())
    }
}

impl Limits {
    fn is_within(self, parent: Self) -> bool {
        self.attempts <= parent.attempts
            && self.events <= parent.events
            && self.checkpoints <= parent.checkpoints
            && self.child_runs <= parent.child_runs
            && self.progress_bytes <= parent.progress_bytes
            && self.checkpoint_bytes <= parent.checkpoint_bytes
            && self.wall_millis <= parent.wall_millis
    }
}

impl AttemptLimits {
    fn validate(self) -> Result<(), Invalid> {
        if self.events == 0
            || self.events == u32::MAX
            || self.checkpoints == u32::MAX
            || self.child_runs == u32::MAX
            || self.progress_bytes == u64::MAX
            || self.checkpoint_bytes == u64::MAX
            || self.wall_millis == 0
            || self.wall_millis == u64::MAX
            || (self.checkpoints == 0) != (self.checkpoint_bytes == 0)
        {
            return Err(Invalid::InvalidTry("limits"));
        }
        Ok(())
    }

    fn is_within(self, run: Limits) -> bool {
        self.events <= run.events
            && self.checkpoints <= run.checkpoints
            && self.child_runs <= run.child_runs
            && self.progress_bytes <= run.progress_bytes
            && self.checkpoint_bytes <= run.checkpoint_bytes
            && self.wall_millis <= run.wall_millis
    }
}

impl Start {
    /// Validate semantic Start intent without consulting ambient Session state.
    pub fn validate(&self) -> Result<(), Invalid> {
        if !valid_schema(&self.spec) {
            return Err(Invalid::InvalidStart("spec"));
        }
        self.input.validate()?;
        validate_resources(&self.resources, "start")?;
        self.limits
            .validate()
            .map_err(|_| Invalid::InvalidStart("limits"))?;
        if self.queries.len() > MAX_QUERIES_PER_SPEC
            || self
                .queries
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left.parent >= right.parent))
        {
            return Err(Invalid::InvalidStart("queries"));
        }
        for query in &self.queries {
            query
                .grant
                .validate()
                .map_err(|_| Invalid::InvalidStart("query"))?;
        }
        Ok(())
    }

    /// Prove this intent is within one exact World-declared Spec.
    ///
    /// A Start always carries an exact Build id. Build publication is durable
    /// identity, not a dispatch gate: this descriptor-only check does not
    /// claim the selected Build has been published. Dispatch selects the
    /// exact package handler for the pinned Build.
    pub fn validate_with_spec(&self, spec: &Spec) -> Result<(), Invalid> {
        self.validate()?;
        spec.validate()?;
        if self.spec.name != spec.name || self.spec.version != spec.version {
            return Err(Invalid::InvalidStart("selection"));
        }
        self.input.validate_with(&spec.input)?;
        if !self.limits.is_within(spec.limits) {
            return Err(Invalid::InvalidStart("limits widening"));
        }
        if self.service.is_some() && spec.service.is_none() {
            return Err(Invalid::InvalidStart("service"));
        }
        for query in &self.queries {
            let mut declared = None;
            for parent in &spec.queries {
                if parent
                    .digest()
                    .map_err(|_| Invalid::InvalidStart("query parent"))?
                    == query.parent
                {
                    declared = Some(parent);
                    break;
                }
            }
            let parent = declared.ok_or(Invalid::InvalidStart("query parent"))?;
            query
                .grant
                .validate_within(parent)
                .map_err(|_| Invalid::InvalidStart("query widening"))?;
        }
        Ok(())
    }

    /// Prove this intent is within one pinned Spec and exact selected Build.
    ///
    /// Session admission still checks the World, authority, resources, Service
    /// state, and active implementation at its pinned coordinates.
    pub fn validate_with(&self, spec: &Spec, build: &Build) -> Result<(), Invalid> {
        self.validate_with_spec(spec)?;
        build.validate()?;
        if self.build != build.id || self.spec != build.spec {
            return Err(Invalid::InvalidStart("selection"));
        }
        Ok(())
    }

    /// Commit to the exact input contract and initial input material.
    pub fn input_digest(&self, spec: &Spec) -> Result<[u8; 32], Invalid> {
        self.validate_with_spec(spec)?;
        let bytes = postcard::to_stdvec(&(&spec.input.schema, &self.input))
            .map_err(|_| Invalid::InvalidStart("input encoding"))?;
        Ok(blake3::derive_key(INPUT_DIGEST_CONTEXT, &bytes))
    }

    /// Commit to the ordered, canonical delegated Find grants.
    pub fn query_grants_digest(&self) -> Result<[u8; 32], Invalid> {
        self.validate()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(QUERY_GRANTS_DIGEST_DOMAIN);
        for query in &self.queries {
            hasher.update(&query.parent.as_bytes());
            let bytes = query
                .grant
                .encode()
                .map_err(|_| Invalid::InvalidStart("query encoding"))?;
            let length =
                u32::try_from(bytes.len()).map_err(|_| Invalid::InvalidStart("query encoding"))?;
            hasher.update(&length.to_be_bytes());
            hasher.update(&bytes);
        }
        Ok(*hasher.finalize().as_bytes())
    }
}

impl Try {
    /// Validate one Attempt intent without consulting mutable Run/Offer state.
    pub fn validate(&self) -> Result<(), Invalid> {
        validate_resources(&self.resources, "try")?;
        self.limits.validate()?;
        if let Some(offer) = &self.offer {
            if offer.station_epoch == StationEpoch::ZERO || offer.epoch == 0 {
                return Err(Invalid::InvalidTry("offer"));
            }
        }
        if let Some(lease) = &self.lease {
            if SchemaId::parse(lease.role.as_str()).as_ref() != Some(&lease.role)
                || lease.epoch == 0
            {
                return Err(Invalid::InvalidTry("lease"));
            }
        }
        if let Some(checkpoint) = &self.checkpoint {
            if checkpoint.sequence == 0
                || checkpoint.build != self.build
                || self.limits.checkpoints == 0
                || self.limits.checkpoint_bytes == 0
            {
                return Err(Invalid::InvalidTry("checkpoint"));
            }
        }
        if let Some(continuation) = &self.continuation {
            continuation
                .input
                .validate()
                .map_err(|_| Invalid::InvalidTry("continuation input"))?;
            if continuation.manifest_root == [0; 32] {
                return Err(Invalid::InvalidTry("continuation root"));
            }
        }
        Ok(())
    }

    /// Prove a cited Offer is the news this Try named, and that it claims the
    /// pinned Build. The Offer still reserves nothing: another Try may omit
    /// this news or cite different news for the same Run.
    pub fn validate_with_offer(&self, offer: &Offer) -> Result<(), Invalid> {
        self.validate()?;
        offer.validate()?;
        let Some(reference) = &self.offer else {
            return Err(Invalid::InvalidTry("offer"));
        };
        if reference.id != offer.id
            || reference.station != offer.station
            || reference.station_epoch != offer.station_epoch
            || reference.epoch != offer.epoch
        {
            return Err(Invalid::InvalidTry("offer"));
        }
        if !offer.covers(self.build) {
            return Err(Invalid::InvalidTry("build"));
        }
        Ok(())
    }

    /// First local Attempt for a Started Run that this Station will perform.
    ///
    /// The first Attempt names this Station activation only. It does not mint
    /// an Offer; E4 publishes signed news when that news exists.
    pub fn local_first(
        run: &Run,
        _station: StationKey,
        station_epoch: StationEpoch,
    ) -> Result<Self, Invalid> {
        if station_epoch == StationEpoch::ZERO {
            return Err(Invalid::InvalidTry("station"));
        }
        let limits = AttemptLimits {
            events: run.started.limits.events,
            checkpoints: run.started.limits.checkpoints,
            child_runs: run.started.limits.child_runs,
            progress_bytes: run.started.limits.progress_bytes,
            checkpoint_bytes: run.started.limits.checkpoint_bytes,
            wall_millis: run.started.limits.wall_millis,
        };
        let intent = Self {
            run: run.id,
            build: run.started.build,
            offer: None,
            resources: run.started.resources.clone(),
            enforcement: None,
            limits,
            lease: None,
            checkpoint: None,
            continuation: None,
        };
        intent.validate_with(run.started.limits)?;
        Ok(intent)
    }

    /// Cite live Offer news on a Station-local Attempt. The Offer still
    /// reserves nothing; another Station may omit it.
    pub fn with_offer(mut self, offer: OfferRef) -> Result<Self, Invalid> {
        self.offer = Some(offer);
        self.validate()?;
        Ok(self)
    }

    /// Prove the Attempt ceilings do not widen the pinned Run ceilings.
    pub fn validate_with(&self, run_limits: Limits) -> Result<(), Invalid> {
        self.validate()?;
        run_limits
            .validate()
            .map_err(|_| Invalid::InvalidTry("run limits"))?;
        if !self.limits.is_within(run_limits) {
            return Err(Invalid::InvalidTry("limits widening"));
        }
        Ok(())
    }
}

/// A World-declared change to durable work.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Create one durable Run from World-declared semantic intent. Runtime
    /// derives the Run id and ambient Session coordinates.
    Start(Start),
    /// Create one physical Attempt for an exact Station activation. Runtime
    /// derives the Attempt id after stateful admission.
    Try(Try),
    /// Commit a cancellation request for a Run. This does not claim an Attempt
    /// has stopped.
    Cancel { run: RunId },
    /// Authorize another Attempt under the same Run and pinned Build.
    Retry { run: RunId },
    /// Authorize another Attempt from a committed checkpoint.
    Resume { run: RunId, checkpoint: ContentRef },
    /// Accept one returned Attempt. Concurrent accepts remain visible.
    Accept { run: RunId, attempt: AttemptId },
    /// Reject one returned Attempt.
    Reject { run: RunId, attempt: AttemptId },
    /// Stop admitting new work to a Service while existing work drains.
    Drain { service: ServiceId },
}

/// Why canonical command bytes were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    /// The command exceeded the bound for the currently implemented control
    /// grammar.
    TooLarge,
    /// The bytes did not decode exactly or did not re-encode byte-for-byte.
    NonCanonical,
    /// The standalone contract generation is unknown to this Runtime.
    UnsupportedVersion(u8),
    /// A declared contract field is malformed, unbounded, duplicated, or out
    /// of canonical order.
    InvalidSpec(&'static str),
    /// A Build field is malformed, unbounded, duplicated, contradictory, or
    /// out of canonical order.
    InvalidBuild(&'static str),
    /// A Start intent field is malformed, unbounded, duplicated, or widening.
    InvalidStart(&'static str),
    /// A durable Run event is malformed or contradicts its derived identity.
    InvalidEvent(&'static str),
    /// A Try intent field is malformed, unbounded, contradictory, or widening.
    InvalidTry(&'static str),
    /// A Build's carried identity does not match its canonical material.
    BuildIdMismatch,
    /// An Offer field is malformed, unbounded, duplicated, expired, or out of
    /// canonical order.
    InvalidOffer(&'static str),
    /// An Offer's carried identity does not match its canonical material.
    OfferIdMismatch,
    /// The Build signature algorithm is unknown to this Runtime.
    UnsupportedSignatureAlgorithm(u8),
    /// The detached signature does not verify against the carried signer.
    BadBuildSignature,
    /// The detached Offer signature does not verify against the carried signer.
    BadOfferSignature,
    /// A readiness challenge is malformed, expired, or unbounded.
    InvalidChallenge(&'static str),
    /// A Ready answer is malformed or does not match its Challenge.
    InvalidReady(&'static str),
    /// The detached Ready signature does not verify against the carried signer.
    BadReadySignature,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for Invalid {}

/// Postcard encodes the variant index, rather than the Rust discriminant, so
/// declaration order is protocol order and is fixture-frozen below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum WireCmd {
    Start(Start),
    Try(Try),
    Cancel { run: RunId },
    Retry { run: RunId },
    Resume { run: RunId, checkpoint: ContentRef },
    Accept { run: RunId, attempt: AttemptId },
    Reject { run: RunId, attempt: AttemptId },
    Drain { service: ServiceId },
}

impl From<&Cmd> for WireCmd {
    fn from(command: &Cmd) -> Self {
        match command {
            Cmd::Start(start) => Self::Start(start.clone()),
            Cmd::Try(intent) => Self::Try(intent.clone()),
            Cmd::Cancel { run } => Self::Cancel { run: *run },
            Cmd::Retry { run } => Self::Retry { run: *run },
            Cmd::Resume { run, checkpoint } => Self::Resume {
                run: *run,
                checkpoint: *checkpoint,
            },
            Cmd::Accept { run, attempt } => Self::Accept {
                run: *run,
                attempt: *attempt,
            },
            Cmd::Reject { run, attempt } => Self::Reject {
                run: *run,
                attempt: *attempt,
            },
            Cmd::Drain { service } => Self::Drain { service: *service },
        }
    }
}

impl Cmd {
    /// Encode one command to its canonical postcard bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        match self {
            Self::Start(start) => start.validate()?,
            Self::Try(intent) => intent.validate()?,
            Self::Cancel { .. }
            | Self::Retry { .. }
            | Self::Resume { .. }
            | Self::Accept { .. }
            | Self::Reject { .. }
            | Self::Drain { .. } => {}
        }
        let bytes = postcard::to_stdvec(&WireCmd::from(self)).map_err(|_| Invalid::NonCanonical)?;
        if bytes.len() > MAX_CMD_BYTES {
            return Err(Invalid::TooLarge);
        }
        Ok(bytes)
    }

    /// Commit to the complete canonical command material retained in a Run.
    pub fn digest(&self) -> Result<[u8; 32], Invalid> {
        Ok(blake3::derive_key(COMMAND_DIGEST_CONTEXT, &self.encode()?))
    }

    /// Decode canonical command bytes.
    ///
    /// Unknown tags, trailing bytes, and non-minimal encodings all collapse to
    /// [`Invalid::NonCanonical`].
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > MAX_CMD_BYTES {
            return Err(Invalid::TooLarge);
        }
        let wire: WireCmd = postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
        let canonical = postcard::to_stdvec(&wire).map_err(|_| Invalid::NonCanonical)?;
        if canonical != bytes {
            return Err(Invalid::NonCanonical);
        }
        let command = match wire {
            WireCmd::Start(start) => Self::Start(start),
            WireCmd::Try(intent) => Self::Try(intent),
            WireCmd::Cancel { run } => Self::Cancel { run },
            WireCmd::Retry { run } => Self::Retry { run },
            WireCmd::Resume { run, checkpoint } => Self::Resume { run, checkpoint },
            WireCmd::Accept { run, attempt } => Self::Accept { run, attempt },
            WireCmd::Reject { run, attempt } => Self::Reject { run, attempt },
            WireCmd::Drain { service } => Self::Drain { service },
        };
        match &command {
            Self::Start(start) => start.validate()?,
            Self::Try(intent) => intent.validate()?,
            Self::Cancel { .. }
            | Self::Retry { .. }
            | Self::Resume { .. }
            | Self::Accept { .. }
            | Self::Reject { .. }
            | Self::Drain { .. } => {}
        }
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use mechanics::authorization::AuthorizedBodyKey;
    use replica::body::{BodyBinding, BodyId, BodyKey, Op, StaticBodyKeys, SupportedSchemas};
    use replica::transaction::{
        AuthoritySource, CommitAuthorization, CommitContext, SeedSigner, StaticAuthorizer,
        NO_PARENT_ROOT,
    };

    fn schema(name: &str, version: u32) -> SchemaRef {
        SchemaRef {
            name: SchemaId::parse(name).unwrap(),
            version,
        }
    }

    fn demand(capability: &str) -> Vec<u8> {
        mechanics::authorization::AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("com.example.exec", capability),
            mechanics::authorization::Resource::root("com.example.exec"),
        )
        .encode_canonical()
        .unwrap()
    }

    fn bound(value: u64) -> crate::find::Bound {
        crate::find::Bound {
            decoded_bodies: value,
            postings_read: value,
            edges_visited: value,
            nodes_visited: value,
            paths_retained: value,
            candidates_per_branch: value,
            score_evaluations: value,
            projected_bytes: value,
            packed_tokens: value,
            wall_millis: value,
        }
    }

    fn grant(name: &str) -> crate::find::Grant {
        crate::find::Grant {
            schemas: vec![crate::find::SchemaRef {
                name: SchemaId::parse(name).unwrap(),
                version: 1,
            }],
            ops: crate::find::OpSet::SEEK,
            fields: Vec::new(),
            edges: Vec::new(),
            gates: Vec::new(),
            modes: crate::find::ModeSet::EXACT,
            features: Vec::new(),
            bound: bound(1),
        }
    }

    fn find_schema(name: &str) -> crate::find::Schema {
        crate::find::Schema {
            reference: crate::find::SchemaRef {
                name: SchemaId::parse(name).unwrap(),
                version: 1,
            },
            sources: vec![crate::find::SourceRef {
                name: SchemaId::parse("record").unwrap(),
                version: 1,
            }],
            fields: Vec::new(),
            edges: Vec::new(),
            gates: Vec::new(),
            analyzers: Vec::new(),
            features: Vec::new(),
            ops: crate::find::OpSet::SEEK,
            modes: crate::find::ModeSet::EXACT,
            bound: bound(2),
        }
    }

    fn payload(name: &str) -> PayloadSpec {
        PayloadSpec {
            schema: schema(name, 1),
            max_inline_bytes: 1_024,
            max_content_refs: 1,
            max_content_bytes: 2_048,
            read: demand("payload.read"),
            max_additional_input_bytes: 0,
        }
    }

    fn spec() -> Spec {
        Spec {
            name: SchemaId::parse("check").unwrap(),
            version: 1,
            access: Access {
                start: demand("start"),
                offer: demand("offer"),
                control: demand("control"),
                accept: demand("accept"),
                attach: None,
            },
            input: payload("check.input"),
            output: payload("check.output"),
            mode: Mode::Unary,
            terminal: TerminalSpec::disabled(),
            resume: Resume::Restart,
            effects: Effects::Pure,
            accept: AcceptRule::World,
            queries: vec![grant("records")],
            service: None,
            links: Vec::new(),
            limits: Limits {
                attempts: 3,
                events: 64,
                checkpoints: 0,
                child_runs: 4,
                progress_bytes: 8_192,
                checkpoint_bytes: 0,
                wall_millis: 60_000,
            },
        }
    }

    fn content(byte: u8) -> ContentRef {
        ContentRef {
            content_id: [byte; 32],
        }
    }

    fn indexed_content(index: u16) -> ContentRef {
        let mut content_id = [0; 32];
        content_id[30..].copy_from_slice(&index.to_be_bytes());
        ContentRef { content_id }
    }

    fn build() -> Build {
        let seed = [0x42; 32];
        Build {
            id: BuildId::from_bytes([0; 32]),
            world: WorldId::parse("com.example.product").unwrap(),
            world_build: [0x11; 32],
            spec: schema("check", 1),
            handler: content(0x22),
            dependencies: Some(content(0x23)),
            environment: [0x24; 32],
            config: vec![content(0x30), content(0x31)],
            checkpoint: Some(schema("check.checkpoint", 1)),
            replay_commands: None,
            compatible_from: vec![BuildId::from_bytes([0x40; 32])],
            publisher: ActorId::from_incept_hash(&"a".repeat(64)),
            signature: Signature {
                signer: mechanics::actor::device_from_seed(&seed),
                algorithm: SIGNATURE_ED25519,
                bytes: [0; 64],
            },
        }
        .sign(&seed)
        .unwrap()
    }

    fn package_build() -> Build {
        package_build_with(0x24, 0x22)
    }

    fn package_build_with(environment: u8, handler: u8) -> Build {
        let mut candidate = build();
        candidate.checkpoint = None;
        candidate.environment = [environment; 32];
        candidate.handler = content(handler);
        candidate.sign(&[0x42; 32]).unwrap()
    }

    fn package_descriptor(declaration: Spec) -> crate::world::Descriptor {
        crate::world::Descriptor {
            id: WorldId::parse("com.example.product").unwrap(),
            implementation_version: crate::world::Version(1),
            schemas: Vec::new(),
            limits: crate::world::Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
            find_schemas: vec![find_schema("records")],
            find_extractors: Vec::new(),
            exec_specs: vec![declaration],
        }
    }

    struct TestHandler {
        binding: HandlerBinding,
    }

    impl Handler for TestHandler {
        fn binding(&self) -> &HandlerBinding {
            &self.binding
        }

        fn handle(&self, _context: &mut dyn HandlerContext) -> Result<Candidate, Failure> {
            Ok(Candidate {
                output: spec().output.schema,
                inline: vec![1, 2, 3],
                content: Vec::new(),
                content_bytes: 0,
                terminal: TerminalClass::Succeeded,
                usage: vec![resource("cpu.millis", 1)],
                evidence: Vec::new(),
            })
        }
    }

    fn package_handler(build: &Build) -> Arc<dyn Handler> {
        Arc::new(TestHandler {
            binding: HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: build.handler,
                role: None,
                links: Vec::new(),
            },
        })
    }

    fn projected_attempt(selected: BuildId) -> (Run, Start) {
        let mut started = started();
        started.build = selected;
        started.world_implementation = [0x11; 32];
        let root = RunEvent::started(started).unwrap();
        let run = root.run();
        let mut leased_event = leased_event(run, root.id().unwrap(), 0x63);
        {
            let RunEventKind::Leased(leased) = &mut leased_event.kind else {
                unreachable!("helper constructs a lease")
            };
            leased.build = selected;
            leased.lease = None;
        }
        let projection = Run::project(&[root, leased_event]).unwrap();
        let mut start = start();
        start.service = None;
        start.build = selected;
        (projection, start)
    }

    #[derive(Clone, Copy)]
    enum BackendBehavior {
        Return,
        Invalid,
        Panic,
    }

    struct BackendHandler {
        binding: HandlerBinding,
        calls: Arc<AtomicU64>,
        behavior: BackendBehavior,
    }

    impl Handler for BackendHandler {
        fn binding(&self) -> &HandlerBinding {
            &self.binding
        }

        fn handle(&self, _context: &mut dyn HandlerContext) -> Result<Candidate, Failure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                BackendBehavior::Panic => panic!("contained handler panic"),
                BackendBehavior::Return | BackendBehavior::Invalid => Ok(Candidate {
                    output: if matches!(self.behavior, BackendBehavior::Invalid) {
                        schema("wrong.output", 1)
                    } else {
                        spec().output.schema
                    },
                    inline: vec![1, 2, 3],
                    content: Vec::new(),
                    content_bytes: 0,
                    terminal: TerminalClass::Succeeded,
                    usage: vec![resource("cpu.millis", 1)],
                    evidence: Vec::new(),
                }),
            }
        }
    }

    fn backend_package(build: &Build, calls: Arc<AtomicU64>, behavior: BackendBehavior) -> Package {
        Package::new()
            .with_spec(spec())
            .with_build(build.clone())
            .with_handler(Arc::new(BackendHandler {
                binding: HandlerBinding {
                    spec: build.spec.clone(),
                    build: build.id,
                    artifact: build.handler,
                    role: None,
                    links: Vec::new(),
                },
                calls,
                behavior,
            }))
    }

    fn resource(name: &str, amount: u64) -> Resource {
        Resource {
            name: SchemaId::parse(name).unwrap(),
            amount,
        }
    }

    /// A child the scheduler let finish before its parent read the clock
    /// still claims wall time. Zero is a refused non-claim, so the claim a
    /// backend builds must never be one — this was a whole class of
    /// fast-child Outcomes rejected as InvalidOutcome on loaded runners.
    #[test]
    fn a_run_faster_than_the_clock_still_claims_wall_time() {
        assert_eq!(wall_claim(std::time::Duration::ZERO), 1);
        assert_eq!(wall_claim(std::time::Duration::from_micros(900)), 1);
        assert_eq!(wall_claim(std::time::Duration::from_millis(7)), 7);
        let claim = vec![resource(
            WALL_TIME_MS,
            wall_claim(std::time::Duration::ZERO),
        )];
        assert!(validate_resources(&claim, "event").is_ok());
        assert!(validate_resources(&[resource(WALL_TIME_MS, 0)], "event").is_err());
    }

    fn offer() -> Offer {
        let seed = [0x63; 32];
        Offer {
            id: OfferId::from_bytes([0; 16]),
            space: mechanics::ids::SpaceId::parse("ws_00000000000000000000000000").unwrap(),
            station: StationKey::from_key_bytes([0x63; 32]),
            station_epoch: StationEpoch::from_u64(2),
            actor: ActorId::from_incept_hash(&"a".repeat(64)),
            device: mechanics::actor::device_from_seed(&seed),
            world: WorldId::parse("com.example.product").unwrap(),
            world_build: [0x11; 32],
            builds: vec![OfferedBuild {
                id: build().id,
                spec: schema("check", 1),
            }],
            resources: vec![resource("memory.bytes", 65_536)],
            backend: SchemaId::parse("in-process.rust").unwrap(),
            enforcement: Enforcement::Advisory,
            resident: vec![content(0x80)],
            availability: Availability::Ready,
            epoch: 3,
            expiry: 4_000_000_000_000,
            publisher: ActorId::from_incept_hash(&"a".repeat(64)),
            signature: Signature {
                signer: mechanics::actor::device_from_seed(&seed),
                algorithm: SIGNATURE_ED25519,
                bytes: [0; 64],
            },
        }
        .sign(&seed)
        .unwrap()
    }

    fn start() -> Start {
        let declared = grant("records");
        Start {
            spec: schema("check", 1),
            build: build().id,
            input: Input {
                inline: vec![1, 2, 3],
                content: vec![content(0x50)],
                content_bytes: 1_024,
            },
            parent: Some(run(0x51)),
            source: Some(run(0x52)),
            service: Some(service(0x53)),
            resources: vec![resource("cpu.millis", 1_000)],
            limits: spec().limits,
            queries: vec![QueryGrant {
                parent: declared.digest().unwrap(),
                grant: declared,
            }],
            target: None,
        }
    }

    fn started() -> Started {
        let space = mechanics::ids::SpaceId::parse("ws_00000000000000000000000000").unwrap();
        let world = WorldId::parse("com.example.product").unwrap();
        let device = mechanics::actor::device_from_seed(&[0x41; 32]);
        let request = [0x42; 16];
        let run = derive_run_id(&space, &world, &device, request, 0);
        let mut intent = start();
        intent.service = None;
        let declaration = spec();
        let command = Cmd::Start(intent.clone());
        let command_bytes = command.encode().unwrap();
        Started {
            space,
            world,
            run,
            spec: intent.spec.clone(),
            mode: declaration.mode,
            terminal: declaration.terminal,
            world_implementation: [0x44; 32],
            build: intent.build,
            invoker: ActorId::from_incept_hash(&"a".repeat(64)),
            device,
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1, 2]),
            parent_manifest_root: [0x45; 32],
            input: declaration.input.schema.clone(),
            input_digest: intent.input_digest(&declaration).unwrap(),
            input_content: intent.input.content.clone(),
            input_content_bytes: intent.input.content_bytes,
            max_additional_input_bytes: declaration.input.max_additional_input_bytes,
            resources: intent.resources.clone(),
            limits: intent.limits,
            request,
            command: 0,
            parent: intent.parent,
            source: intent.source,
            service: intent.service,
            query_grants_digest: intent.query_grants_digest().unwrap(),
            command_digest: command.digest().unwrap(),
            command_bytes: u32::try_from(command_bytes.len()).unwrap(),
            command_chunks: 1,
            target: intent.target.clone(),
        }
    }

    const RUN_REPLICA_SEED: [u8; 32] = [0x41; 32];

    fn run_replica() -> replica::Replica {
        let mut supported = SupportedSchemas::new();
        supported.declare(
            WorldId::parse("com.example.product").unwrap(),
            SchemaId::parse(RUN_BODY_SCHEMA).unwrap(),
            RUN_BODY_SCHEMA_VERSION,
            EncodingId::parse(BODY_ENCODING).unwrap(),
            MUTATION_COLLABORATIVE,
        );
        supported.declare(
            WorldId::parse("com.example.product").unwrap(),
            SchemaId::parse(ACTIVE_RUN_BODY_SCHEMA).unwrap(),
            ACTIVE_RUN_BODY_SCHEMA_VERSION,
            EncodingId::parse(BODY_ENCODING).unwrap(),
            MUTATION_ATOMIC,
        );
        let keys = std::sync::Arc::new(StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([1; 16], [2; 32]),
        ));
        let mut replica = replica::Replica::loro().with_keys(keys);
        replica.set_supported(supported);
        replica
    }

    fn commit_started(replica: &mut replica::Replica) -> RunId {
        let started = started();
        let run = started.run;
        let world = started.world.clone();
        let space = started.space.clone();
        let device = started.device.clone();
        let event = RunEvent::started(started).unwrap().encode().unwrap();
        let mut intent = start();
        intent.service = None;
        let command = Cmd::Start(intent).encode().unwrap();
        let key = BodyKey::new(world.clone(), BodyId::from_bytes(run.as_bytes()));
        let active = active_run_body_key(&world, run);
        let mut operations = vec![
            (key.clone(), Op::Create),
            (
                key.clone(),
                Op::ListInsert {
                    path: RUN_EVENTS_PATH.to_owned(),
                    index: 0,
                    value: event,
                },
            ),
            (
                active.clone(),
                Op::ReplaceAtomic {
                    value: run.as_bytes().to_vec(),
                },
            ),
        ];
        for (index, chunk) in command.chunks(MAX_RUN_COMMAND_CHUNK_BYTES).enumerate() {
            operations.push((
                key.clone(),
                Op::MapSet {
                    path: RUN_COMMAND_PATH.to_owned(),
                    key: format!("{index:08x}"),
                    value: chunk.to_vec(),
                },
            ));
        }
        let signer = SeedSigner(&RUN_REPLICA_SEED);
        let context = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1, 2]),
        };
        let authorizer = StaticAuthorizer {
            world: world.clone(),
            implementation_id: [0x44; 32],
        };
        replica
            .commit_action(
                &context,
                &CommitAuthorization {
                    actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
                    parent_manifest_root: NO_PARENT_ROOT,
                    demand: demand("run.start"),
                    intent_digest: [3; 32],
                    authorizer: &authorizer,
                },
                &world,
                &device,
                &[4; 16],
                &[5; 32],
                Vec::new(),
                vec![key.clone(), active.clone()],
                "start",
                &operations,
                &[
                    (
                        key,
                        BodyBinding {
                            schema: SchemaId::parse(RUN_BODY_SCHEMA).unwrap(),
                            schema_version: RUN_BODY_SCHEMA_VERSION,
                            encoding: EncodingId::parse(BODY_ENCODING).unwrap(),
                            mutation_model: MUTATION_COLLABORATIVE,
                        },
                    ),
                    (
                        active,
                        BodyBinding {
                            schema: SchemaId::parse(ACTIVE_RUN_BODY_SCHEMA).unwrap(),
                            schema_version: ACTIVE_RUN_BODY_SCHEMA_VERSION,
                            encoding: EncodingId::parse(BODY_ENCODING).unwrap(),
                            mutation_model: MUTATION_ATOMIC,
                        },
                    ),
                ],
                &[],
            )
            .unwrap();
        run
    }

    fn commit_attempt(
        replica: &mut replica::Replica,
        include_returned: bool,
    ) -> (RunId, AttemptId) {
        let mut started = started();
        started.world_implementation = [0x11; 32];
        let run = started.run;
        let world = started.world.clone();
        let space = started.space.clone();
        let device = started.device.clone();
        let root = RunEvent::started(started).unwrap();
        let mut leased = leased_event(run, root.id().unwrap(), 0x63);
        let (executor, executor_device) = {
            let RunEventKind::Leased(value) = &mut leased.kind else {
                unreachable!("helper constructs a lease")
            };
            value.lease = None;
            (value.executor.clone(), value.device.clone())
        };
        // The Attempt is named by the (now finalized) lease event itself.
        let attempt = AttemptId::of_event(leased.id().unwrap());
        let began = RunEvent::new(
            vec![leased.id().unwrap()],
            RunEventKind::Began(Began {
                run,
                attempt,
                executor,
                device: executor_device,
            }),
        )
        .unwrap();
        let returned =
            include_returned.then(|| returned_event(run, attempt, began.id().unwrap(), 0x71));
        let events = [Some(root), Some(leased), Some(began), returned]
            .into_iter()
            .flatten()
            .map(|event| event.encode().unwrap())
            .collect::<Vec<_>>();
        let mut intent = start();
        intent.service = None;
        let command = Cmd::Start(intent).encode().unwrap();
        let key = BodyKey::new(world.clone(), BodyId::from_bytes(run.as_bytes()));
        let active = active_run_body_key(&world, run);
        let mut operations = vec![
            (key.clone(), Op::Create),
            (
                active.clone(),
                Op::ReplaceAtomic {
                    value: run.as_bytes().to_vec(),
                },
            ),
        ];
        for (index, event) in events.into_iter().enumerate() {
            operations.push((
                key.clone(),
                Op::ListInsert {
                    path: RUN_EVENTS_PATH.to_owned(),
                    index: u64::try_from(index).unwrap(),
                    value: event,
                },
            ));
        }
        for (index, chunk) in command.chunks(MAX_RUN_COMMAND_CHUNK_BYTES).enumerate() {
            operations.push((
                key.clone(),
                Op::MapSet {
                    path: RUN_COMMAND_PATH.to_owned(),
                    key: format!("{index:08x}"),
                    value: chunk.to_vec(),
                },
            ));
        }
        let signer = SeedSigner(&RUN_REPLICA_SEED);
        let context = CommitContext {
            space: &space,
            signer: &signer,
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1, 2]),
        };
        let authorizer = StaticAuthorizer {
            world: world.clone(),
            implementation_id: [0x11; 32],
        };
        replica
            .commit_action(
                &context,
                &CommitAuthorization {
                    actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
                    parent_manifest_root: NO_PARENT_ROOT,
                    demand: demand("run.start"),
                    intent_digest: [3; 32],
                    authorizer: &authorizer,
                },
                &world,
                &device,
                &[4; 16],
                &[5; 32],
                Vec::new(),
                vec![key.clone(), active.clone()],
                "start",
                &operations,
                &[
                    (
                        key,
                        BodyBinding {
                            schema: SchemaId::parse(RUN_BODY_SCHEMA).unwrap(),
                            schema_version: RUN_BODY_SCHEMA_VERSION,
                            encoding: EncodingId::parse(BODY_ENCODING).unwrap(),
                            mutation_model: MUTATION_COLLABORATIVE,
                        },
                    ),
                    (
                        active,
                        BodyBinding {
                            schema: SchemaId::parse(ACTIVE_RUN_BODY_SCHEMA).unwrap(),
                            schema_version: ACTIVE_RUN_BODY_SCHEMA_VERSION,
                            encoding: EncodingId::parse(BODY_ENCODING).unwrap(),
                            mutation_model: MUTATION_ATOMIC,
                        },
                    ),
                ],
                &[],
            )
            .unwrap();
        (run, attempt)
    }

    fn commit_begun_attempt(replica: &mut replica::Replica) -> (RunId, AttemptId) {
        commit_attempt(replica, false)
    }

    fn commit_returned_attempt(replica: &mut replica::Replica) -> (RunId, AttemptId) {
        commit_attempt(replica, true)
    }

    struct RunAuthorized;

    impl AuthoritySource for RunAuthorized {
        fn signer_authorized(&self, signer: &[u8; 32], _frontier: &AuthorityFrontier) -> bool {
            *signer
                == mechanics::actor::device_from_seed(&RUN_REPLICA_SEED)
                    .key_bytes()
                    .unwrap()
        }
    }

    fn try_intent() -> Try {
        Try {
            run: run(0x61),
            build: build().id,
            offer: Some(OfferRef {
                id: OfferId::from_bytes([0x62; 16]),
                station: StationKey::from_key_bytes([0x63; 32]),
                station_epoch: StationEpoch::from_u64(2),
                epoch: 3,
            }),
            resources: vec![resource("memory.bytes", 65_536)],
            enforcement: Some(content(0x64)),
            limits: AttemptLimits {
                events: 32,
                checkpoints: 0,
                child_runs: 2,
                progress_bytes: 4_096,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
            lease: Some(RoleLease {
                service: service(0x65),
                role: SchemaId::parse("worker").unwrap(),
                lease: LeaseId::from_bytes([0x66; 16]),
                epoch: 4,
            }),
            checkpoint: None,
            continuation: None,
        }
    }

    fn leased_event(run: RunId, predecessor: EventId, station: u8) -> RunEvent {
        let mut intent = try_intent();
        intent.run = run;
        let offer = intent.offer.as_mut().expect("test offer");
        offer.id = OfferId::from_bytes([station; 16]);
        offer.station = StationKey::from_key_bytes([station; 32]);
        let offer = intent.offer.clone().expect("test offer");
        RunEvent::new(
            vec![predecessor],
            RunEventKind::Leased(Leased {
                run,
                station: offer.station.clone(),
                station_epoch: offer.station_epoch,
                executor: ActorId::from_incept_hash(&"b".repeat(64)),
                device: mechanics::actor::device_from_seed(&[station; 32]),
                build: intent.build,
                offer: Some(offer.id),
                offer_epoch: offer.epoch,
                resources: intent.resources,
                enforcement: intent.enforcement,
                limits: intent.limits,
                lease: intent.lease,
                checkpoint: intent.checkpoint,
                continuation: intent.continuation,
            }),
        )
        .unwrap()
    }

    fn returned_event(
        run: RunId,
        attempt: AttemptId,
        predecessor: EventId,
        output: u8,
    ) -> RunEvent {
        RunEvent::new(
            vec![predecessor],
            RunEventKind::Returned(Returned {
                run,
                attempt,
                output: spec().output.schema,
                output_digest: [output; 32],
                output_inline_bytes: 3,
                output_content: vec![content(output)],
                output_content_bytes: 1_024,
                terminal: TerminalClass::Succeeded,
                usage: vec![resource("cpu.millis", u64::from(output))],
                evidence: vec![content(output.saturating_add(1))],
                transcript: None,
            }),
        )
        .unwrap()
    }

    fn run(byte: u8) -> RunId {
        RunId::from_bytes([byte; 16])
    }

    fn attempt(byte: u8) -> AttemptId {
        AttemptId::from_bytes([byte; 16])
    }

    fn service(byte: u8) -> ServiceId {
        ServiceId::from_bytes([byte; 16])
    }

    #[test]
    fn package_composition_binds_exact_specs_builds_and_handlers() {
        let declaration = spec();
        let descriptor = package_descriptor(declaration.clone());
        let build = package_build();
        let package = Package::new()
            .with_spec(declaration)
            .with_build(build.clone())
            .with_handler(package_handler(&build));

        assert_eq!(
            package.validate(
                &WorldId::parse("com.example.product").unwrap(),
                &[0x11; 32],
                &descriptor,
            ),
            Ok(())
        );
        assert_eq!(package.specs().len(), 1);
        assert_eq!(package.builds(), [build]);
        assert_eq!(package.handlers().len(), 1);
    }

    #[test]
    fn package_composition_rejects_ambiguous_or_cross_wired_material() {
        let declaration = spec();
        let descriptor = package_descriptor(declaration.clone());
        let build = package_build();
        let world = WorldId::parse("com.example.product").unwrap();

        let duplicate = Package::new()
            .with_spec(declaration.clone())
            .with_build(build.clone())
            .with_build(build.clone());
        assert_eq!(
            duplicate.validate(&world, &[0x11; 32], &descriptor),
            Err(PackageInvalid::DuplicateBuild(build.id))
        );

        let wrong_implementation = Package::new()
            .with_spec(declaration.clone())
            .with_build(build.clone());
        assert_eq!(
            wrong_implementation.validate(&world, &[0x99; 32], &descriptor),
            Err(PackageInvalid::BuildImplementation(build.id))
        );

        let handler = Arc::new(TestHandler {
            binding: HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: content(0xff),
                role: None,
                links: Vec::new(),
            },
        });
        let cross_wired = Package::new()
            .with_spec(declaration)
            .with_build(build.clone())
            .with_handler(handler);
        assert_eq!(
            cross_wired.validate(&world, &[0x11; 32], &descriptor),
            Err(PackageInvalid::HandlerBuild(build.id))
        );
    }

    #[test]
    fn package_handlers_cannot_claim_absent_roles_or_links() {
        let declaration = spec();
        let descriptor = package_descriptor(declaration.clone());
        let build = package_build();
        let world = WorldId::parse("com.example.product").unwrap();
        let handler = Arc::new(TestHandler {
            binding: HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: build.handler,
                role: Some(SchemaId::parse("worker").unwrap()),
                links: Vec::new(),
            },
        });
        let package = Package::new()
            .with_spec(declaration)
            .with_build(build)
            .with_handler(handler);

        assert!(matches!(
            package.validate(&world, &[0x11; 32], &descriptor),
            Err(PackageInvalid::UnknownHandlerRole { .. })
        ));
    }

    #[test]
    fn selection_uses_the_started_build_even_when_another_build_is_installed_first() {
        let old = package_build_with(0x81, 0x82);
        let new = package_build_with(0x83, 0x84);
        let package = Package::new()
            .with_spec(spec())
            .with_build(new.clone())
            .with_build(old.clone())
            .with_handler(package_handler(&new))
            .with_handler(package_handler(&old));
        package
            .validate(
                &WorldId::parse("com.example.product").unwrap(),
                &[0x11; 32],
                &package_descriptor(spec()),
            )
            .unwrap();

        let (run, _) = projected_attempt(old.id);
        let selected = package.select(&run, &run.attempts[0]).unwrap();
        assert_eq!(selected.spec().version, 1);
        assert_eq!(selected.build().id, old.id);

        let mut changed_retry = run.attempts[0].clone();
        changed_retry.build = new.id;
        assert_eq!(
            package.select(&run, &changed_retry).unwrap_err(),
            SelectionInvalid::Coordinates
        );
    }

    #[test]
    fn in_process_backend_is_advisory_bounded_and_panic_contained() {
        let backend = InProcess::new();
        assert_eq!(backend.enforcement(), Enforcement::Advisory);
        let build = package_build();
        let (run, start) = projected_attempt(build.id);
        let cancel = AtomicBool::new(false);

        let calls = Arc::new(AtomicU64::new(0));
        let package = backend_package(&build, calls.clone(), BackendBehavior::Return);
        let selected = package.select(&run, &run.attempts[0]).unwrap();
        let mut context = Context::new(&run, &start, &run.attempts[0], &[], &cancel).unwrap();
        let completion = backend.invoke(&selected, &mut context).unwrap();
        assert_eq!(completion.candidate().inline, [1, 2, 3]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        cancel.store(true, Ordering::Release);
        assert_eq!(
            backend.invoke(&selected, &mut context),
            Err(Failure::Cancelled)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        cancel.store(false, Ordering::Release);
        let invalid = backend_package(
            &build,
            Arc::new(AtomicU64::new(0)),
            BackendBehavior::Invalid,
        );
        let selected = invalid.select(&run, &run.attempts[0]).unwrap();
        assert_eq!(
            backend.invoke(&selected, &mut context),
            Err(Failure::InvalidOutcome)
        );

        let panicking =
            backend_package(&build, Arc::new(AtomicU64::new(0)), BackendBehavior::Panic);
        let selected = panicking.select(&run, &run.attempts[0]).unwrap();
        assert_eq!(
            backend.invoke(&selected, &mut context),
            Err(Failure::Handler)
        );
    }

    #[test]
    fn dispatcher_reaches_a_backend_only_from_a_complete_committed_attempt() {
        let build = build();
        let calls = Arc::new(AtomicU64::new(0));
        let package = backend_package(&build, calls.clone(), BackendBehavior::Return);
        let dispatcher = Dispatcher::new(&package, InProcess::new());
        let world = WorldId::parse("com.example.product").unwrap();
        let cancel = AtomicBool::new(false);

        let empty = run_replica();
        assert!(dispatcher
            .observe(&empty.read_snapshot(), &world)
            .unwrap()
            .is_empty());
        assert_eq!(
            dispatcher.invoke(
                &empty.read_snapshot(),
                &world,
                run(0x90),
                attempt(0x91),
                &cancel,
                Facets::default(),
            ),
            Err(DispatchFailure::Run(run(0x90)))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let mut root_only = run_replica();
        let root = commit_started(&mut root_only);
        assert_eq!(
            dispatcher.invoke(
                &root_only.read_snapshot(),
                &world,
                root,
                attempt(0x91),
                &cancel,
                Facets::default(),
            ),
            Err(DispatchFailure::Attempt(attempt(0x91)))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let mut committed = run_replica();
        let (run, attempt) = commit_begun_attempt(&mut committed);
        let observed = dispatcher
            .observe(&committed.read_snapshot(), &world)
            .unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].run.id, run);
        let completion = dispatcher
            .invoke(
                &committed.read_snapshot(),
                &world,
                run,
                attempt,
                &cancel,
                Facets::default(),
            )
            .unwrap();
        assert_eq!(completion.candidate().inline, [1, 2, 3]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn outcome_facade_decodes_only_an_exact_committed_return() {
        let mut replica = run_replica();
        let (run, attempt_id) = commit_returned_attempt(&mut replica);
        let world = WorldId::parse("com.example.product").unwrap();
        let snapshot = replica.read_snapshot();

        let facts = outcome_facts(&snapshot, &world, run, attempt_id)
            .unwrap()
            .unwrap();
        assert_eq!(facts.run, run);
        assert_eq!(facts.attempt, attempt_id);
        assert_eq!(facts.spec, schema("check", 1));
        assert_eq!(facts.build, build().id);
        assert_eq!(facts.output, spec().output.schema);
        assert_eq!(facts.output_digest, [0x71; 32]);
        assert_eq!(facts.output_content, [content(0x71)]);
        assert_eq!(facts.terminal, TerminalClass::Succeeded);
        assert!(facts.returned_exactly_once);
        assert_eq!(
            outcome_facts(&snapshot, &world, run, attempt(0xff)).unwrap(),
            None
        );
    }

    #[test]
    fn checkpoint_and_child_sinks_stage_only_bounded_runtime_material() {
        let build = package_build();
        let (mut projection, mut parent) = projected_attempt(build.id);
        projection.attempts[0].limits.checkpoints = 1;
        projection.attempts[0].limits.checkpoint_bytes = 2_048;
        projection.attempts[0].limits.child_runs = 1;
        parent.parent = None;
        let cancel = AtomicBool::new(false);
        let mut context =
            Context::new(&projection, &parent, &projection.attempts[0], &[], &cancel).unwrap();
        let checkpoint = CheckpointRef {
            content: content(0x81),
            build: build.id,
            sequence: 1,
        };
        context.save_checkpoint(checkpoint.clone()).unwrap();
        assert_eq!(
            context.save_checkpoint(checkpoint),
            Err(Failure::InvalidCheckpoint)
        );

        let mut child = parent.clone();
        child.parent = Some(projection.id);
        child.source = None;
        child.queries.clear();
        child.limits.events = projection.attempts[0].limits.events;
        child.limits.checkpoints = projection.attempts[0].limits.checkpoints;
        child.limits.child_runs = projection.attempts[0].limits.child_runs;
        child.limits.progress_bytes = projection.attempts[0].limits.progress_bytes;
        child.limits.checkpoint_bytes = projection.attempts[0].limits.checkpoint_bytes;
        child.limits.wall_millis = projection.attempts[0].limits.wall_millis;
        context.start_child(child.clone()).unwrap();
        assert_eq!(context.start_child(child), Err(Failure::ChildLimit));

        let (checkpoints, checkpoint_blobs, children, output_blobs) = context.take_staged();
        let completion = Completion {
            candidate: Candidate {
                output: spec().output.schema,
                inline: vec![1, 2, 3],
                content: Vec::new(),
                content_bytes: 0,
                terminal: TerminalClass::Succeeded,
                usage: vec![resource("cpu.millis", 1)],
                evidence: Vec::new(),
            },
            checkpoints,
            checkpoint_blobs,
            children,
            output_blobs,
            transcript_blob: None,
            transcript: None,
        };
        let events = completion
            .events(
                projection.id,
                projection.attempts[0].id,
                vec![projection.attempts[0].leased_event],
            )
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].kind, RunEventKind::Saved(_)));
        assert!(matches!(events[1].kind, RunEventKind::Returned(_)));
        assert_eq!(events[1].predecessors, vec![events[0].id().unwrap()]);
        assert_eq!(completion.children().len(), 1);
    }

    #[test]
    fn context_exposes_only_authenticated_attempt_coordinates_and_bounds() {
        let root = RunEvent::started(started()).unwrap();
        let run_id = root.run();
        let leased = leased_event(run_id, root.id().unwrap(), 0x63);
        let attempt = AttemptId::of_event(leased.id().unwrap());
        let projection = Run::project(&[root, leased]).unwrap();
        let mut intent = start();
        intent.service = None;
        let cancel = AtomicBool::new(false);
        let mut context =
            Context::new(&projection, &intent, &projection.attempts[0], &[], &cancel).unwrap();

        assert_eq!(context.world().as_str(), "com.example.product");
        assert_eq!(context.run(), run_id);
        assert_eq!(context.attempt(), attempt);
        assert_eq!(context.spec(), &schema("check", 1));
        assert_eq!(context.input_inline(), [1, 2, 3]);
        assert_eq!(context.input_content(), [content(0x50)]);
        assert_eq!(
            context.accepted_resources(),
            [resource("memory.bytes", 65_536)]
        );
        assert!(!context.cancel_asked());
        cancel.store(true, Ordering::Release);
        assert!(context.cancel_asked());

        let handler = TestHandler {
            binding: HandlerBinding {
                spec: schema("check", 1),
                build: context.build(),
                artifact: content(0x22),
                role: None,
                links: Vec::new(),
            },
        };
        let candidate = handler.handle(&mut context).unwrap();
        assert_eq!(candidate.validate_with_spec(&spec()), Ok(()));
    }

    #[test]
    fn semantic_ids_are_exact_canonical_bytes() {
        let id = RunId::from_bytes([0xa5; 16]);
        assert_eq!(id.as_bytes(), [0xa5; 16]);
        assert_eq!(postcard::to_stdvec(&id).unwrap(), vec![0xa5; 16]);
        assert_eq!(postcard::from_bytes::<RunId>(&[0xa5; 16]).unwrap(), id);
    }

    #[test]
    fn run_identity_binds_the_complete_idempotency_scope_and_ordinal() {
        let space = mechanics::ids::SpaceId::parse("ws_00000000000000000000000000").unwrap();
        let world = WorldId::parse("com.example.product").unwrap();
        let device = mechanics::actor::device_from_seed(&[0x41; 32]);
        let request = [0x42; 16];
        let run = derive_run_id(&space, &world, &device, request, 0);
        assert_eq!(run, derive_run_id(&space, &world, &device, request, 0));
        assert_ne!(run, derive_run_id(&space, &world, &device, request, 1));
        assert_ne!(
            run,
            derive_run_id(
                &space,
                &world,
                &mechanics::actor::device_from_seed(&[0x43; 32]),
                request,
                0,
            )
        );
        assert_ne!(
            run,
            derive_run_id(
                &space,
                &WorldId::parse("com.example.other").unwrap(),
                &device,
                request,
                0,
            )
        );
    }

    #[test]
    fn started_event_round_trips_and_rejects_identity_or_chunk_tampering() {
        let started = started();
        let event = RunEvent::started(started.clone()).unwrap();
        let bytes = event.encode().unwrap();
        assert_eq!(RunEvent::decode_canonical(&bytes), Ok(event.clone()));
        assert_eq!(
            event.id().unwrap(),
            RunEvent::decode_canonical(&bytes).unwrap().id().unwrap()
        );

        let mut wrong_run = started.clone();
        wrong_run.run = RunId::from_bytes([0x99; 16]);
        assert_eq!(
            RunEvent::started(wrong_run),
            Err(Invalid::InvalidEvent("started"))
        );
        let mut wrong_chunks = started;
        wrong_chunks.command_chunks = 2;
        assert_eq!(
            RunEvent::started(wrong_chunks),
            Err(Invalid::InvalidEvent("started"))
        );
    }

    #[test]
    fn incorporating_started_is_inert_until_local_control_consumes_the_scan() {
        static LAUNCHES: AtomicU64 = AtomicU64::new(0);

        LAUNCHES.store(0, Ordering::SeqCst);
        let mut origin = run_replica();
        let expected_run = commit_started(&mut origin);
        let material = origin.export_material().unwrap();

        let mut adopted = run_replica();
        let started = started();
        let signer = SeedSigner(&RUN_REPLICA_SEED);
        let context = CommitContext {
            space: &started.space,
            signer: &signer,
            authority_frontier: started.authority_frontier.clone(),
        };
        for (transaction, bodies) in &material {
            adopted
                .incorporate(&context, transaction, bodies, &RunAuthorized)
                .unwrap();
        }
        assert_eq!(LAUNCHES.load(Ordering::SeqCst), 0);

        let unresolved = scan_unresolved(&adopted.read_snapshot(), &started.world).unwrap();
        assert_eq!(LAUNCHES.load(Ordering::SeqCst), 0);
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].run.id, expected_run);
        assert_eq!(unresolved[0].start.spec, started.spec);

        // This increment stands in for the app-owned local controller. Neither
        // incorporation nor the scan has a route to it.
        for _candidate in unresolved {
            LAUNCHES.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(LAUNCHES.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn scan_requires_exact_complete_command_chunks() {
        let started = started();
        let mut intent = start();
        intent.service = None;
        let command = Cmd::Start(intent.clone()).encode().unwrap();
        let mut view = fabric::CollaborativeView::default();
        view.maps.insert(
            RUN_COMMAND_PATH.to_owned(),
            BTreeMap::from([("00000000".to_owned(), command)]),
        );
        assert_eq!(start_from_body(&view, &started), Ok(intent));

        view.maps
            .get_mut(RUN_COMMAND_PATH)
            .unwrap()
            .insert("00000001".to_owned(), Vec::new());
        assert_eq!(
            start_from_body(&view, &started),
            Err(Invalid::InvalidEvent("run command"))
        );
    }

    #[test]
    fn run_event_kind_tags_and_round_trips_are_frozen() {
        fn tag(kind: &RunEventKind) -> u8 {
            postcard::to_stdvec(kind).unwrap()[0]
        }

        let root = RunEvent::started(started()).unwrap();
        let run = root.run();
        let predecessor = root.id().unwrap();
        let actor = ActorId::from_incept_hash(&"b".repeat(64));
        let device = mechanics::actor::device_from_seed(&[0x46; 32]);
        let intent = try_intent();
        let offer = intent.offer.clone().expect("test offer");
        let leased = Leased {
            run,
            station: offer.station,
            station_epoch: offer.station_epoch,
            executor: actor.clone(),
            device: device.clone(),
            build: intent.build,
            offer: Some(offer.id),
            offer_epoch: offer.epoch,
            resources: intent.resources,
            enforcement: intent.enforcement,
            limits: intent.limits,
            lease: intent.lease,
            checkpoint: intent.checkpoint,
            continuation: intent.continuation,
        };
        let kinds = [
            RunEventKind::Started(started()),
            RunEventKind::Leased(leased),
            RunEventKind::Began(Began {
                run,
                attempt: attempt(1),
                executor: actor.clone(),
                device: device.clone(),
            }),
            RunEventKind::Saved(Saved {
                run,
                attempt: attempt(1),
                checkpoint: CheckpointRef {
                    content: content(0x70),
                    build: build().id,
                    sequence: 1,
                },
            }),
            RunEventKind::Returned(Returned {
                run,
                attempt: attempt(1),
                output: spec().output.schema,
                output_digest: [0x71; 32],
                output_inline_bytes: 3,
                output_content: vec![content(0x72)],
                output_content_bytes: 1_024,
                terminal: TerminalClass::Succeeded,
                usage: vec![resource("cpu.millis", 10)],
                evidence: vec![content(0x73)],
                transcript: None,
            }),
            RunEventKind::Failed(Failed {
                run,
                attempt: attempt(1),
                class: FailureClass::Backend,
                evidence: vec![content(0x74)],
            }),
            RunEventKind::CancelAsked(CancelAsked {
                run,
                actor: actor.clone(),
                device: device.clone(),
            }),
            RunEventKind::Cancelled(Cancelled {
                run,
                attempt: Some(attempt(1)),
                actor: actor.clone(),
                device: device.clone(),
            }),
            RunEventKind::Accepted(Accepted {
                run,
                attempt: attempt(1),
                actor: actor.clone(),
                device: device.clone(),
            }),
            RunEventKind::Rejected(Rejected {
                run,
                attempt: attempt(1),
                actor: actor.clone(),
                device: device.clone(),
            }),
            RunEventKind::Presumed(Presumed {
                run,
                attempt: attempt(1),
                evidence: PresumedEvidence::Deadline,
                presumer: actor.clone(),
                device: device.clone(),
            }),
            RunEventKind::Renewed(Renewed {
                run,
                attempt: attempt(1),
                executor: actor,
                device,
            }),
        ];
        assert_eq!(
            kinds.iter().map(tag).collect::<Vec<_>>(),
            (0..12).collect::<Vec<_>>()
        );

        for kind in kinds.into_iter().skip(1) {
            let event = RunEvent::new(vec![predecessor], kind).unwrap();
            let bytes = event.encode().unwrap();
            assert_eq!(RunEvent::decode_canonical(&bytes), Ok(event));
        }
    }

    #[test]
    fn concurrent_acceptance_heads_remain_visible_until_an_explicit_join() {
        let root = RunEvent::started(started()).unwrap();
        let run = root.run();
        let root_id = root.id().unwrap();
        let actor = ActorId::from_incept_hash(&"b".repeat(64));
        let device = mechanics::actor::device_from_seed(&[0x46; 32]);
        let accepted = |attempt, predecessor| {
            RunEvent::new(
                vec![predecessor],
                RunEventKind::Accepted(Accepted {
                    run,
                    attempt,
                    actor: actor.clone(),
                    device: device.clone(),
                }),
            )
            .unwrap()
        };
        let first = accepted(attempt(1), root_id);
        let second = accepted(attempt(2), root_id);
        let first_id = first.id().unwrap();
        let second_id = second.id().unwrap();
        let mut expected = vec![first_id, second_id];
        expected.sort_unstable();
        assert_eq!(
            run_event_heads(&[second.clone(), root.clone(), first.clone()]),
            Ok(expected.clone())
        );

        let joined = RunEvent::new(
            expected,
            RunEventKind::Accepted(Accepted {
                run,
                attempt: attempt(1),
                actor,
                device,
            }),
        )
        .unwrap();
        assert_eq!(
            run_event_heads(&[first, joined.clone(), root, second]),
            Ok(vec![joined.id().unwrap()])
        );
    }

    #[test]
    fn event_history_rejects_missing_duplicate_or_noncanonical_predecessors() {
        let root = RunEvent::started(started()).unwrap();
        let run = root.run();
        let actor = ActorId::from_incept_hash(&"b".repeat(64));
        let device = mechanics::actor::device_from_seed(&[0x46; 32]);
        let kind = || {
            RunEventKind::CancelAsked(CancelAsked {
                run,
                actor: actor.clone(),
                device: device.clone(),
            })
        };

        assert_eq!(
            RunEvent::new(Vec::new(), kind()),
            Err(Invalid::InvalidEvent("predecessors"))
        );
        let missing = RunEvent::new(vec![EventId::from_bytes([0x77; 32])], kind()).unwrap();
        assert_eq!(
            run_event_heads(&[root.clone(), missing]),
            Err(Invalid::InvalidEvent("missing predecessor"))
        );
        assert_eq!(
            run_event_heads(&[root.clone(), root]),
            Err(Invalid::InvalidEvent("duplicate event"))
        );

        let high = EventId::from_bytes([2; 32]);
        let low = EventId::from_bytes([1; 32]);
        assert_eq!(
            RunEvent::new(vec![high, low], kind()),
            Err(Invalid::InvalidEvent("predecessors"))
        );
    }

    #[test]
    fn run_projection_keeps_retry_attempts_outcomes_and_acceptance_facts_distinct() {
        let root = RunEvent::started(started()).unwrap();
        let run = root.run();
        let root_id = root.id().unwrap();
        let first_lease = leased_event(run, root_id, 0x51);
        let second_lease = leased_event(run, root_id, 0x52);
        let first_attempt = AttemptId::of_event(first_lease.id().unwrap());
        let second_attempt = AttemptId::of_event(second_lease.id().unwrap());
        let first_return = returned_event(run, first_attempt, first_lease.id().unwrap(), 0x61);
        let second_return = returned_event(run, second_attempt, second_lease.id().unwrap(), 0x62);
        let actor = ActorId::from_incept_hash(&"c".repeat(64));
        let accepting_actor = actor.clone();
        let device = mechanics::actor::device_from_seed(&[0x63; 32]);
        let first_accept = RunEvent::new(
            vec![first_return.id().unwrap()],
            RunEventKind::Accepted(Accepted {
                run,
                attempt: first_attempt,
                actor: actor.clone(),
                device: device.clone(),
            }),
        )
        .unwrap();
        let second_accept = RunEvent::new(
            vec![second_return.id().unwrap()],
            RunEventKind::Accepted(Accepted {
                run,
                attempt: second_attempt,
                actor,
                device,
            }),
        )
        .unwrap();
        let events = [
            second_accept.clone(),
            first_return,
            root,
            second_lease,
            first_accept.clone(),
            first_lease,
            second_return,
        ];

        let projection = Run::project(&events).unwrap();
        assert_eq!(projection.id, run);
        // Attempts sort by their content-hash identity, not insertion order,
        // so look each up by id rather than by position.
        let by_id = |id| {
            projection
                .attempts
                .iter()
                .find(|attempt| attempt.id == id)
                .expect("attempt present")
        };
        let mut ids = projection
            .attempts
            .iter()
            .map(|attempt| attempt.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        let mut want = vec![first_attempt, second_attempt];
        want.sort_unstable();
        assert_eq!(ids, want);
        assert_eq!(by_id(first_attempt).outcomes.len(), 1);
        assert_eq!(by_id(first_attempt).outcomes[0].output_digest, [0x61; 32]);
        assert_eq!(by_id(second_attempt).outcomes.len(), 1);
        assert_eq!(projection.accepted.len(), 2);
        assert!(projection
            .accepted
            .iter()
            .all(|fact| fact.value.actor == accepting_actor));
        let mut acceptance_heads = vec![first_accept.id().unwrap(), second_accept.id().unwrap()];
        acceptance_heads.sort_unstable();
        assert_eq!(projection.heads, acceptance_heads);
    }

    #[test]
    fn work_requests_keep_watch_cursors_canonical_and_start_out_of_the_capability() {
        let world = WorldId::parse("com.example.product").unwrap();
        let run = run(0x41);
        let low = EventId::from_bytes([1; 32]);
        let high = EventId::from_bytes([2; 32]);

        let inspect = WorkRequest::Inspect {
            world: world.clone(),
            run,
        };
        assert_eq!(inspect.validate(), Ok(()));
        assert!(!inspect.is_command());
        assert_eq!(inspect.world(), &world);
        assert_eq!(inspect.run(), run);
        assert!(!serde_json::to_string(&inspect).unwrap().contains("Start"));

        let canonical = WorkRequest::Watch {
            world: world.clone(),
            run,
            known_heads: vec![low, high],
        };
        assert_eq!(canonical.validate(), Ok(()));
        assert_ne!(canonical.digest().unwrap(), inspect.digest().unwrap());

        for known_heads in [vec![high, low], vec![low, low]] {
            assert_eq!(
                WorkRequest::Watch {
                    world: world.clone(),
                    run,
                    known_heads,
                }
                .validate(),
                Err(Invalid::InvalidEvent("work watch heads"))
            );
        }

        assert!(WorkRequest::Cancel { world, run }.is_command());
    }

    #[test]
    fn only_run_level_terminal_facts_remove_a_run_from_the_unresolved_scan() {
        let root = RunEvent::started(started()).unwrap();
        let run = root.run();
        let root_id = root.id().unwrap();
        assert!(Run::project(std::slice::from_ref(&root))
            .unwrap()
            .is_unresolved());

        let lease = leased_event(run, root_id, 0x51);
        let lease_attempt = AttemptId::of_event(lease.id().unwrap());
        let actor = ActorId::from_incept_hash(&"b".repeat(64));
        let device = mechanics::actor::device_from_seed(&[0x52; 32]);
        let attempt_cancelled = RunEvent::new(
            vec![lease.id().unwrap()],
            RunEventKind::Cancelled(Cancelled {
                run,
                attempt: Some(lease_attempt),
                actor: actor.clone(),
                device: device.clone(),
            }),
        )
        .unwrap();
        assert!(Run::project(&[root.clone(), lease, attempt_cancelled])
            .unwrap()
            .is_unresolved());

        let run_cancelled = RunEvent::new(
            vec![root_id],
            RunEventKind::Cancelled(Cancelled {
                run,
                attempt: None,
                actor,
                device,
            }),
        )
        .unwrap();
        assert!(!Run::project(&[root, run_cancelled])
            .unwrap()
            .is_unresolved());
    }

    #[test]
    fn unresolved_scan_preserves_typed_body_image_capacity_failure() {
        struct RefusingReader {
            world: WorldId,
            run: RunId,
        }

        impl ReservedBodyReader for RefusingReader {
            fn content_descriptor(
                &self,
                _content: &replica::content::ContentRef,
            ) -> Option<replica::content::ContentDescriptor> {
                None
            }

            fn binding(&self, key: &BodyKey) -> Option<BodyBinding> {
                (key == &active_run_body_key(&self.world, self.run)).then(|| BodyBinding {
                    schema: SchemaId::parse(ACTIVE_RUN_BODY_SCHEMA).unwrap(),
                    schema_version: ACTIVE_RUN_BODY_SCHEMA_VERSION,
                    encoding: replica::body::EncodingId::parse(BODY_ENCODING).unwrap(),
                    mutation_model: MUTATION_ATOMIC,
                })
            }

            fn body_keys_page_with_schema(
                &self,
                _world: &WorldId,
                _schema: &SchemaId,
                after: Option<&BodyKey>,
                _limit: usize,
            ) -> Vec<BodyKey> {
                after
                    .is_none()
                    .then(|| active_run_body_key(&self.world, self.run))
                    .into_iter()
                    .collect()
            }

            fn read_atomic(
                &self,
                key: &BodyKey,
            ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure>
            {
                Err(crate::world::BodyReadFailure::Capacity(
                    crate::world::BodyReadCoordinate::new(key.clone(), Some([0x81; 32])),
                ))
            }

            fn read_collaborative(
                &self,
                _key: &BodyKey,
            ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure>
            {
                panic!("active marker refusal must happen before Run hydration")
            }
        }

        let world = WorldId::parse("com.example.product").unwrap();
        let reader = RefusingReader {
            world: world.clone(),
            run: run(0x82),
        };
        assert!(matches!(
            scan_unresolved(&reader, &world),
            Err(ReadFailure::Body(crate::world::BodyReadFailure::Capacity(
                _
            )))
        ));
    }

    #[test]
    fn predecessor_run_schema_is_quarantined_without_poisoning_dispatch() {
        struct PredecessorReader {
            world: WorldId,
            run: RunId,
        }

        impl ReservedBodyReader for PredecessorReader {
            fn content_descriptor(
                &self,
                _content: &replica::content::ContentRef,
            ) -> Option<replica::content::ContentDescriptor> {
                None
            }

            fn binding(&self, key: &BodyKey) -> Option<BodyBinding> {
                if key == &active_run_body_key(&self.world, self.run) {
                    return Some(BodyBinding {
                        schema: SchemaId::parse(ACTIVE_RUN_BODY_SCHEMA).unwrap(),
                        schema_version: ACTIVE_RUN_BODY_SCHEMA_VERSION,
                        encoding: replica::body::EncodingId::parse(BODY_ENCODING).unwrap(),
                        mutation_model: MUTATION_ATOMIC,
                    });
                }
                (key.body.as_bytes() == self.run.as_bytes()).then(|| BodyBinding {
                    schema: SchemaId::parse(RUN_BODY_SCHEMA).unwrap(),
                    schema_version: 1,
                    encoding: replica::body::EncodingId::parse(BODY_ENCODING).unwrap(),
                    mutation_model: MUTATION_COLLABORATIVE,
                })
            }

            fn body_keys_page_with_schema(
                &self,
                _world: &WorldId,
                _schema: &SchemaId,
                after: Option<&BodyKey>,
                _limit: usize,
            ) -> Vec<BodyKey> {
                after
                    .is_none()
                    .then(|| active_run_body_key(&self.world, self.run))
                    .into_iter()
                    .collect()
            }

            fn read_atomic(
                &self,
                key: &BodyKey,
            ) -> Result<Option<crate::world::BodyBytes>, crate::world::BodyReadFailure>
            {
                Ok((key == &active_run_body_key(&self.world, self.run))
                    .then(|| crate::world::BodyBytes::owned(self.run.as_bytes().to_vec())))
            }

            fn read_collaborative(
                &self,
                _key: &BodyKey,
            ) -> Result<Option<crate::world::CollaborativeBody>, crate::world::BodyReadFailure>
            {
                panic!("a predecessor Run must be refused before reading its event encoding")
            }
        }

        let world = WorldId::parse("com.example.compatibility").unwrap();
        let run = run(0x83);
        let reader = PredecessorReader {
            world: world.clone(),
            run,
        };

        assert_eq!(scan_unresolved(&reader, &world).unwrap(), Vec::new());
        assert_eq!(
            read_committed_run(&reader, &world, run),
            Err(ReadFailure::UnsupportedRunSchema { found: 1 })
        );
        assert_eq!(
            work_state(&reader, &world, run),
            Err(ReadFailure::UnsupportedRunSchema { found: 1 })
        );
    }

    #[test]
    fn run_projection_refuses_ambiguous_or_unbound_attempt_facts() {
        let root = RunEvent::started(started()).unwrap();
        let run = root.run();
        let root_id = root.id().unwrap();
        let lease = leased_event(run, root_id, 0x51);
        let attempt_id = AttemptId::of_event(lease.id().unwrap());
        let first_return = returned_event(run, attempt_id, lease.id().unwrap(), 0x61);
        let second_return = returned_event(run, attempt_id, lease.id().unwrap(), 0x62);
        assert_eq!(
            Run::project(&[root.clone(), lease.clone(), first_return, second_return]),
            Err(Invalid::InvalidEvent("repeated return"))
        );

        let actor = ActorId::from_incept_hash(&"c".repeat(64));
        let device = mechanics::actor::device_from_seed(&[0x63; 32]);
        let accepted = RunEvent::new(
            vec![lease.id().unwrap()],
            RunEventKind::Accepted(Accepted {
                run,
                attempt: attempt_id,
                actor,
                device,
            }),
        )
        .unwrap();
        assert_eq!(
            Run::project(&[root.clone(), lease.clone(), accepted]),
            Err(Invalid::InvalidEvent("choice without outcome"))
        );

        // Attempt identity is now the lease event's identity, so the only
        // duplicate Attempt is the very same signed event twice — caught as a
        // duplicate event before the attempt map is even built. A different
        // Station would mint a different event id, hence a distinct Attempt.
        let duplicate = lease.clone();
        assert_eq!(
            Run::project(&[root, lease, duplicate]),
            Err(Invalid::InvalidEvent("duplicate event"))
        );
    }

    #[test]
    fn a_presumption_is_non_terminal_and_an_outcome_reconciles_over_it() {
        let root = RunEvent::started(started()).unwrap();
        let run = root.run();
        let root_id = root.id().unwrap();
        let lease = leased_event(run, root_id, 0x51);
        let attempt_id = AttemptId::of_event(lease.id().unwrap());
        // The helper leases under the device derived from its station byte.
        let executor_device = mechanics::actor::device_from_seed(&[0x51; 32]);

        // A third party — a different device — presumes the Attempt dead.
        let presumer = ActorId::from_incept_hash(&"e".repeat(64));
        let presumer_device = mechanics::actor::device_from_seed(&[0x99; 32]);
        let presumed = RunEvent::new(
            vec![lease.id().unwrap()],
            RunEventKind::Presumed(Presumed {
                run,
                attempt: attempt_id,
                evidence: PresumedEvidence::Deadline,
                presumer: presumer.clone(),
                device: presumer_device.clone(),
            }),
        )
        .unwrap();

        // The presumption attaches, but the Run is still unresolved: a
        // suspicion is not a verdict.
        let projection = Run::project(&[root.clone(), lease.clone(), presumed.clone()]).unwrap();
        assert!(projection.is_unresolved());
        assert_eq!(projection.attempts.len(), 1);
        assert_eq!(projection.attempts[0].presumptions.len(), 1);
        assert_eq!(
            projection.attempts[0].presumptions[0].value.evidence,
            PresumedEvidence::Deadline
        );
        assert!(projection.attempts[0].outcomes.is_empty());

        // The original executor returns after being presumed: the Outcome
        // reconciles over the presumption, on the same Attempt.
        let returned = returned_event(run, attempt_id, presumed.id().unwrap(), 0x61);
        let reconciled =
            Run::project(&[root.clone(), lease.clone(), presumed.clone(), returned]).unwrap();
        assert_eq!(reconciled.attempts[0].presumptions.len(), 1);
        assert_eq!(reconciled.attempts[0].outcomes.len(), 1);
        assert!(reconciled.is_unresolved());

        // The executor cannot presume itself: a self-presumption is malformed.
        let self_presumed = RunEvent::new(
            vec![lease.id().unwrap()],
            RunEventKind::Presumed(Presumed {
                run,
                attempt: attempt_id,
                evidence: PresumedEvidence::Deadline,
                presumer,
                device: executor_device,
            }),
        )
        .unwrap();
        assert_eq!(
            Run::project(&[root, lease, self_presumed]),
            Err(Invalid::InvalidEvent("self presumption"))
        );
    }

    #[test]
    fn run_projection_allows_zero_attempts_and_checks_limits_before_expansion() {
        let root = RunEvent::started(started()).unwrap();
        let projection = Run::project(std::slice::from_ref(&root)).unwrap();
        assert!(projection.attempts.is_empty());
        assert_eq!(projection.heads, vec![root.id().unwrap()]);

        let mut bounded = started();
        bounded.limits.attempts = 1;
        let root = RunEvent::started(bounded).unwrap();
        let run = root.run();
        let root_id = root.id().unwrap();
        let first = leased_event(run, root_id, 0x51);
        let second = leased_event(run, root_id, 0x52);
        assert_eq!(
            Run::project(&[root, first, second]),
            Err(Invalid::InvalidEvent("attempts"))
        );
    }

    #[test]
    fn runtime_body_schema_reservations_are_frozen() {
        let schemas = body_schemas();
        assert_eq!(
            schemas
                .iter()
                .map(|schema| schema.id.as_str())
                .collect::<Vec<_>>(),
            RESERVED_SCHEMAS
        );
        assert_eq!(
            schemas
                .iter()
                .map(|schema| schema.version)
                .collect::<Vec<_>>(),
            [
                RUN_BODY_SCHEMA_VERSION,
                BUILD_BODY_SCHEMA_VERSION,
                SERVICE_BODY_SCHEMA_VERSION,
                ACTIVE_RUN_BODY_SCHEMA_VERSION,
            ]
        );
        for schema in &schemas[..3] {
            assert_eq!(schema.encoding.as_str(), BODY_ENCODING);
            assert!(matches!(schema.mutation, MutationModel::Collaborative(_)));
            assert!(schema.readable_predecessors.is_empty());
            assert!(is_reserved_schema(&schema.id));
        }
        assert_eq!(schemas[3].encoding.as_str(), BODY_ENCODING);
        assert_eq!(schemas[3].mutation, MutationModel::Atomic);
        assert!(schemas[3].readable_predecessors.is_empty());
        assert!(is_reserved_schema(&schemas[3].id));
        assert!(!is_reserved_schema(
            &SchemaId::parse("product.run").unwrap()
        ));
    }

    #[test]
    fn spec_shape_and_canonical_bytes_are_frozen() {
        let spec = spec();
        let bytes = spec.encode().unwrap();
        assert_eq!(Spec::decode_canonical(&bytes), Ok(spec));

        let digest = blake3::hash(&bytes);
        assert_eq!(
            digest.to_hex().as_str(),
            "62eaea6c55fbd52990c85ebac0130d350ca42a584e607ef595b0319c9b3ee9ab"
        );

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            Spec::decode_canonical(&trailing),
            Err(Invalid::NonCanonical)
        );

        let mut unknown = bytes;
        unknown[0] = SPEC_VERSION + 1;
        assert_eq!(
            Spec::decode_canonical(&unknown),
            Err(Invalid::UnsupportedVersion(SPEC_VERSION + 1))
        );
        assert_eq!(
            Spec::decode_canonical(&vec![0; MAX_SPEC_BYTES + 1]),
            Err(Invalid::TooLarge)
        );
    }

    #[test]
    fn component_variant_tags_are_frozen() {
        fn tag<T: Serialize>(value: &T) -> u8 {
            postcard::to_stdvec(value).unwrap()[0]
        }

        assert_eq!(
            [
                tag(&Mode::Unary),
                tag(&Mode::Stream),
                tag(&Mode::Interactive)
            ],
            [0, 1, 2]
        );
        assert_eq!(
            [
                tag(&Resume::Restart),
                tag(&Resume::Checkpoint {
                    codec: schema("checkpoint", 1),
                }),
                tag(&Resume::Replay { commands: 1 }),
                tag(&Resume::Never),
            ],
            [0, 1, 2, 3]
        );
        assert_eq!(
            [
                tag(&Effects::Pure),
                tag(&Effects::Idempotent {
                    key: schema("effect.key", 1),
                }),
                tag(&Effects::ExternalAtLeastOnce),
            ],
            [0, 1, 2]
        );
        assert_eq!(
            [tag(&AcceptRule::World), tag(&AcceptRule::Authorized)],
            [0, 1]
        );
        assert_eq!(
            [
                tag(&Send::Store),
                tag(&Send::Direct),
                tag(&Send::Signal),
                tag(&Send::Stream),
                tag(&Send::Fetch),
            ],
            [0, 1, 2, 3, 4]
        );
        assert_eq!(
            [
                tag(&RankRule::Inherit),
                tag(&RankRule::Cap(1)),
                tag(&RankRule::Reset),
                tag(&RankRule::Recompute),
            ],
            [0, 1, 2, 3]
        );
        assert_eq!(tag(&ReadyRule::All), 0);
    }

    #[test]
    fn every_access_and_payload_read_demand_is_canonical_and_nonempty() {
        for field in 0..6 {
            let mut candidate = spec();
            match field {
                0 => candidate.access.start.clear(),
                1 => candidate.access.offer = vec![0],
                2 => candidate.access.control.clear(),
                3 => candidate.access.accept = vec![0],
                4 => candidate.input.read.clear(),
                5 => candidate.output.read = vec![0],
                _ => unreachable!(),
            }
            assert!(matches!(candidate.validate(), Err(Invalid::InvalidSpec(_))));
        }
    }

    #[test]
    fn payload_mode_resume_and_limit_bounds_reject_before_encoding() {
        let mut candidate = spec();
        candidate.input.max_content_refs = 0;
        assert_eq!(candidate.validate(), Err(Invalid::InvalidSpec("input")));

        let mut candidate = spec();
        candidate.mode = Mode::Interactive;
        assert_eq!(
            candidate.validate(),
            Err(Invalid::InvalidSpec("interactive input"))
        );

        let mut candidate = spec();
        candidate.mode = Mode::Interactive;
        candidate.access.attach = Some(demand("attach"));
        candidate.terminal = TerminalSpec {
            transcript: Transcript::OnReturn,
            max_transcript_bytes: 4_096,
            max_live_input_bytes: 1_024,
        };
        candidate.resume = Resume::Never;
        assert_eq!(candidate.validate(), Ok(()));
        assert_eq!(
            Spec::decode_canonical(&candidate.encode().unwrap()),
            Ok(candidate.clone())
        );

        candidate.mode = Mode::Unary;
        assert_eq!(
            candidate.validate(),
            Err(Invalid::InvalidSpec("additional input mode"))
        );

        let mut candidate = spec();
        candidate.terminal = TerminalSpec {
            transcript: Transcript::Never,
            max_transcript_bytes: 1,
            max_live_input_bytes: 0,
        };
        assert_eq!(candidate.validate(), Err(Invalid::InvalidSpec("terminal")));

        let mut candidate = spec();
        candidate.resume = Resume::Replay { commands: 65 };
        assert_eq!(
            candidate.validate(),
            Err(Invalid::InvalidSpec("replay commands"))
        );

        let mut candidate = spec();
        candidate.limits.wall_millis = u64::MAX;
        assert_eq!(candidate.validate(), Err(Invalid::InvalidSpec("limits")));

        let mut candidate = spec();
        candidate.resume = Resume::Checkpoint {
            codec: schema("checkpoint", 1),
        };
        assert_eq!(
            candidate.validate(),
            Err(Invalid::InvalidSpec("checkpoint resume"))
        );
    }

    #[test]
    fn find_grants_are_valid_sorted_duplicate_free_contracts() {
        let mut candidate = spec();
        candidate.queries = vec![grant("z"), grant("a")];
        assert_eq!(candidate.validate(), Err(Invalid::InvalidSpec("queries")));

        let mut candidate = spec();
        candidate.queries = vec![grant("a"), grant("a")];
        assert_eq!(candidate.validate(), Err(Invalid::InvalidSpec("queries")));

        let mut candidate = spec();
        candidate.queries[0].bound.wall_millis = 0;
        assert_eq!(candidate.validate(), Err(Invalid::InvalidSpec("query")));

        let candidate = spec();
        assert_eq!(
            candidate.validate_with_find(&[find_schema("records")]),
            Ok(())
        );
        assert_eq!(
            candidate.validate_with_find(&[find_schema("other")]),
            Err(Invalid::InvalidSpec("query declaration"))
        );
    }

    #[test]
    fn service_roles_links_and_readiness_are_bounded_and_canonical() {
        let roles = vec![
            RoleSpec {
                name: SchemaId::parse("entry").unwrap(),
                spec: schema("entry.run", 1),
            },
            RoleSpec {
                name: SchemaId::parse("stage").unwrap(),
                spec: schema("stage.run", 1),
            },
        ];
        let link = LinkSpec {
            name: SchemaId::parse("activation").unwrap(),
            from: SchemaId::parse("entry").unwrap(),
            to: SchemaId::parse("stage").unwrap(),
            codec: schema("activation", 1),
            send: Send::Stream,
            rank: RankRule::Cap(10),
            max_messages: 128,
            max_bytes: 1_048_576,
        };
        let mut candidate = spec();
        candidate.service = Some(ServiceSpec::Set {
            roles: roles.clone(),
            links: vec![link.clone()],
            ready: ReadyRule::All,
        });
        assert_eq!(candidate.validate(), Ok(()));

        if let Some(ServiceSpec::Set { roles, .. }) = &mut candidate.service {
            roles.swap(0, 1);
        }
        assert_eq!(
            candidate.validate(),
            Err(Invalid::InvalidSpec("service roles"))
        );

        let mut candidate = spec();
        let mut bad = link;
        bad.to = SchemaId::parse("missing").unwrap();
        candidate.service = Some(ServiceSpec::Set {
            roles,
            links: vec![bad],
            ready: ReadyRule::All,
        });
        assert_eq!(
            candidate.validate(),
            Err(Invalid::InvalidSpec("service link role"))
        );
    }

    #[test]
    fn build_identity_envelope_and_canonical_bytes_are_frozen() {
        let build = build();
        let bytes = build.encode().unwrap();
        assert_eq!(Build::decode_canonical(&bytes), Ok(build.clone()));
        assert_eq!(build.derived_id(), Ok(build.id));
        assert_eq!(
            data_encoding::HEXLOWER.encode(&build.id.as_bytes()),
            "33990566ebbc81f41f6e57013f65d68ba4843891521820aca4b7cf1259b4c166"
        );
        assert_eq!(
            blake3::hash(&bytes).to_hex().as_str(),
            "ab4c9e19b357a4105e70f495373b84278df1dc90f55199963cfde2f7a003307a"
        );
    }

    #[test]
    fn build_material_and_signature_tampering_are_distinct_failures() {
        let mut material = build();
        material.world_build[0] ^= 1;
        assert_eq!(material.validate(), Err(Invalid::BuildIdMismatch));

        let mut envelope = build();
        envelope.signature.bytes[0] ^= 1;
        assert_eq!(envelope.validate(), Err(Invalid::BadBuildSignature));

        let mut algorithm = build();
        algorithm.signature.algorithm = SIGNATURE_ED25519 + 1;
        assert_eq!(
            algorithm.validate(),
            Err(Invalid::UnsupportedSignatureAlgorithm(
                SIGNATURE_ED25519 + 1
            ))
        );
    }

    #[test]
    fn build_lists_and_resume_artifacts_are_bounded_and_canonical() {
        let mut duplicate_config = build();
        duplicate_config.config = vec![content(1), content(1)];
        assert_eq!(
            duplicate_config.derived_id(),
            Err(Invalid::InvalidBuild("config"))
        );

        let mut unordered_config = build();
        unordered_config.config = vec![content(2), content(1)];
        assert_eq!(
            unordered_config.derived_id(),
            Err(Invalid::InvalidBuild("config"))
        );

        let mut excessive_config = build();
        excessive_config.config = (0..=MAX_CONFIG_REFS_PER_BUILD)
            .map(|index| indexed_content(u16::try_from(index).unwrap()))
            .collect();
        assert_eq!(
            excessive_config.derived_id(),
            Err(Invalid::InvalidBuild("config"))
        );

        let mut duplicate_compatibility = build();
        duplicate_compatibility.compatible_from =
            vec![BuildId::from_bytes([1; 32]), BuildId::from_bytes([1; 32])];
        assert_eq!(
            duplicate_compatibility.derived_id(),
            Err(Invalid::InvalidBuild("compatible builds"))
        );

        let mut excessive_compatibility = build();
        excessive_compatibility.compatible_from = (0..=MAX_COMPATIBLE_BUILDS)
            .map(|index| {
                let mut raw = [0; 32];
                raw[30..].copy_from_slice(&u16::try_from(index).unwrap().to_be_bytes());
                BuildId::from_bytes(raw)
            })
            .collect();
        assert_eq!(
            excessive_compatibility.derived_id(),
            Err(Invalid::InvalidBuild("compatible builds"))
        );

        let mut both_resume_artifacts = build();
        both_resume_artifacts.replay_commands = Some(1);
        assert_eq!(
            both_resume_artifacts.derived_id(),
            Err(Invalid::InvalidBuild("resume artifacts"))
        );

        let mut zero_replay = build();
        zero_replay.checkpoint = None;
        zero_replay.replay_commands = Some(0);
        assert_eq!(
            zero_replay.derived_id(),
            Err(Invalid::InvalidBuild("replay commands"))
        );

        let mut excessive_replay = build();
        excessive_replay.checkpoint = None;
        excessive_replay.replay_commands = Some(MAX_REPLAY_COMMANDS + 1);
        assert_eq!(
            excessive_replay.derived_id(),
            Err(Invalid::InvalidBuild("replay commands"))
        );
    }

    #[test]
    fn publisher_attestation_does_not_change_build_identity() {
        let first = build();
        let mut second = first.clone();
        second.publisher = ActorId::from_incept_hash(&"b".repeat(64));
        second = second.sign(&[0x43; 32]).unwrap();

        assert_eq!(first.id, second.id);
        assert_ne!(first.publisher, second.publisher);
        assert_ne!(first.signature, second.signature);
        assert_ne!(first.encode().unwrap(), second.encode().unwrap());
        assert_eq!(first.validate(), Ok(()));
        assert_eq!(second.validate(), Ok(()));
    }

    #[test]
    fn build_decoder_rejects_unknown_trailing_and_oversized_bytes() {
        let bytes = build().encode().unwrap();

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            Build::decode_canonical(&trailing),
            Err(Invalid::NonCanonical)
        );

        let mut unknown = bytes;
        unknown[0] = BUILD_VERSION + 1;
        assert_eq!(
            Build::decode_canonical(&unknown),
            Err(Invalid::UnsupportedVersion(BUILD_VERSION + 1))
        );
        assert_eq!(
            Build::decode_canonical(&vec![0; MAX_BUILD_BYTES + 1]),
            Err(Invalid::TooLarge)
        );
    }

    #[test]
    fn start_and_try_tags_field_order_and_bytes_are_frozen() {
        // Generation-2 Start bytes: carries the optional `target` Station.
        let start = Cmd::Start(start());
        let start_bytes = start.encode().unwrap();
        assert_eq!(start_bytes[0], 0);
        assert_eq!(Cmd::decode_canonical(&start_bytes), Ok(start));
        assert_eq!(
            blake3::hash(&start_bytes).to_hex().as_str(),
            "d8b207a3f9c5a38ad57ba7c77c800f30506b62b4df53488a5893c70ecbc47e84"
        );

        let intent = Cmd::Try(try_intent());
        let intent_bytes = intent.encode().unwrap();
        assert_eq!(intent_bytes[0], 1);
        assert_eq!(Cmd::decode_canonical(&intent_bytes), Ok(intent));
        assert_eq!(
            blake3::hash(&intent_bytes).to_hex().as_str(),
            "23fd0fc22e23358e94776ff5b8e034eaeb63b2d13c809c54fb97c667dcb0b705"
        );
    }

    #[test]
    fn start_proves_exact_selection_and_find_grant_containment() {
        let start = start();
        let mut declaration = spec();
        declaration.service = Some(ServiceSpec::Warm {
            role: RoleSpec {
                name: SchemaId::parse("worker").unwrap(),
                spec: schema("check", 1),
            },
            max_runs: 8,
        });
        assert_eq!(start.validate_with(&declaration, &build()), Ok(()));

        let mut wrong_build = start.clone();
        wrong_build.build = BuildId::from_bytes([0x99; 32]);
        assert_eq!(
            wrong_build.validate_with(&declaration, &build()),
            Err(Invalid::InvalidStart("selection"))
        );

        let mut widened = start;
        widened.queries[0].grant.bound.wall_millis = 2;
        assert_eq!(
            widened.validate_with(&declaration, &build()),
            Err(Invalid::InvalidStart("query widening"))
        );
    }

    #[test]
    fn start_and_try_reject_unbounded_or_contradictory_intent() {
        let mut input = start();
        input.input.content.clear();
        assert_eq!(input.validate(), Err(Invalid::InvalidStart("input")));

        let mut resources = start();
        resources.resources[0].amount = u64::MAX;
        assert_eq!(
            resources.validate(),
            Err(Invalid::InvalidStart("resources"))
        );

        let mut unbounded = try_intent();
        unbounded.limits.wall_millis = u64::MAX;
        assert_eq!(unbounded.validate(), Err(Invalid::InvalidTry("limits")));

        let mut no_activation = try_intent();
        no_activation
            .offer
            .as_mut()
            .expect("test offer")
            .station_epoch = StationEpoch::ZERO;
        assert_eq!(no_activation.validate(), Err(Invalid::InvalidTry("offer")));

        let mut station_only = try_intent();
        station_only.offer = None;
        station_only.enforcement = None;
        assert_eq!(station_only.validate(), Ok(()));

        let mut cross_build_checkpoint = try_intent();
        cross_build_checkpoint.checkpoint = Some(CheckpointRef {
            content: content(0x70),
            build: BuildId::from_bytes([0x71; 32]),
            sequence: 1,
        });
        assert_eq!(
            cross_build_checkpoint.validate(),
            Err(Invalid::InvalidTry("checkpoint"))
        );

        let mut unsupported_checkpoint = try_intent();
        unsupported_checkpoint.checkpoint = Some(CheckpointRef {
            content: content(0x70),
            build: unsupported_checkpoint.build,
            sequence: 1,
        });
        assert_eq!(
            unsupported_checkpoint.validate(),
            Err(Invalid::InvalidTry("checkpoint"))
        );

        let mut malformed_wire = try_intent();
        malformed_wire.limits.wall_millis = u64::MAX;
        let bytes = postcard::to_stdvec(&WireCmd::Try(malformed_wire)).unwrap();
        assert_eq!(
            Cmd::decode_canonical(&bytes),
            Err(Invalid::InvalidTry("limits"))
        );

        let mut continuation = try_intent();
        continuation.continuation = Some(ContinuationInput {
            manifest_root: [0x72; 32],
            input: Input {
                inline: b"next".to_vec(),
                content: Vec::new(),
                content_bytes: 0,
            },
        });
        assert_eq!(continuation.validate(), Ok(()));
        let command = Cmd::Try(continuation.clone());
        assert_eq!(
            Cmd::decode_canonical(&command.encode().unwrap()),
            Ok(command)
        );

        continuation.continuation.as_mut().unwrap().manifest_root = [0; 32];
        assert_eq!(
            continuation.validate(),
            Err(Invalid::InvalidTry("continuation root"))
        );
    }

    #[test]
    fn control_command_tags_and_field_order_are_frozen() {
        let checkpoint = ContentRef {
            content_id: [0x33; 32],
        };
        let cases = [
            (
                Cmd::Cancel { run: run(0x10) },
                [vec![2], vec![0x10; 16]].concat(),
            ),
            (
                Cmd::Retry { run: run(0x11) },
                [vec![3], vec![0x11; 16]].concat(),
            ),
            (
                Cmd::Resume {
                    run: run(0x12),
                    checkpoint,
                },
                [vec![4], vec![0x12; 16], vec![0x33; 32]].concat(),
            ),
            (
                Cmd::Accept {
                    run: run(0x13),
                    attempt: attempt(0x23),
                },
                [vec![5], vec![0x13; 16], vec![0x23; 16]].concat(),
            ),
            (
                Cmd::Reject {
                    run: run(0x14),
                    attempt: attempt(0x24),
                },
                [vec![6], vec![0x14; 16], vec![0x24; 16]].concat(),
            ),
            (
                Cmd::Drain {
                    service: service(0x15),
                },
                [vec![7], vec![0x15; 16]].concat(),
            ),
        ];

        for (command, expected) in cases {
            assert_eq!(command.encode(), Ok(expected.clone()));
            assert_eq!(Cmd::decode_canonical(&expected), Ok(command));
        }
    }

    #[test]
    fn truncated_unknown_trailing_and_oversized_commands_reject() {
        assert_eq!(Cmd::decode_canonical(&[0]), Err(Invalid::NonCanonical));
        assert_eq!(Cmd::decode_canonical(&[1]), Err(Invalid::NonCanonical));
        assert_eq!(Cmd::decode_canonical(&[8]), Err(Invalid::NonCanonical));

        let mut trailing = Cmd::Cancel { run: run(1) }.encode().unwrap();
        trailing.push(0);
        assert_eq!(Cmd::decode_canonical(&trailing), Err(Invalid::NonCanonical));
        assert_eq!(
            Cmd::decode_canonical(&vec![0; MAX_CMD_BYTES + 1]),
            Err(Invalid::TooLarge)
        );
    }

    #[test]
    fn offer_envelope_identity_and_bytes_are_frozen() {
        let signed = offer();
        assert_eq!(signed.validate(), Ok(()));
        assert_eq!(signed.usable_at(signed.expiry - 1), Ok(()));
        let bytes = signed.encode().unwrap();
        assert_eq!(bytes[0], OFFER_VERSION);
        assert_eq!(Offer::decode_canonical(&bytes), Ok(signed.clone()));
        assert_eq!(
            blake3::hash(&bytes).to_hex().as_str(),
            "901009fa9344aa884d4704a380f68aacb0e26054321edacd4f38a0af42535fce"
        );

        let mut republished = signed.clone();
        republished.publisher = ActorId::from_incept_hash(&"b".repeat(64));
        republished = republished.sign(&[0x64; 32]).unwrap();
        assert_eq!(signed.id, republished.id);
        assert_ne!(signed.publisher, republished.publisher);
        assert_ne!(signed.signature, republished.signature);
        assert_ne!(signed.encode().unwrap(), republished.encode().unwrap());
    }

    #[test]
    fn offer_rejects_impossible_or_expired_news() {
        let signed = offer();
        assert_eq!(
            signed.usable_at(signed.expiry),
            Err(Invalid::InvalidOffer("expiry"))
        );

        let mut zero_expiry = offer();
        zero_expiry.expiry = 0;
        assert_eq!(
            zero_expiry.derived_id(),
            Err(Invalid::InvalidOffer("expiry"))
        );

        let mut stale = offer();
        stale.expiry = 2;
        stale = stale.sign(&[0x63; 32]).unwrap();
        assert_eq!(stale.validate(), Ok(()));
        assert_eq!(stale.usable_at(1), Ok(()));
        assert_eq!(stale.usable_at(2), Err(Invalid::InvalidOffer("expiry")));

        let mut no_builds = offer();
        no_builds.builds.clear();
        assert_eq!(no_builds.derived_id(), Err(Invalid::InvalidOffer("builds")));

        let mut unsorted = offer();
        unsorted.builds = vec![
            OfferedBuild {
                id: BuildId::from_bytes([0x02; 32]),
                spec: schema("check", 1),
            },
            OfferedBuild {
                id: BuildId::from_bytes([0x01; 32]),
                spec: schema("check", 1),
            },
        ];
        assert_eq!(unsorted.derived_id(), Err(Invalid::InvalidOffer("builds")));

        let mut catalog_path = offer();
        catalog_path.resident = vec![content(0x82), content(0x81)];
        assert_eq!(
            catalog_path.derived_id(),
            Err(Invalid::InvalidOffer("resident"))
        );

        let mut unknown = signed.encode().unwrap();
        unknown[0] = OFFER_VERSION + 1;
        assert_eq!(
            Offer::decode_canonical(&unknown),
            Err(Invalid::UnsupportedVersion(OFFER_VERSION + 1))
        );
        assert_eq!(
            Offer::decode_canonical(&vec![0; MAX_OFFER_BYTES + 1]),
            Err(Invalid::TooLarge)
        );
    }

    #[test]
    fn try_treats_offer_as_optional_evidence_not_ownership() {
        let signed = offer();
        let mut intent = try_intent();
        intent.offer = Some(signed.reference());
        intent.build = signed.builds[0].id;
        assert_eq!(intent.validate_with_offer(&signed), Ok(()));

        let mut other_station = signed.clone();
        other_station.station = StationKey::from_key_bytes([0x99; 32]);
        other_station = other_station.sign(&[0x63; 32]).unwrap();
        assert_eq!(
            intent.validate_with_offer(&other_station),
            Err(Invalid::InvalidTry("offer"))
        );

        let mut other_build = intent.clone();
        other_build.build = BuildId::from_bytes([0x77; 32]);
        assert_eq!(
            other_build.validate_with_offer(&signed),
            Err(Invalid::InvalidTry("build"))
        );

        let mut station_only = intent;
        station_only.offer = None;
        station_only.enforcement = None;
        assert_eq!(station_only.validate(), Ok(()));
        assert_eq!(
            station_only.validate_with_offer(&signed),
            Err(Invalid::InvalidTry("offer"))
        );
        assert_eq!(try_intent().validate(), Ok(()));
    }

    fn challenge_for(offer: &Offer, nonce: u8, issued_at: u64, expiry: u64) -> Challenge {
        Challenge {
            offer: offer.id,
            nonce: [nonce; 16],
            station: offer.station.clone(),
            station_epoch: offer.station_epoch,
            issued_at,
            expiry,
        }
    }

    #[test]
    fn ready_answers_one_challenge_and_not_another() {
        let signed = offer();
        let challenge = challenge_for(&signed, 0x21, 1, 1_000);
        let ready = Ready::sign(&challenge, &[0x63; 32]).unwrap();
        assert_eq!(ready.validate_for(&challenge), Ok(()));
        assert_eq!(challenge.usable_at(1), Ok(()));
        assert_eq!(
            challenge.usable_at(1_000),
            Err(Invalid::InvalidChallenge("expiry"))
        );

        let other = challenge_for(&signed, 0x22, 1, 1_000);
        assert_eq!(
            ready.validate_for(&other),
            Err(Invalid::InvalidReady("challenge"))
        );

        let mut zero = challenge.clone();
        zero.nonce = [0; 16];
        assert_eq!(zero.validate(), Err(Invalid::InvalidChallenge("nonce")));
    }
}
