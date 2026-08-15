//! Handing a URL to the person's own browser.
//!
//! `Open` is a handoff, and this is the handoff. It is the one place in the
//! client that starts something it does not own — the browser belongs to the
//! person, not to us — so there is no handle here and nothing to stop later.
//!
//! ## Why not `cmd /C start`
//!
//! That is what `src/serve/mod.rs` does, and it is wrong from here for a reason
//! that has nothing to do with taste: `astrolabe.exe` is a windows-subsystem
//! program with no console, so spawning a console-subsystem child allocates one
//! — a black window that appears on screen, in front of whatever the person was
//! looking at, for as long as the shell takes to hand the URL on.
//!
//! `ShellExecuteW` asks the shell directly. No intermediate process, no
//! console, and no quoting rules to get wrong on a URL that carries a
//! credential in its query.

use crate::client::{ClientError, ClientResult};

/// Open `url` in whatever the person has chosen as their browser.
///
/// Refuses anything that is not `http://` or `https://`. The URL reaching here
/// is composed by this program from a head's own address, so this is not a
/// filter against a hostile caller — it is the guard that stops a defect
/// upstream from turning `ShellExecuteW` into "run whatever this string names",
/// which is what the shell would do with a path or a `file:` URL.
pub fn open(url: &str) -> ClientResult<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ClientError::invalid(format!(
            "'{url}' is not a web address, and the shell will not be asked to open it"
        )));
    }
    imp::open(url)
}

#[cfg(windows)]
mod imp {
    use crate::client::{ClientError, ClientResult};

    pub fn open(url: &str) -> ClientResult<()> {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        let target: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        // SAFETY: both strings are null-terminated wide buffers that outlive the
        // call, and every optional argument is passed as null, which the API
        // documents as "no directory" and "no parameters".
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // The return is a fake `HINSTANCE`: anything above 32 is success, and
        // anything at or below it is an error code wearing a pointer's clothes.
        // Read as an address rather than cast, which is the same comparison
        // without a lint waiver and without a width to get wrong.
        let outcome = result.addr();
        if outcome > 32 {
            return Ok(());
        }
        Err(ClientError::internal(format!(
            "the shell could not open a browser ({outcome})"
        )))
    }
}

#[cfg(not(windows))]
mod imp {
    use crate::client::{ClientError, ClientResult};

    /// Not a v1 target — Windows is — but the client builds and its tests run
    /// everywhere, and a `todo!()` here would make every non-Windows test run
    /// depend on nothing ever reaching this line.
    pub fn open(url: &str) -> ClientResult<()> {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                ClientError::internal(format!("could not open a browser with {opener}: {error}"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell will happily "open" a path, a document or an executable. What
    /// reaches here is always a head's own address, so this guard exists to keep
    /// a defect upstream from becoming an execution primitive.
    #[test]
    fn only_a_web_address_is_ever_handed_to_the_shell() {
        for hostile in [
            "file:///C:/Windows/System32/cmd.exe",
            "C:\\Windows\\System32\\cmd.exe",
            "javascript:alert(1)",
            "lait:invite/abc",
            "",
        ] {
            assert!(
                open(hostile).is_err(),
                "'{hostile}' was handed to the shell to open"
            );
        }
    }
}
