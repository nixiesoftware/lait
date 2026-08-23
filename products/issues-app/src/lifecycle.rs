#![allow(
    clippy::expect_used,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "formation constants and tracker records are compile-time validated and bounded"
)]
//! Product-owned formation policy and crash-resumable tracker bootstrap.
//!
//! The orbital host forms or enters a Space, activates a Station, and supplies
//! a docked Session. This module owns every Issues-specific decision made
//! during that lifecycle: reviewed implementation identity, founder grants,
//! initial Catalog contents, and bootstrap persistence.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use issues::ids::{ProjectId, SpaceId, SystemUlidSource};
use runtime::{
    world::call::{IdentityAccess, SessionAccess},
    world::Intent,
    world::LifecycleSourceCoordinate,
    world::Query,
    world::RequestId,
    world::SignedWorldAction,
};

const UPGRADE_RECORD_VERSION: u16 = 1;
/// Host-side lifecycle storage enforces its own outer bound. This tighter
/// product bound prevents an opaque record from becoming a hidden payload
/// transport; one prepared V4Migrate action is only a few hundred bytes.
pub const MAX_UPGRADE_RECORD_BYTES: usize = 64 * 1_024;

/// Exact implementation coordinate understood by the product lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImplementationCoordinate {
    pub id: [u8; 32],
    pub version: u32,
}

/// Pure upgrade classification. `Direct` is reserved for a tracker with no
/// active implementation (new formation); an older active implementation must
/// pass through explicit launcher consent and the distinct migrator package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeAssessment {
    Current,
    Direct,
    ConsentRequired { migrator: ImplementationCoordinate },
    InProgress { migrator: ImplementationCoordinate },
    Unsupported { reason: String },
}

/// Resources supplied by the generic lifecycle host for one bounded step.
/// Opaque record placement and atomic persistence remain host-owned.
pub struct UpgradeContext<'a> {
    pub space: &'a SpaceId,
    pub session: &'a dyn SessionAccess,
    pub identity: &'a dyn IdentityAccess,
    pub device: &'a str,
    pub active: ImplementationCoordinate,
    pub migrator: ImplementationCoordinate,
    pub preferred: ImplementationCoordinate,
    pub source: &'a LifecycleSourceCoordinate,
    pub record: Option<&'a [u8]>,
}

/// One bounded step. The host persists `record` before scheduling another
/// call or activating preferred v4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeProgress {
    Pending {
        completed: u64,
        remaining: Option<u64>,
        record: Vec<u8>,
    },
    Verified {
        record: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum UpgradePhase {
    Prepared,
    Verified,
}

/// Product-authored, host-persisted migration checkpoint. `Prepared` is
/// intentionally a separate call from submission: a crash can only lose both
/// the record and the work, or replay the exact signed action idempotently.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpgradeRecord {
    version: u16,
    space: String,
    migrator: ImplementationCoordinate,
    preferred: ImplementationCoordinate,
    source: runtime::publication::PublicationId,
    source_frontier: replica::frontier::ReplicaFrontier,
    completed: u64,
    #[serde(default)]
    cursor: String,
    phase: UpgradePhase,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    signed_action: Vec<u8>,
}

pub fn preferred_implementation() -> ImplementationCoordinate {
    implementation_coordinate(issues::IssuesWorld::implementation_descriptor())
}

pub fn migrator_implementation() -> ImplementationCoordinate {
    implementation_coordinate(issues::IssuesWorld::migrator_implementation_descriptor())
}

fn implementation_coordinate(
    implementation: runtime::world::Implementation,
) -> ImplementationCoordinate {
    ImplementationCoordinate {
        id: implementation
            .id()
            .expect("canonical Issues implementation descriptor"),
        version: implementation.implementation_version,
    }
}

/// Decide whether this active tracker can move directly to preferred v4.
pub fn assess_upgrade(
    active: Option<ImplementationCoordinate>,
    preferred: ImplementationCoordinate,
) -> UpgradeAssessment {
    let compiled_preferred = preferred_implementation();
    let migrator = migrator_implementation();
    if preferred != compiled_preferred {
        return UpgradeAssessment::Unsupported {
            reason: "the lifecycle preferred coordinate is not this Issues build".into(),
        };
    }
    match active {
        None => UpgradeAssessment::Direct,
        Some(active) if active == compiled_preferred => UpgradeAssessment::Current,
        Some(active) if active == migrator => UpgradeAssessment::InProgress { migrator },
        Some(active) if active.version < compiled_preferred.version => {
            UpgradeAssessment::ConsentRequired { migrator }
        }
        Some(active) => UpgradeAssessment::Unsupported {
            reason: format!(
                "active Issues implementation v{} ({}) is not an upgrade source for v{} ({})",
                active.version,
                data_encoding::HEXLOWER.encode(&active.id[..8]),
                compiled_preferred.version,
                data_encoding::HEXLOWER.encode(&compiled_preferred.id[..8]),
            ),
        },
    }
}

/// Advance exactly one crash-idempotent migration step.
pub fn upgrade_step(context: UpgradeContext<'_>) -> Result<UpgradeProgress> {
    validate_upgrade_context(&context)?;
    let record = match context.record {
        Some(bytes) => decode_upgrade_record(bytes, &context)?,
        None => {
            let verification = migration_verification(context.session)?;
            if let Some(verification) = verification.as_ref().filter(|verification| {
                verification.source_publication == context.source.publication.publication
                    && verification.source_frontier == context.source.frontier
            }) {
                if verification.verified() {
                    let verified = verified_record(&context, verification);
                    return Ok(UpgradeProgress::Verified {
                        record: encode_upgrade_record(&verified)?,
                    });
                }
                if verification.marker_complete {
                    return Err(anyhow!(
                        "Issues migration cursor is complete but its bounded audit proof is not"
                    ));
                }
            }
            let (completed, cursor) =
                verification
                    .as_ref()
                    .map_or((0, String::new()), |verification| {
                        if verification.source_publication == context.source.publication.publication
                            && verification.source_frontier == context.source.frontier
                        {
                            (verification.batch, verification.cursor.clone())
                        } else {
                            // A fresh causal cut re-enumerates from its beginning,
                            // but audit batch ids remain globally monotonic.
                            (verification.batch, String::new())
                        }
                    });
            let record = prepare_upgrade_record(&context, completed, cursor)?;
            return pending(record);
        }
    };
    if record.phase == UpgradePhase::Verified {
        return Ok(UpgradeProgress::Verified {
            record: encode_upgrade_record(&record)?,
        });
    }

    let action = validate_prepared_action(&record, &context)?;
    context
        .session
        .submit_lifecycle_from(action, context.source.clone())
        .map_err(|error| anyhow!("submit Issues v4 migration batch: {error:?}"))?;
    let verification = migration_verification(context.session)?;
    let verification = verification
        .as_ref()
        .ok_or_else(|| anyhow!("Issues migrator committed no durable migration marker"))?;
    if verification.verified() {
        let verified = verified_record(&context, verification);
        return Ok(UpgradeProgress::Verified {
            record: encode_upgrade_record(&verified)?,
        });
    }
    if verification.marker_complete {
        return Err(anyhow!(
            "Issues migration cursor is complete but the structural audit is not"
        ));
    }
    let next = prepare_upgrade_record(&context, verification.batch, verification.cursor.clone())?;
    pending(next)
}

fn verified_record(
    context: &UpgradeContext<'_>,
    verification: &issues::contract::MigrationVerification,
) -> UpgradeRecord {
    UpgradeRecord {
        version: UPGRADE_RECORD_VERSION,
        space: context.space.as_str().into(),
        migrator: context.migrator,
        preferred: context.preferred,
        source: context.source.publication.publication,
        source_frontier: context.source.frontier,
        completed: verification.batch,
        cursor: verification.cursor.clone(),
        phase: UpgradePhase::Verified,
        signed_action: Vec::new(),
    }
}

fn pending(record: UpgradeRecord) -> Result<UpgradeProgress> {
    let completed = record.completed;
    Ok(UpgradeProgress::Pending {
        completed,
        remaining: None,
        record: encode_upgrade_record(&record)?,
    })
}

fn validate_upgrade_context(context: &UpgradeContext<'_>) -> Result<()> {
    let expected_migrator = migrator_implementation();
    let expected_preferred = preferred_implementation();
    if context.migrator != expected_migrator
        || context.active != expected_migrator
        || context.preferred != expected_preferred
        || context.session.space_id() != context.space
        || context.session.world_id() != &issues::contract::world_id()
        || context.identity.device().as_str() != context.device
        || context.source.publication.publication.implementation_digest != context.migrator.id
    {
        return Err(anyhow!(
            "Issues migration step was not bound to its exact migrator session"
        ));
    }
    Ok(())
}

fn prepare_upgrade_record(
    context: &UpgradeContext<'_>,
    completed: u64,
    cursor: String,
) -> Result<UpgradeRecord> {
    let ts = mechanics::wallclock::now_secs().max(1);
    let mut prepare = |ctx: &runtime::world::Context<'_>| {
        let plan =
            issues::IssuesWorld::prepare_v4_migration_plan(ctx, completed, cursor.clone(), ts)?;
        postcard::to_stdvec(&plan).map_err(|_| runtime::world::Rejection::ContractViolation)
    };
    let encoded = context
        .session
        .with_lifecycle_source(context.source, &mut prepare)
        .map_err(|error| anyhow!("open exact Issues migration source: {error:?}"))?
        .map_err(|error| anyhow!("prepare bounded Issues migration window: {error:?}"))?;
    let plan = postcard::from_bytes(&encoded)
        .map_err(|error| anyhow!("decode bounded Issues migration window: {error}"))?;
    let intent = issues::contract::IssueIntent::V4Migrate { plan };
    let action = context
        .identity
        .sign_action(
            context.session,
            RequestId::mint(),
            Intent {
                schema: issues::contract::issue_schema(),
                schema_version: issues::contract::ISSUE_SCHEMA_VERSION,
                payload: intent.to_json(),
            },
        )
        .map_err(|error| anyhow!("sign Issues v4 migration batch: {error:?}"))?;
    Ok(UpgradeRecord {
        version: UPGRADE_RECORD_VERSION,
        space: context.space.as_str().into(),
        migrator: context.migrator,
        preferred: context.preferred,
        source: context.source.publication.publication,
        source_frontier: context.source.frontier,
        completed,
        cursor,
        phase: UpgradePhase::Prepared,
        signed_action: action.encode(),
    })
}

fn validate_prepared_action(
    record: &UpgradeRecord,
    context: &UpgradeContext<'_>,
) -> Result<SignedWorldAction> {
    let action = SignedWorldAction::decode_canonical(&record.signed_action)
        .map_err(|error| anyhow!("Issues migration action is not canonical: {error}"))?;
    action
        .verify_self()
        .map_err(|error| anyhow!("Issues migration action signature is invalid: {error}"))?;
    if action.header.space != *context.space
        || action.header.world != issues::contract::world_id()
        || action.header.intent_schema != issues::contract::issue_schema()
        || action.header.intent_version != issues::contract::ISSUE_SCHEMA_VERSION
        || action.header.device.as_str() != context.device
        || !matches!(
            issues::contract::IssueIntent::from_json(&action.payload),
            Some(issues::contract::IssueIntent::V4Migrate { .. })
        )
    {
        return Err(anyhow!(
            "Issues migration record contains an action for a different coordinate"
        ));
    }
    Ok(action)
}

fn migration_verification(
    session: &dyn SessionAccess,
) -> Result<Option<issues::contract::MigrationVerification>> {
    let projection = session
        .query(Query {
            schema: issues::contract::issue_schema(),
            schema_version: issues::contract::ISSUE_SCHEMA_VERSION,
            payload: issues::contract::IssueQuery::V4MigrationStatus.to_json(),
            publication: None,
        })
        .map_err(|error| anyhow!("query Issues migration status: {error:?}"))?;
    serde_json::from_slice(&projection.bytes)
        .map_err(|error| anyhow!("decode Issues migration status: {error}"))
}

fn encode_upgrade_record(record: &UpgradeRecord) -> Result<Vec<u8>> {
    let bytes = postcard::to_stdvec(record)?;
    if bytes.len() > MAX_UPGRADE_RECORD_BYTES {
        return Err(anyhow!("Issues migration record exceeds its product bound"));
    }
    Ok(bytes)
}

fn decode_upgrade_record(bytes: &[u8], context: &UpgradeContext<'_>) -> Result<UpgradeRecord> {
    if bytes.len() > MAX_UPGRADE_RECORD_BYTES {
        return Err(anyhow!("Issues migration record exceeds its product bound"));
    }
    let record: UpgradeRecord = postcard::from_bytes(bytes)
        .map_err(|error| anyhow!("decode Issues migration record: {error}"))?;
    if postcard::to_stdvec(&record)? != bytes
        || record.version != UPGRADE_RECORD_VERSION
        || record.space != context.space.as_str()
        || record.migrator != context.migrator
        || record.preferred != context.preferred
        || record.source != context.source.publication.publication
        || record.source_frontier != context.source.frontier
        || record.cursor.len() > 512
        || (record.phase == UpgradePhase::Prepared && record.signed_action.is_empty())
        || (record.phase == UpgradePhase::Verified
            && (!record.signed_action.is_empty()
                || record.completed == 0
                || record.cursor.is_empty()))
    {
        return Err(anyhow!(
            "Issues migration record is inconsistent with this lifecycle"
        ));
    }
    Ok(record)
}

/// One deterministic founder grant supplied to the generic authority host.
pub struct FounderGrant {
    pub capability: mechanics::authorization::PolicyCapability,
    pub resource: mechanics::authorization::Resource,
    pub salt: [u8; 16],
}

/// The Issues policy a newly formed Space installs before its first World
/// transaction.
pub struct FounderPolicy {
    pub world: &'static str,
    pub implementation: [u8; 32],
    pub grants: Vec<FounderGrant>,
}

/// The reviewed IssuesWorld implementation id shipped by this package.
pub fn implementation_id() -> [u8; 32] {
    issues::IssuesWorld::implementation_descriptor()
        .id()
        .expect("canonical IssuesWorld descriptor")
}

/// Build the idempotent founder policy for this product.
pub fn founder_policy() -> FounderPolicy {
    FounderPolicy {
        world: issues::contract::PRODUCT_WORLD,
        implementation: implementation_id(),
        grants: issues::contract::founder_capabilities()
            .into_iter()
            .enumerate()
            .map(|(index, (capability, resource))| FounderGrant {
                capability,
                resource,
                salt: [index as u8; 16],
            })
            .collect(),
    }
}

/// The initial Issues project created atomically with the tracker Catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialProject {
    pub name: String,
    pub key: String,
}

impl InitialProject {
    pub fn for_space(display_name: &str) -> Self {
        let name = if display_name.trim().is_empty() {
            "Main".to_string()
        } else {
            display_name.trim().to_string()
        };
        let key = derive_project_key(&name);
        Self { name, key }
    }
}

/// Derive the default Issues project key from a display name.
pub fn derive_project_key(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|character: char| !character.is_ascii_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    let key: String = match words.len() {
        0 => "PRJ".to_string(),
        1 => words[0].chars().take(4).collect(),
        _ => words
            .iter()
            .take(4)
            .filter_map(|word| word.chars().next())
            .collect(),
    };
    key.to_ascii_uppercase()
}

/// The complete signed `InitializeTracker` action, durably persisted before
/// submission and replayed byte-for-byte after a crash.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuesBootstrapRecord {
    pub version: u16,
    pub space: String,
    pub world_implementation: [u8; 32],
    pub request_id: [u8; 16],
    pub canonical_intent_bytes: Vec<u8>,
    pub signed_action: Vec<u8>,
    pub phase: BootstrapPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BootstrapPhase {
    Recorded,
    Complete,
}

/// Failure points used only by the crate-local crash-resumption matrix.
#[cfg(feature = "fault-injection")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    BeforeRecord,
    AfterRecord,
    BeforeSubmit,
    BeforeComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    BeforeRecord,
    AfterRecord,
    BeforeSubmit,
    BeforeComplete,
}

#[cfg(feature = "fault-injection")]
impl From<Fault> for Point {
    fn from(fault: Fault) -> Self {
        match fault {
            Fault::BeforeRecord => Self::BeforeRecord,
            Fault::AfterRecord => Self::AfterRecord,
            Fault::BeforeSubmit => Self::BeforeSubmit,
            Fault::BeforeComplete => Self::BeforeComplete,
        }
    }
}

fn bootstrap_record_path(store_root: &Path, space: &SpaceId) -> PathBuf {
    store_root.join(space.as_str()).join("issues-bootstrap.bin")
}

fn write_bootstrap_record(
    store_root: &Path,
    space: &SpaceId,
    record: &IssuesBootstrapRecord,
) -> Result<()> {
    let path = bootstrap_record_path(store_root, space);
    let temporary = path.with_extension("bin.tmp");
    let bytes = postcard::to_stdvec(record)?;
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

/// Read the product bootstrap record beneath an orbital store root.
pub fn read_bootstrap_record(store_root: &Path, space: &SpaceId) -> Option<IssuesBootstrapRecord> {
    let bytes = std::fs::read(bootstrap_record_path(store_root, space)).ok()?;
    postcard::from_bytes(&bytes).ok()
}

/// Initialize the Issues World through a host-supplied docked Session.
///
/// This owns no Station or process lifecycle. It owns the product transaction
/// and its replay record, which is the part a different World must replace.
#[allow(clippy::too_many_arguments)]
pub fn bootstrap_tracker(
    store_root: &Path,
    space: &SpaceId,
    session: &dyn SessionAccess,
    identity: &dyn IdentityAccess,
    device: &str,
    display_name: &str,
    initial_project: Option<InitialProject>,
) -> Result<()> {
    bootstrap_tracker_inner(
        store_root,
        space,
        session,
        identity,
        device,
        display_name,
        initial_project,
        None,
    )
}

#[cfg(feature = "fault-injection")]
#[allow(clippy::too_many_arguments)]
pub fn bootstrap_tracker_with_fault(
    store_root: &Path,
    space: &SpaceId,
    session: &dyn SessionAccess,
    identity: &dyn IdentityAccess,
    device: &str,
    display_name: &str,
    initial_project: Option<InitialProject>,
    fault: Fault,
) -> Result<()> {
    bootstrap_tracker_inner(
        store_root,
        space,
        session,
        identity,
        device,
        display_name,
        initial_project,
        Some(fault.into()),
    )
}

#[allow(clippy::too_many_arguments)]
fn bootstrap_tracker_inner(
    store_root: &Path,
    space: &SpaceId,
    session: &dyn SessionAccess,
    identity: &dyn IdentityAccess,
    device: &str,
    display_name: &str,
    initial_project: Option<InitialProject>,
    fault: Option<Point>,
) -> Result<()> {
    let record = match read_bootstrap_record(store_root, space) {
        Some(record) => {
            if record.phase == BootstrapPhase::Complete {
                return Ok(());
            }
            record
        }
        None => {
            if fault == Some(Point::BeforeRecord) {
                return Err(anyhow!("injected fault: before record write"));
            }
            let project =
                initial_project.unwrap_or_else(|| InitialProject::for_space(display_name));
            let project_id = ProjectId::mint(&SystemUlidSource).as_str().to_string();
            // `.max(1)`: this feeds a tracker's creation stamp, and zero reads
            // as "unknown" downstream.
            let now = mechanics::wallclock::now_secs().max(1);
            let intent_payload = issues::contract::initialize_tracker_intent(
                display_name,
                now,
                &project_id,
                &project.name,
                &project.key,
                device,
            )
            .to_json();
            let request_id = RequestId::mint();
            let action = identity
                .sign_action(
                    session,
                    request_id,
                    Intent {
                        schema: issues::contract::issue_schema(),
                        schema_version: issues::contract::ISSUE_SCHEMA_VERSION,
                        payload: intent_payload.clone(),
                    },
                )
                .map_err(|error| anyhow!("sign initialize-tracker: {error:?}"))?;
            let record = IssuesBootstrapRecord {
                version: 1,
                space: space.as_str().to_string(),
                world_implementation: implementation_id(),
                request_id: request_id.as_bytes(),
                canonical_intent_bytes: intent_payload,
                signed_action: postcard::to_stdvec(&action)?,
                phase: BootstrapPhase::Recorded,
            };
            write_bootstrap_record(store_root, space, &record)?;
            if fault == Some(Point::AfterRecord) {
                return Err(anyhow!("injected fault: after record write"));
            }
            record
        }
    };

    let action: SignedWorldAction = postcard::from_bytes(&record.signed_action)
        .map_err(|error| anyhow!("bootstrap record corrupt: {error}"))?;
    if fault == Some(Point::BeforeSubmit) {
        return Err(anyhow!("injected fault: before submit"));
    }
    session
        .submit(action)
        .map_err(|error| anyhow!("initialize-tracker: {error:?}"))?;
    if fault == Some(Point::BeforeComplete) {
        return Err(anyhow!("injected fault: before completion marking"));
    }

    let mut complete = record;
    complete.phase = BootstrapPhase::Complete;
    write_bootstrap_record(store_root, space, &complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_key_policy_is_product_owned() {
        assert_eq!(derive_project_key("Engineering"), "ENGI");
        assert_eq!(derive_project_key("Customer Success"), "CS");
        assert_eq!(derive_project_key("123"), "PRJ");
    }

    #[test]
    fn new_tracker_forms_directly_on_preferred() {
        let preferred = preferred_implementation();
        assert_eq!(assess_upgrade(None, preferred), UpgradeAssessment::Direct);
        assert_eq!(implementation_id(), preferred.id);
        assert_ne!(preferred, migrator_implementation());
    }

    #[test]
    fn historical_tracker_requires_the_distinct_migrator() {
        let preferred = preferred_implementation();
        let historical = ImplementationCoordinate {
            id: [0x51; 32],
            version: preferred.version.saturating_sub(1),
        };
        assert_eq!(
            assess_upgrade(Some(historical), preferred),
            UpgradeAssessment::ConsentRequired {
                migrator: migrator_implementation(),
            }
        );
        assert_eq!(
            assess_upgrade(Some(migrator_implementation()), preferred),
            UpgradeAssessment::InProgress {
                migrator: migrator_implementation(),
            }
        );
    }
}
