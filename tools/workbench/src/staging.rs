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
    /// Copy the executable beneath `root` and spawn the copy.
    ///
    /// `root` is shared and the copies inside it are keyed by fingerprint, so
    /// two runs of the same build reuse one image and two different builds get
    /// their own. That is what keeps a rebuild from contending with a daemon
    /// that is up.
    ///
    /// It is also why it has to be swept. Every distinct build leaves a copy of
    /// the whole executable — on this project, most of two hundred megabytes —
    /// and a day of rebuilding leaves a directory per build, permanently. The
    /// failure that produces is worse than its size: the client comes up
    /// looking entirely normal and silently has no daemon, because staging the
    /// next image is the thing that ran out of room. See [`StagedImage::sweep`].
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

        if let Staging::Staged { root } = policy {
            // Best effort, and after the image this run needs is in place: a
            // sweep that failed must never be the reason a daemon cannot start,
            // which is the failure it exists to prevent.
            Self::sweep(root, &fingerprint);
        }

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

    /// How many images survive a sweep, including the one just staged.
    ///
    /// More than one because a daemon that is up keeps running against the
    /// image it started with, and the run before this one is the likeliest to
    /// still be up. Not many more, because every one is a whole executable and
    /// the reason to sweep at all is that they are large.
    const KEEP: usize = 3;

    /// Remove staged images this run is not using, newest kept first.
    ///
    /// Nothing here can know which images are *in use* — a supervisor knows
    /// what it started, and this is reached from a constructor that does not.
    /// So it keeps the one just staged, keeps the most recent few beside it,
    /// and lets the rest go.
    ///
    /// Unlinking an executable a process is running is safe where this matters:
    /// on Unix the running image survives as an open inode, and on Windows the
    /// removal simply fails and that image is skipped. Either way a removal
    /// that goes wrong costs a directory, never a daemon.
    fn sweep(root: &Path, keep_fingerprint: &str) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        let mut staged: Vec<(std::time::SystemTime, PathBuf, String)> = entries
            .flatten()
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path(), name))
            })
            .collect();
        // Newest first, so what survives is what a daemon is likeliest to hold.
        staged.sort_by(|a, b| b.0.cmp(&a.0));

        let mut kept: usize = 0;
        for (_, path, name) in staged {
            if name == keep_fingerprint {
                continue;
            }
            kept = kept.saturating_add(1);
            if kept < Self::KEEP {
                continue;
            }
            let _ = std::fs::remove_dir_all(&path);
        }
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

    /// The leak. Every distinct build left a copy of the whole executable and
    /// nothing ever removed one, so a day of rebuilding filled the disk — and
    /// the failure it produced was that staging the *next* image ran out of
    /// room, which the client reports by coming up looking normal and silently
    /// having no daemon.
    #[test]
    fn staging_does_not_keep_every_build_forever() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("images");
        let source = directory.path().join("lait.exe");

        for build in 0..8u8 {
            write_executable(&source, &[build; 64]);
            StagedImage::prepare(
                &source,
                &Staging::Staged { root: root.clone() },
                build.into(),
            )
            .expect("prepare");
        }

        let staged = std::fs::read_dir(&root)
            .expect("the staging root")
            .flatten()
            .count();
        assert_eq!(
            staged,
            StagedImage::KEEP,
            "eight builds must not leave eight images"
        );
    }

    /// A daemon that is up keeps running against the image it started with, so
    /// the run before this one is the one most likely to still be held.
    #[test]
    fn a_sweep_keeps_the_image_this_run_staged_and_the_one_before_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("images");
        let source = directory.path().join("lait.exe");

        write_executable(&source, b"the build a daemon is running");
        let previous = StagedImage::prepare(&source, &Staging::Staged { root: root.clone() }, 1)
            .expect("prepare");
        // A moment apart, so "newest" is not a coin toss on a coarse clock.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_executable(&source, b"the build this run staged");
        let current = StagedImage::prepare(&source, &Staging::Staged { root: root.clone() }, 2)
            .expect("prepare");

        assert!(
            std::path::Path::new(&current.facts().staged_path).exists(),
            "the image this run needs survives its own sweep"
        );
        assert!(
            std::path::Path::new(&previous.facts().staged_path).exists(),
            "and so does the one a running daemon is likeliest to hold"
        );
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
