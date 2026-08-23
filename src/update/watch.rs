//! Continuous, silent staging in the daemon (CLIENT-66).
//!
//! The always-on daemon is this product's resident updater — the role Omaha
//! plays for Chrome and the bootstrapper plays for Steam. On a jittered
//! period it resolves the channel, and when the channel holds something
//! newer it stages that release's tree beside the live one. Nothing is
//! prompted, nothing is applied, and nothing downloaded ever runs: applying
//! is the stub's act at a launch, and this half only ever leaves bytes on
//! disk that a later launch may or may not accept.
//!
//! ## The standing is a fact on disk, not a message
//!
//! Every check writes what it learned to `update-standing.json` under the
//! identity directory, and the staged tree's own manifest is the authority
//! for whether something is waiting. Together they answer "what does this
//! machine know about the channel" without the client and the daemon having
//! to be running at the same moment, and without a subscription that could
//! be missed. Absence is absence: a machine that has never completed a check
//! has no file, which is neither "up to date" nor "could not ask".
//!
//! ## Why the period is jittered
//!
//! A fleet that checks on a round number checks together. Chrome's updater
//! stretches its period by 20% one time in ten and delays each check by a
//! random fraction of a minute for exactly this reason; the same shape is
//! here, drawn from `getrandom` because this workspace deliberately carries
//! no `rand`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{feed, tree};

/// The base period between channel checks. Chrome's updater uses 4.5 hours
/// and has the fleet-scale evidence; there is no reason to invent another
/// number.
pub const CHECK_PERIOD: Duration = Duration::from_secs(4 * 60 * 60 + 30 * 60);

/// The most a period is ever stretched, as a fraction of itself.
const MAX_STRETCH: f64 = 0.2;

/// The most a single check is ever delayed after its period elapses.
const MAX_SPREAD: Duration = Duration::from_secs(60);

/// Where the standing is recorded under the identity directory.
pub const STANDING_FILE: &str = "update-standing.json";

/// What this machine knows about its channel, as of the last completed
/// check. Absence of a standing is a fourth thing and is never encoded here:
/// a machine that has never finished a check has no file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "standing", rename_all = "snake_case")]
pub enum Standing {
    /// The channel holds nothing this build does not already have.
    Current {
        /// What the channel points at, which this build is at or beyond.
        channel_version: String,
    },
    /// Something newer exists and is not staged — the state between a check
    /// finding a release and the download finishing, and the state a machine
    /// sits in when staging failed for a reason worth retrying.
    Available {
        /// The newer version the channel names.
        version: String,
    },
    /// This build is below the channel's compatibility floor. Crossing is not
    /// staged: the canonical installer must replace the owned release trees.
    ReinstallRequired {
        /// The release the channel names.
        version: String,
        /// The oldest client the release permits.
        floor: String,
    },
    /// A verified tree is on disk waiting for a launch to accept it.
    Staged {
        /// The version that will be live after the next launch.
        version: String,
        /// When it was staged, unix seconds.
        ///
        /// Recorded because the only question a person is ever asked on this
        /// path is *when to restart*, and how insistently to ask depends on
        /// how long a release has been waiting — not on how new it is. A
        /// staged release with no timestamp could only ever be asked about at
        /// one volume.
        #[serde(default)]
        at: u64,
    },
    /// The channel could not be asked. Never rendered as up to date.
    CouldNotAsk {
        /// The reason, as the feed named it.
        why: String,
    },
    /// Bytes arrived and did not verify, or broke a rule. Categorically not
    /// "could not ask": it means the host is compromised or a publish used
    /// the wrong key.
    Refused {
        /// The reason, as the feed named it.
        why: String,
    },
    /// A verified pointer older than one this node already believed. The one
    /// failure a signature cannot catch, and the one most likely to be folded
    /// into "no new release" — which is exactly the silence the attack buys.
    Stale {
        /// The reason, as the feed named it.
        why: String,
    },
}

impl Standing {
    /// The version waiting to become live, when one is.
    pub fn staged_version(&self) -> Option<&str> {
        match self {
            Self::Staged { version, .. } => Some(version),
            _ => None,
        }
    }

    /// How long a staged release has been waiting, at `now` (unix seconds).
    pub fn staged_for(&self, now: u64) -> Option<std::time::Duration> {
        match self {
            Self::Staged { at, .. } => {
                Some(std::time::Duration::from_secs(now.saturating_sub(*at)))
            }
            _ => None,
        }
    }
}

/// The install root of a stub-managed client, discovered from a path inside
/// it: this binary at `<root>/current/lait`, the stub at `<root>/astrolabe`
/// — the stub's *installed* name, which every installer renames it to so
/// shell artifacts point at a path no update moves.
///
/// Both halves are checked, because "my grandparent directory exists" is
/// true of every binary everywhere: a developer's `target/debug/lait` must
/// not be read as an installation and staged into.
pub fn install_root_of(executable: &Path) -> Option<PathBuf> {
    let live = executable.parent()?;
    if live.file_name()? != tree::LIVE_DIR {
        return None;
    }
    let root = live.parent()?;
    let stub = root.join(if cfg!(windows) {
        "astrolabe.exe"
    } else {
        "astrolabe"
    });
    stub.is_file().then(|| root.to_path_buf())
}

/// The install root of the running binary, when it is stub-managed.
pub fn install_root() -> Option<PathBuf> {
    install_root_of(&std::env::current_exe().ok()?)
}

/// The application bundle this binary ships inside, on macOS.
///
/// The one platform with no stub: the person put `Astrolabe.app` where they
/// wanted it, Launch Services and the Dock key on that path, and the bundle
/// itself is what an update replaces. So the live application is found by
/// asking where this daemon is running from rather than by a layout rule —
/// `/Applications` is a convention, not a guarantee.
#[cfg(target_os = "macos")]
pub fn live_bundle_of(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|ancestor| {
            ancestor
                .extension()
                .is_some_and(|extension| extension == "app")
        })
        .map(Path::to_path_buf)
}

/// The application bundle the running daemon ships inside.
#[cfg(target_os = "macos")]
pub fn live_bundle() -> Option<PathBuf> {
    live_bundle_of(&std::env::current_exe().ok()?)
}

/// Where a bundle installation keeps `staged/`, `previous/` and the stage
/// manifest. Beside the identity, because a `.app` has no install root to put
/// them in and must not grow one inside its signed seal.
#[cfg(target_os = "macos")]
const BUNDLE_STAGING_DIR: &str = "client-update";

/// Where this installation stages client releases, whichever shape it has.
///
/// A stub-managed installation stages inside its own root. A macOS bundle has
/// no root — the person put the application where they wanted it — so it
/// stages beside the identity and the two bundles are exchanged.
///
/// `None` is neither shape: a developer's build tree or a standalone `lait`,
/// where staging has nowhere to go and inventing one would drop a client tree
/// beside somebody's `target/`.
pub fn staging_root_of(executable: &Path, identity: &Path) -> Option<PathBuf> {
    if let Some(root) = install_root_of(executable) {
        return Some(root);
    }
    #[cfg(target_os = "macos")]
    if live_bundle_of(executable).is_some() {
        return Some(identity.join(BUNDLE_STAGING_DIR));
    }
    let _ = identity;
    None
}

/// [`staging_root_of`] for the running binary.
pub fn staging_root(identity: &Path) -> Option<PathBuf> {
    staging_root_of(&std::env::current_exe().ok()?, identity)
}

/// Now, in unix seconds.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Read the recorded standing, or `None` when no check has ever completed.
pub fn standing(identity: &Path) -> Option<Standing> {
    let bytes = std::fs::read(identity.join(STANDING_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Record what a check learned, replacing whatever was there.
///
/// Best effort: a standing that cannot be written is a fact nobody can read,
/// which is bad, but failing the check over it would turn a full disk into a
/// machine that stops updating rather than one that stops explaining.
fn record(identity: &Path, standing: &Standing) {
    let Ok(encoded) = serde_json::to_vec_pretty(standing) else {
        return;
    };
    let staged = identity.join(format!("{STANDING_FILE}.tmp-{}", std::process::id()));
    if std::fs::write(&staged, encoded).is_ok()
        && std::fs::rename(&staged, identity.join(STANDING_FILE)).is_err()
    {
        let _ = std::fs::remove_file(&staged);
    }
}

/// One check: resolve the channel, and stage the tree when it holds
/// something newer than what is installed here.
///
/// Blocking (HTTP, archive extract, file writes), so callers on a reactor
/// must hand it to `spawn_blocking`. Injected fetch and channel for the same
/// reason the rest of this module's neighbours inject theirs.
pub fn check_with<R, F>(
    resolve: R,
    fetch: F,
    current: &semver::Version,
    target: &str,
    root: &Path,
    previous: Option<Standing>,
) -> Standing
where
    R: FnOnce() -> Result<feed::Resolved, feed::Failure>,
    F: Fn(&str, u64) -> Result<Vec<u8>, feed::Failure>,
{
    let resolved = match resolve() {
        Ok(resolved) => resolved,
        Err(failure) => {
            let why = failure.to_string();
            return match failure {
                feed::Failure::Unreachable(_) => Standing::CouldNotAsk { why },
                feed::Failure::Stale(_) => Standing::Stale { why },
                feed::Failure::Verification(_) | feed::Failure::Invalid(_) => {
                    Standing::Refused { why }
                }
            };
        }
    };

    if resolved.version <= *current {
        return Standing::Current {
            channel_version: resolved.version.to_string(),
        };
    }

    if let Some(floor) = resolved
        .floor
        .as_ref()
        .filter(|floor| *floor <= &resolved.version && current < *floor)
    {
        return Standing::ReinstallRequired {
            version: resolved.version.to_string(),
            floor: floor.to_string(),
        };
    }

    // A tree already staged at this version is the answer; re-downloading it
    // every period would be bytes spent to learn nothing.
    if let Some(staged) = tree::staged_version(root) {
        if staged == resolved.version.to_string() {
            // The clock is *not* reset. A period that re-observes the same
            // staged release must leave its age alone, or a release waiting a
            // week would look four hours old at every check and the
            // escalation would never escalate.
            let at = previous
                .and_then(|standing| match standing {
                    Standing::Staged { version, at, .. } if version == staged => Some(at),
                    _ => None,
                })
                .unwrap_or_else(now);
            return Standing::Staged {
                version: staged,
                at,
            };
        }
    }

    match tree::stage_tree_with(fetch, &resolved, target, root) {
        Ok(staged) => Standing::Staged {
            version: staged.version,
            at: now(),
        },
        Err(error) => {
            // The release exists and this machine could not take it. That is
            // "available", not "current" and not "could not ask" — the
            // channel answered, and a later period may well succeed.
            tracing::warn!(%error, "a release could not be staged");
            Standing::Available {
                version: resolved.version.to_string(),
            }
        }
    }
}

/// Check every World this build hosts, and stage any head published for this
/// runtime.
///
/// Separate from the client check and never able to fail it: a World's
/// publisher and the product's are different parties on different cadences,
/// so one channel being unreachable, unpublished, or compromised must not
/// stop the other from being asked. Each outcome is logged and nothing is
/// prompted — a staged head becomes live at the next head that starts, which
/// is the same "applied at a boundary" rule the client tree follows.
fn check_worlds(identity: &Path, channel: feed::Channel) {
    let worlds = crate::serve::head::installations_root(identity);
    let installed = crate::world::installed::declarations(&worlds).unwrap_or_default();
    for declaration in installed {
        let world = declaration.manifest.id;
        match super::world::check(&world, &worlds, channel) {
            Ok(outcome) => {
                // Recorded, not only logged. A World is published in seconds
                // and this period is hours, so between the two a machine is
                // behind — and until this line the only place that fact
                // existed was a log nobody draws.
                super::world::note(&worlds, &world, &outcome, now());
                tracing::debug!(%world, ?outcome, "world bundle checked");
            }
            // Named, never folded into the client's standing: "this World's
            // channel could not be asked" is a different fact from anything
            // the product's channel said, and collapsing them would report a
            // World's outage as the product's.
            Err(error) => tracing::warn!(%world, %error, "a world bundle could not be staged"),
        }
    }
}

/// The ordinary check, against the real feed and the real host.
fn check(identity: &Path, root: &Path) -> Standing {
    let channel = feed::Channel::current();
    let current = semver::Version::parse(env!("LAIT_VERSION_SEMVER"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
    let standing = check_with(
        || feed::resolve(channel),
        feed::http_fetch,
        &current,
        env!("LAIT_TARGET"),
        root,
        standing(identity),
    );
    record(identity, &standing);
    apply_if_this_platform_has_no_stub(root);
    check_worlds(identity, channel);
    standing
}

/// On macOS, apply what was just staged.
///
/// Every other platform has a stub that applies at the next launch, which is
/// the moment nothing is running. macOS has no stub — the person launches the
/// `.app` itself — so the daemon is the only thing positioned to do it, and
/// it may: a process running inside a bundle survives that bundle being
/// exchanged, which `bundle::exchange` measures rather than assumes.
///
/// Under the claim, always. The claim is what "a client is alive here" means,
/// and a swap under a live client would leave that client's sidecar
/// resolution pointing into a bundle that moved.
#[cfg(target_os = "macos")]
fn apply_if_this_platform_has_no_stub(root: &Path) {
    let Some(live) = live_bundle() else {
        // Not running from inside a bundle: a developer's build, or a lait
        // installed on its own. There is no application to replace.
        return;
    };
    match astrolabe_stub::claim(root) {
        Ok(Some(claim)) => {
            let outcome = astrolabe_stub::bundle::apply_staged(root, &live, &claim);
            tracing::debug!(?outcome, application = %live.display(), "staged application applied");
        }
        // A live client holds the installation. The stage keeps, and the next
        // period tries again — the same deferral the stub makes at a launch.
        Ok(None) => tracing::debug!("a client holds this installation; the stage waits"),
        Err(error) => tracing::warn!(%error, "the installation could not be claimed"),
    }
}

/// Every other platform: the stub applies at the next launch.
#[cfg(not(target_os = "macos"))]
fn apply_if_this_platform_has_no_stub(_root: &Path) {}

/// How long to wait before the next check: the period, stretched one time in
/// five, plus a spread of up to a minute so two daemons started together do
/// not stay together.
fn next_delay(period: Duration, spread: Duration) -> Duration {
    let stretch = if draw() < 0.2 {
        1.0 + MAX_STRETCH * draw()
    } else {
        1.0
    };
    period
        .mul_f64(stretch)
        .saturating_add(spread.mul_f64(draw()))
}

/// A number in `[0, 1)`. From `getrandom` because this workspace carries no
/// `rand`, and degrading to the midpoint rather than failing: a check that
/// refused to run because entropy was unavailable would be a machine that
/// stops updating over a number that only needs to be roughly spread.
fn draw() -> f64 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    // 32 bits, converted without a cast: every u32 is exactly representable
    // in f64, so the quotient is exact and lands in [0, 1]. Far more spread
    // than a jitter needs, and it keeps this arithmetic-free of the silent
    // conversions the crate refuses.
    f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX)
}

/// Run periodic staging until the daemon stops.
///
/// Silent by construction: the only observable effects are the standing file
/// and a staged tree. A daemon on a machine that is not a stub-managed
/// installation never starts this at all — there is nowhere to stage to, and
/// inventing one would put a client tree beside a developer's `target/`.
pub async fn serve(identity: PathBuf, root: PathBuf, stop: tokio::sync::watch::Receiver<bool>) {
    serve_checking(identity, root, stop, CHECK_PERIOD, MAX_SPREAD, check).await;
}

/// The loop itself, with its period and its check supplied.
///
/// Split out for one reason: the parts of this module were unit-tested and the
/// *loop* was not, so nothing asserted that a running daemon ever reaches its
/// check — the composition, which is the half this tree keeps getting wrong
/// while every part is correct. A `fn` pointer rather than a closure keeps it
/// `Send + 'static` for `spawn_blocking` without a generic parameter reaching
/// into the production path.
async fn serve_checking(
    identity: PathBuf,
    root: PathBuf,
    mut stop: tokio::sync::watch::Receiver<bool>,
    period: Duration,
    spread: Duration,
    check: fn(&Path, &Path) -> Standing,
) {
    tracing::info!(root = %root.display(), "staging updates for this installation");
    // The first check is spread too, so a fleet restarted together by a
    // reboot or a deploy does not arrive at the host together either.
    let mut delay = spread.mul_f64(draw()).min(period);
    loop {
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            _ = stop.changed() => return,
        }
        if *stop.borrow() {
            return;
        }
        let (identity, root) = (identity.clone(), root.clone());
        match tokio::task::spawn_blocking(move || check(&identity, &root)).await {
            Ok(standing) => tracing::debug!(?standing, "channel checked"),
            Err(error) => tracing::warn!(%error, "the staging check panicked"),
        }
        delay = next_delay(period, spread);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(version: &str) -> feed::Resolved {
        feed::Resolved {
            version: semver::Version::parse(version).expect("a version"),
            manifest: serde_json::from_value(serde_json::json!({
                "version": version,
                "bundles": {},
                "artifacts": {},
            }))
            .expect("a manifest"),
            floor: None,
            floor_defect: false,
            published_at: None,
        }
    }

    #[test]
    fn a_channel_with_nothing_newer_is_current_and_downloads_nothing() {
        let root = tempfile::tempdir().expect("a root");
        let standing = check_with(
            || Ok(resolved("0.8.0")),
            |_, _| panic!("nothing may be fetched when there is nothing newer"),
            &semver::Version::parse("0.8.0").unwrap(),
            "x86_64-unknown-linux-gnu",
            root.path(),
            None,
        );
        assert_eq!(
            standing,
            Standing::Current {
                channel_version: "0.8.0".into()
            }
        );
    }

    /// Each failure arm is its own fact. Folding any of them into "current"
    /// is the false-quiet this taxonomy exists to prevent, and folding
    /// `Stale` into `CouldNotAsk` would hide a replay behind a network blip.
    #[test]
    fn every_feed_failure_keeps_its_own_kind_and_none_reads_as_current() {
        let root = tempfile::tempdir().expect("a root");
        let cases = [
            (
                feed::Failure::Unreachable("no route".into()),
                Standing::CouldNotAsk {
                    why: "the channel could not be asked: no route".into(),
                },
            ),
            (
                feed::Failure::Verification("bad signature".into()),
                Standing::Refused {
                    why: "feed signature verification failed: bad signature".into(),
                },
            ),
            (
                feed::Failure::Invalid("stable names a prerelease".into()),
                Standing::Refused {
                    why: "feed object invalid: stable names a prerelease".into(),
                },
            ),
            (
                feed::Failure::Stale("older than believed".into()),
                Standing::Stale {
                    why: "feed answered with a stale pointer: older than believed".into(),
                },
            ),
        ];
        for (failure, expected) in cases {
            let standing = check_with(
                || Err(failure),
                |_, _| panic!("nothing may be fetched when the channel did not resolve"),
                &semver::Version::parse("0.8.0").unwrap(),
                "x86_64-unknown-linux-gnu",
                root.path(),
                None,
            );
            assert_eq!(standing, expected);
        }
    }

    #[test]
    fn a_release_that_cannot_be_staged_is_available_rather_than_current() {
        let root = tempfile::tempdir().expect("a root");
        // The release carries no artifact for this target, so staging fails
        // — and the machine must remember that something newer exists.
        let standing = check_with(
            || Ok(resolved("0.9.0")),
            |_, _| panic!("an absent artifact is never fetched"),
            &semver::Version::parse("0.8.0").unwrap(),
            "x86_64-unknown-linux-gnu",
            root.path(),
            None,
        );
        assert_eq!(
            standing,
            Standing::Available {
                version: "0.9.0".into()
            }
        );
    }

    #[test]
    fn a_standing_round_trips_and_absence_is_its_own_answer() {
        let identity = tempfile::tempdir().expect("an identity dir");
        assert_eq!(
            standing(identity.path()),
            None,
            "a machine that never checked reported a standing"
        );
        let staged = Standing::Staged {
            version: "0.9.0".into(),
            at: 1_700_000_000,
            below_floor: false,
        };
        record(identity.path(), &staged);
        assert_eq!(standing(identity.path()), Some(staged));
    }

    /// The layout test: a binary is only inside an installation when the
    /// whole shape is there. A developer's `target/debug/lait` must never be
    /// read as one, or the daemon would stage a client tree into the build
    /// directory.
    #[test]
    fn an_install_root_is_recognised_only_by_its_whole_shape() {
        let root = tempfile::tempdir().expect("a scratch root");
        let live = root.path().join(tree::LIVE_DIR);
        std::fs::create_dir(&live).expect("a live tree");
        let lait = live.join(if cfg!(windows) { "lait.exe" } else { "lait" });
        std::fs::write(&lait, b"not really a binary").expect("stage lait");

        assert_eq!(
            install_root_of(&lait),
            None,
            "a live tree with no stub beside it was read as an installation"
        );

        let stub = root.path().join(if cfg!(windows) {
            "astrolabe.exe"
        } else {
            "astrolabe"
        });
        std::fs::write(&stub, b"not really a stub").expect("stage the stub");
        assert_eq!(
            install_root_of(&lait),
            Some(root.path().to_path_buf()),
            "the whole shape was not recognised"
        );

        let elsewhere = root.path().join("target").join("debug");
        std::fs::create_dir_all(&elsewhere).expect("a build directory");
        let built = elsewhere.join(if cfg!(windows) { "lait.exe" } else { "lait" });
        std::fs::write(&built, b"a developer's build").expect("stage it");
        assert_eq!(
            install_root_of(&built),
            None,
            "a build directory was read as an installation"
        );
    }

    /// A period that re-observes the same staged release must leave its age
    /// alone. Resetting it would make a release waiting a week look four hours
    /// old at every check, and the escalation would never escalate.
    #[test]
    fn re_observing_a_staged_release_does_not_reset_how_long_it_has_waited() {
        let root = tempfile::tempdir().expect("a root");
        // A tree already staged at the version the channel names.
        std::fs::create_dir_all(root.path().join(tree::STAGED_DIR)).expect("a staged tree");
        std::fs::write(
            root.path().join(tree::STAGE_MANIFEST),
            serde_json::json!({ "version": "0.9.0", "entry": "astrolabe", "files": [] })
                .to_string(),
        )
        .expect("a stage manifest");

        let long_ago = 1_700_000_000;
        let standing = check_with(
            || Ok(resolved("0.9.0")),
            |_, _| panic!("an already-staged release must not be downloaded again"),
            &semver::Version::parse("0.8.0").unwrap(),
            "x86_64-unknown-linux-gnu",
            root.path(),
            Some(Standing::Staged {
                version: "0.9.0".into(),
                at: long_ago,
                below_floor: false,
            }),
        );
        assert_eq!(
            standing,
            Standing::Staged {
                version: "0.9.0".into(),
                at: long_ago,
                below_floor: false,
            },
            "re-observing the same staged release reset its clock"
        );
        assert!(
            standing.staged_for(long_ago + 604_800).expect("an age")
                >= std::time::Duration::from_secs(604_800),
            "a release waiting a week did not report a week"
        );
    }

    /// The application is found by asking where this daemon runs from, not by
    /// Both installation shapes reach a staging root, and nothing else does.
    ///
    /// The stub layout was once the only shape recognized, so a macOS bundle
    /// answered `None` and its daemon never started the watcher — every
    /// component of the apply path correct, and unreachable. This asserts the
    /// composition rather than the parts.
    #[test]
    fn every_installed_shape_has_somewhere_to_stage_and_a_build_tree_does_not() {
        let root = tempfile::tempdir().expect("a scratch dir");
        let identity = root.path().join("identity");

        // A stub-managed installation stages inside its own root.
        let install = root.path().join("Programs").join("Astrolabe");
        std::fs::create_dir_all(install.join(tree::LIVE_DIR)).expect("the live tree");
        let stub = install.join(if cfg!(windows) {
            "astrolabe.exe"
        } else {
            "astrolabe"
        });
        std::fs::write(&stub, b"the stub").expect("stage the stub");
        assert_eq!(
            staging_root_of(&install.join(tree::LIVE_DIR).join("lait"), &identity),
            Some(install),
            "a stub-managed installation did not find its own root"
        );

        // A developer's build is neither shape and must stage nowhere.
        assert_eq!(
            staging_root_of(
                &root.path().join("target").join("debug").join("lait"),
                &identity
            ),
            None,
            "a build tree was treated as an installation"
        );

        // A macOS bundle has no root of its own and stages beside the identity.
        #[cfg(target_os = "macos")]
        assert_eq!(
            staging_root_of(
                &root
                    .path()
                    .join("Astrolabe.app")
                    .join("Contents")
                    .join("MacOS")
                    .join("lait"),
                &identity,
            ),
            Some(identity.join(BUNDLE_STAGING_DIR)),
            "a bundle installation had nowhere to stage, so it would never update"
        );
    }

    /// assuming `/Applications`: the person put the bundle where they wanted
    /// it, and a rule that guessed would update a copy nobody launches.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_live_application_is_the_bundle_this_daemon_ships_inside() {
        let root = tempfile::tempdir().expect("a scratch dir");
        let inside = root
            .path()
            .join("Elsewhere")
            .join("Astrolabe.app")
            .join("Contents")
            .join("MacOS")
            .join("lait");
        assert_eq!(
            live_bundle_of(&inside),
            Some(root.path().join("Elsewhere").join("Astrolabe.app")),
            "the daemon did not find the bundle it ships inside"
        );
        // A developer's build is not inside a bundle, and inventing one would
        // replace something nobody installed.
        assert_eq!(
            live_bundle_of(&root.path().join("target").join("debug").join("lait")),
            None
        );
    }

    /// Jitter is what keeps a fleet from arriving together. The property that
    /// matters is spread, not any particular draw, so the assertion is over a
    /// sample: identical delays would mean the draw is not being used.
    #[test]
    fn the_period_is_spread_so_two_daemons_do_not_stay_together() {
        let period = Duration::from_secs(1000);
        let delays: Vec<Duration> = (0..64).map(|_| next_delay(period, MAX_SPREAD)).collect();
        assert!(
            delays.iter().any(|delay| *delay != delays[0]),
            "every delay was identical, so nothing is spreading the fleet"
        );
        for delay in &delays {
            assert!(
                *delay >= period,
                "a delay came in under the period: {delay:?}"
            );
            assert!(
                *delay <= period.mul_f64(1.0 + MAX_STRETCH) + MAX_SPREAD,
                "a delay exceeded the stretch and spread bounds: {delay:?}"
            );
        }
    }

    /// How many times the loop's check has been reached, and by which paths.
    ///
    /// A static rather than a captured counter because the loop takes a `fn`
    /// pointer, which is what keeps `spawn_blocking` free of a generic.
    static REACHED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counting_check(_identity: &Path, _root: &Path) -> Standing {
        REACHED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Standing::Current {
            channel_version: "0.0.0".into(),
        }
    }

    /// The gap this issue left open: every part of the check was tested and the
    /// *loop* was not, so a daemon could have been wired to a watcher that
    /// never reached its check and every unit test would still have passed.
    ///
    /// Asserts the two things the loop is: it comes back round on its period
    /// rather than checking once, and it ends on the stop signal every other
    /// service in `Daemon::serve` shares. It does not assert that the daemon
    /// spawns it — `spawn_staging` is the remaining joint, and it is guarded by
    /// `an_install_root_is_recognised_only_by_its_whole_shape` on one side.
    ///
    /// Real time, not a paused clock. The first cut paused it and advanced past
    /// several periods, which failed seven runs in eight: the check runs on
    /// `spawn_blocking`, and a blocking pool thread has no relationship to a
    /// clock the test is moving by hand. Both timing figures are parameters of
    /// the loop instead, so the test can pick milliseconds and wait for the
    /// real thing to happen.
    #[tokio::test]
    async fn the_loop_comes_back_round_on_its_period_and_stops_when_told() {
        REACHED.store(0, std::sync::atomic::Ordering::SeqCst);
        let identity = tempfile::tempdir().expect("an identity directory");
        let root = tempfile::tempdir().expect("an install root");
        let (stop, receiver) = tokio::sync::watch::channel(false);

        let loops = tokio::spawn(serve_checking(
            identity.path().to_path_buf(),
            root.path().to_path_buf(),
            receiver,
            Duration::from_millis(5),
            Duration::from_millis(1),
            counting_check,
        ));

        // Waited for rather than assumed: a loaded CI box is slow, and the
        // assertion is that the loop comes back at all, not that it comes back
        // within a number this test invented.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while REACHED.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the loop reached its check {} time(s) in ten seconds at a 5ms \
                 period — a watcher that checks once is not a watcher",
                REACHED.load(std::sync::atomic::Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        stop.send(true).expect("the stop signal is received");
        tokio::time::timeout(Duration::from_secs(10), loops)
            .await
            .expect("the loop did not end on the stop signal, so a shutdown would hang")
            .expect("the loop panicked");
    }
}
