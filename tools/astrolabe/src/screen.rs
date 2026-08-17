//! Whether this machine comes up as a screen, and pointed at what.
//!
//! # Two facts, not one
//!
//! Being a screen and showing something are separate, so both are recorded.
//! Collapsing them — persisting only a selection, and inferring the mode from
//! its presence — would mean a person who entered Big Picture and had not yet
//! chosen came back to the Library, which is the client quietly overriding a
//! choice they made.
//!
//! # No secret here
//!
//! An Orbit id, a World, a surface id, the package's own input and a title.
//! Not one field authenticates anything, so this is an ordinary file with the
//! same reasoning `coordinator-policy.json` carries: a wrap would buy nothing
//! and would make a restore onto another profile lose a preference for no
//! reason.
//!
//! # Absence keeps its kind
//!
//! A missing file is *never chosen*. A file that will not parse is **also**
//! treated as never chosen, and says so in a log line rather than refusing the
//! client's start — a preference is not worth failing to launch over, and a
//! client that would not start because it could not remember a screen would be
//! the worst possible trade.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::PresentationSelection;

const FILE: &str = "screen.json";
const VERSION: u32 = 1;

/// The largest this file may be before it is treated as corrupt. A preference
/// holding a package input is small; anything approaching this is not one.
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenPreference {
    pub version: u32,
    /// Whether the last session was in Big Picture at all.
    pub presenting: bool,
    /// What it was pointed at, if anything. `None` with `presenting` true is a
    /// screen that was entered and never chosen — a real state to come back to.
    #[serde(default)]
    pub selection: Option<Selection>,
}

/// The persisted half of [`PresentationSelection`]. A separate type on purpose:
/// this one is a durable format and has to keep its shape, while the model's is
/// free to change with the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub orbit: String,
    pub world: String,
    pub surface: String,
    pub input: String,
    pub title: String,
}

impl From<&PresentationSelection> for Selection {
    fn from(selection: &PresentationSelection) -> Self {
        Self {
            orbit: selection.orbit.clone(),
            world: selection.world.clone(),
            surface: selection.surface.clone(),
            input: selection.input.clone(),
            title: selection.title.clone(),
        }
    }
}

impl From<Selection> for PresentationSelection {
    fn from(selection: Selection) -> Self {
        Self {
            orbit: selection.orbit,
            world: selection.world,
            surface: selection.surface,
            input: selection.input,
            title: selection.title,
        }
    }
}

fn path(state_root: &Path) -> PathBuf {
    state_root.join(FILE)
}

/// What the last session left. `None` is "never chosen", including the case
/// where the file exists and cannot be read.
pub fn load(state_root: &Path) -> Option<ScreenPreference> {
    let path = path(state_root);
    let size = std::fs::metadata(&path).ok()?.len();
    if size > MAX_BYTES {
        tracing::warn!(
            path = %path.display(),
            size,
            "screen preference is implausibly large; coming up as the Library"
        );
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice::<ScreenPreference>(&bytes) {
        Ok(held) if held.version == VERSION => Some(held),
        Ok(held) => {
            tracing::warn!(
                version = held.version,
                "screen preference is a version this build does not read"
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, "screen preference did not parse");
            None
        }
    }
}

/// Record that this machine is a screen, and what it is pointed at.
pub fn save(state_root: &Path, selection: Option<&PresentationSelection>) -> std::io::Result<()> {
    write(
        state_root,
        &ScreenPreference {
            version: VERSION,
            presenting: true,
            selection: selection.map(Selection::from),
        },
    )
}

/// Record that this machine is not a screen.
///
/// Written rather than deleted, so leaving is a decision the next launch reads
/// rather than an absence indistinguishable from never having entered.
pub fn clear(state_root: &Path) -> std::io::Result<()> {
    write(
        state_root,
        &ScreenPreference {
            version: VERSION,
            presenting: false,
            selection: None,
        },
    )
}

fn write(state_root: &Path, held: &ScreenPreference) -> std::io::Result<()> {
    std::fs::create_dir_all(state_root)?;
    let target = path(state_root);
    let temporary = target.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(held)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(&temporary, &bytes)?;
    // Replace rather than truncate-in-place: a client killed mid-write should
    // come up with the previous preference, not half of this one.
    std::fs::rename(&temporary, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "astrolabe-screen-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn selection() -> PresentationSelection {
        PresentationSelection {
            orbit: "ws_one".into(),
            world: "com.lait.signage".into(),
            surface: "signage.program".into(),
            input: "{\"program\":\"bod_x\"}".into(),
            title: "Lobby loop".into(),
        }
    }

    #[test]
    fn a_machine_that_was_a_screen_comes_back_as_one() {
        let root = temp();
        save(&root, Some(&selection())).unwrap();
        let held = load(&root).expect("a preference");
        assert!(held.presenting);
        assert_eq!(
            PresentationSelection::from(held.selection.unwrap()),
            selection()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn entering_without_choosing_is_a_state_worth_returning_to() {
        let root = temp();
        save(&root, None).unwrap();
        let held = load(&root).expect("a preference");
        assert!(held.presenting, "entering was not remembered");
        assert!(held.selection.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn leaving_is_recorded_rather_than_forgotten() {
        let root = temp();
        save(&root, Some(&selection())).unwrap();
        clear(&root).unwrap();

        // Written, not deleted: "they left" and "they never entered" are the
        // same next launch either way, but only one of them is a fact this
        // build actually observed.
        let held = load(&root).expect("a preference");
        assert!(!held.presenting);
        assert!(held.selection.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_unreadable_preference_comes_up_as_the_library() {
        let root = temp();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(path(&root), b"{ this is not json").unwrap();

        // A preference is not worth failing to launch over.
        assert!(load(&root).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_preference_from_a_later_build_is_not_guessed_at() {
        let root = temp();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            path(&root),
            br#"{"version":99,"presenting":true,"selection":null}"#,
        )
        .unwrap();
        assert!(load(&root).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nothing_recorded_is_not_a_screen() {
        assert!(load(&temp()).is_none());
    }
}
