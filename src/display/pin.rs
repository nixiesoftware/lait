//! The pinned registrar chronicle head — this identity's half of the ratchet.
//!
//! One file beside the identity, like the update feed's pointer stamp, and
//! with the same posture stated there: a missing or unreadable pin re-arms at
//! the next head — loudly, because a node that re-pins has no divergence
//! protection for that one step while looking exactly like one that does —
//! and a failure to write is loud for the same reason. The pin itself only
//! moves through [`mechanics::chronicle::advance`], which is where forward-
//! only lives; this file is storage, not policy.
//!
//! The whole signed [`Head`] is kept, not the reduced pin: on divergence the
//! two irreconcilable heads *are* the evidence, and evidence with the
//! signature stripped is a claim.

use std::path::{Path, PathBuf};

use mechanics::chronicle::Head;

fn pin_path(identity_home: &Path) -> PathBuf {
    identity_home.join("registry-chronicle.pin")
}

fn evidence_path(identity_home: &Path) -> PathBuf {
    identity_home.join("registry-chronicle.diverged")
}

/// The head this identity last accepted, if one is held and readable.
pub fn load(identity_home: &Path) -> Option<Head> {
    let path = pin_path(identity_home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "the pinned chronicle head could not be read; the next head re-pins without \
                 divergence protection for this one step"
            );
            return None;
        }
    };
    match serde_json::from_str::<Head>(&text) {
        Ok(head) => Some(head),
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "the pinned chronicle head did not decode; the next head re-pins without \
                 divergence protection for this one step"
            );
            None
        }
    }
}

/// Record an accepted head. Loud on failure, for the reason the feed's stamp
/// is: a node that silently cannot persist this has no replay protection
/// while looking exactly like one that does. Written atomically — a crash or
/// a full disk mid-write must not leave a truncated pin that `load` would
/// read as absent and re-arm from, which is a timed-crash reset of the
/// ratchet.
pub fn save(identity_home: &Path, head: &Head) {
    let path = pin_path(identity_home);
    let Ok(text) = serde_json::to_string(head) else {
        tracing::warn!(path = %path.display(), "the chronicle head did not encode");
        return;
    };
    if let Err(error) = atomic_write(&path, text.as_bytes()) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not record the chronicle pin; this identity cannot detect a rewritten registry"
        );
    }
}

/// Keep both irreconcilable signed heads. This file existing is the fact a
/// surface reports; its contents are what a third party checks — so a second
/// divergence must not clobber the first's evidence. Written once, never
/// overwritten: if the file is already there, the earliest incriminating pair
/// is the one that is kept.
pub fn keep_divergence(identity_home: &Path, held: &Head, offered: &Head) {
    let path = evidence_path(identity_home);
    if path.exists() {
        tracing::error!(
            path = %path.display(),
            "chronicle divergence again — earlier evidence retained, this pair not overwritten"
        );
        return;
    }
    let Ok(text) = serde_json::to_string(&serde_json::json!({
        "held": held,
        "offered": offered,
    })) else {
        return;
    };
    if let Err(error) = atomic_write(&path, text.as_bytes()) {
        tracing::error!(
            path = %path.display(),
            %error,
            "could not retain the divergence evidence"
        );
    }
}

/// Write via a temp file and rename, so a reader never sees a half-written
/// file: the rename is atomic on every platform this runs on, and a crash
/// before it leaves the previous contents intact.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::chronicle::Chronicle;

    #[test]
    fn a_pin_round_trips_and_absence_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load(dir.path()).is_none());
        let mut log = Chronicle::new();
        log.append(b"entry").expect("append");
        let head = log.head(&[9u8; 32]).expect("head");
        save(dir.path(), &head);
        let held = load(dir.path()).expect("held");
        assert_eq!(held, head);
        held.verify().expect("the stored artifact still verifies");
    }

    #[test]
    fn a_corrupt_pin_reads_as_absent_not_as_a_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(pin_path(dir.path()), b"not a head").expect("write");
        assert!(load(dir.path()).is_none());
    }
}
