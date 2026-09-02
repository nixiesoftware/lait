#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::todo,
        clippy::panic
    )
)]

//! A supervised process boundary for independently shipped Worlds.
//!
//! A release is immutable and a running instance stays pinned to it. Updating
//! an installed World therefore changes only what the next launch selects;
//! [`Instance::relaunch`] is the explicit generation transition. The child is
//! addressed through a bounded loopback protocol and is stopped only through
//! the owned process handle retained here.

pub mod wasm_abi;

#[cfg(target_arch = "wasm32")]
pub mod guest;

mod protocol;
#[cfg(not(target_arch = "wasm32"))]
mod server;
mod service;

pub use protocol::{
    decode_frame, encode_frame, read_frame, write_frame, Operation, Ready, Reply, Request,
    ServiceDescriptor, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
#[cfg(not(target_arch = "wasm32"))]
pub use server::serve;
pub use service::{Host, Service};

use std::fs;
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

/// What a World runner is given from the environment it was launched from.
///
/// An allowlist rather than a blocklist: the interesting variables are the
/// ones nobody thought of, and a blocklist is a list of the ones somebody did.
/// A World that needs more than this should declare it in `world.json`, where
/// it is a reviewable part of the release rather than an accident of whichever
/// process happened to start the daemon.
const INHERITED: &[&str] = &[
    // Finding a linker, a shell, a system library.
    "PATH",
    // Where a process is allowed to write scratch.
    "TMPDIR",
    "TMP",
    "TEMP",
    // Locale and encoding, or text handling differs from the host's.
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    // Windows needs these to resolve system libraries at all.
    "SystemRoot",
    "SystemDrive",
    "windir",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "COMSPEC",
    // A home for platform conventions that resolve one. Not the user's
    // credentials — those live in files under it that a World has no reason
    // to open, and this does not hand over the reason.
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];

const READY_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_BUDGET: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(50);

/// Where a release's bytes came from, and what this host is able to say about
/// them.
///
/// Not an `Option<[u8; 32]>`, because the absent case is not a missing digest —
/// it is a different kind of thing to be running, and the World's own process
/// is told which. Nothing here re-verifies anything: staging is what proves a
/// sealed release, so this is a provenance label and always was. Which is
/// exactly why a tree somebody is working on must not be handed a plausible
/// one — a fabricated digest would make it indistinguishable from a release to
/// the one party downstream of every check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A sealed release, admitted when it was staged.
    Sealed([u8; 32]),
    /// A tree on this machine that nobody sealed and nothing verified.
    Local,
}

impl Provenance {
    /// What the World's process is told it is running, in `LAIT_WORLD_RELEASE`.
    pub fn stated(&self) -> String {
        match self {
            Self::Sealed(digest) => data_encoding::HEXLOWER.encode(digest),
            Self::Local => "local".to_owned(),
        }
    }
}

/// The exact immutable release an instance must execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub world: String,
    pub version: String,
    pub provenance: Provenance,
    pub root: PathBuf,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl Release {
    /// Construct a release from paths declared inside its bundle.
    pub fn under(
        root: impl Into<PathBuf>,
        world: impl Into<String>,
        version: impl Into<String>,
        provenance: Provenance,
        program: impl AsRef<Path>,
        args: Vec<String>,
        cwd: Option<impl AsRef<Path>>,
    ) -> Result<Self> {
        let root = root.into();
        let program = relative_inside("program", program.as_ref())?;
        let cwd = cwd
            .map(|path| relative_inside("working directory", path.as_ref()))
            .transpose()?;
        Ok(Self {
            world: world.into(),
            version: version.into(),
            provenance,
            program,
            args,
            cwd,
            root,
        })
    }

    fn executable(&self) -> PathBuf {
        self.root.join(&self.program)
    }
}

fn relative_inside(kind: &str, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!(
            "World {kind} '{}' is not a plain relative bundle path",
            path.display()
        );
    }
    Ok(path.to_path_buf())
}

/// What stopping an owned World instance actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    Stopped,
    Forced,
    WasAlreadyGone { status: String },
}

/// Handles callbacks that outlive the top-level request which created their
/// capability (for example, a bounded projection worker over a retained Find
/// publication).
pub trait CallbackHandler: Send + Sync + 'static {
    fn call(&self, operation: &str, payload: &[u8]) -> Result<Vec<u8>, String>;
}

impl<F> CallbackHandler for F
where
    F: Fn(&str, &[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
{
    fn call(&self, operation: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
        self(operation, payload)
    }
}

/// One bounded request route to an already supervised World generation.
///
/// Creating a client performs crash detection and any generation-preserving
/// restart while the owning [`Instance`] is exclusively borrowed. Transport
/// then proceeds independently, allowing a World callback to re-enter the
/// same process over a second connection without deadlocking supervision.
pub struct RequestClient {
    world: String,
    ready: Ready,
    id: u64,
}

impl std::fmt::Debug for RequestClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestClient")
            .field("world", &self.world)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

struct NoDetachedCallbacks;

impl CallbackHandler for NoDetachedCallbacks {
    fn call(&self, operation: &str, _payload: &[u8]) -> Result<Vec<u8>, String> {
        Err(format!("unsupported detached World callback {operation:?}"))
    }
}

/// A detached handler that refuses — the caller retains no Find lease, so no
/// callback outlives the request. The default for every request that is not a
/// semantic call.
pub fn no_detached_callbacks() -> std::sync::Arc<dyn CallbackHandler> {
    std::sync::Arc::new(NoDetachedCallbacks)
}

/// A supervised World generation, whatever executes it: a native child process
/// or a WebAssembly module the host runs in-process. The host addresses it
/// through the same postcard `Operation`/`Reply` frames either way — the seam
/// exists so everything above the frame (all of `world-sdk`, all product code)
/// is written once and neither backend leaks its transport upward.
pub trait HostedRunner: Send {
    /// The reviewed service identity declared at readiness, checked against the
    /// descriptor before any call is dispatched.
    fn descriptor(&self) -> &ServiceDescriptor;
    /// Prepare one request route to this exact generation. Preparation detects
    /// and restarts a crashed generation (preserving its immutable identity)
    /// and allocates a request id; the returned [`Conversation`] then runs
    /// independently, so the host releases the supervision lock before
    /// dispatching — a World callback may synchronously re-enter this same
    /// generation.
    fn open(&mut self) -> Result<Box<dyn Conversation>>;
}

/// One prepared request against a [`HostedRunner`]. Given an `Operation` and a
/// synchronous callback, it produces a `Reply`, servicing every callback the
/// World raises in between. Owned and independent of the runner's supervision
/// lock, so two conversations may run without deadlocking a re-entrant call.
pub trait Conversation: Send {
    fn dispatch(
        &mut self,
        operation: Operation,
        callback: &mut dyn FnMut(&str, &[u8]) -> Result<Vec<u8>, String>,
        detached: std::sync::Arc<dyn CallbackHandler>,
    ) -> Result<Reply>;
}

impl Conversation for RequestClient {
    fn dispatch(
        &mut self,
        operation: Operation,
        callback: &mut dyn FnMut(&str, &[u8]) -> Result<Vec<u8>, String>,
        detached: std::sync::Arc<dyn CallbackHandler>,
    ) -> Result<Reply> {
        self.request_with_detached(operation, callback, detached)
    }
}

/// One running process pinned to one exact release.
pub struct Instance {
    release: Release,
    ready: Ready,
    service: ServiceDescriptor,
    process_group: ProcessGroup,
    child: Child,
    next_request: u64,
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Instance")
            .field("world", &self.release.world)
            .field("version", &self.release.version)
            .field("provenance", &self.release.provenance.stated())
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl Instance {
    /// Spawn a release and wait for its authenticated readiness declaration.
    pub fn launch(mut release: Release) -> Result<Self> {
        let root = release
            .root
            .canonicalize()
            .with_context(|| format!("World release root {} is absent", release.root.display()))?;
        if !root.is_dir() {
            bail!("World release root {} is not a directory", root.display());
        }
        release.root = root;
        let executable = release.executable().canonicalize().with_context(|| {
            format!(
                "World executable {} is absent",
                release.executable().display()
            )
        })?;
        if !executable.starts_with(&release.root) {
            bail!(
                "World executable {} resolves outside release {}",
                executable.display(),
                release.root.display()
            );
        }
        let metadata = fs::metadata(&executable)
            .with_context(|| format!("World executable {} is absent", executable.display()))?;
        if !metadata.is_file() {
            bail!("World executable {} is not a file", executable.display());
        }

        let working_directory = release
            .cwd
            .as_deref()
            .map_or_else(
                || release.root.clone(),
                |relative| release.root.join(relative),
            )
            .canonicalize()
            .context("resolve World working directory")?;
        if !working_directory.starts_with(&release.root) {
            bail!(
                "World working directory {} resolves outside release {}",
                working_directory.display(),
                release.root.display()
            );
        }
        if !working_directory.is_dir() {
            bail!(
                "World working directory {} is absent",
                working_directory.display()
            );
        }

        let mut command = Command::new(&executable);
        // Cleared, then filled deliberately. A World runner used to inherit
        // whatever launched it — and what launches it is a daemon, a head, or
        // `lait mcp`, which an editor spawns. So the runner inherited the
        // *editor's* environment: API keys, cloud credentials, `SSH_AUTH_SOCK`.
        // That is the payoff in the IDE auto-execution class of vulnerability,
        // and it costs one line not to hand it over.
        //
        // Sealed releases are cleared too, not only unsealed trees. A signed
        // World has no more business holding somebody's cloud credentials than
        // an unsigned one, and a control that applies to one kind is a control
        // somebody has to remember to extend.
        command.env_clear();
        // And none of the daemon's capabilities either, for the same reason
        // the environment is cleared: what launches a runner is a process
        // that may hold more authority than the runner has any business with.
        disinherit_capabilities(&mut command);
        for name in INHERITED {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .args(&release.args)
            .current_dir(&working_directory)
            .env("LAIT_WORLD_ID", &release.world)
            .env("LAIT_WORLD_VERSION", &release.version)
            .env("LAIT_WORLD_RELEASE", release.provenance.stated())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        own_process_group(&mut command);
        no_console(&mut command);

        let mut child = command
            .spawn()
            .with_context(|| format!("launch World {}", executable.display()))?;
        let process_group = ProcessGroup::attach(&child).context("contain World process tree")?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("World process exposed no readiness channel"))?;
        let ready = match read_ready(stdout) {
            Ok(ready) => ready,
            Err(error) => {
                process_group.force(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if ready.protocol != PROTOCOL_VERSION {
            process_group.force(&child);
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "World {} speaks runner protocol {}, host speaks {}",
                release.world,
                ready.protocol,
                PROTOCOL_VERSION
            );
        }
        if ready.world != release.world || ready.version != release.version {
            process_group.force(&child);
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "World process answered for {} {} while release is {} {}",
                ready.world,
                ready.version,
                release.world,
                release.version
            );
        }

        let mut instance = Self {
            release,
            ready,
            service: ServiceDescriptor {
                world: String::new(),
                implementation: [0; 32],
                implementation_version: 0,
            },
            process_group,
            child,
            next_request: 1,
        };
        match instance.request(Operation::Describe)? {
            Reply::Descriptor(descriptor) if descriptor.world == instance.release.world => {
                instance.service = descriptor;
            }
            Reply::Descriptor(descriptor) => {
                let actual = descriptor.world;
                let _ = instance.stop();
                bail!("World service described {actual}, not the release it launched for")
            }
            other => {
                let _ = instance.stop();
                bail!("World service did not describe itself: {other:?}")
            }
        }
        Ok(instance)
    }

    pub fn release(&self) -> &Release {
        &self.release
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn service(&self) -> &ServiceDescriptor {
        &self.service
    }

    /// Prepare one request against this exact supervised generation.
    ///
    /// The returned client does not own or mutate process supervision. If the
    /// process fails after preparation, that request fails without replay; the
    /// next client preparation restores the same immutable release.
    pub fn client(&mut self) -> Result<RequestClient> {
        // Supervision is generation-preserving: if the child died between
        // calls, restart the same immutable release before admitting the next
        // call. A call that may have reached a dying child is never retried.
        self.restart_if_gone()?;
        let id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or_else(|| anyhow!("World runner request id exhausted"))?;
        Ok(RequestClient {
            world: self.release.world.clone(),
            ready: self.ready.clone(),
            id,
        })
    }

    pub fn request(&mut self, operation: Operation) -> Result<Reply> {
        self.request_with(operation, |operation, _| {
            Err(format!("unsupported World callback {operation:?}"))
        })
    }

    /// Make one request, servicing bounded callbacks on the same authenticated
    /// connection until the World returns its final reply.
    pub fn request_with(
        &mut self,
        operation: Operation,
        mut callback: impl FnMut(&str, &[u8]) -> Result<Vec<u8>, String>,
    ) -> Result<Reply> {
        self.request_with_detached(
            operation,
            &mut callback,
            std::sync::Arc::new(NoDetachedCallbacks),
        )
    }

    pub fn request_with_detached(
        &mut self,
        operation: Operation,
        callback: &mut dyn FnMut(&str, &[u8]) -> Result<Vec<u8>, String>,
        detached: std::sync::Arc<dyn CallbackHandler>,
    ) -> Result<Reply> {
        self.client()?
            .request_with_detached(operation, callback, detached)
    }

    pub fn ping(&mut self) -> Result<()> {
        match self.request(Operation::Ping)? {
            Reply::Pong => Ok(()),
            other => bail!("World answered ping with {other:?}"),
        }
    }

    /// Restore a crashed child from the same immutable release. Returns
    /// whether a replacement was launched. The service identity must remain
    /// byte-for-byte stable; a nondeterministic executable is not the same
    /// generation merely because its path is unchanged.
    pub fn restart_if_gone(&mut self) -> Result<bool> {
        if self
            .child
            .try_wait()
            .context("poll World process")?
            .is_none()
        {
            return Ok(false);
        }
        let replacement = Self::launch(self.release.clone())?;
        if replacement.service != self.service {
            let actual = replacement.service.clone();
            let _ = replacement.stop();
            bail!(
                "World {} changed service identity while restarting the same release: {actual:?}",
                self.release.world
            );
        }
        let retired = std::mem::replace(self, replacement);
        drop(retired);
        Ok(true)
    }

    /// Replace this process with a newly selected immutable release.
    pub fn relaunch(self, release: Release) -> Result<(Stopped, Self)> {
        let stopped = self.stop()?;
        let replacement = Self::launch(release)?;
        Ok((stopped, replacement))
    }

    pub fn stop(mut self) -> Result<Stopped> {
        if let Some(status) = self.child.try_wait().context("poll World process")? {
            return Ok(Stopped::WasAlreadyGone {
                status: status.to_string(),
            });
        }
        let _ = self.request(Operation::Stop);
        let deadline = Instant::now().checked_add(STOP_BUDGET);
        while deadline.is_some_and(|until| Instant::now() < until) {
            if self
                .child
                .try_wait()
                .context("poll World shutdown")?
                .is_some()
            {
                return Ok(Stopped::Stopped);
            }
            std::thread::sleep(STOP_POLL);
        }
        self.process_group.force(&self.child);
        self.child.kill().context("force World process to stop")?;
        self.child.wait().context("collect World process")?;
        Ok(Stopped::Forced)
    }
}

impl HostedRunner for Instance {
    fn descriptor(&self) -> &ServiceDescriptor {
        // The inherent accessor; the trait exists so a wasm-backed runner can
        // answer the same question without a process behind it.
        Instance::service(self)
    }

    fn open(&mut self) -> Result<Box<dyn Conversation>> {
        Ok(Box::new(Instance::client(self)?))
    }
}

impl RequestClient {
    pub fn request(&mut self, operation: Operation) -> Result<Reply> {
        self.request_with(operation, |operation, _| {
            Err(format!("unsupported World callback {operation:?}"))
        })
    }

    /// Make one request, servicing bounded callbacks on the same authenticated
    /// connection until the World returns its final reply.
    pub fn request_with(
        &mut self,
        operation: Operation,
        mut callback: impl FnMut(&str, &[u8]) -> Result<Vec<u8>, String>,
    ) -> Result<Reply> {
        self.request_with_detached(
            operation,
            &mut callback,
            std::sync::Arc::new(NoDetachedCallbacks),
        )
    }

    pub fn request_with_detached(
        &mut self,
        operation: Operation,
        callback: &mut dyn FnMut(&str, &[u8]) -> Result<Vec<u8>, String>,
        detached: std::sync::Arc<dyn CallbackHandler>,
    ) -> Result<Reply> {
        let id = self.id;
        let request = Request {
            protocol: PROTOCOL_VERSION,
            token: self.ready.token.clone(),
            id,
            operation,
        };
        let mut stream = TcpStream::connect(self.ready.address)
            .with_context(|| format!("connect to World {}", self.world))?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .context("bound World response read")?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .context("bound World request write")?;
        write_frame(&mut stream, &request).context("send World request")?;
        loop {
            let response: protocol::Response =
                read_frame(&mut stream).context("read World response")?;
            match response {
                protocol::Response::Complete {
                    id: response_id,
                    outcome,
                } => {
                    if response_id != id {
                        bail!("World response id {response_id} does not match request {id}");
                    }
                    let reply = outcome.map_err(|message| anyhow!(message))?;
                    stream
                        .set_read_timeout(None)
                        .context("unbound retained World callback reads")?;
                    stream
                        .set_write_timeout(Some(REQUEST_TIMEOUT))
                        .context("bound retained World callback writes")?;
                    let token = self.ready.token.clone();
                    std::thread::spawn(move || {
                        continue_callbacks(stream, id, &token, detached);
                    });
                    return Ok(reply);
                }
                protocol::Response::Callback {
                    id: response_id,
                    callback: callback_id,
                    operation,
                    payload,
                } => {
                    if response_id != id {
                        bail!(
                            "World callback response id {response_id} does not match request {id}"
                        );
                    }
                    let outcome = callback(&operation, &payload);
                    write_frame(
                        &mut stream,
                        &protocol::CallbackResponse {
                            protocol: PROTOCOL_VERSION,
                            token: self.ready.token.clone(),
                            id,
                            callback: callback_id,
                            outcome,
                        },
                    )
                    .context("answer World callback")?;
                }
            }
        }
    }
}

fn continue_callbacks(
    mut stream: TcpStream,
    request: u64,
    token: &str,
    handler: std::sync::Arc<dyn CallbackHandler>,
) {
    loop {
        let response: protocol::Response = match read_frame(&mut stream) {
            Ok(response) => response,
            Err(_) => return,
        };
        let protocol::Response::Callback {
            id,
            callback,
            operation,
            payload,
        } = response
        else {
            return;
        };
        if id != request {
            return;
        }
        let outcome = handler.call(&operation, &payload);
        if write_frame(
            &mut stream,
            &protocol::CallbackResponse {
                protocol: PROTOCOL_VERSION,
                token: token.to_string(),
                id,
                callback,
                outcome,
            },
        )
        .is_err()
        {
            return;
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.process_group.force(&self.child);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn read_ready(stdout: std::process::ChildStdout) -> Result<Ready> {
    use std::io::{BufRead as _, BufReader};
    use std::sync::mpsc;

    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    break Err(anyhow!(
                        "World process exited before it announced readiness"
                    ))
                }
                Ok(_) if line.trim().is_empty() => {}
                Ok(_) => {
                    let trimmed = line.trim();
                    break serde_json::from_str(trimmed).with_context(|| {
                        format!("World announced unreadable readiness: {trimmed}")
                    });
                }
                Err(error) => break Err(error).context("read World readiness"),
            }
        };
        let _ = send.send(result);
    });
    match receive.recv_timeout(READY_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            bail!("World process did not announce readiness in time")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("World readiness reader ended without an answer")
        }
    }
}

#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {}

struct ProcessGroup {
    #[cfg(windows)]
    // Windows declares HANDLE as a raw pointer, even though handles are opaque
    // process-local values. Store the value rather than the pointer spelling so
    // an owned Instance can safely move to its supervisor thread.
    job: usize,
}

impl ProcessGroup {
    #[cfg(unix)]
    fn attach(_child: &Child) -> Result<Self> {
        Ok(Self {})
    }

    #[cfg(windows)]
    fn attach(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error()).context("create World Job Object");
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                u32::try_from(std::mem::size_of_val(&limits)).unwrap_or(u32::MAX),
            )
        };
        if configured == 0 {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(std::io::Error::last_os_error()).context("configure World Job Object");
        }
        let assigned = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as _) };
        if assigned == 0 {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(std::io::Error::last_os_error()).context("assign World to Job Object");
        }
        Ok(Self { job: job as usize })
    }

    #[cfg(not(any(unix, windows)))]
    fn attach(_child: &Child) -> Result<Self> {
        Ok(Self {})
    }

    #[cfg(unix)]
    fn force(&self, child: &Child) {
        if let Ok(pid) = i32::try_from(child.id()) {
            if let Some(process_group) = pid.checked_neg() {
                unsafe {
                    libc::kill(process_group, libc::SIGKILL);
                }
            }
        }
    }

    #[cfg(windows)]
    fn force(&self, _child: &Child) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job as _, 1);
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn force(&self, _child: &Child) {}
}

#[cfg(windows)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job as _);
        }
    }
}

#[cfg(windows)]
fn no_console(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console(_command: &mut Command) {}

/// Hand the child none of this process's capabilities.
///
/// A daemon under the service unit holds `CAP_NET_ADMIN` **ambiently**, so the
/// net plane can open its interface without being root — and ambient is
/// precisely the set that survives `execve`. `CapabilityBoundingSet=` bounds
/// what may ever be gained; it drops nothing. So without this, every World runner starts
/// holding the daemon's authority over the machine's network.
///
/// Cleared in the child rather than in the daemon: `netstack::tun`'s route
/// changes shell out to `ip`, which needs the capability in *its* child, and
/// clearing it process-wide around a spawn would be a race every other thread
/// could lose. `pre_exec` runs after the fork, so it reaches this child only.
#[cfg(target_os = "linux")]
#[allow(
    clippy::as_conversions,
    reason = "prctl's variadic arguments are `unsigned long` by kernel contract"
)]
fn disinherit_capabilities(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: the closure runs between fork and exec and must be
    // async-signal-safe; a bare `prctl` syscall is.
    unsafe {
        command.pre_exec(|| {
            // SAFETY: a documented prctl option taking no pointers.
            let cleared = libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            );
            if cleared < 0 {
                let error = std::io::Error::last_os_error();
                // A kernel too old for ambient capabilities has none to
                // clear — `PR_CAP_AMBIENT` and the unit option that fills it
                // arrived together, in 4.3. Any other failure refuses the
                // spawn: a child that kept the daemon's authority is worse
                // than a child that never started.
                if error.raw_os_error() != Some(libc::EINVAL) {
                    return Err(error);
                }
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn disinherit_capabilities(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    /// The list is the policy, so the list is what is asserted.
    ///
    /// A runner is launched by a daemon, a head, or `lait mcp` — and an editor
    /// spawns `lait mcp`, so before `env_clear` the runner inherited the
    /// *editor's* environment. That is the payoff in the IDE auto-execution
    /// class of vulnerability.
    ///
    /// This guards the allowlist rather than the clearing. Proving the clearing
    /// end to end needs a child that speaks the runner handshake, which is
    /// `tests/process.rs`'s job and not something a fixture shell script can
    /// fake — a test that spawned its own probe with its own `env_clear` would
    /// pass whether or not `launch` cleared anything, which is no test at all.
    #[test]
    fn nothing_a_world_could_authenticate_with_is_on_the_inherited_list() {
        for name in INHERITED {
            let upper = name.to_ascii_uppercase();
            assert!(
                !upper.contains("KEY")
                    && !upper.contains("TOKEN")
                    && !upper.contains("SECRET")
                    && !upper.contains("PASSWORD")
                    && !upper.contains("CREDENTIAL")
                    && !upper.contains("AUTH")
                    && !upper.contains("SESSION"),
                "{name} reads like a credential and must not be handed to a World"
            );
        }
        assert!(
            INHERITED.contains(&"PATH"),
            "a runner still has to find a linker"
        );
        assert!(
            !INHERITED.contains(&"SSH_AUTH_SOCK"),
            "an agent socket is the whole point of not inheriting"
        );
        assert!(
            !INHERITED.contains(&"LD_PRELOAD"),
            "nor anything that injects code"
        );
    }

    use super::*;

    #[test]
    fn release_paths_never_escape_the_bundle() {
        let root = tempfile::tempdir().expect("bundle");
        for path in ["", "../world", "/world", "bin/../../world"] {
            assert!(
                Release::under(
                    root.path(),
                    "com.example.world",
                    "1.0.0",
                    Provenance::Sealed([0; 32]),
                    path,
                    Vec::new(),
                    None::<&Path>,
                )
                .is_err(),
                "unsafe program path was admitted: {path}"
            );
        }
    }
}
