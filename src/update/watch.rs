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
    /// A verified tree is on disk waiting for a launch to accept it.
    Staged {
        /// The version that will be live after the next launch.
        version: String,
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
            Self::Staged { version } => Some(version),
            _ => None,
        }
    }
}

/// The install root of a stub-managed client, discovered from a path inside
/// it.
///
/// A tree-managed installation puts this binary at `<root>/current/lait`,
/// with the stub at `<root>/astrolabe-stub`. Both halves are checked,
/// because "my grandparent directory exists" is true of every binary
/// everywhere: a developer's `target/debug/lait` must not be read as an
/// installation and staged into.
pub fn install_root_of(executable: &Path) -> Option<PathBuf> {
    let live = executable.parent()?;
    if live.file_name()? != tree::LIVE_DIR {
        return None;
    }
    let root = live.parent()?;
    let stub = root.join(if cfg!(windows) {
        "astrolabe-stub.exe"
    } else {
        "astrolabe-stub"
    });
    stub.is_file().then(|| root.to_path_buf())
}

/// The install root of the running binary, when it is stub-managed.
pub fn install_root() -> Option<PathBuf> {
    install_root_of(&std::env::current_exe().ok()?)
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

    // A tree already staged at this version is the answer; re-downloading it
    // every period would be bytes spent to learn nothing.
    if let Some(staged) = tree::staged_version(root) {
        if staged == resolved.version.to_string() {
            return Standing::Staged { version: staged };
        }
    }

    match tree::stage_tree_with(fetch, &resolved, target, root) {
        Ok(staged) => Standing::Staged {
            version: staged.version,
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
    );
    record(identity, &standing);
    standing
}

/// How long to wait before the next check: the period, stretched one time in
/// five, plus a spread of up to a minute so two daemons started together do
/// not stay together.
fn next_delay(period: Duration) -> Duration {
    let stretch = if draw() < 0.2 {
        1.0 + MAX_STRETCH * draw()
    } else {
        1.0
    };
    let spread = MAX_SPREAD.mul_f64(draw());
    period.mul_f64(stretch).saturating_add(spread)
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
pub async fn serve(identity: PathBuf, root: PathBuf, mut stop: tokio::sync::watch::Receiver<bool>) {
    tracing::info!(root = %root.display(), "staging updates for this installation");
    // The first check is spread too, so a fleet restarted together by a
    // reboot or a deploy does not arrive at the host together either.
    let mut delay = MAX_SPREAD.mul_f64(draw());
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
        delay = next_delay(CHECK_PERIOD);
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
            "astrolabe-stub.exe"
        } else {
            "astrolabe-stub"
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

    /// Jitter is what keeps a fleet from arriving together. The property that
    /// matters is spread, not any particular draw, so the assertion is over a
    /// sample: identical delays would mean the draw is not being used.
    #[test]
    fn the_period_is_spread_so_two_daemons_do_not_stay_together() {
        let period = Duration::from_secs(1000);
        let delays: Vec<Duration> = (0..64).map(|_| next_delay(period)).collect();
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
}
