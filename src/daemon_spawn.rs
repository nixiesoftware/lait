#![allow(
    clippy::as_conversions,
    reason = "OS process APIs require lossless platform-handle and flag conversions at this private adapter boundary."
)]

//! Spawning the daemon without handing it anything else of ours.
//!
//! The daemon outlives the command that spawns it, so every handle it comes up
//! holding, it holds *for its whole life*. On unix that is already true by
//! construction: fds are `CLOSE_ON_EXEC`, so the child starts with nothing but
//! the three `Stdio` slots we named. Windows has no such default —
//! `CreateProcess` takes a single `bInheritHandles` switch, and `TRUE` means
//! *every* inheritable handle in this process, not just the ones in
//! `STARTUPINFO`. A daemon spawned from a head whose stdout somebody captured
//! therefore came up owning a write-end of that pipe, and whoever was reading it
//! waited forever on an EOF that could not arrive (see
//! `process::disinherit_stdio`, which covers our *own* stdio — this module
//! covers everything else, including the handles we inherited from our parent
//! and never knew about).
//!
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` is the only way to say "inherit exactly
//! these" — so the Windows path drives `CreateProcessW` itself. std exposes the
//! same attribute only under an unstable feature (rust#114854) and this crate is
//! pinned to stable, so there is no std route to it today. If that stabilises,
//! this module collapses back into a `Command` builder.

use std::io;
use std::path::Path;
use std::process::ExitStatus;

/// A spawned daemon. Only what `ensure_daemon` needs: is it still alive?
pub struct DaemonChild {
    pid: u32,
    #[cfg(windows)]
    proc: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    child: std::process::Child,
}

/// Spawn the identity-scoped `<exe> daemon`, with `log` (when present) as its
/// stderr and `NUL`/`/dev/null` for the rest.
///
/// `log` is the daemon's own diagnosis when a spawn fails ("another lait daemon
/// is already running…"), which is the whole error message on that path — so it
/// is a real file, not a null sink.
///
/// `identity` pins which `secret.key` it runs on, passed as `--home` rather than
/// an env var. `None` selects the ordinary per-user identity; `Some` is the
/// self-contained `$LAIT_HOME` case. Orbit selection is deliberately absent:
/// the general daemon's [`crate::orbits::Router`] places many Orbits.
pub fn spawn(
    exe: &Path,
    log: Option<std::fs::File>,
    identity: Option<&Path>,
) -> io::Result<DaemonChild> {
    imp::spawn(exe, log, identity)
}

impl DaemonChild {
    /// The process id returned by the spawn operation.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// `Some(status)` once the daemon has exited, `None` while it is running.
    ///
    /// A daemon that has already exited is never going to answer, so the spawn
    /// wait polls this to fail fast with its own words instead of blaming a 20s
    /// timeout.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        imp::try_wait(self)
    }

    /// Force this exact spawned child to exit and collect its status.
    ///
    /// This deliberately lives on the owned child handle rather than accepting
    /// an arbitrary pid. Callers cannot race pid reuse and terminate a process
    /// they did not spawn.
    pub fn force_kill_and_wait(&mut self) -> io::Result<ExitStatus> {
        imp::force_kill_and_wait(self)
    }

    /// Give up the handle to a reaper, so the daemon's *exit* is collected
    /// whenever it comes.
    ///
    /// The daemon outlives the spawn call, so nothing here waits for it — and
    /// on unix "nothing waits for it" is the whole bug. A child whose parent
    /// never `wait`s becomes a zombie the instant it exits: still listed by
    /// `ps`, still answering `kill -0`, immune to `kill -9` (there is nothing
    /// left to signal), and cleared only when the parent itself dies. A head
    /// that outlives the daemon it started therefore turns every ordinary
    /// daemon shutdown into a process that looks unkillable — which is exactly
    /// how this surfaced, as "SIGTERM does nothing, and neither does SIGKILL".
    ///
    /// Reaping is not stopping: this only collects the exit status, whenever it
    /// arrives. A daemon told to keep running keeps running, and one that
    /// outlives *this* process is reparented and reaped by init as usual.
    pub fn reap(self) {
        imp::reap(self);
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::process::{Command, Stdio};

    pub fn spawn(
        exe: &Path,
        log: Option<std::fs::File>,
        identity: Option<&Path>,
    ) -> io::Result<DaemonChild> {
        let stderr = match log {
            Some(f) => Stdio::from(f),
            None => Stdio::null(),
        };
        let mut cmd = Command::new(exe);
        cmd.arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        // `--home` is a global flag the child turns into its own `LAIT_HOME`,
        // selecting a self-contained daemon identity.
        if let Some(identity) = identity {
            cmd.arg("--home").arg(identity);
        }
        let child = cmd.spawn()?;
        let pid = child.id();
        Ok(DaemonChild { pid, child })
    }

    pub fn try_wait(c: &mut DaemonChild) -> io::Result<Option<ExitStatus>> {
        c.child.try_wait()
    }

    pub fn force_kill_and_wait(c: &mut DaemonChild) -> io::Result<ExitStatus> {
        if let Some(status) = c.child.try_wait()? {
            return Ok(status);
        }
        c.child.kill()?;
        c.child.wait()
    }

    /// A plain OS thread, not a runtime task: `wait` is a blocking syscall, and
    /// the reaper has to outlive whatever runtime happened to be up when the
    /// daemon was spawned. It parks in the kernel until the daemon exits, which
    /// for a healthy daemon is the rest of this process's life.
    pub fn reap(c: DaemonChild) {
        let mut child = c.child;
        // A reaper we could not start is not worth failing a spawn over: the
        // daemon is up, and the cost is the zombie we had before.
        let _ = std::thread::Builder::new()
            .name("lait-daemon-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::{c_void, OsStr};
    use std::fs::{File, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::os::windows::process::ExitStatusExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
        InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject, CREATE_NO_WINDOW, EXTENDED_STARTUPINFO_PRESENT,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        STARTF_USESTDHANDLES, STARTUPINFOEXW,
    };

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// A handle named in the inherit list must itself be inheritable — the list
    /// narrows what crosses, it does not mark anything.
    fn make_inheritable(f: &File) -> io::Result<HANDLE> {
        let h = f.as_raw_handle() as HANDLE;
        // SAFETY: `h` is the live handle of a `File` we own and hold borrowed.
        if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(h)
    }

    fn nul(write: bool) -> io::Result<File> {
        if write {
            OpenOptions::new().write(true).open("NUL")
        } else {
            OpenOptions::new().read(true).open("NUL")
        }
    }

    /// Frees the attribute list on every exit path, including the error ones.
    struct AttrList(LPPROC_THREAD_ATTRIBUTE_LIST);
    impl Drop for AttrList {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a list we successfully initialised, freed once.
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }

    pub fn spawn(
        exe: &Path,
        log: Option<std::fs::File>,
        identity: Option<&Path>,
    ) -> io::Result<DaemonChild> {
        // Held to the end of the call: these must outlive `CreateProcessW`, which
        // duplicates them into the child. Our copies close on drop.
        let stdin = nul(false)?;
        let stdout = nul(true)?;
        let stderr = match log {
            Some(f) => f,
            None => nul(true)?,
        };
        let handles: [HANDLE; 3] = [
            make_inheritable(&stdin)?,
            make_inheritable(&stdout)?,
            make_inheritable(&stderr)?,
        ];

        // Sized by the API, then allocated as `usize` words: an attribute list is
        // pointer-aligned, which a `Vec<u8>` would not guarantee.
        let mut size = 0usize;
        // SAFETY: the sizing call. It always "fails" (ERROR_INSUFFICIENT_BUFFER)
        // and writes the required size, which is the only reason we call it.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
        let words = size.div_ceil(std::mem::size_of::<usize>()).max(1);
        let mut buf: Vec<usize> = vec![0; words];
        let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `list` points to `size` bytes of pointer-aligned storage.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let list = AttrList(list);

        // The whole point: the child inherits these three handles and nothing
        // else, whatever else of ours happens to be inheritable.
        // SAFETY: `handles` outlives the `CreateProcessW` call below, as the API
        // requires (the list borrows it rather than copying).
        if unsafe {
            UpdateProcThreadAttribute(
                list.0,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr() as *const c_void,
                std::mem::size_of_val(&handles),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }

        let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = handles[0];
        si.StartupInfo.hStdOutput = handles[1];
        si.StartupInfo.hStdError = handles[2];
        si.lpAttributeList = list.0;

        let app = wide(exe.as_os_str());
        // `CreateProcessW` may write to the command line buffer, so it is ours and
        // mutable. Quoted because a path with a space would otherwise split; a
        // Windows path cannot contain `"`, so there is nothing else to escape.
        // `--home` rather than an env override: the block below is inherited
        // wholesale, so an env pin would have to be process-wide and would race
        // any other spawn in flight. An argv is the child's alone.
        let mut cmdline = wide(OsStr::new(&match identity {
            Some(id) => format!("\"{}\" daemon --home \"{}\"", exe.display(), id.display()),
            None => format!("\"{}\" daemon", exe.display()),
        }));

        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer is valid for the call. `bInheritHandles` must be
        // TRUE for the attribute list to be consulted at all — it is what the
        // list narrows.
        //
        // `CREATE_NO_WINDOW` because the daemon is a console-subsystem image
        // with nowhere to write: its three handles are `NUL` and a log file, so
        // a console is never read and never typed into. Without the flag
        // Windows gives a console child one anyway — inherited from the spawner
        // when there is one, and *freshly allocated* when there is not. The
        // second case is the visible one: a GUI parent (Astrolabe) starting a
        // daemon flashes a black window on screen for as long as the process
        // lives. The first is quieter and worse — sharing the spawner's console
        // puts the daemon in that console's process group, so a Ctrl-C or a
        // closed terminal delivers a control event to a process whose whole
        // contract is to outlive the command that started it.
        let ok = unsafe {
            CreateProcessW(
                app.as_ptr(),
                cmdline.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                ptr::null(),
                ptr::null(),
                &si.StartupInfo,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful CreateProcessW hands us both handles. We never use
        // the thread; the process handle becomes ours to own.
        unsafe { CloseHandle(pi.hThread) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as RawHandle) };
        Ok(DaemonChild {
            pid: pi.dwProcessId,
            proc,
        })
    }

    /// Nothing to reap: Windows has no zombie, and the process object goes when
    /// the last handle to it closes — which is this drop.
    pub fn reap(_c: DaemonChild) {}

    pub fn try_wait(c: &mut DaemonChild) -> io::Result<Option<ExitStatus>> {
        let h = c.proc.as_raw_handle() as HANDLE;
        // A zero timeout is the poll. Asking the process object rather than
        // reading the exit code first: 259 is both STILL_ACTIVE and a legal exit
        // code, so the code alone cannot tell them apart.
        // SAFETY: `h` is the live process handle we own.
        match unsafe { WaitForSingleObject(h, 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: as above; the process has exited, so the code is final.
                if unsafe { GetExitCodeProcess(h, &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(code)))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub fn force_kill_and_wait(c: &mut DaemonChild) -> io::Result<ExitStatus> {
        if let Some(status) = try_wait(c)? {
            return Ok(status);
        }
        let h = c.proc.as_raw_handle() as HANDLE;
        // SAFETY: `h` is the live process handle owned by `c`.
        if unsafe { TerminateProcess(h, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // A terminated process must become signalled; waiting here also makes
        // the returned exit code stable before the handle is reused.
        if unsafe { WaitForSingleObject(h, u32::MAX) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        try_wait(c)?.ok_or_else(|| io::Error::other("terminated process is still running"))
    }
}
