//! The consume half of the release feed (SUB-13): signed channel pointers,
//! release manifests, and the rules for believing them.
//!
//! The feed is *proven, not trusted*. It lives on a plain object host with no
//! ambient authority, so nothing here is believed before its signature
//! verifies against [`FEED_PUBKEY_HEX`], the key pinned into this binary at
//! build time. A host compromise that rewrites pointers or manifests yields
//! refusals, not installs.
//!
//! Two kinds of object, and only one ever changes. Immutable releases live
//! under `/releases/<version>/` — artifacts, digests, and a signed manifest.
//! Mutable channel pointers live at `/channels/<channel>`, served no-cache,
//! each naming exactly one release. Promotion is pointer motion. A pointer may
//! instead carry a signed *relocation* ("this channel moved"), followed
//! exactly once, so the pointer URL itself is never a permanent commitment.
//!
//! Error shape is load-bearing: [`Failure::Unreachable`] is "the channel
//! could not be asked", [`Failure::Verification`] is a signature that failed,
//! and neither may ever be rendered as "up to date". The distinction is the
//! same absence law the client's surfaces hold everywhere else.
//!
//! The publish half — sealing these envelopes, and the publish-time rules that
//! refuse an unsatisfiable floor or a stable pointer naming a prerelease —
//! lives in `tools/feed`. The envelope format is duplicated there by design
//! (the tool must not depend on the application crate); the round-trip tests
//! here are what hold the two halves to one shape.

use serde::Deserialize;
use std::collections::BTreeMap;

/// The dist host: `gs://the-foundation-dist`, public-read, decided and stood
/// up on SUB-13. Only this base is pinned — every artifact URL travels inside
/// the signed manifest, so artifacts can move hosts without touching installed
/// machines, and a pointer-host migration ships as an ordinary update (or as a
/// relocation record left at the old URL).
pub const FEED_BASE_URL: &str = "https://storage.googleapis.com/the-foundation-dist";

/// The feed's verifying key. The signing seed exists in exactly one place —
/// the maintainer's custody, minted by `lait-feed keygen` — and never on a
/// build machine or in the repository.
pub const FEED_PUBKEY_HEX: &str =
    "227e448a16c19623707a3da8b8af6e1f70afcf18fb4e509e82115ef797666ba9";

/// A pointer or manifest larger than this is not a feed object; refuse before
/// buffering someone's mistake (or someone's flood) into memory.
const MAX_FEED_OBJECT: u64 = 1024 * 1024;

/// The release stream this node follows.
///
/// Stable is the default and never resolves a prerelease — enforced at publish
/// by `lait-feed pointer`, and again here, because a defense that exists only
/// on the other side of the trust boundary is a convention, not a rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Stable,
    Test,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Test => "test",
        }
    }

    pub fn parse(s: &str) -> Option<Channel> {
        match s.trim() {
            "stable" => Some(Channel::Stable),
            "test" => Some(Channel::Test),
            _ => None,
        }
    }

    /// The channel this node follows: `LAIT_UPDATE_CHANNEL` when set (a
    /// development convenience), else the recorded choice beside the identity,
    /// else stable. An unparseable record is stable, not an error — following
    /// the wrong stream because a file was corrupted must degrade toward the
    /// conservative channel, never toward test builds.
    pub fn current() -> Channel {
        if let Ok(value) = std::env::var("LAIT_UPDATE_CHANNEL") {
            if let Some(channel) = Channel::parse(&value) {
                return channel;
            }
        }
        Self::recorded().unwrap_or(Channel::Stable)
    }

    /// Record this channel as the node's choice. Opting in is an explicit act
    /// (CLIENT-52); this is only its persistence.
    pub fn record(self) -> anyhow::Result<()> {
        let dir = crate::config::identity_dir()?;
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("update-channel"), self.as_str())?;
        Ok(())
    }

    fn recorded() -> Option<Channel> {
        let dir = crate::config::identity_dir().ok()?;
        let text = std::fs::read_to_string(dir.join("update-channel")).ok()?;
        Channel::parse(&text)
    }
}

/// Why the feed could not answer — three different facts, never folded.
#[derive(Debug)]
pub enum Failure {
    /// The channel could not be asked: network, DNS, HTTP failure, or a
    /// pointer that does not exist yet. Never "up to date".
    Unreachable(String),
    /// Bytes arrived and their signature did not verify against the pinned
    /// key. Worth acting on: either the feed host is compromised or a publish
    /// was made with the wrong key.
    Verification(String),
    /// Bytes arrived, verified, and then broke a rule: malformed JSON, a
    /// stable pointer naming a prerelease, a relocation chain, a
    /// pointer/manifest version disagreement.
    Invalid(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Unreachable(detail) => {
                write!(f, "the channel could not be asked: {detail}")
            }
            Failure::Verification(detail) => {
                write!(f, "feed signature verification failed: {detail}")
            }
            Failure::Invalid(detail) => write!(f, "feed object invalid: {detail}"),
        }
    }
}

impl std::error::Error for Failure {}

#[derive(Deserialize)]
struct Envelope {
    payload: String,
    signature: String,
}

/// What a channel pointer says, once its envelope has verified.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PointerPayload {
    /// The channel points at exactly one release.
    Release { version: String, manifest: String },
    /// The channel moved. Followed exactly once; a chain is refused.
    Moved { to: String },
}

/// A release manifest: the versions and artifacts of one immutable release.
/// Unknown fields are ignored so an older binary can still read a manifest
/// published by a newer pipeline.
#[derive(Deserialize, Debug)]
pub struct Manifest {
    pub version: String,
    /// The compatibility floor: the lowest version still permitted to run.
    /// Absent means no floor is published.
    #[serde(default)]
    pub min_supported: Option<String>,
    /// Bundle name → version for every bundle in the release (`lait`,
    /// `astrolabe`, …).
    #[serde(default)]
    pub bundles: BTreeMap<String, String>,
    /// Bundle name → target triple → artifact.
    #[serde(default)]
    pub artifacts: BTreeMap<String, BTreeMap<String, Artifact>>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Artifact {
    pub url: String,
    pub blake3: String,
    pub size: u64,
}

/// The pinned key as raw bytes. Decodes a compile-time constant; a test pins
/// the round-trip, so the error arm is unreachable in a build that passed CI —
/// but it is an error, not a panic, because the updater must never be the
/// thing that crashes a daemon.
pub fn pinned_pubkey() -> Result<[u8; 32], Failure> {
    let bytes = data_encoding::HEXLOWER
        .decode(FEED_PUBKEY_HEX.as_bytes())
        .map_err(|e| Failure::Invalid(format!("pinned key is not hex: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Failure::Invalid("pinned key is not 32 bytes".into()))
}

/// Open a signed envelope: verify, then hand back the exact payload bytes that
/// were signed. Shape errors are [`Failure::Invalid`]; a well-formed envelope
/// whose signature does not verify is [`Failure::Verification`].
pub fn open_envelope(bytes: &[u8], pubkey: &[u8; 32]) -> Result<Vec<u8>, Failure> {
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|e| Failure::Invalid(format!("envelope is not JSON: {e}")))?;
    let payload = data_encoding::BASE64
        .decode(envelope.payload.as_bytes())
        .map_err(|e| Failure::Invalid(format!("payload base64: {e}")))?;
    let signature: [u8; 64] = data_encoding::BASE64
        .decode(envelope.signature.as_bytes())
        .map_err(|e| Failure::Invalid(format!("signature base64: {e}")))?
        .try_into()
        .map_err(|_| Failure::Invalid("signature is not 64 bytes".into()))?;
    if !mechanics::actor::verify_detached(pubkey, &payload, &signature) {
        return Err(Failure::Verification(
            "envelope signature does not verify against the pinned key".into(),
        ));
    }
    Ok(payload)
}

/// A channel, resolved: the release it points at and the floor it publishes.
#[derive(Debug)]
pub struct Resolved {
    pub version: semver::Version,
    pub manifest: Manifest,
    /// The published floor, when one exists and is satisfiable.
    pub floor: Option<semver::Version>,
    /// The published floor exceeded the very release that shipped it — a
    /// defect the publish step exists to make impossible. The floor is ignored
    /// (never obeyed, never looped on) and this flag says so.
    pub floor_defect: bool,
}

/// Resolve a channel against the real feed.
pub fn resolve(channel: Channel) -> Result<Resolved, Failure> {
    resolve_with(
        |url| http_fetch(url, MAX_FEED_OBJECT),
        channel,
        FEED_BASE_URL,
        &pinned_pubkey()?,
    )
}

/// [`resolve`] with the fetch injected, which is what makes every rule below
/// testable without a socket.
pub fn resolve_with<F>(
    fetch: F,
    channel: Channel,
    base: &str,
    pubkey: &[u8; 32],
) -> Result<Resolved, Failure>
where
    F: Fn(&str) -> Result<Vec<u8>, Failure>,
{
    let pointer_url = format!(
        "{}/channels/{}",
        base.trim_end_matches('/'),
        channel.as_str()
    );
    let payload = open_envelope(&fetch(&pointer_url)?, pubkey)?;
    let pointer: PointerPayload = serde_json::from_slice(&payload)
        .map_err(|e| Failure::Invalid(format!("pointer payload: {e}")))?;

    let (version, manifest) = match pointer {
        PointerPayload::Release { version, manifest } => (version, manifest),
        PointerPayload::Moved { to } => {
            let payload = open_envelope(&fetch(&to)?, pubkey)?;
            let relocated: PointerPayload = serde_json::from_slice(&payload)
                .map_err(|e| Failure::Invalid(format!("relocated pointer payload: {e}")))?;
            match relocated {
                PointerPayload::Release { version, manifest } => (version, manifest),
                // One hop is a migration; two is a publish mistake or bait.
                PointerPayload::Moved { .. } => {
                    return Err(Failure::Invalid("relocation chain refused".into()))
                }
            }
        }
    };
    let version = semver::Version::parse(&version)
        .map_err(|e| Failure::Invalid(format!("pointer version {version:?}: {e}")))?;
    if channel == Channel::Stable && !version.pre.is_empty() {
        return Err(Failure::Invalid(format!(
            "the stable pointer names a prerelease ({version})"
        )));
    }

    let payload = open_envelope(&fetch(&manifest)?, pubkey)?;
    let manifest: Manifest = serde_json::from_slice(&payload)
        .map_err(|e| Failure::Invalid(format!("manifest payload: {e}")))?;
    if manifest.version != version.to_string() {
        return Err(Failure::Invalid(format!(
            "pointer names {version} but the manifest says {}",
            manifest.version
        )));
    }

    let (floor, floor_defect) = match &manifest.min_supported {
        None => (None, false),
        Some(text) => {
            let floor = semver::Version::parse(text)
                .map_err(|e| Failure::Invalid(format!("floor {text:?}: {e}")))?;
            if floor > version {
                // Unsatisfiable: no client, not even one that applies this very
                // release, can reach a version at or above it. Obeying it would
                // force every installed machine forever, so it is ignored and
                // reported — the runtime half of the guard whose publish half
                // lives in `lait-feed pointer`.
                tracing::warn!(
                    floor = %floor,
                    release = %version,
                    "feed published an unsatisfiable floor; ignoring it"
                );
                (None, true)
            } else {
                (Some(floor), false)
            }
        }
    };

    Ok(Resolved {
        version,
        manifest,
        floor,
        floor_defect,
    })
}

/// Fetch a URL fully into memory, refusing bodies over `limit`. Every failure
/// is [`Failure::Unreachable`]: a 404 pointer is a channel with nothing
/// published yet, and a refused connection is a channel that could not be
/// asked — neither is an answer.
pub fn http_fetch(url: &str, limit: u64) -> Result<Vec<u8>, Failure> {
    use std::io::Read;
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(300))
        .call()
        .map_err(|e| Failure::Unreachable(e.to_string()))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::Unreachable(format!("read body: {e}")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(Failure::Invalid(format!(
            "body exceeds {limit} bytes, not a feed object"
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde::Serialize;
    use std::collections::HashMap;

    /// The sealing half, mirrored from `tools/feed` (which must not be a
    /// dependency of this crate). This test module is what holds the two
    /// implementations to one format: `seal` here == `seal` there, and any
    /// drift breaks the round-trip below rather than an installed machine.
    pub(crate) fn seal(payload: &impl Serialize, seed: &[u8; 32]) -> String {
        let bytes = serde_json::to_vec(payload).unwrap();
        let signature = mechanics::actor::sign_detached(seed, &bytes);
        serde_json::json!({
            "payload": data_encoding::BASE64.encode(&bytes),
            "signature": data_encoding::BASE64.encode(&signature),
        })
        .to_string()
    }

    pub(crate) fn test_keypair() -> ([u8; 32], [u8; 32]) {
        let seed = [7u8; 32];
        let device = mechanics::actor::device_from_seed(&seed);
        let pubkey: [u8; 32] = data_encoding::HEXLOWER
            .decode(device.as_str().as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        (seed, pubkey)
    }

    fn manifest_json(version: &str, floor: Option<&str>) -> serde_json::Value {
        let mut m = serde_json::json!({
            "version": version,
            "bundles": {"lait": version},
            "artifacts": {"lait": {"x86_64-pc-windows-msvc": {
                "url": format!("https://feed.example/releases/{version}/lait-x86_64-pc-windows-msvc.zip"),
                "blake3": "00".repeat(32),
                "size": 1,
            }}},
        });
        if let Some(floor) = floor {
            m["min_supported"] = floor.into();
        }
        m
    }

    /// A feed served from a HashMap: url → sealed envelope text.
    fn feed_of(
        objects: &HashMap<String, String>,
    ) -> impl Fn(&str) -> Result<Vec<u8>, Failure> + '_ {
        move |url: &str| {
            objects
                .get(url)
                .map(|text| text.as_bytes().to_vec())
                .ok_or_else(|| Failure::Unreachable(format!("no object at {url}")))
        }
    }

    fn pointer_json(version: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "release",
            "version": version,
            "manifest": format!("https://feed.example/releases/{version}/manifest.json"),
        })
    }

    fn feed_with(
        seed: &[u8; 32],
        channel: &str,
        version: &str,
        floor: Option<&str>,
    ) -> HashMap<String, String> {
        HashMap::from([
            (
                format!("https://feed.example/channels/{channel}"),
                seal(&pointer_json(version), seed),
            ),
            (
                format!("https://feed.example/releases/{version}/manifest.json"),
                seal(&manifest_json(version, floor), seed),
            ),
        ])
    }

    #[test]
    fn the_pinned_key_is_well_formed() {
        // `pinned_pubkey` errors at runtime on a malformed constant; this is
        // what turns that dead arm into a red build instead of a daemon that
        // can never resolve its own feed.
        assert_eq!(pinned_pubkey().unwrap().len(), 32);
    }

    #[test]
    fn a_sealed_envelope_opens_and_a_tampered_one_is_a_verification_failure() {
        let (seed, pubkey) = test_keypair();
        let sealed = seal(&serde_json::json!({"hello": "feed"}), &seed);
        let opened = open_envelope(sealed.as_bytes(), &pubkey).unwrap();
        assert_eq!(
            opened,
            serde_json::to_vec(&serde_json::json!({"hello": "feed"})).unwrap()
        );

        // Tamper with the payload but keep the envelope well-formed: the
        // failure must be Verification, never Invalid and never silence.
        let mut envelope: serde_json::Value = serde_json::from_str(&sealed).unwrap();
        envelope["payload"] = data_encoding::BASE64.encode(br#"{"hello":"evil"}"#).into();
        let err = open_envelope(envelope.to_string().as_bytes(), &pubkey).unwrap_err();
        assert!(matches!(err, Failure::Verification(_)), "{err}");

        // A different pinned key refuses the honest envelope the same way.
        let other_pub: [u8; 32] = {
            let device = mechanics::actor::device_from_seed(&[9u8; 32]);
            data_encoding::HEXLOWER
                .decode(device.as_str().as_bytes())
                .unwrap()
                .try_into()
                .unwrap()
        };
        let err = open_envelope(sealed.as_bytes(), &other_pub).unwrap_err();
        assert!(matches!(err, Failure::Verification(_)), "{err}");
    }

    #[test]
    fn garbage_is_invalid_not_a_verification_failure() {
        let (_, pubkey) = test_keypair();
        let err = open_envelope(b"not json at all", &pubkey).unwrap_err();
        assert!(matches!(err, Failure::Invalid(_)), "{err}");
    }

    #[test]
    fn a_channel_resolves_to_the_release_its_pointer_names() {
        let (seed, pubkey) = test_keypair();
        let objects = feed_with(&seed, "test", "0.9.0-test.1", None);
        let resolved = resolve_with(
            feed_of(&objects),
            Channel::Test,
            "https://feed.example",
            &pubkey,
        )
        .unwrap();
        assert_eq!(resolved.version.to_string(), "0.9.0-test.1");
        assert!(resolved.floor.is_none());
        assert!(!resolved.floor_defect);
        assert!(resolved.manifest.artifacts["lait"].contains_key("x86_64-pc-windows-msvc"));
    }

    #[test]
    fn the_stable_channel_refuses_a_prerelease_even_when_the_pointer_names_one() {
        // The publish tool refuses to write this pointer; if one exists anyway,
        // the client is the second, independent, half of the rule.
        let (seed, pubkey) = test_keypair();
        let objects = feed_with(&seed, "stable", "0.9.0-test.1", None);
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &pubkey,
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Invalid(_)), "{err}");
    }

    #[test]
    fn a_pointer_and_manifest_that_disagree_on_version_are_refused() {
        let (seed, pubkey) = test_keypair();
        let mut objects = feed_with(&seed, "stable", "0.9.0", None);
        // Swap in a manifest claiming a different version, correctly signed —
        // signature is necessary but not sufficient.
        objects.insert(
            "https://feed.example/releases/0.9.0/manifest.json".into(),
            seal(&manifest_json("0.9.1", None), &seed),
        );
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &pubkey,
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Invalid(_)), "{err}");
    }

    #[test]
    fn a_relocation_is_followed_once_and_a_chain_is_refused() {
        let (seed, pubkey) = test_keypair();
        let mut objects = feed_with(&seed, "stable", "0.9.0", None);
        // The old URL carries a tombstone to the real pointer's new home.
        let real_pointer = objects
            .remove("https://feed.example/channels/stable")
            .unwrap();
        objects.insert(
            "https://feed.example/channels/stable".into(),
            seal(
                &serde_json::json!({"kind": "moved", "to": "https://newhost.example/channels/stable"}),
                &seed,
            ),
        );
        objects.insert(
            "https://newhost.example/channels/stable".into(),
            real_pointer,
        );
        let resolved = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &pubkey,
        )
        .unwrap();
        assert_eq!(resolved.version.to_string(), "0.9.0");

        // A relocation that answers with another relocation is refused: one
        // hop is a migration, a chain is a publish mistake or bait.
        objects.insert(
            "https://newhost.example/channels/stable".into(),
            seal(
                &serde_json::json!({"kind": "moved", "to": "https://a-third.example/channels/stable"}),
                &seed,
            ),
        );
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &pubkey,
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Invalid(_)), "{err}");
    }

    #[test]
    fn a_floor_is_carried_and_an_unsatisfiable_floor_is_ignored_and_flagged() {
        let (seed, pubkey) = test_keypair();
        let objects = feed_with(&seed, "stable", "0.9.0", Some("0.8.0"));
        let resolved = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &pubkey,
        )
        .unwrap();
        assert_eq!(resolved.floor.unwrap().to_string(), "0.8.0");
        assert!(!resolved.floor_defect);

        // The floor the publish gate exists to make impossible: above the very
        // release that carries it. Obeyed, it would force every machine
        // forever, so it is ignored and reported — never looped on.
        let objects = feed_with(&seed, "stable", "0.9.0", Some("0.9.1"));
        let resolved = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &pubkey,
        )
        .unwrap();
        assert!(resolved.floor.is_none());
        assert!(resolved.floor_defect);
    }

    #[test]
    fn an_unreachable_pointer_stays_unreachable_never_up_to_date() {
        let (_, pubkey) = test_keypair();
        let objects = HashMap::new();
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &pubkey,
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Unreachable(_)), "{err}");
    }

    /// The cross-implementation check the unit tests structurally cannot make:
    /// every envelope above was sealed by THIS module's test mirror, but the
    /// feed is published by `tools/feed`. This resolves the real test channel
    /// on the real host with the real pinned key — tool-sealed, lib-opened.
    /// `#[ignore]` because it needs the network and a published test release;
    /// run it by hand after a publish, and in CI once the `updater-contract`
    /// job repoints from the GitHub mirror to the feed.
    #[test]
    #[ignore = "needs network and a published test-channel release"]
    fn the_real_test_channel_resolves_with_the_pinned_key() {
        let resolved = resolve(Channel::Test).expect("resolve the live test channel");
        assert!(
            resolved.manifest.artifacts["lait"].len() >= 5,
            "a release ships every target"
        );
        assert!(!resolved.floor_defect);
    }

    #[test]
    fn channel_parse_accepts_exactly_the_two_channels() {
        assert_eq!(Channel::parse("stable"), Some(Channel::Stable));
        assert_eq!(Channel::parse(" test\n"), Some(Channel::Test));
        assert_eq!(Channel::parse("beta"), None);
        assert_eq!(Channel::parse(""), None);
    }
}
