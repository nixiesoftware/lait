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
    world::Intent, world::LocalIdentity, world::RequestId, world::SignedWorldAction, Session,
};

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
    session: &Session,
    identity: &LocalIdentity,
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
    session: &Session,
    identity: &LocalIdentity,
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
    session: &Session,
    identity: &LocalIdentity,
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
}
