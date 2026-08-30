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

/// What can be proven about a process this run did not spawn.
///
/// A pid on its own proves nothing: pids are reused, and the process answering
/// to one now may have started after the record that named it. The executable
/// and the start time together make the identity checkable — a supervisor can
/// insist that the process it is about to stop is the same one it recorded,
/// rather than whatever inherited the number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// The image the process is running, as the OS reports it — not as anything
    /// recorded earlier claims.
    pub executable: std::path::PathBuf,
    /// Milliseconds since the Unix epoch. The half of the identity that pid
    /// reuse cannot forge.
    pub started_at_ms: u64,
}

/// Ask the operating system what the process at `pid` actually is.
///
/// `ErrorKind::Unsupported` on a platform with no implementation, and that is a
/// deliberate shape rather than a gap: every caller treats an unprovable
/// identity as a refusal, so a platform that cannot answer never authorises
/// stopping anything. Windows is the first-class target and the one that
/// answers.
pub fn identify(pid: u32) -> io::Result<ProcessIdentity> {
    imp::identify(pid)
}

/// Terminate a process this run did not spawn, but only if it is still exactly
/// the process described by `expected`.
///
/// The check happens *inside* this call, against the same handle that does the
/// terminating. That is the whole point: a caller that identified a process,
/// decided it matched, and then asked to kill the pid would leave a window in
/// which the process could exit and the number be reused — and the kill would
/// land on a stranger. A handle names a process object rather than a number, so
/// verifying and terminating through one handle cannot be raced.
///
/// [`io::ErrorKind::PermissionDenied`] when the process no longer matches, which
/// callers surface as a refusal rather than a failure: nothing is wrong, the
/// evidence simply is not there.
pub fn terminate_verified(expected: &ProcessIdentity) -> io::Result<()> {
    imp::terminate_verified(expected)
}

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
    profile: Option<&str>,
) -> io::Result<DaemonChild> {
    imp::spawn(exe, log, identity, profile)
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

    /// Hand the child none of this process's capabilities.
    ///
    /// A daemon under the service unit holds `CAP_NET_ADMIN` **ambiently**, so the
    /// net plane can open its interface without being root — and ambient is
    /// precisely the set that survives `execve`. `CapabilityBoundingSet=` bounds
    /// what may ever be gained; it drops nothing. So without this, a daemon
    /// spawned by a head starts holding the spawner's authority over the
    /// machine's network — and the supervised daemon is started by systemd, which
    /// grants it its capability there rather than through this path.
    ///
    /// Cleared in the child rather than in the spawner: `netstack::tun`'s route
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
                    // spawn: a child that kept the spawner's authority is worse
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

    pub fn spawn(
        exe: &Path,
        log: Option<std::fs::File>,
        identity: Option<&Path>,
        profile: Option<&str>,
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
        // Its own process group, which is what the windows branch below already
        // achieves with `CREATE_NO_WINDOW` and explains at length: sharing the
        // spawner's group means "a Ctrl-C or a closed terminal delivers a control
        // event to a process whose whole contract is to outlive the command that
        // started it". This branch did the thing that comment warns against.
        //
        // It became urgent rather than untidy when heads gained their own process
        // groups and a graceful stop that signals one. A head runs
        // `host_client::ensure_lait_daemon`, so a head can be the process that
        // spawns the machine's daemon — after a staged upgrade replaces the binary,
        // or any time the daemon died mid-session. With the daemon inheriting the
        // head's group, "stop this World" delivered SIGTERM to the identity daemon
        // as well: a node-wide off switch reachable from a per-World control, and
        // from *close and stay online*, with nothing in either path able to see it.
        //
        // A new group rather than `setsid`: it is a safe stable API with no
        // `pre_exec`, and it closes this completely, since group-directed signals
        // and a terminal's foreground-group signals both stop reaching. A new
        // *session* would additionally drop the controlling terminal, which is
        // worth having and is a separate change — this one is the bug.
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        disinherit_capabilities(&mut cmd);
        // `--home` is a global flag the child turns into its own `LAIT_HOME`,
        // selecting a self-contained daemon identity.
        if let Some(identity) = identity {
            cmd.arg("--home").arg(identity);
        }
        // The stack this daemon serves, for the same reason `--home` is an
        // argv and not an env pin: the environment block is inherited
        // wholesale, so an ambient selector would let a daemon and the client
        // that started it disagree about which identity they are on. An argv
        // is the child's alone.
        if let Some(profile) = profile {
            cmd.arg("--profile").arg(profile);
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

    /// Unimplemented off Windows, which callers read as "cannot be proven" and
    /// therefore as a refusal. v1 targets Windows; adding a `/proc` reader here
    /// would be the whole change, and until something needs it, an honest
    /// `Unsupported` beats a second code path nobody exercises.
    pub fn identify(_pid: u32) -> io::Result<ProcessIdentity> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process identity is not implemented on this platform",
        ))
    }

    /// Unprovable identity is a refusal, so this never terminates anything.
    pub fn terminate_verified(_expected: &ProcessIdentity) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process identity is not implemented on this platform, so no unowned process may be stopped",
        ))
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
    use std::ffi::{c_void, OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::os::windows::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::ptr;
    use windows_sys::Win32::Foundation::{
        CloseHandle, SetHandleInformation, FILETIME, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, GetProcessTimes,
        InitializeProcThreadAttributeList, OpenProcess, QueryFullProcessImageNameW,
        TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW,
        EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
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
        profile: Option<&str>,
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
        let mut line = format!("\"{}\" daemon", exe.display());
        if let Some(id) = identity {
            line.push_str(&format!(" --home \"{}\"", id.display()));
        }
        // A profile name is `[a-z0-9-]+` by construction (`ProfileName`), so
        // there is nothing here to quote or escape — the validation at the
        // type is what makes this safe to interpolate.
        if let Some(profile) = profile {
            line.push_str(&format!(" --profile {profile}"));
        }
        let mut cmdline = wide(OsStr::new(&line));

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

    /// `PROCESS_QUERY_LIMITED_INFORMATION` is the least this can ask for and
    /// still get both answers, and unlike `PROCESS_QUERY_INFORMATION` it is
    /// granted for processes at a higher integrity level — so a daemon started
    /// from an elevated shell can still be *identified* here, and refused for
    /// the right reason rather than for a missing handle.
    pub fn identify(pid: u32) -> io::Result<ProcessIdentity> {
        // SAFETY: a null handle is returned as failure and never used.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Owned so every path below closes it, including the error ones.
        // SAFETY: `OpenProcess` returned a live handle we now own.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        describe(handle.as_raw_handle() as HANDLE, pid)
    }

    /// Read image and start time from a handle that already carries
    /// `PROCESS_QUERY_LIMITED_INFORMATION`.
    fn describe(raw: HANDLE, pid: u32) -> io::Result<ProcessIdentity> {
        let mut buffer = [0u16; 32_768];
        let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: `buffer` is valid for `length` u16s and `length` is updated
        // to the count actually written.
        if unsafe { QueryFullProcessImageNameW(raw, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let executable = PathBuf::from(OsString::from_wide(
            buffer.get(..length as usize).unwrap_or(&[]),
        ));

        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: four out-parameters we own, and a live process handle.
        if unsafe { GetProcessTimes(raw, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(ProcessIdentity {
            pid,
            executable,
            started_at_ms: filetime_to_unix_ms(created),
        })
    }

    pub fn terminate_verified(expected: &ProcessIdentity) -> io::Result<()> {
        // TERMINATE alone cannot read the image name or the times, so the
        // handle carries both rights and the verification runs on it.
        // SAFETY: a null handle is returned as failure and never used.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                expected.pid,
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `OpenProcess` returned a live handle we now own.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        let actual = describe(handle.as_raw_handle() as HANDLE, expected.pid)?;
        if &actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "process {} is not the one recorded ({} started {}), it is {} started {}",
                    expected.pid,
                    expected.executable.display(),
                    expected.started_at_ms,
                    actual.executable.display(),
                    actual.started_at_ms
                ),
            ));
        }
        // SAFETY: the handle we verified through, and the same process object.
        if unsafe { TerminateProcess(handle.as_raw_handle() as HANDLE, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// A `FILETIME` counts 100-nanosecond ticks from 1601-01-01; Unix counts
    /// milliseconds from 1970-01-01. The constant is the gap between those two
    /// epochs in ticks.
    fn filetime_to_unix_ms(time: FILETIME) -> u64 {
        const EPOCH_DELTA_TICKS: u64 = 116_444_736_000_000_000;
        let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        ticks
            .saturating_sub(EPOCH_DELTA_TICKS)
            .saturating_div(10_000)
    }

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

#[cfg(all(test, unix))]
mod group_isolation {
    //! A spawned daemon must not be in its spawner's process group.
    //!
    //! Its whole contract is to outlive whoever started it, and the windows branch
    //! has said so in prose since it was written. What made this worth a test is
    //! that heads gained their own process groups and a graceful stop that signals
    //! one: a head runs `ensure_lait_daemon`, so a head can be the daemon's parent,
    //! and a daemon sharing the head's group turned "stop this World" into a
    //! node-wide off switch. No test in the supervisor could see it — the two
    //! halves are in different crates.

    /// The daemon's process group is its own, not this process's.
    ///
    /// Driven through the real `imp::spawn`, not through a `Command` this test
    /// builds. A first draft asserted the arrangement on its own command and would
    /// have passed with the fix deleted from production — proving that
    /// `process_group(0)` does what the manual says, which nobody doubted.
    ///
    /// The payload is a temporary script because `spawn` always appends `daemon` as
    /// an argument: a real long-running child is needed for `getpgid` to have
    /// something to answer about, and the script ignores its arguments and sleeps.
    #[test]
    fn a_spawned_daemon_leads_its_own_process_group() {
        let dir = tempfile::tempdir().expect("a tempdir");
        let script = dir.path().join("fake-daemon");
        std::fs::write(&script, "#!/bin/sh\nsleep 5\n").expect("write the script");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("make it executable");
        }

        let mut child = super::spawn(&script, None, None, None).expect("spawn a daemon");
        let pid = i32::try_from(child.id()).expect("a pid fits an i32");

        // SAFETY: the documented POSIX form; `getpgid` only reads.
        let child_group = unsafe { libc::getpgid(pid) };
        // SAFETY: as above, for this process.
        let ours = unsafe { libc::getpgid(0) };

        assert_eq!(
            child_group, pid,
            "a daemon must lead its own group, or a group-directed signal aimed at \
             its spawner reaches it"
        );
        assert_ne!(
            child_group, ours,
            "a daemon in its spawner's group is a node-wide off switch reachable \
             from any per-World stop"
        );

        let _ = child.force_kill_and_wait();
    }
}
