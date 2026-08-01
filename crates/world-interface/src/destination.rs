//! Turning a name a peer chose into a file this machine will write.
//!
//! An attachment's display name is product data authored by whoever attached
//! it. It travels, it is shown, and — the part that matters — it is the obvious
//! thing to name the file when someone saves it. That makes it a path supplied
//! by a remote party, and a path supplied by a remote party is not a path.
//!
//! So the display name and the destination are two different things here.
//! Sanitising produces a *file name*: no directories, no traversal, nothing a
//! shell or a filesystem will read as an instruction. Where that file goes is a
//! separate, local decision, and one nobody remote participates in.
//!
//! Nothing in this module touches the filesystem. It decides; the caller writes.

/// The longest sanitised name, in bytes.
///
/// Every filesystem in practical use allows 255 bytes per component. Leaving
/// room under that is what lets a caller add a temporary suffix and an
/// extension without the rename failing at the last step.
pub const MAX_DISPLAY_NAME_BYTES: usize = 200;

/// What a name is replaced with when nothing usable survives.
const FALLBACK: &str = "attachment";

/// Names Windows refuses whatever the extension, because they are devices.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Reduce a peer-authored display name to one safe file name.
///
/// Total: there is no input this refuses, because refusing would mean a peer
/// could make a file unsaveable by naming it badly. Everything hostile is
/// removed and something usable always comes back.
///
/// What it removes, and why each one:
///
/// - **Directory separators, both kinds.** `..\..\evil` is a traversal on
///   Windows and a legal single file name on Unix, so both are cut regardless
///   of the host — a store written on one platform is read on the other.
/// - **`.` and `..`** entirely, which is what traversal is made of.
/// - **Drive letters and roots**, so an absolute path becomes a name.
/// - **Control characters**, including CR and LF: a name containing them lands
///   in a `Content-Disposition` header and splits it.
/// - **Windows device names**, which are not files at any path.
/// - **Trailing dots and spaces**, which Windows silently strips — so
///   `evil.txt.` and `evil.txt` are the same file, and only one was checked.
pub fn sanitize_display_name(raw: &str) -> String {
    // The last component of whatever was given, treating both separators as
    // separators. Taking the last rather than rejecting the whole thing is what
    // keeps this total.
    let last = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        // A drive-relative name like `C:evil.txt` has no separator at all.
        .rsplit(':')
        .next()
        .unwrap_or(raw);

    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            // Reserved on Windows, awkward everywhere. Replaced rather than
            // dropped so two different names cannot collapse into one.
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim();

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return FALLBACK.to_string();
    }

    // A device name is prefixed rather than returned, so the bounds below still
    // apply to it. Returning here skipped both — and the prefix *adds* eleven
    // bytes, so the one path that most needed truncating was the one that
    // never got it.
    let stem_lower = cleaned
        .split('.')
        .next()
        .unwrap_or(cleaned)
        .to_ascii_lowercase();
    let prefixed;
    let cleaned = if RESERVED.contains(&stem_lower.as_str()) {
        prefixed = format!("{FALLBACK}-{cleaned}");
        prefixed.as_str()
    } else {
        cleaned
    };

    let truncated = truncate_preserving_extension(cleaned, MAX_DISPLAY_NAME_BYTES);

    // Trailing dots and spaces last, because truncation can create one.
    let trimmed = truncated.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        FALLBACK.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Shorten to `limit` bytes while keeping the extension, on a char boundary.
///
/// The extension is kept because it is what tells a person, and their operating
/// system, what the file is — truncating `report.pdf` to `report` produces
/// something nobody can open.
fn truncate_preserving_extension(name: &str, limit: usize) -> String {
    if name.len() <= limit {
        return name.to_string();
    }
    let extension = name
        .rfind('.')
        .filter(|dot| {
            *dot > 0
                && name
                    .len()
                    .checked_sub(*dot)
                    .is_some_and(|length| length <= 16)
        })
        .and_then(|dot| name.get(dot..))
        .unwrap_or("");
    let room = limit.saturating_sub(extension.len());
    let mut end = room.min(name.len());
    while end > 0 && !name.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", name.get(..end).unwrap_or(""), extension)
}

/// Where saved content is allowed to land.
///
/// `None` from [`Self::resolve`] means no destination is configured, and that
/// is a refusal rather than a default. Guessing one means writing a peer's
/// bytes to a directory nobody chose — and the process may be running with more
/// authority than the person who would have chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDestination {
    directory: std::path::PathBuf,
}

impl LocalDestination {
    /// Take a configured directory, or refuse.
    pub fn resolve(configured: Option<&std::path::Path>) -> Option<Self> {
        configured.map(|directory| Self {
            directory: directory.to_path_buf(),
        })
    }

    /// The full path a display name maps to inside this destination.
    ///
    /// The name is sanitised here rather than trusted from the caller, so there
    /// is no ordering in which an unsanitised name reaches a path.
    pub fn path_for(&self, display_name: &str) -> std::path::PathBuf {
        self.directory.join(sanitize_display_name(display_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_becomes_a_file_name() {
        for hostile in [
            "../../evil.txt",
            r"..\..\evil.txt",
            "/etc/passwd",
            r"C:\Windows\System32\evil.txt",
            r"C:evil.txt",
            "....//....//evil.txt",
        ] {
            let safe = sanitize_display_name(hostile);
            assert!(
                !safe.contains('/') && !safe.contains('\\') && !safe.contains(".."),
                "{hostile:?} became {safe:?}"
            );
            assert!(!safe.is_empty());
        }
    }

    #[test]
    fn a_name_that_would_split_a_header_cannot() {
        // The display name lands in `Content-Disposition`. A control character
        // there ends the header and starts whatever follows.
        let safe = sanitize_display_name("report.pdf\r\nX-Evil: yes");
        assert!(!safe.contains('\r') && !safe.contains('\n'), "{safe:?}");
    }

    #[test]
    fn nothing_is_ever_refused_outright() {
        // Refusing would let a peer make a file unsaveable by naming it badly.
        for empty in ["", ".", "..", "   ", "///", "\u{7}"] {
            assert!(!sanitize_display_name(empty).is_empty(), "{empty:?}");
        }
    }

    #[test]
    fn a_device_name_is_still_bounded_and_still_trimmed() {
        // The reserved branch used to return early, before truncation and
        // before the trailing-dot trim — so the one path that *added* eleven
        // bytes was the only one that never got shortened. A peer naming an
        // attachment `CON.` + 400 characters produced a 415-byte name, past
        // every filesystem's component limit, and the write failed with
        // ENAMETOOLONG rather than saving anything.
        let hostile = format!("CON.{}", "a".repeat(400));
        let safe = sanitize_display_name(&hostile);
        assert!(
            safe.len() <= MAX_DISPLAY_NAME_BYTES,
            "a device name escaped the bound at {} bytes: {safe}",
            safe.len()
        );
        assert!(safe.starts_with(FALLBACK));
        // And the trailing dot, which Windows strips — the reason it is
        // stripped here is that `evil.txt.` and `evil.txt` are one file and
        // only one of them was looked at.
        assert_eq!(sanitize_display_name("nul."), "attachment-nul");
    }

    #[test]
    fn windows_devices_are_not_files_at_any_path() {
        for device in ["CON", "nul.txt", "LPT1.log", "aux"] {
            let safe = sanitize_display_name(device);
            assert!(safe.starts_with(FALLBACK), "{device} became {safe}");
        }
    }

    #[test]
    fn a_trailing_dot_cannot_smuggle_a_second_name_past_a_check() {
        // Windows strips them, so `evil.txt.` and `evil.txt` are one file and
        // only one of them was looked at.
        assert_eq!(sanitize_display_name("evil.txt."), "evil.txt");
        assert_eq!(sanitize_display_name("evil.txt   "), "evil.txt");
    }

    #[test]
    fn a_long_name_keeps_its_extension() {
        let long = format!("{}.pdf", "a".repeat(400));
        let safe = sanitize_display_name(&long);
        assert!(safe.len() <= MAX_DISPLAY_NAME_BYTES);
        assert!(safe.ends_with(".pdf"), "{safe}");
    }

    #[test]
    fn an_ordinary_name_is_left_alone() {
        for fine in ["report.pdf", "Screenshot 2026-07-30.png", "notes"] {
            assert_eq!(sanitize_display_name(fine), fine);
        }
    }

    #[test]
    fn no_destination_configured_is_a_refusal_not_a_default() {
        assert_eq!(LocalDestination::resolve(None), None);
        let configured = LocalDestination::resolve(Some(std::path::Path::new("/tmp/saves")))
            .expect("a configured directory resolves");
        let path = configured.path_for("../../evil.txt");
        assert!(path.ends_with("evil.txt"), "{path:?}");
        assert!(path.starts_with("/tmp/saves"), "{path:?}");
    }
}

/// The regression this module exists for.
///
/// Kept beside the sanitizer rather than in the product, because the product is
/// one caller and the property is about the function.
#[cfg(test)]
mod attachment_regression {
    use super::*;

    #[test]
    fn a_peer_chosen_attachment_name_cannot_choose_where_we_write() {
        // `attachment get` defaulted its destination to the stored display
        // name, which is authored by whoever attached the file. That is an
        // arbitrary-path write of peer-controlled bytes, triggered by a local
        // user running an ordinary read command.
        for hostile in [
            r"..\..\..\Users\Public\startup.bat",
            "../../../../etc/cron.d/evil",
            r"C:\Windows\System32\drivers\etc\hosts",
        ] {
            let safe = sanitize_display_name(hostile);
            let path = std::path::Path::new(&safe);
            assert_eq!(
                path.components().count(),
                1,
                "{hostile:?} must reduce to a single component, got {safe:?}"
            );
            assert!(path.is_relative(), "{safe:?}");
        }
    }
}
