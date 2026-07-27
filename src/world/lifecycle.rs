//! Issues product formation and adoption over the orbital lifecycle.
//!
//! The semantic package remains pure. This outer application adapter owns
//! filesystem placement, founder policy bootstrap, signed initialization
//! persistence, and join materialization.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use runtime::{ActivationOptions, EnterOptions, Runtime, SpaceFormationOptions, WorldRegistry};

use crate::orbital::{
    discover_space_id, orbital_store_root, unsupported_store_at, OrbitalMechanics,
};

/// The issues Runtime registry used by formation and direct adoption helpers.
fn issues_registry() -> Result<WorldRegistry> {
    crate::world::packages()
        .build()
        .map(|(registry, _)| registry)
        .map_err(|e| anyhow::anyhow!("world registry: {e:?}"))
}

/// The reviewed IssuesWorld implementation id this build ships — the authority
/// identity the founder activates and every product transaction pins.
pub fn issues_implementation_id() -> [u8; 32] {
    crate::world::implementation_id()
}

/// The founder product-authority bootstrap: activate the IssuesWorld
/// implementation and grant the founder the Space capabilities. Idempotent —
/// an exact replay changes nothing (both the activation and each grant are
/// idempotent through the authority ledger). Public so a caller forming a
/// Space directly through [`OrbitalMechanics::form`] can run the same
/// deterministic bootstrap the CLI composition root does.
pub fn seed_founder_policy(mechanics: &OrbitalMechanics) -> Result<()> {
    mechanics.activate_implementation(
        crate::world::contract::PRODUCT_WORLD,
        issues_implementation_id(),
    )?;
    for (i, (capability, resource)) in crate::world::contract::founder_capabilities()
        .into_iter()
        .enumerate()
    {
        mechanics.grant_self_capability(capability, resource, [i as u8; 16])?;
    }
    Ok(())
}

/// The persisted product-formation lifecycle record (plan M4): the complete
/// signed `InitializeTracker` action, written durably **before** submission
/// and replayed byte-for-byte after a crash. Formation never reconstructs the
/// action with a fresh timestamp, id, parent Manifest, or signature.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuesBootstrapRecord {
    pub version: u16,
    pub space: String,
    pub world_implementation: [u8; 32],
    pub request_id: [u8; 16],
    /// The canonical intent JSON the signed action carries.
    pub canonical_intent_bytes: Vec<u8>,
    /// The complete signed action (canonical postcard).
    pub signed_action: Vec<u8>,
    pub phase: BootstrapPhase,
}

/// The bootstrap lifecycle phase — monotonic, persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BootstrapPhase {
    /// The signed action is durable; submission may or may not have started.
    Recorded,
    /// The RequestReceipt and resulting Manifest are durable.
    Complete,
}

/// Injected failure points for the formation crash matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapFault {
    BeforeRecord,
    AfterRecord,
    BeforeSubmit,
    BeforeComplete,
}

fn bootstrap_record_path(home: &Path, space: &crate::ids::SpaceId) -> PathBuf {
    orbital_store_root(home)
        .join(space.as_str())
        .join("issues-bootstrap.bin")
}

/// Write the bootstrap record durably: temp file, fsync, atomic rename.
fn write_bootstrap_record(
    home: &Path,
    space: &crate::ids::SpaceId,
    record: &IssuesBootstrapRecord,
) -> Result<()> {
    let path = bootstrap_record_path(home, space);
    let tmp = path.with_extension("bin.tmp");
    let bytes = postcard::to_stdvec(record)?;
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read the bootstrap record, if one is persisted.
pub fn read_bootstrap_record(
    home: &Path,
    space: &crate::ids::SpaceId,
) -> Option<IssuesBootstrapRecord> {
    let bytes = std::fs::read(bootstrap_record_path(home, space)).ok()?;
    postcard::from_bytes(&bytes).ok()
}

/// Found a fresh Space's full orbital footprint under `home` — the `lait init`
/// heir. Mints the mechanics material ([`OrbitalMechanics::form`]), runs the
/// founder product-authority bootstrap, then submits ONE persisted, signed,
/// crash-resumable `InitializeTracker` action that atomically creates the
/// deterministic Catalog, the built-in roles, the default workflow, and the
/// initial project. Re-running after any interruption resumes: the exact same
/// signed bytes are replayed, never a reconstructed action.
pub fn form_space(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
) -> Result<(OrbitalMechanics, runtime::SignedCoordinates)> {
    form_space_with_fault(home, device_seed, display_name, None, None)
}

/// [`form_space`] with an injected failure point and an explicit initial
/// project `(name, key)` — the crash-matrix seam. Public for the formation
/// gates; production callers pass `None`.
#[doc(hidden)]
pub fn form_space_with_fault(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
    project: Option<(String, String)>,
    fault: Option<BootstrapFault>,
) -> Result<(OrbitalMechanics, runtime::SignedCoordinates)> {
    if let Some(err) = unsupported_store_at(home) {
        return Err(anyhow::anyhow!("{err}"));
    }
    let root = orbital_store_root(home);
    // Resume-aware: a crashed formation left a Space store behind; open it
    // rather than forming a second one.
    let (mechanics, coords) = match discover_space_id(home) {
        Some(space) => {
            let mech = OrbitalMechanics::open(&root, &space, device_seed)?;
            let coords = mech.mint_coordinates(device_seed, display_name, vec![], None)?;
            (mech, coords)
        }
        None => OrbitalMechanics::form(&root, device_seed, display_name, vec![])?,
    };
    let space = mechanics.space();
    // The LAIT composition root's product-authority bootstrap: activate the
    // reviewed IssuesWorld implementation id and grant the founder the Space
    // capabilities, one deterministic idempotent step under the founder's
    // genesis policy-admin standing.
    seed_founder_policy(&mechanics)?;

    // Formation is offline and exclusive until the bootstrap completes.
    let rt = Runtime::open(
        root,
        issues_registry()?,
        Arc::new(mechanics.clone()),
        Arc::new(mechanics.clone()),
    );
    let orbit = rt
        .enter_orbit(&coords, EnterOptions)
        .map_err(|e| anyhow::anyhow!("materialize orbit: {e:?}"))?;
    let station = orbit
        .activate(ActivationOptions::offline())
        .map_err(|e| anyhow::anyhow!("activate: {e:?}"))?;
    let identity = Runtime::identity_from_seed(device_seed);
    let session = station
        .dock(&crate::world::contract::world_id(), &identity)
        .map_err(|e| anyhow::anyhow!("dock: {e:?}"))?;

    let record = match read_bootstrap_record(home, &space) {
        Some(record) => {
            if record.phase == BootstrapPhase::Complete {
                let _ = station.go_dormant();
                return Ok((mechanics, coords));
            }
            record
        }
        None => {
            // Capture the formation facts ONCE: display name, timestamp, the
            // initial project identity, and the golden commitments; build and
            // sign the one canonical InitializeTracker action; persist the
            // complete signed bytes BEFORE submission.
            if fault == Some(BootstrapFault::BeforeRecord) {
                let _ = station.go_dormant();
                return Err(anyhow::anyhow!("injected fault: before record write"));
            }
            let (project_name, project_key) = project.unwrap_or_else(|| {
                let name = if display_name.trim().is_empty() {
                    "Main".to_string()
                } else {
                    display_name.trim().to_string()
                };
                let key = crate::spaces::derive_project_key(&name);
                (name, key)
            });
            let project_id = crate::ids::ProjectId::mint(&crate::ids::SystemUlidSource)
                .as_str()
                .to_string();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(1)
                .max(1);
            let intent_payload = crate::world::contract::initialize_tracker_intent(
                display_name,
                now,
                &project_id,
                &project_name,
                &project_key,
                crate::crypto::device_from_seed(device_seed).as_str(),
            )
            .to_json();
            let request_id = runtime::RequestId::mint();
            let action = identity
                .sign_action(
                    &session,
                    request_id,
                    runtime::WorldIntent {
                        schema: crate::world::contract::issue_schema(),
                        schema_version: crate::world::contract::ISSUE_SCHEMA_VERSION,
                        payload: intent_payload.clone(),
                    },
                )
                .map_err(|e| anyhow::anyhow!("sign initialize-tracker: {e:?}"))?;
            let record = IssuesBootstrapRecord {
                version: 1,
                space: space.as_str().to_string(),
                world_implementation: issues_implementation_id(),
                request_id: request_id.as_bytes(),
                canonical_intent_bytes: intent_payload,
                signed_action: postcard::to_stdvec(&action)?,
                phase: BootstrapPhase::Recorded,
            };
            write_bootstrap_record(home, &space, &record)?;
            if fault == Some(BootstrapFault::AfterRecord) {
                let _ = station.go_dormant();
                return Err(anyhow::anyhow!("injected fault: after record write"));
            }
            record
        }
    };

    // Submit the EXACT persisted signed bytes (fresh build or crash replay).
    let action: runtime::SignedWorldAction = postcard::from_bytes(&record.signed_action)
        .map_err(|e| anyhow::anyhow!("bootstrap record corrupt: {e}"))?;
    if fault == Some(BootstrapFault::BeforeSubmit) {
        let _ = station.go_dormant();
        return Err(anyhow::anyhow!("injected fault: before submit"));
    }
    session
        .submit(action)
        .map_err(|e| anyhow::anyhow!("initialize-tracker: {e:?}"))?;
    if fault == Some(BootstrapFault::BeforeComplete) {
        let _ = station.go_dormant();
        return Err(anyhow::anyhow!("injected fault: before completion marking"));
    }
    // The RequestReceipt and resulting Manifest are durable: mark complete.
    let mut complete = record;
    complete.phase = BootstrapPhase::Complete;
    write_bootstrap_record(home, &space, &complete)?;
    let _ = station.go_dormant();
    let _ = SpaceFormationOptions::default(); // keep the type referenced
    Ok((mechanics, coords))
}

/// The `lait init` heir with first-run UX parity: [`form_space`] with a seeded
/// default project (named after the space, key derived) so `lait issues new` works on
/// the very next command. The project is part of the ONE atomic
/// `InitializeTracker` transaction. Returns the founded Space id and the
/// default project's brief.
pub fn found_space_cli(
    home: &Path,
    device_seed: &[u8; 32],
    display_name: &str,
) -> Result<(crate::ids::SpaceId, crate::spaces::ProjectBrief)> {
    let project_name = if display_name.trim().is_empty() {
        "Main".to_string()
    } else {
        display_name.trim().to_string()
    };
    let project_key = crate::spaces::derive_project_key(&project_name);
    let (mechanics, _coords) = form_space_with_fault(
        home,
        device_seed,
        display_name,
        Some((project_name.clone(), project_key.clone())),
        None,
    )?;
    Ok((
        mechanics.space(),
        crate::spaces::ProjectBrief {
            key: project_key,
            name: project_name,
        },
    ))
}

/// Bootstrap a joiner's full orbital footprint under `home` from an invite
/// link — the `lait join` store heir. Parses the founder-signed Coordinates,
/// mints the joiner's mechanics store ([`OrbitalMechanics::enter`]: genesis
/// adoption, self-inception held pending, admission stashed), then materializes
/// the Runtime Orbit store at the SAME Space by entering those Coordinates.
/// Returns the mechanics handle and the entered Space id. Admission itself is
/// redeemed later, over Contact, once the daemon is serving.
pub fn enter_space(
    home: &Path,
    device_seed: &[u8; 32],
    invite_link: &str,
) -> Result<(OrbitalMechanics, runtime::SignedCoordinates)> {
    if let Some(err) = unsupported_store_at(home) {
        return Err(anyhow::anyhow!("{err}"));
    }
    let coords = runtime::SignedCoordinates::parse_link(invite_link.trim())
        .map_err(|e| anyhow::anyhow!("invalid invite link: {e}"))?;
    let root = orbital_store_root(home);
    let mechanics = OrbitalMechanics::enter(&root, device_seed, &coords)?;
    // Materialize the Runtime Orbit store at the entered Space so the daemon can
    // acquire the Orbit and drive Contact/admission.
    let rt = Runtime::open(
        root,
        issues_registry()?,
        Arc::new(mechanics.clone()),
        Arc::new(mechanics.clone()),
    );
    rt.enter_orbit(&coords, EnterOptions)
        .map_err(|e| anyhow::anyhow!("materialize orbit: {e:?}"))?;
    Ok((mechanics, coords))
}
