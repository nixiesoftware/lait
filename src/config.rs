//! On-disk state: identity, store discovery, and layered local settings.
//!
//! Two locations (DUR-5): a **global identity** (the `secret.key`, under the
//! platform config dir) and a **per-repo space store** (the `.lait/`
//! dir discovered git-style by walking up from the cwd). One identity spans every
//! repo-bound store, like a single `git` `user.email` across many repos.
//! `$LAIT_HOME` collapses both into one self-contained dir (tests, `--home`,
//! advanced setups).
//!
//! Discovery **never creates a store**: spaces come into being only through the
//! two host-plane formation requests (`HostSpaceFound` founds, `HostSpaceEnter`
//! bootstraps from an invite) via [`store_dir_for_init`]. Everything else
//! resolves an existing store or fails with [`NoStoreHere`] — the silent
//! decoy-store auto-create (and the directory-trap guard rail it required) is
//! gone by design.
//!
//! Settings are git-style layered key/value maps ([`Settings`]): a global
//! `config.json` under the config root and a per-store `config.json` inside
//! `.lait/`, nearest (store) wins. Keys are validated against the static
//! [`KEYS`] table; the `space.*` namespace is reserved for future settings
//! synced through the Catalog.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::registry::{agents_base, Registry, SessionMap};

pub mod profile;

pub use profile::{Lacks, Profile, ProfileName, ProfileRefused, ProfileUnfounded};

/// The base config directory (ignoring `$LAIT_HOME`) — where the named
/// identity registry (`agents/`) and the session map live.
///
/// Answers for **this process's stack**: the default installation, or the
/// profile it was started under. The profile is resolved once per process
/// (`profile::current`) rather than read here, so two calls in one process can
/// never disagree, and a stack is never split by a variable changing mid-run.
/// A selection that addresses a *different* stack — an agent binding routed to
/// the profile that owns a store — carries its own [`Profile`] and does not
/// come through here.
pub fn config_root() -> Result<PathBuf> {
    // `$LAIT_CONFIG_ROOT` is the test hook, and it is read live rather than
    // through the memoized profile because the suites that use it relocate it
    // between tests inside one process. It relocates this one root and nothing
    // else, which is why `Profile::select` refuses it in combination with a
    // named profile: a stack whose identity moved while its client state did
    // not is the defect profiles exist to prevent, and it is refused where it
    // would be introduced rather than asserted in prose here.
    if let Some(root) = std::env::var_os("LAIT_CONFIG_ROOT") {
        let dir = PathBuf::from(root);
        fs::create_dir_all(&dir).with_context(|| format!("create config dir {}", dir.display()))?;
        return Ok(dir);
    }
    profile::current()?.config_root()
}

/// The registry of named identities, and the session→identity map beside it.
pub fn registry() -> Result<(Registry, PathBuf)> {
    let root = config_root()?;
    let base = agents_base(&root);
    fs::create_dir_all(&base)?;
    Ok((Registry::new(base), root.join("sessions.json")))
}

/// The per-repo space store directory name, discovered git-style.
const STORE_DIR: &str = ".lait";

/// Walk up from `start` for an existing `.lait/` space store, so a
/// command run anywhere inside a repo binds that repo's store (like `git`
/// finding `.git`). Returns the store dir, or `None` if none exists above `start`.
fn find_store_dir(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(STORE_DIR);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Canonicalize a path so a head and the daemon it spawns hash the *same* store
/// path (the control channel + single-instance lock are keyed on it). Falls back
/// to the input if canonicalization fails (e.g. the dir was just created).
///
/// Reachable from the host plane because admission is keyed on the same digest:
/// a caller-supplied spelling of a served store must resolve to the Orbit that
/// store is registered under, or a custody check aimed at it misses.
///
/// `pub` for the integration suite, which hand-builds the paths the daemon
/// reaches through [`Selection`] and has to agree with it about which directory
/// a spelling names — a CI runner's temp dir sits under an 8.3 alias.
pub fn canonical(p: &Path) -> PathBuf {
    resolved(p).unwrap_or_else(|| p.to_path_buf())
}

/// The directory a spelling actually names, or `None` when the filesystem
/// cannot say.
///
/// [`canonical`]'s fallback is a convenience for a path that is about to exist.
/// A check that decides *whose* directory this is cannot take it: a spelling
/// with a component that does not exist resolves to nothing, and comparing the
/// spelling instead compares a string that need not describe the directory the
/// write will land in — `<somewhere>/nope/../agents/scout` is not under
/// `agents/` as text, and is exactly that directory on disk.
pub(crate) fn resolved(p: &Path) -> Option<PathBuf> {
    fs::canonicalize(p).ok().map(strip_extended_prefix)
}

/// On Windows, `fs::canonicalize` returns an extended-length `\\?\C:\…` path.
/// That prefix breaks a lot of tooling and Windows APIs (and would flow into the
/// daemon we spawn), so strip it for ordinary drive paths — leaving genuine UNC
/// (`\\?\UNC\…`) paths untouched. No-op on unix.
#[cfg(windows)]
fn strip_extended_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        // Only unwrap plain `X:\…` drive paths, not `\\?\UNC\server\share`.
        let b = rest.as_bytes();
        if b.get(1) == Some(&b':') {
            return PathBuf::from(rest);
        }
    }
    p
}
#[cfg(not(windows))]
fn strip_extended_prefix(p: PathBuf) -> PathBuf {
    p
}

/// Drop a `.gitignore` into a fresh store so the parent repo never accidentally
/// commits this node's local space replica + daemon state — it syncs over
/// P2P, and (like `.git/`) is per-node, not source. No-op if one already exists.
fn ensure_store_gitignore(store: &Path) {
    let p = store.join(".gitignore");
    if !p.exists() {
        let _ = fs::write(
            &p,
            "# lait local store — per-node, synced over P2P, do not commit\n*\n",
        );
    }
}

/// What an invocation explicitly selected, carried as a value.
///
/// `--home` and `--orbit` used to be applied by writing `$LAIT_HOME` /
/// `$LAIT_STORE` back into this process, which made the selection a property of
/// the process rather than of the call. A long-lived head serves many
/// identities out of one process, so a selection stored that way is every
/// tenant's selection; the last caller to write it wins for everybody.
///
/// [`Selection::default`] is the ambient one — whatever the environment and the
/// cwd already say — which is what the daemon and `serve` come up with.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// A self-contained identity+store directory (`--home`).
    pub identity: Option<PathBuf>,
    /// An already-resolved store directory (`--orbit`).
    pub store: Option<PathBuf>,
    /// The stack this selection addresses, when it is not this process's own.
    ///
    /// `None` means "whatever stack this process belongs to" — the ordinary
    /// case, resolved through [`config_root`]. `Some` is how one process
    /// addresses *another* profile's identity: an agent that found a store on
    /// disk resolves which profile registered it and binds a selection for
    /// that profile, so the daemon it reaches is the store's owner rather than
    /// whichever stack the editor happened to launch it in.
    pub profile: Option<Profile>,
}

impl Selection {
    /// A selection that pins one self-contained identity directory.
    pub fn for_identity(identity: impl Into<PathBuf>) -> Self {
        Self {
            identity: Some(identity.into()),
            store: None,
            profile: None,
        }
    }

    /// A selection that addresses one profile's whole stack.
    ///
    /// Not self-contained: a profile is an ordinary many-Space identity that
    /// happens to live somewhere else, so it keeps its own catalog, its own
    /// registry and its own `.lait` discovery. That is the difference from
    /// `--home`, which collapses a stack into a single store with a single
    /// Orbit, and it is why the two are refused in combination.
    pub fn for_profile(profile: Profile) -> Self {
        Self {
            identity: None,
            store: None,
            profile: Some(profile),
        }
    }

    /// The stack this selection addresses.
    pub fn profile(&self) -> Result<Profile> {
        match &self.profile {
            Some(profile) => Ok(profile.clone()),
            None => Ok(profile::current()?.clone()),
        }
    }

    /// Where this invocation's selection came from, for orientation readouts.
    pub fn source(&self) -> String {
        if self.identity.is_some() {
            "--home".into()
        } else if self.store.is_some() {
            "--orbit".into()
        } else if std::env::var_os("LAIT_HOME").is_some() {
            "LAIT_HOME".into()
        } else if std::env::var_os("LAIT_STORE").is_some() {
            "LAIT_STORE".into()
        } else if let Some(profile) = self
            .profile
            .clone()
            .or_else(|| profile::current().ok().cloned())
            .filter(|profile| !profile.is_default())
        {
            // Named, because an orientation readout that could not say which
            // stack it was reading answered "none" for a profiled client and
            // for an ordinary one alike — the same word for two different
            // machines.
            format!("profile {}", profile.label())
        } else if self.existing_home().is_some() {
            "cwd".into()
        } else {
            "none".into()
        }
    }

    /// The self-contained home as it was *spelled*, before the filesystem has a
    /// say. Only the questions that are about the selection itself — was one
    /// made, and where did it come from — may use this.
    fn named_home(&self) -> Option<PathBuf> {
        self.identity
            .clone()
            .or_else(|| std::env::var_os("LAIT_HOME").map(PathBuf::from))
    }

    /// The self-contained home this selection binds, if any: the explicit
    /// `--home`, else an ambient `$LAIT_HOME`.
    ///
    /// `None` is the ordinary per-user identity, and the distinction is what a
    /// daemon spawn needs: passing the shared config root as `--home` would
    /// collapse the global catalog into one self-contained identity.
    ///
    /// Canonicalized, because a store's [`crate::daemon::LocalOrbitId`] is a
    /// digest of the path it is registered under and formation registers the
    /// canonical spelling. A `--home` given relatively, through a symlink
    /// (`/tmp` is `/private/tmp` on macOS) or as an 8.3 short name would be the
    /// same directory under a different id: the catalog drops the row as
    /// belonging to another identity and the store this node just formed
    /// answers "no such local Orbit" to its own next command. Every other
    /// branch here already canonicalizes for the same reason.
    pub fn self_contained_home(&self) -> Option<PathBuf> {
        self.named_home().map(|dir| canonical(&dir))
    }

    /// The self-contained home, created first so its canonical spelling is
    /// knowable at all — `fs::canonicalize` of a directory that does not exist
    /// yet fails, and a first founding would otherwise register a path no later
    /// call derives.
    fn prepared_self_contained_home(&self) -> Result<Option<PathBuf>> {
        let Some(dir) = self.named_home() else {
            return Ok(None);
        };
        fs::create_dir_all(&dir).with_context(|| format!("create home {}", dir.display()))?;
        Ok(Some(canonical(&dir)))
    }

    /// Whether this selection is a self-contained identity home (identity key
    /// beside its store) rather than the shared per-user identity.
    pub fn self_contained(&self) -> bool {
        self.named_home().is_some()
    }

    /// The directory holding this selection's identity `secret.key`.
    pub fn identity_dir(&self) -> Result<PathBuf> {
        match self.prepared_self_contained_home()? {
            Some(dir) => Ok(dir),
            // A selection that names a profile answers for *that* stack; one
            // that does not answers for this process's. Routing already flows
            // from here — `daemon_home` is derived from it, and
            // `daemon::Client::for_selection` from that — so carrying the
            // profile in the selection is what makes an agent reach the
            // daemon that owns its store rather than the default one.
            None => match &self.profile {
                Some(profile) => profile.config_root(),
                None => config_root(),
            },
        }
    }

    /// The private runtime home of this selection's Lait daemon.
    pub fn daemon_home(&self) -> Result<PathBuf> {
        let dir = self.identity_dir()?.join("daemon");
        fs::create_dir_all(&dir)
            .with_context(|| format!("create Lait daemon home {}", dir.display()))?;
        Ok(dir)
    }

    /// The store this selection WOULD bind if it already exists — never
    /// creating one.
    pub fn existing_home(&self) -> Option<PathBuf> {
        if let Some(dir) = self.self_contained_home() {
            return Some(dir);
        }
        if let Some(store) = &self.store {
            return Some(canonical(store));
        }
        if let Some(p) = std::env::var_os("LAIT_STORE") {
            return Some(canonical(&PathBuf::from(p)));
        }
        let cwd = std::env::current_dir().ok()?;
        find_store_dir(&cwd).map(|s| canonical(&s))
    }

    /// Resolve the **existing** space store for this selection, never creating
    /// one. A discovery miss is a typed [`NoStoreHere`].
    pub fn resolve_existing_store(&self) -> Result<PathBuf> {
        if let Some(dir) = self.prepared_self_contained_home()? {
            return Ok(dir);
        }
        if let Some(store) = &self.store {
            fs::create_dir_all(store)?;
            return Ok(canonical(store));
        }
        if let Some(p) = std::env::var_os("LAIT_STORE") {
            let dir = PathBuf::from(p);
            fs::create_dir_all(&dir)?;
            return Ok(canonical(&dir));
        }
        let cwd = std::env::current_dir().context("get current dir")?;
        match find_store_dir(&cwd) {
            Some(s) => Ok(canonical(&s)),
            None => Err(anyhow::Error::new(NoStoreHere { cwd })),
        }
    }

    /// Resolve the store for the stdio agent head: everything
    /// [`Self::resolve_existing_store`] accepts, then the Orbit registry when
    /// the directory names none.
    ///
    /// An agent head runs wherever its harness spawned it, so "cd into a
    /// space" is advice no agent config can follow — while the registry
    /// already records every local Orbit this device serves. When it holds
    /// exactly one whose store is really there, that Orbit is the only thing
    /// the head could mean, and it binds. Zero, several, or a sole entry whose
    /// store is gone stay the typed refusal: this is selection among what
    /// exists, never a guess between candidates and never a creation.
    pub fn resolve_for_agent(&self) -> Result<PathBuf> {
        self.bind_for_agent().map(|bound| bound.store)
    }

    /// The store an agent binds, **and the stack that owns it**.
    ///
    /// The store comes first and the environment second, because only one of
    /// them is reliable here. An editor parents `lait mcp` with an environment
    /// the editor controls; the store on disk was registered by exactly one
    /// stack, and that registration is true whether or not any client is
    /// running. So a store found by walking up from the working directory
    /// decides which daemon this session talks to — never the reverse.
    pub fn bind_for_agent(&self) -> Result<AgentBinding> {
        let miss = match self.resolve_existing_store() {
            Ok(store) => {
                // Found on disk. Whichever stack registered it is the stack
                // whose daemon can answer for it; routing to this process's
                // own would reach a catalog that has never seen this
                // directory and be told "no such local Orbit" about a store
                // the agent is standing in.
                let profile = crate::orbits::owner_of(&store);
                return Ok(AgentBinding { store, profile });
            }
            Err(error) if error.downcast_ref::<NoStoreHere>().is_some() => error,
            Err(error) => return Err(error),
        };
        // No store on disk, so the environment's stack is all there is — and
        // its registry alone, never a union across stacks. Two profiles each
        // holding one Space must not add up to "exactly one".
        let entries = crate::orbits::list();
        let [entry] = entries.as_slice() else {
            return Err(miss);
        };
        let store = PathBuf::from(&entry.path);
        match crate::orbital::discover_space(&store) {
            crate::orbital::SpaceStore::One(_) => Ok(AgentBinding {
                store: canonical(&store),
                profile: None,
            }),
            _ => Err(miss),
        }
    }

    /// The store directory a creation verb will populate under `dir`: this
    /// selection's self-contained home if it has one, else `<dir>/.lait`.
    pub fn store_dir_for_init(&self, dir: &Path) -> Result<PathBuf> {
        if let Some(home) = self.prepared_self_contained_home()? {
            ensure_store_gitignore(&home);
            return Ok(home);
        }
        store_dir_under(dir)
    }
}

/// Typed "no space store here" error, so a head can tell "nothing to bind"
/// apart from real I/O failures and answer with the guided next step (found a
/// space, enter one from an invite, or point at an existing store with
/// `--orbit`) instead of a bare failure.
#[derive(Debug)]
pub struct NoStoreHere {
    /// The directory discovery started from.
    pub cwd: PathBuf,
}
impl std::fmt::Display for NoStoreHere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no lait space found here (searched up from {})",
            self.cwd.display()
        )
    }
}
impl std::error::Error for NoStoreHere {}

/// What an agent session binds: a store, and the stack whose daemon owns it.
#[derive(Debug, Clone)]
pub struct AgentBinding {
    /// The store this session addresses.
    pub store: PathBuf,
    /// The stack that registered it. `None` means "this process's own" —
    /// either the store is registered here, or the sole-Orbit fallback
    /// answered from this stack's registry.
    pub profile: Option<Profile>,
}

impl AgentBinding {
    /// A selection that reaches the daemon owning this store.
    pub fn selection(&self) -> Selection {
        Selection {
            identity: None,
            store: Some(self.store.clone()),
            profile: self.profile.clone(),
        }
    }
}

/// Resolve the **existing** space store for this invocation — never
/// creating one. Precedence:
///   1. an explicit named identity (`$LAIT_AS`) — a self-contained home
///      under the identity registry (created on demand: it is an identity
///      container, not a space).
///   2. `$LAIT_HOME` — explicit, self-contained override (identity + store
///      in one dir): `lait daemon --home`, tests, advanced setups.
///   3. `$LAIT_STORE` — the pin a launcher passes to the daemon it spawns (and
///      what `--orbit` resolves to), so both bind the exact store the launcher
///      chose, independent of cwd.
///   4. git-style discovery: walk up from the cwd for a `.lait/`.
///
/// A discovery miss is a typed [`NoStoreHere`] error — stores are only born in
/// [`store_dir_for_init`], which only the two formation requests reach. The
/// identity key is resolved separately ([`identity_dir`]) — global by default,
/// so one identity spans every repo-bound store.
pub fn resolve_existing_store(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(name) = explicit {
        let (reg, _) = registry()?;
        let home = reg.home_for(name);
        fs::create_dir_all(&home)?;
        return Ok(home);
    }
    Selection::default().resolve_existing_store()
}

/// Create (or reuse) the `.lait/` store dir under `dir` — the raw creation
/// primitive, ignoring `$LAIT_HOME` so that a caller-named directory wins over
/// an ambient one.
pub fn store_dir_under(dir: &Path) -> Result<PathBuf> {
    prepare_store_dir(&dir.join(STORE_DIR))
}

/// Create (or reuse) an **exact** store directory and drop its `.gitignore`.
///
/// The form a formation request takes: by the time a path crosses the daemon
/// boundary the caller has already decided which directory becomes the store,
/// and appending `.lait` to it a second time would put the store somewhere
/// nobody asked for.
pub fn prepare_store_dir(store: &Path) -> Result<PathBuf> {
    fs::create_dir_all(store).with_context(|| format!("create store dir {}", store.display()))?;
    let store = canonical(store);
    ensure_store_gitignore(&store);
    Ok(store)
}

/// The store directory a formation request will populate: an explicit
/// `$LAIT_HOME` if set, else `<dir>/.lait`. Creates the directory and
/// drops the store `.gitignore`. Together with [`store_dir_under`], the ONLY
/// paths that bring a store into existence.
pub fn store_dir_for_init(dir: &Path) -> Result<PathBuf> {
    Selection::default().store_dir_for_init(dir)
}

/// The directory holding this node's identity `secret.key`. A self-contained
/// home (`$LAIT_HOME`) keeps the key beside its store; otherwise the key is
/// **global** (under [`config_root`]) so one identity spans every repo-bound
/// store — like one `git` `user.email` across many repos.
pub fn identity_dir() -> Result<PathBuf> {
    Selection::default().identity_dir()
}

/// Private runtime home of the identity-scoped Lait daemon.
///
/// Kept below the identity directory, but distinct from every Orbit home: the
/// host process owns the catalog-wide control socket and process lock while
/// each active [`crate::orbital::StationHost`] independently holds its Orbit
/// lease. A self-contained `$LAIT_HOME` therefore still gets one daemon without
/// colliding with the Station occupying that same directory.
pub fn lait_daemon_home() -> Result<PathBuf> {
    Selection::default().daemon_home()
}

/// The store this invocation WOULD bind if it already exists — WITHOUT creating
/// one. For commands like `update` that must not spawn a stray `.lait/` just
/// to look for a running daemon.
pub fn existing_home() -> Option<PathBuf> {
    Selection::default().existing_home()
}

/// Where a head offers to put a store when nobody has said where.
///
/// A terminal defaulted to its working directory. A browser has none, and an
/// empty path box is a founding form nobody can complete — so the node names a
/// directory it can always write, under the same config root every other piece
/// of node-local state lives in (and which `$LAIT_CONFIG_ROOT` relocates, so a
/// test never proposes the developer's real one). Creates nothing: the caller
/// picks a name, and formation makes the directory.
pub fn spaces_root() -> PathBuf {
    match config_root() {
        Ok(root) => root.join("spaces"),
        // A config root we cannot even name is a failure the *next* request
        // reports properly; a suggestion is not the place to raise it.
        Err(_) => PathBuf::from("spaces"),
    }
}

/// Names of all registered identities.
pub fn list_identities() -> Result<Vec<String>> {
    let (reg, _) = registry()?;
    Ok(reg.list())
}

/// Bind the current session to a named identity (creating it if needed) so this
/// session — and future resumes of it — recall that identity. Returns its home.
pub fn bind_session(name: &str) -> Result<PathBuf> {
    let (reg, sessions) = registry()?;
    let home = reg.home_for(name);
    fs::create_dir_all(&home)?;
    if let Ok(sid) = std::env::var("CLAUDE_CODE_SESSION_ID") {
        SessionMap::load(sessions).set(&sid, name)?;
    }
    Ok(home)
}

/// A short, stable hex token derived from a home path. Used to name the control
/// channel uniquely per home (so several `$LAIT_HOME` nodes on one machine
/// never collide) — as a filesystem socket name on unix and a named-pipe name on
/// Windows. Both the daemon and its clients hash the same home, so they agree.
pub fn home_hash(home: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    home.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Filesystem path to the control socket for the running daemon (unix only; on
/// Windows the control channel is a named pipe, see `control::control_name`).
///
/// AF_UNIX socket paths are capped at 104 bytes on macOS (`sun_path`; 108 on
/// Linux). The per-agent home under `~/Library/Application Support/…/agents/
/// agent-XXXXXX/` can exceed that for longer usernames — the daemon then fails
/// to `bind()` and never comes online ("daemon did not come online in time").
/// When the natural in-home path would be too long, fall back to a short, stable
/// path in the temp dir derived from a hash of the home. Both the daemon and the
/// client resolve this the same way (same binary, same home), so they agree on
/// where to bind/connect.
#[cfg(unix)]
pub fn socket_path(home: &Path) -> PathBuf {
    let direct = home.join("control.sock");
    // Leave margin below the 104-byte macOS limit (path bytes + NUL terminator).
    const MAX_SUN_PATH: usize = 100;
    if direct.as_os_str().len() <= MAX_SUN_PATH {
        return direct;
    }

    // On iOS every writable path lives under the app container, so even the
    // temp dir barely fits sun_path. Two rescues, both iOS-only: `/private/var`
    // and `/var` are the same file (a firmlink), and the shorter spelling buys
    // eight bytes; and the hash drops to eight hex — 32 bits is plenty to keep
    // the handful of homes one sandbox can hold apart.
    #[cfg(target_os = "ios")]
    {
        let tmp = std::env::temp_dir();
        let tmp = match tmp.strip_prefix("/private") {
            Ok(rest) => Path::new("/").join(rest),
            Err(_) => tmp,
        };
        return tmp.join(format!("gc-{}.sock", &home_hash(home)[..8]));
    }

    #[cfg(not(target_os = "ios"))]
    std::env::temp_dir().join(format!("gc-{}.sock", home_hash(home)))
}

/// Path to the single-instance lock file for a home.
fn lock_path(home: &Path) -> PathBuf {
    home.join("daemon.lock")
}

/// Path to the file naming the daemon that holds a home. Deliberately not the
/// lock file — see `acquire_daemon_lock`.
fn pid_path(home: &Path) -> PathBuf {
    home.join("daemon.pid")
}

/// A held single-instance lock for a daemon home. The underlying OS advisory
/// lock (`flock(2)` on unix, `LockFileEx` on Windows, via `fs2`) is released
/// automatically when this value is dropped or the process exits — even on a
/// crash — so the lock can never go stale.
#[derive(Debug)]
pub struct DaemonLock {
    _file: fs::File,
}

/// Acquire the exclusive operational lock for a home.
///
/// A Lait daemon uses this for its process home; a StationHost runner uses it
/// for an Orbit home. In both cases there is at most one live owner for that
/// exact resource.
pub fn acquire_daemon_lock(home: &Path) -> Result<DaemonLock> {
    use fs2::FileExt;
    let path = lock_path(home);
    let file =
        fs::File::create(&path).with_context(|| format!("create lock file {}", path.display()))?;
    // Exclusive, non-blocking advisory lock held by this open file handle. The
    // OS releases it when the handle closes (process exit or crash), so the lock
    // can never go stale. A second daemon for the same home gets a would-block
    // error here and bails instead of clobbering the live one. `fs2` maps to
    // flock(2) on unix and LockFileEx on Windows — same guarantee, portably.
    file.try_lock_exclusive().map_err(|_| {
        anyhow!(
            "another lait daemon is already running for this home ({})",
            home.display()
        )
    })?;
    // Name ourselves *beside* the lock, never inside it. The lock says only that
    // *someone* holds this home; the pid says who, which is what lets a client
    // clean up a daemon that has stopped answering (`Request::Stop` alone is not
    // enough — a v0.4.8-era daemon acknowledges `stop` and keeps running, see
    // `node::signal_shutdown`, so the fallback needs a signal target).
    //
    // Writing it into the lock file itself is a unix-only assumption, not a
    // unix-only API — which is why it compiled everywhere and only failed on
    // Windows CI. `flock(2)` is *advisory*, so any handle may read a locked file;
    // `LockFileEx` is **mandatory** and blocks other handles from reading the
    // locked range, making the pid unreadable by precisely the client that needs
    // it. A separate file is readable on both.
    //
    // Best-effort: a failure here costs the cleanup path, not the daemon.
    let _ = fs::write(pid_path(home), std::process::id().to_string());
    Ok(DaemonLock { _file: file })
}

/// Whether this home's daemon lock is free, without taking it.
///
/// A **pure probe**: it opens the lock read-only, tries a shared acquisition,
/// and releases at once. It creates nothing and writes nothing.
///
/// That is the whole difference from asking the same question with
/// [`acquire_daemon_lock`], which is what the wait-for-a-daemon-to-leave path
/// used to do — and which, on every call, stamped the *probing* process's pid
/// into `daemon.pid`. The file whose entire purpose is naming the daemon that
/// holds this home would then name a process that is not a daemon and is about
/// to exit, and pids are reused. A probe that answers a question must not
/// change the answer to a different one.
pub fn daemon_lock_free(home: &Path) -> bool {
    use fs2::FileExt;
    let path = lock_path(home);
    if !path.exists() {
        return true;
    }
    let Ok(file) = fs::File::open(&path) else {
        // Unreadable is not free. Reporting "nobody holds it" because the file
        // could not be opened is exactly the kind of absence-read-as-zero this
        // tree refuses everywhere else.
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            true
        }
        Err(_) => false,
    }
}

/// The pid of the daemon that last held this home, if one recorded itself.
///
/// Only meaningful once a caller has *independently* established that a daemon is
/// there (`control::probe` answering anything but `Absent`). This file outlives a
/// crashed daemon, and a pid is reused; the probe is what rules out signalling a
/// stranger. A daemon that predates the stamp simply returns `None`.
pub fn daemon_pid(home: &Path) -> Option<u32> {
    fs::read_to_string(pid_path(home)).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe must answer a question without changing the answer to another
    /// one.
    ///
    /// The path this replaces asked "has the daemon left?" by *taking* the
    /// daemon lock — and taking it stamps the caller's own pid into
    /// `daemon.pid`. The file whose only purpose is naming the daemon that
    /// holds this home would then name a process that is not a daemon and is
    /// about to exit, on every launch that met a daemon it wanted gone. Pids
    /// are reused, and the next reader has no way to know.
    #[test]
    fn the_daemon_lock_probe_never_writes_the_pid_file() {
        let dir = std::env::temp_dir().join(format!("gc-probe-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("a scratch home");
        let _ = fs::remove_file(pid_path(&dir));

        assert!(daemon_lock_free(&dir), "an unheld home read as occupied");
        assert!(
            !pid_path(&dir).exists(),
            "probing a free home wrote a pid file, inventing a daemon that does not exist"
        );

        let held = acquire_daemon_lock(&dir).expect("take the home");
        let stamped = daemon_pid(&dir).expect("the holder named itself");
        for _ in 0..20 {
            assert!(
                !daemon_lock_free(&dir),
                "a held home read as free, so a second daemon would start over the first"
            );
        }
        assert_eq!(
            daemon_pid(&dir),
            Some(stamped),
            "probing overwrote the pid of the daemon actually holding this home"
        );

        drop(held);
        assert!(daemon_lock_free(&dir), "the lock outlived its holder");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A home founded under the two-seed derivation keeps its address when the
    /// genesis is carried: the witness in `kinship.key` co-signs the same link,
    /// is retired, and the key goes. If the id moved, every issued address and
    /// display pairing of that home would die on the upgrade.
    #[test]
    fn a_legacy_kinship_key_migrates_to_a_carried_genesis_and_keeps_its_profile() {
        let home = tempfile::tempdir().unwrap();
        let identity = load_or_create_identity(home.path()).expect("identity");
        let witness = [0x77u8; 32];
        fs::write(
            kinship_key_path(home.path()),
            data_encoding::HEXLOWER.encode(&witness),
        )
        .unwrap();
        let legacy = correspondence::plane::ReachPlane::profile_for(&[identity, witness])
            .expect("the derivation the home used before");

        let migrated = identity_profile(home.path()).expect("migrate");
        assert_eq!(migrated, legacy, "the profile id did not move");
        assert!(
            !kinship_key_path(home.path()).exists(),
            "the witness key is retired"
        );
        let carried = addressbook::ReachStore::at(home.path())
            .load()
            .expect("load")
            .expect("a store was written");
        let genesis = carried.genesis.expect("the genesis is carried");
        assert!(genesis.names(&mechanics::actor::device_from_seed(&identity)));
        assert_eq!(
            carried.registry.resolve(&legacy),
            Some(vec![mechanics::actor::device_from_seed(&identity)]),
            "the witness signed and left"
        );

        assert_eq!(
            identity_profile(home.path()).expect("again"),
            legacy,
            "a second boot reads"
        );
        let _ = load_identity(home.path()).expect("nothing touched the identity");
    }

    /// `kinship.bin` copied from another machine: the genesis verifies, the
    /// profile is real, and this seed is no device of it. That must refuse
    /// exactly as an unreconstructable profile does — not boot, anchor the
    /// coordinator on somebody else's address, and leave the reach plane
    /// unrestored behind a warning.
    #[test]
    fn a_kinship_store_from_another_machine_refuses_to_boot_here() {
        let home = tempfile::tempdir().unwrap();
        let _identity = load_or_create_identity(home.path()).expect("identity");
        let elsewhere = [0x51u8; 32];
        let plane =
            correspondence::plane::ReachPlane::found_here(elsewhere, Some([0x52u8; 32]), None, 1)
                .expect("found elsewhere");
        addressbook::ReachStore::at(home.path())
            .save(&plane.state())
            .expect("copy the store here");
        let before = fs::read(home.path().join("kinship.bin")).unwrap();

        let error = identity_profile(home.path()).expect_err("refused");
        assert!(
            error.to_string().contains("cannot be reconstructed"),
            "the refusal must be the boot refusal, got: {error}"
        );
        assert_eq!(
            fs::read(home.path().join("kinship.bin")).unwrap(),
            before,
            "a refusal writes nothing"
        );
    }

    /// A store authored under a profile this home can no longer reproduce —
    /// no carried genesis, no witness key — is a refusal. Founding in its
    /// place would answer a new address under the old log, silently.
    #[test]
    fn a_home_that_cannot_reconstruct_its_profile_refuses_to_boot() {
        let home = tempfile::tempdir().unwrap();
        let identity = load_or_create_identity(home.path()).expect("identity");
        let witness = [0x78u8; 32];
        let plane = correspondence::plane::ReachPlane::found_here(identity, Some(witness), None, 1)
            .expect("found");
        let mut state = plane.state();
        state.genesis = None;
        let store = addressbook::ReachStore::at(home.path());
        store.save(&state).expect("a pre-carriage store");
        let before = fs::read(home.path().join("kinship.bin")).unwrap();

        let error = identity_profile(home.path()).expect_err("refused");
        assert!(
            error.to_string().contains("kinship.key"),
            "the refusal says what would repair it: {error}"
        );
        assert_eq!(
            fs::read(home.path().join("kinship.bin")).unwrap(),
            before,
            "nothing was written"
        );
        assert!(!kinship_key_path(home.path()).exists());
    }

    #[test]
    fn the_lock_holder_stays_identifiable_while_it_holds_the_lock() {
        let dir = std::env::temp_dir().join(format!("gc-lock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let _held = acquire_daemon_lock(&dir).expect("first daemon takes the lock");
        // Readable *while the lock is held* — the only moment it is worth
        // anything, and the reason the pid cannot live inside the lock file:
        // Windows locks are mandatory, so that read would fail there while
        // passing on unix.
        assert_eq!(
            daemon_pid(&dir),
            Some(std::process::id()),
            "the daemon holding a home must be identifiable while it holds it",
        );

        // A second daemon must lose, and must not disturb the winner's identity —
        // it never gets far enough to write one.
        assert!(
            acquire_daemon_lock(&dir).is_err(),
            "a second daemon must not get the lock",
        );
        assert_eq!(
            daemon_pid(&dir),
            Some(std::process::id()),
            "a daemon that lost the lock race must not blank the winner's pid",
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// One rule for every hosted service: env override, then the key, then
    /// the built-in — and present-but-empty env is explicitly offline, not a
    /// fallthrough. The cloud pole itself lives in `built_in` and is
    /// release-only, so this debug-built test asserts the layers above it.
    #[test]
    fn a_service_url_resolves_env_then_key_and_empty_is_offline() {
        let with_key = |value: &str| Settings {
            global: {
                let mut g = ConfigMap::default();
                g.set("post.url", value);
                g
            },
            store: ConfigMap::default(),
        };

        // A bespoke env name per case: the real overrides are process-global
        // and the suite is parallel.
        std::env::set_var("LAIT_TEST_SVC_A", "https://override.example/");
        assert_eq!(
            with_key("https://key.example").service_url("post.url", "LAIT_TEST_SVC_A"),
            Some("https://override.example".to_string()),
            "the env override wins over the key"
        );
        std::env::set_var("LAIT_TEST_SVC_B", "");
        assert_eq!(
            with_key("https://key.example").service_url("post.url", "LAIT_TEST_SVC_B"),
            None,
            "present-but-empty is explicitly offline"
        );
        assert_eq!(
            with_key("https://key.example/").service_url("post.url", "LAIT_TEST_SVC_UNSET"),
            Some("https://key.example".to_string()),
            "the key answers when nothing overrides"
        );
        assert_eq!(
            with_key("not-a-url").service_url("post.url", "LAIT_TEST_SVC_UNSET"),
            None,
            "an explicit non-URL is off, never a fallthrough to the cloud"
        );
        std::env::remove_var("LAIT_TEST_SVC_A");
        std::env::remove_var("LAIT_TEST_SVC_B");

        // The built-in pole: debug builds carry none, so a bare default here
        // is offline — the release product's cloud default rides `built_in`
        // and is exercised by the release smoke, not invented here.
        if cfg!(debug_assertions) {
            assert_eq!(Settings::default().post_url(), None);
            assert_eq!(Settings::default().directory_url(), None);
        }
    }

    /// The book is an ordering, and the ordering is the reader's: it resolves
    /// through the same three layers every endpoint does, keeps the position
    /// it was written in, and an empty value weighs nobody rather than falling
    /// through to whatever the build shipped.
    #[test]
    fn the_marker_book_reads_env_then_key_then_built_in_and_empty_opts_out() {
        let device = |seed: u8| mechanics::actor::device_from_seed(&[seed; 32]);
        let with_key = |value: &str| Settings {
            global: {
                let mut g = ConfigMap::default();
                g.set("marks.book", value);
                g
            },
            store: ConfigMap::default(),
        };

        let book = with_key(&format!(
            "https://first.example/@{}, https://second.example",
            device(3).as_str()
        ))
        .book("marks.book", "LAIT_TEST_BOOK_UNSET")
        .expect("the key names two markers");
        assert_eq!(
            book.iter().map(|m| m.base.as_str()).collect::<Vec<_>>(),
            vec!["https://first.example", "https://second.example"],
            "position is the weight, so the written order is the read order"
        );
        assert_eq!(
            book[0].by.as_ref(),
            Some(&device(3)),
            "the signer ships inside the entry"
        );
        assert_eq!(book[1].by, None, "an entry may name no signer at all");

        assert_eq!(
            with_key("https://ok.example, nonsense, https://bad.example@not-a-device")
                .book("marks.book", "LAIT_TEST_BOOK_UNSET")
                .map(|book| book.len()),
            Some(1),
            "a malformed entry is dropped, never repaired, and never the whole book"
        );

        // A bespoke env name per case: the real override is process-global and
        // this suite is parallel.
        std::env::set_var("LAIT_TEST_BOOK_A", "https://override.example");
        assert_eq!(
            with_key("https://key.example")
                .book("marks.book", "LAIT_TEST_BOOK_A")
                .map(|book| book[0].base.clone()),
            Some("https://override.example".to_string()),
            "the env override wins over the key"
        );
        std::env::set_var("LAIT_TEST_BOOK_B", "");
        assert_eq!(
            with_key("https://key.example").book("marks.book", "LAIT_TEST_BOOK_B"),
            None,
            "present-but-empty weighs nobody"
        );
        std::env::remove_var("LAIT_TEST_BOOK_A");
        std::env::remove_var("LAIT_TEST_BOOK_B");
        assert_eq!(
            with_key("").book("marks.book", "LAIT_TEST_BOOK_UNSET"),
            None,
            "an emptied key opts out too, rather than falling through to the built-in"
        );

        // The shipped pole. Its signer is the whole of F-19: an entry that
        // named only a host would have every fresh install pin whatever key
        // answered there.
        let shipped = MarkerEntry::parse(FOUNDATION_MARKS_BOOK).expect("the shipped entry parses");
        assert!(
            shipped.by.is_some(),
            "the shipped marker names its signer — there is no trust on first use here"
        );
        if cfg!(debug_assertions) {
            assert_eq!(
                Settings::default().marks_book(),
                None,
                "development builds weigh nobody; the Foundation rides built_in in release"
            );
        }
    }

    /// Named relays are lait's own rendezvous; none is the public mesh. The
    /// explicit `LAIT_NETWORK` semantics stay whole above both.
    #[test]
    fn the_network_follows_the_relay_list_and_debug_defaults_public() {
        let with_relays = |value: &str| Settings {
            global: {
                let mut g = ConfigMap::default();
                g.set("relay.urls", value);
                g
            },
            store: ConfigMap::default(),
        };
        match with_relays("https://relay.example, https://second.example/").network() {
            Ok(comms::policy::Network::Local(local)) => assert_eq!(
                local.relays,
                vec!["https://relay.example", "https://second.example"],
                "a comma list, trimmed, trailing slash dropped"
            ),
            other => panic!("named relays are Local: {other:?}"),
        }
        assert!(
            matches!(
                with_relays("not-a-url").network(),
                Ok(comms::policy::Network::Public)
            ),
            "nothing usable falls back to the public mesh rather than half a Local"
        );
        if cfg!(debug_assertions) {
            assert!(
                matches!(
                    Settings::default().network(),
                    Ok(comms::policy::Network::Public)
                ),
                "the development default is the public mesh; the Foundation relay rides built_in in release"
            );
        }
    }

    /// A coordinator answers where it is told to, and a typo does not cost a
    /// daemon that will not start.
    #[test]
    fn the_display_port_is_configured_and_degrades_to_the_built_in() {
        let settings = |value: &str| Settings {
            global: {
                let mut g = ConfigMap::default();
                g.set("display.port", value);
                g
            },
            store: ConfigMap::default(),
        };

        assert_eq!(settings("8443").display_port(), 8443);
        assert_eq!(settings("  8443  ").display_port(), 8443);
        assert_eq!(
            Settings::default().display_port(),
            crate::display::DEFAULT_DISPLAY_PORT,
            "unset is the built-in"
        );
        for refused in ["0", "not-a-port", "99999", ""] {
            assert_eq!(
                settings(refused).display_port(),
                crate::display::DEFAULT_DISPLAY_PORT,
                "{refused} falls back rather than refusing to start"
            );
        }
    }

    #[test]
    fn settings_store_layer_wins_over_global() {
        let dir = std::env::temp_dir().join(format!("gc-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut store = ConfigMap::default();
        store.set("user.nick", "store-nick");
        store.save(&store_config_path(&dir)).unwrap();
        let s = Settings {
            global: {
                let mut g = ConfigMap::default();
                g.set("user.nick", "global-nick");
                g.set("project.default", "ENG");
                g
            },
            store: ConfigMap::load(&store_config_path(&dir)),
        };
        assert_eq!(s.get("user.nick"), Some("store-nick"));
        assert_eq!(s.get("project.default"), Some("ENG"));
        assert_eq!(s.nick(), "store-nick");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_map_roundtrips_and_degrades_to_empty() {
        let dir = std::env::temp_dir().join(format!("gc-cfgmap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        assert!(ConfigMap::load(&p).0.is_empty(), "missing file → empty");
        let mut m = ConfigMap::default();
        m.set("user.nick", "x");
        m.save(&p).unwrap();
        assert_eq!(ConfigMap::load(&p).get("user.nick"), Some("x"));
        fs::write(&p, "{corrupt").unwrap();
        assert!(ConfigMap::load(&p).0.is_empty(), "corrupt file → empty");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_table_rejects_unknown_and_reserves_space_namespace() {
        assert!(key_spec("user.nick").is_ok());
        assert!(key_spec("project.default").is_ok());
        let unknown = key_spec("user.nickk").unwrap_err().to_string();
        assert!(unknown.contains("known keys"), "{unknown}");
        let reserved = key_spec("space.name").unwrap_err().to_string();
        assert!(reserved.contains("reserved"), "{reserved}");
    }

    #[test]
    fn discovery_never_creates_but_init_path_does() {
        let root = std::env::temp_dir().join(format!("gc-nostore-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // Bare dir: discovery finds nothing and creates nothing.
        assert_eq!(find_store_dir(&root), None);
        assert!(!root.join(STORE_DIR).exists());
        // The creation verb path mints the store + gitignore.
        let store = store_dir_for_init(&root).unwrap();
        assert!(store.is_dir());
        assert!(store.join(".gitignore").exists());
        // And discovery now binds it.
        assert!(find_store_dir(&root).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_store_dir_walks_up_to_the_nearest_lait() {
        let root = std::env::temp_dir().join(format!("gc-disc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        // No `.lait/` anywhere above `nested`.
        assert_eq!(find_store_dir(&nested), None);

        // Create the store at the repo root; discovery from a deep subdir and
        // from the root itself both bind it (git-style walk-up).
        let store = repo.join(STORE_DIR);
        std::fs::create_dir_all(&store).unwrap();
        assert_eq!(find_store_dir(&nested), Some(store.clone()));
        assert_eq!(find_store_dir(&repo), Some(store));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn second_daemon_lock_fails_while_first_is_held() {
        let dir = std::env::temp_dir().join(format!("gc-locktest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let first = acquire_daemon_lock(&dir).expect("first lock should succeed");
        let second = acquire_daemon_lock(&dir);
        assert!(
            second.is_err(),
            "a second daemon lock must fail while the first is held"
        );

        drop(first);
        let third = acquire_daemon_lock(&dir)
            .expect("lock should be available again after the first is dropped");
        drop(third);

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn socket_path_stays_under_the_unix_limit() {
        // Short home: socket lives in the home, as before.
        let short = PathBuf::from("/Users/moon/Library/Application Support/dev.nixi.lait");
        assert_eq!(socket_path(&short), short.join("control.sock"));

        // Long per-agent home (longer username) that would blow past macOS's
        // 104-byte sun_path limit — must fall back to a short, bindable path.
        let long = PathBuf::from(
            "/Users/maximiliana.rosencrantz-hutchinson/Library/Application Support/\
             dev.nixi.lait/agents/agent-6c8502",
        );
        assert!(
            long.join("control.sock").as_os_str().len() > 104,
            "test premise: the natural path should exceed the limit"
        );
        let p = socket_path(&long);
        assert!(
            p.as_os_str().len() <= 104,
            "control socket path must fit in sun_path: {} bytes ({})",
            p.as_os_str().len(),
            p.display()
        );

        // Deterministic: daemon and client must resolve the same long home identically.
        assert_eq!(socket_path(&long), socket_path(&long));
    }
}

fn secret_key_path(home: &Path) -> PathBuf {
    home.join("secret.key")
}

fn kinship_key_path(home: &Path) -> PathBuf {
    home.join("kinship.key")
}

/// The identity's own profile — the address everything anchors on.
///
/// One derivation for the whole daemon: the reach plane and the display
/// coordinator both anchor here, which is what makes a receiver paired to the
/// coordinator a receiver paired to the *identity*. The genesis is carried in
/// `kinship.bin` and read back; it is founded on first boot, a legacy home —
/// two seeds, the second in `kinship.key` — is carried forward under the same
/// id with the key retired, and a home whose profile cannot be reconstructed
/// refuses rather than re-founding under a new address (F-27). The store is
/// saved before the key is removed, so a crash between the two lands on the
/// idempotent path next boot.
pub fn identity_profile(home: &Path) -> Result<mechanics::kinship::ProfileId> {
    use correspondence::plane::{Failure, ReachPlane};

    let identity = load_or_create_identity(home)?;
    let store = addressbook::ReachStore::at(home);
    let held = store
        .load()
        .map_err(|error| anyhow!("read {}: {error}", home.join("kinship.bin").display()))?;
    let witness = legacy_witness_seed(home)?;
    let key = kinship_key_path(home);

    if let Some(genesis) = held.as_ref().and_then(|state| state.genesis.clone()) {
        let profile = mechanics::kinship::KinshipLog::found(genesis.clone())
            .map_err(|error| anyhow!("the carried genesis does not verify: {error}"))?
            .profile()
            .clone();
        // A genesis that verifies is not yet this device's: a `kinship.bin`
        // copied from another machine carries a perfectly good profile that
        // this seed is no device of. Booting on it would anchor the display
        // coordinator on an address the daemon cannot sign for and then leave
        // the reach plane unrestored — the unreconstructable profile wearing a
        // quieter error. It is the same refusal, asked here.
        let me = mechanics::actor::device_from_seed(&identity);
        let mine = held
            .as_ref()
            .and_then(|state| state.registry.resolve(&profile))
            .is_some_and(|devices| devices.contains(&me));
        if !mine {
            return Err(anyhow!(
                "this home's profile cannot be reconstructed; restore kinship.key or re-pair this device (kinship.bin names a profile this device is not part of)"
            ));
        }
        if let Some(witness) = witness {
            // A key still beside a carried genesis is the crash window between
            // save and remove, closed here — but only once it is proven to be
            // the witness that signed this genesis, or it may be somebody's
            // only copy of something else.
            match ReachPlane::genesis_for(&[identity, witness]) {
                Ok(derived) if derived == genesis => remove_kinship_key(&key)?,
                _ => tracing::warn!(
                    path = %key.display(),
                    "kinship.key does not name this home's genesis; leaving it in place"
                ),
            }
        }
        return Ok(profile);
    }

    let plane = ReachPlane::found_here(
        identity,
        witness,
        held,
        mechanics::wallclock::now_secs(),
    )
    .map_err(|error| match error {
        Failure::NoGenesis => anyhow!(
            "this home's profile cannot be reconstructed; restore kinship.key or re-pair this device"
        ),
        other => anyhow!("found identity profile: {other}"),
    })?;
    store
        .save(&plane.state())
        .map_err(|error| anyhow!("save {}: {error}", home.join("kinship.bin").display()))?;
    if witness.is_some() {
        remove_kinship_key(&key)?;
    }
    Ok(plane.profile().clone())
}

/// The legacy second seed, read-only: `Some` iff `<home>/kinship.key` exists.
/// A key that exists and does not parse is an error, not an absence — the
/// absence has a meaning of its own on the boot path.
pub fn legacy_witness_seed(home: &Path) -> Result<Option<[u8; 32]>> {
    let path = kinship_key_path(home);
    if !path.exists() {
        return Ok(None);
    }
    read_seed(&path).map(Some)
}

/// Idempotent: the key is retired material once the genesis is carried, and
/// a boot that finds it already gone has nothing to do.
fn remove_kinship_key(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow::Error::new(error).context("remove kinship key")),
    }
}

fn read_seed(path: &Path) -> Result<[u8; 32]> {
    let hex = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw = data_encoding::HEXLOWER_PERMISSIVE
        .decode(hex.trim().as_bytes())
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    raw.as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} must be 32 bytes", path.display()))
}

/// Load the persistent identity **seed** (32 bytes), creating one on first run.
///
/// lait's identity is the seed, not a transport keypair: it is stored as hex in
/// `secret.key`, and the transport derives its own keypair from these bytes at
/// its edge ([`mechanics::actor::device_from_seed`] maps the same seed to the
/// `DeviceId`). The on-disk format is unchanged — 64 hex chars of the 32-byte
/// seed — so existing keys load as-is.
pub fn load_or_create_identity(home: &Path) -> Result<[u8; 32]> {
    let path = secret_key_path(home);
    if path.exists() {
        load_identity(home)
    } else {
        let seed = mechanics::actor::random_seed().context("generate secret key")?;
        let hex = data_encoding::HEXLOWER.encode(&seed);
        fs::write(&path, hex).context("write secret key")?;
        Ok(seed)
    }
}

/// Load the persistent identity seed without ever minting one.
///
/// An attributed write must name an identity that already exists: creating one
/// as a side effect would let a bookkeeping call decide who this machine is.
pub fn load_identity(home: &Path) -> Result<[u8; 32]> {
    let path = secret_key_path(home);
    let hex =
        fs::read_to_string(&path).with_context(|| format!("no identity at {}", path.display()))?;
    let raw = data_encoding::HEXLOWER_PERMISSIVE
        .decode(hex.trim().as_bytes())
        .map_err(|e| anyhow::anyhow!("parse secret key: {e}"))?;
    raw.as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret key must be 32 bytes"))
}

// ---- layered local settings (the `HostConfig*` requests) ----

/// Which layers a config key may be written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyLayers {
    /// Both the global and the per-store file (store wins on read).
    GlobalAndStore,
    /// Per-store only (a `global` write is rejected).
    StoreOnly,
}

/// One row of the closed key table: a settings write refuses names not listed
/// here (typo safety — git's anything-goes is a support trap for a tool this
/// young). Add a row to introduce a key.
#[derive(Debug, Clone, Copy)]
pub struct KeySpec {
    pub name: &'static str,
    pub layers: KeyLayers,
    /// Whether a running daemon consumes this key (⇒ `HostConfigSet` sends a
    /// best-effort `ConfigReload` so the change is never a silent no-op).
    pub daemon_read: bool,
    pub help: &'static str,
    /// The built-in fallback when unset at every layer, if one exists.
    pub built_in: fn() -> Option<String>,
}

/// The Foundation cloud's service host: the Post, the directory, and the
/// registry are three mounts on one deployment (`/`, `/directory`,
/// `/registry`), so they share one base and one default.
pub const FOUNDATION_SERVICES: &str = "https://post.foundation.pub";

/// The Foundation relay — lait's own rendezvous, `lait-relay` behind a name.
/// `relay` is on the registry's RESERVED list for exactly this.
pub const FOUNDATION_RELAY: &str = "https://relay.foundation.pub";

/// The Foundation notify relay — `lait-feed-notify` on Cloud Run. What an
/// installed machine subscribes to so a publish is heard within the round
/// trip rather than on the period; the bucket stays the authority.
///
/// The service's own deterministic URL rather than `notify.foundation.pub`:
/// the zone is held outside GCP and carries no record for it yet. When one
/// exists this moves behind the name like the relay and the Post did.
pub const FOUNDATION_NOTIFY: &str = "https://foundation-notify-894246603476.us-central1.run.app";

/// The marker book a release build ships with: the Foundation's chronicle,
/// and **the device that signs it**.
///
/// The signer rides inside the entry because trust on first use is poisonable
/// exactly where a reader is weakest — a fresh install asking a *name* whose
/// host has moved. Two deployments have answered at this name; one brought up
/// without `REGISTRY_CHRONICLE_SEED` mints an ephemeral signer, every client
/// that pinned it would then read the real marker as a stranger, and the only
/// recovery would be deleting a file per device. Naming the signer makes the
/// impostor a refusal at first contact instead of a pin nobody can undo.
///
/// The id is what `GET /registry/chronicle` answers as `head.by`; it survives
/// a revision roll because the seed is a stored secret and not a mint.
/// Rotating that seed is a fleet-wide `WrongSigner` and has to ship with a
/// release that moves this line.
pub const FOUNDATION_MARKS_BOOK: &str =
    "https://post.foundation.pub@7b974cb04849e56e7e6bf2226cfd33a7ca55fb197e2f4644a783562cea3cac9c";

/// The cloud default for a hosted-service endpoint.
///
/// Present only in release builds — the product connects out of the box, and
/// a self-hosted or air-gapped install opts *out* by setting the key (or its
/// env override) empty. Development builds default to nothing, because every
/// test in this tree spawns real daemons and a built-in host would put the
/// network under suites that promise hermeticity. The same debug/release
/// split the client's image staging already uses, for the same reason: the
/// two audiences genuinely want different poles.
fn cloud_default(url: &'static str) -> Option<String> {
    if cfg!(debug_assertions) {
        None
    } else {
        Some(url.to_string())
    }
}

/// The closed set of recognized config keys.
pub const KEYS: &[KeySpec] = &[
    KeySpec {
        name: "user.nick",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: true,
        help: "Display nickname (presence, activity attribution).",
        built_in: || Some(whoami_fallback()),
    },
    KeySpec {
        name: "project.default",
        layers: KeyLayers::StoreOnly,
        daemon_read: false,
        help: "Project key issue-creating commands fall back to when -p is omitted.",
        built_in: || None,
    },
    // The identity's public label on the deployment root — `acme` answering
    // at acme.<root>. Read at daemon start beside the registry URL; the label
    // has no built-in because a default label is somebody else's, and it is
    // the one thing that keeps publication opt-in now that the service URLs
    // default to the cloud.
    KeySpec {
        name: "identity.label",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Public label this identity answers at (applies at next daemon start).",
        built_in: || None,
    },
    // The three hosted-service endpoints, one resolution rule each: the env
    // override wins when present (empty = explicitly offline), then this key,
    // then the Foundation cloud in release builds. `lait config` lists them,
    // which the env-only ancestors of the first two never managed.
    KeySpec {
        name: "registry.url",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Registry base URL routes publish to (applies at next daemon start; release default is the Foundation cloud, empty opts out).",
        built_in: || cloud_default(FOUNDATION_SERVICES),
    },
    KeySpec {
        name: "post.url",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Hosted Post correspondence is carried over (applies at next daemon start; LAIT_POST_URL overrides; release default is the Foundation cloud, empty opts out).",
        built_in: || cloud_default(FOUNDATION_SERVICES),
    },
    KeySpec {
        name: "relay.urls",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Comma-separated relay URLs the overlay rendezvouses through (applies at next daemon start; LAIT_RELAY overrides, LAIT_NETWORK overrides everything; release default is the Foundation relay, empty falls back to the public mesh).",
        built_in: || cloud_default(FOUNDATION_RELAY),
    },
    KeySpec {
        name: "update.notify",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Notify relay the daemon subscribes to for instant channel updates (applies at next daemon start; LAIT_FEED_NOTIFY overrides; release default is the Foundation relay, empty falls back to the check period).",
        built_in: || cloud_default(FOUNDATION_NOTIFY),
    },
    KeySpec {
        name: "directory.url",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Identity directory addresses publish and resolve against (applies at next daemon start; LAIT_DIRECTORY_URL overrides; release default is the Foundation cloud, empty opts out).",
        built_in: || cloud_default(FOUNDATION_SERVICES),
    },
    // The markers this identity weighs, in the order it weighs them. Position
    // is the weight and the ordering is the *reader's* — no marker states its
    // own rank, and a mark confers nothing wherever it lands in the list.
    KeySpec {
        name: "marks.book",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Ordered comma-separated markers whose chronicles this identity follows, each `base[@device-id]`, first weighed most (applies at next daemon start; LAIT_MARKS_BOOK overrides; release default is the Foundation, empty weighs nobody).",
        built_in: || cloud_default(FOUNDATION_MARKS_BOOK),
    },
    // Not `daemon_read`: the port is spent at bind, so a live daemon cannot
    // honour a change without dropping the listener every receiver is on. The
    // help says so rather than a reload pretending otherwise.
    KeySpec {
        name: "display.port",
        layers: KeyLayers::GlobalAndStore,
        daemon_read: false,
        help: "Port the display coordinator serves on (applies at next daemon start).",
        built_in: || Some(crate::display::DEFAULT_DISPLAY_PORT.to_string()),
    },
];

/// One marker this identity follows: where its chronicle answers, and — when
/// the book names one — the only device allowed to sign that chronicle.
///
/// A named signer is what makes the *first* answer checkable. Without it the
/// reader has nothing to compare against and must take whatever key replies,
/// which is a decision made by whoever happens to be answering at that host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerEntry {
    pub base: String,
    pub by: Option<mechanics::ids::DeviceId>,
}

impl MarkerEntry {
    /// Parse one `base[@device-id]` entry, or `None`.
    ///
    /// A malformed entry is dropped rather than repaired: repairing it would
    /// mean following a marker nobody wrote down, and the empty book is a
    /// perfectly good answer.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (base, by) = match text.rsplit_once('@') {
            Some((base, by)) => (base, Some(mechanics::ids::DeviceId::parse(by)?)),
            None => (text, None),
        };
        let base = base.trim_end_matches('/');
        (base.starts_with("http://") || base.starts_with("https://")).then(|| Self {
            base: base.to_string(),
            by,
        })
    }
}

/// Look up a key in the table. `space.*` names get the reserved-namespace
/// error (future synced space settings); anything else unknown lists the
/// valid keys.
///
/// The `tui.*` namespace (theme, saved tabs, and the open `tui.key.<action-id>`
/// override prefix) went with the TUI. The web client keeps the same *shape* of
/// idea — rebind by stable action id, warn rather than gate — but its overrides
/// live client-side for now; see `docs/UI.md`. If they ever want a home on disk,
/// this table is where a `web.key.*` prefix would go.
pub fn key_spec(name: &str) -> Result<&'static KeySpec> {
    if name.starts_with("space.") {
        anyhow::bail!("'{name}' is reserved for synced space settings (not available yet)");
    }
    KEYS.iter().find(|k| k.name == name).ok_or_else(|| {
        let known: Vec<&str> = KEYS.iter().map(|k| k.name).collect();
        anyhow!(
            "unknown config key '{name}' — known keys: {}",
            known.join(", ")
        )
    })
}

/// Path of the global settings file (`config_root/config.json`).
pub fn global_config_path() -> Result<PathBuf> {
    Ok(config_root()?.join("config.json"))
}

/// Path of a store's settings file (`.lait/config.json`).
pub fn store_config_path(home: &Path) -> PathBuf {
    home.join("config.json")
}

/// One settings file: a flat `key → value` string map, so `get`/`set`/`unset`
/// need no struct churn as keys are added. Missing or corrupt files degrade to
/// empty (settings are conveniences, never gates).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConfigMap(pub std::collections::BTreeMap<String, String>);

impl ConfigMap {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist atomically (temp file + rename) so a concurrent reader — e.g. a
    /// daemon handling `ConfigReload` — never sees a half-written file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self).context("encode config")?;
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("commit {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
    pub fn set(&mut self, key: &str, value: &str) {
        self.0.insert(key.to_string(), value.to_string());
    }
    /// Returns whether the key was present.
    pub fn unset(&mut self, key: &str) -> bool {
        self.0.remove(key).is_some()
    }
}

/// The merged two-layer view: per-store `config.json` over the global one
/// (nearest wins, like git). Load is cheap (two small files); daemon paths that
/// need a per-request fresh value (e.g. `project.default`) just re-load.
#[derive(Debug, Default)]
pub struct Settings {
    pub global: ConfigMap,
    pub store: ConfigMap,
}

impl Settings {
    /// Load both layers for a store. `home = None` loads only the global layer
    /// (a settings request that names no store).
    pub fn load(home: Option<&Path>) -> Self {
        let global = global_config_path()
            .map(|p| ConfigMap::load(&p))
            .unwrap_or_default();
        let store = home
            .map(|h| ConfigMap::load(&store_config_path(h)))
            .unwrap_or_default();
        Settings { global, store }
    }

    /// Effective value: store layer, then global. No built-in fallback — use
    /// the key's `built_in` for that, so display code can annotate `(default)`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.store.get(key).or_else(|| self.global.get(key))
    }

    /// The effective display nickname (built-in: `$USER`/`$USERNAME`/"anon").
    pub fn nick(&self) -> String {
        self.get("user.nick")
            .map(str::to_string)
            .unwrap_or_else(whoami_fallback)
    }

    /// The configured default project key, if any.
    pub fn default_project(&self) -> Option<String> {
        self.get("project.default").map(str::to_string)
    }

    /// One hosted-service endpoint, resolved by the one rule every service
    /// shares: the env override wins when present — and **present-but-empty
    /// is explicitly offline**, the escape hatch a test harness or an
    /// air-gapped install reaches for — then the config key, then the cloud
    /// built-in release builds carry. A value that is not an HTTP(S) URL is
    /// off, never a fallback: an explicit setting that silently fell through
    /// to the cloud would point an install at a host its operator just tried
    /// to leave.
    fn service_url(&self, key: &str, env_override: &str) -> Option<String> {
        let candidate = match std::env::var(env_override) {
            Ok(value) => Some(value),
            Err(_) => self
                .get(key)
                .map(str::to_string)
                .or_else(|| (key_spec(key).ok()?.built_in)()),
        }?;
        let candidate = candidate.trim();
        (candidate.starts_with("http://") || candidate.starts_with("https://"))
            .then(|| candidate.trim_end_matches('/').to_string())
    }

    /// The hosted Post this identity carries correspondence over, if any.
    pub fn post_url(&self) -> Option<String> {
        self.service_url("post.url", "LAIT_POST_URL")
    }

    /// The notify relay this daemon subscribes to for instant updates, if any.
    pub fn notify_url(&self) -> Option<String> {
        self.service_url("update.notify", "LAIT_FEED_NOTIFY")
    }

    /// The identity directory addresses publish and resolve against, if any.
    pub fn directory_url(&self) -> Option<String> {
        self.service_url("directory.url", "LAIT_DIRECTORY_URL")
    }

    /// The relays this identity's overlay rendezvouses through, if it names
    /// its own. Same chain as every service endpoint — `LAIT_RELAY` override
    /// (present-but-empty falls back to the public mesh), then the key, then
    /// the Foundation relay release builds carry — parsed as a comma list
    /// with the same tolerance `LocalNet::parse` extends.
    pub fn relay_urls(&self) -> Option<Vec<String>> {
        let candidate = match std::env::var("LAIT_RELAY") {
            Ok(value) => Some(value),
            Err(_) => self
                .get("relay.urls")
                .map(str::to_string)
                .or_else(|| (key_spec("relay.urls").ok()?.built_in)()),
        }?;
        let relays: Vec<String> = candidate
            .split(',')
            .map(str::trim)
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            .map(|url| url.trim_end_matches('/').to_string())
            .collect();
        (!relays.is_empty()).then_some(relays)
    }

    /// The markers this identity follows, in the order it weighs them.
    ///
    /// Same chain as every service endpoint — `LAIT_MARKS_BOOK` first
    /// (present-but-empty weighs nobody, the whole opt-out), then the key,
    /// then the Foundation in release builds — and the same tolerance
    /// `relay.urls` extends: a comma list with malformed entries dropped
    /// rather than the book refused.
    ///
    /// **Position is the weight, and the ordering is this reader's.** Nothing
    /// a marker says sets its own rank, and nothing in the list confers
    /// admission, membership, a grant or standing on anybody — a book of
    /// markers is a book of witnesses.
    pub fn marks_book(&self) -> Option<Vec<MarkerEntry>> {
        self.book("marks.book", "LAIT_MARKS_BOOK")
    }

    /// The book resolution, with its env name supplied so the layering is
    /// testable without reaching for a process-global the parallel suite
    /// shares — the split `service_url` already uses.
    fn book(&self, key: &str, env_override: &str) -> Option<Vec<MarkerEntry>> {
        let candidate = match std::env::var(env_override) {
            Ok(value) => Some(value),
            Err(_) => self
                .get(key)
                .map(str::to_string)
                .or_else(|| (key_spec(key).ok()?.built_in)()),
        }?;
        let markers: Vec<MarkerEntry> = candidate
            .split(',')
            .filter_map(MarkerEntry::parse)
            .collect();
        (!markers.is_empty()).then_some(markers)
    }

    /// The transport environment this identity's daemon builds against.
    ///
    /// `LAIT_NETWORK` stays the explicit override with its exact old
    /// semantics — `public`, `isolated`, and `local` (which reads
    /// `LAIT_RELAY`) all mean what they always meant. Absent it, the fleet
    /// question is the relay list's: named relays are `Local` — lait's own
    /// rendezvous, no third-party discovery, the PeerBook as the whole
    /// resolution story — and no relays is the public mesh, which is the
    /// development default and the pre-consolidation behavior.
    pub fn network(&self) -> anyhow::Result<comms::policy::Network> {
        if std::env::var_os("LAIT_NETWORK").is_some() {
            return comms::policy::Network::from_env();
        }
        Ok(match self.relay_urls() {
            Some(relays) => comms::policy::Network::Local(comms::policy::LocalNet { relays }),
            None => comms::policy::Network::Public,
        })
    }

    /// The identity's public label and the registry it publishes routes to.
    /// The label is the opt-in: with the registry URL defaulting to the cloud,
    /// choosing a label is what turns publication on, and a label with the
    /// registry explicitly emptied is a hope with nowhere to go.
    pub fn route_publication(&self) -> Option<(String, String)> {
        let label = self.get("identity.label")?.trim().to_string();
        if label.is_empty() {
            return None;
        }
        let registry = self.service_url("registry.url", "LAIT_REGISTRY_URL")?;
        Some((label, registry))
    }

    /// The port the display coordinator serves on.
    ///
    /// Unparseable or zero falls back to the built-in rather than refusing:
    /// a typo here would otherwise mean an identity daemon that will not start
    /// at all, and a coordinator on the default port is the recoverable
    /// failure. Zero is excluded because `DisplayTlsIdentity` refuses it —
    /// an ephemeral port cannot be told to a receiver.
    pub fn display_port(&self) -> u16 {
        self.get("display.port")
            .and_then(|value| value.trim().parse::<u16>().ok())
            .filter(|port| *port != 0)
            .unwrap_or(crate::display::DEFAULT_DISPLAY_PORT)
    }
}

// ---- `config` as values (`get` / `set` / `unset` / `list`) ----
//
// The bodies used to live in the CLI dispatcher, where every answer was a
// `println!` and every refusal a `process::exit`. Neither survives a head that
// has to render the same facts into JSON, a browser, or an agent transcript, so
// the layer policy and the key table live here — beside the files they read —
// and the surface only formats what they return.

/// Which of the two settings files a value came from, or the built-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigOrigin {
    Store,
    Global,
    Default,
}

/// One effective setting: its value and the layer that supplied it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigRow {
    pub key: String,
    /// `None` when the key is unset at every layer and has no built-in.
    pub value: Option<String>,
    pub origin: ConfigOrigin,
    pub help: String,
}

/// Which file a write landed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    Store,
    Global,
}

/// The record of one completed `set`/`unset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigWrite {
    pub key: String,
    /// The new value, or `None` for an `unset`.
    pub value: Option<String>,
    pub layer: ConfigLayer,
    /// Whether a running daemon consumes this key, so the caller knows a
    /// reload has to be pushed for the change to take effect before restart.
    pub daemon_read: bool,
}

/// A key that resolved to nothing at all — no layer, no built-in.
///
/// Typed rather than a message, because "unset" is the one config outcome a
/// caller acts on differently (a `NotFound` on the wire, an empty field in a
/// browser); string-matching a sentence for that is how the two drift apart.
#[derive(Debug)]
pub struct ConfigUnset {
    pub key: String,
    /// True when the key was absent from the layer a write targeted, as opposed
    /// to absent everywhere.
    pub in_layer: bool,
}

impl std::fmt::Display for ConfigUnset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.in_layer {
            write!(f, "'{}' was not set in that layer", self.key)
        } else {
            write!(f, "'{}' is unset", self.key)
        }
    }
}
impl std::error::Error for ConfigUnset {}

fn row_for(spec: &KeySpec, settings: &Settings) -> ConfigRow {
    let (value, origin) = match (
        settings.store.get(spec.name),
        settings.global.get(spec.name),
    ) {
        (Some(v), _) => (Some(v.to_string()), ConfigOrigin::Store),
        (None, Some(v)) => (Some(v.to_string()), ConfigOrigin::Global),
        (None, None) => ((spec.built_in)(), ConfigOrigin::Default),
    };
    ConfigRow {
        key: spec.name.to_string(),
        value,
        origin,
        help: spec.help.to_string(),
    }
}

/// Every recognized key with its effective value and origin. `home` is the
/// store layer; `None` reads the global layer alone.
pub fn config_list(home: Option<&Path>) -> Vec<ConfigRow> {
    let settings = Settings::load(home);
    KEYS.iter().map(|spec| row_for(spec, &settings)).collect()
}

/// One key's effective value. A key that is unset everywhere and has no
/// built-in is [`ConfigUnset`], not an empty string.
pub fn config_get(home: Option<&Path>, key: &str) -> Result<ConfigRow> {
    let spec = key_spec(key)?;
    let row = row_for(spec, &Settings::load(home));
    if row.value.is_none() {
        return Err(anyhow::Error::new(ConfigUnset {
            key: key.to_string(),
            in_layer: false,
        }));
    }
    Ok(row)
}

/// The file a write for `key` targets, applying the key's layer policy.
fn write_target(home: Option<&Path>, key: &str, global: bool) -> Result<(PathBuf, ConfigLayer)> {
    let spec = key_spec(key)?;
    if global && spec.layers == KeyLayers::StoreOnly {
        anyhow::bail!("'{key}' is a per-store key — it has no global layer to write");
    }
    if global {
        return Ok((global_config_path()?, ConfigLayer::Global));
    }
    let home = home
        .ok_or_else(|| anyhow!("no space named — name the store to write, or write it globally"))?;
    Ok((store_config_path(home), ConfigLayer::Store))
}

/// Write one setting into the store layer (or the global layer with `global`).
pub fn config_set(
    home: Option<&Path>,
    key: &str,
    value: &str,
    global: bool,
) -> Result<ConfigWrite> {
    let (path, layer) = write_target(home, key, global)?;
    let mut map = ConfigMap::load(&path);
    map.set(key, value);
    map.save(&path)?;
    Ok(ConfigWrite {
        key: key.to_string(),
        value: Some(value.to_string()),
        layer,
        daemon_read: key_spec(key)?.daemon_read,
    })
}

/// Clear one setting from the layer a write would target. A key that was not
/// set in that layer is [`ConfigUnset`] — nothing is written.
pub fn config_unset(home: Option<&Path>, key: &str, global: bool) -> Result<ConfigWrite> {
    let (path, layer) = write_target(home, key, global)?;
    let mut map = ConfigMap::load(&path);
    if !map.unset(key) {
        return Err(anyhow::Error::new(ConfigUnset {
            key: key.to_string(),
            in_layer: true,
        }));
    }
    map.save(&path)?;
    Ok(ConfigWrite {
        key: key.to_string(),
        value: None,
        layer,
        daemon_read: key_spec(key)?.daemon_read,
    })
}

fn whoami_fallback() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "anon".to_string())
}
