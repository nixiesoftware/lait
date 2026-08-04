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
