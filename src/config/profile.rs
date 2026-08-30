//! Profiles: one machine, more than one client stack (CLIENT-70).
//!
//! A profile is a **founded device that happens to share hardware**. It has
//! its own identity keypair, its own kinship seeds and therefore its own `prf_`
//! correspondence address, its own Spaces, its own installed Worlds. That is
//! not a design preference; it is what the rest of the kernel already requires.
//! Correspondence collects "on exactly one device, with the seed that opens for
//! it"; the display coordinator derives its profile from the device seed;
//! membership is `device_from_seed`. One keypair behind two live daemons would
//! double-collect a mailbox, publish two routes for one address, and race one
//! coordinator identity — a "one device, two daemons" concept this tree has no
//! model for, and inventing one would be a second model disagreeing with the
//! first exactly when it mattered.
//!
//! So a second profile is a second device, in the way a second laptop is: you
//! invite it, sponsor it, pair it. Nothing about that was ever the defect. The
//! defect was that a **read** minted one — an unrecognised name resolved to a
//! freshly created directory holding a freshly minted keypair, and every
//! surface then reported an empty machine as a healthy one.
//!
//! Three rules follow, and they are what this module is:
//!
//! 1. **A name is an identifier, never a path.** There is no relative,
//!    rooted, or separator-bearing profile to defend against, because those do
//!    not parse. `..`, `/`, `C:`, `./dev` are refused by name.
//! 2. **Reading never creates.** [`Profile::resolve`] answers only for a
//!    profile that was founded, and refuses by name otherwise, saying where it
//!    looked. Founding is [`found`] — an explicit act, reachable from the
//!    client, that mints the device and reports what it lacks.
//! 3. **One selection site.** [`Profile::select`] is the only place the
//!    environment is consulted. The answer is carried in a [`Selection`] and
//!    passed to spawned processes as an explicit `--profile`, so a daemon,
//!    a head or an agent can never disagree with the client about which stack
//!    it belongs to.
//!
//! [`Selection`]: super::Selection

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The environment variable that selects a profile when no flag does.
pub const PROFILE_VAR: &str = "LAIT_PROFILE";

/// Where founded profiles are recorded, under the default config root.
pub const REGISTRY_FILE: &str = "profiles.json";

/// The directory profiles live under, inside each of the two product roots.
const PROFILES_DIR: &str = "profiles";

/// The longest a profile name may be.
const MAX_NAME: usize = 32;

/// A profile name: `[a-z0-9][a-z0-9-]{0,31}`.
///
/// Constructing one is the whole validation. A name is not a path and cannot
/// become one — which is why there is no "is this relative", "is this rooted",
/// "does this escape the product directory" check anywhere downstream. Those
/// questions are unaskable of a value that only holds lowercase letters,
/// digits and hyphens.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileName(String);

impl ProfileName {
    /// The name as it is written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProfileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ProfileName> for String {
    fn from(name: ProfileName) -> Self {
        name.0
    }
}

impl TryFrom<String> for ProfileName {
    type Error = ProfileRefused;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let refuse = |why: &str| ProfileRefused {
            value: value.clone(),
            why: why.to_string(),
        };
        if value.is_empty() {
            return Err(refuse("a profile name cannot be empty"));
        }
        if value.len() > MAX_NAME {
            return Err(refuse(&format!(
                "a profile name is at most {MAX_NAME} characters"
            )));
        }
        if !value.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return Err(refuse(
                "a profile name starts with a lowercase letter or a digit",
            ));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(refuse(
                "a profile name holds only lowercase letters, digits and hyphens \
                 — it is a name, never a path",
            ));
        }
        Ok(Self(value))
    }
}

impl std::str::FromStr for ProfileName {
    type Err = ProfileRefused;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_string())
    }
}

/// A value that is not a profile name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRefused {
    /// What was offered, echoed so a person can see their own typo.
    pub value: String,
    /// Why it is not a name.
    pub why: String,
}

impl std::fmt::Display for ProfileRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} is not a profile name: {}", self.value, self.why)
    }
}

impl std::error::Error for ProfileRefused {}

/// A profile that was named but never founded.
///
/// Carries where it looked, because the failure this replaces was a directory
/// silently created at a path nobody could see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileUnfounded {
    /// The name that was asked for.
    pub name: ProfileName,
    /// The registry consulted.
    pub registry: PathBuf,
    /// The profiles that *are* founded, so the answer can be acted on.
    pub founded: Vec<ProfileName>,
}

impl std::fmt::Display for ProfileUnfounded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no profile named {:?} has been founded on this machine (registry {})",
            self.name.as_str(),
            self.registry.display()
        )?;
        if self.founded.is_empty() {
            write!(f, "; none have been founded")
        } else {
            let names: Vec<_> = self.founded.iter().map(ProfileName::as_str).collect();
            write!(f, "; founded: {}", names.join(", "))
        }
    }
}

impl std::error::Error for ProfileUnfounded {}

/// Which client stack this process belongs to.
///
/// `Default` is the ordinary installation and is byte-for-byte what shipped:
/// the same config root, the same managed state root, the same instance guard.
/// Nothing about an existing machine moves because this type exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Profile {
    /// The ordinary launch.
    #[default]
    Default,
    /// A founded profile of this machine's own.
    Named {
        /// Its name.
        name: ProfileName,
        /// Its config root — identity, Spaces, Worlds, standing.
        config: PathBuf,
        /// Its managed client state root — staged images, remembered screen.
        state: PathBuf,
    },
}

impl Profile {
    /// The one place the environment is consulted.
    ///
    /// `flag` is `--profile`, which wins. Otherwise `$LAIT_PROFILE`. Otherwise
    /// the default stack.
    ///
    /// Refuses the combinations that would split a stack across two roots:
    /// a named profile with `$LAIT_CONFIG_ROOT`, and a named profile with
    /// `$LAIT_HOME` or `--home`. The previous design asserted in a comment that
    /// one knob moved every root together while leaving `$LAIT_CONFIG_ROOT`
    /// able to move one of them; this refuses at the parse instead of claiming
    /// it in prose.
    pub fn select(flag: Option<&str>, self_contained_home: bool) -> Result<Self> {
        let named = match flag {
            Some(raw) => Some(raw.parse::<ProfileName>()?),
            None => match std::env::var_os(PROFILE_VAR) {
                Some(raw) => {
                    let raw = raw.to_str().ok_or_else(|| {
                        anyhow::anyhow!("{PROFILE_VAR} is not valid UTF-8, so it is not a name")
                    })?;
                    if raw.is_empty() {
                        None
                    } else {
                        Some(raw.parse::<ProfileName>()?)
                    }
                }
                None => None,
            },
        };
        let rooted = std::env::var_os("LAIT_CONFIG_ROOT").map(PathBuf::from);
        match (named, rooted) {
            (Some(name), Some(root)) => anyhow::bail!(
                "profile {:?} and LAIT_CONFIG_ROOT ({}) both name a config root. \
                 A profile owns every root its stack has; LAIT_CONFIG_ROOT moves one. \
                 Together they would put this stack's identity in one place and its \
                 client state in another — unset one.",
                name.as_str(),
                root.display()
            ),
            (Some(name), None) if self_contained_home => anyhow::bail!(
                "profile {:?} and a self-contained home (--home / LAIT_HOME) both name \
                 an identity. A self-contained home is a single store with a single \
                 Orbit; a profile is a whole stack. Choose one.",
                name.as_str()
            ),
            (Some(name), None) => Self::resolve(&name),
            // `$LAIT_CONFIG_ROOT` alone is not a stack, it is a relocation of
            // one root — the test hook. It is honoured by `config::config_root`
            // live, because the suites that use it move it between tests inside
            // one process, and it is refused above in combination with a
            // profile so it can never split a real stack.
            (None, _) => Ok(Self::Default),
        }
    }

    /// The founded profile of this name, or a refusal that names it.
    ///
    /// **Creates nothing.** A name nobody founded is a question this cannot
    /// answer, and answering it by making a directory is how a typo became an
    /// empty machine reporting itself healthy.
    pub fn resolve(name: &ProfileName) -> Result<Self> {
        let registry = registry_path()?;
        let founded = founded_names();
        if !founded.contains(name) {
            return Err(ProfileUnfounded {
                name: name.clone(),
                registry,
                founded,
            }
            .into());
        }
        Ok(Self::Named {
            config: config_dir_for(name)?,
            state: state_dir_for(name)?,
            name: name.clone(),
        })
    }

    /// This profile's config root: identity, Spaces, installed Worlds, the
    /// address book, the update standing.
    /// Creating the directory here is not the thing "reads never create"
    /// forbids. That rule is about *minting a profile* — a stack with an
    /// identity of its own — and it is enforced where it belongs, in
    /// [`Profile::resolve`], which refuses a name nobody founded before this
    /// is ever reached. By the time a root is being resolved the stack is
    /// known to exist; making its directory is the same courtesy the default
    /// root has always been given.
    pub fn config_root(&self) -> Result<PathBuf> {
        let dir = match self {
            Self::Default => return default_config_root(),
            Self::Named { config, .. } => config.clone(),
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create config dir {}", dir.display()))?;
        Ok(dir)
    }

    /// This profile's managed client state root — staged daemon images and the
    /// remembered screen.
    ///
    /// `None` for anything but a named profile: the default stack keeps the
    /// location it has always had, and the client resolves that itself. The
    /// engine deliberately does not know where that is — it is the client's
    /// question, and answering it here would put a desktop layout in the
    /// kernel.
    pub fn state_root(&self) -> Option<&Path> {
        match self {
            Self::Named { state, .. } => Some(state),
            _ => None,
        }
    }

    /// The name of this profile, when it has one.
    pub fn name(&self) -> Option<&ProfileName> {
        match self {
            Self::Named { name, .. } => Some(name),
            _ => None,
        }
    }

    /// How to say which stack this is, on a surface or in a refusal.
    pub fn label(&self) -> String {
        match self {
            Self::Default => "default".to_string(),
            Self::Named { name, .. } => name.to_string(),
        }
    }

    /// Whether this is the ordinary installation.
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Default)
    }
}

/// This process's stack, resolved once.
///
/// Sampled at first use and never re-read — the same rule, for the same
/// reason, as `control::BuildFingerprint::here()`: the question is *which
/// stack is this process*, and that is fixed the moment it starts. Re-reading
/// the environment later answers a different question and answers it wrongly,
/// because a variable that changed mid-process would move a root out from
/// under files already open beneath it.
///
/// A refusal is remembered as a refusal. A machine told to use a profile that
/// was never founded must not quietly fall back to the default stack and
/// report itself healthy — that is the whole defect this design exists to
/// remove — so the failure is stored and returned by every root resolution.
static CURRENT: std::sync::OnceLock<std::result::Result<Profile, String>> =
    std::sync::OnceLock::new();

/// Fix this process's stack from an explicit flag, before anything reads it.
///
/// Called by each entry point that has a `--profile` to honour. Returns what
/// was established, which is the first caller's answer: a second call is a
/// no-op, because two answers to "which stack is this process" is the
/// disagreement this type exists to prevent.
pub fn establish(profile: Profile) -> &'static std::result::Result<Profile, String> {
    CURRENT.get_or_init(|| Ok(profile))
}

/// This process's stack.
///
/// Falls back to reading `$LAIT_PROFILE` once, for a process nobody
/// established — a head, a test, an embedded daemon. Never falls back to the
/// default stack on a *refusal*: an unfounded or malformed name is an error
/// every root resolution carries, so a mistyped profile refuses loudly instead
/// of silently serving somebody else's identity.
pub fn current() -> Result<&'static Profile> {
    match CURRENT.get_or_init(|| Profile::select(None, false).map_err(|error| format!("{error:#}")))
    {
        Ok(profile) => Ok(profile),
        Err(why) => anyhow::bail!("{why}"),
    }
}

/// What a freshly founded profile does not have.
///
/// Founding a profile mints a device, and a device with no history lacks
/// specific things a person is about to go looking for. Naming them by class
/// at the moment of founding is the disclosure the previous design owed and
/// did not pay: it switched identity silently and let every surface report the
/// result as an ordinary, healthy, empty machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lacks {
    /// A device identity — this profile is a new device to every peer.
    Identity,
    /// Every Space this machine has founded or entered.
    Spaces,
    /// Every installed World; they are re-fetched from their signed channels.
    InstalledWorlds,
    /// Every local World registration — the "+ Add local World" list.
    LocalWorlds,
    /// The address book, and the correspondence address that goes with it.
    AddressBook,
    /// Display pairings.
    DisplayPairings,
    /// Named agent identities.
    Agents,
}

impl Lacks {
    /// Everything a new profile lacks, in the order worth reading.
    pub const ALL: &'static [Self] = &[
        Self::Identity,
        Self::Spaces,
        Self::InstalledWorlds,
        Self::LocalWorlds,
        Self::AddressBook,
        Self::DisplayPairings,
        Self::Agents,
    ];

    /// One line a person can act on.
    pub fn says(self) -> &'static str {
        match self {
            Self::Identity => {
                "a device identity of its own — to every peer this is a new device, \
                 and it must be invited or sponsored like a second laptop"
            }
            Self::Spaces => "no Spaces; found one, or enter one from an invite",
            Self::InstalledWorlds => {
                "no installed Worlds; they install again from their signed channels"
            }
            Self::LocalWorlds => "no local World registrations",
            Self::AddressBook => "an empty address book, and a correspondence address of its own",
            Self::DisplayPairings => "no display pairings",
            Self::Agents => "no named agent identities",
        }
    }
}

/// What founding produced.
#[derive(Debug, Clone)]
pub struct Founded {
    /// The profile now founded.
    pub profile: Profile,
    /// What it does not have, to be said before anything else happens.
    pub lacks: &'static [Lacks],
}

/// One row of the founded-profile registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// The profile's name.
    pub name: ProfileName,
    /// When it was founded, unix seconds.
    #[serde(default)]
    pub founded_at: u64,
}

/// Found a profile: make its roots and record it.
///
/// The one act that creates. Everything else in this module reads.
///
/// Idempotent — founding a profile that exists returns it, so a launcher that
/// offers "found it" beside a refusal cannot double-mint. The identity keypair
/// is *not* minted here; it is minted the first time that profile's daemon
/// runs, exactly as the default stack's is, so there is one code path for
/// "this device has no key yet".
pub fn found(name: &ProfileName) -> Result<Founded> {
    let config = config_dir_for(name)?;
    let state = state_dir_for(name)?;
    std::fs::create_dir_all(&config)
        .with_context(|| format!("create the profile config root {}", config.display()))?;
    std::fs::create_dir_all(&state)
        .with_context(|| format!("create the profile state root {}", state.display()))?;
    let mut records = records();
    if !records.iter().any(|record| &record.name == name) {
        records.push(Record {
            name: name.clone(),
            founded_at: mechanics::wallclock::now_secs(),
        });
        save_records(&records)?;
    }
    Ok(Founded {
        profile: Profile::Named {
            name: name.clone(),
            config,
            state,
        },
        lacks: Lacks::ALL,
    })
}

/// Un-found a profile: drop its registry row, leaving its data on disk.
///
/// The counterpart of [`found`], and deliberately only half of a delete. What
/// makes a stack *reachable* is the row — a name nobody founded refuses
/// everywhere — so removing it is enough to retire the stack, and it is the
/// part that can be undone by founding again. The directories hold an identity
/// keypair and somebody's Spaces; removing those is a data deletion, which
/// this tree makes a separate act with its own confirmation and never a side
/// effect of tidying a list.
///
/// Answers whether a row was actually removed, so a caller can tell "retired"
/// from "there was nothing by that name".
pub fn forget(name: &ProfileName) -> Result<bool> {
    let mut records = records();
    let before = records.len();
    records.retain(|record| &record.name != name);
    if records.len() == before {
        return Ok(false);
    }
    save_records(&records)?;
    Ok(true)
}

/// Every founded profile, for a surface that lists them and for resolving the
/// owner of a store.
pub fn founded_names() -> Vec<ProfileName> {
    records().into_iter().map(|record| record.name).collect()
}

/// Every founded profile as a resolved [`Profile`], plus the default.
///
/// The default is always first and always present: it is the stack an
/// installation ships as, whether or not anything was ever founded beside it.
pub fn all() -> Vec<Profile> {
    let mut out = vec![Profile::Default];
    for name in founded_names() {
        if let Ok(profile) = Profile::resolve(&name) {
            out.push(profile);
        }
    }
    out
}

/// The rows as recorded. Best-effort: an unreadable registry is an empty one,
/// because a corrupt navigation file must not make a machine unusable.
fn records() -> Vec<Record> {
    let Ok(path) = registry_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_records(records: &[Record]) -> Result<()> {
    let path = registry_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let encoded = serde_json::to_vec_pretty(records).context("encode the profile registry")?;
    let staged = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&staged, encoded).with_context(|| format!("write {}", staged.display()))?;
    std::fs::rename(&staged, &path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// The registry lives under the **default** config root, not under any
/// profile: it is the machine's list of stacks, and a list that lived inside
/// one of them could not be read to find the others.
fn registry_path() -> Result<PathBuf> {
    Ok(default_config_root()?.join(REGISTRY_FILE))
}

/// The default stack's config root — the shipped location, unchanged.
fn default_config_root() -> Result<PathBuf> {
    let dir = project_dirs()?.config_dir().to_path_buf();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create config dir {}", dir.display()))?;
    Ok(dir)
}

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "nixi", "lait")
        .context("could not determine config directory")
}

/// A profile's config root, beside the default one.
///
/// Under `config_dir()/profiles/<name>` — the *config* directory, because this
/// holds the identity keypair and the Spaces, and those belong wherever the
/// default stack's keypair belongs. Not reached by walking to a parent: on
/// Linux `config_dir()` is `~/.config/lait` and on macOS it is
/// `~/Library/Application Support/dev.nixi.lait`, so a `.parent()` hop leaves
/// the product's own namespace entirely and drops a keypair into a directory
/// shared with every other application.
fn config_dir_for(name: &ProfileName) -> Result<PathBuf> {
    Ok(project_dirs()?
        .config_dir()
        .join(PROFILES_DIR)
        .join(name.as_str()))
}

/// A profile's managed client state root.
///
/// Under `data_local_dir()/profiles/<name>`: **local**, never roaming. It holds
/// staged daemon images — binaries — and a roaming profile would sync them
/// between machines through Windows roaming or OneDrive Known Folder Move.
fn state_dir_for(name: &ProfileName) -> Result<PathBuf> {
    Ok(project_dirs()?
        .data_local_dir()
        .join(PROFILES_DIR)
        .join(name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::poison::LockRecovering as _;

    /// `LAIT_CONFIG_ROOT` is process-global, so the tests that set it cannot
    /// run beside each other or beside anything reading a root.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The whole defence against paths is that a name cannot be one. Every
    /// value here was a real hazard in the design this replaces: `/` and `C:`
    /// classified as bare names and `Path::join` *replaced* with them, putting
    /// a config root at the filesystem root; `./dev` resolved against whatever
    /// working directory a process happened to have.
    #[test]
    fn a_profile_name_is_an_identifier_and_never_a_path() {
        for refused in [
            "/", "\\", "C:", "..", ".", "./dev", "../dev", "a/b", "a\\b", "", " ", "Dev", "dev ",
            "-dev", "dev/", "café", "a:b",
        ] {
            assert!(
                refused.parse::<ProfileName>().is_err(),
                "{refused:?} parsed as a profile name, so it can reach a path join"
            );
        }
        for accepted in ["dev", "d", "scratch-2", "a-b-c", "0", "x9"] {
            assert!(
                accepted.parse::<ProfileName>().is_ok(),
                "{accepted:?} is a name and was refused"
            );
        }
        assert!(
            "x".repeat(MAX_NAME + 1).parse::<ProfileName>().is_err(),
            "an unbounded name reaches the filesystem's own limits"
        );
    }

    /// A profile's roots live inside the product's own directories on every
    /// platform. Asserted as a *prefix* — the previous design asserted the
    /// suffix (`ends_with("profiles/dev")`), which passes identically whether
    /// the root is inside the product namespace or at the top of a shared one.
    #[test]
    fn a_profile_root_is_inside_the_product_directories_on_every_platform() {
        let name: ProfileName = "dev".parse().expect("a name");
        let dirs = project_dirs().expect("product directories");
        let config = config_dir_for(&name).expect("a config root");
        let state = state_dir_for(&name).expect("a state root");
        assert!(
            config.starts_with(dirs.config_dir()),
            "the profile config root escaped the product config directory: {}",
            config.display()
        );
        assert!(
            state.starts_with(dirs.data_local_dir()),
            "the profile state root escaped the product local-data directory: {}",
            state.display()
        );
    }

    /// The invariant the previous design asserted in prose and did not
    /// enforce: one knob moves every root, so no combination of selectors can
    /// leave a stack half-moved.
    ///
    /// Asserted **with `LAIT_CONFIG_ROOT` set**, which is the part that
    /// matters. The test this replaces removed that variable before asserting
    /// — it deleted the one thing that would have falsified the claim.
    #[test]
    fn a_profile_and_a_config_root_are_refused_together() {
        let _guard = ENV_LOCK.lock_recovering();
        std::env::set_var(
            "LAIT_CONFIG_ROOT",
            std::env::temp_dir().join("lait-split-test"),
        );
        let refused = Profile::select(Some("dev"), false);
        std::env::remove_var("LAIT_CONFIG_ROOT");
        let error = refused.expect_err(
            "a profile and a config root were accepted together, so a stack can still be split \
             — identity in one place, client state in another",
        );
        let said = format!("{error:#}");
        assert!(
            said.contains("LAIT_CONFIG_ROOT") && said.contains("dev"),
            "the refusal did not name both halves of the conflict: {said}"
        );
    }

    /// A profile and a self-contained home are two different answers to "which
    /// identity", and a self-contained home collapses a stack to one store
    /// with one Orbit. Together they are a contradiction, not a combination.
    #[test]
    fn a_profile_and_a_self_contained_home_are_refused_together() {
        let _guard = ENV_LOCK.lock_recovering();
        std::env::remove_var("LAIT_CONFIG_ROOT");
        assert!(
            Profile::select(Some("dev"), true).is_err(),
            "a profile was accepted alongside --home, which would collapse the stack's catalog"
        );
    }

    /// Reading never creates. The failure this replaces resolved an
    /// unrecognised name to a freshly made directory holding a freshly minted
    /// keypair, and then every surface reported that empty machine as healthy.
    #[test]
    fn an_unfounded_profile_is_refused_and_nothing_is_created() {
        let _guard = ENV_LOCK.lock_recovering();
        std::env::remove_var("LAIT_CONFIG_ROOT");
        let name: ProfileName = format!("unfounded-{}", std::process::id())
            .parse()
            .expect("a name");
        let config = config_dir_for(&name).expect("a config path");
        let state = state_dir_for(&name).expect("a state path");
        let _ = std::fs::remove_dir_all(&config);
        let _ = std::fs::remove_dir_all(&state);

        let error = Profile::resolve(&name).expect_err("an unfounded profile must refuse");
        let said = format!("{error:#}");
        assert!(
            said.contains(name.as_str()),
            "the refusal did not name the profile asked for: {said}"
        );
        assert!(
            !config.exists() && !state.exists(),
            "resolving an unfounded profile created its roots, which is how a typo becomes an \
             empty machine that reports itself healthy"
        );
    }

    /// The default stack must be exactly what shipped. If this drifts, every
    /// installed machine's identity moves.
    #[test]
    fn the_default_profile_is_the_shipped_location() {
        let dirs = project_dirs().expect("product directories");
        assert_eq!(
            Profile::Default.config_root().expect("a default root"),
            dirs.config_dir(),
            "the default config root moved, which relocates every existing identity"
        );
        assert_eq!(
            Profile::Default.state_root(),
            None,
            "the engine answered for the client's state root on the default stack"
        );
    }
}
