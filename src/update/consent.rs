//! Durable native consent and progress for World bundle/lifecycle updates.
//!
//! Enqueue is intentionally only an atomic bounded record write. Network
//! resolution, bundle verification, Space placement, product migration and
//! implementation activation all happen later on the daemon's bounded worker.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const FORMAT: u8 = 2;
const MAX_JOB_BYTES: usize = 64 * 1024;
static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Accepted,
    Fetching,
    Relaunching,
    Migrating,
    Waiting,
    Verified,
    Refused,
}

impl Phase {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Verified | Self::Refused)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Fetching => "fetching",
            Self::Relaunching => "relaunching",
            Self::Migrating => "migrating",
            Self::Waiting => "waiting",
            Self::Verified => "verified",
            Self::Refused => "refused",
        }
    }
}

/// One identity-owned World update operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    format: u8,
    pub world: String,
    pub operation: [u8; 16],
    pub phase: Phase,
    #[serde(default)]
    pub staged_version: Option<String>,
    #[serde(default)]
    pub current_orbit: Option<String>,
    #[serde(default)]
    pub after_orbit: Option<String>,
    #[serde(default)]
    pub completed_spaces: u64,
    #[serde(default)]
    pub total_spaces: u64,
    #[serde(default)]
    pub completed_records: u64,
    #[serde(default)]
    pub remaining_records: Option<u64>,
    #[serde(default)]
    pub message: Option<String>,
    pub updated_at: u64,
}

impl Job {
    pub fn operation_hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.operation)
    }

    pub fn terminal(&self) -> bool {
        self.phase.terminal()
    }
}

fn path(worlds: &Path, world: &str) -> PathBuf {
    super::world::world_root(worlds, world).join("upgrade.json")
}

/// Read the exact durable job, if one exists. Corruption is a typed error: it
/// never becomes absence and never silently authorizes another operation.
pub fn load(worlds: &Path, world: &str) -> Result<Option<Job>> {
    let path = path(worlds, world);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if bytes.len() > MAX_JOB_BYTES {
        anyhow::bail!(
            "World update record exceeds its {} byte bound",
            MAX_JOB_BYTES
        );
    }
    let mut job: Job =
        serde_json::from_slice(&bytes).with_context(|| format!("decode {}", path.display()))?;
    if !(1..=FORMAT).contains(&job.format) || job.world != world {
        anyhow::bail!("World update record identity does not match its path");
    }
    // Format 2 adds the durable relaunching phase. Format 1 records contain
    // only phases still represented above, so their exact progress upgrades
    // without reinterpretation on the next write.
    job.format = FORMAT;
    Ok(Some(job))
}

/// Durably accept an update operation before any size-proportional work.
/// Repeated consent while a job is live returns the same operation.
pub fn enqueue(worlds: &Path, world: &str, now: u64) -> Result<Job> {
    if replica::body::WorldId::parse(world).is_none() {
        anyhow::bail!("invalid World id");
    }
    if let Some(job) = load(worlds, world)? {
        if !job.terminal() {
            return Ok(job);
        }
    }
    let job = Job {
        format: FORMAT,
        world: world.to_owned(),
        operation: runtime::world::RequestId::mint().as_bytes(),
        phase: Phase::Accepted,
        staged_version: None,
        current_orbit: None,
        after_orbit: None,
        completed_spaces: 0,
        total_spaces: 0,
        completed_records: 0,
        remaining_records: None,
        message: None,
        updated_at: now,
    };
    save(worlds, &job)?;
    Ok(job)
}

/// Atomically replace one bounded job record.
pub fn save(worlds: &Path, job: &Job) -> Result<()> {
    if replica::body::WorldId::parse(&job.world).is_none() || job.format != FORMAT {
        anyhow::bail!("invalid World update record");
    }
    let bytes = serde_json::to_vec(job).context("encode World update record")?;
    if bytes.len() > MAX_JOB_BYTES {
        anyhow::bail!(
            "World update record exceeds its {} byte bound",
            MAX_JOB_BYTES
        );
    }
    let path = path(worlds, &job.world);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("World update record has no parent directory"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("tmp.{}.{}", std::process::id(), nonce));
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
    }
    commit_replace(&temporary, &path, parent)
        .with_context(|| format!("commit {}", path.display()))?;
    Ok(())
}

/// Windows-safe atomic replacement with the repository's bounded retry and
/// parent-directory durability contract.
pub(crate) fn commit_replace(temporary: &Path, path: &Path, parent: &Path) -> Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match mechanics::secretfs::persist_replace(temporary, path) {
            Ok(()) => {
                sync_dir(parent).with_context(|| format!("sync {}", parent.display()))?;
                return Ok(());
            }
            Err(error) => {
                last = Some(error);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
                }
            }
        }
    }
    Err(last.map_or_else(
        || anyhow!("atomic replacement did not run"),
        anyhow::Error::from,
    ))
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path).and_then(|directory| directory.sync_all())
}

#[cfg(windows)]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .or_else(|_| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
        });
    match directory {
        Ok(directory) => directory.sync_all(),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_is_durable_and_idempotent_while_pending() {
        let root = tempfile::tempdir().expect("temp root");
        let world = "lait.test.update";
        let first = enqueue(root.path(), world, 7).expect("enqueue");
        let second = enqueue(root.path(), world, 8).expect("repeat enqueue");
        assert_eq!(first.operation, second.operation);
        assert_eq!(load(root.path(), world).unwrap(), Some(first));
    }

    #[test]
    fn terminal_job_allows_a_new_explicit_consent() {
        let root = tempfile::tempdir().expect("temp root");
        let world = "lait.test.update";
        let mut first = enqueue(root.path(), world, 7).expect("enqueue");
        first.phase = Phase::Verified;
        save(root.path(), &first).expect("save terminal");
        let second = enqueue(root.path(), world, 8).expect("enqueue again");
        assert_ne!(first.operation, second.operation);
    }

    #[test]
    fn corrupt_job_is_not_treated_as_absent() {
        let root = tempfile::tempdir().expect("temp root");
        let path = path(root.path(), "lait.test.update");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"{").unwrap();
        assert!(load(root.path(), "lait.test.update").is_err());
    }

    #[test]
    fn format_one_progress_is_upgraded_without_losing_its_cursor() {
        let root = tempfile::tempdir().expect("temp root");
        let world = "lait.test.update";
        let mut legacy = enqueue(root.path(), world, 7).expect("enqueue");
        legacy.format = 1;
        legacy.phase = Phase::Waiting;
        legacy.current_orbit = Some("space-b".into());
        legacy.completed_records = 19;
        std::fs::write(
            path(root.path(), world),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        let upgraded = load(root.path(), world).unwrap().expect("legacy job");
        assert_eq!(upgraded.format, FORMAT);
        assert_eq!(upgraded.current_orbit.as_deref(), Some("space-b"));
        assert_eq!(upgraded.completed_records, 19);
    }

    #[test]
    fn restart_preserves_progress_and_replayed_consent() {
        let root = tempfile::tempdir().expect("temp root");
        let world = "lait.test.update";
        let mut staged = enqueue(root.path(), world, 7).expect("enqueue");
        staged.phase = Phase::Waiting;
        staged.staged_version = Some("4.0.0".into());
        staged.current_orbit = Some("space-b".into());
        staged.after_orbit = Some("space-a".into());
        staged.completed_spaces = 1;
        staged.completed_records = 256;
        staged.remaining_records = Some(512);
        staged.message = Some("Waiting to retry the bounded Space lifecycle step".into());
        save(root.path(), &staged).expect("save progress");

        let reopened = load(root.path(), world).unwrap().expect("reopen");
        assert_eq!(reopened, staged);
        assert_eq!(
            enqueue(root.path(), world, 8).expect("replayed consent"),
            staged,
            "a restart/replay must retain the operation and exact cursor"
        );
    }
}
