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
    imp::acquire()
}

/// The name is fixed and process-independent — it has to be, since the whole
/// point is for two unrelated launches to collide on it. The `Local\` prefix
/// scopes it to the logon session, so two people signed in to the same machine
/// each get their own client rather than one locking the other out.
const MUTEX_NAME: &str = r"Local\lait-astrolabe-single-instance";

#[cfg(windows)]
mod imp {
    use super::{Guard, Outcome, MUTEX_NAME};
    use anyhow::Result;

    pub struct Handle(std::os::windows::io::OwnedHandle);

    // SAFETY: a mutex HANDLE is not thread-affine; it is closed once, on drop.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    pub fn acquire() -> Result<Outcome> {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        let name: Vec<u16> = MUTEX_NAME.encode_utf16().chain(Some(0)).collect();
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
    pub fn acquire() -> Result<Outcome> {
        let path = std::env::temp_dir().join("lait-astrolabe.lock");
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
}
