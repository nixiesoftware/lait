//! Fetching a World's web head from its own channel (SUB-22).
//!
//! The other half of [`super::runtime`] and `serve::head`: those decide which
//! bundle a head *serves*, this one puts a bundle there. A World ships on its
//! own cadence, so it has its own mutable object —
//! `channels/worlds/<world>/<channel>` — resolved by exactly the rules the
//! product's channel is resolved by, because [`feed::resolve_pointer_with`] is
//! the same function. Signature, one-hop relocation, freshness ratchet, and
//! the stable-never-prerelease refusal all come along; nothing about a World
//! gets a second, weaker contract.
//!
//! ## The runtime version is the key, so incompatibility is absence
//!
//! A release's artifacts are keyed `artifacts[<world>][<runtime>]`, and this
//! node asks for its own runtime and nothing else. A bundle built for another
//! runtime is not refused here — it is *not found*, which is the same shape
//! VS Code's marketplace gives `engines.vscode`: an incompatible version is
//! withheld rather than delivered and rejected. That is why "a bundle newer
//! than its host" needs no handling anywhere in this path.
//!
//! ## Staging is where a bundle is proven
//!
//! Bytes are verified against the signed manifest — size then digest — before
//! anything is extracted, and the tree lands in a scratch directory that is
//! renamed into place only once it is whole. `serve::head` therefore serves a
//! directory it did not verify itself, and is right to: a directory present
//! under a runtime's name is one that was proven when it landed, by this.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{feed, tree};

/// What a check of one World's channel came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A bundle was fetched, proven, and put in place.
    Staged {
        /// The bundle version now staged for this runtime.
        version: String,
    },
    /// The channel's release already sits under this runtime, so nothing was
    /// fetched. Re-downloading it every period would spend bytes to learn
    /// nothing.
    Current {
        /// The version already in place.
        version: String,
    },
    /// The channel resolved and its release carries no bundle for this
    /// runtime.
    ///
    /// The ordinary answer for a build the publisher has not shipped for —
    /// withheld, not refused, and never an error. Whatever is already in
    /// place keeps serving, and that may be the embedded floor.
    NothingForThisRuntime {
        /// The release the channel names, which holds nothing for us.
        version: String,
    },
}

/// What sits under `heads/<runtime>/`, recorded beside it rather than inside.
///
/// Beside, because everything inside that directory is *served*: a marker file
/// within it would be reachable at a URL, and a served tree must hold only
/// what its publisher put there.
#[derive(Debug, Serialize, Deserialize)]
pub struct StagedHead {
    /// The World this bundle belongs to.
    pub world: String,
    /// The bundle version.
    pub version: String,
    /// The runtime version it was published for — the same token as the
    /// directory name, recorded so a hand-inspected install explains itself.
    pub runtime: String,
    /// How many files the bundle holds.
    pub files: usize,
}

/// The record beside `heads/<runtime>/`, when a bundle is staged there.
pub fn staged(heads: &Path, runtime: &str) -> Option<StagedHead> {
    if !heads.join(runtime).is_dir() {
        return None;
    }
    let bytes = std::fs::read(record_path(heads, runtime)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn record_path(heads: &Path, runtime: &str) -> PathBuf {
    heads.join(format!("{runtime}.json"))
}

/// The pointer URL of a World's channel.
pub fn pointer_url(base: &str, world: &str, channel: feed::Channel) -> String {
    format!(
        "{}/channels/worlds/{world}/{}",
        base.trim_end_matches('/'),
        channel.as_str()
    )
}

/// Fetch and stage this World's head for `runtime`, if the channel holds one
/// this node does not already have.
///
/// Blocking, and policy-free: whether to check at all is the caller's
/// decision, and the fetch is injected for the same reason every other
/// function on this path injects one.
pub fn stage_head_with<F>(
    fetch: F,
    resolved: &feed::Resolved,
    world: &str,
    runtime: &str,
    heads: &Path,
) -> Result<Outcome>
where
    F: Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure>,
{
    let version = resolved
        .manifest
        .bundles
        .get(world)
        .cloned()
        .unwrap_or_else(|| resolved.version.to_string());

    let Some(artifact) = resolved
        .manifest
        .artifacts
        .get(world)
        .and_then(|targets| targets.get(runtime))
    else {
        return Ok(Outcome::NothingForThisRuntime { version });
    };

    if staged(heads, runtime).is_some_and(|head| head.version == version && head.world == world) {
        return Ok(Outcome::Current { version });
    }

    let bytes = fetch(&artifact.url, artifact.size)
        .map_err(|error| anyhow!("world bundle download: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.size {
        bail!(
            "world bundle size mismatch: manifest says {} bytes, got {}",
            artifact.size,
            bytes.len()
        );
    }
    let digest = blake3::hash(&bytes).to_hex().to_string();
    if digest != artifact.blake3.to_lowercase() {
        bail!(
            "world bundle digest verification failed for {}: manifest {}, downloaded {digest}",
            artifact.url,
            artifact.blake3
        );
    }

    std::fs::create_dir_all(heads)
        .with_context(|| format!("create the heads directory at {}", heads.display()))?;
    let scratch = heads.join(tree::scratch_name(&format!("{runtime}.tmp-")));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("create the staging scratch at {}", scratch.display()))?;

    // The same extractor the client tree uses: one root directory stripped,
    // every path proven to be plain and relative before it is written. Shared
    // rather than reimplemented, because two unpackers with one contract is
    // the drift this whole path exists to avoid.
    let files = match tree::extract_tree(&bytes, &scratch) {
        Ok(files) => files,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(error);
        }
    };
    if files.is_empty() {
        let _ = std::fs::remove_dir_all(&scratch);
        bail!("the {world} bundle for {runtime} carries no files");
    }

    let live = heads.join(runtime);
    let record = record_path(heads, runtime);
    // The record goes first, so from here until it is written again there is
    // no believable staged head — the state every reader must see while the
    // directory beneath it is being replaced.
    let _ = std::fs::remove_file(&record);
    if live.exists() {
        // Rename aside rather than delete in place: a rename onto an existing
        // directory is an error on Windows, and a delete there routinely
        // loses to a scanner holding a file open.
        let aside = heads.join(tree::scratch_name(&format!("{runtime}.old-")));
        std::fs::rename(&live, &aside)
            .with_context(|| format!("set aside the prior head at {}", live.display()))?;
        let _ = std::fs::remove_dir_all(&aside);
    }
    std::fs::rename(&scratch, &live)
        .with_context(|| format!("move the staged head into place at {}", live.display()))?;

    let head = StagedHead {
        world: world.to_string(),
        version: version.clone(),
        runtime: runtime.to_string(),
        files: files.len(),
    };
    let encoded = serde_json::to_vec_pretty(&head).context("encode the staged head record")?;
    let staging = heads.join(tree::scratch_name(&format!("{runtime}.json.tmp-")));
    std::fs::write(&staging, encoded).context("write the staged head record")?;
    std::fs::rename(&staging, &record).context("seal the staged head record")?;

    Ok(Outcome::Staged { version })
}

/// Resolve one World's channel and stage what it holds, against the real feed.
///
/// `Err` is only ever a verification or transport failure worth reporting;
/// "this release holds nothing for my runtime" is an ordinary [`Outcome`].
pub fn check(world: &str, runtime: &str, heads: &Path, channel: feed::Channel) -> Result<Outcome> {
    let pubkeys = feed::pinned_pubkeys().map_err(|error| anyhow!("{error}"))?;
    let url = pointer_url(feed::FEED_BASE_URL, world, channel);
    let resolved = feed::resolve_pointer_with(
        |asked| feed::http_fetch(asked, 1024 * 1024),
        &url,
        channel,
        &pubkeys,
        None,
    )
    .map_err(|error| anyhow!("{error}"))?;
    stage_head_with(feed::http_fetch, &resolved, world, runtime, heads)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::feed::Channel;

    /// A bundle archive as `lait-feed world` packs one: gzip'd tar, a single
    /// root directory, every path beneath it.
    fn bundle(root: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (path, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("{root}/{path}"), *contents)
                    .expect("a bundle entry appends");
            }
            builder
                .into_inner()
                .expect("the tar seals")
                .finish()
                .expect("the gzip seals");
        }
        bytes
    }

    /// Seal a World's feed: a pointer at the World's own channel URL, and a
    /// manifest keying the artifact by runtime version.
    fn sealed(
        world: &str,
        version: &str,
        runtime: &str,
        archive: &[u8],
        size_claim: u64,
        digest_claim: &str,
    ) -> (std::collections::HashMap<String, Vec<u8>>, [u8; 32]) {
        let (seed, pubkey) = feed::tests::test_keypair();
        let url = format!("https://feed.example/releases/worlds/{world}/{version}/bundle.tar.gz");
        let manifest = serde_json::json!({
            "version": version,
            "bundles": { world: version },
            "artifacts": { world: { runtime: {
                "url": url,
                "blake3": digest_claim,
                "size": size_claim,
            }}},
        });
        let pointer = serde_json::json!({
            "kind": "release",
            "version": version,
            "manifest": format!("https://feed.example/releases/worlds/{world}/{version}/manifest.json"),
        });
        let mut objects = std::collections::HashMap::new();
        objects.insert(
            pointer_url("https://feed.example", world, Channel::Test),
            feed::tests::seal(&pointer, &seed).into_bytes(),
        );
        objects.insert(
            format!("https://feed.example/releases/worlds/{world}/{version}/manifest.json"),
            feed::tests::seal(&manifest, &seed).into_bytes(),
        );
        objects.insert(url, archive.to_vec());
        (objects, pubkey)
    }

    fn resolve(
        objects: &std::collections::HashMap<String, Vec<u8>>,
        pubkey: [u8; 32],
        world: &str,
    ) -> feed::Resolved {
        feed::resolve_pointer_with(
            |url| {
                objects
                    .get(url)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &pointer_url("https://feed.example", world, Channel::Test),
            Channel::Test,
            &[pubkey],
            None,
        )
        .expect("the World's signed channel resolves")
    }

    fn fetcher(
        objects: &std::collections::HashMap<String, Vec<u8>>,
    ) -> impl Fn(&str, u64) -> std::result::Result<Vec<u8>, feed::Failure> + '_ {
        move |url, _| {
            objects
                .get(url)
                .cloned()
                .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
        }
    }

    const WORLD: &str = "com.lait.issues";

    #[test]
    fn a_signed_bundle_for_this_runtime_is_staged_under_its_runtime_name() {
        let archive = bundle(
            "world-com.lait.issues-0.1.0",
            &[
                ("index.html", b"<html>the head</html>"),
                ("assets/app.js", b"//"),
            ],
        );
        let (objects, pubkey) = sealed(
            WORLD,
            "0.1.0",
            "rt-abc",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey, WORLD);
        let heads = tempfile::tempdir().expect("a heads root");

        let outcome = stage_head_with(fetcher(&objects), &resolved, WORLD, "rt-abc", heads.path())
            .expect("the bundle stages");
        assert_eq!(
            outcome,
            Outcome::Staged {
                version: "0.1.0".into()
            }
        );
        assert_eq!(
            std::fs::read(heads.path().join("rt-abc").join("index.html")).expect("the entry"),
            b"<html>the head</html>"
        );
        let record = staged(heads.path(), "rt-abc").expect("the record beside the tree");
        assert_eq!(record.version, "0.1.0");
        assert_eq!(record.world, WORLD);
        assert_eq!(record.files, 2);
        // Beside, never inside: everything under the runtime directory is
        // served, so a marker within it would be reachable at a URL.
        assert!(
            !heads.path().join("rt-abc").join("rt-abc.json").exists(),
            "the record was written inside the served tree"
        );
    }

    /// The withheld case, and the reason "a bundle newer than its host" needs
    /// no handling: a release built for another runtime is not refused, it is
    /// absent.
    #[test]
    fn a_release_with_no_bundle_for_this_runtime_is_withheld_rather_than_refused() {
        let archive = bundle("world-x-0.1.0", &[("index.html", b"x")]);
        let (objects, pubkey) = sealed(
            WORLD,
            "0.1.0",
            "rt-theirs",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey, WORLD);
        let heads = tempfile::tempdir().expect("a heads root");
        let outcome = stage_head_with(
            |_, _| panic!("nothing may be fetched for a runtime the release does not carry"),
            &resolved,
            WORLD,
            "rt-mine",
            heads.path(),
        )
        .expect("an absent bundle is not an error");
        assert_eq!(
            outcome,
            Outcome::NothingForThisRuntime {
                version: "0.1.0".into()
            }
        );
        assert!(
            !heads.path().join("rt-mine").exists(),
            "a withheld release left a staged head behind"
        );
    }

    #[test]
    fn a_bundle_already_staged_at_this_version_is_not_fetched_again() {
        let archive = bundle("world-com.lait.issues-0.1.0", &[("index.html", b"one")]);
        let (objects, pubkey) = sealed(
            WORLD,
            "0.1.0",
            "rt-abc",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey, WORLD);
        let heads = tempfile::tempdir().expect("a heads root");
        stage_head_with(fetcher(&objects), &resolved, WORLD, "rt-abc", heads.path())
            .expect("the first stage");
        let outcome = stage_head_with(
            |_, _| panic!("a head already at this version must not be downloaded again"),
            &resolved,
            WORLD,
            "rt-abc",
            heads.path(),
        )
        .expect("the second check");
        assert_eq!(
            outcome,
            Outcome::Current {
                version: "0.1.0".into()
            }
        );
    }

    #[test]
    fn a_tampered_bundle_fails_the_digest_and_stages_nothing() {
        let honest = bundle(
            "world-com.lait.issues-0.1.0",
            &[("index.html", b"as built!")],
        );
        let swapped = bundle(
            "world-com.lait.issues-0.1.0",
            &[("index.html", b"backdoor!")],
        );
        // The manifest is signed, so it describes the honest bundle's digest;
        // its size claim is the delivered length so the cheaper size gate
        // cannot answer this before the digest does.
        let (mut objects, pubkey) = sealed(
            WORLD,
            "0.1.0",
            "rt-abc",
            &honest,
            swapped.len() as u64,
            &blake3::hash(&honest).to_hex().to_string(),
        );
        let artifact = objects
            .keys()
            .find(|k| k.ends_with("bundle.tar.gz"))
            .expect("the artifact url")
            .clone();
        objects.insert(artifact, swapped);
        let resolved = resolve(&objects, pubkey, WORLD);
        let heads = tempfile::tempdir().expect("a heads root");

        let error = stage_head_with(fetcher(&objects), &resolved, WORLD, "rt-abc", heads.path())
            .expect_err("a tampered bundle must refuse")
            .to_string();
        assert!(
            error.contains("digest verification failed"),
            "the refusal must name the digest: {error}"
        );
        assert!(
            !heads.path().join("rt-abc").exists() && staged(heads.path(), "rt-abc").is_none(),
            "a refused bundle left staged state behind"
        );
    }

    #[test]
    fn an_over_long_bundle_is_refused_by_size_before_the_digest_is_reached() {
        let archive = bundle("world-com.lait.issues-0.1.0", &[("index.html", b"honest")]);
        let (objects, pubkey) = sealed(
            WORLD,
            "0.1.0",
            "rt-abc",
            &archive,
            archive.len() as u64,
            &blake3::hash(&archive).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey, WORLD);
        let heads = tempfile::tempdir().expect("a heads root");
        let error = stage_head_with(
            |url, _| {
                objects
                    .get(url)
                    .cloned()
                    .map(|mut bytes| {
                        if url.ends_with(".tar.gz") {
                            bytes.extend_from_slice(b"padding the host added");
                        }
                        bytes
                    })
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &resolved,
            WORLD,
            "rt-abc",
            heads.path(),
        )
        .expect_err("an over-long bundle must refuse")
        .to_string();
        assert!(error.contains("size mismatch"), "{error}");
    }

    /// A World's pointer is its own object, one level inside the product's
    /// channel layout — and it is resolved by the same function, so it
    /// inherits every rule rather than getting a weaker copy of them.
    #[test]
    fn a_world_channel_sits_beside_the_products_and_under_the_same_rules() {
        assert_eq!(
            pointer_url("https://feed.example/", WORLD, Channel::Test),
            "https://feed.example/channels/worlds/com.lait.issues/test"
        );
        assert_eq!(
            pointer_url("https://feed.example", WORLD, Channel::Stable),
            "https://feed.example/channels/worlds/com.lait.issues/stable"
        );

        // A pointer that verifies but names a prerelease on stable is refused
        // by the shared resolver, not by anything written here.
        let archive = bundle(
            "world-com.lait.issues-0.2.0-test.1",
            &[("index.html", b"x")],
        );
        let (seed, pubkey) = feed::tests::test_keypair();
        let pointer = serde_json::json!({
            "kind": "release",
            "version": "0.2.0-test.1",
            "manifest": "https://feed.example/m.json",
        });
        let mut objects = std::collections::HashMap::new();
        objects.insert(
            pointer_url("https://feed.example", WORLD, Channel::Stable),
            feed::tests::seal(&pointer, &seed).into_bytes(),
        );
        let _ = archive;
        let failure = feed::resolve_pointer_with(
            |url| {
                objects
                    .get(url)
                    .cloned()
                    .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {url}")))
            },
            &pointer_url("https://feed.example", WORLD, Channel::Stable),
            Channel::Stable,
            &[pubkey],
            None,
        )
        .expect_err("a prerelease on a World's stable channel must refuse");
        assert!(
            matches!(failure, feed::Failure::Invalid(_)),
            "the refusal must be Invalid, not a weaker arm: {failure:?}"
        );
    }

    #[test]
    fn a_newer_bundle_replaces_the_prior_one_whole() {
        let first = bundle(
            "world-com.lait.issues-0.1.0",
            &[("index.html", b"one"), ("only-in-one.js", b"//")],
        );
        let (objects, pubkey) = sealed(
            WORLD,
            "0.1.0",
            "rt-abc",
            &first,
            first.len() as u64,
            &blake3::hash(&first).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey, WORLD);
        let heads = tempfile::tempdir().expect("a heads root");
        stage_head_with(fetcher(&objects), &resolved, WORLD, "rt-abc", heads.path())
            .expect("the first stage");

        let second = bundle("world-com.lait.issues-0.2.0", &[("index.html", b"two")]);
        let (objects, pubkey) = sealed(
            WORLD,
            "0.2.0",
            "rt-abc",
            &second,
            second.len() as u64,
            &blake3::hash(&second).to_hex().to_string(),
        );
        let resolved = resolve(&objects, pubkey, WORLD);
        stage_head_with(fetcher(&objects), &resolved, WORLD, "rt-abc", heads.path())
            .expect("the second stage");

        assert!(
            !heads.path().join("rt-abc").join("only-in-one.js").exists(),
            "a restage carried a file from the bundle it replaced"
        );
        assert_eq!(
            staged(heads.path(), "rt-abc").expect("the record").version,
            "0.2.0"
        );
    }
}
