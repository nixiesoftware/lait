//! Durable display coordinator state, split along the custody boundary.
//!
//! # Two stores, because there are two kinds of fact
//!
//! **Custody** is material that *authenticates*: the coordinator's identifier
//! key, and one symmetric proof key per enrolled receiver. Spending it is
//! speaking as the coordinator, so it is device-bound at rest and never leaves
//! this machine in the clear.
//!
//! **Standing** is *who may show what, where*: device labels and negotiated
//! capabilities, and the assignment records naming an Orbit, a Space, a World,
//! a surface contract, a controller, a coordinator actor, and a policy. Not one
//! field of it is a secret.
//!
//! These were one file until this split, and the cost was paid by standing.
//! `secretfs::Wrap::DeviceBound` — DPAPI on Windows — was applied to the whole
//! blob, so losing an operating-system profile destroyed every *assignment*
//! along with the keys, and `mechanics::custody` already names that failure:
//! a wrap treated as a durability boundary makes the OS profile "an accidental
//! founder, which nobody chose and nobody can audit". Standing had no reason to
//! be in that position.
//!
//! So the policy file is [`Wrap::Portable`] — owner-only on disk, readable
//! after a restore onto another machine or account. That is also what makes the
//! display Spec's v1 commitment real: *backup and export of non-secret display
//! configuration* is now copying one file, rather than a feature nobody wrote.
//!
//! # The boundary is in the API, not only on disk
//!
//! [`CoordinatorStore::snapshot`] answers with standing alone. A credential is
//! reachable only through [`CoordinatorStore::proof_key`] and
//! [`CoordinatorStore::identifier_key`], so no caller acquires key material
//! while reading policy, and every site that does spend a key says so.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use display_protocol::bounds::MAX_STATIC_DELAY_MS;
use display_protocol::ids::{DisplayAssignmentId, DisplayDeviceId, DisplayProgramId, ProofKey};
use display_protocol::program::{validate_sync_group, DisplaySyncMode, FreshnessPolicy};
use display_protocol::receiver::{validate_capabilities, ReceiverCapabilities};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use world_interface::display::{CanonicalDisplayInput, DisplaySurfaceId, DisplayTheme};

/// The one-file format this split replaced. Read once, at migration.
const LEGACY_STATE_VERSION: u32 = 1;
const STATE_VERSION: u32 = 2;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

const LEGACY_FILE: &str = "coordinator-state.json";
const POLICY_FILE: &str = "coordinator-policy.json";
const SECRETS_FILE: &str = "coordinator-secrets.json";

/// Standing for one enrolled receiver. Deliberately carries no credential —
/// the proof key lives in [`CoordinatorSecrets`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub version: u32,
    pub device: DisplayDeviceId,
    pub label: String,
    pub capabilities: ReceiverCapabilities,
    pub issued_at_unix_ms: u64,
    pub revoked_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceGrant {
    pub world: String,
    pub implementation: [u8; 32],
    pub surface: DisplaySurfaceId,
    pub surface_contract_version: u32,
    pub surface_contract_digest: [u8; 32],
    pub input: CanonicalDisplayInput,
    pub input_sha256: [u8; 32],
}

impl SourceGrant {
    pub fn new(
        world: String,
        implementation: [u8; 32],
        surface: DisplaySurfaceId,
        surface_contract_version: u32,
        surface_contract_digest: [u8; 32],
        input: CanonicalDisplayInput,
    ) -> Self {
        let input_sha256 = Sha256::digest(input.as_bytes()).into();
        Self {
            world,
            implementation,
            surface,
            surface_contract_version,
            surface_contract_digest,
            input,
            input_sha256,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentRecord {
    pub version: u32,
    pub id: DisplayAssignmentId,
    pub device: DisplayDeviceId,
    pub orbit: String,
    pub space: String,
    pub program: DisplayProgramId,
    pub source: SourceGrant,
    pub controller: String,
    pub coordinator_actor: String,
    pub protocol_major: u32,
    pub theme: DisplayTheme,
    pub freshness: FreshnessPolicy,
    #[serde(default)]
    pub sync: Option<AssignmentSync>,
    pub expires_at_unix_ms: Option<u64>,
    pub revoked_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentSync {
    pub group: String,
    pub mode: DisplaySyncMode,
    pub epoch_unix_ms: u64,
    pub static_delay_ms: i32,
}

/// Everything the coordinator may show, and to whom. No secret is
/// representable here, which is why this file is portable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorPolicy {
    pub version: u32,
    pub devices: BTreeMap<String, DeviceRecord>,
    pub assignments: BTreeMap<String, AssignmentRecord>,
}

impl CoordinatorPolicy {
    fn empty() -> Self {
        Self {
            version: STATE_VERSION,
            devices: BTreeMap::new(),
            assignments: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != STATE_VERSION {
            return Err(anyhow!("unsupported display coordinator policy version"));
        }
        for (key, device) in &self.devices {
            if key != device.device.as_str()
                || device.version != 1
                || validate_capabilities(&device.capabilities).is_err()
            {
                return Err(anyhow!("invalid display device record"));
            }
        }
        for (key, assignment) in &self.assignments {
            if key != assignment.id.as_str()
                || assignment.version != 1
                || assignment.protocol_major != display_protocol::PROTOCOL_MAJOR
                || assignment.source.input_sha256
                    != <[u8; 32]>::from(Sha256::digest(assignment.source.input.as_bytes()))
                || assignment.sync.as_ref().is_some_and(|sync| {
                    validate_sync_group(&sync.group).is_err()
                        || sync.epoch_unix_ms == 0
                        || !(-MAX_STATIC_DELAY_MS..=MAX_STATIC_DELAY_MS)
                            .contains(&sync.static_delay_ms)
                })
            {
                return Err(anyhow!("invalid display assignment record"));
            }
        }
        Ok(())
    }
}

/// Material that authenticates. Device-bound at rest; never portable, and
/// never handed out alongside policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorSecrets {
    pub version: u32,
    pub identifier_key: [u8; 32],
    pub proof_keys: BTreeMap<String, ProofKey>,
}

impl CoordinatorSecrets {
    fn new(identifier_key: [u8; 32]) -> Self {
        Self {
            version: STATE_VERSION,
            identifier_key,
            proof_keys: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != STATE_VERSION || self.identifier_key == [0; 32] {
            return Err(anyhow!(
                "unsupported or invalid display coordinator secrets"
            ));
        }
        Ok(())
    }
}

/// The pre-split single file, decoded only to be migrated out of.
#[derive(Deserialize)]
struct LegacyState {
    version: u32,
    identifier_key: [u8; 32],
    devices: BTreeMap<String, LegacyDeviceRecord>,
    assignments: BTreeMap<String, AssignmentRecord>,
}

#[derive(Deserialize)]
struct LegacyDeviceRecord {
    version: u32,
    device: DisplayDeviceId,
    label: String,
    proof_key: ProofKey,
    capabilities: ReceiverCapabilities,
    issued_at_unix_ms: u64,
    revoked_at_unix_ms: Option<u64>,
}

pub struct CoordinatorStore {
    policy_path: PathBuf,
    secrets_path: PathBuf,
    policy: Mutex<CoordinatorPolicy>,
    secrets: Mutex<CoordinatorSecrets>,
}

impl CoordinatorStore {
    pub fn open(root: &Path, identifier_key: [u8; 32]) -> Result<Self> {
        private_dir(root)?;
        let policy_path = root.join(POLICY_FILE);
        let secrets_path = root.join(SECRETS_FILE);
        let legacy_path = root.join(LEGACY_FILE);

        let (policy, secrets) = if policy_path.exists() || secrets_path.exists() {
            (
                read_json::<CoordinatorPolicy>(&policy_path)?
                    .unwrap_or_else(CoordinatorPolicy::empty),
                read_json::<CoordinatorSecrets>(&secrets_path)?
                    .unwrap_or_else(|| CoordinatorSecrets::new(identifier_key)),
            )
        } else if legacy_path.exists() {
            migrate_legacy(&legacy_path)?
        } else {
            (
                CoordinatorPolicy::empty(),
                CoordinatorSecrets::new(identifier_key),
            )
        };

        policy.validate()?;
        secrets.validate()?;

        let store = Self {
            policy_path,
            secrets_path,
            policy: Mutex::new(policy),
            secrets: Mutex::new(secrets),
        };

        // Commit a migrated pair before the legacy file is retired, so an
        // interrupted migration reopens from the legacy file rather than from
        // nothing.
        if legacy_path.exists() {
            store.flush_policy()?;
            store.flush_secrets()?;
            fs::remove_file(&legacy_path)
                .with_context(|| format!("retire {}", legacy_path.display()))?;
        }

        Ok(store)
    }

    /// Standing only. A credential is never reachable through this.
    pub fn snapshot(&self) -> Result<CoordinatorPolicy> {
        self.policy
            .lock()
            .map(|policy| policy.clone())
            .map_err(|_| anyhow!("display coordinator policy lock was poisoned"))
    }

    /// Custody: the key deriving assignment-bound item and asset identifiers.
    pub fn identifier_key(&self) -> Result<[u8; 32]> {
        self.secrets
            .lock()
            .map(|secrets| secrets.identifier_key)
            .map_err(|_| anyhow!("display coordinator secrets lock was poisoned"))
    }

    /// Custody: the symmetric credential shared with one receiver installation.
    pub fn proof_key(&self, device: &DisplayDeviceId) -> Result<Option<ProofKey>> {
        self.secrets
            .lock()
            .map(|secrets| secrets.proof_keys.get(device.as_str()).cloned())
            .map_err(|_| anyhow!("display coordinator secrets lock was poisoned"))
    }

    /// Enrol a receiver. The only method that mints custody, so the one place
    /// to audit for it. Standing is written last: a crash between the two
    /// leaves an unreferenced key, which is inert, rather than a device visible
    /// in policy that nothing can authenticate.
    pub fn enrol(&self, device: DeviceRecord, proof_key: ProofKey) -> Result<()> {
        let key = device.device.as_str().to_string();
        self.update_secrets(|secrets| {
            secrets.proof_keys.insert(key.clone(), proof_key);
        })?;
        self.update_policy(|policy| {
            policy.devices.insert(key.clone(), device);
        })
    }

    /// Amend standing for a receiver that is already enrolled — a negotiated
    /// capability, a label. It cannot enrol, because a device row with no key
    /// behind it would authenticate as nothing while reading as present.
    pub fn update_device(&self, device: DeviceRecord) -> Result<()> {
        let key = device.device.as_str().to_string();
        if self.proof_key(&device.device)?.is_none() {
            return Err(anyhow!("display device is not enrolled"));
        }
        self.update_policy(|policy| {
            policy.devices.insert(key, device);
        })
    }

    pub fn device(&self, device: &DisplayDeviceId) -> Result<Option<DeviceRecord>> {
        Ok(self.snapshot()?.devices.get(device.as_str()).cloned())
    }

    pub fn put_assignment(&self, assignment: AssignmentRecord) -> Result<()> {
        self.update_policy(|policy| {
            policy
                .assignments
                .insert(assignment.id.as_str().to_string(), assignment);
        })
    }

    pub fn replace_assignment_for_device(
        &self,
        assignment: AssignmentRecord,
        now_unix_ms: u64,
    ) -> Result<()> {
        self.update_policy(|policy| {
            for existing in policy.assignments.values_mut() {
                if existing.device == assignment.device && existing.revoked_at_unix_ms.is_none() {
                    existing.revoked_at_unix_ms = Some(now_unix_ms);
                }
            }
            policy
                .assignments
                .insert(assignment.id.as_str().to_string(), assignment);
        })
    }

    pub fn revoke_assignment(
        &self,
        assignment: &DisplayAssignmentId,
        now_unix_ms: u64,
    ) -> Result<bool> {
        let mut found = false;
        self.update_policy(|policy| {
            if let Some(record) = policy.assignments.get_mut(assignment.as_str()) {
                if record.revoked_at_unix_ms.is_none() {
                    record.revoked_at_unix_ms = Some(now_unix_ms);
                }
                found = true;
            }
        })?;
        Ok(found)
    }

    /// Revocation is a standing change, so the credential deliberately stays.
    /// `authorize` verifies the request *before* it reports `Revoked`, and a
    /// receiver holding a valid key must learn that its enrollment was revoked
    /// rather than that it was never enrolled — the two refusals are different
    /// facts and only one of them is actionable.
    pub fn revoke_device(&self, device: &DisplayDeviceId, now_unix_ms: u64) -> Result<bool> {
        let mut found = false;
        self.update_policy(|policy| {
            if let Some(record) = policy.devices.get_mut(device.as_str()) {
                if record.revoked_at_unix_ms.is_none() {
                    record.revoked_at_unix_ms = Some(now_unix_ms);
                }
                found = true;
            }
            for assignment in policy.assignments.values_mut() {
                if &assignment.device == device && assignment.revoked_at_unix_ms.is_none() {
                    assignment.revoked_at_unix_ms = Some(now_unix_ms);
                }
            }
        })?;
        Ok(found)
    }

    pub fn assignment_for_device(
        &self,
        device: &DisplayDeviceId,
    ) -> Result<Option<AssignmentRecord>> {
        let policy = self.snapshot()?;
        Ok(policy
            .assignments
            .values()
            .find(|assignment| {
                &assignment.device == device && assignment.revoked_at_unix_ms.is_none()
            })
            .cloned())
    }

    fn update_policy(&self, mutate: impl FnOnce(&mut CoordinatorPolicy)) -> Result<()> {
        let mut held = self
            .policy
            .lock()
            .map_err(|_| anyhow!("display coordinator policy lock was poisoned"))?;
        let mut next = held.clone();
        mutate(&mut next);
        next.validate()?;
        write_atomic(
            &self.policy_path,
            &serde_json::to_vec_pretty(&next)?,
            mechanics::secretfs::Wrap::Portable,
        )?;
        *held = next;
        Ok(())
    }

    fn update_secrets(&self, mutate: impl FnOnce(&mut CoordinatorSecrets)) -> Result<()> {
        let mut held = self
            .secrets
            .lock()
            .map_err(|_| anyhow!("display coordinator secrets lock was poisoned"))?;
        let mut next = held.clone();
        mutate(&mut next);
        next.validate()?;
        write_atomic(
            &self.secrets_path,
            &serde_json::to_vec_pretty(&next)?,
            mechanics::secretfs::Wrap::DeviceBound,
        )?;
        *held = next;
        Ok(())
    }

    fn flush_policy(&self) -> Result<()> {
        self.update_policy(|_| {})
    }

    fn flush_secrets(&self) -> Result<()> {
        self.update_secrets(|_| {})
    }
}

fn migrate_legacy(path: &Path) -> Result<(CoordinatorPolicy, CoordinatorSecrets)> {
    let legacy = read_json::<LegacyState>(path)?
        .ok_or_else(|| anyhow!("display coordinator state disappeared while opening"))?;
    if legacy.version != LEGACY_STATE_VERSION {
        return Err(anyhow!("unsupported display coordinator state version"));
    }

    let mut policy = CoordinatorPolicy::empty();
    let mut secrets = CoordinatorSecrets::new(legacy.identifier_key);
    for (key, device) in legacy.devices {
        secrets.proof_keys.insert(key.clone(), device.proof_key);
        policy.devices.insert(
            key,
            DeviceRecord {
                version: device.version,
                device: device.device,
                label: device.label,
                capabilities: device.capabilities,
                issued_at_unix_ms: device.issued_at_unix_ms,
                revoked_at_unix_ms: device.revoked_at_unix_ms,
            },
        );
    }
    policy.assignments = legacy.assignments;
    Ok((policy, secrets))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let metadata = fs::metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if metadata.len() > MAX_STATE_BYTES {
        return Err(anyhow!("display coordinator state exceeds its bound"));
    }
    let Some(bytes) = mechanics::secretfs::read_private(path).map_err(|error| anyhow!(error))?
    else {
        return Ok(None);
    };
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
        return Err(anyhow!("display coordinator state exceeds its bound"));
    }
    serde_json::from_slice::<T>(&bytes)
        .with_context(|| format!("decode {}", path.display()))
        .map(Some)
}

fn write_atomic(path: &Path, bytes: &[u8], wrap: mechanics::secretfs::Wrap) -> Result<()> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
        return Err(anyhow!("display coordinator state exceeds its bound"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("display coordinator state has no parent"))?;
    private_dir(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("display coordinator state has no file name"))?;
    let temporary = parent.join(format!("{file_name}.tmp"));
    mechanics::secretfs::write_private(
        &temporary,
        bytes,
        mechanics::secretfs::Create::Replace,
        wrap,
    )
    .with_context(|| format!("write {}", temporary.display()))?;
    atomic_replace(&temporary, path).with_context(|| format!("commit {}", path.display()))?;
    sync_dir(parent).with_context(|| format!("sync {}", parent.display()))?;
    Ok(())
}

fn private_dir(path: &Path) -> Result<()> {
    mechanics::secretfs::create_private_dir(path)
        .with_context(|| format!("protect {}", path.display()))
}

fn atomic_replace(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match mechanics::secretfs::persist_replace(temporary, destination) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
                }
            }
        }
    }
    last.map_or_else(|| Ok(()), Err)
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path).and_then(|directory| directory.sync_all())
}

#[cfg(windows)]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .or_else(|_| {
            OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
        });
    match directory {
        Ok(directory) => directory.sync_all(),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Millisecond wall-clock is not unique enough to separate tests that run
    /// in parallel in one process — they collided on the same directory and
    /// raced each other's temporary files. The counter is what makes it a
    /// per-test root rather than a per-millisecond one.
    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let root = std::env::temp_dir().join(format!(
            "lait-display-store-{}-{}-{}",
            std::process::id(),
            mechanics::wallclock::now_millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn device_id() -> DisplayDeviceId {
        DisplayDeviceId::parse("11".repeat(16)).unwrap()
    }

    fn proof() -> ProofKey {
        ProofKey::parse("22".repeat(32)).unwrap()
    }

    fn record(device: &DisplayDeviceId) -> DeviceRecord {
        DeviceRecord {
            version: 1,
            device: device.clone(),
            label: "Lobby".into(),
            capabilities: capabilities(),
            issued_at_unix_ms: 1,
            revoked_at_unix_ms: None,
        }
    }

    #[test]
    fn state_is_atomic_versioned_and_reopens() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32]).unwrap();
        let device = device_id();
        store.enrol(record(&device), proof()).unwrap();

        let reopened = CoordinatorStore::open(&root, [9; 32]).unwrap();
        assert_eq!(reopened.identifier_key().unwrap(), [7; 32]);
        assert!(reopened
            .snapshot()
            .unwrap()
            .devices
            .contains_key(device.as_str()));
        assert_eq!(reopened.proof_key(&device).unwrap(), Some(proof()));
        let _ = fs::remove_dir_all(root);
    }

    /// The reason the split exists: the policy file is portable, so a restore
    /// onto another machine or account keeps every assignment. If this ever
    /// reads as device-bound again, losing an OS profile destroys standing that
    /// was never secret.
    #[test]
    fn policy_is_portable_and_holds_no_credential() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32]).unwrap();
        let device = device_id();
        store.enrol(record(&device), proof()).unwrap();

        let raw = fs::read(root.join(POLICY_FILE)).unwrap();
        let text = String::from_utf8(raw).expect("policy is stored verbatim, not wrapped");
        assert!(
            text.contains("Lobby"),
            "standing is readable without a wrap"
        );
        assert!(
            !text.contains(&"22".repeat(32)),
            "no proof key may appear in the policy file"
        );
        assert!(
            !text.contains("proof_key") && !text.contains("identifier_key"),
            "no custody field may appear in the policy file"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A snapshot is standing. Acquiring a credential has to be a separate,
    /// named call, so no caller picks one up while reading policy.
    #[test]
    fn a_snapshot_cannot_yield_a_credential() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32]).unwrap();
        let device = device_id();
        store.enrol(record(&device), proof()).unwrap();

        let policy = store.snapshot().unwrap();
        let serialized = serde_json::to_string(&policy).unwrap();
        assert!(!serialized.contains(&"22".repeat(32)));
        assert!(!serialized.contains(&"07".repeat(32)));
        let _ = fs::remove_dir_all(root);
    }

    /// Revocation changes standing and leaves the credential in place, so
    /// `authorize` can still tell "revoked" from "never enrolled".
    #[test]
    fn revocation_keeps_the_credential_so_the_refusal_stays_honest() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32]).unwrap();
        let device = device_id();
        store.enrol(record(&device), proof()).unwrap();
        assert!(store.revoke_device(&device, 99).unwrap());

        assert_eq!(store.proof_key(&device).unwrap(), Some(proof()));
        assert!(store.device(&device).unwrap().unwrap().revoked_at_unix_ms == Some(99));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_legacy_single_file_migrates_into_the_split_pair() {
        let root = temp();
        private_dir(&root).unwrap();
        let device_key = "11".repeat(16);
        let mut devices = serde_json::Map::new();
        devices.insert(
            device_key.clone(),
            serde_json::json!({
                "version": 1,
                "device": device_key,
                "label": "Lobby",
                "proof_key": "22".repeat(32),
                "capabilities": capabilities(),
                "issued_at_unix_ms": 1,
                "revoked_at_unix_ms": null,
            }),
        );
        let legacy = serde_json::json!({
            "version": 1,
            "identifier_key": vec![7u8; 32],
            "devices": serde_json::Value::Object(devices),
            "assignments": serde_json::Map::new(),
        });
        write_atomic(
            &root.join(LEGACY_FILE),
            &serde_json::to_vec_pretty(&legacy).unwrap(),
            mechanics::secretfs::Wrap::DeviceBound,
        )
        .unwrap();

        let store = CoordinatorStore::open(&root, [1; 32]).unwrap();
        let device = device_id();
        assert_eq!(store.identifier_key().unwrap(), [7; 32]);
        assert_eq!(store.proof_key(&device).unwrap(), Some(proof()));
        assert_eq!(
            store.device(&device).unwrap().unwrap().label,
            "Lobby".to_string()
        );
        assert!(
            !root.join(LEGACY_FILE).exists(),
            "the legacy file is retired only after both halves commit"
        );
        assert!(root.join(POLICY_FILE).exists() && root.join(SECRETS_FILE).exists());
        let _ = fs::remove_dir_all(root);
    }

    fn capabilities() -> ReceiverCapabilities {
        use display_protocol::program::DisplayAssetMediaType;
        use display_protocol::receiver::{
            AccessibilityCapabilities, HealthGranularity, LatencyClass, PlaybackCapabilities,
            PlaybackTier, ReceiverPlatform, SyncClass, Viewport,
        };

        ReceiverCapabilities {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            platform: ReceiverPlatform::Desktop,
            build: "test".into(),
            viewport: Viewport {
                width: 1920,
                height: 1080,
                scale_milli: 1000,
            },
            image_types: vec![DisplayAssetMediaType::ImagePng],
            max_asset_bytes: 16 * 1024 * 1024,
            max_staged_bytes: 48 * 1024 * 1024,
            max_program_items: 16,
            max_staging_horizon_ms: 86_400_000,
            locale: "en-US".into(),
            accessibility: AccessibilityCapabilities {
                native_screen_reader: false,
                spoken_summary: true,
                captions: false,
                audio_description: false,
            },
            playback: PlaybackCapabilities {
                tier: PlaybackTier::Frame,
                sync_class: SyncClass::Boundary,
                rate_control_probed: false,
                latency_class: LatencyClass::Snapshot,
                health_granularity: HealthGranularity::Full,
            },
        }
    }
}
