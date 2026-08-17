//! Durable display coordinator state, split along the custody boundary.
//!
//! # Three files, because there are three kinds of fact
//!
//! **Standing** is *who may show what, where*: device labels and negotiated
//! capabilities, and the assignment records naming an Orbit, a Space, a World,
//! a surface contract, a controller, a coordinator actor, and a policy. Not one
//! field of it is a secret, so `coordinator-policy.json` is [`Wrap::Portable`]
//! — owner-only on disk, readable after a restore onto another machine or
//! account. That is what makes the display Spec's v1 commitment real: *backup
//! and export of non-secret display configuration* is copying one file, rather
//! than a feature nobody wrote.
//!
//! **A receiver credential** is one symmetric proof key per enrolled
//! installation. `coordinator-secrets.json` is [`Wrap::DeviceBound`], because
//! that material authenticates and re-pairing is already the display Spec's
//! recovery path for it.
//!
//! **The identifier key** is neither. It derives every assignment-bound item
//! and asset identifier, so losing it invalidates mappings receivers already
//! hold — it is *durability*, not merely secrecy, and a device-bound wrap is
//! the wrong boundary for durability. `coordinator-identifier.json` holds it as
//! a [`custody::Custodied`] envelope under several independent unlock paths and
//! is itself [`Wrap::Portable`], because its confidentiality comes from the
//! slots. See [`IdentifierCustody`].
//!
//! These were one file, device-bound, until two successive splits. Each time
//! the cost was paid by whichever fact did not belong there, and each time
//! `mechanics::custody` had already named the failure: a wrap treated as a
//! durability boundary makes the operating-system profile "an accidental
//! founder, which nobody chose and nobody can audit".
//!
//! # The boundary is in the API, not only on disk
//!
//! [`CoordinatorStore::snapshot`] answers with standing alone, and
//! [`CoordinatorStore::identifier_custody`] answers *how many ways in exist*
//! without opening one. A credential is reachable only through
//! [`CoordinatorStore::proof_key`] and [`CoordinatorStore::identifier_key`], so
//! no caller acquires key material while reading policy, and every site that
//! does spend a key says so.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use display_protocol::bounds::MAX_STATIC_DELAY_MS;
use display_protocol::ids::{DisplayAssignmentId, DisplayDeviceId, DisplayProgramId, ProofKey};
use display_protocol::program::{validate_sync_group, DisplaySyncMode, FreshnessPolicy};
use display_protocol::receiver::{validate_capabilities, ReceiverCapabilities};
use mechanics::authorization::custody;
use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use world_interface::display::{CanonicalDisplayInput, DisplaySurfaceId, DisplayTheme};

/// The one-file format this split replaced. Read once, at migration.
const LEGACY_STATE_VERSION: u32 = 1;
const STATE_VERSION: u32 = 2;
/// Secrets at version 2 still carried the identifier key inline; version 3
/// holds proof keys only, and the identifier moved to its own custody file.
const SECRETS_VERSION: u32 = 3;
const IDENTIFIER_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

const LEGACY_FILE: &str = "coordinator-state.json";
const POLICY_FILE: &str = "coordinator-policy.json";
const SECRETS_FILE: &str = "coordinator-secrets.json";
const IDENTIFIER_FILE: &str = "coordinator-identifier.json";

/// The purpose the identifier key is sealed under. Bound into the payload key,
/// so this envelope cannot be opened by a caller asking for some other secret.
const IDENTIFIER_PURPOSE: &str = "lait/display/identifier-key/1";

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

/// Material that authenticates one receiver. Device-bound at rest; never
/// portable, and never handed out alongside policy.
///
/// A proof key is deliberately *not* held under [`custody::Custodied`]: it is
/// shared with exactly one receiver installation, and the display Spec already
/// names re-pairing as its recovery path. Multiplying its unlock paths would
/// buy nothing and widen where a receiver credential can be read from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorSecrets {
    pub version: u32,
    pub proof_keys: BTreeMap<String, ProofKey>,
}

impl CoordinatorSecrets {
    fn empty() -> Self {
        Self {
            version: SECRETS_VERSION,
            proof_keys: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != SECRETS_VERSION {
            return Err(anyhow!(
                "unsupported or invalid display coordinator secrets"
            ));
        }
        Ok(())
    }
}

/// The identifier key's custody file.
///
/// # Why this is not in the secrets file
///
/// The key deriving every assignment-bound item and asset identifier had
/// exactly one unlock path — the operating-system profile, through
/// [`Wrap::DeviceBound`] on the secrets file — and losing it invalidates those
/// mappings rather than merely inconveniencing an operator. That is the shape
/// `mechanics::custody` exists to prevent, and putting a second slot *inside* a
/// device-bound file would fix nothing: the file would still be unreadable
/// without the profile that was lost.
///
/// So the envelope carries its own protection and the file is
/// [`Wrap::Portable`]. Confidentiality comes from the slots, which is what
/// makes a second unlock path mean something, and what makes export a copy
/// rather than a decryption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifierCustody {
    pub version: u32,
    pub held: custody::Custodied,
}

impl IdentifierCustody {
    fn validate(&self) -> Result<()> {
        if self.version != IDENTIFIER_VERSION {
            return Err(anyhow!("unsupported display identifier custody version"));
        }
        Ok(())
    }
}

/// Who can open the coordinator's identifier key on this machine.
///
/// The store never holds the seed behind [`Custodian::unlock`]; a caller
/// supplies it at each site that spends it, which is the same discipline the
/// proof-key accessor follows.
pub struct Custodian {
    /// The device a freshly minted envelope seals its first slot to.
    pub device: DeviceId,
    /// How this machine opens an envelope that already exists.
    pub unlock: custody::UnlockKey,
}

/// The pre-split single file, decoded only to be migrated out of.
#[derive(Deserialize)]
struct LegacyState {
    version: u32,
    identifier_key: [u8; 32],
    devices: BTreeMap<String, LegacyDeviceRecord>,
    assignments: BTreeMap<String, AssignmentRecord>,
}

/// Secrets as version 2 wrote them, decoded only to lift the identifier key out
/// into its own custody file.
#[derive(Deserialize)]
struct LegacySecrets {
    version: u32,
    identifier_key: [u8; 32],
    proof_keys: BTreeMap<String, ProofKey>,
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
    identifier_path: PathBuf,
    policy: Mutex<CoordinatorPolicy>,
    secrets: Mutex<CoordinatorSecrets>,
    identifier: Mutex<IdentifierCustody>,
    /// The opened key, held once rather than unwrapped per compilation. The
    /// envelope on disk is the durable artifact; this is the spend.
    identifier_key: [u8; 32],
}

impl CoordinatorStore {
    /// Open the coordinator's durable state.
    ///
    /// `minted` is the key to seal if no identifier envelope exists yet;
    /// `custodian` is who may open one that does. A machine that cannot open
    /// its own identifier envelope is refused rather than quietly reminting an
    /// equivalent one, because a fresh key silently invalidates every
    /// assignment-bound item and asset mapping already delivered.
    pub fn open(root: &Path, minted: [u8; 32], custodian: &Custodian) -> Result<Self> {
        private_dir(root)?;
        let policy_path = root.join(POLICY_FILE);
        let secrets_path = root.join(SECRETS_FILE);
        let identifier_path = root.join(IDENTIFIER_FILE);
        let legacy_path = root.join(LEGACY_FILE);

        // Whether anything was created or lifted on this open, and therefore
        // has to reach disk before the source it came from is retired.
        let mut migrated = false;
        let (policy, secrets, identifier) =
            if policy_path.exists() || secrets_path.exists() || identifier_path.exists() {
                let policy = read_json::<CoordinatorPolicy>(&policy_path)?
                    .unwrap_or_else(CoordinatorPolicy::empty);
                let (secrets, lifted) = read_secrets(&secrets_path)?;
                migrated |= lifted.is_some();
                let identifier = match read_json::<IdentifierCustody>(&identifier_path)? {
                    Some(held) => held,
                    // A version-2 secrets file carried the key inline. Lift that
                    // exact key into an envelope rather than minting a new one —
                    // the mappings it derives are already in receivers' hands.
                    None => {
                        migrated = true;
                        seal_identifier(lifted.unwrap_or(minted), custodian)?
                    }
                };
                (policy, secrets, identifier)
            } else if legacy_path.exists() {
                migrated = true;
                let (policy, secrets, key) = migrate_legacy(&legacy_path)?;
                (policy, secrets, seal_identifier(key, custodian)?)
            } else {
                migrated = true;
                (
                    CoordinatorPolicy::empty(),
                    CoordinatorSecrets::empty(),
                    seal_identifier(minted, custodian)?,
                )
            };

        policy.validate()?;
        secrets.validate()?;
        identifier.validate()?;

        let opened = identifier
            .held
            .open(IDENTIFIER_PURPOSE, &custodian.unlock)
            .context(
                "this machine holds no unlock path for the display coordinator's identifier key",
            )?;
        let identifier_key = <[u8; 32]>::try_from(opened.as_slice())
            .map_err(|_| anyhow!("the display identifier envelope holds a malformed key"))?;
        if identifier_key == [0; 32] {
            return Err(anyhow!("the display identifier envelope holds no key"));
        }

        let store = Self {
            policy_path,
            secrets_path,
            identifier_path,
            policy: Mutex::new(policy),
            secrets: Mutex::new(secrets),
            identifier: Mutex::new(identifier),
            identifier_key,
        };

        // Commit every migrated half before the source is retired, so an
        // interrupted migration reopens from what it was reading rather than
        // from nothing.
        if migrated {
            store.flush_identifier()?;
            store.flush_secrets()?;
        }
        if legacy_path.exists() {
            store.flush_policy()?;
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
        Ok(self.identifier_key)
    }

    /// The unlock paths the identifier key is held under, and whether any of
    /// them survives this machine.
    ///
    /// Standing, not custody — it says *how many ways in exist*, never what is
    /// behind any of them, so a status surface can report the loss exposure
    /// without acquiring key material to do it.
    pub fn identifier_custody(&self) -> Result<IdentifierCustodyStatus> {
        let held = self
            .identifier
            .lock()
            .map_err(|_| anyhow!("display identifier custody lock was poisoned"))?;
        Ok(IdentifierCustodyStatus {
            slots: held
                .held
                .slot_kinds()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            portable: held.held.has_portable_slot(),
        })
    }

    /// Add an unlock path to the identifier key, proving one already held.
    ///
    /// The key is not re-encrypted and never leaves the envelope: only the
    /// data-encryption key is re-wrapped, so admitting a path costs nothing
    /// already delivered.
    pub fn admit_identifier_slot(
        &self,
        unlock: &custody::UnlockKey,
        spec: &custody::SlotSpec,
    ) -> Result<()> {
        self.update_identifier(|held| held.held.admit(unlock, spec))
    }

    /// Export the identifier envelope for backup.
    ///
    /// What leaves is the sealed artifact, not the key: a copy is only as good
    /// as a slot somebody can open, which is what makes this a copy rather than
    /// a decryption, and why it needs no separate confirmation.
    pub fn export_identifier(&self) -> Result<Vec<u8>> {
        let held = self
            .identifier
            .lock()
            .map_err(|_| anyhow!("display identifier custody lock was poisoned"))?;
        serde_json::to_vec_pretty(&*held).context("encode display identifier custody")
    }

    /// Import an identifier envelope onto this machine, proving it opens.
    ///
    /// Refused unless the envelope yields a usable key under `unlock`, so a
    /// restore cannot install something this machine will discover it cannot
    /// read at the first compilation.
    pub fn import_identifier(
        root: &Path,
        exported: &[u8],
        unlock: &custody::UnlockKey,
    ) -> Result<()> {
        let held: IdentifierCustody =
            serde_json::from_slice(exported).context("decode display identifier custody")?;
        held.validate()?;
        let opened = held
            .held
            .open(IDENTIFIER_PURPOSE, unlock)
            .context("this envelope does not open with the key offered")?;
        if <[u8; 32]>::try_from(opened.as_slice()).map_or(true, |key| key == [0; 32]) {
            return Err(anyhow!("this envelope holds no usable identifier key"));
        }
        private_dir(root)?;
        write_atomic(
            &root.join(IDENTIFIER_FILE),
            &serde_json::to_vec_pretty(&held)?,
            mechanics::secretfs::Wrap::Portable,
        )
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

    fn update_identifier(
        &self,
        mutate: impl FnOnce(&mut IdentifierCustody) -> Result<()>,
    ) -> Result<()> {
        let mut held = self
            .identifier
            .lock()
            .map_err(|_| anyhow!("display identifier custody lock was poisoned"))?;
        let mut next = held.clone();
        mutate(&mut next)?;
        next.validate()?;
        write_atomic(
            &self.identifier_path,
            &serde_json::to_vec_pretty(&next)?,
            mechanics::secretfs::Wrap::Portable,
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

    fn flush_identifier(&self) -> Result<()> {
        self.update_identifier(|_| Ok(()))
    }
}

/// How exposed the identifier key is to the loss of this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifierCustodyStatus {
    /// One entry per independent unlock path.
    pub slots: Vec<String>,
    /// Whether any path survives leaving this machine. False means one profile
    /// loss invalidates every assignment-bound identifier this coordinator has
    /// issued.
    pub portable: bool,
}

/// Seal a freshly minted or lifted identifier key to the custodian's device.
///
/// One slot at first boot, and it is deliberately the portable one: a
/// device-bound slot here would reproduce the single unlock path this envelope
/// exists to end. Further paths — a passphrase, a second device — are admitted
/// as a ceremony, and until one is, [`IdentifierCustodyStatus`] says so.
fn seal_identifier(key: [u8; 32], custodian: &Custodian) -> Result<IdentifierCustody> {
    Ok(IdentifierCustody {
        version: IDENTIFIER_VERSION,
        held: custody::Custodied::seal(
            IDENTIFIER_PURPOSE,
            &key,
            &[custody::SlotSpec::RecoveryKey {
                recipient: custodian.device.clone(),
            }],
        )?,
    })
}

/// Read the secrets file at either version, reporting an identifier key that
/// still has to be lifted out of a version-2 one.
fn read_secrets(path: &Path) -> Result<(CoordinatorSecrets, Option<[u8; 32]>)> {
    if let Some(current) = read_json::<CoordinatorSecrets>(path)? {
        if current.version == SECRETS_VERSION {
            return Ok((current, None));
        }
    }
    match read_json::<LegacySecrets>(path)? {
        Some(legacy) if legacy.version == STATE_VERSION => Ok((
            CoordinatorSecrets {
                version: SECRETS_VERSION,
                proof_keys: legacy.proof_keys,
            },
            Some(legacy.identifier_key),
        )),
        Some(_) => Err(anyhow!(
            "unsupported or invalid display coordinator secrets"
        )),
        None => Ok((CoordinatorSecrets::empty(), None)),
    }
}

fn migrate_legacy(path: &Path) -> Result<(CoordinatorPolicy, CoordinatorSecrets, [u8; 32])> {
    let legacy = read_json::<LegacyState>(path)?
        .ok_or_else(|| anyhow!("display coordinator state disappeared while opening"))?;
    if legacy.version != LEGACY_STATE_VERSION {
        return Err(anyhow!("unsupported display coordinator state version"));
    }

    let mut policy = CoordinatorPolicy::empty();
    let mut secrets = CoordinatorSecrets::empty();
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
    Ok((policy, secrets, legacy.identifier_key))
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

    /// The machine's own custodian, as the daemon builds it.
    fn custodian_from(seed: [u8; 32]) -> Custodian {
        let device = mechanics::actor::device_from_seed(&seed);
        Custodian {
            unlock: custody::UnlockKey::RecoveryKey {
                seed,
                me: device.clone(),
            },
            device,
        }
    }

    fn custodian() -> Custodian {
        custodian_from([42; 32])
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
        let store = CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();
        let device = device_id();
        store.enrol(record(&device), proof()).unwrap();

        let reopened = CoordinatorStore::open(&root, [9; 32], &custodian()).unwrap();
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
        let store = CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();
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
        let store = CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();
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
        let store = CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();
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

        let store = CoordinatorStore::open(&root, [1; 32], &custodian()).unwrap();
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

    /// Cheap parameters so tests do not spend a second per derivation.
    fn fast_passphrase(secret: &str) -> custody::SlotSpec {
        custody::SlotSpec::Passphrase {
            passphrase: secret.into(),
            salt: [3u8; 16],
            params: custody::Argon2Params {
                m_cost_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        }
    }

    #[test]
    fn the_identifier_key_survives_losing_the_machine_that_minted_it() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();
        store
            .admit_identifier_slot(&custodian().unlock, &fast_passphrase("a spare way in"))
            .unwrap();

        // The identity seed is gone with the machine. The passphrase is the
        // other path, and the key that was already deriving delivered
        // identifiers comes back unchanged — which is the whole point, since a
        // different key would invalidate every mapping receivers hold.
        let reopened = CoordinatorStore::open(
            &root,
            [9; 32],
            &Custodian {
                device: mechanics::actor::device_from_seed(&[77; 32]),
                unlock: custody::UnlockKey::Passphrase("a spare way in".into()),
            },
        )
        .unwrap();
        assert_eq!(reopened.identifier_key().unwrap(), [7; 32]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_machine_holding_no_unlock_path_is_refused_rather_than_reminting() {
        let root = temp();
        CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();

        // Reminting here would be the quiet catastrophe: the coordinator would
        // come up healthy and every asset handle already in a receiver's hands
        // would stop resolving.
        let refused = CoordinatorStore::open(&root, [9; 32], &custodian_from([88; 32]));
        assert!(refused.is_err(), "a stranger opened the identifier key");

        let reopened = CoordinatorStore::open(&root, [9; 32], &custodian()).unwrap();
        assert_eq!(reopened.identifier_key().unwrap(), [7; 32]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_version_two_secrets_file_lifts_its_key_instead_of_minting_a_new_one() {
        let root = temp();
        private_dir(&root).unwrap();
        write_atomic(
            &root.join(SECRETS_FILE),
            &serde_json::to_vec_pretty(&serde_json::json!({
                "version": 2,
                "identifier_key": vec![7u8; 32],
                "proof_keys": { "11".repeat(16): "22".repeat(32) },
            }))
            .unwrap(),
            mechanics::secretfs::Wrap::DeviceBound,
        )
        .unwrap();

        let store = CoordinatorStore::open(&root, [9; 32], &custodian()).unwrap();
        assert_eq!(store.identifier_key().unwrap(), [7; 32]);
        assert_eq!(store.proof_key(&device_id()).unwrap(), Some(proof()));
        assert!(root.join(IDENTIFIER_FILE).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_exported_envelope_is_sealed_and_imports_onto_another_machine() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();
        store
            .admit_identifier_slot(&custodian().unlock, &fast_passphrase("carried by hand"))
            .unwrap();
        let exported = store.export_identifier().unwrap();

        // What leaves is the envelope, not the key. An export that had to be
        // handled as plaintext would need a confirmation this one does not.
        assert!(
            !exported.windows(32).any(|window| window == [7u8; 32]),
            "the exported envelope carries the identifier key in the clear"
        );

        let elsewhere = temp();
        assert!(
            CoordinatorStore::import_identifier(
                &elsewhere,
                &exported,
                &custody::UnlockKey::Passphrase("wrong".into()),
            )
            .is_err(),
            "an envelope installed without proving it opens"
        );
        CoordinatorStore::import_identifier(
            &elsewhere,
            &exported,
            &custody::UnlockKey::Passphrase("carried by hand".into()),
        )
        .unwrap();

        let restored = CoordinatorStore::open(
            &elsewhere,
            [9; 32],
            &Custodian {
                device: mechanics::actor::device_from_seed(&[99; 32]),
                unlock: custody::UnlockKey::Passphrase("carried by hand".into()),
            },
        )
        .unwrap();
        assert_eq!(restored.identifier_key().unwrap(), [7; 32]);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(elsewhere);
    }

    #[test]
    fn custody_status_counts_paths_without_yielding_one() {
        let root = temp();
        let store = CoordinatorStore::open(&root, [7; 32], &custodian()).unwrap();
        let first = store.identifier_custody().unwrap();
        assert_eq!(first.slots, vec!["recovery-key".to_string()]);
        assert!(
            first.portable,
            "a coordinator's first slot must survive its own profile"
        );

        store
            .admit_identifier_slot(&custodian().unlock, &fast_passphrase("second way"))
            .unwrap();
        assert_eq!(
            store.identifier_custody().unwrap().slots,
            vec!["recovery-key".to_string(), "passphrase".to_string()]
        );
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
