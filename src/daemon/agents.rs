//! Durable lifecycle for identities whose owner has made them an agent.
//!
//! An agent lives beside, rather than inside, a World. Its directory is one
//! identity home below [`crate::registry::agents_base`], so the ordinary Reach
//! machinery gives it a real profile, key and correspondence state. This
//! module adds the owner bond and inventory; it does not invent another
//! identity system and it does not let `act_as` become a management authority.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use agent::{
    AgentLifecycle, AgentRecord, AgentState, AgentStore, FieldKey, FieldValue, InventoryField,
    InventoryItem, InventoryItemId, InventoryMutation, InventoryProjection, InventoryReader,
    OwnerAuthor, OwnershipBond, OwnershipRole, OwnershipTerms, PrimitiveKind, PrimitiveStanding,
    PublicAgentView, RecordMutation, SecretBinding, SecretRef, SecretStanding, StateMutation,
    StateRevision, Visibility, VisibilityOverride,
};
use fs2::FileExt as _;
use mechanics::ids::DeviceId;
use mechanics::kinship::ProfileId;
use serde::{Deserialize, Serialize};

const JOURNAL_MAGIC: &[u8; 8] = b"LAITACR1";
const JOURNAL_ENVELOPE_VERSION: u8 = 1;
const JOURNAL_VERSION: u16 = 1;
const JOURNAL_PREFIX: usize = 8 + 1 + 4;
const MAX_JOURNAL_BYTES: usize = 16 * 1024;
const JOURNAL_FILE: &str = "create.bin";
const JOURNAL_TEMP: &str = "create.tmp";
const CREATE_LOCK: &str = "create.lock";
const MAX_AGENTS_PER_OWNER: usize = 128;

static LIVE_RUNTIME_STANDING: OnceLock<
    Mutex<std::collections::BTreeMap<ProfileId, PrimitiveStanding>>,
> = OnceLock::new();

pub(crate) fn set_live_runtime_standing(profile: ProfileId, standing: PrimitiveStanding) {
    if let Ok(mut held) = LIVE_RUNTIME_STANDING
        .get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
        .lock()
    {
        held.insert(profile, standing);
    }
}

fn live_runtime_standing(profile: &str) -> Option<PrimitiveStanding> {
    let profile = ProfileId::parse(profile)?;
    LIVE_RUNTIME_STANDING
        .get()
        .and_then(|held| held.lock().ok()?.get(&profile).copied())
}

#[derive(Debug)]
pub(crate) enum Error {
    InvalidName(String),
    ActAsForbidden,
    AgentSelfManagement,
    Unauthorized,
    NotFound(String),
    AlreadyExists(String),
    Capacity,
    Incomplete(String),
    Corrupt(&'static str),
    Identity(String),
    Reach(String),
    Social(String),
    Agent(agent::Error),
    Storage(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(message) => write!(f, "invalid agent name: {message}"),
            Self::ActAsForbidden => {
                f.write_str("act_as is never accepted by the agent management API")
            }
            Self::AgentSelfManagement => {
                f.write_str("an agent cannot use its own identity to manage itself")
            }
            Self::Unauthorized => f.write_str("only the agent's owner may perform this action"),
            Self::NotFound(name) => write!(f, "agent '{name}' does not exist"),
            Self::AlreadyExists(name) => {
                write!(
                    f,
                    "agent '{name}' already exists with different presentation"
                )
            }
            Self::Capacity => f.write_str("the owner agent registry is at capacity"),
            Self::Incomplete(name) => write!(
                f,
                "agent '{name}' has an interrupted creation journal; recover it first"
            ),
            Self::Corrupt(what) => write!(f, "corrupt agent creation state: {what}"),
            Self::Identity(message) => write!(f, "agent identity: {message}"),
            Self::Reach(message) => write!(f, "agent reach state: {message}"),
            Self::Social(message) => write!(f, "agent social profile: {message}"),
            Self::Agent(error) => write!(f, "agent state: {error}"),
            Self::Storage(message) => write!(f, "private agent registry storage: {message}"),
            Self::Io(error) => write!(f, "agent registry I/O: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Agent(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<agent::Error> for Error {
    fn from(value: agent::Error) -> Self {
        Self::Agent(value)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// The authenticated identity at the management boundary.
///
/// `act_as` is carried only so every eventual control adapter has to prove it
/// was absent. It is never treated as authority here.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ManagementRequest<'a> {
    pub requester: &'a ProfileId,
    pub act_as: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(crate) struct RegisteredAgent {
    pub name: String,
    pub home: PathBuf,
    pub state: AgentState,
}

/// Verified custody needed by the identity-local agent supervisor. The seed
/// stays inside the daemon and is used only to author as the agent itself;
/// `owner` is delegation provenance, never a substitute signer.
pub(crate) struct AgentRuntimeMaterial {
    pub name: String,
    pub home: PathBuf,
    pub agent: ProfileId,
    pub owner: ProfileId,
    pub seed: [u8; 32],
    pub ownership: OwnershipBond,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentView {
    pub record: PublicAgentView,
    pub revision: StateRevision,
    pub inventory: InventoryProjection,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ShowFor<'a> {
    Public,
    Contact,
    Owner(ManagementRequest<'a>),
}

/// The one registry for agent identities owned by a local Reach identity.
#[derive(Clone)]
pub(crate) struct AgentRegistry {
    config_root: PathBuf,
    owner_home: PathBuf,
}

/// Owner-scoped custody seam injected into Stations launched by that owner's
/// Router. It deliberately does not consult process-global configuration, so
/// custom roots and compatibility-attached/self-contained hosts cannot wander
/// into another daemon's agent inventory.
#[derive(Clone)]
pub(crate) struct AgentSeedResolver {
    registry: AgentRegistry,
    owner: ProfileId,
}

impl AgentSeedResolver {
    pub(crate) fn open(agents_base: &Path, owner_home: PathBuf) -> Result<Self, Error> {
        let owner = crate::config::identity_profile(&owner_home)
            .map_err(|error| Error::Identity(error.to_string()))?;
        Ok(Self {
            registry: AgentRegistry::from_agents_base(agents_base, owner_home)?,
            owner,
        })
    }

    /// Resolve only a stable ProfileId owned by this exact Reach identity.
    pub(crate) fn resolve(&self, agent: &str) -> Result<[u8; 32], Error> {
        let held = self.registry.load_profile(agent)?;
        authorize_management(
            ManagementRequest {
                requester: &self.owner,
                act_as: None,
            },
            &held.state,
        )?;
        identity_material(&held.home, held.state.record.ownership.agent()).map(|held| held.seed)
    }
}

impl AgentRegistry {
    #[must_use]
    pub(crate) fn new(config_root: PathBuf, owner_home: PathBuf) -> Self {
        Self {
            config_root,
            owner_home,
        }
    }

    /// Reconstruct the canonical registry from the catalog's derived agent
    /// base. The final component must be exactly what `agents_base` derives;
    /// management cannot be redirected at an arbitrary directory.
    pub(crate) fn from_agents_base(agents_base: &Path, owner_home: PathBuf) -> Result<Self, Error> {
        let config_root = agents_base
            .parent()
            .ok_or(Error::Corrupt("agent base has no configuration root"))?;
        if crate::registry::agents_base(config_root) != agents_base {
            return Err(Error::Corrupt("agent base is not canonical"));
        }
        let registry = Self::new(config_root.to_path_buf(), owner_home);
        registry.validate_registry_roots()?;
        Ok(registry)
    }

    /// Create or resume creation of one named agent.
    ///
    /// Repeating the same request returns the same profile. A different
    /// presentation under an existing name is an explicit conflict rather
    /// than a disguised edit.
    pub(crate) fn create(
        &self,
        request: ManagementRequest<'_>,
        name: &str,
        introduction: &str,
    ) -> Result<RegisteredAgent, Error> {
        reject_act_as(request)?;
        validate_presentation(name, introduction)?;
        let owner_profile = crate::config::identity_profile(&self.owner_home)
            .map_err(|error| Error::Identity(error.to_string()))?;
        if &owner_profile != request.requester {
            return Err(Error::Unauthorized);
        }
        let owner = identity_material(&self.owner_home, &owner_profile)?;

        let home = self.prepare_home(name)?;
        let _lock = creation_lock(&home)?;
        let journal = match read_journal(&home)? {
            Some(held) => {
                held.validate()?;
                if held.name != name
                    || held.introduction != introduction
                    || held.owner != owner.profile
                {
                    return Err(Error::AlreadyExists(name.to_string()));
                }
                held
            }
            None => {
                let mut journal = CreateJournal::reserved(
                    name.to_string(),
                    introduction.to_string(),
                    owner.profile.clone(),
                    mechanics::wallclock::now_secs(),
                );
                if let Some(state) = AgentStore::at(&home).load()? {
                    if state.record.name != name
                        || state.record.introduction != introduction
                        || state.record.ownership.owner() != &owner.profile
                    {
                        return Err(Error::AlreadyExists(name.to_string()));
                    }
                    let agent_profile = state.record.ownership.agent().clone();
                    let agent_identity = identity_material(&home, &agent_profile)?;
                    state.verify(&agent_identity.devices, &owner.devices)?;
                    ensure_social_card(&home, &state, &agent_identity)?;
                    journal.created_at = state.record.ownership.terms().created_at;
                    journal.note_identity(&agent_profile)?;
                    journal.note_state(&agent_profile, &owner.profile)?;
                    journal.complete(&agent_profile, &owner.profile)?;
                    write_journal(&home, &journal)?;
                    return Ok(RegisteredAgent {
                        name: name.to_string(),
                        home,
                        state,
                    });
                }
                write_journal(&home, &journal)?;
                journal
            }
        };
        self.resume_locked(&home, journal, &owner)
    }

    /// Resume every durable creation journal. Intended for daemon startup.
    pub(crate) fn recover(&self) -> Result<Vec<RegisteredAgent>, Error> {
        self.recover_available()?
            .into_iter()
            .map(|(_, result)| result)
            .collect()
    }

    /// Resume creation journals independently so one corrupt child cannot
    /// deny recovery to healthy siblings during daemon startup.
    pub(crate) fn recover_available(
        &self,
    ) -> Result<Vec<(String, Result<RegisteredAgent, Error>)>, Error> {
        let base = crate::registry::agents_base(&self.config_root);
        mechanics::secretfs::create_private_dir(&base)
            .map_err(|error| Error::Storage(error.to_string()))?;
        let mut names = directory_names(&base)?;
        names.sort();
        let mut recovered = Vec::new();
        for name in names {
            let home = match self.validated_home(&name) {
                Ok(home) => home,
                Err(error) => {
                    recovered.push((name, Err(error)));
                    continue;
                }
            };
            if !journal_exists(&home) {
                continue;
            }
            let result = (|| {
                let _lock = creation_lock(&home)?;
                let journal = read_journal(&home)?.ok_or(Error::Corrupt("journal disappeared"))?;
                journal.validate()?;
                let owner = identity_material(&self.owner_home, &journal.owner)?;
                self.resume_locked(&home, journal, &owner)
            })();
            recovered.push((name, result));
        }
        Ok(recovered)
    }

    pub(crate) fn load(&self, name: &str) -> Result<RegisteredAgent, Error> {
        let home = self.validated_home(name)?;
        reject_symlink_or_non_directory(&home)?;
        if !home.is_dir() {
            return Err(Error::NotFound(name.to_string()));
        }
        let state_dir = journal_dir(&home);
        reject_symlink_or_non_directory(&state_dir)?;
        let has_state =
            state_dir.join("state.bin").is_file() || state_dir.join("state.tmp").is_file();
        if !has_state {
            return Err(Error::NotFound(name.to_string()));
        }
        if journal_exists(&home) {
            let journal = read_journal(&home)?.ok_or(Error::Corrupt("journal disappeared"))?;
            if !matches!(journal.phase, CreatePhase::Complete { .. }) {
                return Err(Error::Incomplete(name.to_string()));
            }
        }
        let state = AgentStore::at(&home)
            .load()?
            .ok_or_else(|| Error::NotFound(name.to_string()))?;
        self.verify_state(name, &home, &state)?;
        Ok(RegisteredAgent {
            name: name.to_string(),
            home,
            state,
        })
    }

    /// Resolve one stable global profile to its verified local registry entry.
    /// Names are presentation only and never select authority after creation.
    pub(crate) fn load_profile(&self, profile: &str) -> Result<RegisteredAgent, Error> {
        let profile =
            ProfileId::parse(profile).ok_or_else(|| Error::NotFound(profile.to_string()))?;
        let index = self.verified_profile_index()?;
        let evidence = index
            .classify_profile(&profile)
            .ok_or_else(|| Error::NotFound(profile.to_string()))?;
        let held = self.load(&evidence.name)?;
        if held.state.record.ownership.agent() != &profile {
            return Err(Error::Corrupt(
                "profile index changed while resolving the agent",
            ));
        }
        Ok(held)
    }

    pub(crate) fn runtime_material(&self, name: &str) -> Result<AgentRuntimeMaterial, Error> {
        let held = self.load(name)?;
        if held.state.record.lifecycle != AgentLifecycle::Active {
            return Err(Error::Unauthorized);
        }
        let console_enabled = held.state.inventory.items.iter().any(|item| {
            item.kind.as_str() == "lait.console"
                && matches!(
                    item.standing,
                    PrimitiveStanding::Ready | PrimitiveStanding::Unavailable
                )
        });
        if !console_enabled {
            return Err(Error::Unauthorized);
        }
        let agent = held.state.record.ownership.agent().clone();
        let owner = held.state.record.ownership.owner().clone();
        let identity = identity_material(&held.home, &agent)?;
        Ok(AgentRuntimeMaterial {
            name: held.name,
            home: held.home,
            agent,
            owner,
            seed: identity.seed,
            ownership: held.state.record.ownership,
        })
    }

    /// Re-read and verify the signed owner state before an already-running
    /// Console node accepts more work.
    pub(crate) fn console_runtime_enabled(&self, name: &str) -> Result<bool, Error> {
        let held = self.load(name)?;
        Ok(held.state.record.lifecycle == AgentLifecycle::Active
            && held.state.inventory.items.iter().any(|item| {
                item.kind.as_str() == "lait.console"
                    && matches!(
                        item.standing,
                        PrimitiveStanding::Ready | PrimitiveStanding::Unavailable
                    )
            }))
    }

    pub(crate) fn list(&self) -> Result<Vec<PublicAgentView>, Error> {
        let base = crate::registry::agents_base(&self.config_root);
        let Ok(_) = fs::metadata(&base) else {
            return Ok(Vec::new());
        };
        let mut names = directory_names(&base)?;
        names.sort();
        let mut records = Vec::new();
        for name in names {
            let home = self.validated_home(&name)?;
            let has_state = home.join("agent").join("state.bin").is_file()
                || home.join("agent").join("state.tmp").is_file();
            if !has_state {
                continue;
            }
            match self.load(&name) {
                Ok(agent) => records.push(agent.state.record.public_view()),
                // Enumeration is best effort. A corrupt or interrupted sibling
                // is not evidence about a healthy agent and must not deny the
                // whole owner inventory.
                Err(_) => {}
            }
        }
        Ok(records)
    }

    pub(crate) fn show(&self, name: &str, audience: ShowFor<'_>) -> Result<AgentView, Error> {
        let held = self.load(name)?;
        let reader = match audience {
            ShowFor::Public => InventoryReader::Public,
            ShowFor::Contact => InventoryReader::Contact,
            ShowFor::Owner(request) => {
                authorize_management(request, &held.state)?;
                InventoryReader::Owner(request.requester)
            }
        };
        Ok(AgentView {
            record: held.state.record.public_view(),
            revision: held.state.head(),
            inventory: held
                .state
                .inventory
                .project(&held.state.record.ownership, reader)?,
        })
    }

    pub(crate) fn set_lifecycle(
        &self,
        request: ManagementRequest<'_>,
        name: &str,
        expected: StateRevision,
        lifecycle: AgentLifecycle,
    ) -> Result<RegisteredAgent, Error> {
        self.mutate(
            request,
            name,
            expected,
            StateMutation::Record(RecordMutation::SetLifecycle(lifecycle)),
        )
    }

    pub(crate) fn mutate_inventory(
        &self,
        request: ManagementRequest<'_>,
        name: &str,
        expected: StateRevision,
        mutation: InventoryMutation,
    ) -> Result<RegisteredAgent, Error> {
        self.mutate(request, name, expected, StateMutation::Inventory(mutation))
    }

    fn mutate(
        &self,
        request: ManagementRequest<'_>,
        name: &str,
        expected: StateRevision,
        mutation: StateMutation,
    ) -> Result<RegisteredAgent, Error> {
        let held = self.load(name)?;
        authorize_management(request, &held.state)?;
        let owner = identity_material(&self.owner_home, request.requester)?;
        let author = OwnerAuthor {
            profile: &owner.profile,
            seed: &owner.seed,
            resolved_devices: &owner.devices,
        };
        let state = AgentStore::at(&held.home).mutate(&author, expected, mutation)?;
        self.verify_state(name, &held.home, &state)?;
        Ok(RegisteredAgent {
            name: name.to_string(),
            home: held.home,
            state,
        })
    }

    fn resume_locked(
        &self,
        home: &Path,
        mut journal: CreateJournal,
        owner: &IdentityMaterial,
    ) -> Result<RegisteredAgent, Error> {
        if journal.owner != owner.profile {
            return Err(Error::Corrupt("journal owner changed"));
        }
        ensure_identity_seed(home)?;
        let agent_profile = crate::config::identity_profile(home)
            .map_err(|error| Error::Identity(error.to_string()))?;
        journal.note_identity(&agent_profile)?;
        write_journal(home, &journal)?;
        let agent_identity = identity_material(home, &agent_profile)?;

        let store = AgentStore::at(home);
        let state = match store.load()? {
            Some(state) => state,
            None => {
                let state = initial_state(&journal, &agent_identity, owner, home)?;
                store.create(&state, &agent_identity.devices, &owner.devices)?;
                state
            }
        };
        verify_created_state(&journal, &state, &agent_identity, owner)?;
        ensure_social_card(home, &state, &agent_identity)?;
        journal.note_state(&agent_profile, &owner.profile)?;
        write_journal(home, &journal)?;
        journal.complete(&agent_profile, &owner.profile)?;
        write_journal(home, &journal)?;
        Ok(RegisteredAgent {
            name: journal.name,
            home: home.to_path_buf(),
            state,
        })
    }

    fn verify_state(&self, name: &str, home: &Path, state: &AgentState) -> Result<(), Error> {
        if state.record.name != name {
            return Err(Error::Corrupt("directory name and agent record disagree"));
        }
        let agent_identity = identity_material(home, state.record.ownership.agent())?;
        let owner = identity_material(&self.owner_home, state.record.ownership.owner())?;
        state.verify(&agent_identity.devices, &owner.devices)?;
        Ok(())
    }

    fn prepare_home(&self, name: &str) -> Result<PathBuf, Error> {
        let home = self.validated_home(name)?;
        let base = crate::registry::agents_base(&self.config_root);
        self.validate_registry_roots()?;
        mechanics::secretfs::create_private_dir(&base)
            .map_err(|error| Error::Storage(error.to_string()))?;
        self.validate_registry_roots()?;
        if !home.exists() && directory_names(&base)?.len() >= MAX_AGENTS_PER_OWNER {
            return Err(Error::Capacity);
        }
        reject_symlink_or_non_directory(&home)?;
        mechanics::secretfs::create_private_dir(&home)
            .map_err(|error| Error::Storage(error.to_string()))?;
        reject_symlink_or_non_directory(&home)?;
        Ok(home)
    }

    fn validated_home(&self, name: &str) -> Result<PathBuf, Error> {
        validate_name(name)?;
        Ok(crate::registry::agents_base(&self.config_root).join(name))
    }

    fn validate_registry_roots(&self) -> Result<(), Error> {
        let config = fs::symlink_metadata(&self.config_root)?;
        if config.file_type().is_symlink() || !config.is_dir() {
            return Err(Error::Corrupt("configuration root is not a real directory"));
        }
        // Ancestors may have platform-defined aliases (`/var` -> `/private/var`
        // on macOS). What matters at this trust boundary is that the selected
        // configuration entry itself is a real directory and the agent base,
        // once present, resolves as its direct child.
        let canonical_config = self.config_root.canonicalize()?;
        let base = crate::registry::agents_base(&self.config_root);
        match fs::symlink_metadata(&base) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(Error::Corrupt("agent base is a symbolic link"))
            }
            Ok(metadata) if !metadata.is_dir() => {
                Err(Error::Corrupt("agent base is not a directory"))
            }
            Ok(_) => {
                let canonical = base.canonicalize()?;
                if canonical.parent() != Some(canonical_config.as_path()) {
                    Err(Error::Corrupt("agent base escaped its configuration root"))
                } else {
                    Ok(())
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::Io(error)),
        }
    }
}

fn validate_name(name: &str) -> Result<(), Error> {
    crate::agent_token::plain_agent_name(name)
        .map_err(|error| Error::InvalidName(error.to_string()))?;
    if name.len() > agent::MAX_NAME_BYTES {
        return Err(Error::InvalidName(format!(
            "an agent name is at most {} bytes",
            agent::MAX_NAME_BYTES
        )));
    }
    Ok(())
}

fn validate_presentation(name: &str, introduction: &str) -> Result<(), Error> {
    validate_name(name)?;
    if introduction.len() > agent::MAX_INTRODUCTION_BYTES
        || introduction.chars().any(char::is_control)
    {
        return Err(Error::Identity("invalid agent introduction".to_string()));
    }
    Ok(())
}

fn reject_symlink_or_non_directory(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(Error::Corrupt("agent home is a symbolic link"))
        }
        Ok(metadata) if !metadata.is_dir() => Err(Error::Corrupt("agent home is not a directory")),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error)),
    }
}

fn reject_act_as(request: ManagementRequest<'_>) -> Result<(), Error> {
    if request.act_as.is_some() {
        Err(Error::ActAsForbidden)
    } else {
        Ok(())
    }
}

fn authorize_management(request: ManagementRequest<'_>, state: &AgentState) -> Result<(), Error> {
    reject_act_as(request)?;
    if request.requester == state.record.ownership.agent() {
        return Err(Error::AgentSelfManagement);
    }
    if request.requester != state.record.ownership.owner() {
        return Err(Error::Unauthorized);
    }
    Ok(())
}

struct IdentityMaterial {
    profile: ProfileId,
    seed: [u8; 32],
    devices: Vec<DeviceId>,
}

fn identity_material(home: &Path, expected: &ProfileId) -> Result<IdentityMaterial, Error> {
    let seed =
        crate::config::load_identity(home).map_err(|error| Error::Identity(error.to_string()))?;
    let state = addressbook::ReachStore::at(home)
        .load()
        .map_err(|error| Error::Reach(error.to_string()))?
        .ok_or_else(|| Error::Reach("identity has no persisted Reach state".to_string()))?;
    let genesis = state
        .genesis
        .clone()
        .ok_or_else(|| Error::Reach("identity has no carried genesis".to_string()))?;
    let profile = mechanics::kinship::KinshipLog::found(genesis)
        .map_err(|error| Error::Reach(format!("identity genesis does not verify: {error}")))?
        .profile()
        .clone();
    if &profile != expected {
        return Err(Error::Reach(
            "the identity home's founded profile is not the expected profile".to_string(),
        ));
    }
    let devices = state
        .registry
        .resolve(&profile)
        .ok_or_else(|| Error::Reach("expected profile is not held by this identity".to_string()))?;
    let this_device = mechanics::actor::device_from_seed(&seed);
    if !devices.contains(&this_device) {
        return Err(Error::Reach(
            "this device is not resolved by the expected profile".to_string(),
        ));
    }
    Ok(IdentityMaterial {
        profile,
        seed,
        devices,
    })
}

fn ensure_identity_seed(home: &Path) -> Result<(), Error> {
    let path = home.join("secret.key");
    if path.exists() {
        crate::config::load_identity(home)
            .map(|_| ())
            .map_err(|error| Error::Identity(error.to_string()))
    } else {
        let seed =
            mechanics::actor::random_seed().map_err(|error| Error::Identity(error.to_string()))?;
        let encoded = data_encoding::HEXLOWER.encode(&seed);
        mechanics::secretfs::write_private(
            &path,
            encoded.as_bytes(),
            mechanics::secretfs::Create::New,
            mechanics::secretfs::Wrap::Portable,
        )
        .map_err(|error| Error::Storage(error.to_string()))
    }
}

fn initial_state(
    journal: &CreateJournal,
    agent_identity: &IdentityMaterial,
    owner: &IdentityMaterial,
    home: &Path,
) -> Result<AgentState, Error> {
    let terms = OwnershipTerms::new(
        agent_identity.profile.clone(),
        owner.profile.clone(),
        journal.created_at,
        random_nonce()?,
    );
    let agent_half = terms.sign(OwnershipRole::Agent, &agent_identity.seed)?;
    let owner_half = terms.sign(OwnershipRole::Owner, &owner.seed)?;
    let bond = OwnershipBond::assemble(
        terms,
        agent_half,
        owner_half,
        &agent_identity.devices,
        &owner.devices,
    )?;
    let author = OwnerAuthor {
        profile: &owner.profile,
        seed: &owner.seed,
        resolved_devices: &owner.devices,
    };
    let record = AgentRecord::new(
        bond.clone(),
        journal.name.clone(),
        journal.introduction.clone(),
    )?;
    let mut inventory = agent::InventoryManifest::empty(&bond, Visibility::Public, &author)?;
    for item in native_inventory(&agent_identity.profile, home)? {
        let revision = inventory.revision;
        inventory.apply(&bond, &author, revision, InventoryMutation::Add(item))?;
    }
    AgentState::new(record, inventory).map_err(Error::from)
}

fn native_inventory(agent_profile: &ProfileId, home: &Path) -> Result<Vec<InventoryItem>, Error> {
    let profile_field = InventoryField {
        key: FieldKey::parse("profile")?,
        value: FieldValue::Profile(agent_profile.clone()),
    };
    let home_field = InventoryField {
        key: FieldKey::parse("local-home")?,
        value: FieldValue::Text(home.display().to_string()),
    };
    Ok(vec![
        native_item(
            "identity",
            "lait.identity",
            "Identity",
            "A distinct Reach profile with its own signing identity.",
            PrimitiveStanding::Ready,
            vec![profile_field],
            Vec::new(),
        )?,
        native_item(
            "correspondence",
            "lait.correspondence",
            "Correspondence",
            "Durable signed correspondence carried by the agent's Reach identity.",
            PrimitiveStanding::Ready,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "brain",
            "lait.brain",
            "Brain",
            "The agent's reasoning loop and model binding.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "console",
            "lait.console",
            "Console",
            "An owner-governed computer command session.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "memory",
            "lait.memory",
            "Memory",
            "Durable agent-owned working and recalled context.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "home",
            "lait.home",
            "Home",
            "Private durable storage belonging to this agent identity.",
            PrimitiveStanding::Ready,
            Vec::new(),
            vec![home_field],
        )?,
        native_item(
            "scratch",
            "lait.scratch",
            "Scratch",
            "Ephemeral workspace isolated to this agent.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "skills",
            "lait.skills",
            "Skills",
            "Owner-managed reusable instructions and capabilities.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "compute",
            "lait.compute",
            "Compute",
            "Isolated execution capacity assigned to the agent.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "secrets",
            "lait.secrets",
            "Secrets",
            "Opaque, policy-bound secret references; never secret values.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "approvals",
            "lait.approvals",
            "Approvals",
            "Human decisions required before delicate actions proceed.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "activity",
            "lait.activity",
            "Activity",
            "An inspectable history of consequential agent actions.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
        native_item(
            "schedule",
            "lait.schedule",
            "Schedule",
            "Owner-governed recurring and deferred work.",
            PrimitiveStanding::Unavailable,
            Vec::new(),
            Vec::new(),
        )?,
    ])
}

fn native_item(
    id: &str,
    kind: &str,
    label: &str,
    summary: &str,
    standing: PrimitiveStanding,
    public_fields: Vec<InventoryField>,
    owner_fields: Vec<InventoryField>,
) -> Result<InventoryItem, Error> {
    let item = InventoryItem {
        id: InventoryItemId::parse(id)?,
        kind: PrimitiveKind::parse(kind)?,
        label: label.to_string(),
        summary: summary.to_string(),
        visibility: VisibilityOverride::Inherit,
        standing,
        public_fields,
        owner_fields,
        secrets: Vec::new(),
    };
    item.validate()?;
    Ok(item)
}

fn random_nonce() -> Result<[u8; 16], Error> {
    let seed =
        mechanics::actor::random_seed().map_err(|error| Error::Identity(error.to_string()))?;
    seed.get(..16)
        .and_then(|part| part.try_into().ok())
        .ok_or(Error::Corrupt("random nonce"))
}

fn verify_created_state(
    journal: &CreateJournal,
    state: &AgentState,
    agent_identity: &IdentityMaterial,
    owner: &IdentityMaterial,
) -> Result<(), Error> {
    state.verify(&agent_identity.devices, &owner.devices)?;
    if state.record.name != journal.name
        || state.record.introduction != journal.introduction
        || state.record.ownership.agent() != &agent_identity.profile
        || state.record.ownership.owner() != &owner.profile
    {
        return Err(Error::Corrupt("created state disagrees with journal"));
    }
    Ok(())
}

fn ensure_social_card(
    home: &Path,
    state: &AgentState,
    identity: &IdentityMaterial,
) -> Result<(), Error> {
    let book = super::address_book::AddressBookService::open(home)
        .map_err(|error| Error::Social(error.to_string()))?;
    let device = mechanics::actor::device_from_seed(&identity.seed);
    book.ensure_self_card(&state.record.name, &state.record.introduction, &device)
        .map_err(Error::Social)
}

/// Whether a control request belongs to the global agent supervisor. Keeping
/// this list beside its handler makes the daemon route explicit and prevents a
/// new agent verb from falling through to a Space host.
pub(crate) fn is_agent_request(request: &crate::control::Request) -> bool {
    matches!(
        request,
        crate::control::Request::AgentCreate { .. }
            | crate::control::Request::AgentList
            | crate::control::Request::AgentShow { .. }
            | crate::control::Request::AgentSetLifecycle { .. }
            | crate::control::Request::AgentInventoryMutate { .. }
    )
}

/// Serve one explicit daemon-scoped agent request.
///
/// The requester is always the primary correspondence profile. `act_as` is
/// refused even for read requests so a client cannot mistake World signer
/// selection for an identity-management context.
pub(crate) fn handle_control(
    router: &crate::orbits::Router,
    request: crate::control::Request,
    act_as: Option<&str>,
) -> crate::control::Response {
    if act_as.is_some() {
        return crate::control::Response::err(Error::ActAsForbidden.to_string());
    }
    if router.catalog().self_contained() {
        return crate::control::Response::err(
            "a self-contained agent daemon cannot manage sibling agent identities",
        );
    }
    let Some(requester) = router.correspondence().profile() else {
        return crate::control::Response::err("the primary correspondence profile is not standing");
    };
    let registry = match AgentRegistry::from_agents_base(
        router.catalog().agents_base(),
        router.catalog().identity().to_path_buf(),
    ) {
        Ok(registry) => registry,
        Err(error) => return crate::control::Response::err(error.to_string()),
    };
    let management = ManagementRequest {
        requester: &requester,
        act_as: None,
    };

    let result = match request {
        crate::control::Request::AgentCreate { name, introduction } => registry
            .create(management, &name, &introduction)
            .and_then(|created| {
                let introduced = router
                    .correspondence_host()
                    .introduce_agent(&created.name, super::correspondence::now_secs())
                    .map_err(Error::Reach)?;
                if introduced.primary != requester
                    || introduced.agent != *created.state.record.ownership.agent()
                {
                    return Err(Error::Corrupt(
                        "mutual introduction returned different profiles",
                    ));
                }
                let evidence = registry.verified_profile_index()?;
                let verified = evidence
                    .classify_profile(&introduced.agent)
                    .ok_or(Error::Corrupt("created agent has no verified relationship"))?;
                router
                    .book()
                    .map_err(Error::Social)?
                    .install_verified_agent(verified)
                    .map_err(Error::Social)?;
                registry.show(&created.name, ShowFor::Owner(management))
            })
            .map(agent_view_to_control)
            .map(|view| crate::control::Response::Agent(Box::new(view))),
        crate::control::Request::AgentList => {
            registry
                .list()
                .map(|agents| crate::control::Response::Agents {
                    agents: agents.into_iter().map(agent_list_to_control).collect(),
                })
        }
        crate::control::Request::AgentShow { agent, audience } => {
            let held = match registry.load_profile(&agent) {
                Ok(held) => held,
                Err(error) => return crate::control::Response::err(error.to_string()),
            };
            let show_for = match audience {
                crate::control::AgentInventoryAudience::Public => ShowFor::Public,
                crate::control::AgentInventoryAudience::Contacts => ShowFor::Contact,
                crate::control::AgentInventoryAudience::Owner => ShowFor::Owner(management),
            };
            registry
                .show(&held.name, show_for)
                .map(agent_view_to_control)
                .map(|view| crate::control::Response::Agent(Box::new(view)))
        }
        crate::control::Request::AgentSetLifecycle {
            agent,
            expected,
            lifecycle,
        } => registry
            .load_profile(&agent)
            .and_then(|held| {
                registry.set_lifecycle(
                    management,
                    &held.name,
                    state_revision_from_control(expected),
                    lifecycle_from_control(lifecycle),
                )?;
                registry.show(&held.name, ShowFor::Owner(management))
            })
            .map(agent_view_to_control)
            .map(|view| crate::control::Response::Agent(Box::new(view))),
        crate::control::Request::AgentInventoryMutate {
            agent,
            expected,
            mutation,
        } => registry
            .load_profile(&agent)
            .and_then(|held| {
                inventory_mutation_from_control(mutation).and_then(|mutation| {
                    registry.mutate_inventory(
                        management,
                        &held.name,
                        state_revision_from_control(expected),
                        mutation,
                    )
                })?;
                registry.show(&held.name, ShowFor::Owner(management))
            })
            .map(agent_view_to_control)
            .map(|view| crate::control::Response::Agent(Box::new(view))),
        _ => return crate::control::Response::err("request is not an agent request"),
    };
    result.unwrap_or_else(|error| crate::control::Response::err(error.to_string()))
}

/// Resolve an owner-requested Space sponsorship to the canonical agent's
/// local device public key. The seed is opened only to prove which public key
/// belongs to this identity home; it is never returned or used to sign the
/// owner's Space operation.
pub(crate) fn sponsorship_key(
    router: &crate::orbits::Router,
    agent: &str,
    act_as: Option<&str>,
) -> Result<String, Error> {
    if router.catalog().self_contained() {
        return Err(Error::Unauthorized);
    }
    let requester = router
        .correspondence()
        .profile()
        .ok_or_else(|| Error::Reach("the primary correspondence profile is not standing".into()))?;
    let management = ManagementRequest {
        requester: &requester,
        act_as,
    };
    reject_act_as(management)?;
    let registry = AgentRegistry::from_agents_base(
        router.catalog().agents_base(),
        router.catalog().identity().to_path_buf(),
    )?;
    let held = registry.load_profile(agent)?;
    authorize_management(management, &held.state)?;
    let material = identity_material(&held.home, held.state.record.ownership.agent())?;
    Ok(mechanics::actor::device_from_seed(&material.seed).to_string())
}

/// Add verified agent relationship facts to a primary Reach projection. The
/// Address Book may contribute an owner-authored parent display name, but it
/// can never classify a profile as an agent by itself.
pub(crate) fn decorate_reach(
    router: &crate::orbits::Router,
    response: &mut crate::control::Response,
) {
    let crate::control::Response::Reach(view) = response else {
        return;
    };
    if router.catalog().self_contained() {
        return;
    }
    let Ok(registry) = AgentRegistry::from_agents_base(
        router.catalog().agents_base(),
        router.catalog().identity().to_path_buf(),
    ) else {
        return;
    };
    let Ok(index) = registry.verified_profile_index() else {
        return;
    };
    let book = router.book().ok();
    view.agents = index
        .profiles()
        .map(|evidence| crate::control::ReachAgentEvidenceView {
            profile: evidence.agent.to_string(),
            name: evidence.name.clone(),
            owner: evidence.owner.to_string(),
            owner_name: book.and_then(|book| book.verified_agent_owner_name(evidence)),
        })
        .collect();
}

fn lifecycle_from_control(value: crate::control::AgentLifecycleSetting) -> AgentLifecycle {
    match value {
        crate::control::AgentLifecycleSetting::Active => AgentLifecycle::Active,
        crate::control::AgentLifecycleSetting::Suspended => AgentLifecycle::Suspended,
        crate::control::AgentLifecycleSetting::Retired => AgentLifecycle::Retired,
    }
}

fn lifecycle_to_control(value: AgentLifecycle) -> crate::control::AgentLifecycleSetting {
    match value {
        AgentLifecycle::Active => crate::control::AgentLifecycleSetting::Active,
        AgentLifecycle::Suspended => crate::control::AgentLifecycleSetting::Suspended,
        AgentLifecycle::Retired => crate::control::AgentLifecycleSetting::Retired,
    }
}

fn state_revision_from_control(value: crate::control::AgentStateRevision) -> StateRevision {
    StateRevision {
        record: value.record,
        inventory: value.inventory,
    }
}

fn agent_list_to_control(value: PublicAgentView) -> crate::control::AgentListItemView {
    crate::control::AgentListItemView {
        profile: value.agent.to_string(),
        owner: value.owner.to_string(),
        name: value.name,
        introduction: value.introduction,
        lifecycle: lifecycle_to_control(value.lifecycle),
    }
}

fn agent_view_to_control(value: AgentView) -> crate::control::AgentView {
    let record = agent_list_to_control(value.record);
    let live_runtime = live_runtime_standing(&record.profile);
    crate::control::AgentView {
        profile: record.profile,
        owner: record.owner,
        name: record.name,
        introduction: record.introduction,
        lifecycle: record.lifecycle,
        can_manage: true,
        revision: crate::control::AgentStateRevision {
            record: value.revision.record,
            inventory: value.revision.inventory,
        },
        inventory: inventory_projection_to_control(value.inventory, live_runtime),
    }
}

fn inventory_projection_to_control(
    value: InventoryProjection,
    live_runtime: Option<PrimitiveStanding>,
) -> crate::control::AgentInventoryView {
    match value {
        InventoryProjection::Hidden => crate::control::AgentInventoryView::Hidden,
        InventoryProjection::Public(view) => crate::control::AgentInventoryView::Public {
            version: view.version,
            agent: view.agent.to_string(),
            revision: view.revision,
            audience: match view.audience {
                agent::PublicationAudience::Public => {
                    crate::control::AgentInventoryAudience::Public
                }
                agent::PublicationAudience::Contacts => {
                    crate::control::AgentInventoryAudience::Contacts
                }
            },
            items: view
                .items
                .into_iter()
                .map(|item| crate::control::AgentInventoryPublicItemView {
                    id: item.id.as_str().to_string(),
                    primitive: item.kind.as_str().to_string(),
                    label: item.label,
                    summary: item.summary,
                    standing: item.standing.map(standing_to_control),
                    operational_standing: item.standing.and_then(|standing| {
                        live_primitive_standing(item.kind.as_str(), standing, live_runtime)
                            .map(standing_to_control)
                    }),
                    fields: item.fields.into_iter().map(field_to_control).collect(),
                })
                .collect(),
            authored_by: view.authored_by.as_str().to_string(),
            signature: data_encoding::HEXLOWER.encode(view.signature.bytes()),
        },
        InventoryProjection::Owner(view) => crate::control::AgentInventoryView::Owner {
            revision: view.revision,
            default_visibility: visibility_to_control(view.default_visibility),
            items: view
                .items
                .into_iter()
                .map(|item| crate::control::AgentInventoryOwnerItemView {
                    public: crate::control::AgentInventoryPublicItemView {
                        id: item.public.id.as_str().to_string(),
                        primitive: item.public.kind.as_str().to_string(),
                        label: item.public.label,
                        summary: item.public.summary,
                        standing: item.public.standing.map(standing_to_control),
                        operational_standing: item.public.standing.and_then(|standing| {
                            live_primitive_standing(
                                item.public.kind.as_str(),
                                standing,
                                live_runtime,
                            )
                            .map(standing_to_control)
                        }),
                        fields: item
                            .public
                            .fields
                            .into_iter()
                            .map(field_to_control)
                            .collect(),
                    },
                    visibility: item_visibility_to_control(item.visibility),
                    fields: item.fields.into_iter().map(field_to_control).collect(),
                    secrets: item
                        .secrets
                        .into_iter()
                        .map(|secret| crate::control::AgentSecretSummaryView {
                            label: secret.label,
                            standing: secret_standing_to_control(secret.standing),
                        })
                        .collect(),
                    editable: item.editable,
                })
                .collect(),
        },
        // This variant contains opaque secret references and is only for the
        // agent's trusted local runtime. It must never cross this adapter.
        InventoryProjection::Secret(_) => crate::control::AgentInventoryView::Hidden,
    }
}

fn inventory_mutation_from_control(
    value: crate::control::AgentInventoryMutationSetting,
) -> Result<InventoryMutation, Error> {
    match value {
        crate::control::AgentInventoryMutationSetting::SetDefaultVisibility { visibility } => Ok(
            InventoryMutation::SetDefaultVisibility(visibility_from_control(visibility)),
        ),
        crate::control::AgentInventoryMutationSetting::Add { item } => {
            inventory_item_from_control(item).map(InventoryMutation::Add)
        }
        crate::control::AgentInventoryMutationSetting::Replace { item } => {
            inventory_item_from_control(item).map(InventoryMutation::Replace)
        }
        crate::control::AgentInventoryMutationSetting::Remove { item } => {
            InventoryItemId::parse(item)
                .map(InventoryMutation::Remove)
                .map_err(Error::from)
        }
    }
}

fn inventory_item_from_control(
    value: crate::control::AgentInventoryItemSetting,
) -> Result<InventoryItem, Error> {
    let item = InventoryItem {
        id: InventoryItemId::parse(value.id)?,
        kind: PrimitiveKind::parse(value.primitive)?,
        label: value.label,
        summary: value.summary,
        visibility: item_visibility_from_control(value.visibility),
        standing: standing_from_control(value.standing),
        public_fields: value
            .public_fields
            .into_iter()
            .map(field_from_control)
            .collect::<Result<Vec<_>, _>>()?,
        owner_fields: value
            .owner_fields
            .into_iter()
            .map(field_from_control)
            .collect::<Result<Vec<_>, _>>()?,
        secrets: value
            .secrets
            .into_iter()
            .map(|secret| {
                Ok(SecretBinding {
                    label: secret.label,
                    reference: SecretRef::parse(secret.reference)?,
                    standing: secret_standing_from_control(secret.standing),
                })
            })
            .collect::<Result<Vec<_>, agent::Error>>()?,
    };
    item.validate()?;
    Ok(item)
}

fn field_from_control(value: crate::control::AgentInventoryField) -> Result<InventoryField, Error> {
    let field_value = match value.value {
        crate::control::AgentFieldValue::Boolean(value) => FieldValue::Boolean(value),
        crate::control::AgentFieldValue::Integer(value) => FieldValue::Integer(value),
        crate::control::AgentFieldValue::Unsigned(value) => FieldValue::Unsigned(value),
        crate::control::AgentFieldValue::DurationMillis(value) => FieldValue::DurationMillis(value),
        crate::control::AgentFieldValue::ByteSize(value) => FieldValue::ByteSize(value),
        crate::control::AgentFieldValue::Text(value) => FieldValue::Text(value),
        crate::control::AgentFieldValue::Choice(value) => FieldValue::Choice(value),
        crate::control::AgentFieldValue::ContentRef(value) => FieldValue::ContentRef(value),
        crate::control::AgentFieldValue::Profile(value) => {
            FieldValue::Profile(ProfileId::parse(&value).ok_or(Error::Agent(
                agent::Error::Invalid("inventory field profile"),
            ))?)
        }
    };
    Ok(InventoryField {
        key: FieldKey::parse(value.key)?,
        value: field_value,
    })
}

fn field_to_control(value: InventoryField) -> crate::control::AgentInventoryField {
    crate::control::AgentInventoryField {
        key: value.key.as_str().to_string(),
        value: match value.value {
            FieldValue::Boolean(value) => crate::control::AgentFieldValue::Boolean(value),
            FieldValue::Integer(value) => crate::control::AgentFieldValue::Integer(value),
            FieldValue::Unsigned(value) => crate::control::AgentFieldValue::Unsigned(value),
            FieldValue::DurationMillis(value) => {
                crate::control::AgentFieldValue::DurationMillis(value)
            }
            FieldValue::ByteSize(value) => crate::control::AgentFieldValue::ByteSize(value),
            FieldValue::Text(value) => crate::control::AgentFieldValue::Text(value),
            FieldValue::Choice(value) => crate::control::AgentFieldValue::Choice(value),
            FieldValue::ContentRef(value) => crate::control::AgentFieldValue::ContentRef(value),
            FieldValue::Profile(value) => {
                crate::control::AgentFieldValue::Profile(value.to_string())
            }
        },
    }
}

fn visibility_from_control(value: crate::control::AgentInventoryVisibility) -> Visibility {
    match value {
        crate::control::AgentInventoryVisibility::Public => Visibility::Public,
        crate::control::AgentInventoryVisibility::Contacts => Visibility::Contacts,
        crate::control::AgentInventoryVisibility::Private => Visibility::Private,
    }
}

fn visibility_to_control(value: Visibility) -> crate::control::AgentInventoryVisibility {
    match value {
        Visibility::Public => crate::control::AgentInventoryVisibility::Public,
        Visibility::Contacts => crate::control::AgentInventoryVisibility::Contacts,
        Visibility::Private => crate::control::AgentInventoryVisibility::Private,
    }
}

fn item_visibility_from_control(value: crate::control::AgentItemVisibility) -> VisibilityOverride {
    match value {
        crate::control::AgentItemVisibility::Inherit => VisibilityOverride::Inherit,
        crate::control::AgentItemVisibility::Contacts => VisibilityOverride::Contacts,
        crate::control::AgentItemVisibility::Private => VisibilityOverride::Private,
    }
}

fn item_visibility_to_control(value: VisibilityOverride) -> crate::control::AgentItemVisibility {
    match value {
        VisibilityOverride::Inherit => crate::control::AgentItemVisibility::Inherit,
        VisibilityOverride::Contacts => crate::control::AgentItemVisibility::Contacts,
        VisibilityOverride::Private => crate::control::AgentItemVisibility::Private,
    }
}

fn standing_from_control(value: crate::control::AgentPrimitiveStanding) -> PrimitiveStanding {
    match value {
        crate::control::AgentPrimitiveStanding::Ready => PrimitiveStanding::Ready,
        crate::control::AgentPrimitiveStanding::Unavailable => PrimitiveStanding::Unavailable,
        crate::control::AgentPrimitiveStanding::Suspended => PrimitiveStanding::Suspended,
        crate::control::AgentPrimitiveStanding::Revoked => PrimitiveStanding::Revoked,
    }
}

fn standing_to_control(value: PrimitiveStanding) -> crate::control::AgentPrimitiveStanding {
    match value {
        PrimitiveStanding::Ready => crate::control::AgentPrimitiveStanding::Ready,
        PrimitiveStanding::Unavailable => crate::control::AgentPrimitiveStanding::Unavailable,
        PrimitiveStanding::Suspended => crate::control::AgentPrimitiveStanding::Suspended,
        PrimitiveStanding::Revoked => crate::control::AgentPrimitiveStanding::Revoked,
    }
}

fn live_primitive_standing(
    primitive: &str,
    authored: PrimitiveStanding,
    live_runtime: Option<PrimitiveStanding>,
) -> Option<PrimitiveStanding> {
    if matches!(primitive, "lait.console" | "lait.scratch" | "lait.compute")
        && matches!(
            authored,
            PrimitiveStanding::Ready | PrimitiveStanding::Unavailable
        )
    {
        live_runtime.filter(|live| *live != authored)
    } else {
        None
    }
}

fn secret_standing_from_control(value: crate::control::AgentSecretStanding) -> SecretStanding {
    match value {
        crate::control::AgentSecretStanding::Connected => SecretStanding::Connected,
        crate::control::AgentSecretStanding::Missing => SecretStanding::Missing,
        crate::control::AgentSecretStanding::Unavailable => SecretStanding::Unavailable,
    }
}

fn secret_standing_to_control(value: SecretStanding) -> crate::control::AgentSecretStanding {
    match value {
        SecretStanding::Connected => crate::control::AgentSecretStanding::Connected,
        SecretStanding::Missing => crate::control::AgentSecretStanding::Missing,
        SecretStanding::Unavailable => crate::control::AgentSecretStanding::Unavailable,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateJournal {
    version: u16,
    name: String,
    introduction: String,
    owner: ProfileId,
    created_at: u64,
    phase: CreatePhase,
}

impl CreateJournal {
    fn reserved(name: String, introduction: String, owner: ProfileId, created_at: u64) -> Self {
        Self {
            version: JOURNAL_VERSION,
            name,
            introduction,
            owner,
            created_at,
            phase: CreatePhase::Reserved,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if self.version != JOURNAL_VERSION {
            return Err(Error::Corrupt("unsupported journal version"));
        }
        validate_presentation(&self.name, &self.introduction)?;
        match &self.phase {
            CreatePhase::Reserved => Ok(()),
            CreatePhase::IdentityReady { agent } => {
                if agent == &self.owner {
                    Err(Error::Corrupt("owner and agent profiles are identical"))
                } else {
                    Ok(())
                }
            }
            CreatePhase::StateReady { agent, owner } | CreatePhase::Complete { agent, owner } => {
                if owner != &self.owner || agent == owner {
                    Err(Error::Corrupt("journal profile transition"))
                } else {
                    Ok(())
                }
            }
        }
    }

    fn note_identity(&mut self, agent: &ProfileId) -> Result<(), Error> {
        match &self.phase {
            CreatePhase::Reserved => {
                self.phase = CreatePhase::IdentityReady {
                    agent: agent.clone(),
                };
                Ok(())
            }
            CreatePhase::IdentityReady { agent: held }
            | CreatePhase::StateReady { agent: held, .. }
            | CreatePhase::Complete { agent: held, .. }
                if held == agent =>
            {
                Ok(())
            }
            _ => Err(Error::Corrupt("agent profile changed during creation")),
        }
    }

    fn note_state(&mut self, agent: &ProfileId, owner: &ProfileId) -> Result<(), Error> {
        match &self.phase {
            CreatePhase::IdentityReady { agent: held } if held == agent => {
                self.phase = CreatePhase::StateReady {
                    agent: agent.clone(),
                    owner: owner.clone(),
                };
                Ok(())
            }
            CreatePhase::StateReady {
                agent: held_agent,
                owner: held_owner,
            }
            | CreatePhase::Complete {
                agent: held_agent,
                owner: held_owner,
            } if held_agent == agent && held_owner == owner => Ok(()),
            _ => Err(Error::Corrupt("state appeared in the wrong creation phase")),
        }
    }

    fn complete(&mut self, agent: &ProfileId, owner: &ProfileId) -> Result<(), Error> {
        match &self.phase {
            CreatePhase::StateReady {
                agent: held_agent,
                owner: held_owner,
            } if held_agent == agent && held_owner == owner => {
                self.phase = CreatePhase::Complete {
                    agent: agent.clone(),
                    owner: owner.clone(),
                };
                Ok(())
            }
            CreatePhase::Complete {
                agent: held_agent,
                owner: held_owner,
            } if held_agent == agent && held_owner == owner => Ok(()),
            _ => Err(Error::Corrupt("creation completed from the wrong phase")),
        }
    }

    fn rank(&self) -> u8 {
        match &self.phase {
            CreatePhase::Reserved => 0,
            CreatePhase::IdentityReady { .. } => 1,
            CreatePhase::StateReady { .. } => 2,
            CreatePhase::Complete { .. } => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum CreatePhase {
    Reserved,
    IdentityReady { agent: ProfileId },
    StateReady { agent: ProfileId, owner: ProfileId },
    Complete { agent: ProfileId, owner: ProfileId },
}

fn journal_dir(home: &Path) -> PathBuf {
    home.join("agent")
}

fn journal_exists(home: &Path) -> bool {
    let dir = journal_dir(home);
    dir.join(JOURNAL_FILE).is_file() || dir.join(JOURNAL_TEMP).is_file()
}

fn creation_lock(home: &Path) -> Result<File, Error> {
    let dir = journal_dir(home);
    reject_symlink_or_non_directory(&dir)?;
    mechanics::secretfs::create_private_dir(&dir)
        .map_err(|error| Error::Storage(error.to_string()))?;
    reject_symlink_or_non_directory(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(CREATE_LOCK))?;
    file.lock_exclusive()?;
    Ok(file)
}

fn write_journal(home: &Path, journal: &CreateJournal) -> Result<(), Error> {
    journal.validate()?;
    let bytes = encode_journal(journal)?;
    let dir = journal_dir(home);
    mechanics::secretfs::create_private_dir(&dir)
        .map_err(|error| Error::Storage(error.to_string()))?;
    let temporary = dir.join(JOURNAL_TEMP);
    let standing = dir.join(JOURNAL_FILE);
    mechanics::secretfs::write_private(
        &temporary,
        &bytes,
        mechanics::secretfs::Create::Replace,
        mechanics::secretfs::Wrap::Portable,
    )
    .map_err(|error| Error::Storage(error.to_string()))?;
    let reread = mechanics::secretfs::read_private(&temporary)
        .map_err(|error| Error::Storage(error.to_string()))?
        .ok_or(Error::Corrupt("journal disappeared while writing"))?;
    let decoded = decode_journal(&reread)?;
    if decoded.name != journal.name
        || decoded.owner != journal.owner
        || decoded.rank() != journal.rank()
    {
        return Err(Error::Corrupt("journal changed while writing"));
    }
    mechanics::secretfs::persist_replace(&temporary, &standing)?;
    Ok(())
}

fn read_journal(home: &Path) -> Result<Option<CreateJournal>, Error> {
    let dir = journal_dir(home);
    let standing = read_journal_path(&dir.join(JOURNAL_FILE));
    let temporary = read_journal_path(&dir.join(JOURNAL_TEMP));
    match (standing, temporary) {
        (Ok(None), Ok(None)) => Ok(None),
        (Ok(Some(held)), Ok(None)) | (Ok(None), Ok(Some(held))) => Ok(Some(held)),
        (Ok(Some(held)), Err(_)) => Ok(Some(held)),
        (Err(_), Ok(Some(temporary))) => Ok(Some(temporary)),
        (Ok(Some(held)), Ok(Some(temporary))) => {
            if held.name != temporary.name
                || held.owner != temporary.owner
                || held.introduction != temporary.introduction
            {
                return Err(Error::Corrupt("journal and temporary disagree"));
            }
            if temporary.rank() >= held.rank() {
                Ok(Some(temporary))
            } else {
                Ok(Some(held))
            }
        }
        (Err(error), Ok(None)) | (Ok(None), Err(error)) | (Err(error), Err(_)) => Err(error),
    }
}

fn read_journal_path(path: &Path) -> Result<Option<CreateJournal>, Error> {
    if !path.exists() {
        return Ok(None);
    }
    let metadata = fs::metadata(path)?;
    let max = u64::try_from(MAX_JOURNAL_BYTES).map_err(|_| Error::Corrupt("journal bound"))?;
    if metadata.len() > max {
        return Err(Error::Corrupt("journal exceeds its bound"));
    }
    let bytes = mechanics::secretfs::read_private(path)
        .map_err(|error| Error::Storage(error.to_string()))?
        .ok_or(Error::Corrupt("journal disappeared while reading"))?;
    decode_journal(&bytes).map(Some)
}

fn encode_journal(journal: &CreateJournal) -> Result<Vec<u8>, Error> {
    let body = postcard::to_stdvec(journal).map_err(|_| Error::Corrupt("journal encode"))?;
    let body_len = u32::try_from(body.len()).map_err(|_| Error::Corrupt("journal body bound"))?;
    let total = JOURNAL_PREFIX
        .checked_add(body.len())
        .and_then(|length| length.checked_add(32))
        .ok_or(Error::Corrupt("journal envelope bound"))?;
    if total > MAX_JOURNAL_BYTES {
        return Err(Error::Corrupt("journal envelope exceeds its bound"));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(JOURNAL_MAGIC);
    out.push(JOURNAL_ENVELOPE_VERSION);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(blake3::hash(&out).as_bytes());
    Ok(out)
}

fn decode_journal(bytes: &[u8]) -> Result<CreateJournal, Error> {
    let minimum = JOURNAL_PREFIX
        .checked_add(32)
        .ok_or(Error::Corrupt("journal envelope bound"))?;
    if bytes.len() < minimum || bytes.len() > MAX_JOURNAL_BYTES {
        return Err(Error::Corrupt("journal envelope length"));
    }
    if bytes.get(..8) != Some(JOURNAL_MAGIC.as_slice()) {
        return Err(Error::Corrupt("journal magic"));
    }
    if bytes.get(8).copied() != Some(JOURNAL_ENVELOPE_VERSION) {
        return Err(Error::Corrupt("journal envelope version"));
    }
    let length: [u8; 4] = bytes
        .get(9..13)
        .and_then(|part| part.try_into().ok())
        .ok_or(Error::Corrupt("journal body length"))?;
    let body_len = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| Error::Corrupt("journal body length"))?;
    let body_end = JOURNAL_PREFIX
        .checked_add(body_len)
        .ok_or(Error::Corrupt("journal body length"))?;
    let expected = body_end
        .checked_add(32)
        .ok_or(Error::Corrupt("journal body length"))?;
    if bytes.len() != expected {
        return Err(Error::Corrupt("journal length disagrees with file"));
    }
    let digest: [u8; 32] = bytes
        .get(body_end..expected)
        .and_then(|part| part.try_into().ok())
        .ok_or(Error::Corrupt("journal digest"))?;
    let signed = bytes
        .get(..body_end)
        .ok_or(Error::Corrupt("journal digest"))?;
    if blake3::hash(signed) != digest {
        return Err(Error::Corrupt("journal digest"));
    }
    let body = bytes
        .get(JOURNAL_PREFIX..body_end)
        .ok_or(Error::Corrupt("journal body"))?;
    let journal: CreateJournal =
        postcard::from_bytes(body).map_err(|_| Error::Corrupt("journal decode"))?;
    journal.validate()?;
    Ok(journal)
}

fn directory_names(base: &Path) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Corrupt("non-UTF-8 agent directory name"))?;
            validate_name(&name)?;
            names.push(name);
        }
    }
    Ok(names)
}

/// Verified global identity evidence for one locally registered agent.
///
/// `agent` and `owner` come from the dual-signed ownership bond; the device
/// sets are the current Reach facts used to verify its signers. `name` is the
/// validated registry presentation and is never used to decide that a profile
/// is an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedAgentProfile {
    pub agent: ProfileId,
    pub owner: ProfileId,
    pub name: String,
    pub agent_devices: Vec<DeviceId>,
    pub owner_devices: Vec<DeviceId>,
}

/// One local registry entry omitted from a verified evidence index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentEvidenceRefusal {
    pub entry: String,
    pub why: String,
}

/// A best-effort read-only index over the global agent registry.
///
/// Corrupt and unverified records are not classifications. Their refusals stay
/// visible independently so one damaged sibling never erases evidence for the
/// others.
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentEvidenceIndex {
    by_profile: std::collections::BTreeMap<ProfileId, VerifiedAgentProfile>,
    pub refused: Vec<AgentEvidenceRefusal>,
}

impl AgentEvidenceIndex {
    #[must_use]
    pub(crate) fn classify_profile(&self, profile: &ProfileId) -> Option<&VerifiedAgentProfile> {
        self.by_profile.get(profile)
    }

    #[must_use]
    pub(crate) fn profiles(&self) -> impl Iterator<Item = &VerifiedAgentProfile> {
        self.by_profile.values()
    }
}

// Kept as a separate additive implementation block so control-plane registry
// integration can evolve without making relationship evidence depend on its
// DTOs or routing.
impl AgentRegistry {
    /// Read every completed local agent record into verified profile evidence.
    ///
    /// This never founds an identity, resumes the creation journal, or changes
    /// a logical agent record (the store may complete its ordinary temp-file
    /// recovery while reading). A missing registry is an empty index.
    /// Directory-wide I/O failure is an error; malformed individual entries
    /// are isolated in `refused`.
    pub(crate) fn verified_profile_index(&self) -> Result<AgentEvidenceIndex, Error> {
        let base = crate::registry::agents_base(&self.config_root);
        let entries = match fs::read_dir(&base) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AgentEvidenceIndex::default())
            }
            Err(error) => return Err(Error::Io(error)),
        };
        let mut index = AgentEvidenceIndex::default();
        let mut conflicted = std::collections::BTreeSet::new();

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    index.refused.push(AgentEvidenceRefusal {
                        entry: "<unreadable directory entry>".into(),
                        why: error.to_string(),
                    });
                    continue;
                }
            };
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => {
                    index.refused.push(AgentEvidenceRefusal {
                        entry: "<non-UTF-8 directory entry>".into(),
                        why: "agent directory name is not UTF-8".into(),
                    });
                    continue;
                }
            };
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {}
                Ok(_) => continue,
                Err(error) => {
                    index.refused.push(AgentEvidenceRefusal {
                        entry: name,
                        why: error.to_string(),
                    });
                    continue;
                }
            }
            let home = base.join(&name);
            let has_state = home.join("agent").join("state.bin").is_file()
                || home.join("agent").join("state.tmp").is_file();
            if !has_state {
                continue;
            }

            let evidence = (|| {
                let registered = self.load(&name)?;
                let agent = registered.state.record.ownership.agent().clone();
                let owner = registered.state.record.ownership.owner().clone();
                let agent_identity = identity_material(&registered.home, &agent)?;
                let owner_identity = identity_material(&self.owner_home, &owner)?;
                Ok::<_, Error>(VerifiedAgentProfile {
                    agent,
                    owner,
                    name: registered.name,
                    agent_devices: agent_identity.devices,
                    owner_devices: owner_identity.devices,
                })
            })();

            let evidence = match evidence {
                Ok(evidence) => evidence,
                Err(error) => {
                    index.refused.push(AgentEvidenceRefusal {
                        entry: name,
                        why: error.to_string(),
                    });
                    continue;
                }
            };
            if conflicted.contains(&evidence.agent) {
                index.refused.push(AgentEvidenceRefusal {
                    entry: evidence.name,
                    why: "agent profile is claimed by more than one local record".into(),
                });
                continue;
            }
            if let Some(previous) = index.by_profile.remove(&evidence.agent) {
                conflicted.insert(evidence.agent.clone());
                index.refused.push(AgentEvidenceRefusal {
                    entry: previous.name,
                    why: "agent profile is claimed by more than one local record".into(),
                });
                index.refused.push(AgentEvidenceRefusal {
                    entry: evidence.name,
                    why: "agent profile is claimed by more than one local record".into(),
                });
                continue;
            }
            index.by_profile.insert(evidence.agent.clone(), evidence);
        }

        index.refused.sort_by(|left, right| {
            left.entry
                .cmp(&right.entry)
                .then_with(|| left.why.cmp(&right.why))
        });
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found_owner(home: &Path) -> ProfileId {
        mechanics::secretfs::create_private_dir(home).expect("private owner home");
        crate::config::identity_profile(home).expect("owner profile")
    }

    fn fixture() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        ProfileId,
        AgentRegistry,
    ) {
        let root = tempfile::tempdir().expect("temporary root");
        let config_root = root.path().join("config");
        mechanics::secretfs::create_private_dir(&config_root).expect("private config root");
        let owner_home = root.path().join("owner");
        let owner = found_owner(&owner_home);
        let registry = AgentRegistry::new(config_root.clone(), owner_home.clone());
        (root, config_root, owner_home, owner, registry)
    }

    fn management(owner: &ProfileId) -> ManagementRequest<'_> {
        ManagementRequest {
            requester: owner,
            act_as: None,
        }
    }

    fn router(
        config_root: &Path,
        owner_home: &Path,
        self_contained: bool,
    ) -> crate::orbits::Router {
        crate::orbits::Router::new(
            crate::orbits::Catalog::new(
                owner_home.to_path_buf(),
                crate::registry::agents_base(config_root),
                self_contained,
            ),
            crate::orbital::WorldPackages::new(),
        )
    }

    #[test]
    fn create_is_restart_idempotent_and_bond_is_real_and_distinct() {
        let (_root, config_root, owner_home, owner, registry) = fixture();
        let first = registry
            .create(management(&owner), "adam", "Security DVR virtual assistant")
            .expect("create Adam");
        let restarted = AgentRegistry::new(config_root, owner_home);
        let second = restarted
            .create(management(&owner), "adam", "Security DVR virtual assistant")
            .expect("resume Adam");
        assert_eq!(
            first.state.record.ownership.agent(),
            second.state.record.ownership.agent()
        );
        assert_ne!(first.state.record.ownership.agent(), &owner);

        let agent_reach = addressbook::ReachStore::at(&first.home)
            .load()
            .expect("read agent Reach")
            .expect("persisted Reach");
        let owner_reach = addressbook::ReachStore::at(&restarted.owner_home)
            .load()
            .expect("read owner Reach")
            .expect("persisted owner Reach");
        let agent_devices = agent_reach
            .registry
            .resolve(first.state.record.ownership.agent())
            .expect("agent devices");
        let owner_devices = owner_reach.registry.resolve(&owner).expect("owner devices");
        first
            .state
            .record
            .ownership
            .verify(&agent_devices, &owner_devices)
            .expect("dual-signed bond");

        let social = addressbook::Store::at(&first.home)
            .open()
            .expect("open agent address book")
            .expect("agent address book exists")
            .book()
            .expect("project agent address book");
        let claimed: Vec<&addressbook::Card> = social
            .cards
            .values()
            .filter(|card| card.self_claim.is_some())
            .collect();
        assert_eq!(claimed.len(), 1);
        let card = claimed.first().expect("self card");
        assert_eq!(card.name.value, "adam");
        assert_eq!(card.note.value, "Security DVR virtual assistant");
        let agent_device = mechanics::actor::device_from_seed(
            &crate::config::load_identity(&first.home).expect("agent seed"),
        );
        assert!(card.handles.iter().any(|link| {
            matches!(&link.handle, addressbook::Handle::Device(device) if device == &agent_device)
        }));
    }

    #[test]
    fn verified_profile_evidence_files_the_owner_contact_and_isolates_a_corrupt_sibling() {
        let (_root, _config_root, owner_home, owner, registry) = fixture();
        let adam = registry
            .create(management(&owner), "adam", "Security DVR virtual assistant")
            .expect("create Adam");
        let broken = registry
            .create(management(&owner), "broken", "damaged sibling")
            .expect("create sibling");
        std::fs::write(broken.home.join("agent/state.bin"), b"not an agent record")
            .expect("damage only the sibling record");

        let index = registry
            .verified_profile_index()
            .expect("read verified evidence index");
        let adam_profile = adam.state.record.ownership.agent();
        let evidence = index
            .classify_profile(adam_profile)
            .expect("Adam is classified from his verified ownership bond");
        assert!(
            index.classify_profile(&owner).is_none(),
            "being an owner does not itself classify a profile as an agent"
        );
        assert_eq!(evidence.owner, owner);
        assert_eq!(evidence.name, "adam");
        assert_eq!(index.profiles().count(), 1);
        assert_eq!(index.refused.len(), 1);
        assert_eq!(index.refused[0].entry, "broken");
        assert!(index
            .refused
            .first()
            .is_some_and(|refusal| !refusal.why.is_empty()));

        let owner_book = super::super::address_book::AddressBookService::open(&owner_home)
            .expect("open owner address book");
        let first = owner_book
            .install_verified_agent(evidence)
            .expect("install verified Adam contact");
        assert_eq!(first.agent, adam_profile.clone());
        assert_eq!(first.parent, owner);
        assert_eq!(first.parent_name, None, "a bond does not invent a name");
        assert!(first.filed);

        let owner_seed = crate::config::load_identity(&owner_home).expect("owner seed");
        let owner_device = mechanics::actor::device_from_seed(&owner_seed);
        owner_book
            .ensure_self_card("Omar", "Adam's owner", &owner_device)
            .expect("author the owner's own display evidence");
        let second = owner_book
            .install_verified_agent(evidence)
            .expect("project the same durable relationship");
        assert_eq!(second.parent_name.as_deref(), Some("Omar"));
        assert!(second.filed);

        drop(owner_book);
        let book = addressbook::Store::at(&owner_home)
            .open()
            .expect("reload owner address book")
            .expect("owner address book is durable")
            .book()
            .expect("project durable owner address book");
        let agent_handle = addressbook::Handle::Device(
            evidence
                .agent_devices
                .first()
                .expect("agent has a device")
                .clone(),
        );
        let card = book
            .authored_cards_for(&agent_handle)
            .into_iter()
            .next()
            .and_then(|id| book.cards.get(&id))
            .expect("verified profile has an authored contact");
        assert_eq!(card.name.value, "adam");
        assert!(card
            .groups
            .iter()
            .any(|group| group.name == crate::control::AGENT_GROUP));
    }

    #[test]
    fn public_initial_inventory_names_only_real_native_primitives() {
        let (_root, _config, _owner_home, owner, registry) = fixture();
        let created = registry
            .create(management(&owner), "adam", "virtual assistant")
            .expect("create Adam");
        let view = registry.show("adam", ShowFor::Public).expect("public view");
        let InventoryProjection::Public(publication) = view.inventory else {
            panic!("public inventory should be published");
        };
        let ids: Vec<&str> = publication
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "activity",
                "approvals",
                "brain",
                "compute",
                "console",
                "correspondence",
                "home",
                "identity",
                "memory",
                "schedule",
                "scratch",
                "secrets",
                "skills"
            ]
        );
        let standing = publication
            .items
            .iter()
            .map(|item| (item.kind.as_str(), item.standing))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            standing.get("lait.identity"),
            Some(&Some(PrimitiveStanding::Ready))
        );
        assert_eq!(
            standing.get("lait.console"),
            Some(&Some(PrimitiveStanding::Unavailable))
        );
        assert_eq!(
            standing.get("lait.compute"),
            Some(&Some(PrimitiveStanding::Unavailable))
        );
        assert!(created.home.join("secret.key").is_file());
        assert!(created.home.join("kinship.bin").is_file());
    }

    #[test]
    fn owner_mutations_require_no_act_as_and_fresh_revisions() {
        let (root, _config, _owner_home, owner, registry) = fixture();
        let created = registry
            .create(management(&owner), "adam", "virtual assistant")
            .expect("create Adam");
        let head = created.state.head();
        let suspended = registry
            .set_lifecycle(management(&owner), "adam", head, AgentLifecycle::Suspended)
            .expect("suspend");
        assert_eq!(suspended.state.record.lifecycle, AgentLifecycle::Suspended);
        let contacts_only = registry
            .mutate_inventory(
                management(&owner),
                "adam",
                suspended.state.head(),
                InventoryMutation::SetDefaultVisibility(Visibility::Contacts),
            )
            .expect("change inventory visibility");
        assert_eq!(
            contacts_only.state.inventory.default_visibility,
            Visibility::Contacts
        );
        assert!(matches!(
            registry.set_lifecycle(management(&owner), "adam", head, AgentLifecycle::Active),
            Err(Error::Agent(agent::Error::Conflict { .. }))
        ));

        let attacker_home = root.path().join("attacker");
        let attacker = found_owner(&attacker_home);
        assert!(matches!(
            registry.set_lifecycle(
                management(&attacker),
                "adam",
                contacts_only.state.head(),
                AgentLifecycle::Active
            ),
            Err(Error::Unauthorized)
        ));
        assert!(matches!(
            registry.set_lifecycle(
                ManagementRequest {
                    requester: &owner,
                    act_as: Some("adam")
                },
                "adam",
                contacts_only.state.head(),
                AgentLifecycle::Active
            ),
            Err(Error::ActAsForbidden)
        ));
        assert!(matches!(
            registry.set_lifecycle(
                management(created.state.record.ownership.agent()),
                "adam",
                contacts_only.state.head(),
                AgentLifecycle::Active
            ),
            Err(Error::AgentSelfManagement)
        ));
    }

    #[test]
    fn reserved_and_identity_ready_journals_recover_without_a_second_profile() {
        let (_root, _config, _owner_home, owner, registry) = fixture();
        let owner_material = identity_material(&registry.owner_home, &owner).expect("owner");

        let reserved_home = registry.prepare_home("reserved").expect("reserved home");
        let reserved = CreateJournal::reserved(
            "reserved".to_string(),
            "recover me".to_string(),
            owner.clone(),
            7,
        );
        write_journal(&reserved_home, &reserved).expect("reserved journal");
        let recovered = registry.recover().expect("recover reserved");
        let reserved_profile = recovered
            .iter()
            .find(|held| held.name == "reserved")
            .expect("reserved recovered")
            .state
            .record
            .ownership
            .agent()
            .clone();

        let identity_home = registry.prepare_home("identified").expect("identity home");
        ensure_identity_seed(&identity_home).expect("identity seed");
        let identity_profile = crate::config::identity_profile(&identity_home).expect("profile");
        let mut identified = CreateJournal::reserved(
            "identified".to_string(),
            "recover me too".to_string(),
            owner.clone(),
            8,
        );
        identified
            .note_identity(&identity_profile)
            .expect("note identity");
        write_journal(&identity_home, &identified).expect("identity journal");
        registry.recover().expect("recover identity-ready");
        let loaded = registry.load("identified").expect("identified agent");
        assert_eq!(loaded.state.record.ownership.agent(), &identity_profile);
        assert_ne!(reserved_profile, identity_profile);
        drop(owner_material);
    }

    #[test]
    fn state_ready_journal_recovers_to_complete() {
        let (_root, _config, _owner_home, owner, registry) = fixture();
        let created = registry
            .create(management(&owner), "adam", "virtual assistant")
            .expect("create Adam");
        let mut journal = read_journal(&created.home)
            .expect("read journal")
            .expect("journal");
        journal.phase = CreatePhase::StateReady {
            agent: created.state.record.ownership.agent().clone(),
            owner: owner.clone(),
        };
        write_journal(&created.home, &journal).expect("rewind journal phase");
        let recovered = registry.recover().expect("recover state-ready");
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].state.record.ownership.agent(),
            created.state.record.ownership.agent()
        );
        assert!(matches!(
            read_journal(&created.home)
                .expect("read journal")
                .expect("journal")
                .phase,
            CreatePhase::Complete { .. }
        ));
    }

    #[test]
    fn names_are_single_safe_components() {
        let (_root, _config, _owner_home, owner, registry) = fixture();
        for name in ["", "../adam", "a/b", ".adam", "CON", "adam "] {
            assert!(matches!(
                registry.create(management(&owner), name, "intro"),
                Err(Error::InvalidName(_))
            ));
        }
        let too_long = "a".repeat(agent::MAX_NAME_BYTES.saturating_add(1));
        assert!(matches!(
            registry.create(management(&owner), &too_long, "intro"),
            Err(Error::InvalidName(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_agent_base_is_refused_before_creation_mutates_it() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temporary root");
        let config_root = root.path().join("config");
        mechanics::secretfs::create_private_dir(&config_root).expect("private config root");
        let escaped = root.path().join("escaped");
        mechanics::secretfs::create_private_dir(&escaped).expect("escape target");
        symlink(&escaped, crate::registry::agents_base(&config_root)).expect("agent base symlink");
        let owner_home = root.path().join("owner");
        let owner = found_owner(&owner_home);
        let registry = AgentRegistry::new(config_root, owner_home);

        assert!(matches!(
            registry.create(management(&owner), "adam", "virtual assistant"),
            Err(Error::Corrupt("agent base is a symbolic link"))
        ));
        assert!(directory_names(&escaped)
            .expect("unchanged target")
            .is_empty());
    }

    #[test]
    fn lifecycle_suspension_revokes_console_runtime_material_immediately() {
        let (_root, _config, _owner_home, owner, registry) = fixture();
        let created = registry
            .create(management(&owner), "adam", "virtual assistant")
            .expect("create Adam");
        assert!(registry
            .console_runtime_enabled("adam")
            .expect("active state"));
        registry
            .set_lifecycle(
                management(&owner),
                "adam",
                created.state.head(),
                AgentLifecycle::Suspended,
            )
            .expect("suspend Adam");
        assert!(!registry
            .console_runtime_enabled("adam")
            .expect("freshly read suspension"));
        assert!(matches!(
            registry.runtime_material("adam"),
            Err(Error::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn daemon_control_create_list_show_mutate_and_restart_are_one_identity() {
        let (_root, config_root, owner_home, owner, _registry) = fixture();
        let owner_router = router(&config_root, &owner_home, false);
        let created = handle_control(
            &owner_router,
            crate::control::Request::AgentCreate {
                name: "adam".into(),
                introduction: "Security DVR virtual assistant".into(),
            },
            None,
        );
        let crate::control::Response::Agent(created) = created else {
            panic!("agent creation must return the owner projection");
        };
        assert_eq!(created.owner, owner.to_string());
        assert_ne!(created.profile, created.owner);
        assert!(matches!(
            created.inventory,
            crate::control::AgentInventoryView::Owner { .. }
        ));
        let profile = created.profile.clone();

        for selector in [
            "adam".to_string(),
            ProfileId::from_digest([0xf4; 16]).to_string(),
        ] {
            let refused = handle_control(
                &owner_router,
                crate::control::Request::AgentShow {
                    agent: selector,
                    audience: crate::control::AgentInventoryAudience::Owner,
                },
                None,
            );
            assert!(
                matches!(refused, crate::control::Response::Error { .. }),
                "a name or unknown profile must never retarget agent authority"
            );
        }

        let listed = handle_control(&owner_router, crate::control::Request::AgentList, None);
        let crate::control::Response::Agents { agents } = listed else {
            panic!("agent list must return summaries");
        };
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].profile, profile);

        let shown = handle_control(
            &owner_router,
            crate::control::Request::AgentShow {
                agent: profile.clone(),
                audience: crate::control::AgentInventoryAudience::Public,
            },
            None,
        );
        let crate::control::Response::Agent(shown) = shown else {
            panic!("agent show must return the requested projection");
        };
        assert!(matches!(
            shown.inventory,
            crate::control::AgentInventoryView::Public { .. }
        ));

        let mutated = handle_control(
            &owner_router,
            crate::control::Request::AgentSetLifecycle {
                agent: profile.clone(),
                expected: created.revision,
                lifecycle: crate::control::AgentLifecycleSetting::Suspended,
            },
            None,
        );
        let crate::control::Response::Agent(mutated) = mutated else {
            panic!("owner lifecycle mutation must return the new projection");
        };
        assert_eq!(
            mutated.lifecycle,
            crate::control::AgentLifecycleSetting::Suspended
        );
        assert_eq!(mutated.revision.record, created.revision.record + 1);

        let stale = handle_control(
            &owner_router,
            crate::control::Request::AgentSetLifecycle {
                agent: profile.clone(),
                expected: created.revision,
                lifecycle: crate::control::AgentLifecycleSetting::Active,
            },
            None,
        );
        assert!(matches!(stale, crate::control::Response::Error { .. }));

        let agent_home = crate::registry::agents_base(&config_root).join("adam");
        let agent_profile = ProfileId::parse(&profile).expect("wire profile");
        assert!(owner_router
            .correspondence_host()
            .profile(&agent_profile)
            .is_some());
        let agent_device = mechanics::actor::device_from_seed(
            &crate::config::load_identity(&agent_home).expect("agent seed"),
        );
        let owner_book = addressbook::Store::at(&owner_home)
            .open()
            .expect("open owner book")
            .expect("owner contact was installed")
            .book()
            .expect("project owner book");
        let card = owner_book
            .authored_cards_for(&addressbook::Handle::Device(agent_device))
            .into_iter()
            .next()
            .and_then(|id| owner_book.cards.get(&id))
            .expect("verified Adam contact");
        assert_eq!(card.name.value, "adam");
        assert!(card
            .groups
            .iter()
            .any(|group| group.name == crate::control::AGENT_GROUP));

        let mut reach = owner_router
            .correspondence()
            .handle(crate::control::Request::ReachView)
            .await;
        decorate_reach(&owner_router, &mut reach);
        let crate::control::Response::Reach(reach) = reach else {
            panic!("Reach view");
        };
        assert_eq!(reach.agents.len(), 1);
        assert_eq!(reach.agents[0].profile, profile);
        assert_eq!(reach.agents[0].name, "adam");
        assert_eq!(reach.agents[0].owner, owner.to_string());

        drop(owner_router);
        let restarted = router(&config_root, &owner_home, false);
        let repeated = handle_control(
            &restarted,
            crate::control::Request::AgentCreate {
                name: "adam".into(),
                introduction: "Security DVR virtual assistant".into(),
            },
            None,
        );
        let crate::control::Response::Agent(repeated) = repeated else {
            panic!("idempotent restart creation must return Adam");
        };
        assert_eq!(repeated.profile, profile);
    }

    #[test]
    fn daemon_control_refuses_act_as_and_self_contained_management() {
        let (_root, config_root, owner_home, _owner, _registry) = fixture();
        let owner_router = router(&config_root, &owner_home, false);
        let selected = handle_control(
            &owner_router,
            crate::control::Request::AgentList,
            Some("adam"),
        );
        let crate::control::Response::Error { message, .. } = selected else {
            panic!("act_as must be refused");
        };
        assert!(message.contains("act_as"));

        let self_contained = router(&config_root, &owner_home, true);
        let refused = handle_control(
            &self_contained,
            crate::control::Request::AgentCreate {
                name: "adam".into(),
                introduction: "virtual assistant".into(),
            },
            None,
        );
        let crate::control::Response::Error { message, .. } = refused else {
            panic!("an agent daemon must not expose owner management");
        };
        assert!(message.contains("self-contained"));
    }

    #[test]
    fn corrupt_sibling_does_not_deny_healthy_control_requests() {
        let (_root, config_root, owner_home, _owner, registry) = fixture();
        let owner_router = router(&config_root, &owner_home, false);
        for name in ["adam", "broken"] {
            let response = handle_control(
                &owner_router,
                crate::control::Request::AgentCreate {
                    name: name.into(),
                    introduction: "virtual assistant".into(),
                },
                None,
            );
            assert!(matches!(response, crate::control::Response::Agent(_)));
        }
        let broken = registry.load("broken").expect("broken before corruption");
        std::fs::write(broken.home.join("agent/state.bin"), b"corrupt sibling")
            .expect("damage sibling only");

        let listed = handle_control(&owner_router, crate::control::Request::AgentList, None);
        let crate::control::Response::Agents { agents } = listed else {
            panic!("healthy list survives a corrupt sibling");
        };
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "adam");
        let adam_profile = registry
            .load("adam")
            .expect("healthy Adam")
            .state
            .record
            .ownership
            .agent()
            .to_string();
        let shown = handle_control(
            &owner_router,
            crate::control::Request::AgentShow {
                agent: adam_profile,
                audience: crate::control::AgentInventoryAudience::Owner,
            },
            None,
        );
        assert!(matches!(shown, crate::control::Response::Agent(_)));
    }
}
