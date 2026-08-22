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
    imp::acquire_named(MUTEX_NAME)
}

/// The name is fixed and process-independent — it has to be, since the whole
/// point is for two unrelated launches to collide on it. The `Local\` prefix
/// scopes it to the logon session, so two people signed in to the same machine
/// each get their own client rather than one locking the other out.
const MUTEX_NAME: &str = r"Local\lait-astrolabe-single-instance";

/// What a launch is, once the guard has spoken.
pub enum Claim {
    /// This process is the client: the guard is held and the channel below
    /// receives what later launches were asked to do.
    Primary { guard: Guard, channel: Channel },
    /// The running client has this launch's arguments; this process is done.
    Forwarded,
}

/// The receiving half of the instance channel: one message per later launch,
/// each the whole argv it arrived with.
pub struct Channel {
    listener: interprocess::local_socket::Listener,
}

impl Channel {
    /// Every later launch's argv, blocking. Ends with the process, or with a
    /// channel that can no longer accept.
    pub fn messages(self) -> impl Iterator<Item = String> {
        use interprocess::local_socket::traits::Listener as _;
        use std::io::Read as _;
        std::iter::from_fn(move || loop {
            let mut connection = self.listener.accept().ok()?;
            let mut message = String::new();
            if connection.read_to_string(&mut message).is_ok() {
                return Some(message);
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
    claim_named(MUTEX_NAME, &channel_name(), args)
}

fn claim_named(
    mutex: &str,
    channel: &str,
    args: impl IntoIterator<Item = String>,
) -> Result<Claim> {
    match imp::acquire_named(mutex)? {
        Outcome::Held(guard) => Ok(Claim::Primary {
            guard,
            channel: bind_channel(channel)?,
        }),
        Outcome::AlreadyRunning => {
            if let Err(error) = forward(channel, args) {
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
fn channel_name() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".into());
    format!("lait-astrolabe-instance-{user}")
}

fn bind_channel(name: &str) -> Result<Channel> {
    use anyhow::Context as _;
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _};
    let ns_name = name
        .to_ns_name::<GenericNamespaced>()
        .context("name the instance channel")?;
    let listener = ListenerOptions::new()
        .name(ns_name)
        .create_sync()
        .context("bind the instance channel")?;
    Ok(Channel { listener })
}

fn forward(name: &str, args: impl IntoIterator<Item = String>) -> Result<()> {
    use anyhow::Context as _;
    use interprocess::local_socket::{traits::Stream as _, GenericNamespaced, ToNsName as _};
    use std::io::Write as _;
    let ns_name = name
        .to_ns_name::<GenericNamespaced>()
        .context("name the instance channel")?;
    let mut stream =
        interprocess::local_socket::Stream::connect(ns_name).context("reach the running client")?;
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

    pub fn acquire_named(mutex: &str) -> Result<Outcome> {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;

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
    pub fn acquire_named(mutex: &str) -> Result<Outcome> {
        let file_name: String = mutex
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect();
        let path = std::env::temp_dir().join(format!("{file_name}.lock"));
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
        let mutex = format!(r"Local\lait-astrolabe-test-{}", std::process::id());
        let channel = format!("lait-astrolabe-test-{}", std::process::id());

        let Claim::Primary {
            guard: _guard,
            channel: receiver,
        } = claim_named(&mutex, &channel, Vec::new()).expect("first claim")
        else {
            panic!("the first launch was not the primary");
        };
        let received = std::thread::spawn(move || receiver.messages().next());

        let second = claim_named(
            &mutex,
            &channel,
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
