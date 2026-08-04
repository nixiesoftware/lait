//! Replacing the installed binary with the latest published release.
//!
//! Reached as [`crate::control::Request::HostUpdate`], so it runs in the
//! identity's daemon — the process that knows which build it is. Pure Rust
//! (`ureq` + rustls, gzip/zip extraction, atomic self-replace); no external
//! updater binary, and nothing here shells out.
//!
//! The daemon does not stop itself first. `self_update` replaces a live
//! executable by *renaming* it out of the way rather than writing over it, so
//! the swap lands while this process keeps running the old code — which is why
//! the reply says a restart is what makes it take effect.

use anyhow::{anyhow, Result};

/// What a self-update did.
pub struct Updated {
    /// The version this node was running.
    pub from: String,
    /// The version now on disk.
    pub to: String,
    /// False when this node was already on the latest release.
    pub replaced: bool,
}

/// Fetch the newest `nixiesoftware/lait` release for this platform and replace
/// the running executable with it.
///
/// Blocking (HTTP + archive extract + file swap), so callers on a reactor must
/// hand it to `spawn_blocking`.
pub fn run() -> Result<Updated> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner("nixiesoftware")
        .repo_name("lait")
        .bin_name("lait")
        .bin_path_in_archive(bin_path_in_archive())
        // Clean-semver version (build.rs): a dev build reports `X.Y.Z-dev.<sha>`,
        // which sorts below stable `X.Y.Z`, so an update heals a dev node onto
        // the stable release instead of seeing "already up to date".
        .current_version(env!("LAIT_VERSION_SEMVER"))
        // Nobody is watching this process's stdout: it is a daemon, and its
        // stdout is the null device.
        .show_download_progress(false)
        .no_confirm(true)
        .build()
        .and_then(|updater| updater.update())
        .map_err(|error| anyhow!("self-update failed: {error}"))?;
    Ok(Updated {
        from: env!("LAIT_VERSION_SEMVER").to_string(),
        to: status.version().to_string(),
        replaced: status.updated(),
    })
}

/// The in-archive path to the `lait` binary for `self_update`, matching
/// cargo-dist's **per-OS** release layout: the unix `.tar.gz` archives nest
/// everything under a `lait-<target-triple>/` directory, while the Windows
/// `.zip` is flat with `lait.exe` at the archive root.
///
/// `{{ target }}` / `{{ bin }}` are expanded by self_update. The trap is
/// `{{ bin }}`: it expands to `bin_name` *after* the crate has appended
/// `EXE_SUFFIX` (see `Update::bin_name`), so it is already `lait.exe` on
/// Windows. Spelling the suffix again yields `lait.exe.exe` and extraction
/// fails with "specified file not found in archive" — which is exactly what
/// shipped in v0.4.8/v0.5.0. Getting it wrong is invisible on the host that
/// builds the release and fatal on the host that runs it.
///
/// Takes `target` rather than reading `#[cfg(windows)]` so that **every**
/// platform's answer is computable from any host — a `cfg` split can only ever
/// be tested on the platform it selects, which is why the Windows arm went
/// unexercised through two releases. The real caller passes
/// [`self_update::get_target`], the same string self_update substitutes for
/// `{{ target }}`, so there is no skew between what we plan and what it does.
fn bin_path_in_archive_for(target: &str) -> &'static str {
    if target.contains("-windows-") {
        // flat zip; `{{ bin }}` already carries `.exe`.
        "{{ bin }}"
    } else {
        "lait-{{ target }}/{{ bin }}"
    }
}

/// The in-archive binary path for the target this binary was built for.
fn bin_path_in_archive() -> &'static str {
    bin_path_in_archive_for(self_update::get_target())
}

#[cfg(test)]
mod tests {
    use super::bin_path_in_archive_for;

    #[test]
    fn updater_version_is_clean_semver() {
        // The self-updater compares `current_version` as semver, so the string it
        // gets (LAIT_VERSION_SEMVER) must be valid semver — never the ` (<date>)`
        // form of LAIT_VERSION_LONG. In a non-dev build (no LAIT_BUILD_SHA, as in
        // CI/test) it equals the crate version exactly; a dev build appends a
        // `-dev.<sha>` prerelease that sorts below stable.
        let v = env!("LAIT_VERSION_SEMVER");
        assert!(
            !v.contains(' ') && !v.contains('('),
            "updater version must be valid semver, got {v:?}"
        );
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
    }

    /// Expand a `bin_path_in_archive` template the way self_update does, for an
    /// arbitrary target — mirroring `Update::bin_name` (which appends
    /// `EXE_SUFFIX` to the configured name) and `update.rs`'s `{{ var }}`
    /// substitution. Modelling it here is what lets a single host check the
    /// answer for platforms it isn't running on.
    fn expand(target: &str) -> String {
        let exe_suffix = if target.contains("-windows-") {
            ".exe"
        } else {
            ""
        };
        let bin_name = format!("{}{}", "lait".trim_end_matches(exe_suffix), exe_suffix);
        bin_path_in_archive_for(target)
            .replace("{{ bin }}", &bin_name)
            .replace("{{ target }}", target)
    }

    #[test]
    fn update_bin_path_matches_the_published_archive_layout_for_every_target() {
        // Ground truth, read off the real v0.5.0 release artifacts and their
        // dist-manifest.json: the unix `.tar.gz` archives nest everything under
        // `lait-<target>/`, and the Windows `.zip` is FLAT with `lait.exe` at the
        // root. Assert the EXPANDED path — the old test pinned the template
        // string verbatim, so it merely restated the code and cheerfully asserted
        // `{{ bin }}.exe`, letting the `lait.exe.exe` bug ship twice.
        //
        // Every shipped target is checked from any host: `bin_path_in_archive_for`
        // takes the target instead of branching on `#[cfg(windows)]`, so the
        // Windows arm is no longer invisible to CI's Linux runners.
        assert_eq!(expand("x86_64-pc-windows-msvc"), "lait.exe");
        for target in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
        ] {
            assert_eq!(expand(target), format!("lait-{target}/lait"));
        }
    }

    #[test]
    fn update_bin_path_never_doubles_the_exe_suffix() {
        // The exact v0.4.8/v0.5.0 defect, named: `{{ bin }}` already carries
        // `EXE_SUFFIX`, so any template that also spells `.exe` produces
        // `lait.exe.exe` and every Windows self-update dies on extraction.
        let win = expand("x86_64-pc-windows-msvc");
        assert!(!win.contains(".exe.exe"), "doubled EXE_SUFFIX: {win}");
        assert_eq!(win.matches(".exe").count(), 1, "expected one `.exe`: {win}");
    }

    /// Every target cargo-dist ships, and the archive extension it ships it as.
    const RELEASE_TARGETS: &[(&str, &str)] = &[
        ("x86_64-pc-windows-msvc", "zip"),
        ("aarch64-apple-darwin", "tar.gz"),
        ("x86_64-apple-darwin", "tar.gz"),
        ("aarch64-unknown-linux-gnu", "tar.gz"),
        ("x86_64-unknown-linux-gnu", "tar.gz"),
    ];

    /// The paths inside a real release archive.
    fn entries(archive: &std::path::Path, ext: &str) -> Vec<String> {
        let f = std::fs::File::open(archive).unwrap_or_else(|e| panic!("open {archive:?}: {e}"));
        if ext == "zip" {
            let mut z = zip::ZipArchive::new(f).expect("read zip");
            (0..z.len())
                .map(|i| z.by_index(i).unwrap().name().to_string())
                .collect()
        } else {
            let mut t = tar::Archive::new(flate2::read::GzDecoder::new(f));
            t.entries()
                .expect("read tar")
                .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
                .collect()
        }
    }

    /// The check the unit tests above structurally cannot make: that our path is
    /// really in the archive users download. Everything else in this file models
    /// cargo-dist's layout and self_update's substitution — and a model is exactly
    /// what shipped `lait.exe.exe` twice. This one reads the bytes.
    ///
    /// `#[ignore]` because it needs the archives on disk; CI's `updater-contract`
    /// job fetches the latest release into `$LAIT_RELEASE_ARCHIVES` and runs it
    /// with `--ignored`. Missing archives fail loudly rather than skipping —
    /// a check that silently passes when its input is absent is worse than none.
    #[test]
    #[ignore = "needs $LAIT_RELEASE_ARCHIVES; run in CI's updater-contract job"]
    fn update_bin_path_is_a_real_entry_in_the_published_archives() {
        let dir = std::env::var("LAIT_RELEASE_ARCHIVES")
            .expect("set $LAIT_RELEASE_ARCHIVES to a dir of downloaded lait-<target>.{zip,tar.gz}");
        let dir = std::path::Path::new(&dir);
        for (target, ext) in RELEASE_TARGETS {
            let archive = dir.join(format!("lait-{target}.{ext}"));
            assert!(archive.is_file(), "missing release archive {archive:?}");
            let want = expand(target);
            let found = entries(&archive, ext);
            assert!(
                found.contains(&want),
                "self_update would extract {want:?} from lait-{target}.{ext}, \
                 but that archive contains: {found:?}"
            );
        }
    }
}
