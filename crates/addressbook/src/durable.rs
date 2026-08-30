//! Replacing a file without losing it, and finding it again after a crash.
//!
//! The two halves belong together and are worth having in one place, because
//! shipping one without the other is a silent data-loss bug rather than a
//! visible one: [`atomic_replace`] deliberately leaves a window in which the
//! main file does not exist, and [`open_or_recover`] is the only thing that
//! makes that window survivable. A second store that copied the replace and not
//! the recovery would answer "there is nothing here" for a file that is sitting
//! right beside it, fully written.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::Error;

/// Write `bytes` in place of whatever `path` holds, keeping the old copy.
///
/// The new bytes are synced under `.tmp` *before* the swap begins, so the
/// window between removing the old file and renaming the new one always has a
/// complete survivor on disk. [`open_or_recover`] is what finds it.
pub fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("bin.tmp");
    let bak = path.with_extension("bin.bak");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    if path.exists() {
        fs::copy(path, &bak)?;
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Read `path`, or the survivor of an interrupted [`atomic_replace`].
///
/// Absence is only "nothing was ever written" when no survivor exists. Prefer
/// the `.tmp` — it was synced before the swap began — then the `.bak`. A
/// survivor that reads is copied back into place and the survivor kept. When
/// candidates exist and none of them read, this fails closed with the first
/// error rather than answering absence, because absence is what a caller acts
/// on by starting over.
pub fn open_or_recover<T>(
    path: &Path,
    read: impl Fn(&Path) -> Result<T, Error>,
) -> Result<Option<T>, Error> {
    if path.exists() {
        return read(path).map(Some);
    }
    let tmp = path.with_extension("bin.tmp");
    let bak = path.with_extension("bin.bak");
    let mut first_failure: Option<Error> = None;
    for candidate in [&tmp, &bak] {
        if !candidate.exists() {
            continue;
        }
        match read(candidate) {
            Ok(value) => {
                fs::copy(candidate, path)?;
                return Ok(Some(value));
            }
            Err(err) => {
                if first_failure.is_none() {
                    first_failure = Some(err);
                }
            }
        }
    }
    match first_failure {
        Some(err) => Err(err),
        None => Ok(None),
    }
}
