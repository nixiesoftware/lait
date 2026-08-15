//! Replacing the installed binary with what the release feed points at.
//!
//! Reached as [`crate::control::Request::HostUpdate`], so it runs in the
//! identity's daemon — the process that knows which build it is. Resolution is
//! the signed first-party feed in [`feed`] (never a forge API); the swap is a
//! pure-Rust staged download, digest verification, archive extraction, and an
//! atomic self-replace.
//!
//! The daemon does not stop itself first. `self_replace` renames the live
//! executable out of the way rather than writing over it, so the swap lands
//! while this process keeps running the old code — which is why the reply says
//! a restart is what makes it take effect. Nothing downloaded ever executes
//! before that restart: the updater stages bytes, it never launches them.

pub mod feed;

use anyhow::{anyhow, bail, Context, Result};
use feed::Channel;

/// What a self-update did, and the channel facts it learned doing it.
pub struct Updated {
    /// The version this node was running.
    pub from: String,
    /// The version now on disk.
    pub to: String,
    /// False when this node was already on the channel's release.
    pub replaced: bool,
    /// The channel this node follows.
    pub channel: String,
    /// The newest release the channel points at.
    pub available: Option<String>,
    /// The published compatibility floor, when the release declares a
    /// satisfiable one.
    pub floor: Option<String>,
}

/// Resolve this node's channel and move the installed binary to the release it
/// points at.
///
/// Blocking (HTTP + archive extract + file swap), so callers on a reactor must
/// hand it to `spawn_blocking`.
pub fn run() -> Result<Updated> {
    let channel = Channel::current();
    // Clean-semver version (build.rs): a dev build reports `X.Y.Z-dev.<sha>`,
    // which sorts below stable `X.Y.Z`, so an update heals a dev node onto the
    // published release instead of seeing "already up to date".
    let current = semver::Version::parse(env!("LAIT_VERSION_SEMVER"))
        .context("LAIT_VERSION_SEMVER is not semver")?;

    let resolved = feed::resolve(channel).map_err(|error| anyhow!("{error}"))?;

    let mut updated = Updated {
        from: current.to_string(),
        to: current.to_string(),
        replaced: false,
        channel: channel.as_str().to_string(),
        available: Some(resolved.version.to_string()),
        floor: resolved.floor.as_ref().map(|floor| floor.to_string()),
    };
    // Stage and prove before the swap, so every failure short of the swap
    // leaves the machine exactly as it was.
    let Some(binary) = stage_with(feed::http_fetch, &resolved, &current, env!("LAIT_TARGET"))?
    else {
        return Ok(updated);
    };
    swap_self(&binary)?;

    updated.to = resolved.version.to_string();
    updated.replaced = true;
    Ok(updated)
}

/// Everything an update does before it touches the installed binary: decide
/// whether there is one, choose the artifact for this target, fetch it, prove
/// it byte for byte, and hand back the binary.
///
/// `Ok(None)` means the channel offers nothing newer — the ordinary answer, and
/// deliberately not an error.
///
/// Split out of [`run`] so the whole chain can be driven in a test without
/// replacing an executable. Every link here was already covered on its own —
/// archive layout against the real published archives, both extraction layouts,
/// the doubled-suffix guard — while the *composition* was not, and a chain of
/// correct parts assembled wrongly is the defect this repository has already
/// paid for twice in the client-to-process seam. The fetch is injected for the
/// same reason `feed::resolve_with` injects one.
fn stage_with<F>(
    fetch: F,
    resolved: &feed::Resolved,
    current: &semver::Version,
    target: &str,
) -> Result<Option<Vec<u8>>>
where
    F: Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure>,
{
    if resolved.version <= *current {
        return Ok(None);
    }

    let artifact = resolved
        .manifest
        .artifacts
        .get("lait")
        .and_then(|targets| targets.get(target))
        .ok_or_else(|| {
            anyhow!(
                "release {} carries no lait artifact for {target}",
                resolved.version
            )
        })?;

    // The manifest's size is passed as the fetch ceiling as well as checked
    // after, so a host that answers with a hundred gigabytes is refused while
    // streaming rather than after buffering it.
    let bytes = fetch(&artifact.url, artifact.size)
        .map_err(|error| anyhow!("artifact download: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.size {
        bail!(
            "artifact size mismatch: manifest says {} bytes, got {}",
            artifact.size,
            bytes.len()
        );
    }
    let digest = blake3::hash(&bytes).to_hex().to_string();
    if digest != artifact.blake3.to_lowercase() {
        bail!(
            "artifact digest verification failed for {}: manifest {}, downloaded {digest}",
            artifact.url,
            artifact.blake3
        );
    }

    Ok(Some(extract_binary(&artifact.url, &bytes, target)?))
}

/// Pull the `lait` binary out of a release archive, addressed by
/// [`bin_path_for`] — the layout claim the `updater-contract` CI job checks
/// against the archives users actually download.
// The suffix compare is already case-insensitive: `url` is ascii-lowered on
// entry, which the lint cannot see.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn extract_binary(url: &str, bytes: &[u8], target: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let want = bin_path_for(target);
    let url = url.to_ascii_lowercase();
    if url.ends_with(".zip") {
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("open release zip")?;
        let mut file = archive
            .by_name(&want)
            .with_context(|| format!("{want} not found in release zip"))?;
        let mut binary = Vec::new();
        file.read_to_end(&mut binary).context("read from zip")?;
        Ok(binary)
    } else if url.ends_with(".tar.gz") {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in archive.entries().context("read release tar")? {
            let mut entry = entry.context("tar entry")?;
            if entry
                .path()
                .map(|p| p.to_string_lossy() == want)
                .unwrap_or(false)
            {
                let mut binary = Vec::new();
                entry.read_to_end(&mut binary).context("read from tar")?;
                return Ok(binary);
            }
        }
        bail!("{want} not found in release tar.gz")
    } else {
        bail!("unrecognized archive extension on {url}")
    }
}

/// Write the staged binary beside the running executable (same filesystem, so
/// the final rename is atomic) and swap it in.
fn swap_self(binary: &[u8]) -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let staged = exe.with_file_name(format!("lait-staged-{}.tmp", std::process::id()));
    std::fs::write(&staged, binary)
        .with_context(|| format!("stage binary at {}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .context("mark staged binary executable")?;
    }
    let swapped = self_replace::self_replace(&staged);
    // The staged copy is consumed on success on some platforms and left behind
    // on others; remove it either way, and never at the cost of the swap.
    let _ = std::fs::remove_file(&staged);
    swapped.context("self-replace")?;
    Ok(())
}

/// The in-archive path of the `lait` binary in a published release archive,
/// per cargo-dist's **per-OS** layout: the unix `.tar.gz` archives nest
/// everything under a `lait-<target-triple>/` directory, while the Windows
/// `.zip` is flat with `lait.exe` at the archive root.
///
/// Takes `target` rather than reading `#[cfg(windows)]` so that **every**
/// platform's answer is computable from any host — a `cfg` split can only ever
/// be tested on the platform it selects, which is how the `lait.exe.exe`
/// doubled-suffix bug went unexercised through two releases (v0.4.8, v0.5.0).
fn bin_path_for(target: &str) -> String {
    if target.contains("-windows-") {
        "lait.exe".to_string()
    } else {
        format!("lait-{target}/lait")
    }
}

#[cfg(test)]
mod tests {
    use super::bin_path_for;
    use crate::update::feed::{self, Channel};

    #[test]
    fn updater_version_is_clean_semver() {
        // The updater compares `current_version` as semver, so the string it
        // gets (LAIT_VERSION_SEMVER) must be valid semver — never the ` (<date>)`
        // form of LAIT_VERSION_LONG. In a non-dev build (no LAIT_BUILD_SHA, as in
        // CI/test) it equals the crate version exactly; a dev build appends a
        // `-dev.<sha>` prerelease that sorts below stable.
        let version = env!("LAIT_VERSION_SEMVER");
        assert!(
            semver::Version::parse(version).is_ok(),
            "not semver: {version:?}"
        );
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_build_target_is_a_shipped_release_target_or_at_least_a_triple() {
        // `run` addresses the manifest by LAIT_TARGET (build.rs). A target
        // that is not in the release matrix simply finds no artifact — but an
        // empty or garbled emission would be invisible until a real update.
        let target = env!("LAIT_TARGET");
        assert!(
            target.split('-').count() >= 3,
            "not a target triple: {target:?}"
        );
    }

    #[test]
    fn update_bin_path_matches_the_published_archive_layout_for_every_target() {
        // Ground truth, read off the real v0.5.0 release artifacts and their
        // dist-manifest.json: the unix `.tar.gz` archives nest everything under
        // `lait-<target>/`, and the Windows `.zip` is FLAT with `lait.exe` at
        // the root.
        assert_eq!(bin_path_for("x86_64-pc-windows-msvc"), "lait.exe");
        for target in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
        ] {
            assert_eq!(bin_path_for(target), format!("lait-{target}/lait"));
        }
    }

    #[test]
    fn update_bin_path_never_doubles_the_exe_suffix() {
        // The exact v0.4.8/v0.5.0 defect, named: the old self_update template
        // expanded `{{ bin }}` with EXE_SUFFIX already applied, so any template
        // that also spelled `.exe` produced `lait.exe.exe` and every Windows
        // self-update died on extraction. The template machinery is gone;
        // the guard stays, because the failure mode is invisible on the host
        // that builds the release and fatal on the host that runs it.
        let win = bin_path_for("x86_64-pc-windows-msvc");
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

    /// The check the unit tests above structurally cannot make: that our path
    /// is really in the archives users download. Everything else here models
    /// the layout — and a model is exactly what shipped `lait.exe.exe` twice.
    /// This one reads the bytes.
    ///
    /// `#[ignore]` because it needs the archives on disk; CI's
    /// `updater-contract` job fetches the latest release into
    /// `$LAIT_RELEASE_ARCHIVES` and runs it with `--ignored`. Missing archives
    /// fail loudly rather than skipping — a check that silently passes when
    /// its input is absent is worse than none. Once the feed carries its first
    /// published release, the job fetches from the feed instead of the GitHub
    /// mirror; the assertion is the same either way.
    #[test]
    #[ignore = "needs $LAIT_RELEASE_ARCHIVES; run in CI's updater-contract job"]
    fn update_bin_path_is_a_real_entry_in_the_published_archives() {
        let dir = std::env::var("LAIT_RELEASE_ARCHIVES")
            .expect("set $LAIT_RELEASE_ARCHIVES to a dir of downloaded lait-<target>.{zip,tar.gz}");
        let dir = std::path::Path::new(&dir);
        for (target, ext) in RELEASE_TARGETS {
            let archive = dir.join(format!("lait-{target}.{ext}"));
            assert!(archive.is_file(), "missing release archive {archive:?}");
            let want = bin_path_for(target);
            let found = entries(&archive, ext);
            assert!(
                found.contains(&want),
                "the updater would extract {want:?} from lait-{target}.{ext}, \
                 but that archive contains: {found:?}"
            );
        }
    }

    /// A release archive shaped like the real Windows one: flat, `lait.exe` at
    /// the root, carrying `binary`.
    fn windows_release_zip(binary: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            writer
                .start_file::<_, ()>("lait.exe", zip::write::FileOptions::default())
                .unwrap();
            writer.write_all(binary).unwrap();
            writer.finish().unwrap();
        }
        bytes
    }

    /// Seal a whole feed — pointer and manifest — naming one artifact, and hand
    /// back the url map plus the verifying key.
    fn sealed_feed(
        version: &str,
        artifact_url: &str,
        archive: &[u8],
        size_claim: u64,
        digest_claim: &str,
    ) -> (std::collections::HashMap<String, Vec<u8>>, [u8; 32]) {
        let (seed, pubkey) = crate::update::feed::tests::test_keypair();
        let manifest = serde_json::json!({
            "version": version,
            "bundles": {"lait": version},
            "artifacts": {"lait": {"x86_64-pc-windows-msvc": {
                "url": artifact_url,
                "blake3": digest_claim,
                "size": size_claim,
            }}},
        });
        let pointer = serde_json::json!({
            "kind": "release",
            "version": version,
            "manifest": "https://feed.example/releases/m.json",
        });
        let mut objects = std::collections::HashMap::new();
        objects.insert(
            "https://feed.example/channels/test".to_string(),
            crate::update::feed::tests::seal(&pointer, &seed).into_bytes(),
        );
        objects.insert(
            "https://feed.example/releases/m.json".to_string(),
            crate::update::feed::tests::seal(&manifest, &seed).into_bytes(),
        );
        objects.insert(artifact_url.to_string(), archive.to_vec());
        (objects, pubkey)
    }

    /// The chain, end to end: a signed pointer names a signed manifest, the
    /// manifest names an artifact, the artifact is fetched, proven by size and
    /// digest, and the binary comes back out of it byte for byte.
    ///
    /// Every link had a test already. This is the composition, which is what
    /// the client-to-process seam taught this tree to assert directly: correct
    /// parts wired wrongly fail with a symptom that names nothing.
    #[test]
    fn the_update_chain_holds_from_signed_pointer_to_extracted_binary() {
        let binary = b"a real lait binary would sit here";
        let url = "https://feed.example/releases/0.9.0/lait-x86_64-pc-windows-msvc.zip";
        let archive = windows_release_zip(binary);
        let digest = blake3::hash(&archive).to_hex().to_string();
        let (objects, pubkey) = sealed_feed("0.9.0", url, &archive, archive.len() as u64, &digest);

        let resolved = crate::update::feed::resolve_with(
            |u| {
                objects
                    .get(u)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
            },
            Channel::Test,
            "https://feed.example",
            &[pubkey],
            None,
        )
        .expect("the signed feed resolves");

        let staged = super::stage_with(
            |u, _limit| {
                objects
                    .get(u)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
            },
            &resolved,
            &semver::Version::parse("0.8.0").unwrap(),
            "x86_64-pc-windows-msvc",
        )
        .expect("the artifact stages")
        .expect("0.9.0 is newer than 0.8.0, so there is something to stage");

        assert_eq!(
            staged, binary,
            "what came out of the chain must be the exact bytes that went into the archive"
        );
    }

    #[test]
    fn a_tampered_artifact_fails_the_digest_and_never_reaches_the_swap() {
        // The manifest is signed, so an attacker who can rewrite the artifact
        // bytes cannot rewrite the digest that describes them. This is the
        // check that makes a compromised artifact host a refusal rather than an
        // install, and it must fire before anything is written.
        let url = "https://feed.example/releases/0.9.0/lait-x86_64-pc-windows-msvc.zip";
        // Equal-length payloads, because padding to the published size is
        // trivial for an attacker and the cheaper size gate would otherwise be
        // what refuses this. The digest is the check under test.
        let honest = windows_release_zip(b"lait v0.9.0 as the maintainer built");
        let swapped = windows_release_zip(b"lait v0.9.0 with a back door added!");
        assert_eq!(
            honest.len(),
            swapped.len(),
            "the fixture must isolate the digest check from the size check"
        );
        let digest = blake3::hash(&honest).to_hex().to_string();

        // The manifest is signed, so its size and digest describe the honest
        // archive. Only the bytes on the host changed.
        let (objects, pubkey) = sealed_feed("0.9.0", url, &swapped, honest.len() as u64, &digest);
        let resolved = crate::update::feed::resolve_with(
            |u| {
                objects
                    .get(u)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
            },
            Channel::Test,
            "https://feed.example",
            &[pubkey],
            None,
        )
        .unwrap();

        let error = super::stage_with(
            |u, _| {
                objects
                    .get(u)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
            },
            &resolved,
            &semver::Version::parse("0.8.0").unwrap(),
            "x86_64-pc-windows-msvc",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("digest verification failed"),
            "the refusal must name the digest, not fail vaguely: {error}"
        );
    }

    #[test]
    fn a_channel_offering_nothing_newer_stages_nothing_and_is_not_an_error() {
        let url = "https://feed.example/releases/0.9.0/lait-x86_64-pc-windows-msvc.zip";
        let archive = windows_release_zip(b"whatever");
        let digest = blake3::hash(&archive).to_hex().to_string();
        let (objects, pubkey) = sealed_feed("0.9.0", url, &archive, archive.len() as u64, &digest);
        let resolved = crate::update::feed::resolve_with(
            |u| {
                objects
                    .get(u)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
            },
            Channel::Test,
            "https://feed.example",
            &[pubkey],
            None,
        )
        .unwrap();

        // Already on 0.9.0, and also on something newer than the channel — both
        // are "nothing to do" rather than a failure, and neither may download.
        for running in ["0.9.0", "0.9.1"] {
            let staged = super::stage_with(
                |_, _| panic!("nothing may be fetched when there is nothing newer"),
                &resolved,
                &semver::Version::parse(running).unwrap(),
                "x86_64-pc-windows-msvc",
            )
            .unwrap();
            assert!(staged.is_none(), "running {running} must stage nothing");
        }
    }

    /// The env var the child half reads. Deliberately not `LAIT_`-prefixed:
    /// this crate's test harness scrubs every ambient `LAIT_*` at process load,
    /// before any test runs, so a `LAIT_`-named handoff would arrive empty in
    /// the child and the parent would look like a silent no-op.
    const SWAP_PROBE_PAYLOAD: &str = "SWAP_SELF_PROBE_PAYLOAD";

    /// The child half of the self-replace proof. Inert unless invoked with the
    /// payload variable set, so it costs nothing in a normal run.
    ///
    /// It replaces *itself* — the parent hands it a disposable copy of the test
    /// binary to be, never the real one.
    #[test]
    fn swap_self_child_replaces_the_binary_it_is_running_from() {
        let Ok(payload) = std::env::var(SWAP_PROBE_PAYLOAD) else {
            return;
        };
        let bytes = std::fs::read(&payload).expect("child reads the staged payload");
        super::swap_self(&bytes).expect("a running executable replaces itself");
    }

    /// `swap_self` replaces the executable of the process that is running it.
    ///
    /// Everything else in this module could be tested in-process; this could
    /// not, which is exactly why it went unexercised. The consequence was that
    /// the riskiest step in the whole update — the one that has to defeat a
    /// live image lock on Windows, where this repository meets that lock so
    /// often the build fails when a daemon is running — rested on the belief
    /// that the library handles it.
    ///
    /// The proof spends a child process: copy this test binary somewhere
    /// disposable, run the copy, and have it replace itself. The real binary is
    /// never a candidate.
    #[test]
    fn a_running_executable_replaces_itself_with_the_staged_bytes() {
        let workspace = std::env::temp_dir().join(format!(
            "lait-swap-probe-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("probe workspace");

        let victim = workspace.join(format!("victim{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(std::env::current_exe().unwrap(), &victim).expect("copy the test binary");
        let before = std::fs::read(&victim).unwrap();

        // Not a valid executable, deliberately: this asserts the *replacement*
        // landed, and using real binary bytes would make "did it change?"
        // unanswerable. Whether the result still loads is a separate claim and
        // is not made here.
        let payload_bytes = b"lait-swap-probe: these bytes replaced a running image";
        let payload = workspace.join("payload.bin");
        std::fs::write(&payload, payload_bytes).unwrap();

        let output = std::process::Command::new(&victim)
            .args([
                "update::tests::swap_self_child_replaces_the_binary_it_is_running_from",
                "--exact",
                "--nocapture",
            ])
            .env(SWAP_PROBE_PAYLOAD, &payload)
            .output()
            .expect("run the disposable copy");

        assert!(
            output.status.success(),
            "the child failed to replace itself:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let after = std::fs::read(&victim).expect("the replaced binary is still at its path");
        assert_ne!(before, after, "the image on disk did not change");
        assert_eq!(
            after, payload_bytes,
            "the bytes on disk must be exactly what was staged"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn extract_binary_finds_the_flat_zip_and_the_nested_tar_layouts() {
        use std::io::Write;
        // A zip shaped like the Windows release: flat, `lait.exe` at the root.
        let mut zip_bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            writer
                .start_file::<_, ()>("lait.exe", zip::write::FileOptions::default())
                .unwrap();
            writer.write_all(b"windows-binary").unwrap();
            writer.finish().unwrap();
        }
        let extracted =
            super::extract_binary("https://x/lait-x.zip", &zip_bytes, "x86_64-pc-windows-msvc")
                .unwrap();
        assert_eq!(extracted, b"windows-binary");

        // A tar.gz shaped like a unix release: nested under `lait-<target>/`.
        let target = "x86_64-unknown-linux-gnu";
        let mut tar_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let payload = b"linux-binary";
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("lait-{target}/lait"), &payload[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let extracted =
            super::extract_binary("https://x/lait-x.tar.gz", &tar_bytes, target).unwrap();
        assert_eq!(extracted, b"linux-binary");

        // The wrong path in an otherwise valid archive is an error naming the
        // path, not a silent empty binary.
        let err = super::extract_binary(
            "https://x/lait-x.tar.gz",
            &tar_bytes,
            "aarch64-apple-darwin",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("lait-aarch64-apple-darwin/lait"),
            "{err}"
        );
    }
}
