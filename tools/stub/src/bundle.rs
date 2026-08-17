//! Replacing a macOS application bundle (CLIENT-65).
//!
//! macOS is the one platform where the stub layout inverts. A `.app` is what
//! Launch Services, the Dock, TCC and the person all key on, so it must stay
//! the real application at a stable path rather than becoming a launcher that
//! redirects into versioned trees. The unit of replacement is therefore the
//! bundle itself — the same conclusion Sparkle and Squirrel.Mac reached.
//!
//! ## A running program survives the exchange — measured, after a false alarm
//!
//! The rule is that a replacement must produce a *new inode* at the
//! destination. Overwriting a running binary's bytes in place kills it with
//! SIGKILL on the next page-in; exchanging the directory that contains it
//! leaves the process reading the image it opened, alive until it exits on
//! its own terms. The test below measures exactly that.
//!
//! It is worth recording how nearly this module documented the opposite. The
//! first subject was a copy of `/bin/sleep`, which is killed within a few
//! hundred milliseconds *on its own*, untouched, because a copied platform
//! binary fails code-signing validation on macOS. Three experiments all
//! reported SIGKILL, all of them measuring the instrument rather than the
//! exchange, and the conclusion drawn from them — that nothing inside a
//! bundle can survive its replacement, so the daemon could never perform the
//! swap — was wrong in a way that would have changed the design. What caught
//! it was a control: a second copy, left entirely alone, that died too.
//!
//! So the subject here is `sleeper`, a binary this workspace builds. Any
//! measurement of this kind needs one, and needs a control beside it.
//!
//! ## `renamex_np` with `RENAME_SWAP`, and what to do when it is refused
//!
//! One syscall exchanges two paths atomically, so there is no instant at which
//! the application is absent. That matters beyond tidiness: the Dock observes
//! a bundle vanish, orphans its tiles, and starts treating each version as a
//! new application — a real, reported consequence of non-atomic replacement.
//!
//! It is same-volume only and filesystem-conditional, so both are checked
//! rather than assumed. `/Applications` and a home directory are one APFS
//! volume on a stock machine, and are not when somebody keeps applications on
//! an external disk — which is ordinary, not exotic.

use std::path::Path;

/// What a bundle replacement did, or why it did not.
#[derive(Debug, PartialEq, Eq)]
pub enum Replaced {
    /// The two paths were exchanged atomically. The old bundle now sits where
    /// the replacement was staged, which is what makes it the rollback.
    Swapped,
    /// The paths are on different volumes, or the filesystem does not offer
    /// the exchange. Nothing was changed.
    Unsupported {
        /// What the attempt reported, in the words the caller should say.
        why: String,
    },
}

/// Exchange `live` and `staged` atomically.
///
/// On success the application that was live is at `staged` and the
/// replacement is at `live` — an exchange rather than a move, so the previous
/// bundle is preserved by the same operation that installs the new one and no
/// separate copy step can fail halfway.
///
/// Both paths must exist and sit on one volume. A caller that gets
/// [`Replaced::Unsupported`] has an install it cannot swap in place; that is a
/// condition to say, never to force.
#[cfg(target_os = "macos")]
pub fn exchange(live: &Path, staged: &Path) -> std::io::Result<Replaced> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    // `RENAME_SWAP` is not in libc's constants on every version, and it is a
    // stable kernel ABI value (sys/stdio.h).
    const RENAME_SWAP: libc::c_uint = 0x0000_0002;

    for path in [live, staged] {
        if !path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "{} does not exist, so there is nothing to exchange",
                    path.display()
                ),
            ));
        }
    }
    // Same volume, checked before the syscall so the refusal names the reason
    // a person can act on rather than an errno.
    let (live_dev, staged_dev) = (device_of(live)?, device_of(staged)?);
    if live_dev != staged_dev {
        return Ok(Replaced::Unsupported {
            why: format!(
                "{} and {} are on different volumes, so they cannot be exchanged in place",
                live.display(),
                staged.display()
            ),
        });
    }

    let from = CString::new(live.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path holds a NUL"))?;
    let to = CString::new(staged.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path holds a NUL"))?;
    // SAFETY: both pointers are valid NUL-terminated C strings that outlive
    // the call, and the flag is the documented constant.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), RENAME_SWAP) };
    if result == 0 {
        return Ok(Replaced::Swapped);
    }
    let error = std::io::Error::last_os_error();
    // ENOTSUP is the filesystem saying it does not do this, which is a fact
    // about the disk rather than a failure of the update.
    if error.raw_os_error() == Some(libc::ENOTSUP) {
        return Ok(Replaced::Unsupported {
            why: format!(
                "the filesystem holding {} cannot exchange two paths atomically",
                live.display()
            ),
        });
    }
    Err(error)
}

/// The device a path lives on.
#[cfg(target_os = "macos")]
fn device_of(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(path.metadata()?.dev())
}

/// Not macOS: there is no bundle to exchange, and the stub's tree swap is the
/// mechanism instead.
#[cfg(not(target_os = "macos"))]
pub fn exchange(live: &Path, _staged: &Path) -> std::io::Result<Replaced> {
    Ok(Replaced::Unsupported {
        why: format!(
            "{} is not a macOS application bundle; this platform swaps trees instead",
            live.display()
        ),
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// A binary this build produced, beside the test binary.
    fn built(name: &str) -> Option<std::path::PathBuf> {
        let profile = std::env::current_exe()
            .ok()?
            .parent()?
            .parent()?
            .to_path_buf();
        let candidate = profile.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        });
        candidate.is_file().then_some(candidate)
    }

    fn tree(root: &Path, name: &str, marker: &[u8]) -> std::path::PathBuf {
        let bundle = root.join(name);
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).expect("a bundle shape");
        std::fs::write(bundle.join("Contents/MacOS/marker"), marker).expect("its marker");
        bundle
    }

    #[test]
    fn two_bundles_exchange_places_in_one_operation() {
        let root = tempfile::tempdir().expect("a scratch volume");
        let live = tree(root.path(), "Astrolabe.app", b"the live one");
        let staged = tree(root.path(), "staged.app", b"the new one");

        assert_eq!(
            exchange(&live, &staged).expect("the exchange runs"),
            Replaced::Swapped
        );
        // An exchange, not a move: the replacement is live and the old
        // application is where the replacement was — which is what makes the
        // rollback a product of the same operation rather than a copy that
        // could fail on its own.
        assert_eq!(
            std::fs::read(live.join("Contents/MacOS/marker")).expect("the live marker"),
            b"the new one"
        );
        assert_eq!(
            std::fs::read(staged.join("Contents/MacOS/marker")).expect("the staged marker"),
            b"the live one"
        );
    }

    /// The measurement the design rests on: a program running from inside a
    /// bundle **survives** that bundle being exchanged, because the path it
    /// was launched from gets a new inode rather than new bytes.
    ///
    /// The subject is `sleeper`, built by this workspace. A copy of
    /// `/bin/sleep` is not a valid subject — macOS kills a copied platform
    /// binary on its own within a few hundred milliseconds — and using one
    /// produced three confident, entirely wrong measurements before a control
    /// caught it. The control is here for that reason and stays.
    #[test]
    fn a_program_running_inside_the_bundle_survives_the_exchange() {
        let Some(sleeper) = built("sleeper") else {
            panic!(
                "no sleeper binary beside the test binary; build the workspace bins \
                 (cargo build -p astrolabe-stub) — a skipped measurement is how a \
                 platform rule comes to be trusted while measuring nothing"
            );
        };
        let root = tempfile::tempdir().expect("a scratch volume");
        let live = tree(root.path(), "Astrolabe.app", b"live");
        let staged = tree(root.path(), "staged.app", b"new");

        let inside = live.join("Contents/MacOS/sleeper");
        std::fs::copy(&sleeper, &inside).expect("a program inside the bundle");
        let outside = root.path().join("bystander");
        std::fs::copy(&sleeper, &outside).expect("a program outside it");

        let mut subject = std::process::Command::new(&inside)
            .arg("4")
            .spawn()
            .expect("the program starts from inside the bundle");
        let mut control = std::process::Command::new(&outside)
            .arg("4")
            .spawn()
            .expect("the control starts");
        std::thread::sleep(std::time::Duration::from_millis(400));

        // The control proves the subject's fate is the exchange's doing and
        // not the platform's opinion of copied binaries.
        assert!(
            control.try_wait().expect("the control is polled").is_none(),
            "the control died untouched, so this measures the instrument"
        );
        assert!(
            subject.try_wait().expect("the subject is polled").is_none(),
            "the subject exited before the exchange, so this proves nothing"
        );

        assert_eq!(
            exchange(&live, &staged).expect("the exchange runs"),
            Replaced::Swapped
        );

        let status = subject.wait().expect("the subject is waited on");
        assert!(
            status.success(),
            "a program running inside the bundle did not survive its exchange: {status}"
        );
        let _ = control.kill();
        let _ = control.wait();
    }

    /// The whole macOS apply: a verified staged bundle becomes the live
    /// application, and the outgoing one becomes the rollback by the same
    /// exchange rather than by a copy that could fail on its own.
    #[test]
    fn a_verified_staged_bundle_becomes_live_and_the_old_one_becomes_previous() {
        let root = tempfile::tempdir().expect("a scratch identity");
        let applications = tempfile::tempdir().expect("a scratch /Applications");
        let live = tree(applications.path(), "Astrolabe.app", b"the live one");

        // The staged bundle, as `update::tree` leaves it: `staged/` beside a
        // manifest, holding the .app's own contents.
        let staged = root.path().join(crate::STAGED_DIR);
        std::fs::create_dir_all(staged.join("Contents/MacOS")).expect("a staged bundle");
        let entry = staged.join("Contents/MacOS/marker");
        std::fs::write(&entry, b"the new one").expect("its marker");
        let manifest = crate::StageManifest {
            version: "0.9.0".into(),
            entry: "Contents/MacOS/marker".into(),
            files: vec![crate::StagedFile {
                path: "Contents/MacOS/marker".into(),
                blake3: blake3::hash(b"the new one").to_hex().to_string(),
                size: 11,
                executable: false,
            }],
        };
        std::fs::write(
            root.path().join(crate::STAGE_MANIFEST),
            serde_json::to_vec(&manifest).expect("a manifest encodes"),
        )
        .expect("a stage manifest");

        let claim = crate::claim(root.path())
            .expect("the claim file opens")
            .expect("nothing holds this installation");
        assert_eq!(
            apply_staged(root.path(), &live, &claim),
            crate::Outcome::Applied {
                version: "0.9.0".into()
            }
        );
        assert_eq!(
            std::fs::read(live.join("Contents/MacOS/marker")).expect("the live marker"),
            b"the new one",
            "the staged application did not become live"
        );
        assert_eq!(
            std::fs::read(
                root.path()
                    .join(crate::PREVIOUS_DIR)
                    .join("Contents/MacOS/marker")
            )
            .expect("the kept marker"),
            b"the live one",
            "the outgoing application was not kept as the rollback"
        );
        assert!(
            !root.path().join(crate::STAGE_MANIFEST).exists(),
            "a consumed stage manifest was left behind"
        );
    }

    #[test]
    fn a_tampered_staged_bundle_is_refused_and_the_live_application_is_untouched() {
        let root = tempfile::tempdir().expect("a scratch identity");
        let applications = tempfile::tempdir().expect("a scratch /Applications");
        let live = tree(applications.path(), "Astrolabe.app", b"the live one");

        let staged = root.path().join(crate::STAGED_DIR);
        std::fs::create_dir_all(staged.join("Contents/MacOS")).expect("a staged bundle");
        std::fs::write(staged.join("Contents/MacOS/marker"), b"tampered!!!")
            .expect("the tampered marker");
        let manifest = crate::StageManifest {
            version: "0.9.0".into(),
            entry: "Contents/MacOS/marker".into(),
            files: vec![crate::StagedFile {
                path: "Contents/MacOS/marker".into(),
                blake3: blake3::hash(b"the new one").to_hex().to_string(),
                size: 11,
                executable: false,
            }],
        };
        std::fs::write(
            root.path().join(crate::STAGE_MANIFEST),
            serde_json::to_vec(&manifest).expect("a manifest encodes"),
        )
        .expect("a stage manifest");

        let claim = crate::claim(root.path())
            .expect("the claim file opens")
            .expect("nothing holds this installation");
        let crate::Outcome::Refused { reason } = apply_staged(root.path(), &live, &claim) else {
            panic!("a tampered bundle was applied");
        };
        assert!(reason.contains("verification failed"), "{reason}");
        assert_eq!(
            std::fs::read(live.join("Contents/MacOS/marker")).expect("the live marker"),
            b"the live one",
            "a refused bundle changed the live application"
        );
    }

    /// An application the person moved is a condition to say, not to guess at.
    #[test]
    fn an_application_that_is_not_where_it_was_installed_is_refused_by_name() {
        let root = tempfile::tempdir().expect("a scratch identity");
        let staged = root.path().join(crate::STAGED_DIR);
        std::fs::create_dir_all(staged.join("Contents/MacOS")).expect("a staged bundle");
        std::fs::write(root.path().join(crate::STAGE_MANIFEST), b"{}").expect("a manifest");
        let claim = crate::claim(root.path())
            .expect("the claim file opens")
            .expect("nothing holds this installation");
        let crate::Outcome::Refused { reason } =
            apply_staged(root.path(), &root.path().join("gone.app"), &claim)
        else {
            panic!("an absent application was applied to");
        };
        assert!(reason.contains("was moved"), "{reason}");
    }

    #[test]
    fn an_absent_path_is_an_error_rather_than_a_silent_no_op() {
        let root = tempfile::tempdir().expect("a scratch volume");
        let live = tree(root.path(), "Astrolabe.app", b"live");
        let error = exchange(&live, &root.path().join("nothing.app"))
            .expect_err("exchanging with nothing must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    /// Applications on an external disk are ordinary. The refusal must name
    /// the volumes rather than surface an errno nobody can act on.
    #[test]
    fn paths_on_two_volumes_are_refused_by_name_and_change_nothing() {
        let root = tempfile::tempdir().expect("a scratch volume");
        let live = tree(root.path(), "Astrolabe.app", b"live");
        // /dev is a different device on macOS, and always present.
        let elsewhere = Path::new("/dev/null");
        let Replaced::Unsupported { why } =
            exchange(&live, elsewhere).expect("a cross-volume exchange is not an error")
        else {
            panic!("a cross-volume exchange was performed");
        };
        assert!(why.contains("different volumes"), "{why}");
        assert_eq!(
            std::fs::read(live.join("Contents/MacOS/marker")).expect("the live marker"),
            b"live",
            "a refused exchange changed the live bundle"
        );
    }
}

/// Apply a staged bundle to the live application, when one is staged and
/// nothing holds the installation.
///
/// The tree-swap shape the stub performs on Windows and Linux does not fit
/// here: there is no install root holding `current/`, because the live
/// application *is* `/Applications/Astrolabe.app` and the person put it
/// there. So the staged tree lives beside the identity, the live application
/// stays where it is, and the two are exchanged.
///
/// The exchange leaves the outgoing application where the staged one was, and
/// that is what becomes `previous/` — the rollback is a product of the same
/// syscall rather than a copy that could fail on its own.
///
/// `root` is the directory holding `staged/`, `previous/` and the stage
/// manifest; `live` is the application bundle to replace.
pub fn apply_staged(root: &Path, live: &Path, _claim: &crate::Claim) -> crate::Outcome {
    use crate::Outcome;

    let staged = root.join(crate::STAGED_DIR);
    if !root.join(crate::STAGE_MANIFEST).is_file() {
        return Outcome::NothingStaged;
    }
    if !staged.is_dir() {
        // The residue of an apply whose last cleanup lost a race. Clearing it
        // is the repair; leaving it makes every launch report a refusal about
        // a release that does not exist.
        let _ = std::fs::remove_file(root.join(crate::STAGE_MANIFEST));
        let how = "a stage manifest without its bundle was cleared".to_string();
        crate::say(root, &how);
        return Outcome::Recovered { how };
    }
    if !live.is_dir() {
        let reason = format!(
            "the live application is not at {} — this installation was moved, \
             and an update cannot guess where to",
            live.display()
        );
        crate::say(root, &reason);
        return Outcome::Refused { reason };
    }

    // Verify before swap, exactly as the tree path does: the manifest is the
    // record staging wrote, and a marker is never trusted in place of the
    // bytes it describes.
    let manifest = match crate::verify_staged(root) {
        Ok(manifest) => manifest,
        Err(reason) => {
            crate::say(root, &reason);
            return Outcome::Refused { reason };
        }
    };

    match exchange(live, &staged) {
        Ok(Replaced::Swapped) => {}
        Ok(Replaced::Unsupported { why }) => {
            // Said, never forced. An application on a volume that cannot do
            // this is a fact about the machine, and copying a bundle over
            // itself is exactly the non-atomic replacement that orphans the
            // Dock's tiles.
            crate::say(root, &why);
            return Outcome::Refused { reason: why };
        }
        Err(error) => {
            let reason = format!("the application could not be exchanged: {error}");
            crate::say(root, &reason);
            return Outcome::Refused { reason };
        }
    }

    // Past here the new application is live. Neither cleanup below may report
    // that as a failure: a completed update told as a refusal is the defect
    // the tree path's ordering already exists to prevent.
    let previous = root.join(crate::PREVIOUS_DIR);
    if previous.exists() {
        let aside = root.join(crate::scratch_name("previous.trash-"));
        if std::fs::rename(&previous, &aside).is_ok() {
            let _ = std::fs::remove_dir_all(&aside);
        }
    }
    if std::fs::rename(&staged, &previous).is_err() {
        crate::say(
            root,
            "the outgoing application could not be kept as the rollback; it will be \
             cleared at the next apply",
        );
    }
    if std::fs::remove_file(root.join(crate::STAGE_MANIFEST)).is_err() {
        crate::say(
            root,
            "the consumed stage manifest could not be removed; it will be cleared at \
             the next apply",
        );
    }
    crate::say(
        root,
        &format!("applied the staged {} application", manifest.version),
    );
    Outcome::Applied {
        version: manifest.version,
    }
}
