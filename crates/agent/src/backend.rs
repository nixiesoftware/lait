//! A fail-closed local isolation backend for an agent runtime.
//!
//! This module prepares a process description that Runtime's `Subprocess`
//! performer can consume directly. The process is always an explicitly named
//! OCI engine; the workload itself is never offered as a host-process fallback.
//! The only bind mount is a persistent home created beneath the selected
//! agent's private runtime directory; scratch is an engine-bounded tmpfs. The
//! identity home, keys, Reach state, ownership record, Console ledger, and
//! engine socket are never mounted.

use std::fmt;
use std::fs::OpenOptions;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs2::FileExt as _;
use mechanics::kinship::ProfileId;

const OCI_USER: &str = "podman-rootless-keep-id";
const CONTAINER_HOME: &str = "/home/agent";
const CONTAINER_SCRATCH: &str = "/scratch";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub const MAX_RUNTIME_COORDINATE_BYTES: usize = 128;

/// Hard ceilings for one isolated invocation.
///
/// CPU, memory, process, descriptor, and per-file limits are handed to the OCI
/// engine. Runtime must apply the same wall and output ceilings around the
/// engine subprocess; the prepared enforcement record makes that split
/// explicit rather than claiming the container engine enforces what it does
/// not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeLimits {
    /// CPU quota in thousandths of one CPU. `1_000` is one complete CPU.
    pub cpu_millis: u32,
    pub memory_bytes: u64,
    pub wall_millis: u64,
    pub pids: u32,
    pub open_files: u32,
    /// `RLIMIT_FSIZE`: maximum size in bytes of any one file, also used as the
    /// aggregate size of this backend's ephemeral scratch tmpfs.
    pub single_file_bytes: u64,
    pub output_bytes: u64,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            cpu_millis: 1_000,
            memory_bytes: 512 * 1024 * 1024,
            wall_millis: 5 * 60 * 1_000,
            pids: 64,
            open_files: 256,
            single_file_bytes: 64 * 1024 * 1024,
            output_bytes: 4 * 1024 * 1024,
        }
    }
}

impl RuntimeLimits {
    fn validate(self) -> Result<Self, RuntimeBackendError> {
        if self.cpu_millis == 0 || self.cpu_millis > 64_000 {
            return Err(RuntimeBackendError::InvalidLimits("CPU quota"));
        }
        if self.memory_bytes < 16 * 1024 * 1024 || self.memory_bytes > 64 * 1024 * 1024 * 1024 {
            return Err(RuntimeBackendError::InvalidLimits("memory"));
        }
        if self.wall_millis == 0 || self.wall_millis > 24 * 60 * 60 * 1_000 {
            return Err(RuntimeBackendError::InvalidLimits("wall time"));
        }
        if self.pids == 0 || self.pids > 4_096 {
            return Err(RuntimeBackendError::InvalidLimits("process count"));
        }
        if self.open_files < 3 || self.open_files > 65_536 {
            return Err(RuntimeBackendError::InvalidLimits("open files"));
        }
        if self.single_file_bytes == 0 || self.single_file_bytes > 1024 * 1024 * 1024 * 1024 {
            return Err(RuntimeBackendError::InvalidLimits("file size"));
        }
        if self.output_bytes == 0 || self.output_bytes > 1024 * 1024 * 1024 {
            return Err(RuntimeBackendError::InvalidLimits("output"));
        }
        Ok(self)
    }
}

/// The only caller-selected facts accepted by the isolation backend.
///
/// There is deliberately no command, argument, environment, network, mount,
/// user, OCI flag, or engine field. The Console package fixes the workload as
/// `/bin/sh -s`; committed Console input reaches that process over stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeScope {
    /// The identity whose runtime this is. It remains the subject even when an
    /// owner authorized the operation.
    pub agent: ProfileId,
    /// The profile that delegated this operation. No delegator credential is
    /// passed to the workload.
    pub delegated_by: ProfileId,
    pub agent_home: PathBuf,
    /// Durable Runtime coordinates used to derive a fresh, non-retargetable
    /// scratch directory after an Attempt has begun.
    pub run: String,
    pub attempt: String,
}

/// The complete environment made available to the Podman client process.
///
/// Podman needs its configured home, and rootless Linux commonly needs
/// `XDG_RUNTIME_DIR`; neither value is inherited from the daemon or forwarded
/// to the container. Both paths are resolved and pinned during construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineClientEnvironment {
    pub home: PathBuf,
    pub xdg_runtime_dir: Option<PathBuf>,
}

/// Which component is responsible for a represented ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitEnforcement {
    OciEngine,
    OuterRuntime,
}

/// The isolation facts claimed by a prepared invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnforcement {
    pub read_only_root: bool,
    pub capabilities_dropped: bool,
    pub no_new_privileges: bool,
    pub network_none: bool,
    pub non_root_user: String,
    pub engine_socket_mounted: bool,
    pub ambient_environment: bool,
    pub ambient_working_directory: bool,
    pub unrestricted_filesystem: bool,
    pub secrets_mounted: bool,
    /// A digest-pinned image and local engine policy are not remote or
    /// hardware attestation.
    pub externally_attested: bool,
    pub cpu: LimitEnforcement,
    pub memory: LimitEnforcement,
    pub wall: LimitEnforcement,
    pub pids: LimitEnforcement,
    pub open_files: LimitEnforcement,
    pub single_file_size: LimitEnforcement,
    pub output: LimitEnforcement,
    pub limits: RuntimeLimits,
}

impl RuntimeEnforcement {
    fn isolated(limits: RuntimeLimits) -> Self {
        Self {
            read_only_root: true,
            capabilities_dropped: true,
            no_new_privileges: true,
            network_none: true,
            non_root_user: OCI_USER.to_owned(),
            engine_socket_mounted: false,
            ambient_environment: false,
            ambient_working_directory: false,
            unrestricted_filesystem: false,
            secrets_mounted: false,
            externally_attested: false,
            cpu: LimitEnforcement::OciEngine,
            memory: LimitEnforcement::OciEngine,
            wall: LimitEnforcement::OuterRuntime,
            pids: LimitEnforcement::OciEngine,
            open_files: LimitEnforcement::OciEngine,
            single_file_size: LimitEnforcement::OciEngine,
            output: LimitEnforcement::OuterRuntime,
            limits,
        }
    }
}

/// Why the configured local provider cannot safely accept work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeProviderUnavailable {
    UnsupportedPlatform,
    MissingEngine,
    EnginePathIsSymlink,
    EnginePathIsNotAFile,
    EnginePathInsideAgentRoot,
    EngineIsNotExecutable,
    ProbeCouldNotStart,
    ProbeFailed { exit_code: Option<i32> },
    ProbeTimedOut,
    ProbeResponseInvalid,
    EngineIsNotRootless,
    MachineConnectionUnsafe,
    MachineUnavailable,
    RemotePolicyUnverified,
    SeccompUnavailable,
    PinnedImageAbsent,
}

impl fmt::Display for RuntimeProviderUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                f.write_str("the local OCI Console is unavailable on this platform")
            }
            Self::MissingEngine => f.write_str("the configured OCI engine is absent"),
            Self::EnginePathIsSymlink => {
                f.write_str("the configured OCI engine path contains a symlink")
            }
            Self::EnginePathIsNotAFile => {
                f.write_str("the configured OCI engine is not a regular file")
            }
            Self::EnginePathInsideAgentRoot => {
                f.write_str("the configured OCI engine is inside the agent-managed root")
            }
            Self::EngineIsNotExecutable => {
                f.write_str("the configured OCI engine is not executable")
            }
            Self::ProbeCouldNotStart => f.write_str("the OCI engine health probe could not start"),
            Self::ProbeFailed { exit_code } => {
                write!(f, "the OCI engine health probe failed ({exit_code:?})")
            }
            Self::ProbeTimedOut => f.write_str("the OCI engine health probe timed out"),
            Self::ProbeResponseInvalid => {
                f.write_str("the Podman health response is invalid or too large")
            }
            Self::EngineIsNotRootless => f.write_str("Podman is not running rootless"),
            Self::MachineConnectionUnsafe => {
                f.write_str("Podman's default remote is not a local managed machine")
            }
            Self::MachineUnavailable => {
                f.write_str("Podman's local managed machine is not running rootless")
            }
            Self::RemotePolicyUnverified => f.write_str(
                "the managed Podman machine's effective OCI policy is not locally verifiable",
            ),
            Self::SeccompUnavailable => f.write_str("Podman reports that seccomp is unavailable"),
            Self::PinnedImageAbsent => {
                f.write_str("the pinned OCI image is not present in local storage")
            }
        }
    }
}

/// Current provider standing. Absence and failed health are both explicit and
/// neither authorizes preparation. `Ready` narrowly means the exact executable
/// answered as native rootless Podman or a verified local managed machine,
/// with seccomp enabled and the configured digest-pinned image already in
/// storage. It is not Build attestation and says nothing about a workload that
/// has not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeProviderStanding {
    Ready {
        engine: PathBuf,
        image: String,
        posture: RuntimeProviderPosture,
    },
    Unavailable(RuntimeProviderUnavailable),
}

/// Verified provider shape behind `Ready`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeProviderPosture {
    NativeRootless,
    LocalManagedMachine { name: String },
}

impl RuntimeProviderStanding {
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// Live operational projection for Console, compute, and scratch views.
    /// This does not mutate the owner-authored inventory manifest.
    #[must_use]
    pub const fn inventory_standing(&self) -> crate::PrimitiveStanding {
        match self {
            Self::Ready { .. } => crate::PrimitiveStanding::Ready,
            Self::Unavailable(_) => crate::PrimitiveStanding::Unavailable,
        }
    }
}

#[derive(Debug)]
pub enum RuntimeBackendError {
    InvalidEnginePath(&'static str),
    InvalidImagePin,
    InvalidAgentsRoot(&'static str),
    InvalidClientEnvironment(&'static str),
    InvalidLimits(&'static str),
    InvalidScope(&'static str),
    ProviderUnavailable(RuntimeProviderUnavailable),
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
}

impl fmt::Display for RuntimeBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEnginePath(why) => write!(f, "invalid OCI engine path: {why}"),
            Self::InvalidImagePin => f.write_str("the OCI image is not pinned by sha256 digest"),
            Self::InvalidAgentsRoot(why) => write!(f, "invalid agents root: {why}"),
            Self::InvalidClientEnvironment(why) => {
                write!(f, "invalid OCI client environment: {why}")
            }
            Self::InvalidLimits(what) => write!(f, "invalid runtime {what} ceiling"),
            Self::InvalidScope(why) => write!(f, "invalid agent runtime scope: {why}"),
            Self::ProviderUnavailable(why) => write!(f, "runtime provider unavailable: {why}"),
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
        }
    }
}

impl std::error::Error for RuntimeBackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A fully prepared engine subprocess. Runtime passes the first three fields
/// to `Subprocess`, clears its environment, and then installs exactly the
/// bounded `environment` entries below.
#[derive(Debug, Clone)]
pub struct PreparedRuntime {
    pub agent: ProfileId,
    pub delegated_by: ProfileId,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    /// Runtime must clear its ambient environment before installing exactly
    /// these Podman-client-only entries.
    pub clear_environment: bool,
    pub environment: Vec<(String, String)>,
    pub provider: RuntimeProviderStanding,
    pub enforcement: RuntimeEnforcement,
    /// Immutable configuration and observed-provider posture selected before
    /// the Run was submitted. This is distinct from the Attempt-specific
    /// process binding below.
    pub configuration: RuntimeConfigurationBinding,
    /// Returned for lifecycle cleanup; these are the only host paths mounted.
    pub persistent_home: PathBuf,
    pub scratch: PathBuf,
    /// Host-only cidfile used for exact, idempotent cleanup.
    pub cidfile: PathBuf,
    /// Process-local lease held for the full Attempt. A restarted daemon may
    /// scavenge only scratch entries whose lease can be acquired exclusively.
    _lease: Arc<std::fs::File>,
    /// Commits the subject, delegation, executable, full argv, working
    /// directory, and enforcement claims.
    pub binding: RuntimeBinding,
}

/// Content binding for one fully prepared invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBinding([u8; 32]);

impl RuntimeBinding {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl PreparedRuntime {
    /// Detect retargeting before handing the directly consumable fields to
    /// Runtime's subprocess performer.
    #[must_use]
    pub fn verify_binding(&self) -> bool {
        let process_matches = derive_binding(
            ProcessBinding {
                agent: &self.agent,
                delegated_by: &self.delegated_by,
                program: &self.program,
                args: &self.args,
                working_directory: &self.working_directory,
                clear_environment: self.clear_environment,
                environment: &self.environment,
            },
            &self.provider,
            &self.enforcement,
        )
        .is_some_and(|binding| binding == self.binding);
        let configuration_matches = derive_configuration_binding(
            &self.program,
            &self.environment,
            &self.provider,
            self.enforcement.limits,
        )
        .is_some_and(|binding| binding == self.configuration);
        process_matches && configuration_matches
    }
}

/// Commitment to all immutable backend configuration and the provider posture
/// observed before submission. It is not remote or hardware attestation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeConfigurationBinding([u8; 32]);

impl RuntimeConfigurationBinding {
    #[must_use]
    pub const fn as_bytes(&self) -> [u8; 32] {
        self.0
    }
}

/// Provider-neutral boundary used by the Console supervisor.
pub trait AgentRuntimeBackend: Send + Sync {
    fn probe(&self) -> RuntimeProviderStanding;
    fn configuration_binding(
        &self,
        provider: &RuntimeProviderStanding,
    ) -> Result<RuntimeConfigurationBinding, RuntimeBackendError>;
    fn prepare(&self, scope: &RuntimeScope) -> Result<PreparedRuntime, RuntimeBackendError>;
    fn cleanup(&self, prepared: &PreparedRuntime) -> Result<(), RuntimeBackendError>;
}

/// Production builder for the rootless local Podman CLI contract.
///
/// The engine path is explicit, the image is immutable, and preparation never
/// searches `PATH`, pulls an image, uses a remote Podman service, or falls back
/// to executing the workload on the host. `--userns=keep-id` is load-bearing:
/// Podman maps the calling non-root user's uid/gid into the container and runs
/// the init process as that user, so owner-only host mounts remain writable
/// without running the workload as root or weakening host permissions.
pub struct OciRuntimeBackend {
    engine: PathBuf,
    image: String,
    agents_root: PathBuf,
    limits: RuntimeLimits,
    client_environment: Vec<(String, String)>,
    hooks_dir: PathBuf,
    probe_timeout: Duration,
    #[cfg(test)]
    probe_override: Option<RuntimeProviderStanding>,
}

impl OciRuntimeBackend {
    pub fn new(
        engine: PathBuf,
        image: String,
        agents_root: PathBuf,
        limits: RuntimeLimits,
        client_environment: EngineClientEnvironment,
    ) -> Result<Self, RuntimeBackendError> {
        validate_absolute_clean_path(&engine).map_err(RuntimeBackendError::InvalidEnginePath)?;
        if engine.to_str().is_none() {
            return Err(RuntimeBackendError::InvalidEnginePath(
                "path is not UTF-8 and cannot be bound canonically",
            ));
        }
        validate_image_pin(&image)?;
        let agents_root = strict_canonical_directory(&agents_root)
            .map_err(RuntimeBackendError::InvalidAgentsRoot)?;
        let limits = limits.validate()?;
        let mut client_environment = validate_client_environment(client_environment, &agents_root)?;
        let policy_root = agents_root
            .parent()
            .ok_or(RuntimeBackendError::InvalidAgentsRoot("missing parent"))?
            .join("agent-runtime-policy");
        mechanics::secretfs::create_private_dir(&policy_root).map_err(|error| {
            RuntimeBackendError::Io {
                operation: "create runtime policy directory",
                source: std::io::Error::other(error.to_string()),
            }
        })?;
        let policy_root = strict_canonical_directory(&policy_root)
            .map_err(RuntimeBackendError::InvalidAgentsRoot)?;
        let hooks_dir = ensure_private_child(&policy_root, "hooks")?;
        if std::fs::read_dir(&hooks_dir)
            .map_err(|source| RuntimeBackendError::Io {
                operation: "inspect runtime hooks directory",
                source,
            })?
            .next()
            .is_some()
        {
            return Err(RuntimeBackendError::InvalidClientEnvironment(
                "runtime hooks directory is not empty",
            ));
        }
        for (name, variable) in [
            ("containers.conf", "CONTAINERS_CONF"),
            ("mounts.conf", "CONTAINERS_MOUNTS_CONF"),
        ] {
            let path = policy_root.join(name);
            if !path.exists() {
                mechanics::secretfs::write_private(
                    &path,
                    b"",
                    mechanics::secretfs::Create::New,
                    mechanics::secretfs::Wrap::Portable,
                )
                .map_err(|error| RuntimeBackendError::Io {
                    operation: "write runtime policy file",
                    source: std::io::Error::other(error.to_string()),
                })?;
            }
            let bytes = mechanics::secretfs::read_private(&path)
                .map_err(|error| RuntimeBackendError::Io {
                    operation: "read runtime policy file",
                    source: std::io::Error::other(error.to_string()),
                })?
                .ok_or(RuntimeBackendError::InvalidClientEnvironment(
                    "runtime policy file disappeared",
                ))?;
            if !bytes.is_empty() {
                return Err(RuntimeBackendError::InvalidClientEnvironment(
                    "runtime policy file was modified",
                ));
            }
            let value = path
                .to_str()
                .ok_or(RuntimeBackendError::InvalidClientEnvironment(
                    "runtime policy path is not UTF-8",
                ))?
                .to_owned();
            client_environment.push((variable.to_owned(), value));
        }
        Ok(Self {
            engine,
            image,
            agents_root,
            limits,
            client_environment,
            hooks_dir,
            probe_timeout: PROBE_TIMEOUT,
            #[cfg(test)]
            probe_override: None,
        })
    }

    #[cfg(test)]
    fn with_probe_override(mut self, standing: RuntimeProviderStanding) -> Self {
        self.probe_override = Some(standing);
        self
    }

    /// Remove scratch left by a crashed daemon without touching a live
    /// Attempt. Every current-version Attempt holds an exclusive advisory lock
    /// for its full prepared lifetime. Entries without that exact lease are
    /// legacy/foreign and remain quarantined rather than being guessed stale.
    ///
    /// A cidfile is removed through Podman first; only a successful idempotent
    /// engine removal permits host scratch deletion. An entry created before
    /// the engine started has no cidfile and can be removed once its lease is
    /// proven unheld.
    pub fn scavenge_stale_attempts(&self) -> Result<usize, RuntimeBackendError> {
        #[cfg(not(unix))]
        return Err(RuntimeBackendError::ProviderUnavailable(
            RuntimeProviderUnavailable::UnsupportedPlatform,
        ));

        let mut removed = 0usize;
        let homes =
            std::fs::read_dir(&self.agents_root).map_err(|source| RuntimeBackendError::Io {
                operation: "scan agent runtime homes",
                source,
            })?;
        for home in homes.take(4_097) {
            let Ok(home) = home else { continue };
            let home_path = home.path();
            if !safe_direct_directory(&home_path, &self.agents_root) {
                continue;
            }
            let scratch_parent = home_path.join("agent").join("runtime").join("scratch");
            if !safe_direct_directory_chain(&scratch_parent, &home_path) {
                continue;
            }
            let entries =
                std::fs::read_dir(&scratch_parent).map_err(|source| RuntimeBackendError::Io {
                    operation: "scan agent runtime scratch",
                    source,
                })?;
            for entry in entries.take(4_097) {
                let Ok(entry) = entry else { continue };
                let scratch = entry.path();
                let Some(name) = scratch.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if !valid_scratch_name(name) || !safe_direct_directory(&scratch, &scratch_parent) {
                    continue;
                }
                let lease_path = scratch.join("attempt.lock");
                let Ok(metadata) = std::fs::symlink_metadata(&lease_path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    continue;
                }
                let lease = match OpenOptions::new().read(true).write(true).open(&lease_path) {
                    Ok(lease) => lease,
                    Err(_) => continue,
                };
                match lease.try_lock_exclusive() {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(source) => {
                        return Err(RuntimeBackendError::Io {
                            operation: "lock stale runtime attempt",
                            source,
                        })
                    }
                }
                let cidfile = scratch.join("container.cid");
                match std::fs::symlink_metadata(&cidfile) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                        continue
                    }
                    Ok(_) => {
                        let engine = self
                            .engine_standing()
                            .map_err(RuntimeBackendError::ProviderUnavailable)?;
                        let args = [
                            "rm",
                            "--force",
                            "--time=0",
                            "--ignore",
                            "--cidfile",
                            cidfile.to_str().ok_or(RuntimeBackendError::InvalidScope(
                                "stale cidfile path is not UTF-8",
                            ))?,
                        ];
                        self.run_probe(&engine, &args, false)
                            .map_err(RuntimeBackendError::ProviderUnavailable)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(RuntimeBackendError::Io {
                            operation: "inspect stale runtime cidfile",
                            source,
                        })
                    }
                }
                std::fs::remove_dir_all(&scratch).map_err(|source| RuntimeBackendError::Io {
                    operation: "remove stale runtime scratch",
                    source,
                })?;
                removed = removed.saturating_add(1);
            }
        }
        Ok(removed)
    }

    fn engine_standing(&self) -> Result<PathBuf, RuntimeProviderUnavailable> {
        let metadata = match std::fs::symlink_metadata(&self.engine) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(RuntimeProviderUnavailable::MissingEngine)
            }
            Err(_) => return Err(RuntimeProviderUnavailable::ProbeCouldNotStart),
        };
        if metadata.file_type().is_symlink() {
            return Err(RuntimeProviderUnavailable::EnginePathIsSymlink);
        }
        if !metadata.file_type().is_file() {
            return Err(RuntimeProviderUnavailable::EnginePathIsNotAFile);
        }
        let canonical = self
            .engine
            .canonicalize()
            .map_err(|_| RuntimeProviderUnavailable::ProbeCouldNotStart)?;
        if canonical != self.engine {
            return Err(RuntimeProviderUnavailable::EnginePathIsSymlink);
        }
        if canonical.starts_with(&self.agents_root) {
            return Err(RuntimeProviderUnavailable::EnginePathInsideAgentRoot);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(RuntimeProviderUnavailable::EngineIsNotExecutable);
            }
        }
        Ok(canonical)
    }

    fn probe_process(&self, engine: &Path, args: &[&str], capture: bool) -> Command {
        let mut command = Command::new(engine);
        command
            .args(args)
            .env_clear()
            .envs(self.client_environment.iter().cloned())
            .current_dir(&self.agents_root)
            .stdin(Stdio::null())
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null());
        command
    }

    fn run_probe(
        &self,
        engine: &Path,
        args: &[&str],
        capture: bool,
    ) -> Result<Vec<u8>, RuntimeProviderUnavailable> {
        use std::io::Read as _;

        let mut child = self
            .probe_process(engine, args, capture)
            .spawn()
            .map_err(|_| RuntimeProviderUnavailable::ProbeCouldNotStart)?;
        let began = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    if !capture {
                        return Ok(Vec::new());
                    }
                    let mut bytes = Vec::new();
                    let mut stdout = child
                        .stdout
                        .take()
                        .ok_or(RuntimeProviderUnavailable::ProbeResponseInvalid)?
                        .take(64 * 1024 + 1);
                    stdout
                        .read_to_end(&mut bytes)
                        .map_err(|_| RuntimeProviderUnavailable::ProbeResponseInvalid)?;
                    if bytes.len() > 64 * 1024 {
                        return Err(RuntimeProviderUnavailable::ProbeResponseInvalid);
                    }
                    return Ok(bytes);
                }
                Ok(Some(status)) => {
                    return Err(RuntimeProviderUnavailable::ProbeFailed {
                        exit_code: status.code(),
                    })
                }
                Ok(None) if began.elapsed() < self.probe_timeout => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RuntimeProviderUnavailable::ProbeTimedOut);
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RuntimeProviderUnavailable::ProbeCouldNotStart);
                }
            }
        }
    }

    fn verify_local_machine(&self, engine: &Path) -> Result<String, RuntimeProviderUnavailable> {
        let connections = self.run_probe(
            engine,
            &["system", "connection", "list", "--format=json"],
            true,
        )?;
        let connections: serde_json::Value = serde_json::from_slice(&connections)
            .map_err(|_| RuntimeProviderUnavailable::ProbeResponseInvalid)?;
        let connections = connections
            .as_array()
            .ok_or(RuntimeProviderUnavailable::ProbeResponseInvalid)?;
        let mut defaults = connections.iter().filter(|connection| {
            connection.get("Default").and_then(|value| value.as_bool()) == Some(true)
        });
        let connection = defaults
            .next()
            .ok_or(RuntimeProviderUnavailable::MachineConnectionUnsafe)?;
        if defaults.next().is_some()
            || connection
                .get("IsMachine")
                .and_then(|value| value.as_bool())
                != Some(true)
            || connection
                .get("ReadWrite")
                .and_then(|value| value.as_bool())
                != Some(true)
        {
            return Err(RuntimeProviderUnavailable::MachineConnectionUnsafe);
        }
        let name = connection
            .get("Name")
            .and_then(|value| value.as_str())
            .filter(|name| valid_machine_name(name))
            .ok_or(RuntimeProviderUnavailable::MachineConnectionUnsafe)?;
        connection
            .get("URI")
            .and_then(|value| value.as_str())
            .filter(|uri| safe_machine_uri(uri))
            .ok_or(RuntimeProviderUnavailable::MachineConnectionUnsafe)?;

        let machine = self.run_probe(engine, &["machine", "inspect", name], true)?;
        let machine: serde_json::Value = serde_json::from_slice(&machine)
            .map_err(|_| RuntimeProviderUnavailable::ProbeResponseInvalid)?;
        let held = machine
            .as_array()
            .and_then(|machines| (machines.len() == 1).then(|| machines.first()).flatten())
            .ok_or(RuntimeProviderUnavailable::MachineUnavailable)?;
        if held.get("Name").and_then(|value| value.as_str()) != Some(name)
            || held.get("State").and_then(|value| value.as_str()) != Some("running")
            || held.get("Rootful").and_then(|value| value.as_bool()) != Some(false)
        {
            return Err(RuntimeProviderUnavailable::MachineUnavailable);
        }
        Ok(name.to_owned())
    }

    fn prepare_inner(
        &self,
        scope: &RuntimeScope,
        provider: RuntimeProviderStanding,
    ) -> Result<PreparedRuntime, RuntimeBackendError> {
        validate_scope(scope)?;
        let agent_home = strict_canonical_directory(&scope.agent_home)
            .map_err(RuntimeBackendError::InvalidScope)?;
        if agent_home.parent() != Some(self.agents_root.as_path()) {
            return Err(RuntimeBackendError::InvalidScope(
                "agent home is not a direct child of the configured agents root",
            ));
        }
        validate_mount_source(&agent_home)?;

        // Never mount the identity root. The only durable container-visible
        // directory is this dedicated, fixed subtree.
        let agent_dir = ensure_private_child(&agent_home, "agent")?;
        let runtime_dir = ensure_private_child(&agent_dir, "runtime")?;
        let persistent_home = ensure_private_child(&runtime_dir, "home")?;
        let scratch_parent = ensure_private_child(&runtime_dir, "scratch")?;
        let scratch = scratch_parent.join(scratch_name(&scope.run, &scope.attempt));
        match std::fs::create_dir(&scratch) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(RuntimeBackendError::InvalidScope(
                    "scratch attempt already exists",
                ))
            }
            Err(source) => {
                return Err(RuntimeBackendError::Io {
                    operation: "create fresh runtime scratch",
                    source,
                })
            }
        }
        mechanics::secretfs::create_private_dir(&scratch).map_err(|error| {
            RuntimeBackendError::Io {
                operation: "make runtime scratch private",
                source: std::io::Error::other(error.to_string()),
            }
        })?;
        let scratch =
            strict_canonical_directory(&scratch).map_err(RuntimeBackendError::InvalidScope)?;
        let lease_path = scratch.join("attempt.lock");
        let lease = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&lease_path)
            .map_err(|source| RuntimeBackendError::Io {
                operation: "create runtime attempt lease",
                source,
            })?;
        lease
            .lock_exclusive()
            .map_err(|source| RuntimeBackendError::Io {
                operation: "lock runtime attempt lease",
                source,
            })?;
        let cidfile = scratch.join("container.cid");
        validate_mount_source(&persistent_home)?;
        validate_mount_source(&scratch)?;

        let engine = match &provider {
            RuntimeProviderStanding::Ready { engine, .. } => engine.clone(),
            RuntimeProviderStanding::Unavailable(why) => {
                return Err(RuntimeBackendError::ProviderUnavailable(why.clone()))
            }
        };
        let mut args = Vec::new();
        if let RuntimeProviderStanding::Ready {
            posture: RuntimeProviderPosture::LocalManagedMachine { name },
            ..
        } = &provider
        {
            args.push(format!("--connection={name}"));
        }
        args.push(format!("--hooks-dir={}", self.hooks_dir.display()));
        args.extend([
            "run".to_owned(),
            "--rm".to_owned(),
            "--init".to_owned(),
            "--interactive".to_owned(),
            "--pull=never".to_owned(),
            "--read-only".to_owned(),
            "--cap-drop=ALL".to_owned(),
            "--security-opt=no-new-privileges".to_owned(),
            "--network=none".to_owned(),
            "--http-proxy=false".to_owned(),
            "--unsetenv-all".to_owned(),
            "--ipc=private".to_owned(),
            "--userns=keep-id".to_owned(),
            "--log-driver=none".to_owned(),
            format!(
                "--timeout={}",
                self.limits.wall_millis.saturating_add(999) / 1_000
            ),
            format!("--cidfile={}", cidfile.display()),
            format!(
                "--name=lait-agent-{}",
                scratch_name(&scope.run, &scope.attempt)
            ),
            format!("--label=lait.run={}", scope.run),
            format!("--label=lait.attempt={}", scope.attempt),
            format!("--cpus={}", cpu_value(self.limits.cpu_millis)),
            format!("--memory={}b", self.limits.memory_bytes),
            format!("--pids-limit={}", self.limits.pids),
            format!("--ulimit=nofile={0}:{0}", self.limits.open_files),
            format!("--ulimit=fsize={0}:{0}", self.limits.single_file_bytes),
            format!(
                "--volume={}:{CONTAINER_HOME}:rw,nosuid,nodev",
                persistent_home.display()
            ),
            format!(
                "--tmpfs={CONTAINER_SCRATCH}:rw,nosuid,nodev,size={}",
                self.limits.single_file_bytes
            ),
            format!("--workdir={CONTAINER_SCRATCH}"),
            format!("--env=HOME={CONTAINER_HOME}"),
            format!("--env=TMPDIR={CONTAINER_SCRATCH}"),
            "--entrypoint=/bin/sh".to_owned(),
            self.image.clone(),
            "-s".to_owned(),
        ]);

        let enforcement = RuntimeEnforcement::isolated(self.limits);
        let configuration = self.configuration_binding(&provider)?;
        let binding = derive_binding(
            ProcessBinding {
                agent: &scope.agent,
                delegated_by: &scope.delegated_by,
                program: &engine,
                args: &args,
                working_directory: &scratch,
                clear_environment: true,
                environment: &self.client_environment,
            },
            &provider,
            &enforcement,
        )
        .ok_or(RuntimeBackendError::InvalidScope(
            "prepared invocation contains a non-UTF-8 process path",
        ))?;
        Ok(PreparedRuntime {
            agent: scope.agent.clone(),
            delegated_by: scope.delegated_by.clone(),
            program: engine,
            args,
            working_directory: scratch.clone(),
            clear_environment: true,
            environment: self.client_environment.clone(),
            provider,
            enforcement,
            configuration,
            persistent_home,
            scratch,
            cidfile,
            _lease: Arc::new(lease),
            binding,
        })
    }
}

impl AgentRuntimeBackend for OciRuntimeBackend {
    fn probe(&self) -> RuntimeProviderStanding {
        #[cfg(not(unix))]
        return RuntimeProviderStanding::Unavailable(
            RuntimeProviderUnavailable::UnsupportedPlatform,
        );

        #[cfg(test)]
        if let Some(standing) = &self.probe_override {
            return standing.clone();
        }

        let engine = match self.engine_standing() {
            Ok(engine) => engine,
            Err(why) => return RuntimeProviderStanding::Unavailable(why),
        };
        let info = match self.run_probe(&engine, &["info", "--format=json"], true) {
            Ok(info) => info,
            Err(why) => return RuntimeProviderStanding::Unavailable(why),
        };
        let info: serde_json::Value = match serde_json::from_slice(&info) {
            Ok(info) => info,
            Err(_) => {
                return RuntimeProviderStanding::Unavailable(
                    RuntimeProviderUnavailable::ProbeResponseInvalid,
                )
            }
        };
        if info
            .pointer("/host/security/rootless")
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            return RuntimeProviderStanding::Unavailable(
                RuntimeProviderUnavailable::EngineIsNotRootless,
            );
        }
        let posture = match info
            .pointer("/host/serviceIsRemote")
            .and_then(|value| value.as_bool())
        {
            Some(false) => RuntimeProviderPosture::NativeRootless,
            Some(true) => {
                if let Err(why) = self.verify_local_machine(&engine) {
                    return RuntimeProviderStanding::Unavailable(why);
                }
                // Client-side containers.conf and hooks controls do not prove
                // the server-side defaults inside Podman Machine. Fail closed
                // until create+inspect policy attestation is implemented.
                return RuntimeProviderStanding::Unavailable(
                    RuntimeProviderUnavailable::RemotePolicyUnverified,
                );
            }
            None => {
                return RuntimeProviderStanding::Unavailable(
                    RuntimeProviderUnavailable::ProbeResponseInvalid,
                )
            }
        };
        if info
            .pointer("/host/security/seccompEnabled")
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            return RuntimeProviderStanding::Unavailable(
                RuntimeProviderUnavailable::SeccompUnavailable,
            );
        }
        let mut image_probe = Vec::new();
        if let RuntimeProviderPosture::LocalManagedMachine { name } = &posture {
            image_probe.push(format!("--connection={name}"));
        }
        image_probe.extend(["image".to_owned(), "exists".to_owned(), self.image.clone()]);
        let image_probe = image_probe.iter().map(String::as_str).collect::<Vec<_>>();
        if let Err(why) = self.run_probe(&engine, &image_probe, false) {
            return RuntimeProviderStanding::Unavailable(match why {
                RuntimeProviderUnavailable::ProbeFailed { .. } => {
                    RuntimeProviderUnavailable::PinnedImageAbsent
                }
                other => other,
            });
        }
        RuntimeProviderStanding::Ready {
            engine,
            image: self.image.clone(),
            posture,
        }
    }

    fn prepare(&self, scope: &RuntimeScope) -> Result<PreparedRuntime, RuntimeBackendError> {
        let provider = self.probe();
        if let RuntimeProviderStanding::Unavailable(why) = &provider {
            return Err(RuntimeBackendError::ProviderUnavailable(why.clone()));
        }
        self.prepare_inner(scope, provider)
    }

    fn configuration_binding(
        &self,
        provider: &RuntimeProviderStanding,
    ) -> Result<RuntimeConfigurationBinding, RuntimeBackendError> {
        match provider {
            RuntimeProviderStanding::Ready { engine, image, .. }
                if engine == &self.engine && image == &self.image => {}
            RuntimeProviderStanding::Ready { .. } => {
                return Err(RuntimeBackendError::InvalidScope(
                    "provider standing does not match configured backend",
                ))
            }
            RuntimeProviderStanding::Unavailable(why) => {
                return Err(RuntimeBackendError::ProviderUnavailable(why.clone()))
            }
        }
        derive_configuration_binding(
            &self.engine,
            &self.client_environment,
            provider,
            self.limits,
        )
        .ok_or(RuntimeBackendError::InvalidScope(
            "backend configuration contains a non-UTF-8 process path",
        ))
    }

    fn cleanup(&self, prepared: &PreparedRuntime) -> Result<(), RuntimeBackendError> {
        if !prepared.verify_binding()
            || prepared.configuration != self.configuration_binding(&prepared.provider)?
        {
            return Err(RuntimeBackendError::InvalidScope(
                "cleanup invocation does not match this backend",
            ));
        }
        let mut args = Vec::new();
        if let RuntimeProviderStanding::Ready {
            posture: RuntimeProviderPosture::LocalManagedMachine { name },
            ..
        } = &prepared.provider
        {
            args.push(format!("--connection={name}"));
        }
        args.extend([
            "rm".to_owned(),
            "--force".to_owned(),
            "--time=0".to_owned(),
            "--ignore".to_owned(),
            "--cidfile".to_owned(),
            prepared.cidfile.to_string_lossy().into_owned(),
        ]);
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        self.run_probe(&prepared.program, &args, false)
            .map_err(RuntimeBackendError::ProviderUnavailable)?;
        std::fs::remove_dir_all(&prepared.scratch).map_err(|source| RuntimeBackendError::Io {
            operation: "remove runtime scratch",
            source,
        })
    }
}

fn validate_image_pin(image: &str) -> Result<(), RuntimeBackendError> {
    let Some((repository, digest)) = image.rsplit_once("@sha256:") else {
        return Err(RuntimeBackendError::InvalidImagePin);
    };
    if repository.is_empty()
        || repository.starts_with('-')
        || repository.contains('@')
        || repository
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || !repository.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'_' | b'/' | b':')
        })
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RuntimeBackendError::InvalidImagePin);
    }
    Ok(())
}

fn valid_machine_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn safe_machine_uri(uri: &str) -> bool {
    if let Some(rest) = uri.strip_prefix("ssh://core@127.0.0.1:") {
        let Some((port, path)) = rest.split_once('/') else {
            return false;
        };
        return port.parse::<u16>().is_ok_and(|port| port > 0)
            && path.starts_with("run/user/")
            && path.ends_with("/podman/podman.sock")
            && !path.split('/').any(|part| part == "." || part == "..");
    }
    let Some(path) = uri.strip_prefix("unix://") else {
        return false;
    };
    let path = Path::new(path);
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("podman-machine-") && name.ends_with("-api.sock"))
}

fn validate_scope(scope: &RuntimeScope) -> Result<(), RuntimeBackendError> {
    if scope.agent == scope.delegated_by {
        return Err(RuntimeBackendError::InvalidScope(
            "agent subject cannot impersonate its delegator",
        ));
    }
    if !valid_runtime_coordinate(&scope.run) {
        return Err(RuntimeBackendError::InvalidScope("run id"));
    }
    if !valid_runtime_coordinate(&scope.attempt) {
        return Err(RuntimeBackendError::InvalidScope("attempt id"));
    }
    Ok(())
}

fn valid_runtime_coordinate(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RUNTIME_COORDINATE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn scratch_name(run: &str, attempt: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/agent/runtime-scratch/1");
    bind_part(&mut hasher, run.as_bytes());
    bind_part(&mut hasher, attempt.as_bytes());
    format!("attempt-{}", hasher.finalize().to_hex())
}

fn valid_scratch_name(name: &str) -> bool {
    let Some(digest) = name.strip_prefix("attempt-") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_direct_directory(path: &Path, parent: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return false;
    }
    let (Ok(path), Ok(parent)) = (path.canonicalize(), parent.canonicalize()) else {
        return false;
    };
    path.parent() == Some(parent.as_path())
}

fn safe_direct_directory_chain(scratch: &Path, home: &Path) -> bool {
    let agent = home.join("agent");
    let runtime = agent.join("runtime");
    safe_direct_directory(&agent, home)
        && safe_direct_directory(&runtime, &agent)
        && safe_direct_directory(scratch, &runtime)
}

fn validate_client_environment(
    configured: EngineClientEnvironment,
    agents_root: &Path,
) -> Result<Vec<(String, String)>, RuntimeBackendError> {
    let home = strict_canonical_directory(&configured.home)
        .map_err(RuntimeBackendError::InvalidClientEnvironment)?;
    if home.starts_with(agents_root) {
        return Err(RuntimeBackendError::InvalidClientEnvironment(
            "client home is agent-controlled",
        ));
    }
    let mut environment = vec![(
        "HOME".to_owned(),
        home.to_str()
            .ok_or(RuntimeBackendError::InvalidClientEnvironment(
                "client home is not UTF-8",
            ))?
            .to_owned(),
    )];
    if let Some(runtime) = configured.xdg_runtime_dir {
        let runtime = strict_canonical_directory(&runtime)
            .map_err(RuntimeBackendError::InvalidClientEnvironment)?;
        if runtime.starts_with(agents_root) {
            return Err(RuntimeBackendError::InvalidClientEnvironment(
                "client runtime directory is agent-controlled",
            ));
        }
        environment.push((
            "XDG_RUNTIME_DIR".to_owned(),
            runtime
                .to_str()
                .ok_or(RuntimeBackendError::InvalidClientEnvironment(
                    "client runtime directory is not UTF-8",
                ))?
                .to_owned(),
        ));
    }
    Ok(environment)
}

fn validate_absolute_clean_path(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute() {
        return Err("path is not absolute");
    }
    if !path
        .components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("path contains navigation components");
    }
    Ok(())
}

fn strict_canonical_directory(path: &Path) -> Result<PathBuf, &'static str> {
    validate_absolute_clean_path(path)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "directory is absent")?;
    if metadata.file_type().is_symlink() {
        return Err("directory is a symlink");
    }
    if !metadata.is_dir() {
        return Err("path is not a directory");
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "directory cannot be resolved")?;
    if canonical != path {
        return Err("directory path contains a symlink or alias");
    }
    Ok(canonical)
}

fn validate_mount_source(path: &Path) -> Result<(), RuntimeBackendError> {
    let text = path
        .to_str()
        .ok_or(RuntimeBackendError::InvalidScope("mount path is not UTF-8"))?;
    if text.contains(',')
        || text.contains(':')
        || text.contains('\0')
        || text.contains('\n')
        || text.contains('\r')
    {
        return Err(RuntimeBackendError::InvalidScope(
            "mount path contains an OCI option delimiter",
        ));
    }
    Ok(())
}

fn ensure_private_child(parent: &Path, child: &str) -> Result<PathBuf, RuntimeBackendError> {
    let path = parent.join(child);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(RuntimeBackendError::InvalidScope(
                "runtime directory contains a symlink",
            ))
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(RuntimeBackendError::InvalidScope(
                "runtime directory component is not a directory",
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&path).map_err(|source| RuntimeBackendError::Io {
                operation: "create private runtime directory",
                source,
            })?;
        }
        Err(source) => {
            return Err(RuntimeBackendError::Io {
                operation: "inspect private runtime directory",
                source,
            })
        }
    }
    mechanics::secretfs::create_private_dir(&path).map_err(|error| RuntimeBackendError::Io {
        operation: "make runtime directory private",
        source: std::io::Error::other(error.to_string()),
    })?;
    let canonical = strict_canonical_directory(&path).map_err(RuntimeBackendError::InvalidScope)?;
    if canonical.parent() != Some(parent) {
        return Err(RuntimeBackendError::InvalidScope(
            "runtime directory escaped its parent",
        ));
    }
    Ok(canonical)
}

fn cpu_value(millis: u32) -> String {
    format!("{}.{:03}", millis / 1_000, millis % 1_000)
}

struct ProcessBinding<'a> {
    agent: &'a ProfileId,
    delegated_by: &'a ProfileId,
    program: &'a Path,
    args: &'a [String],
    working_directory: &'a Path,
    clear_environment: bool,
    environment: &'a [(String, String)],
}

fn derive_configuration_binding(
    engine: &Path,
    environment: &[(String, String)],
    provider: &RuntimeProviderStanding,
    limits: RuntimeLimits,
) -> Option<RuntimeConfigurationBinding> {
    let RuntimeProviderStanding::Ready {
        engine: observed_engine,
        image,
        posture,
    } = provider
    else {
        return None;
    };
    if observed_engine != engine {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/agent/runtime-configuration/1");
    bind_part(&mut hasher, engine.to_str()?.as_bytes());
    bind_part(&mut hasher, image.as_bytes());
    match posture {
        RuntimeProviderPosture::NativeRootless => {
            hasher.update(&[1]);
        }
        RuntimeProviderPosture::LocalManagedMachine { name } => {
            hasher.update(&[2]);
            bind_part(&mut hasher, name.as_bytes());
        }
    }
    hasher.update(
        &u64::try_from(environment.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for (name, value) in environment {
        bind_part(&mut hasher, name.as_bytes());
        bind_part(&mut hasher, value.as_bytes());
    }
    hasher.update(&limits.cpu_millis.to_be_bytes());
    hasher.update(&limits.memory_bytes.to_be_bytes());
    hasher.update(&limits.wall_millis.to_be_bytes());
    hasher.update(&limits.pids.to_be_bytes());
    hasher.update(&limits.open_files.to_be_bytes());
    hasher.update(&limits.single_file_bytes.to_be_bytes());
    hasher.update(&limits.output_bytes.to_be_bytes());
    Some(RuntimeConfigurationBinding(*hasher.finalize().as_bytes()))
}

fn derive_binding(
    process: ProcessBinding<'_>,
    provider: &RuntimeProviderStanding,
    enforcement: &RuntimeEnforcement,
) -> Option<RuntimeBinding> {
    let program = process.program.to_str()?;
    let working_directory = process.working_directory.to_str()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/agent/runtime-binding/2");
    bind_part(&mut hasher, process.agent.as_str().as_bytes());
    bind_part(&mut hasher, process.delegated_by.as_str().as_bytes());
    bind_part(&mut hasher, program.as_bytes());
    hasher.update(
        &u64::try_from(process.args.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for argument in process.args {
        bind_part(&mut hasher, argument.as_bytes());
    }
    bind_part(&mut hasher, working_directory.as_bytes());
    hasher.update(&[u8::from(process.clear_environment)]);
    hasher.update(
        &u64::try_from(process.environment.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for (name, value) in process.environment {
        bind_part(&mut hasher, name.as_bytes());
        bind_part(&mut hasher, value.as_bytes());
    }
    match provider {
        RuntimeProviderStanding::Ready {
            engine,
            image,
            posture,
        } => {
            hasher.update(&[1]);
            bind_part(&mut hasher, engine.to_str()?.as_bytes());
            bind_part(&mut hasher, image.as_bytes());
            match posture {
                RuntimeProviderPosture::NativeRootless => {
                    hasher.update(&[1]);
                }
                RuntimeProviderPosture::LocalManagedMachine { name } => {
                    hasher.update(&[2]);
                    bind_part(&mut hasher, name.as_bytes());
                }
            }
        }
        RuntimeProviderStanding::Unavailable(why) => {
            hasher.update(&[0]);
            bind_part(&mut hasher, why.to_string().as_bytes());
        }
    }
    hasher.update(&[
        u8::from(enforcement.read_only_root),
        u8::from(enforcement.capabilities_dropped),
        u8::from(enforcement.no_new_privileges),
        u8::from(enforcement.network_none),
        u8::from(enforcement.engine_socket_mounted),
        u8::from(enforcement.ambient_environment),
        u8::from(enforcement.ambient_working_directory),
        u8::from(enforcement.unrestricted_filesystem),
        u8::from(enforcement.secrets_mounted),
        u8::from(enforcement.externally_attested),
    ]);
    bind_part(&mut hasher, enforcement.non_root_user.as_bytes());
    hasher.update(&[
        enforcement_kind(enforcement.cpu),
        enforcement_kind(enforcement.memory),
        enforcement_kind(enforcement.wall),
        enforcement_kind(enforcement.pids),
        enforcement_kind(enforcement.open_files),
        enforcement_kind(enforcement.single_file_size),
        enforcement_kind(enforcement.output),
    ]);
    hasher.update(&enforcement.limits.cpu_millis.to_be_bytes());
    hasher.update(&enforcement.limits.memory_bytes.to_be_bytes());
    hasher.update(&enforcement.limits.wall_millis.to_be_bytes());
    hasher.update(&enforcement.limits.pids.to_be_bytes());
    hasher.update(&enforcement.limits.open_files.to_be_bytes());
    hasher.update(&enforcement.limits.single_file_bytes.to_be_bytes());
    hasher.update(&enforcement.limits.output_bytes.to_be_bytes());
    Some(RuntimeBinding(*hasher.finalize().as_bytes()))
}

const fn enforcement_kind(enforcement: LimitEnforcement) -> u8 {
    match enforcement {
        LimitEnforcement::OciEngine => 1,
        LimitEnforcement::OuterRuntime => 2,
    }
}

fn bind_part(hasher: &mut blake3::Hasher, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(&length.to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str =
        "registry.example/lait/agent@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().expect("temporary root");
        let root = temp
            .path()
            .canonicalize()
            .expect("canonical temporary root");
        let agents = root.join("agents");
        std::fs::create_dir(&agents).expect("agents root");
        let agents = agents.canonicalize().expect("canonical agents root");
        let adam = agents.join("adam");
        std::fs::create_dir(&adam).expect("Adam home");
        let adam = adam.canonicalize().expect("canonical Adam home");
        (temp, agents, adam)
    }

    fn ready_backend(agents: PathBuf) -> OciRuntimeBackend {
        let root = agents.parent().expect("temporary root").to_owned();
        let engine = root.join("healthy-oci");
        let standing = RuntimeProviderStanding::Ready {
            engine: engine.clone(),
            image: DIGEST.to_owned(),
            posture: RuntimeProviderPosture::NativeRootless,
        };
        OciRuntimeBackend::new(
            engine,
            DIGEST.to_owned(),
            agents,
            RuntimeLimits::default(),
            EngineClientEnvironment {
                home: root.clone(),
                xdg_runtime_dir: Some(root),
            },
        )
        .expect("backend")
        .with_probe_override(standing)
    }

    fn client_environment(agents: &Path) -> EngineClientEnvironment {
        EngineClientEnvironment {
            home: agents.parent().expect("temporary root").to_owned(),
            xdg_runtime_dir: Some(agents.parent().expect("temporary root").to_owned()),
        }
    }

    #[cfg(unix)]
    fn scavenge_backend(agents: PathBuf) -> OciRuntimeBackend {
        use std::os::unix::fs::PermissionsExt as _;

        let root = agents.parent().expect("temporary root").to_owned();
        let engine = root.join("scavenge-oci");
        std::fs::write(
            &engine,
            "#!/bin/sh\n[ \"$1\" = rm ] || exit 91\n[ \"$2\" = --force ] || exit 92\n[ \"$3\" = --time=0 ] || exit 93\n[ \"$4\" = --ignore ] || exit 94\n[ \"$5\" = --cidfile ] || exit 95\n[ -f \"$6\" ] || exit 96\nexit 0\n",
        )
        .expect("write scavenge engine");
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700))
            .expect("make scavenge engine executable");
        let standing = RuntimeProviderStanding::Ready {
            engine: engine.canonicalize().expect("canonical scavenge engine"),
            image: DIGEST.to_owned(),
            posture: RuntimeProviderPosture::NativeRootless,
        };
        let configured_engine = match &standing {
            RuntimeProviderStanding::Ready { engine, .. } => engine.clone(),
            RuntimeProviderStanding::Unavailable(_) => unreachable!("fixture is ready"),
        };
        OciRuntimeBackend::new(
            configured_engine,
            DIGEST.to_owned(),
            agents,
            RuntimeLimits::default(),
            EngineClientEnvironment {
                home: root.clone(),
                xdg_runtime_dir: Some(root),
            },
        )
        .expect("scavenge backend")
        .with_probe_override(standing)
    }

    fn scope(adam: PathBuf, attempt: &str) -> RuntimeScope {
        RuntimeScope {
            agent: ProfileId::from_genesis(b"Adam runtime subject"),
            delegated_by: ProfileId::from_genesis(b"Adam runtime owner"),
            agent_home: adam,
            run: "run_01".to_owned(),
            attempt: attempt.to_owned(),
        }
    }

    #[test]
    fn exact_argv_has_only_the_pinned_fail_closed_isolation_shape() {
        let (_temp, agents, adam) = fixture();
        let backend = ready_backend(agents);
        let prepared = backend.prepare(&scope(adam, "op_01")).expect("prepare");
        let home = prepared.persistent_home.display().to_string();
        let hooks = backend.hooks_dir.display().to_string();
        let cidfile = prepared.cidfile.display().to_string();
        let attempt = scratch_name("run_01", "op_01");
        assert_eq!(
            prepared.args,
            vec![
                &format!("--hooks-dir={hooks}"),
                "run",
                "--rm",
                "--init",
                "--interactive",
                "--pull=never",
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--network=none",
                "--http-proxy=false",
                "--unsetenv-all",
                "--ipc=private",
                "--userns=keep-id",
                "--log-driver=none",
                "--timeout=300",
                &format!("--cidfile={cidfile}"),
                &format!("--name=lait-agent-{attempt}"),
                "--label=lait.run=run_01",
                "--label=lait.attempt=op_01",
                "--cpus=1.000",
                "--memory=536870912b",
                "--pids-limit=64",
                "--ulimit=nofile=256:256",
                "--ulimit=fsize=67108864:67108864",
                &format!("--volume={home}:/home/agent:rw,nosuid,nodev"),
                "--tmpfs=/scratch:rw,nosuid,nodev,size=67108864",
                "--workdir=/scratch",
                "--env=HOME=/home/agent",
                "--env=TMPDIR=/scratch",
                "--entrypoint=/bin/sh",
                DIGEST,
                "-s",
            ]
        );
        assert!(prepared.clear_environment);
        assert_eq!(
            prepared.environment.get(..2),
            Some(
                &[
                    (
                        "HOME".to_owned(),
                        backend
                            .agents_root
                            .parent()
                            .expect("root")
                            .display()
                            .to_string()
                    ),
                    (
                        "XDG_RUNTIME_DIR".to_owned(),
                        backend
                            .agents_root
                            .parent()
                            .expect("root")
                            .display()
                            .to_string()
                    ),
                ][..]
            )
        );
        assert!(prepared
            .environment
            .iter()
            .any(|(name, _)| name == "CONTAINERS_CONF"));
        assert!(prepared
            .environment
            .iter()
            .any(|(name, _)| name == "CONTAINERS_MOUNTS_CONF"));
        assert_eq!(prepared.working_directory, prepared.scratch);
        assert!(prepared.enforcement.read_only_root);
        assert!(prepared.enforcement.capabilities_dropped);
        assert!(prepared.enforcement.no_new_privileges);
        assert!(prepared.enforcement.network_none);
        assert!(!prepared.enforcement.engine_socket_mounted);
        assert!(!prepared.enforcement.ambient_environment);
        assert!(!prepared.enforcement.ambient_working_directory);
        assert!(!prepared.enforcement.unrestricted_filesystem);
        assert!(!prepared.enforcement.secrets_mounted);
        assert!(!prepared.enforcement.externally_attested);
        assert_eq!(prepared.enforcement.wall, LimitEnforcement::OuterRuntime);
        assert_eq!(prepared.enforcement.output, LimitEnforcement::OuterRuntime);
        assert_eq!(prepared.enforcement.cpu, LimitEnforcement::OciEngine);
        assert!(prepared.verify_binding());
        let mut retargeted = prepared.clone();
        retargeted.args.push("different".to_owned());
        assert!(!retargeted.verify_binding());
        let mut retargeted = prepared;
        retargeted
            .environment
            .push(("HOST_SECRET".to_owned(), "ambient".to_owned()));
        assert!(!retargeted.verify_binding());
    }

    #[test]
    fn configuration_binding_changes_with_every_selectable_component() {
        let (_temp, agents, _adam) = fixture();
        let backend = ready_backend(agents);
        let standing = backend.probe();
        let base = backend
            .configuration_binding(&standing)
            .expect("base configuration");
        let RuntimeProviderStanding::Ready { engine, image, .. } = &standing else {
            panic!("fixture is ready");
        };
        let changed_posture = RuntimeProviderStanding::Ready {
            engine: engine.clone(),
            image: image.clone(),
            posture: RuntimeProviderPosture::LocalManagedMachine {
                name: "different-machine".into(),
            },
        };
        assert_ne!(
            derive_configuration_binding(
                engine,
                &backend.client_environment,
                &changed_posture,
                backend.limits,
            ),
            Some(base)
        );
        let mut changed_environment = backend.client_environment.clone();
        changed_environment.push(("UNEXPECTED".into(), "value".into()));
        assert_ne!(
            derive_configuration_binding(engine, &changed_environment, &standing, backend.limits),
            Some(base)
        );
        let mut changed_limits = backend.limits;
        changed_limits.pids = changed_limits.pids.saturating_add(1);
        assert_ne!(
            derive_configuration_binding(
                engine,
                &backend.client_environment,
                &standing,
                changed_limits,
            ),
            Some(base)
        );
        let changed_image = RuntimeProviderStanding::Ready {
            engine: engine.clone(),
            image: format!("{image}-different"),
            posture: RuntimeProviderPosture::NativeRootless,
        };
        assert_ne!(
            derive_configuration_binding(
                engine,
                &backend.client_environment,
                &changed_image,
                backend.limits,
            ),
            Some(base)
        );
        let changed_engine = engine.with_extension("different");
        let changed_engine_standing = RuntimeProviderStanding::Ready {
            engine: changed_engine.clone(),
            image: image.clone(),
            posture: RuntimeProviderPosture::NativeRootless,
        };
        assert_ne!(
            derive_configuration_binding(
                &changed_engine,
                &backend.client_environment,
                &changed_engine_standing,
                backend.limits,
            ),
            Some(base)
        );
    }

    #[test]
    fn pins_paths_and_runtime_coordinates_are_refused_before_spawn() {
        let (_temp, agents, adam) = fixture();
        assert!(matches!(
            OciRuntimeBackend::new(
                PathBuf::from("docker"),
                DIGEST.to_owned(),
                agents.clone(),
                RuntimeLimits::default(),
                client_environment(&agents)
            ),
            Err(RuntimeBackendError::InvalidEnginePath(_))
        ));
        assert!(matches!(
            OciRuntimeBackend::new(
                PathBuf::from("/usr/bin/docker"),
                "registry.example/agent:latest".to_owned(),
                agents.clone(),
                RuntimeLimits::default(),
                client_environment(&agents)
            ),
            Err(RuntimeBackendError::InvalidImagePin)
        ));

        let backend = ready_backend(agents);
        let mut traversal = scope(adam.clone(), "../outside");
        assert!(matches!(
            backend.prepare(&traversal),
            Err(RuntimeBackendError::InvalidScope(_))
        ));
        traversal.attempt = "safe".to_owned();
        traversal.run = "../../run".to_owned();
        assert!(matches!(
            backend.prepare(&traversal),
            Err(RuntimeBackendError::InvalidScope(_))
        ));
        traversal.run = "safe".to_owned();
        traversal.attempt = "unsafe\0--privileged".to_owned();
        assert!(matches!(
            backend.prepare(&traversal),
            Err(RuntimeBackendError::InvalidScope(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_refused_for_agent_home_and_runtime_mounts() {
        use std::os::unix::fs::symlink;

        let (temp, agents, adam) = fixture();
        let backend = ready_backend(agents.clone());
        let sideways = temp.path().join("sideways");
        std::fs::create_dir(&sideways).expect("sideways directory");
        let linked = agents.join("linked");
        symlink(&sideways, &linked).expect("agent symlink");
        assert!(matches!(
            backend.prepare(&scope(linked, "op_link")),
            Err(RuntimeBackendError::InvalidScope(_))
        ));

        let agent_dir = adam.join("agent");
        std::fs::create_dir(&agent_dir).expect("agent directory");
        symlink(&sideways, agent_dir.join("runtime")).expect("runtime symlink");
        assert!(matches!(
            backend.prepare(&scope(adam, "op_runtime_link")),
            Err(RuntimeBackendError::InvalidScope(_))
        ));
    }

    #[test]
    fn absent_engine_is_unavailable_and_never_falls_back_to_the_workload() {
        let (_temp, agents, adam) = fixture();
        let engine = agents
            .parent()
            .expect("temporary root")
            .join("no-such-oci-engine");
        let client_environment = client_environment(&agents);
        let backend = OciRuntimeBackend::new(
            engine,
            DIGEST.to_owned(),
            agents,
            RuntimeLimits::default(),
            client_environment,
        )
        .expect("configured backend");
        assert_eq!(
            backend.probe(),
            RuntimeProviderStanding::Unavailable(RuntimeProviderUnavailable::MissingEngine)
        );
        assert!(matches!(
            backend.prepare(&scope(adam, "op_missing")),
            Err(RuntimeBackendError::ProviderUnavailable(
                RuntimeProviderUnavailable::MissingEngine
            ))
        ));
    }

    #[test]
    fn no_engine_socket_ambient_environment_or_network_can_enter_the_invocation() {
        let (_temp, agents, adam) = fixture();
        let prepared = ready_backend(agents)
            .prepare(&scope(adam, "op_closed"))
            .expect("prepare");
        let joined = prepared.args.join("\n");
        assert!(prepared.clear_environment);
        assert!(joined.contains("--network=none"));
        assert!(!joined.contains("docker.sock"));
        assert!(!joined.contains("podman.sock"));
        assert!(!joined.contains("DOCKER_HOST"));
        assert!(!joined.contains("CONTAINER_HOST"));
        assert!(joined.contains("--unsetenv-all"));
        assert!(joined.contains("--http-proxy=false"));
        assert_eq!(
            prepared
                .args
                .iter()
                .filter(|argument| argument.starts_with("--volume="))
                .count(),
            1
        );
        assert!(!prepared.args.iter().any(|argument| {
            argument
                == &format!(
                    "--volume={}:/scratch:rw,nosuid,nodev",
                    prepared.scratch.parent().unwrap().display()
                )
        }));
    }

    #[test]
    fn provider_standing_and_fresh_scratch_are_explicit() {
        let (_temp, agents, adam) = fixture();
        let backend = ready_backend(agents);
        assert!(backend.probe().is_ready());
        assert_eq!(
            backend.probe().inventory_standing(),
            crate::PrimitiveStanding::Ready
        );
        let first = backend
            .prepare(&scope(adam.clone(), "op_once"))
            .expect("first");
        assert!(matches!(
            first.provider,
            RuntimeProviderStanding::Ready { .. }
        ));
        assert!(matches!(
            backend.prepare(&scope(adam, "op_once")),
            Err(RuntimeBackendError::InvalidScope(
                "scratch attempt already exists"
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn stale_attempt_scavenging_never_removes_a_live_lease() {
        let (_temp, agents, adam) = fixture();
        let backend = scavenge_backend(agents);
        let prepared = backend
            .prepare(&scope(adam, "live_attempt"))
            .expect("prepare live attempt");
        let scratch = prepared.scratch.clone();

        assert_eq!(backend.scavenge_stale_attempts().expect("scavenge"), 0);
        assert!(scratch.is_dir(), "live scratch must remain");
        drop(prepared);
    }

    #[cfg(unix)]
    #[test]
    fn stale_attempt_scavenging_removes_container_before_host_scratch() {
        let (_temp, agents, adam) = fixture();
        let backend = scavenge_backend(agents);
        let prepared = backend
            .prepare(&scope(adam, "crashed_attempt"))
            .expect("prepare crashed attempt");
        let scratch = prepared.scratch.clone();
        std::fs::write(&prepared.cidfile, b"fixture-container-id\n")
            .expect("write crashed cidfile");
        drop(prepared);

        assert_eq!(backend.scavenge_stale_attempts().expect("scavenge"), 1);
        assert!(!scratch.exists(), "stale scratch must be removed");
    }

    #[cfg(unix)]
    #[test]
    fn stale_attempt_scavenging_removes_pre_spawn_scratch_without_engine_cleanup() {
        let (_temp, agents, adam) = fixture();
        let backend = scavenge_backend(agents);
        let prepared = backend
            .prepare(&scope(adam, "crashed_before_spawn"))
            .expect("prepare pre-spawn attempt");
        let scratch = prepared.scratch.clone();
        assert!(!prepared.cidfile.exists());
        drop(prepared);

        assert_eq!(backend.scavenge_stale_attempts().expect("scavenge"), 1);
        assert!(!scratch.exists(), "pre-spawn scratch must be removed");
    }

    #[cfg(unix)]
    #[test]
    fn stale_attempt_scavenging_quarantines_entries_without_current_lease() {
        let (_temp, agents, adam) = fixture();
        let backend = scavenge_backend(agents);
        let agent = adam.join("agent");
        let runtime = agent.join("runtime");
        let scratch_parent = runtime.join("scratch");
        std::fs::create_dir(&agent).expect("agent runtime root");
        std::fs::create_dir(&runtime).expect("runtime root");
        std::fs::create_dir(&scratch_parent).expect("scratch root");
        let legacy = scratch_parent.join(format!("attempt-{}", "a".repeat(64)));
        std::fs::create_dir(&legacy).expect("legacy scratch");

        assert_eq!(backend.scavenge_stale_attempts().expect("scavenge"), 0);
        assert!(
            legacy.is_dir(),
            "unversioned scratch must remain quarantined"
        );
    }

    #[cfg(unix)]
    #[test]
    fn production_probe_uses_only_bounded_environment_and_reports_unhealthy_engine() {
        use std::os::unix::fs::PermissionsExt as _;

        let (temp, agents, _adam) = fixture();
        let engine = temp.path().join("unhealthy-oci");
        std::fs::write(
            &engine,
            "#!/bin/sh\n[ \"$#\" = 2 ] || exit 91\n[ \"$1\" = info ] || exit 92\n[ \"$2\" = '--format=json' ] || exit 93\n[ -z \"$LAIT_OCI_PROBE_AMBIENT\" ] || exit 94\n[ -n \"$HOME\" ] || exit 95\n[ -n \"$XDG_RUNTIME_DIR\" ] || exit 96\nexit 17\n",
        )
        .expect("write probe engine");
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700))
            .expect("make probe executable");
        let engine = engine.canonicalize().expect("canonical engine");
        let client_environment = client_environment(&agents);
        let backend = OciRuntimeBackend::new(
            engine,
            DIGEST.to_owned(),
            agents,
            RuntimeLimits::default(),
            client_environment,
        )
        .expect("backend");
        std::env::set_var("LAIT_OCI_PROBE_AMBIENT", "must-not-pass");
        let standing = backend.probe();
        std::env::remove_var("LAIT_OCI_PROBE_AMBIENT");
        assert_eq!(
            standing,
            RuntimeProviderStanding::Unavailable(RuntimeProviderUnavailable::ProbeFailed {
                exit_code: Some(17)
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_machine_is_refused_until_server_policy_can_be_verified() {
        use std::os::unix::fs::PermissionsExt as _;

        let (temp, agents, adam) = fixture();
        let engine = temp.path().join("managed-podman");
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = info ] && [ "$2" = '--format=json' ]; then
  printf '%s' '{{"host":{{"security":{{"rootless":true,"seccompEnabled":true}},"serviceIsRemote":true}}}}'
  exit 0
fi
if [ "$1" = system ] && [ "$2" = connection ] && [ "$3" = list ] && [ "$4" = '--format=json' ]; then
  printf '%s' '[{{"Name":"podman-machine-default","URI":"ssh://core@127.0.0.1:50123/run/user/501/podman/podman.sock","IsMachine":true,"Default":true,"ReadWrite":true}}]'
  exit 0
fi
if [ "$1" = machine ] && [ "$2" = inspect ] && [ "$3" = podman-machine-default ]; then
  printf '%s' '[{{"Name":"podman-machine-default","State":"running","Rootful":false}}]'
  exit 0
fi
if [ "$1" = '--connection=podman-machine-default' ] && [ "$2" = image ] && [ "$3" = exists ] && [ "$4" = '{DIGEST}' ]; then
  exit 0
fi
exit 97
"#
        );
        std::fs::write(&engine, script).expect("write managed engine");
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700))
            .expect("make managed engine executable");
        let engine = engine.canonicalize().expect("canonical engine");
        let backend = OciRuntimeBackend::new(
            engine.clone(),
            DIGEST.to_owned(),
            agents,
            RuntimeLimits::default(),
            EngineClientEnvironment {
                home: temp.path().canonicalize().expect("client home"),
                xdg_runtime_dir: None,
            },
        )
        .expect("backend");
        assert_eq!(
            backend.probe(),
            RuntimeProviderStanding::Unavailable(
                RuntimeProviderUnavailable::RemotePolicyUnverified
            )
        );
        assert!(matches!(
            backend.prepare(&scope(adam, "managed_01")),
            Err(RuntimeBackendError::ProviderUnavailable(
                RuntimeProviderUnavailable::RemotePolicyUnverified
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn arbitrary_remote_connection_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let (temp, agents, _adam) = fixture();
        let engine = temp.path().join("remote-podman");
        std::fs::write(
            &engine,
            "#!/bin/sh\nif [ \"$1\" = info ]; then printf '%s' '{\"host\":{\"security\":{\"rootless\":true,\"seccompEnabled\":true},\"serviceIsRemote\":true}}'; exit 0; fi\nprintf '%s' '[{\"Name\":\"remote\",\"URI\":\"ssh://operator@example.com/run/podman.sock\",\"IsMachine\":false,\"Default\":true,\"ReadWrite\":true}]'\n",
        )
        .expect("write remote engine");
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700))
            .expect("make remote engine executable");
        let engine = engine.canonicalize().expect("canonical engine");
        let backend = OciRuntimeBackend::new(
            engine,
            DIGEST.to_owned(),
            agents,
            RuntimeLimits::default(),
            EngineClientEnvironment {
                home: temp.path().canonicalize().expect("client home"),
                xdg_runtime_dir: None,
            },
        )
        .expect("backend");
        assert_eq!(
            backend.probe(),
            RuntimeProviderStanding::Unavailable(
                RuntimeProviderUnavailable::MachineConnectionUnsafe
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_mounts_are_writable_by_rootless_keep_id_without_weakening_permissions() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let (_temp, agents, adam) = fixture();
        let prepared = ready_backend(agents)
            .prepare(&scope(adam, "permissions_01"))
            .expect("prepare");
        for mount in [&prepared.persistent_home, &prepared.scratch] {
            let metadata = std::fs::metadata(mount).expect("mount metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
            assert_eq!(metadata.uid(), std::fs::metadata(".").expect("cwd").uid());
            let proof = mount.join("writable-by-daemon-owner");
            std::fs::write(&proof, b"ok").expect("owner can write mount");
        }
        assert!(prepared.args.iter().any(|arg| arg == "--userns=keep-id"));
        assert!(!prepared.args.iter().any(|arg| arg.starts_with("--user=")));
    }

    #[cfg(unix)]
    #[test]
    fn engine_symlinks_and_agent_controlled_client_configuration_are_refused() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let (temp, agents, adam) = fixture();
        let engine = temp.path().join("real-engine");
        std::fs::write(&engine, "#!/bin/sh\nexit 0\n").expect("write engine");
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700))
            .expect("make engine executable");
        let linked = temp.path().join("linked-engine");
        symlink(&engine, &linked).expect("engine symlink");
        let backend = OciRuntimeBackend::new(
            linked,
            DIGEST.to_owned(),
            agents.clone(),
            RuntimeLimits::default(),
            client_environment(&agents),
        )
        .expect("configuration defers engine existence checks to probe");
        assert_eq!(
            backend.probe(),
            RuntimeProviderStanding::Unavailable(RuntimeProviderUnavailable::EnginePathIsSymlink)
        );
        assert!(matches!(
            OciRuntimeBackend::new(
                engine.canonicalize().expect("canonical engine"),
                DIGEST.to_owned(),
                agents,
                RuntimeLimits::default(),
                EngineClientEnvironment {
                    home: adam,
                    xdg_runtime_dir: None,
                },
            ),
            Err(RuntimeBackendError::InvalidClientEnvironment(_))
        ));
    }

    #[test]
    fn installed_podman_help_exposes_every_relied_upon_run_flag() {
        let candidates = [
            Path::new("/opt/homebrew/bin/podman"),
            Path::new("/usr/local/bin/podman"),
            Path::new("/usr/bin/podman"),
        ];
        let Some(engine) = candidates.iter().find(|candidate| candidate.is_file()) else {
            return;
        };
        let output = Command::new(engine)
            .args(["run", "--help"])
            .env_clear()
            .output()
            .expect("installed Podman run help");
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).expect("UTF-8 Podman help");
        for flag in [
            "--rm",
            "--init",
            "--interactive",
            "--pull",
            "--read-only",
            "--cap-drop",
            "--security-opt",
            "--network",
            "--http-proxy",
            "--unsetenv-all",
            "--ipc",
            "--userns",
            "--log-driver",
            "--timeout",
            "--cidfile",
            "--name",
            "--label",
            "--cpus",
            "--memory",
            "--pids-limit",
            "--ulimit",
            "--volume",
            "--tmpfs",
        ] {
            assert!(help.contains(flag), "installed Podman lacks {flag}");
        }
        let global = Command::new(engine)
            .arg("--help")
            .env_clear()
            .output()
            .expect("installed Podman global help");
        assert!(global.status.success());
        assert!(
            String::from_utf8(global.stdout)
                .expect("UTF-8 Podman global help")
                .contains("--hooks-dir"),
            "installed Podman lacks --hooks-dir"
        );
    }
}
