//! One Astrolabe per OS user, per identity.
//!
//! Acquired in the runner *before* the interface starts, so a second launch
//! signals the first and exits rather than racing it. Racing is not theoretical
//! here: two clients would take the same managed state root, and the registry
//! behind it is single-writer locked — the loser would fail at a point where a
//! window already exists and a person is already looking at it.
//!
//! A Win32 named mutex is the Windows shape, and it is the right one because the
//! kernel releases it when the holder dies. A lock file left behind by a crash
//! would have to be aged out or force-broken, and both of those are how a
//! single-instance guard turns into a startup failure nobody can explain.

use anyhow::Result;

/// What acquiring produced.
pub enum Outcome {
    /// This process holds it. Keep the guard alive for as long as the client
    /// runs; dropping it releases.
    Held(Guard),
    /// Somebody else holds it. The caller should hand over whatever it was
    /// asked to do and exit.
    AlreadyRunning,
}

/// The held instance. Releases on drop, and on process death whether or not
/// anything gets to drop.
pub struct Guard {
    /// The shipped guard's Windows mutex handle. `None` for a profile, which
    /// holds a lock file on every platform instead.
    #[cfg(windows)]
    _handle: Option<imp::Handle>,
    /// An advisory lock: the shipped guard's on unix, and a profile's
    /// in-directory lock on every platform.
    _lock: Option<std::fs::File>,
}

/// Take the single-instance guard for this stack.
pub fn acquire(profile: &lait::config::Profile) -> Result<Outcome> {
    match instance_of(profile) {
        Instance::Shipped => imp::acquire_named(INSTANCE),
        Instance::InProfile { root } => in_profile::acquire(&root),
    }
}

/// Which guard a stack takes.
///
/// The exclusion this guard performs is not "one Astrolabe per machine" for
/// its own sake — the header says what it protects: two clients taking the
/// same managed state root, whose registry is single-writer locked. Two
/// clients that do not share a root do not have that problem, and excluding
/// them buys nothing while costing the ability to run a development client
/// beside an installed one.
enum Instance {
    /// The default stack: the shipped, machine-and-user-wide name, unchanged.
    Shipped,
    /// A profile: a lock file inside the profile's own state root.
    InProfile { root: std::path::PathBuf },
}

fn instance_of(profile: &lait::config::Profile) -> Instance {
    match profile.state_root() {
        Some(root) => Instance::InProfile {
            root: root.to_path_buf(),
        },
        None => Instance::Shipped,
    }
}

/// The base name every artifact of the guard derives from — the Windows
/// mutex, the Unix lock file, the channel. Fixed and process-independent: the
/// whole point is for two unrelated launches by the same user to collide on
/// it. Its user-scoped spelling must not move after shipping, or clients on
/// either side of an upgrade stop excluding each other.
const INSTANCE: &str = "lait-astrolabe";

/// What a launch is, once the guard has spoken.
pub enum Claim {
    /// This process is the client: the guard is held, and the channel — when
    /// one could be bound — receives what later launches were asked to do. A
    /// channel that failed to bind costs the handoff, never the guard.
    Primary {
        guard: Guard,
        channel: Option<Channel>,
    },
    /// The running client has this launch's arguments; this process is done.
    Forwarded,
}

/// The receiving half of the instance channel: one message per later launch,
/// each the whole argv it arrived with.
pub struct Channel {
    listener: interprocess::local_socket::Listener,
}

impl Channel {
    /// Every later launch's argv, blocking. A failed accept is tolerated —
    /// the channel serves a whole tray-resident session, and one transient
    /// error must not end it — but a channel that only errs eventually does.
    pub fn messages(self) -> impl Iterator<Item = String> {
        use interprocess::local_socket::traits::Listener as _;
        use std::io::Read as _;
        let mut consecutive_failures = 0u32;
        std::iter::from_fn(move || loop {
            match self.listener.accept() {
                Ok(mut connection) => {
                    consecutive_failures = 0;
                    let mut message = String::new();
                    if connection.read_to_string(&mut message).is_ok() {
                        return Some(message);
                    }
                }
                Err(_) => {
                    consecutive_failures += 1;
                    if consecutive_failures > 100 {
                        return None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        })
    }
}

/// Claim the instance, or hand `args` to the one that already exists.
///
/// The channel is what makes `AlreadyRunning` an answer instead of a dead
/// end: a `lait:` link opens a fresh process on the stub platforms, and
/// without the handoff the link died with it.
pub fn claim(
    profile: &lait::config::Profile,
    args: impl IntoIterator<Item = String>,
) -> Result<Claim> {
    match instance_of(profile) {
        Instance::Shipped => claim_named(INSTANCE, args),
        // A profile's channel is named from its own root, so a `lait:` link
        // opened into one stack reaches that stack's client rather than
        // whichever one happens to be up.
        Instance::InProfile { root } => in_profile::claim(&root, args),
    }
}

fn claim_named(instance: &str, args: impl IntoIterator<Item = String>) -> Result<Claim> {
    match imp::acquire_named(instance)? {
        Outcome::Held(guard) => {
            let channel = match bind_channel(instance) {
                Ok(channel) => Some(channel),
                Err(error) => {
                    eprintln!("astrolabe: no instance channel this session: {error:#}");
                    None
                }
            };
            Ok(Claim::Primary { guard, channel })
        }
        Outcome::AlreadyRunning => {
            if let Err(error) = forward(instance, args) {
                // The holder exists (the guard says so) and did not answer.
                // Said rather than swallowed; there is nobody else to hand to.
                eprintln!("astrolabe: the running client could not be reached: {error:#}");
            }
            Ok(Claim::Forwarded)
        }
    }
}

fn scoped_name(instance: &str, artifact: &str, user: &str) -> String {
    format!("{instance}-{artifact}-{user}")
}

#[cfg(windows)]
fn user_scope() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(unix)]
fn user_scope() -> String {
    // SAFETY: `geteuid` has no preconditions and returns the effective uid of
    // this process. Unlike `$USER`, it cannot be changed to collide with a
    // different login's instance artifacts.
    unsafe { libc::geteuid() }.to_string()
}

/// Per user, matching the guard's scope — two people on one machine each get
/// their own channel.
fn channel_name(instance: &str) -> String {
    scoped_name(instance, "instance", &user_scope())
}

#[cfg(not(windows))]
fn lock_path(instance: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{}.lock",
        scoped_name(instance, "guard", &user_scope())
    ))
}

/// Windows names a pipe; elsewhere the channel is a socket file, unlinked
/// before binding — a crash leaves the file behind with nothing listening,
/// and the guard just won says nobody can be behind it.
#[cfg(windows)]
fn channel_address(instance: &str) -> Result<interprocess::local_socket::Name<'static>> {
    use anyhow::Context as _;
    use interprocess::local_socket::{GenericNamespaced, ToNsName as _};
    channel_name(instance)
        .to_ns_name::<GenericNamespaced>()
        .context("name the instance channel")
}

#[cfg(not(windows))]
fn channel_address(instance: &str) -> Result<interprocess::local_socket::Name<'static>> {
    use anyhow::Context as _;
    use interprocess::local_socket::{GenericFilePath, ToFsName as _};
    std::env::temp_dir()
        .join(format!("{}.sock", channel_name(instance)))
        .to_fs_name::<GenericFilePath>()
        .context("name the instance channel")
}

fn bind_channel(instance: &str) -> Result<Channel> {
    use anyhow::Context as _;
    use interprocess::local_socket::ListenerOptions;
    #[cfg(not(windows))]
    {
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("{}.sock", channel_name(instance))),
        );
    }
    let listener = ListenerOptions::new()
        .name(channel_address(instance)?)
        .create_sync()
        .context("bind the instance channel")?;
    Ok(Channel { listener })
}

fn forward(instance: &str, args: impl IntoIterator<Item = String>) -> Result<()> {
    use anyhow::Context as _;
    use interprocess::local_socket::traits::Stream as _;
    use std::io::Write as _;
    // The holder may still be between winning the guard and binding the
    // channel — a person double-clicking a link cold is exactly that race —
    // so the connect waits it out briefly rather than dropping the link.
    let mut stream = None;
    for _ in 0..40 {
        match interprocess::local_socket::Stream::connect(channel_address(instance)?) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let mut stream = stream.context("reach the running client")?;
    let blob = args.into_iter().collect::<Vec<_>>().join("\n");
    stream
        .write_all(blob.as_bytes())
        .context("hand the arguments over")?;
    Ok(())
}

/// A profile's guard: a lock file **inside the profile's own state root**.
///
/// This is the shape Chrome (`SingletonLock`) and Firefox (`parent.lock`) both
/// use, and it is the right one for the same reasons. The lock lives with the
/// thing it protects, so it moves when that directory moves and disappears
/// when it is deleted; it names its holder, so a stale one is diagnosable
/// rather than opaque; and computing its name costs nothing and creates
/// nothing.
///
/// The alternative — a machine-wide name derived by hashing the directory's
/// path — was considered and is worse in every one of those places: a renamed
/// profile derives a different name and admits a second client onto the same
/// registry, a deleted profile leaves an orphan nothing can interpret, and
/// the hash has to canonicalize a path (and create it first) just to be
/// computed.
mod in_profile {
    use super::{Channel, Claim, Guard, Outcome};
    use anyhow::{Context, Result};
    use fs2::FileExt as _;
    use std::path::Path;

    /// Named for what it holds, beside the state it protects.
    const LOCK: &str = "instance.lock";
    const CHANNEL: &str = "instance.sock";

    pub fn acquire(root: &Path) -> Result<Outcome> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("create the profile state root {}", root.display()))?;
        let path = root.join(LOCK);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                // Who holds it, so a stale lock can be reasoned about instead
                // of merely being in the way. Best-effort: the lock is the
                // guard, this is only its explanation.
                let _ = std::fs::write(
                    root.join("instance.holder"),
                    format!(
                        "{}:{}",
                        hostname().unwrap_or_else(|| "unknown-host".into()),
                        std::process::id()
                    ),
                );
                Ok(Outcome::Held(Guard {
                    #[cfg(windows)]
                    _handle: None,
                    _lock: Some(file),
                }))
            }
            Err(_) => Ok(Outcome::AlreadyRunning),
        }
    }

    pub fn claim(root: &Path, args: impl IntoIterator<Item = String>) -> Result<Claim> {
        match acquire(root)? {
            Outcome::Held(guard) => {
                let channel = match bind(root) {
                    Ok(channel) => Some(channel),
                    Err(error) => {
                        eprintln!("astrolabe: no instance channel this session: {error:#}");
                        None
                    }
                };
                Ok(Claim::Primary { guard, channel })
            }
            Outcome::AlreadyRunning => {
                if let Err(error) = forward(root, args) {
                    eprintln!("astrolabe: the running client could not be reached: {error:#}");
                }
                Ok(Claim::Forwarded)
            }
        }
    }

    #[cfg(windows)]
    fn address(root: &Path) -> Result<interprocess::local_socket::Name<'static>> {
        use interprocess::local_socket::{GenericNamespaced, ToNsName as _};
        // A Windows pipe is a namespace entry, not a file, so it cannot live
        // in the directory. Named from the profile's own name, which is a
        // validated identifier and therefore already a legal pipe name.
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("profile");
        format!("lait-astrolabe-{name}-instance")
            .to_ns_name::<GenericNamespaced>()
            .context("name the instance channel")
    }

    #[cfg(not(windows))]
    fn address(root: &Path) -> Result<interprocess::local_socket::Name<'static>> {
        use interprocess::local_socket::{GenericFilePath, ToFsName as _};
        root.join(CHANNEL)
            .to_fs_name::<GenericFilePath>()
            .context("name the instance channel")
    }

    fn bind(root: &Path) -> Result<Channel> {
        use interprocess::local_socket::ListenerOptions;
        #[cfg(not(windows))]
        let _ = std::fs::remove_file(root.join(CHANNEL));
        let listener = ListenerOptions::new()
            .name(address(root)?)
            .create_sync()
            .context("bind the instance channel")?;
        Ok(Channel { listener })
    }

    fn forward(root: &Path, args: impl IntoIterator<Item = String>) -> Result<()> {
        use interprocess::local_socket::traits::Stream as _;
        use interprocess::local_socket::Stream;
        use std::io::Write as _;
        let mut stream = Stream::connect(address(root)?).context("reach the running client")?;
        let blob = args.into_iter().collect::<Vec<_>>().join("\n");
        stream
            .write_all(blob.as_bytes())
            .context("hand the arguments over")?;
        Ok(())
    }

    fn hostname() -> Option<String> {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
    }
}

#[cfg(windows)]
mod imp {
    use super::{Guard, Outcome};
    use anyhow::Result;

    /// Held, never read. The handle *is* the guard: the mutex is released when
    /// the last handle to it closes, so keeping this alive is the whole
    /// mechanism and reading it would have nothing to say.
    pub struct Handle(
        #[allow(dead_code, reason = "held to keep the mutex, never read")]
        std::os::windows::io::OwnedHandle,
    );

    // SAFETY: a mutex HANDLE is not thread-affine; it is closed once, on drop.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    pub fn acquire_named(instance: &str) -> Result<Outcome> {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        // `Local` scopes it to the logon session; the suffix is historic
        // and must never move.
        let mutex = format!(r"Local\{instance}-single-instance");
        let name: Vec<u16> = mutex.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `name` is a valid null-terminated wide string that outlives
        // the call. A null handle is returned as failure and never used.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error())
                .map_err(|error| anyhow::anyhow!("take the single-instance mutex: {error}"));
        }
        // SAFETY: `CreateMutexW` succeeded, so this handle is ours to own. Owned
        // even in the already-exists case, because the call still returned a
        // handle and leaking it would keep the mutex referenced forever.
        let owned =
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle as RawHandle) };
        // SAFETY: reading the thread's last error immediately after the call
        // that set it.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return Ok(Outcome::AlreadyRunning);
        }
        Ok(Outcome::Held(Guard {
            _handle: Some(Handle(owned)),
            _lock: None,
        }))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Guard, Outcome};
    use anyhow::{Context, Result};
    use fs2::FileExt;

    /// An advisory lock on a file under the temp directory. Same property that
    /// matters: the OS releases it when the process dies, so a crash cannot
    /// leave a stale guard.
    pub fn acquire_named(instance: &str) -> Result<Outcome> {
        let path = super::lock_path(instance);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Outcome::Held(Guard { _lock: Some(file) })),
            Err(_) => Ok(Outcome::AlreadyRunning),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch profile, so a live client on the machine running this suite
    /// is neither reached nor blocked.
    fn scratch_profile(tag: &str) -> (lait::config::Profile, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a scratch profile root");
        let name: lait::config::ProfileName = format!("t{tag}").parse().expect("a profile name");
        (
            lait::config::Profile::Named {
                name,
                config: dir.path().join("config"),
                state: dir.path().join("state"),
            },
            dir,
        )
    }

    /// The property the whole module exists for, and the one that is easy to
    /// get backwards: the *first* holder keeps it, and the second is told so
    /// rather than being handed a second guard.
    ///
    /// Run against a scratch profile rather than the shipped guard, because
    /// the shipped guard is machine-and-user-wide: asserting on it failed
    /// whenever the person running the suite had their own client open, which
    /// reads as the guard being broken and is the guard working.
    #[test]
    fn a_second_acquire_is_told_somebody_else_holds_it() {
        let (profile, _dir) = scratch_profile(&format!("a{}", std::process::id()));
        let first = acquire(&profile).expect("first acquire");
        assert!(
            matches!(first, Outcome::Held(_)),
            "the first launch was refused"
        );

        let second = acquire(&profile).expect("second acquire");
        assert!(
            matches!(second, Outcome::AlreadyRunning),
            "two launches both believed they were the only one"
        );

        // And releasing lets the next launch in — a crashed client must not
        // lock the machine out of its own application.
        drop(first);
        let third = acquire(&profile).expect("third acquire");
        assert!(
            matches!(third, Outcome::Held(_)),
            "the guard was not released, so no later launch can ever start"
        );
    }

    /// The whole point of the feature: two stacks that share no root do not
    /// exclude each other.
    ///
    /// The guard protects a shared managed state root, so two clients that do
    /// not share one have nothing to collide over — and excluding them cost
    /// the ability to run a development client beside an installed one, which
    /// is exactly what this exists to restore.
    #[test]
    fn two_profiles_do_not_exclude_each_other() {
        let (one, _one_dir) = scratch_profile(&format!("b{}", std::process::id()));
        let (two, _two_dir) = scratch_profile(&format!("c{}", std::process::id()));

        let first = acquire(&one).expect("the first stack's guard");
        let second = acquire(&two).expect("the second stack's guard");

        assert!(
            matches!(first, Outcome::Held(_)) && matches!(second, Outcome::Held(_)),
            "two stacks with separate roots excluded each other, which is the defect that kept \
             a development client from running beside an installed one"
        );
    }

    /// A profile's lock lives inside the profile, the way Chrome's
    /// `SingletonLock` and Firefox's `parent.lock` do — so it moves when the
    /// directory moves, vanishes when it is deleted, and names its holder.
    #[test]
    fn a_profiles_guard_lives_in_its_own_root_and_names_its_holder() {
        let (profile, _dir) = scratch_profile(&format!("d{}", std::process::id()));
        let root = profile
            .state_root()
            .expect("a named profile has a state root")
            .to_path_buf();
        let _held = acquire(&profile).expect("the guard");

        assert!(
            root.join("instance.lock").is_file(),
            "the lock is not inside the directory it protects"
        );
        let holder = std::fs::read_to_string(root.join("instance.holder"))
            .expect("the holder is named beside the lock");
        assert!(
            holder.ends_with(&format!(":{}", std::process::id())),
            "the lock does not name its holder, so a stale one cannot be reasoned about: {holder}"
        );
    }

    /// The default stack keeps the exact name it shipped with. If this moves,
    /// clients on either side of an upgrade stop excluding each other.
    #[test]
    fn the_default_guard_spelling_has_not_moved() {
        assert_eq!(INSTANCE, "lait-astrolabe");
        assert!(
            matches!(
                instance_of(&lait::config::Profile::Default),
                Instance::Shipped
            ),
            "the default stack stopped using the shipped guard"
        );
    }

    /// `AlreadyRunning` is an answer, not a dead end: the second launch's
    /// argv reaches the first through the channel — which is how a `lait:`
    /// link opens in the running client instead of dying with the process
    /// the OS started to deliver it. Scratch names, so a live client on the
    /// machine running this suite is neither reached nor blocked.
    #[test]
    fn a_second_launch_hands_its_arguments_to_the_first() {
        let instance = format!("lait-astrolabe-test-{}", std::process::id());

        let Claim::Primary {
            guard: _guard,
            channel: receiver,
        } = claim_named(&instance, Vec::new()).expect("first claim")
        else {
            panic!("the first launch was not the primary");
        };
        let receiver = receiver.expect("the primary bound its channel");
        let received = std::thread::spawn(move || receiver.messages().next());

        let second = claim_named(
            &instance,
            vec![
                "astrolabe.exe".to_string(),
                "lait://world/issues".to_string(),
            ],
        )
        .expect("second claim");
        assert!(
            matches!(second, Claim::Forwarded),
            "two launches both believed they were the only one"
        );

        let message = received
            .join()
            .expect("the drain thread")
            .expect("a message arrived");
        assert_eq!(message, "astrolabe.exe\nlait://world/issues");
    }

    #[test]
    fn instance_artifacts_are_stable_within_a_user_and_distinct_between_users() {
        let alice = scoped_name(INSTANCE, "guard", "1000");
        assert_eq!(alice, scoped_name(INSTANCE, "guard", "1000"));
        assert_ne!(alice, scoped_name(INSTANCE, "guard", "1001"));
        assert_ne!(
            scoped_name(INSTANCE, "instance", "1000"),
            scoped_name(INSTANCE, "instance", "1001")
        );
    }
}
