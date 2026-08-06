//! Process-level disposition this binary sets before it does anything else.

/// Stop our own stdio from leaking into processes we never handed it to.
///
/// On Windows `CreateProcess` is called with `bInheritHandles=TRUE`, and that is
/// all-or-nothing: a child inherits *every* inheritable handle in this process,
/// not just the three named in `STARTUPINFO`. When our stdout/stderr are pipes
/// (any captured run — `Command::output()`, a test harness, an MCP client
/// reading our stdio) those pipe handles are inheritable, so a child that
/// outlives us keeps our caller's stdout open and it never sees the EOF it is
/// reading for. Unix is immune: those fds are `CLOSE_ON_EXEC`.
///
/// The daemon — the child that outlives us by design, and the one this actually
/// bit — is handled precisely in [`crate::daemon_spawn`], which names the
/// handles it may inherit. This is the blanket for everything else we spawn
/// without that ceremony.
///
/// Clearing `HANDLE_FLAG_INHERIT` on our end costs nothing we want: for
/// `Stdio::inherit()` std duplicates the handle with `bInheritHandle=TRUE` into
/// the child's `STARTUPINFO`, so a child we *do* hand stdio to still lands its
/// output on ours.
///
/// Runs for every mode, the MCP server included — it speaks its protocol over
/// stdio pipes, and its client is reading for that same EOF.
#[cfg(windows)]
pub fn disinherit_stdio() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};

    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    for h in handles {
        if h.is_null() {
            continue;
        }
        // SAFETY: `h` is a live std handle we own, borrowed for this call only.
        // Best-effort: a std stream can be closed or invalid (a detached
        // service), and failing to clear a handle we never had is not an error.
        #[allow(
            clippy::as_conversions,
            reason = "the OS API takes the same handle std hands back, under its own type name"
        )]
        unsafe {
            SetHandleInformation(h as _, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

#[cfg(not(windows))]
pub fn disinherit_stdio() {}

/// Start from a known signal environment rather than the caller's.
///
/// `std::process::Command` resets **only** SIGPIPE in the child: it preserves
/// the signal mask on purpose, and it never touches an inherited `SIG_IGN`. So
/// every disposition and every blocked signal in the chain that launched us —
/// a shell ignoring SIGINT/SIGQUIT for a background job, an IDE, a supervisor,
/// an agent harness that masked SIGTERM — arrives here intact, and the daemon
/// inherits it through `daemon_spawn` as well.
///
/// Two of those inheritances are indistinguishable from a bug in this program.
/// A blocked SIGTERM is never delivered at all: `kill` reports success, the
/// signal sits pending forever, and the process looks like one that ignores
/// being asked to stop. An inherited `SIG_IGN` for SIGQUIT does the same to
/// that signal. Neither has anything to do with what this binary wants; both
/// are simply what the launcher was holding.
///
/// SIGPIPE stays ignored — see the note in `main`: every mode here is a
/// long-running service doing socket I/O, and the default action would turn a
/// dropped peer into a killed process.
#[cfg(unix)]
pub fn reset_signal_environment() {
    // SAFETY: all four calls are the documented POSIX forms on zeroed, owned
    // storage of the right type. This runs on the main thread before any other
    // thread of ours exists, and a process-directed signal is delivered to any
    // thread that does not block it, so unblocking here is enough for the
    // process — the mask a later thread inherits does not hide the signal.
    unsafe {
        let mut unblocked: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblocked);
        libc::pthread_sigmask(libc::SIG_SETMASK, &unblocked, std::ptr::null_mut());

        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
            let mut current: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, std::ptr::null(), &mut current) != 0 {
                continue;
            }
            // Only undo an inherited *ignore*. A handler at this point would be
            // one we installed, and there are none this early.
            if current.sa_sigaction == libc::SIG_IGN {
                let mut restored: libc::sigaction = std::mem::zeroed();
                restored.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut restored.sa_mask);
                libc::sigaction(signal, &restored, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(not(unix))]
pub fn reset_signal_environment() {}

/// Give the termination signals back to the kernel.
///
/// Tokio installs its signal handlers through `sigaction` and **never removes
/// them** — "once a signal handler is registered with the process the
/// underlying libc signal handler is never unregistered". That is fine while
/// something is listening. It is not fine afterwards: a registered handler with
/// no live listener discards the signal in silence, so SIGTERM stops meaning
/// "terminate" and starts meaning nothing at all, for the rest of the process's
/// life.
///
/// That is how a daemon becomes unkillable. Its shutdown listener is one-shot;
/// past it, a process still winding down — or wedged winding down — absorbs
/// every SIGTERM without a trace, and only SIGKILL is left. Handing the
/// disposition back at that exact moment restores the ordinary meaning of the
/// ordinary signal.
#[cfg(unix)]
pub fn restore_default_termination_signals() {
    // SAFETY: as above — the documented POSIX form on zeroed, owned storage.
    unsafe {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let mut restored: libc::sigaction = std::mem::zeroed();
            restored.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut restored.sa_mask);
            libc::sigaction(signal, &restored, std::ptr::null_mut());
        }
    }
}

#[cfg(not(unix))]
pub fn restore_default_termination_signals() {}
