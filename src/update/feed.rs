//! The consume half of the release feed (SUB-13): signed channel pointers,
//! release manifests, and the rules for believing them.
//!
//! The feed is *proven, not trusted*. It lives on a plain object host with no
//! ambient authority, so nothing here is believed before its signature
//! verifies against [`FEED_PUBKEYS_HEX`], the keys pinned into this binary at
//! build time. A host compromise that rewrites pointers or manifests yields
//! refusals, not installs.
//!
//! Trust is a *set*, not a key, because a single pinned key is a single point
//! of failure with no recovery: the binary that would carry a replacement can
//! only reach an installed machine through the feed the lost key signs. See
//! [`FEED_PUBKEYS_HEX`] for the rotation procedure and for what this does and
//! does not survive.
//!
//! Two kinds of object, and only one ever changes. Immutable releases live
//! under `/releases/<version>/` — artifacts, digests, and a signed manifest.
//! Mutable channel pointers live at `/channels/<channel>`, served no-cache,
//! each naming exactly one release. Promotion is pointer motion. A pointer may
//! instead carry a signed *relocation* ("this channel moved"), followed
//! exactly once, so the pointer URL itself is never a permanent commitment.
//!
//! A signature proves who wrote a pointer and not *when*, so every pointer
//! carries `published_at` and a node refuses one older than the newest it has
//! already believed. Without that, anyone able to replace the object with an
//! older correctly-signed copy freezes this machine at that release while
//! every other check still passes. Freshness ratchets: once a stamped pointer
//! has been seen, an unstamped one is refused too, so the defence cannot be
//! stepped back off by replaying something from before it shipped.
//!
//! Error shape is load-bearing: [`Failure::Unreachable`] is "the channel
//! could not be asked", [`Failure::Verification`] is a signature that failed,
//! [`Failure::Stale`] is a pointer older than one already seen, and none of
//! them may ever be rendered as "up to date". The distinction is the same
//! absence law the client's surfaces hold everywhere else.
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

/// The feed's verifying keys. Each signing seed exists in exactly one place —
/// a maintainer's custody, minted by `lait-feed keygen` — and never on a build
/// machine or in the repository. An envelope is believed when *any* key here
/// verifies it.
///
/// # Why a set
///
/// One pinned key cannot be rotated. Lose the seed and every installed machine
/// is stranded forever, because the only way to deliver a binary carrying a
/// replacement key is the feed that the lost key alone can sign. Pinning a set
/// turns an unrecoverable loss into an ordinary publish with a different seed.
///
/// # Rotating
///
/// Four steps, none of which asks an installed machine to trust something it
/// did not already trust:
///
/// 1. `lait-feed keygen` into separate custody, and add the public key here.
/// 2. Ship that build, and let the fleet adopt it. Until a machine has this
///    release, it does not know the successor — so nothing may be signed with
///    the successor alone before then.
/// 3. Begin signing with the successor.
/// 4. In a later release, drop the predecessor from this list.
///
/// Adding a key is now a data edit rather than a change of type, callsites and
/// tests. That is the point: rotation must not be a refactor performed under
/// incident pressure.
///
/// # What this does not survive
///
/// Compromise. Every key here is trusted equally and independently, so a stolen
/// seed signs releases every installed machine accepts, and removing it still
/// requires shipping a build — through a feed the thief can also sign.
/// Recovering from theft without shipping a binary needs key statements carried
/// *in* the feed and changed only by a quorum, which is deliberately not this
/// change.
pub const FEED_PUBKEYS_HEX: &[&str] = &[
    // Minted 2026-08, the key every published release is signed with today.
    "227e448a16c19623707a3da8b8af6e1f70afcf18fb4e509e82115ef797666ba9",
    // Successor, minted 2026-08-15 into separate custody and not yet used to
    // sign anything. It is here first on purpose: a machine can only accept a
    // key it already carries, so the successor must reach the fleet before it
    // signs, never alongside. Step 3 of the rotation waits for this build.
    "6397aa15cd939de1109abf2c265147201eb7d189029c2d0137d917292d689e50",
];

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
    /// A verified pointer older than one this node has already seen, or one
    /// that dropped the freshness stamp after a stamped pointer was seen.
    ///
    /// Its own outcome because it is the only failure here that a *signature*
    /// cannot catch. Anyone able to replace the pointer object with an older
    /// correctly-signed copy they kept freezes this node at that release, and
    /// every other check passes: the envelope verifies, the manifest agrees,
    /// the version parses. Folded into any other arm it would read as "no new
    /// release", which is exactly the silence the attack buys.
    Stale(String),
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
            Failure::Stale(detail) => write!(f, "feed answered with a stale pointer: {detail}"),
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
///
/// `published_at` (unix seconds, stamped by `lait-feed pointer`) is what makes
/// a pointer un-replayable. It is optional because pointers published before
/// it existed carry no stamp, and refusing those outright would strand every
/// machine now installed. See [`check_freshness`] for how absence ratchets
/// into refusal rather than staying a permanent hole.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PointerPayload {
    /// The channel points at exactly one release.
    Release {
        version: String,
        manifest: String,
        #[serde(default)]
        published_at: Option<u64>,
    },
    /// The channel moved. Followed exactly once; a chain is refused.
    Moved {
        to: String,
        #[serde(default)]
        published_at: Option<u64>,
    },
}

impl PointerPayload {
    pub(crate) fn published_at(&self) -> Option<u64> {
        match self {
            PointerPayload::Release { published_at, .. } => *published_at,
            PointerPayload::Moved { published_at, .. } => *published_at,
        }
    }
}

/// The freshness ratchet: a pointer may never be older than the newest one
/// this node has already believed.
///
/// The rule has three cases and the third is the one that matters. Having seen
/// no stamp, anything is accepted — that is the unavoidable state of a machine
/// installed before stamping existed, and it is honest rather than safe.
/// Having seen a stamp, an older stamp is refused. And having seen a stamp, a
/// pointer carrying *no* stamp is also refused, because otherwise the whole
/// defence is bypassed by replaying something from before it shipped.
///
/// So protection engages permanently the first time a node sees a stamped
/// pointer, and cannot be walked back off.
fn check_freshness(
    pointer: &PointerPayload,
    seen: Option<u64>,
    what: &str,
) -> Result<Option<u64>, Failure> {
    let stamped = pointer.published_at();
    match (seen, stamped) {
        (Some(seen), None) => Err(Failure::Stale(format!(
            "{what} carries no publish time, but this node has already believed \
             one published at {seen}; a pointer cannot lose its stamp"
        ))),
        (Some(seen), Some(now)) if now < seen => Err(Failure::Stale(format!(
            "{what} was published at {now}, older than the {seen} already seen \
             on this channel"
        ))),
        _ => Ok(stamped),
    }
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

/// The pinned keys as raw bytes. Decodes a compile-time constant; a test pins
/// the round-trip, so the error arms are unreachable in a build that passed CI
/// — but they are errors, not panics, because the updater must never be the
/// thing that crashes a daemon.
///
/// An empty set and a duplicated key are both refused. Neither is reachable
/// through an honest edit, and both are exactly what a hurried rotation
/// produces: an empty set trusts nothing while looking like configuration, and
/// a duplicate hides that a key was replaced rather than added.
pub fn pinned_pubkeys() -> Result<Vec<[u8; 32]>, Failure> {
    if FEED_PUBKEYS_HEX.is_empty() {
        return Err(Failure::Invalid("no feed key is pinned".into()));
    }
    let mut keys = Vec::with_capacity(FEED_PUBKEYS_HEX.len());
    for hex in FEED_PUBKEYS_HEX {
        let bytes = data_encoding::HEXLOWER
            .decode(hex.as_bytes())
            .map_err(|e| Failure::Invalid(format!("pinned key {hex} is not hex: {e}")))?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Failure::Invalid(format!("pinned key {hex} is not 32 bytes")))?;
        if keys.contains(&key) {
            return Err(Failure::Invalid(format!("pinned key {hex} appears twice")));
        }
        keys.push(key);
    }
    Ok(keys)
}

/// Open a signed envelope: verify against any pinned key, then hand back the
/// exact payload bytes that were signed. Shape errors are [`Failure::Invalid`];
/// a well-formed envelope that no pinned key verifies is
/// [`Failure::Verification`].
///
/// Any key is sufficient. That is what makes rotation an overlap rather than a
/// flag day — during step 3 above, the fleet holds a mixture of builds and both
/// the predecessor and the successor must open the same feed.
pub fn open_envelope(bytes: &[u8], pubkeys: &[[u8; 32]]) -> Result<Vec<u8>, Failure> {
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
    if !pubkeys
        .iter()
        .any(|key| mechanics::actor::verify_detached(key, &payload, &signature))
    {
        return Err(Failure::Verification(format!(
            "envelope signature verifies against none of the {} pinned keys",
            pubkeys.len()
        )));
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
    /// When the pointer that produced this answer was published, if it was
    /// stamped. `None` means a feed that predates stamping — no replay
    /// protection is in force yet on this channel, and that is worth surfacing
    /// rather than assuming.
    pub published_at: Option<u64>,
}

/// The file holding the newest publish time believed on a channel. Beside the
/// identity, one per channel, because following stable and test are different
/// histories and a node may switch between them.
fn seen_path(channel: Channel) -> Option<std::path::PathBuf> {
    crate::config::identity_dir()
        .ok()
        .map(|dir| dir.join(format!("update-pointer-{}", channel.as_str())))
}

/// The newest publish time already believed on this channel, if any. A missing
/// or unreadable record is `None` — the conservative direction is to accept and
/// re-arm, never to refuse every update because a file went missing.
fn seen_published_at(channel: Channel) -> Option<u64> {
    let text = std::fs::read_to_string(seen_path(channel)?).ok()?;
    text.trim().parse().ok()
}

/// Record a publish time as believed. Advances only; a failure to write is
/// loud, because a node that silently cannot persist this has no replay
/// protection while looking exactly like one that does.
fn record_published_at(channel: Channel, at: u64) {
    let Some(path) = seen_path(channel) else {
        return;
    };
    if seen_published_at(channel).is_some_and(|seen| seen >= at) {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&path, at.to_string()) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not record the feed's publish time; this node cannot detect a replayed pointer"
        );
    }
}

/// Resolve a channel against the real feed.
pub fn resolve(channel: Channel) -> Result<Resolved, Failure> {
    let resolved = resolve_with(
        |url| http_fetch(url, MAX_FEED_OBJECT),
        channel,
        FEED_BASE_URL,
        &pinned_pubkeys()?,
        seen_published_at(channel),
    )?;
    if let Some(at) = resolved.published_at {
        record_published_at(channel, at);
    }
    Ok(resolved)
}

/// [`resolve`] with the fetch injected, which is what makes every rule below
/// testable without a socket.
///
/// `seen` is the newest publish time this node has already believed on this
/// channel, which is what the ratchet compares against. Persisting it is the
/// caller's job so this function stays pure.
pub fn resolve_with<F>(
    fetch: F,
    channel: Channel,
    base: &str,
    pubkeys: &[[u8; 32]],
    seen: Option<u64>,
) -> Result<Resolved, Failure>
where
    F: Fn(&str) -> Result<Vec<u8>, Failure>,
{
    let pointer_url = format!(
        "{}/channels/{}",
        base.trim_end_matches('/'),
        channel.as_str()
    );
    resolve_pointer_with(fetch, &pointer_url, channel, pubkeys, seen)
}

/// [`resolve_with`] against a pointer at an explicit URL.
///
/// The product's channels sit at `channels/<channel>`; a World's sit one level
/// in, at `channels/worlds/<world>/<channel>`, because a World ships on its own
/// cadence and its pointer is its own mutable object. Every rule below is the
/// same either way — the signature, the one-hop relocation, the freshness
/// ratchet, and the stable-never-prerelease refusal — which is the whole
/// reason the World feed reuses this layout instead of inventing a second one.
///
/// `channel` is still passed because the prerelease rule is a property of the
/// channel a node follows, not of the URL it was found at.
pub fn resolve_pointer_with<F>(
    fetch: F,
    pointer_url: &str,
    channel: Channel,
    pubkeys: &[[u8; 32]],
    seen: Option<u64>,
) -> Result<Resolved, Failure>
where
    F: Fn(&str) -> Result<Vec<u8>, Failure>,
{
    let payload = open_envelope(&fetch(pointer_url)?, pubkeys)?;
    let pointer: PointerPayload = serde_json::from_slice(&payload)
        .map_err(|e| Failure::Invalid(format!("pointer payload: {e}")))?;

    // Every pointer on the path is ratcheted, not just the last one. A
    // relocation record is an object an attacker can replace too, so leaving
    // it unchecked would move the replay one hop upstream rather than close it.
    let mut freshest = check_freshness(&pointer, seen, "the channel pointer")?;

    let (version, manifest) = match pointer {
        PointerPayload::Release {
            version, manifest, ..
        } => (version, manifest),
        PointerPayload::Moved { to, .. } => {
            let payload = open_envelope(&fetch(&to)?, pubkeys)?;
            let relocated: PointerPayload = serde_json::from_slice(&payload)
                .map_err(|e| Failure::Invalid(format!("relocated pointer payload: {e}")))?;
            freshest = freshest.max(check_freshness(&relocated, seen, "the relocated pointer")?);
            match relocated {
                PointerPayload::Release {
                    version, manifest, ..
                } => (version, manifest),
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

    let payload = open_envelope(&fetch(&manifest)?, pubkeys)?;
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
        published_at: freshest,
    })
}

/// Fetch a URL fully into memory, refusing bodies over `limit`. Every failure
/// is [`Failure::Unreachable`]: a 404 pointer is a channel with nothing
/// published yet, and a refused connection is a channel that could not be
/// asked — neither is an answer.
pub fn http_fetch(url: &str, limit: u64) -> Result<Vec<u8>, Failure> {
    http_fetch_with_progress(url, limit, |_, _| {})
}

/// Fetch a URL while reporting how many bytes have arrived.
///
/// The caller supplies the signed manifest's size as `limit`, so the progress
/// denominator is an authenticated claim rather than an HTTP header supplied
/// by the object host. The final size and digest are still checked by the
/// installer after this returns; progress never becomes authority.
pub fn http_fetch_with_progress(
    url: &str,
    limit: u64,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>, Failure> {
    use std::io::Read;
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(300))
        .call()
        .map_err(|e| Failure::Unreachable(e.to_string()))?;
    let mut bytes = Vec::new();
    let mut reader = response.into_reader().take(limit.saturating_add(1));
    // A bounded heap buffer keeps large artifacts off the stack and avoids
    // flooding a native view with one progress frame per network packet.
    let mut chunk = vec![0_u8; 256 * 1024];
    progress(0, limit);
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|e| Failure::Unreachable(format!("read body: {e}")))?;
        if read == 0 {
            break;
        }
        let received = chunk.get(..read).ok_or_else(|| {
            Failure::Unreachable("reader returned more bytes than its buffer can hold".into())
        })?;
        bytes.extend_from_slice(received);
        progress(u64::try_from(bytes.len()).unwrap_or(u64::MAX), limit);
    }
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

    fn stamped_pointer_json(version: &str, published_at: u64) -> serde_json::Value {
        let mut pointer = pointer_json(version);
        pointer["published_at"] = published_at.into();
        pointer
    }

    /// A feed whose pointer carries a publish time.
    fn stamped_feed(
        seed: &[u8; 32],
        channel: &str,
        version: &str,
        published_at: u64,
    ) -> HashMap<String, String> {
        let mut objects = feed_with(seed, channel, version, None);
        objects.insert(
            format!("https://feed.example/channels/{channel}"),
            seal(&stamped_pointer_json(version, published_at), seed),
        );
        objects
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
    fn the_pinned_keys_are_well_formed_and_distinct() {
        // `pinned_pubkeys` errors at runtime on a malformed constant; this is
        // what turns those dead arms into a red build instead of a daemon that
        // can never resolve its own feed. It also catches the two edits a
        // hurried rotation actually produces — an emptied list, or a key
        // pasted twice instead of added.
        let keys = pinned_pubkeys().unwrap();
        assert!(!keys.is_empty(), "a build must trust at least one feed key");
        for key in &keys {
            assert_eq!(key.len(), 32);
        }
    }

    #[test]
    fn any_pinned_key_opens_an_envelope_which_is_what_makes_rotation_an_overlap() {
        // The property the whole key set exists for: during a rotation the
        // fleet holds a mixture of builds, and one feed must satisfy both the
        // predecessor and the successor. Neither key is privileged.
        let (old_seed, old_pub) = test_keypair();
        let new_seed = [11u8; 32];
        let new_pub: [u8; 32] = data_encoding::HEXLOWER
            .decode(
                mechanics::actor::device_from_seed(&new_seed)
                    .as_str()
                    .as_bytes(),
            )
            .unwrap()
            .try_into()
            .unwrap();
        let trusted = [old_pub, new_pub];

        for seed in [old_seed, new_seed] {
            let sealed = seal(&serde_json::json!({"hello": "feed"}), &seed);
            let opened = open_envelope(sealed.as_bytes(), &trusted).unwrap();
            assert_eq!(
                opened,
                serde_json::to_vec(&serde_json::json!({"hello": "feed"})).unwrap()
            );
        }

        // A build that has not yet learned the successor refuses what the
        // successor signed. This is why step 3 of the rotation waits for
        // adoption: signing with a key the fleet does not hold is an outage,
        // and it must present as a refusal rather than as "up to date".
        let sealed = seal(&serde_json::json!({"hello": "feed"}), &new_seed);
        let err = open_envelope(sealed.as_bytes(), &[old_pub]).unwrap_err();
        assert!(matches!(err, Failure::Verification(_)), "{err}");

        // And a seed in nobody's custody is refused by the whole set, not
        // merely by one member of it.
        let sealed = seal(&serde_json::json!({"hello": "feed"}), &[13u8; 32]);
        let err = open_envelope(sealed.as_bytes(), &trusted).unwrap_err();
        assert!(matches!(err, Failure::Verification(_)), "{err}");
    }

    #[test]
    fn a_channel_resolves_when_a_successor_key_signed_it() {
        // The end-to-end shape of step 3: the pointer and manifest are sealed
        // by a key that is in the set but was not the original, and a client
        // carrying both keys resolves normally.
        let successor = [11u8; 32];
        let (_, old_pub) = test_keypair();
        let successor_pub: [u8; 32] = data_encoding::HEXLOWER
            .decode(
                mechanics::actor::device_from_seed(&successor)
                    .as_str()
                    .as_bytes(),
            )
            .unwrap()
            .try_into()
            .unwrap();
        let objects = feed_with(&successor, "stable", "0.9.0", None);
        let resolved = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &[old_pub, successor_pub],
            None,
        )
        .unwrap();
        assert_eq!(resolved.version.to_string(), "0.9.0");
    }

    #[test]
    fn a_sealed_envelope_opens_and_a_tampered_one_is_a_verification_failure() {
        let (seed, pubkey) = test_keypair();
        let sealed = seal(&serde_json::json!({"hello": "feed"}), &seed);
        let opened = open_envelope(sealed.as_bytes(), &[pubkey]).unwrap();
        assert_eq!(
            opened,
            serde_json::to_vec(&serde_json::json!({"hello": "feed"})).unwrap()
        );

        // Tamper with the payload but keep the envelope well-formed: the
        // failure must be Verification, never Invalid and never silence.
        let mut envelope: serde_json::Value = serde_json::from_str(&sealed).unwrap();
        envelope["payload"] = data_encoding::BASE64.encode(br#"{"hello":"evil"}"#).into();
        let err = open_envelope(envelope.to_string().as_bytes(), &[pubkey]).unwrap_err();
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
        let err = open_envelope(sealed.as_bytes(), &[other_pub]).unwrap_err();
        assert!(matches!(err, Failure::Verification(_)), "{err}");
    }

    #[test]
    fn garbage_is_invalid_not_a_verification_failure() {
        let (_, pubkey) = test_keypair();
        let err = open_envelope(b"not json at all", &[pubkey]).unwrap_err();
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
            &[pubkey],
            None,
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
            &[pubkey],
            None,
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
            &[pubkey],
            None,
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
            &[pubkey],
            None,
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
            &[pubkey],
            None,
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
            &[pubkey],
            None,
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
            &[pubkey],
            None,
        )
        .unwrap();
        assert!(resolved.floor.is_none());
        assert!(resolved.floor_defect);
    }

    #[test]
    fn a_replayed_pointer_is_stale_and_never_reads_as_no_new_release() {
        // The attack the stamp exists for: an older, correctly-signed pointer
        // put back in place. Every other check passes — the envelope verifies,
        // the manifest agrees, the version parses — so without this rule the
        // node concludes there is simply nothing newer, which is precisely the
        // silence the attacker is buying.
        let (seed, pubkey) = test_keypair();
        let objects = stamped_feed(&seed, "stable", "0.9.0", 1_000);
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &[pubkey],
            Some(2_000),
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Stale(_)), "{err}");

        // The same pointer is fine for a node that has seen nothing newer.
        let resolved = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &[pubkey],
            Some(1_000),
        )
        .unwrap();
        assert_eq!(resolved.published_at, Some(1_000));
    }

    #[test]
    fn a_pointer_may_not_drop_its_stamp_once_one_has_been_seen() {
        // Otherwise the whole defence is bypassed by replaying any pointer
        // published before stamping existed.
        let (seed, pubkey) = test_keypair();
        let objects = feed_with(&seed, "stable", "0.9.0", None);
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &[pubkey],
            Some(1_000),
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Stale(_)), "{err}");

        // A node that has never seen a stamp still accepts an unstamped
        // pointer: that is the machine installed before stamping shipped, and
        // refusing it would strand the very fleet this protects.
        let resolved = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &[pubkey],
            None,
        )
        .unwrap();
        assert!(resolved.published_at.is_none());
    }

    #[test]
    fn a_replayed_relocation_record_is_stale_too() {
        // The relocation is an object an attacker can replace as well, so
        // ratcheting only the final pointer would move the replay one hop
        // upstream rather than close it.
        let (seed, pubkey) = test_keypair();
        let mut objects = stamped_feed(&seed, "stable", "0.9.0", 5_000);
        let real_pointer = objects
            .remove("https://feed.example/channels/stable")
            .unwrap();
        objects.insert(
            "https://feed.example/channels/stable".into(),
            seal(
                &serde_json::json!({
                    "kind": "moved",
                    "to": "https://newhost.example/channels/stable",
                    "published_at": 1_000,
                }),
                &seed,
            ),
        );
        objects.insert(
            "https://newhost.example/channels/stable".into(),
            real_pointer,
        );
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &[pubkey],
            Some(4_000),
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Stale(_)), "{err}");
    }

    #[test]
    fn an_unreachable_pointer_stays_unreachable_never_up_to_date() {
        let (_, pubkey) = test_keypair();
        let objects = HashMap::new();
        let err = resolve_with(
            feed_of(&objects),
            Channel::Stable,
            "https://feed.example",
            &[pubkey],
            None,
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
