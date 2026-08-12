//! Running a daemon from a copy, so a rebuild never has to wait for it.
//!
//! A running `lait.exe` holds its own image open, and Windows will not let a
//! link step replace a file that is mapped as an executable. The daily cost of
//! that is real: terminate every daemon by hand, rebuild, restart them, and let
//! every agent rediscover an MCP tool surface that died underneath it.
//!
//! Staging removes the contention rather than managing it. A development run
//! copies the executable into a per-run directory and spawns from there, so the
//! workspace target is never the file anything is holding. The build writes to
//! a path nothing has open, and the daemons that are up keep running against
//! the image they started with — which is exactly why the image each device is
//! running is reported rather than assumed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::contract::ImageFacts;

/// Where a supervisor spawns daemons from.
#[derive(Clone, Debug, Default)]
pub enum Staging {
    /// Spawn the executable where it is. The packaged client's case: there is
    /// no build to contend with, and copying would only add a path to explain.
    #[default]
    Direct,
    /// Copy the executable beneath `root` and spawn the copy. `root` is a
    /// per-run directory, not a shared one — two clients staging into the same
    /// place would be back to contending, just with extra steps.
    Staged { root: PathBuf },
}

/// The image a supervisor is spawning, and where it came from.
#[derive(Clone, Debug)]
pub struct StagedImage {
    executable: PathBuf,
    facts: ImageFacts,
}

impl StagedImage {
    /// Resolve `source` under `policy`, copying it if the policy says to.
    pub fn prepare(source: &Path, policy: &Staging, now_ms: u64) -> Result<Self> {
        let source = std::fs::canonicalize(source)
            .with_context(|| format!("resolve executable {}", source.display()))?;
        let bytes = std::fs::read(&source)
            .with_context(|| format!("read executable {}", source.display()))?;
        let fingerprint = fingerprint(&bytes);

        let executable = match policy {
            Staging::Direct => source.clone(),
            Staging::Staged { root } => {
                let directory = root.join(&fingerprint);
                std::fs::create_dir_all(&directory)
                    .with_context(|| format!("create staging directory {}", directory.display()))?;
                let name = source.file_name().unwrap_or_else(|| "lait".as_ref());
                let staged = directory.join(name);
                // Keyed by fingerprint, so an existing copy with this name is
                // byte-identical by construction and re-copying it would only
                // risk failing against a running daemon that already holds it.
                if !staged.exists() {
                    std::fs::write(&staged, &bytes)
                        .with_context(|| format!("stage executable to {}", staged.display()))?;
                    copy_permissions(&source, &staged)?;
                }
                staged
            }
        };

        Ok(Self {
            facts: ImageFacts {
                source_path: source.to_string_lossy().into_owned(),
                staged_path: executable.to_string_lossy().into_owned(),
                fingerprint,
                staged_at_ms: now_ms,
            },
            executable,
        })
    }

    /// The path to spawn.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn facts(&self) -> &ImageFacts {
        &self.facts
    }
}

/// A content hash, hex, truncated to something a directory name and a human can
/// both carry. Collision resistance here only has to distinguish builds of the
/// same binary, not resist an adversary.
fn fingerprint(bytes: &[u8]) -> String {
    let digest = blake3::hash(bytes);
    digest
        .as_bytes()
        .iter()
        .take(8)
        .fold(String::new(), |mut text, byte| {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(unix)]
fn copy_permissions(source: &Path, staged: &Path) -> Result<()> {
    // The execute bit does not travel with the bytes, and a staged image that
    // cannot be executed fails at spawn with an error that names the wrong
    // thing.
    let mode = std::fs::metadata(source)
        .with_context(|| format!("read permissions of {}", source.display()))?
        .permissions();
    std::fs::set_permissions(staged, mode)
        .with_context(|| format!("set permissions on {}", staged.display()))
}

#[cfg(not(unix))]
fn copy_permissions(_source: &Path, _staged: &Path) -> Result<()> {
    // Windows decides executability by extension, which the copy preserves.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_executable(path: &Path, contents: &[u8]) {
        std::fs::write(path, contents).expect("write executable");
    }

    #[test]
    fn direct_staging_spawns_the_file_where_it_is() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("lait.exe");
        write_executable(&source, b"build one");

        let image = StagedImage::prepare(&source, &Staging::Direct, 7).expect("prepare");
        assert_eq!(
            image.executable(),
            std::fs::canonicalize(&source).expect("canonical").as_path()
        );
        assert_eq!(image.facts().source_path, image.facts().staged_path);
    }

    #[test]
    fn a_staged_run_leaves_the_source_free_and_reports_both_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("lait.exe");
        write_executable(&source, b"build one");
        let root = directory.path().join("staging");

        let image = StagedImage::prepare(&source, &Staging::Staged { root }, 7).expect("prepare");
        assert_ne!(image.facts().source_path, image.facts().staged_path);
        assert!(image.executable().is_file());
        assert_eq!(
            std::fs::read(image.executable()).expect("read staged"),
            b"build one"
        );
        // The whole point: the source is not the file being held.
        assert!(source.is_file());
    }

    /// A rebuild replaces the source while a staged copy is still in use. The
    /// old copy must survive untouched — a daemon is running from it — and the
    /// new one must be a different image with a different fingerprint.
    #[test]
    fn restaging_after_a_rebuild_does_not_disturb_the_running_copy() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("lait.exe");
        let root = directory.path().join("staging");
        write_executable(&source, b"build one");
        let first = StagedImage::prepare(&source, &Staging::Staged { root: root.clone() }, 1)
            .expect("first");

        write_executable(&source, b"build two");
        let second = StagedImage::prepare(&source, &Staging::Staged { root }, 2).expect("second");

        assert_ne!(first.facts().fingerprint, second.facts().fingerprint);
        assert_ne!(first.executable(), second.executable());
        assert_eq!(
            std::fs::read(first.executable()).expect("read first"),
            b"build one",
            "restaging overwrote an image a daemon may still be running"
        );
        assert_eq!(
            std::fs::read(second.executable()).expect("read second"),
            b"build two"
        );
    }

    #[test]
    fn identical_bytes_stage_once() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("lait.exe");
        let root = directory.path().join("staging");
        write_executable(&source, b"build one");

        let first = StagedImage::prepare(&source, &Staging::Staged { root: root.clone() }, 1)
            .expect("first");
        let again = StagedImage::prepare(&source, &Staging::Staged { root }, 2).expect("again");
        assert_eq!(first.executable(), again.executable());
        assert_eq!(first.facts().fingerprint, again.facts().fingerprint);
    }
}
