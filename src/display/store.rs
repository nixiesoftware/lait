//! Durable display coordinator state.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use display_protocol::ids::{DisplayAssignmentId, DisplayDeviceId, DisplayProgramId, ProofKey};
use display_protocol::program::FreshnessPolicy;
use display_protocol::receiver::{validate_capabilities, ReceiverCapabilities};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use world_interface::display::{CanonicalDisplayInput, DisplaySurfaceId, DisplayTheme};

const STATE_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub version: u32,
    pub device: DisplayDeviceId,
    pub label: String,
    pub proof_key: ProofKey,
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
    pub expires_at_unix_ms: Option<u64>,
    pub revoked_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorState {
    pub version: u32,
    pub identifier_key: [u8; 32],
    pub devices: BTreeMap<String, DeviceRecord>,
    pub assignments: BTreeMap<String, AssignmentRecord>,
}

impl CoordinatorState {
    pub fn new(identifier_key: [u8; 32]) -> Self {
        Self {
            version: STATE_VERSION,
            identifier_key,
            devices: BTreeMap::new(),
            assignments: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != STATE_VERSION || self.identifier_key == [0; 32] {
            return Err(anyhow!("unsupported or invalid display coordinator state"));
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
            {
                return Err(anyhow!("invalid display assignment record"));
            }
        }
        Ok(())
    }
}

pub struct CoordinatorStore {
    path: PathBuf,
    state: Mutex<CoordinatorState>,
}

impl CoordinatorStore {
    pub fn open(root: &Path, identifier_key: [u8; 32]) -> Result<Self> {
        private_dir(root)?;
        let path = root.join("coordinator-state.json");
        let state = if path.exists() {
            let metadata =
                fs::metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
            if metadata.len() > MAX_STATE_BYTES {
                return Err(anyhow!("display coordinator state exceeds its bound"));
            }
            let bytes = mechanics::secretfs::read_private(&path)
                .map_err(|error| anyhow!(error))?
                .ok_or_else(|| anyhow!("display coordinator state disappeared while opening"))?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
                return Err(anyhow!("display coordinator state exceeds its bound"));
            }
            serde_json::from_slice::<CoordinatorState>(&bytes)
                .with_context(|| format!("decode {}", path.display()))?
        } else {
            CoordinatorState::new(identifier_key)
        };
        state.validate()?;
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn snapshot(&self) -> Result<CoordinatorState> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| anyhow!("display coordinator state lock was poisoned"))
    }

    pub fn put_device(&self, device: DeviceRecord) -> Result<()> {
        self.update(|state| {
            state
                .devices
                .insert(device.device.as_str().to_string(), device);
        })
    }

    pub fn device(&self, device: &DisplayDeviceId) -> Result<Option<DeviceRecord>> {
        Ok(self.snapshot()?.devices.get(device.as_str()).cloned())
    }

    pub fn put_assignment(&self, assignment: AssignmentRecord) -> Result<()> {
        self.update(|state| {
            state
                .assignments
                .insert(assignment.id.as_str().to_string(), assignment);
        })
    }

    pub fn replace_assignment_for_device(
        &self,
        assignment: AssignmentRecord,
        now_unix_ms: u64,
    ) -> Result<()> {
        self.update(|state| {
            for existing in state.assignments.values_mut() {
                if existing.device == assignment.device && existing.revoked_at_unix_ms.is_none() {
                    existing.revoked_at_unix_ms = Some(now_unix_ms);
                }
            }
            state
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
        self.update(|state| {
            if let Some(record) = state.assignments.get_mut(assignment.as_str()) {
                if record.revoked_at_unix_ms.is_none() {
                    record.revoked_at_unix_ms = Some(now_unix_ms);
                }
                found = true;
            }
        })?;
        Ok(found)
    }

    pub fn revoke_device(&self, device: &DisplayDeviceId, now_unix_ms: u64) -> Result<bool> {
        let mut found = false;
        self.update(|state| {
            if let Some(record) = state.devices.get_mut(device.as_str()) {
                if record.revoked_at_unix_ms.is_none() {
                    record.revoked_at_unix_ms = Some(now_unix_ms);
                }
                found = true;
            }
            for assignment in state.assignments.values_mut() {
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
        let state = self.snapshot()?;
        Ok(state
            .assignments
            .values()
            .find(|assignment| {
                &assignment.device == device && assignment.revoked_at_unix_ms.is_none()
            })
            .cloned())
    }

    fn update(&self, mutate: impl FnOnce(&mut CoordinatorState)) -> Result<()> {
        let mut held = self
            .state
            .lock()
            .map_err(|_| anyhow!("display coordinator state lock was poisoned"))?;
        let mut next = held.clone();
        mutate(&mut next);
        next.validate()?;
        write_atomic(&self.path, &serde_json::to_vec_pretty(&next)?)?;
        *held = next;
        Ok(())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
        return Err(anyhow!("display coordinator state exceeds its bound"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("display coordinator state has no parent"))?;
    private_dir(parent)?;
    let temporary = parent.join("coordinator-state.tmp");
    mechanics::secretfs::write_private(
        &temporary,
        bytes,
        mechanics::secretfs::Create::Replace,
        mechanics::secretfs::Wrap::DeviceBound,
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

    fn temp() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lait-display-store-{}-{}",
            std::process::id(),
            mechanics::wallclock::now_millis()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn state_is_atomic_versioned_and_reopens() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32]).unwrap();
        let device = DisplayDeviceId::parse("11".repeat(16)).unwrap();
        store
            .put_device(DeviceRecord {
                version: 1,
                device: device.clone(),
                label: "Lobby".into(),
                proof_key: ProofKey::parse("22".repeat(32)).unwrap(),
                capabilities: capabilities(),
                issued_at_unix_ms: 1,
                revoked_at_unix_ms: None,
            })
            .unwrap();
        let reopened = CoordinatorStore::open(&root, [9; 32]).unwrap();
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(snapshot.identifier_key, [7; 32]);
        assert!(snapshot.devices.contains_key(device.as_str()));
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
