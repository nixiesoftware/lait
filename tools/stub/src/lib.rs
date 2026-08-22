//! The staged swap behind the stub launcher (CLIENT-65).
//!
//! The stub is the stable half of an installed client: the shortcut target
//! that survives every update. Its whole job at launch is four sentences.
//! Claim the installation. If a verified staged tree is waiting, swap it in
//! by rename. Say every condition that prevented that — a tampered stage, a
//! live client, an unwritable root — instead of silently declining. Then
//! start the live tree regardless, because a launcher that refuses to launch
//! is a worse defect than a missed update.
//!
//! The install root is the stub's own directory, discovered from
//! `current_exe` and nothing else — the same "the layout is where I am"
//! rule as `astrolabe`'s `sidecar::beside`. It holds:
//!
//! ```text
//! astrolabe(.exe)          this binary, installed under the APPLICATION'S
//!                          name — the one file an update never moves, so
//!                          every shell artifact points here. `astrolabe-stub`
//!                          is only the build name; nothing installs it.
//! current/                 the live tree: the astrolabe+lait pair, flat
//! previous/                the prior live tree, kept as the rollback target
//! staged/                  a downloaded tree waiting to become current
//! staged.manifest.json     what staged/ must hash to, written by the stager
//! instance.lock            the claim: held while a client is alive here
//! staging.lock             held while staged/ is being written or consumed
//! stub.log                 every named refusal and recovery, appended
//! ```
//!
//! ## The claim is what makes "nothing applies under a running client" true
//!
//! The stub takes `instance.lock` before it looks at anything and holds it
//! **for the lifetime of the client it starts** — it waits on the child
//! rather than exiting under it. That is the whole enforcement: no
//! cooperation from the client is required, no second component has to
//! remember to take a lock, and a second launch finds the claim held and
//! defers. The alternative — trusting the client to hold a lock — was
//! measured and rejected: `astrolabe::single_instance::acquire` has zero
//! production call sites today, so a gate resting on it would read as
//! enforced while enforcing nothing.
//!
//! `staging.lock` is a different fact and therefore a different file: it
//! excludes the stager (`lait::update::tree`, which runs in the daemon while
//! a client is alive) from the stub's verify-then-swap window. Staging must
//! keep working under a live client; consuming a stage must not race it.
//!
//! ## The stage manifest
//!
//! Written by `lait::update::tree`, read here. The shape is deliberately
//! duplicated rather than shared through a dependency — the stub must stay
//! free of the engine — and the chain test in
//! `tools/astrolabe/tests/launch.rs` is what holds the two halves together,
//! the way the packaging test holds `sidecar::beside` and
//! `lait::update::custody_of` together.
//!
//! Invariants carried from the requirement Spec *Update invariants*: verify
//! before swap on every path (the stub re-proves every file against the
//! manifest; a marker is never trusted), nothing applies under a running
//! client, renames only, and keep current plus previous — the previous tree
//! stays bootable until the next *successful* swap, and is what a client
//! that cannot start falls back to.

pub mod bundle;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};

/// The stage manifest's file name, beside — never inside — `staged/`, so a
/// tree without its manifest is inert bytes the stub ignores.
pub const STAGE_MANIFEST: &str = "staged.manifest.json";
/// The staged tree waiting to become current.
pub const STAGED_DIR: &str = "staged";
/// The live tree.
pub const CURRENT_DIR: &str = "current";
/// The prior live tree, kept as the local rollback target.
pub const PREVIOUS_DIR: &str = "previous";
/// The claim: held for as long as a client started here is alive.
pub const INSTANCE_LOCK: &str = "instance.lock";
/// Held while `staged/` is written (by the stager) or consumed (here).
pub const STAGING_LOCK: &str = "staging.lock";
/// Where every named refusal and recovery is appended.
pub const STUB_LOG: &str = "stub.log";

/// Prefixes of the scratch both halves leave behind when a delete loses a
/// race with a scanner. Swept on every claim.
const SWEEPABLE: &[&str] = &[
    "previous.trash-",
    "staged.tmp-",
    "staged.manifest.json.tmp-",
];

/// What `staged/` must contain, byte for byte. Written by the stager
/// (`lait::update::tree`), proven again here before any rename.
#[derive(Debug, Serialize, Deserialize)]
pub struct StageManifest {
    /// The release version this tree carries.
    pub version: String,
    /// The entry binary's path relative to the tree root.
    pub entry: String,
    /// Every file in the tree. A file on disk the manifest does not name is
    /// as much a verification failure as a missing or altered one.
    pub files: Vec<StagedFile>,
}

/// One file of the staged tree.
#[derive(Debug, Serialize, Deserialize)]
pub struct StagedFile {
    /// Path relative to the tree root, `/`-separated on every platform.
    pub path: String,
    /// Lowercase hex blake3 of the file's bytes.
    pub blake3: String,
    /// The file's size in bytes.
    pub size: u64,
    /// Whether the stager recorded an executable mode bit. Re-applied here
    /// before the swap: a tree that verifies byte for byte but cannot exec
    /// is a tree that swaps in and then will not start.
    pub executable: bool,
}

/// The exclusive claim on an installation. Held across the swap *and* the
/// client's lifetime, so its existence is what "a client is alive here"
/// means. Released on drop.
#[derive(Debug)]
pub struct Claim {
    file: fs::File,
}

impl Drop for Claim {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// What launch found and did about the staged state. Every refusal has
/// already been said (stderr and `stub.log`) by the time it is returned —
/// the value exists so the chain test can assert on facts, not log text.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A verified staged tree became the live tree.
    Applied {
        /// The version the stage manifest named.
        version: String,
    },
    /// No stage manifest was present; the ordinary launch.
    NothingStaged,
    /// A staged tree was present and was not applied, for the named reason.
    Refused {
        /// The named condition, exactly as said.
        reason: String,
    },
    /// The root was found mid-swap, or holding residue, and was repaired.
    Recovered {
        /// What the repair did, exactly as said.
        how: String,
    },
}

/// The platform's entry binary name inside a tree.
fn entry_name() -> &'static str {
    if cfg!(windows) {
        "astrolabe.exe"
    } else {
        "astrolabe"
    }
}

/// Say a condition where a person can find it: stderr now, `stub.log`
/// durably. Never fails — a report that could abort the launch would turn
/// every disk hiccup into a client that does not start.
pub(crate) fn say(root: &Path, message: &str) {
    eprintln!("astrolabe-stub: {message}");
    if let Ok(mut log) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(STUB_LOG))
    {
        let _ = writeln!(log, "{message}");
    }
}

/// Open a lock file without disturbing its contents: the bytes are
/// irrelevant, only the advisory lock matters, and truncating a file
/// another process holds is not ours to do.
fn lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

/// A name no concurrent or later run can collide with. Process ids are
/// recycled — aggressively on Windows — so a bare pid suffix eventually
/// meets an orphan of its own name, and a rename onto an existing directory
/// is an error there. The counter closes the within-process case; [`sweep`]
/// clears whatever a lost delete leaves behind.
pub(crate) fn scratch_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Remove scratch a previous run could not delete. Best effort by design: a
/// scanner holding one is a reason to try again next launch, never a reason
/// to refuse an update or a launch.
fn sweep(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SWEEPABLE.iter().any(|prefix| name.starts_with(prefix)) {
            let path = entry.path();
            if path.is_dir() {
                let _ = fs::remove_dir_all(&path);
            } else {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// Claim the installation, or report that something already holds it.
///
/// `Ok(None)` is a live client (or another stub) — the ordinary deferral,
/// not an error. Sweeps stale scratch on the way in, which is the one moment
/// this process is known to be the only one touching the root.
pub fn claim(root: &Path) -> std::io::Result<Option<Claim>> {
    let file = lock_file(&root.join(INSTANCE_LOCK))?;
    if file.try_lock_exclusive().is_err() {
        return Ok(None);
    }
    sweep(root);
    Ok(Some(Claim { file }))
}

/// Prove `staged/` is exactly what the manifest describes.
///
/// Every named file must be present with the named size and digest, and the
/// tree must hold nothing else. The "nothing else" half is a count, not a
/// name comparison: filesystems normalize names (macOS returns NFD where an
/// archive carried NFC), so comparing the stager's recorded strings against
/// what `read_dir` hands back would refuse a legitimate tree with a
/// non-ASCII path. Counting is exactly as strong — every named file is
/// opened and hashed *by path*, so a rename shows up as a missing file and a
/// stowaway shows up as a surplus.
pub(crate) fn verify_staged(root: &Path) -> Result<StageManifest, String> {
    let manifest_bytes = fs::read(root.join(STAGE_MANIFEST))
        .map_err(|error| format!("the stage manifest could not be read: {error}"))?;
    let manifest: StageManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("the stage manifest could not be parsed: {error}"))?;

    if !manifest
        .files
        .iter()
        .any(|file| file.path == manifest.entry)
    {
        return Err(format!(
            "staged tree verification failed: the manifest names {} as its entry and does not \
             carry it",
            manifest.entry
        ));
    }

    let staged = root.join(STAGED_DIR);
    let on_disk = count_files(&staged)
        .map_err(|error| format!("the staged tree could not be walked: {error}"))?;
    if on_disk != manifest.files.len() {
        return Err(format!(
            "staged tree verification failed: the manifest names {} files and the tree holds \
             {on_disk}",
            manifest.files.len()
        ));
    }

    for file in &manifest.files {
        let path = staged.join(Path::new(&file.path));
        let bytes = fs::read(&path)
            .map_err(|error| format!("staged file {} could not be read: {error}", file.path))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != file.size {
            return Err(format!(
                "staged file {} verification failed: the manifest says {} bytes, the tree holds {}",
                file.path,
                file.size,
                bytes.len()
            ));
        }
        let digest = blake3::hash(&bytes).to_hex().to_string();
        if digest != file.blake3.to_lowercase() {
            return Err(format!(
                "staged file {} verification failed: manifest digest {}, tree digest {digest}",
                file.path, file.blake3
            ));
        }
        // The mode is part of what a tree must be, and a digest does not
        // cover it. A staged tree restored by a tool that drops modes hashes
        // as pristine and then cannot exec — so the bit is re-applied here,
        // before the swap, rather than trusted from extraction time.
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|error| {
                format!(
                    "staged file {} could not be made executable: {error}",
                    file.path
                )
            })?;
        }
    }
    Ok(manifest)
}

/// How many files sit under `dir`, at any depth.
fn count_files(dir: &Path) -> std::io::Result<usize> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += count_files(&entry.path())?;
        } else {
            total += 1;
        }
    }
    Ok(total)
}

/// The swap: previous is set aside, current becomes previous, staged becomes
/// current. Renames only, and the rollback tree is not destroyed until the
/// swap it is insurance against has actually landed.
///
/// A failure at the last rename rolls the first two back, so the caller's
/// live tree is the one it had. Only then is the set-aside tree deleted.
fn swap(root: &Path) -> Result<(), String> {
    let previous = root.join(PREVIOUS_DIR);
    let current = root.join(CURRENT_DIR);
    let staged = root.join(STAGED_DIR);

    let aside = if previous.exists() {
        let aside = root.join(scratch_name("previous.trash-"));
        fs::rename(&previous, &aside)
            .map_err(|error| format!("the prior rollback tree could not be set aside: {error}"))?;
        Some(aside)
    } else {
        None
    };

    if let Err(error) = fs::rename(&current, &previous) {
        if let Some(aside) = aside {
            let _ = fs::rename(&aside, &previous);
        }
        return Err(format!(
            "the live tree could not be moved aside, and nothing changed: {error}"
        ));
    }

    if let Err(error) = fs::rename(&staged, &current) {
        // Put the live tree back exactly as it was, in reverse order, so a
        // failed swap costs a launch delay and nothing else.
        let _ = fs::rename(&previous, &current);
        if let Some(aside) = aside {
            let _ = fs::rename(&aside, &previous);
        }
        return Err(format!(
            "the staged tree could not be moved into place, and the live tree was restored: \
             {error}"
        ));
    }

    // Past this line the swap has landed. Neither cleanup below may fail it:
    // a manifest a scanner holds open, or a trash tree that will not delete,
    // are things to retry next launch — reporting them as a failed swap
    // would turn a completed update into a permanent false refusal, which is
    // the defect this ordering exists to prevent.
    if fs::remove_file(root.join(STAGE_MANIFEST)).is_err() {
        say(
            root,
            "the consumed stage manifest could not be removed; it will be cleared at the next \
             launch",
        );
    }
    if let Some(aside) = aside {
        let _ = fs::remove_dir_all(&aside);
    }
    Ok(())
}

/// Handle a root with no live tree — the signature of a swap interrupted
/// between renames, or of a rollback tree that is all that is left. A
/// verified staged tree finishes the swap it started; a kept previous tree
/// comes back otherwise. Both are said.
fn recover(root: &Path) -> Outcome {
    if root.join(STAGE_MANIFEST).is_file() && root.join(STAGED_DIR).is_dir() {
        match verify_staged(root) {
            Ok(manifest) => {
                if let Err(error) = fs::rename(root.join(STAGED_DIR), root.join(CURRENT_DIR)) {
                    let reason = format!("an interrupted swap could not be completed: {error}");
                    say(root, &reason);
                    return Outcome::Refused { reason };
                }
                let _ = fs::remove_file(root.join(STAGE_MANIFEST));
                let how = format!(
                    "an interrupted swap was completed: the staged {} tree is now current",
                    manifest.version
                );
                say(root, &how);
                return Outcome::Recovered { how };
            }
            Err(reason) => say(root, &reason),
        }
    }
    if root.join(PREVIOUS_DIR).is_dir() {
        match fs::rename(root.join(PREVIOUS_DIR), root.join(CURRENT_DIR)) {
            Ok(()) => {
                let how = "no live tree was present; the previous tree was restored".to_string();
                say(root, &how);
                return Outcome::Recovered { how };
            }
            Err(error) => {
                let reason = format!("the previous tree could not be restored: {error}");
                say(root, &reason);
                return Outcome::Refused { reason };
            }
        }
    }
    let reason = "no live tree, no verified staged tree, and no previous tree: \
                  this installation cannot start and needs a reinstall"
        .to_string();
    say(root, &reason);
    Outcome::Refused { reason }
}

/// Apply the staged tree if one is waiting and every gate passes.
///
/// Taking a [`Claim`] by reference rather than a path alone is the type
/// saying what the invariant requires: nothing applies except under the
/// claim this installation is held by.
pub fn apply(root: &Path, _claim: &Claim) -> Outcome {
    if !root.join(CURRENT_DIR).is_dir() {
        return recover(root);
    }

    if !root.join(STAGE_MANIFEST).is_file() {
        return Outcome::NothingStaged;
    }

    // A manifest whose tree is gone is the residue of a swap whose last
    // cleanup lost a race. Clearing it is the repair; leaving it would make
    // every future launch report a refusal about a stage that does not
    // exist, burying the real ones.
    if !root.join(STAGED_DIR).is_dir() {
        let _ = fs::remove_file(root.join(STAGE_MANIFEST));
        let how = "a stage manifest without its tree was cleared".to_string();
        say(root, &how);
        return Outcome::Recovered { how };
    }

    // A translocated install cannot be updated in place. Tested here —
    // after we know there is something to apply — so an ordinary launch of a
    // translocated install stays silent about an update it is not
    // preventing, and says it only when it actually costs something.
    if std::env::current_exe()
        .map(|exe| exe.to_string_lossy().contains("/AppTranslocation/"))
        .unwrap_or(false)
    {
        let reason = "this install is running translocated (macOS Gatekeeper), so the staged \
                      release cannot be applied; move it to Applications and launch again"
            .to_string();
        say(root, &reason);
        return Outcome::Refused { reason };
    }

    // Prove the root is writable before touching anything: a refusal here
    // must name the staging path, not surface later as a half-swap.
    let probe = root.join(scratch_name("staged.tmp-probe"));
    if let Err(error) = fs::write(&probe, b"probe").and_then(|()| fs::remove_file(&probe)) {
        let reason = format!("the install root is not writable, so nothing applies: {error}");
        say(root, &reason);
        return Outcome::Refused { reason };
    }

    // The staging lock excludes the stager, which writes `staged/` from the
    // daemon while a client is alive. Held across verify *and* swap: a stage
    // replaced between the two would put a tree into `current` that nothing
    // verified in this run.
    let staging = match lock_file(&root.join(STAGING_LOCK)) {
        Ok(file) => file,
        Err(error) => {
            let reason = format!("the staging lock could not be opened: {error}");
            say(root, &reason);
            return Outcome::Refused { reason };
        }
    };
    if staging.try_lock_exclusive().is_err() {
        let reason =
            "a release is being staged right now; it applies at the next launch".to_string();
        say(root, &reason);
        return Outcome::Refused { reason };
    }

    let outcome = match verify_staged(root) {
        // The staged tree stays where it is on a refusal: staging is
        // reversible and the stager owns re-staging. The refusal is the
        // deliverable.
        Err(reason) => {
            say(root, &reason);
            Outcome::Refused { reason }
        }
        Ok(manifest) => match swap(root) {
            Ok(()) => {
                say(
                    root,
                    &format!("applied the staged {} tree at launch", manifest.version),
                );
                Outcome::Applied {
                    version: manifest.version,
                }
            }
            Err(reason) => {
                say(root, &reason);
                Outcome::Refused { reason }
            }
        },
    };
    let _ = fs2::FileExt::unlock(&staging);
    outcome
}

/// Start the live tree's entry binary with this process's arguments passed
/// through.
///
/// A live tree that cannot start falls back to the kept previous tree, and
/// says so. That fallback is what makes "keep current plus previous" a
/// rollback rather than an archive: a release that verifies byte for byte
/// and still will not exec would otherwise leave the installation unable to
/// start on every future launch, with the good tree sitting unused beside
/// it.
pub fn launch(root: &Path, args: &[std::ffi::OsString]) -> std::io::Result<std::process::Child> {
    let entry = root.join(CURRENT_DIR).join(entry_name());
    match std::process::Command::new(&entry).args(args).spawn() {
        Ok(child) => Ok(child),
        Err(error) => {
            let fallback = root.join(PREVIOUS_DIR).join(entry_name());
            if !fallback.is_file() {
                return Err(error);
            }
            say(
                root,
                &format!(
                    "the live tree could not be started ({error}); falling back to the previous \
                     tree"
                ),
            );
            std::process::Command::new(&fallback).args(args).spawn()
        }
    }
}

/// The install root: the directory holding the stub itself.
pub fn discover_root() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| std::io::Error::other("the stub executable has no parent directory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_with_current() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("a scratch install root");
        fs::create_dir(root.path().join(CURRENT_DIR)).expect("a live tree");
        fs::write(root.path().join(CURRENT_DIR).join(entry_name()), b"live").expect("a live entry");
        root
    }

    fn stage(root: &Path, version: &str, files: &[(&str, &[u8])]) {
        let staged = root.join(STAGED_DIR);
        fs::create_dir_all(&staged).expect("a staged tree");
        let mut named = Vec::new();
        for (path, bytes) in files {
            let at = staged.join(path);
            if let Some(parent) = at.parent() {
                fs::create_dir_all(parent).expect("a staged subdirectory");
            }
            fs::write(&at, bytes).expect("a staged file");
            named.push(StagedFile {
                path: (*path).to_string(),
                blake3: blake3::hash(bytes).to_hex().to_string(),
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                executable: *path == entry_name(),
            });
        }
        let manifest = StageManifest {
            version: version.to_string(),
            entry: entry_name().to_string(),
            files: named,
        };
        fs::write(
            root.join(STAGE_MANIFEST),
            serde_json::to_vec(&manifest).expect("a manifest encodes"),
        )
        .expect("a stage manifest");
    }

    /// Claim an installation the way `main` does.
    fn claimed(root: &Path) -> Claim {
        claim(root)
            .expect("the claim file opens")
            .expect("nothing else holds this installation")
    }

    #[test]
    fn nothing_staged_is_the_ordinary_launch_and_touches_nothing() {
        let root = root_with_current();
        let claim = claimed(root.path());
        assert_eq!(apply(root.path(), &claim), Outcome::NothingStaged);
        assert!(
            !root.path().join(STUB_LOG).exists(),
            "an ordinary launch wrote a log where nothing happened"
        );
    }

    #[test]
    fn a_verified_stage_is_applied_and_the_previous_tree_is_kept() {
        let root = root_with_current();
        stage(
            root.path(),
            "0.0.2",
            &[(entry_name(), b"new"), ("data/asset", b"a")],
        );
        let claim = claimed(root.path());
        assert_eq!(
            apply(root.path(), &claim),
            Outcome::Applied {
                version: "0.0.2".into()
            }
        );
        assert_eq!(
            fs::read(root.path().join(CURRENT_DIR).join(entry_name())).expect("the new live entry"),
            b"new",
            "the staged tree did not become current"
        );
        assert_eq!(
            fs::read(root.path().join(PREVIOUS_DIR).join(entry_name())).expect("the kept entry"),
            b"live",
            "the prior live tree was not kept as previous"
        );
        assert!(
            !root.path().join(STAGE_MANIFEST).exists(),
            "a consumed stage manifest was left behind"
        );
    }

    #[test]
    fn a_tampered_stage_is_refused_by_name_and_the_live_tree_is_untouched() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[(entry_name(), b"new")]);
        fs::write(root.path().join(STAGED_DIR).join(entry_name()), b"evil").expect("the tamper");
        let claim = claimed(root.path());
        let Outcome::Refused { reason } = apply(root.path(), &claim) else {
            panic!("a tampered stage was not refused");
        };
        assert!(
            reason.contains("verification failed"),
            "the refusal must name verification, not fail vaguely: {reason}"
        );
        assert_eq!(
            fs::read(root.path().join(CURRENT_DIR).join(entry_name())).expect("the live entry"),
            b"live",
            "a refused stage changed the live tree"
        );
    }

    /// The digest is the check that survives an attacker who can pad. Every
    /// other tamper fixture in this suite changes a length, so without this
    /// one the size gate would answer them all and the digest comparison —
    /// the thing verify-before-swap exists for — would be proven nowhere.
    #[test]
    fn a_same_length_tamper_is_caught_by_the_digest_rather_than_the_size() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[(entry_name(), b"as built")]);
        fs::write(root.path().join(STAGED_DIR).join(entry_name()), b"backdoor")
            .expect("the equal-length tamper");
        let claim = claimed(root.path());
        let Outcome::Refused { reason } = apply(root.path(), &claim) else {
            panic!("an equal-length tamper was not refused");
        };
        assert!(
            reason.contains("manifest digest"),
            "the refusal must name the digest, not the size: {reason}"
        );
    }

    #[test]
    fn an_extra_file_in_the_staged_tree_is_a_verification_failure() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[(entry_name(), b"new")]);
        fs::write(root.path().join(STAGED_DIR).join("stowaway"), b"?").expect("the extra file");
        let claim = claimed(root.path());
        let Outcome::Refused { reason } = apply(root.path(), &claim) else {
            panic!("an unmanifested file was not refused");
        };
        assert!(reason.contains("verification failed"), "{reason}");
    }

    #[test]
    fn a_manifest_that_does_not_carry_its_own_entry_is_refused() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[("data/asset", b"a")]);
        let claim = claimed(root.path());
        let Outcome::Refused { reason } = apply(root.path(), &claim) else {
            panic!("a tree without its entry was not refused");
        };
        assert!(
            reason.contains("as its entry and does not carry it"),
            "{reason}"
        );
    }

    #[test]
    fn a_claim_is_exclusive_and_a_second_launch_defers_rather_than_applying() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[(entry_name(), b"new")]);
        let held = claimed(root.path());
        assert!(
            claim(root.path()).expect("the claim file opens").is_none(),
            "a second stub claimed an installation a live client holds"
        );
        drop(held);
        let claim = claimed(root.path());
        assert_eq!(
            apply(root.path(), &claim),
            Outcome::Applied {
                version: "0.0.2".into()
            },
            "the deferred stage did not apply once the claim was free"
        );
    }

    #[test]
    fn a_stage_being_written_right_now_is_left_alone() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[(entry_name(), b"new")]);
        let stager = lock_file(&root.path().join(STAGING_LOCK)).expect("the staging lock file");
        stager.try_lock_exclusive().expect("the stager's lock");
        let claim = claimed(root.path());
        let Outcome::Refused { reason } = apply(root.path(), &claim) else {
            panic!("a swap raced an in-flight staging");
        };
        assert!(reason.contains("being staged right now"), "{reason}");
        assert_eq!(
            fs::read(root.path().join(CURRENT_DIR).join(entry_name())).expect("the live entry"),
            b"live"
        );
    }

    #[test]
    fn an_interrupted_swap_is_finished_at_the_next_launch() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[(entry_name(), b"new")]);
        // The interruption: current was renamed away, staged was not renamed
        // in. Exactly the window between swap()'s second and third renames.
        fs::rename(
            root.path().join(CURRENT_DIR),
            root.path().join(PREVIOUS_DIR),
        )
        .expect("the interrupted rename");
        let claim = claimed(root.path());
        let Outcome::Recovered { how } = apply(root.path(), &claim) else {
            panic!("an interrupted swap was not recovered");
        };
        assert!(how.contains("interrupted swap was completed"), "{how}");
        assert_eq!(
            fs::read(root.path().join(CURRENT_DIR).join(entry_name()))
                .expect("the recovered live entry"),
            b"new"
        );
    }

    #[test]
    fn a_missing_live_tree_with_no_stage_restores_the_previous_tree() {
        let root = tempfile::tempdir().expect("a scratch install root");
        fs::create_dir(root.path().join(PREVIOUS_DIR)).expect("a previous tree");
        fs::write(root.path().join(PREVIOUS_DIR).join(entry_name()), b"old")
            .expect("a previous entry");
        let claim = claimed(root.path());
        let Outcome::Recovered { how } = apply(root.path(), &claim) else {
            panic!("a rootless install was not recovered from previous");
        };
        assert!(how.contains("previous tree was restored"), "{how}");
        assert_eq!(
            fs::read(root.path().join(CURRENT_DIR).join(entry_name())).expect("the restored entry"),
            b"old"
        );
    }

    /// The residue of a swap whose last cleanup lost a race. Left alone it
    /// would make every launch forever report a refusal about a stage that
    /// does not exist — a standing false alarm that buries the real ones.
    #[test]
    fn a_stage_manifest_without_its_tree_is_cleared_rather_than_refused_forever() {
        let root = root_with_current();
        stage(root.path(), "0.0.2", &[(entry_name(), b"new")]);
        fs::remove_dir_all(root.path().join(STAGED_DIR)).expect("the lost tree");
        let claim = claimed(root.path());
        let Outcome::Recovered { how } = apply(root.path(), &claim) else {
            panic!("an orphaned manifest was not cleared");
        };
        assert!(how.contains("without its tree was cleared"), "{how}");
        assert!(
            !root.path().join(STAGE_MANIFEST).exists(),
            "the orphaned manifest survived its own repair"
        );
        assert_eq!(
            apply(root.path(), &claim),
            Outcome::NothingStaged,
            "the repair did not settle into an ordinary launch"
        );
    }

    /// Scratch a previous run could not delete is swept at the moment this
    /// process knows it is alone — never left to accumulate whole client
    /// trees, and never left to collide with a recycled pid.
    #[test]
    fn stale_scratch_is_swept_when_the_installation_is_claimed() {
        let root = root_with_current();
        let orphan = root.path().join("previous.trash-4816-0");
        fs::create_dir(&orphan).expect("an orphaned trash tree");
        fs::write(orphan.join("junk"), b"x").expect("its contents");
        let scratch = root.path().join("staged.tmp-4816-0");
        fs::create_dir(&scratch).expect("an orphaned staging scratch");
        let _claim = claimed(root.path());
        assert!(
            !orphan.exists(),
            "an orphaned trash tree survived the sweep"
        );
        assert!(!scratch.exists(), "an orphaned scratch survived the sweep");
    }

    /// The rollback tree is insurance, and insurance cancelled before the
    /// risk has passed is not insurance. A swap that cannot complete must
    /// leave the installation with both trees it started with.
    #[test]
    fn a_swap_that_cannot_complete_restores_the_live_tree_and_keeps_the_rollback() {
        let root = root_with_current();
        fs::create_dir(root.path().join(PREVIOUS_DIR)).expect("a rollback tree");
        fs::write(root.path().join(PREVIOUS_DIR).join(entry_name()), b"older").expect("its entry");
        stage(root.path(), "0.0.2", &[(entry_name(), b"new")]);

        // The staged tree is verified, then removed before the swap reaches
        // its last rename — the shape of a scanner or a stager taking it
        // away mid-swap. The installation must be exactly as it was.
        let claim = claimed(root.path());
        let manifest = verify_staged(root.path()).expect("the stage verifies");
        assert_eq!(manifest.version, "0.0.2");
        fs::remove_dir_all(root.path().join(STAGED_DIR)).expect("the vanishing stage");
        let failure = swap(root.path()).expect_err("a swap without its stage must fail");
        assert!(failure.contains("the live tree was restored"), "{failure}");

        assert_eq!(
            fs::read(root.path().join(CURRENT_DIR).join(entry_name())).expect("the live entry"),
            b"live",
            "a failed swap did not restore the live tree"
        );
        assert_eq!(
            fs::read(root.path().join(PREVIOUS_DIR).join(entry_name())).expect("the kept entry"),
            b"older",
            "a failed swap destroyed the rollback tree it never replaced"
        );
        drop(claim);
    }

    /// A release that verifies byte for byte and still will not start is the
    /// one failure the digest cannot catch. The kept tree is what makes it
    /// survivable, and only if something actually reaches for it.
    #[test]
    fn a_live_tree_that_cannot_start_falls_back_to_the_previous_tree() {
        let root = tempfile::tempdir().expect("a scratch install root");
        fs::create_dir(root.path().join(CURRENT_DIR)).expect("a live tree");
        // No entry binary at all: the shape of a tree that cannot exec,
        // without needing a real broken executable to make the point.
        fs::create_dir(root.path().join(PREVIOUS_DIR)).expect("a rollback tree");
        let fallback = root.path().join(PREVIOUS_DIR).join(entry_name());
        std::fs::copy(
            std::env::current_exe().expect("this test binary"),
            &fallback,
        )
        .expect("a bootable previous entry");

        let mut child = launch(root.path(), &[std::ffi::OsString::from("--list")])
            .expect("the fallback tree started");
        let _ = child.kill();
        let _ = child.wait();
        let log = fs::read_to_string(root.path().join(STUB_LOG)).expect("the fallback was said");
        assert!(log.contains("falling back to the previous tree"), "{log}");
    }
}
