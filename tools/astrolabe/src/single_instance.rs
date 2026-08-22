//! One Astrolabe per machine, per identity.
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
    #[cfg(windows)]
    _handle: imp::Handle,
    #[cfg(not(windows))]
    _lock: std::fs::File,
}

/// Take the single-instance guard for this machine.
pub fn acquire() -> Result<Outcome> {
    imp::acquire_named(INSTANCE)
}

/// The base name every artifact of the guard derives from — the Windows
/// mutex, the Unix lock file, the channel. Fixed and process-independent: the
/// whole point is for two unrelated launches to collide on it, and the Unix
/// lock file's spelling must never move, or a client from before the rename
/// and one from after stop excluding each other exactly across an upgrade.
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
pub fn claim(args: impl IntoIterator<Item = String>) -> Result<Claim> {
    claim_named(INSTANCE, args)
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

/// Per user, matching the mutex's per-session scope — two people on one
/// machine each get their own channel.
fn channel_name(instance: &str) -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".into());
    format!("{instance}-instance-{user}")
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
            _handle: Handle(owned),
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
        // The historic spelling: a client from before this name was derived
        // and one from after must still exclude each other.
        let path = std::env::temp_dir().join(format!("{instance}.lock"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Outcome::Held(Guard { _lock: file })),
            Err(_) => Ok(Outcome::AlreadyRunning),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole module exists for, and the one that is easy to
    /// get backwards: the *first* holder keeps it, and the second is told so
    /// rather than being handed a second guard.
    #[test]
    fn a_second_acquire_is_told_somebody_else_holds_it() {
        let first = acquire().expect("first acquire");
        assert!(
            matches!(first, Outcome::Held(_)),
            "the first launch was refused"
        );

        let second = acquire().expect("second acquire");
        assert!(
            matches!(second, Outcome::AlreadyRunning),
            "two launches both believed they were the only one"
        );

        // And releasing lets the next launch in — a crashed client must not
        // lock the machine out of its own application.
        drop(first);
        let third = acquire().expect("third acquire");
        assert!(
            matches!(third, Outcome::Held(_)),
            "the guard was not released, so no later launch can ever start"
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
}
