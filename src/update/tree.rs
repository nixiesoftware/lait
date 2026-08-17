//! Staging a whole client tree from the release feed (CLIENT-65).
//!
//! The single-binary path in [`super`] swaps `lait` in place. An installed
//! client is a directory — the astrolabe+lait pair, the Flutter runtime, the
//! assets — and a directory is staged, never self-replaced: this module
//! downloads the release's *tree artifact*, proves it against the signed
//! manifest, extracts it beside the live tree, and records what it must hash
//! to. The stub launcher (`tools/stub`) is the other half: it re-proves the
//! tree against that record and swaps by rename at a moment no client runs.
//!
//! The tree artifact is its own bundle, [`TREE_BUNDLE`], distinct from the
//! `astrolabe` bundle that carries the human installers (`-setup.exe`,
//! `.dmg`) — those are installer containers nothing can extract portably.
//! A tree artifact is a `.tar.gz` with exactly one root directory on every
//! platform, entry binary `astrolabe`/`astrolabe.exe` at the tree root.
//! Shipped binaries ignore the unknown bundle name until they carry this
//! code, which is what lets the feed grow it without a flag day.
//!
//! The stage-manifest shape is deliberately duplicated in `astrolabe-stub`
//! rather than shared through a dependency — the stub must not link the
//! engine. The chain test in `tools/astrolabe/tests/launch.rs` holds the
//! writer and the reader together, the way the packaging test holds
//! `sidecar::beside` and [`super::custody_of`] together.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use super::feed;

/// The bundle name of the swap-consumable tree artifact.
pub const TREE_BUNDLE: &str = "astrolabe-tree";

/// The stage manifest's file name in the install root. Must agree with the
/// stub's constant of the same name; the chain test asserts the agreement.
pub const STAGE_MANIFEST: &str = "staged.manifest.json";

/// The staged tree's directory name in the install root.
pub const STAGED_DIR: &str = "staged";

/// Held while `staged/` is written here or consumed by the stub. Must agree
/// with the stub's constant of the same name.
pub const STAGING_LOCK: &str = "staging.lock";

/// The live tree's directory name in the install root. Must agree with the
/// stub's `CURRENT_DIR`; it is spelled here because the daemon recognises an
/// installation by this shape.
pub const LIVE_DIR: &str = "current";

/// What was staged, for the caller's report.
#[derive(Debug)]
pub struct StagedTree {
    /// The release version the tree carries.
    pub version: String,
    /// The entry binary's path relative to the tree root.
    pub entry: String,
    /// How many files the tree holds.
    pub files: usize,
}

/// The record the stub verifies against. Field for field the shape
/// `astrolabe_stub::StageManifest` reads.
#[derive(Debug, Serialize)]
struct StageRecord {
    version: String,
    entry: String,
    files: Vec<FileRecord>,
}

#[derive(Debug, Serialize)]
struct FileRecord {
    path: String,
    blake3: String,
    size: u64,
    executable: bool,
}

/// The platform entry binary inside a tree, from the target string rather
/// than a `cfg` split, so every platform's answer is computable from any
/// host — the same rule as [`super::bin_path_for`], bought by the same
/// `lait.exe.exe` history.
fn entry_for(target: &str) -> &'static str {
    if target.contains("-windows-") {
        "astrolabe.exe"
    } else {
        "astrolabe"
    }
}

/// The sidecar that must sit beside the entry in every tree, by the same
/// rule: `astrolabe::sidecar::beside` finds it with `with_file_name`, so it
/// is a flat sibling or it is not found at all.
fn sidecar_for(target: &str) -> &'static str {
    if target.contains("-windows-") {
        "lait.exe"
    } else {
        "lait"
    }
}

/// Fetch, prove, and extract this release's tree artifact for `target` into
/// `root`, leaving `root/staged/` and — last, so a crash leaves inert bytes
/// rather than a believable half-stage — `root/staged.manifest.json`.
///
/// Held under `root/staging.lock` for the whole mutation, because the other
/// half of this contract runs in another process: the stub consumes
/// `staged/` at launch, and this runs in the daemon while a client is alive.
/// Without the lock a stage landing between the stub's verification and its
/// rename would put a tree into `current/` that nothing verified.
///
/// Policy-free on purpose: whether staging should happen at all (newness,
/// cadence, channel) is the caller's decision. The fetch is injected for the
/// same reason `feed::resolve_with` injects one.
pub fn stage_tree_with<F>(
    fetch: F,
    resolved: &feed::Resolved,
    target: &str,
    root: &Path,
) -> Result<StagedTree>
where
    F: Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure>,
{
    let artifact = resolved
        .manifest
        .artifacts
        .get(TREE_BUNDLE)
        .and_then(|targets| targets.get(target))
        .ok_or_else(|| {
            anyhow!(
                "release {} carries no {TREE_BUNDLE} artifact for {target}",
                resolved.version
            )
        })?;
    let version = resolved
        .manifest
        .bundles
        .get(TREE_BUNDLE)
        .cloned()
        .unwrap_or_else(|| resolved.version.to_string());

    let bytes = fetch(&artifact.url, artifact.size)
        .map_err(|error| anyhow!("tree artifact download: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.size {
        bail!(
            "tree artifact size mismatch: manifest says {} bytes, got {}",
            artifact.size,
            bytes.len()
        );
    }
    let digest = blake3::hash(&bytes).to_hex().to_string();
    if digest != artifact.blake3.to_lowercase() {
        bail!(
            "tree artifact digest verification failed for {}: manifest {}, downloaded {digest}",
            artifact.url,
            artifact.blake3
        );
    }

    // Exclusive with the stub's verify-then-swap window for the whole
    // mutation below. Taken after the download, so a long fetch never holds
    // a launch's apply back.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(STAGING_LOCK))
        .with_context(|| format!("open the staging lock in {}", root.display()))?;
    fs2::FileExt::lock_exclusive(&lock).context("take the staging lock")?;
    let staged_result = stage_verified_bytes(&bytes, &version, target, root);
    let _ = fs2::FileExt::unlock(&lock);
    staged_result
}

/// The version a staged tree is waiting to become, when one is waiting.
///
/// Reads only the manifest's own claim: this is a cheap "is there anything
/// here" question, and the stub is what proves the tree against it before
/// anything is swapped.
pub fn staged_version(root: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Claim {
        version: String,
    }
    let bytes = std::fs::read(root.join(STAGE_MANIFEST)).ok()?;
    if !root.join(STAGED_DIR).is_dir() {
        return None;
    }
    serde_json::from_slice::<Claim>(&bytes)
        .ok()
        .map(|c| c.version)
}

/// Everything after the bytes are proven: extract, check the shape, put the
/// tree in place, and seal the manifest that blesses it. Split out so the
/// staging lock brackets exactly the mutation and nothing else.
fn stage_verified_bytes(
    bytes: &[u8],
    version: &str,
    target: &str,
    root: &Path,
) -> Result<StagedTree> {
    // Extract into a scratch directory first: `staged/` must only ever hold
    // a complete tree, and the manifest that blesses it comes last. The
    // scratch name cannot collide with an orphan of a recycled pid, and
    // every failure path below removes it — a leaked scratch is a whole
    // client tree left in the install root.
    let scratch = root.join(scratch_name("staged.tmp-"));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("create the staging scratch at {}", scratch.display()))?;
    let staged = match stage_into(&scratch, bytes, version, target, root) {
        Ok(staged) => staged,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(error);
        }
    };
    Ok(staged)
}

/// The mutation proper, with `scratch` already created and owned by the
/// caller's cleanup.
fn stage_into(
    scratch: &Path,
    bytes: &[u8],
    version: &str,
    target: &str,
    root: &Path,
) -> Result<StagedTree> {
    let files = extract_tree(bytes, scratch)?;

    // The pair is what an installed tree *is*: `astrolabe`'s `sidecar::beside`
    // and `update::custody_of` are inverses that both spell `with_file_name`,
    // so a tree missing either half — or nesting one of them — produces an
    // installation whose client cannot find its daemon and whose daemon
    // believes it is standalone. Refused here rather than discovered at the
    // first launch after a swap.
    let entry = entry_for(target);
    let sidecar = sidecar_for(target);
    for required in [entry, sidecar] {
        if !files.iter().any(|file| file.path == required) {
            bail!("the tree artifact carries no {required} at its root");
        }
    }

    let staged = root.join(STAGED_DIR);
    let manifest_path = root.join(STAGE_MANIFEST);
    // The manifest goes first: from here until it is sealed again there is
    // no believable stage, which is the state every reader must see while
    // the tree is being replaced.
    let _ = std::fs::remove_file(&manifest_path);
    // A rename onto an existing directory is an error on Windows, and
    // `remove_dir_all` there routinely loses to a scanner holding a file
    // open. Renaming the old tree aside first turns "delete then rename"
    // into two renames, and the leftover is swept by whoever gets there
    // first — this function on its next run, or the stub on its next claim.
    if staged.exists() {
        let aside = root.join(scratch_name("staged.tmp-old-"));
        std::fs::rename(&staged, &aside)
            .with_context(|| format!("set aside the prior staged tree at {}", staged.display()))?;
        let _ = std::fs::remove_dir_all(&aside);
    }
    std::fs::rename(scratch, &staged)
        .with_context(|| format!("move the staged tree into place at {}", staged.display()))?;

    let record = StageRecord {
        version: version.to_string(),
        entry: entry.to_string(),
        files,
    };
    let encoded = serde_json::to_vec_pretty(&record).context("encode the stage manifest")?;
    let manifest_tmp = root.join(scratch_name(&format!("{STAGE_MANIFEST}.tmp-")));
    std::fs::write(&manifest_tmp, encoded).context("write the stage manifest")?;
    std::fs::rename(&manifest_tmp, &manifest_path).context("seal the stage manifest")?;

    Ok(StagedTree {
        version: version.to_string(),
        entry: entry.to_string(),
        files: record.files.len(),
    })
}

/// A scratch name no concurrent run or recycled pid can collide with.
fn scratch_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Unpack a verified `.tar.gz` tree into `into`, stripping the archive's
/// single root directory and hashing every file as it lands.
///
/// The bytes were already proven against the signed manifest, but paths are
/// still refused rather than trusted: an absolute path or a `..` component
/// is a malformed archive, not an instruction.
fn extract_tree(bytes: &[u8], into: &Path) -> Result<Vec<FileRecord>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut files = Vec::new();
    for entry in archive.entries().context("read the tree archive")? {
        let mut entry = entry.context("tree archive entry")?;
        let path = entry
            .path()
            .context("tree archive entry path")?
            .into_owned();
        let mut components = path.components();
        // Every entry sits under the archive's one root directory; the tree
        // is addressed without it.
        components.next();
        let relative: PathBuf = components.as_path().to_path_buf();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!(
                "the tree archive addresses outside its root: {}",
                path.display()
            );
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }

        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .context("read a tree archive file")?;
        let destination = into.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&destination, &contents)
            .with_context(|| format!("write {}", destination.display()))?;

        let executable = entry
            .header()
            .mode()
            .map(|m| m & 0o111 != 0)
            .unwrap_or(false);
        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("mark {} executable", destination.display()))?;
        }

        files.push(FileRecord {
            path: relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
            blake3: blake3::hash(&contents).to_hex().to_string(),
            size: u64::try_from(contents.len()).unwrap_or(u64::MAX),
            executable,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::feed::{self, Channel};

    /// A tree archive shaped like the published artifact: gzip'd tar with a
    /// single root directory.
    fn tree_targz(root_name: &str, files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (path, contents, executable) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(if *executable { 0o755 } else { 0o644 });
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("{root_name}/{path}"), *contents)
                    .expect("a tree entry appends");
            }
            builder
                .into_inner()
                .expect("the tar seals")
                .finish()
                .expect("the gzip seals");
        }
        bytes
    }

    /// Seal a feed whose release carries one tree artifact, returning the
    /// object map and the verifying key — the same fixture shape as the
    /// single-binary chain test in [`crate::update::tests`].
    fn sealed_tree_feed(
        version: &str,
        target: &str,
        archive: &[u8],
    ) -> (std::collections::HashMap<String, Vec<u8>>, [u8; 32]) {
        let (mut objects, pubkey) = sealed_tree_feed_claiming(
            version,
            target,
            archive.len() as u64,
            blake3::hash(archive).to_hex().as_ref(),
        );
        let url = objects
            .keys()
            .find(|k| k.ends_with(".tar.gz"))
            .expect("the artifact url")
            .clone();
        objects.insert(url, archive.to_vec());
        (objects, pubkey)
    }

    /// The same feed, with the manifest's size and digest claims given
    /// rather than measured — which is what lets a test put the two gates
    /// under separate fire.
    fn sealed_tree_feed_claiming(
        version: &str,
        target: &str,
        size_claim: u64,
        digest_claim: &str,
    ) -> (std::collections::HashMap<String, Vec<u8>>, [u8; 32]) {
        let (seed, pubkey) = feed::tests::test_keypair();
        let url = format!("https://feed.example/releases/{version}/astrolabe-tree.tar.gz");
        let manifest = serde_json::json!({
            "version": version,
            "bundles": { TREE_BUNDLE: version },
            "artifacts": { TREE_BUNDLE: { target: {
                "url": url,
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
            feed::tests::seal(&pointer, &seed).into_bytes(),
        );
        objects.insert(
            "https://feed.example/releases/m.json".to_string(),
            feed::tests::seal(&manifest, &seed).into_bytes(),
        );
        objects.insert(url, Vec::new());
        (objects, pubkey)
    }

    fn resolve(
        objects: &std::collections::HashMap<String, Vec<u8>>,
        pubkey: [u8; 32],
    ) -> feed::Resolved {
        feed::resolve_with(
            |url| {
                objects
                    .get(url)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            Channel::Test,
            "https://feed.example",
            &[pubkey],
            None,
        )
        .expect("the signed feed resolves")
    }

    #[test]
    fn a_signed_tree_stages_with_a_manifest_the_stub_shape_can_read() {
        let target = "x86_64-unknown-linux-gnu";
        let archive = tree_targz(
            "astrolabe-0.0.2",
            &[
                ("astrolabe", b"entry", true),
                ("lait", b"sidecar", true),
                ("data/asset", b"a", false),
            ],
        );
        let (objects, pubkey) = sealed_tree_feed("0.0.2", target, &archive);
        let resolved = resolve(&objects, pubkey);

        let root = tempfile::tempdir().expect("an install root");
        let staged = stage_tree_with(
            |url, _| {
                objects
                    .get(url)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &resolved,
            target,
            root.path(),
        )
        .expect("the tree stages");
        assert_eq!(staged.version, "0.0.2");
        assert_eq!(staged.entry, "astrolabe");
        assert_eq!(staged.files, 3);

        assert_eq!(
            std::fs::read(root.path().join(STAGED_DIR).join("astrolabe"))
                .expect("the staged entry"),
            b"entry"
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join(STAGE_MANIFEST)).expect("the stage manifest"),
        )
        .expect("the manifest parses");
        assert_eq!(manifest["version"], "0.0.2");
        assert_eq!(manifest["entry"], "astrolabe");
        assert_eq!(
            manifest["files"].as_array().expect("a files array").len(),
            3
        );
    }

    #[test]
    fn a_tampered_tree_artifact_fails_the_digest_and_stages_nothing() {
        let target = "x86_64-unknown-linux-gnu";
        let honest = tree_targz(
            "astrolabe-0.0.2",
            &[
                ("astrolabe", b"as built!", true),
                ("lait", b"sidecar", true),
            ],
        );
        let swapped = tree_targz(
            "astrolabe-0.0.2",
            &[
                ("astrolabe", b"back door", true),
                ("lait", b"sidecar", true),
            ],
        );
        // The manifest is signed, so it describes the honest archive's
        // digest; only the bytes on the host changed. Its size claim is the
        // delivered length, because gzip streams of equal-length inputs are
        // not themselves equal length and the cheaper size gate would
        // otherwise answer this before the digest ever ran.
        let (mut objects, pubkey) = sealed_tree_feed_claiming(
            "0.0.2",
            target,
            swapped.len() as u64,
            blake3::hash(&honest).to_hex().as_ref(),
        );
        let url = objects
            .keys()
            .find(|k| k.ends_with(".tar.gz"))
            .expect("the artifact url")
            .clone();
        objects.insert(url, swapped);
        let resolved = resolve(&objects, pubkey);

        let root = tempfile::tempdir().expect("an install root");
        let error = stage_tree_with(
            |u, _| {
                objects
                    .get(u)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
            },
            &resolved,
            target,
            root.path(),
        )
        .expect_err("a tampered artifact must refuse")
        .to_string();
        assert!(
            error.contains("digest verification failed"),
            "the refusal must name the digest: {error}"
        );
        assert!(
            !root.path().join(STAGE_MANIFEST).exists() && !root.path().join(STAGED_DIR).exists(),
            "a refused artifact left staged state behind"
        );
    }

    #[test]
    fn a_release_without_a_tree_artifact_for_this_target_says_so() {
        let target = "x86_64-unknown-linux-gnu";
        let archive = tree_targz(
            "astrolabe-0.0.2",
            &[("astrolabe", b"entry", true), ("lait", b"sidecar", true)],
        );
        let (objects, pubkey) = sealed_tree_feed("0.0.2", target, &archive);
        let resolved = resolve(&objects, pubkey);
        let root = tempfile::tempdir().expect("an install root");
        let error = stage_tree_with(
            |_, _| panic!("nothing may be fetched for an absent artifact"),
            &resolved,
            "aarch64-apple-darwin",
            root.path(),
        )
        .expect_err("an absent artifact must refuse")
        .to_string();
        assert!(
            error.contains("carries no astrolabe-tree artifact for aarch64-apple-darwin"),
            "{error}"
        );
    }

    #[test]
    fn an_archive_addressing_outside_its_root_is_refused() {
        let mut bytes = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            let payload = b"escape";
            header.set_size(payload.len() as u64);
            header.set_mode(0o644);
            // `append_data` refuses `..` in a path, which is the tar crate
            // being a good citizen — but the archive under test is hostile,
            // so the name bytes are written raw, past that courtesy.
            let name = b"root/../../escape";
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            builder
                .append(&header, &payload[..])
                .expect("the hostile entry appends");
            builder.into_inner().expect("tar").finish().expect("gz");
        }
        let scratch = tempfile::tempdir().expect("a scratch dir");
        let error = extract_tree(&bytes, scratch.path())
            .expect_err("a traversal must refuse")
            .to_string();
        assert!(
            error.contains("addresses outside its root"),
            "the refusal must name the traversal: {error}"
        );
    }

    /// The pair is the layout: `sidecar::beside` and `custody_of` are
    /// inverses that both spell `with_file_name`, so a tree missing either
    /// half installs a client that cannot find its daemon — the machine the
    /// packaging tests exist to prevent, reachable through the update path
    /// unless staging refuses it here.
    #[test]
    fn a_tree_missing_half_the_pair_is_refused_before_it_is_ever_staged() {
        let target = "x86_64-unknown-linux-gnu";
        for (missing, files) in [
            ("lait", vec![("astrolabe", &b"entry"[..], true)]),
            ("astrolabe", vec![("lait", &b"sidecar"[..], true)]),
        ] {
            let archive = tree_targz("astrolabe-0.0.2", &files);
            let (objects, pubkey) = sealed_tree_feed("0.0.2", target, &archive);
            let resolved = resolve(&objects, pubkey);
            let root = tempfile::tempdir().expect("an install root");
            let error = stage_tree_with(
                |u, _| {
                    objects
                        .get(u)
                        .cloned()
                        .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
                },
                &resolved,
                target,
                root.path(),
            )
            .expect_err("half a pair must refuse")
            .to_string();
            assert!(
                error.contains(&format!("carries no {missing} at its root")),
                "the refusal must name the missing half: {error}"
            );
            assert!(
                !root.path().join(STAGE_MANIFEST).exists(),
                "a refused tree left a believable stage behind"
            );
        }
    }

    /// The size gate is cheaper than the digest and fires first, so nothing
    /// else in this suite reaches it. A host that answers with more bytes
    /// than the signed manifest describes must be refused by name.
    #[test]
    fn an_artifact_longer_than_the_manifest_says_is_refused_by_size() {
        let target = "x86_64-unknown-linux-gnu";
        let archive = tree_targz(
            "astrolabe-0.0.2",
            &[("astrolabe", b"entry", true), ("lait", b"sidecar", true)],
        );
        let (objects, pubkey) = sealed_tree_feed("0.0.2", target, &archive);
        let resolved = resolve(&objects, pubkey);
        let root = tempfile::tempdir().expect("an install root");
        let error = stage_tree_with(
            |u, _| {
                objects
                    .get(u)
                    .cloned()
                    .map(|mut bytes| {
                        if u.ends_with(".tar.gz") {
                            bytes.extend_from_slice(b"padding the host added");
                        }
                        bytes
                    })
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
            },
            &resolved,
            target,
            root.path(),
        )
        .expect_err("an over-long artifact must refuse")
        .to_string();
        assert!(
            error.contains("size mismatch"),
            "the refusal must name the size: {error}"
        );
    }

    #[test]
    fn the_entry_name_is_computable_for_every_target_from_any_host() {
        assert_eq!(entry_for("x86_64-pc-windows-msvc"), "astrolabe.exe");
        assert_eq!(sidecar_for("x86_64-pc-windows-msvc"), "lait.exe");
        for target in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
        ] {
            assert_eq!(entry_for(target), "astrolabe");
            assert_eq!(sidecar_for(target), "lait");
        }
    }

    #[test]
    fn restaging_replaces_the_prior_stage_whole() {
        let target = "x86_64-unknown-linux-gnu";
        let first = tree_targz(
            "astrolabe-0.0.2",
            &[
                ("astrolabe", b"two", true),
                ("lait", b"sidecar", true),
                ("only-in-two", b"x", false),
            ],
        );
        let (objects, pubkey) = sealed_tree_feed("0.0.2", target, &first);
        let resolved = resolve(&objects, pubkey);
        let root = tempfile::tempdir().expect("an install root");
        let fetch = |map: &std::collections::HashMap<String, Vec<u8>>, u: &str| {
            map.get(u)
                .cloned()
                .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
        };
        stage_tree_with(|u, _| fetch(&objects, u), &resolved, target, root.path())
            .expect("the first stage");

        let second = tree_targz(
            "astrolabe-0.0.3",
            &[("astrolabe", b"three", true), ("lait", b"sidecar", true)],
        );
        let (objects, pubkey) = sealed_tree_feed("0.0.3", target, &second);
        let resolved = resolve(&objects, pubkey);
        stage_tree_with(|u, _| fetch(&objects, u), &resolved, target, root.path())
            .expect("the second stage");

        assert!(
            !root.path().join(STAGED_DIR).join("only-in-two").exists(),
            "a restage carried a file from the stage it replaced"
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join(STAGE_MANIFEST)).expect("the manifest"),
        )
        .expect("parses");
        assert_eq!(manifest["version"], "0.0.3");
    }

    /// The two constants the stub duplicates. A drift here is a stager that
    /// writes where no stub reads — caught end to end by the chain test in
    /// `tools/astrolabe/tests/launch.rs`, and named here so the failure is
    /// legible at the unit tier too.
    #[test]
    fn the_stage_layout_names_are_the_contract_the_stub_reads() {
        assert_eq!(STAGE_MANIFEST, "staged.manifest.json");
        assert_eq!(STAGED_DIR, "staged");
    }
}
