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
//! A headless service has no stub and no launch. There the daemon is the
//! only thing positioned to apply — the same argument that lets macOS
//! exchange its bundle here — so a service check swaps the proven binary
//! over `<root>/bin/lait` and asks for a relaunch, and the relaunch is an
//! exit: `Restart=` is what execs the new bytes, never this process.
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
//! ## The period is the floor, not the latency
//!
//! A machine also holds a subscription to the notify relay
//! ([`super::notify`]), which wakes this loop the moment a pointer it follows
//! is announced. The subscription changes *when* a check runs and nothing
//! about what a check is: the same resolve, the same ratchet, the same
//! verification. A machine with no relay, or no route to it, still checks on
//! its period, which is why the period is a floor and not a fallback.
//!
//! ## Why the period is jittered
//!
//! A fleet that checks on a round number checks together. Chrome's updater
//! stretches its period by 20% one time in ten and delays each check by a
//! random fraction of a minute for exactly this reason; the same shape is
//! here, drawn from `getrandom` because this workspace deliberately carries
//! no `rand`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use super::{feed, tree};

/// The base period between channel checks. Chrome's updater uses 4.5 hours
/// and has the fleet-scale evidence; there is no reason to invent another
/// number. With the subscription carrying the ordinary case, this is only how
/// long a machine that cannot hear the relay stays behind.
pub const CHECK_PERIOD: Duration = Duration::from_secs(4 * 60 * 60 + 30 * 60);

/// The least time between two checks, however many wakes arrive. A relay
/// that rang falsely, or a burst of announcements, costs one check per gap
/// rather than one per ring.
pub const MIN_GAP: Duration = Duration::from_secs(5);

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

/// The directory the service layout keeps its binary in, under the root.
const SERVICE_BIN_DIR: &str = "bin";

/// The record the install line writes beside the service binary. Its presence
/// is what makes `<root>/bin/lait` an installation rather than a binary
/// somebody untarred into a directory that happened to be called `bin`.
pub const SERVICE_INSTALLED_FILE: &str = "installed.json";

/// The shape this daemon is installed in, which decides how a release is
/// applied. Every shape is recognised whole, never by one landmark: "my
/// parent is called `bin`" is true of half the binaries on a machine, and a
/// developer's `target/debug/lait` must never be read as something to swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installation {
    /// A stub-managed client tree: `<root>/current/lait` with the stub at
    /// `<root>/astrolabe`. Releases are staged into the root and the stub
    /// applies them at the next launch.
    StubTree { root: PathBuf },
    /// A macOS application bundle. It has no root of its own — the person put
    /// the `.app` where they wanted it — so releases stage beside the
    /// identity and the daemon exchanges the bundles.
    Bundle { staging: PathBuf },
    /// A headless service: `<root>/bin/lait` (this executable) beside
    /// `<root>/bin/installed.json`, and no client anywhere near it. Releases
    /// are swapped over the binary and a relaunch is asked for.
    Service { root: PathBuf },
}

impl Installation {
    /// Which shape `executable` sits in, or `None` for a bare binary — a
    /// developer's build tree, a tarball somebody untarred and ran — where a
    /// release has nowhere to go and inventing somewhere would drop bytes
    /// beside a `target/`.
    ///
    /// The service shape is refused when a client sits beside the binary,
    /// because `custody_of` reads that as the client's installation and a
    /// sidecar must never replace itself out from under its client.
    pub fn of(executable: &Path, identity: &Path) -> Option<Self> {
        if let Some(root) = install_root_of(executable) {
            return Some(Self::StubTree { root });
        }
        #[cfg(target_os = "macos")]
        if live_bundle_of(executable).is_some() {
            return Some(Self::Bundle {
                staging: identity.join(BUNDLE_STAGING_DIR),
            });
        }
        let _ = identity;
        let bin = executable.parent()?;
        if bin.file_name()? != SERVICE_BIN_DIR
            || !bin.join(SERVICE_INSTALLED_FILE).is_file()
            || super::custody_of(executable) != super::Custody::SelfManaged
        {
            return None;
        }
        Some(Self::Service {
            root: bin.parent()?.to_path_buf(),
        })
    }

    /// The directory this shape writes releases under.
    pub fn root(&self) -> &Path {
        match self {
            Self::StubTree { root } | Self::Service { root } => root,
            Self::Bundle { staging } => staging,
        }
    }

    /// The binary a service swaps.
    fn service_binary(root: &Path) -> PathBuf {
        root.join(SERVICE_BIN_DIR).join("lait")
    }
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

/// One check: resolve the channel, and take the release when it holds
/// something newer than what is installed here — staged as a tree for a
/// client shape, swapped over the binary for a service.
///
/// Blocking (HTTP, archive extract, file writes), so callers on a reactor
/// must hand it to `spawn_blocking`. Injected fetch and channel for the same
/// reason the rest of this module's neighbours inject theirs.
pub fn check_with<R, F>(
    resolve: R,
    fetch: F,
    current: &semver::Version,
    target: &str,
    installation: &Installation,
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

    let version = resolved.version.to_string();
    // The clock is *not* reset when a period re-observes a release it already
    // took. A release waiting a week would otherwise look four hours old at
    // every check, and the escalation would never escalate.
    let taken_at = |version: &str| {
        previous.and_then(|standing| match standing {
            Standing::Staged {
                version: staged,
                at,
            } if staged == version => Some(at),
            _ => None,
        })
    };

    match installation {
        Installation::StubTree { root } | Installation::Bundle { staging: root } => {
            // A tree already staged at this version is the answer;
            // re-downloading it every period would be bytes spent to learn
            // nothing.
            if let Some(staged) = tree::staged_version(root) {
                if staged == version {
                    let at = taken_at(&staged).unwrap_or_else(now);
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
                    // The release exists and this machine could not take it.
                    // That is "available", not "current" and not "could not
                    // ask" — the channel answered, and a later period may
                    // well succeed.
                    tracing::warn!(%error, "a release could not be staged");
                    Standing::Available { version }
                }
            }
        }
        Installation::Service { root } => {
            // The binary was already swapped for this release and the
            // relaunch is what has not happened. Asking again every period
            // would download the same bytes to learn nothing.
            if let Some(at) = taken_at(&version) {
                return Standing::Staged { version, at };
            }
            match super::stage_with(fetch, &resolved, current, target) {
                Ok(Some(binary)) => {
                    match super::swap_at(&Installation::service_binary(root), &binary) {
                        Ok(()) => Standing::Staged { version, at: now() },
                        Err(error) => {
                            tracing::warn!(%error, "a proven release could not be swapped in");
                            Standing::Available { version }
                        }
                    }
                }
                // Unreachable — newness was decided above — but the answer
                // that is true if it ever is.
                Ok(None) => Standing::Current {
                    channel_version: version,
                },
                Err(super::StageFailure::CouldNotTake(why)) => {
                    tracing::warn!(%why, "a release could not be taken");
                    Standing::Available { version }
                }
                // Bytes that did not prove are a fact about the host, not the
                // network, and the binary on disk was never touched.
                Err(super::StageFailure::Refused(why)) => Standing::Refused { why },
            }
        }
    }
}

/// Whether this check is the one that swapped a service's binary: the
/// standing became `Staged` at a version the previous one was not. A
/// relaunch is asked for on that check and no other — a period that
/// re-observes its own swap must not ask again, or a relaunch that failed
/// would be asked for forever.
fn swapped_this_check(previous: Option<&Standing>, standing: &Standing) -> bool {
    match (previous, standing) {
        (Some(Standing::Staged { version: was, .. }), Standing::Staged { version, .. }) => {
            was != version
        }
        (_, Standing::Staged { .. }) => true,
        _ => false,
    }
}

/// Check every selected World installation, and stage any head published for
/// this runtime.
///
/// Separate from the client check and never able to fail it: a World's
/// publisher and the product's are different parties on different cadences,
/// so one channel being unreachable, unpublished, or compromised must not
/// stop the other from being asked. Each outcome is logged and nothing is
/// prompted — a staged head becomes live at the next head that starts, which
/// is the same "applied at a boundary" rule the client tree follows.
/// Takes no channel, deliberately.
///
/// It used to take the node's and hand the same one to every World, which
/// made this the place a World's own channel quietly stopped being true: the
/// record kept saying test, the row kept drawing test, and every period this
/// re-selected the release the *node's* channel named. A choice that is
/// recorded and displayed but never asked is worse than no choice at all.
///
/// The parameter is gone rather than defaulted so that threading a
/// single channel back through here has to fail to compile.
///
/// Returns whether any World's release moved. A selected release is served
/// only by a fresh daemon generation — every Runtime Catalog in this process
/// is pinned to what it launched — so the caller relaunches when this is
/// true, and a World a person is looking at changes under them within the
/// second rather than at the next reboot.
fn check_worlds(identity: &Path) -> bool {
    let worlds = crate::serve::head::installations_root(identity);
    let installed = crate::world::installed::declarations(&worlds).unwrap_or_default();
    let mut staged = false;
    for declaration in installed {
        let world = declaration.manifest.id;
        let channel = super::world::channel_for(&worlds, &world);
        match super::world::check(&world, &worlds, channel) {
            Ok(outcome) => {
                // Recorded, not only logged. A World is published in seconds
                // and this period is hours, so between the two a machine is
                // behind — and until this line the only place that fact
                // existed was a log nobody draws.
                super::world::note(&worlds, &world, &outcome, now());
                tracing::debug!(%world, ?outcome, "world bundle checked");
                if let super::world::Outcome::Staged { version } = &outcome {
                    tracing::info!(%world, %version, "a World release was staged and selected");
                    staged = true;
                }
            }
            // Named, never folded into the client's standing: "this World's
            // channel could not be asked" is a different fact from anything
            // the product's channel said, and collapsing them would report a
            // World's outage as the product's.
            Err(error) => tracing::warn!(%world, %error, "a world bundle could not be staged"),
        }
    }
    staged
}

/// What one check came to: the client's standing, and whether a fresh
/// daemon generation is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub standing: Standing,
    /// Only a fresh daemon generation can serve what this check took: a
    /// World release was staged and selected, or a service's binary was
    /// swapped and the running image is the old one.
    pub relaunch: bool,
}

/// The ordinary check, against the real feed and the real host.
fn check(identity: &Path, installation: &Installation) -> Checked {
    let channel = feed::Channel::current();
    let current = semver::Version::parse(env!("LAIT_VERSION_SEMVER"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
    let previous = standing(identity);
    let standing = check_with(
        || feed::resolve(channel),
        feed::http_fetch,
        &current,
        env!("LAIT_TARGET"),
        installation,
        previous.clone(),
    );
    record(identity, &standing);
    apply_if_this_platform_has_no_stub(installation.root());
    let swapped = matches!(installation, Installation::Service { .. })
        && swapped_this_check(previous.as_ref(), &standing);
    let worlds_staged = check_worlds(identity);
    Checked {
        standing,
        relaunch: worlds_staged || swapped,
    }
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
pub(super) fn draw() -> f64 {
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

/// What the loop does when a check found that a fresh generation is needed —
/// a World release staged and selected, or a service binary swapped: the
/// daemon's generation relaunch, supplied by the host because only it owns
/// the endpoint. A `fn` would do but the host's relaunch captures state.
pub type OnWorldsStaged = Arc<dyn Fn() + Send + Sync>;

/// Run periodic staging until the daemon stops, checking early whenever
/// `wake` is notified.
///
/// Silent by construction: the only observable effects are the standing file
/// and the taken release. A daemon on a machine that is not an installation
/// never starts this at all — there is nowhere for a release to go, and
/// inventing somewhere would put bytes beside a developer's `target/`.
pub async fn serve(
    identity: PathBuf,
    installation: Installation,
    stop: tokio::sync::watch::Receiver<bool>,
    wake: Arc<Notify>,
    on_relaunch_needed: OnWorldsStaged,
) {
    serve_checking(
        identity,
        installation,
        stop,
        wake,
        on_relaunch_needed,
        CHECK_PERIOD,
        MAX_SPREAD,
        MIN_GAP,
        check,
    )
    .await;
}

/// The loop itself, with its period and its check supplied.
///
/// Split out for one reason: the parts of this module were unit-tested and the
/// *loop* was not, so nothing asserted that a running daemon ever reaches its
/// check — the composition, which is the half this tree keeps getting wrong
/// while every part is correct. A `fn` pointer rather than a closure keeps it
/// `Send + 'static` for `spawn_blocking` without a generic parameter reaching
/// into the production path.
#[allow(clippy::too_many_arguments)]
async fn serve_checking(
    identity: PathBuf,
    installation: Installation,
    mut stop: tokio::sync::watch::Receiver<bool>,
    wake: Arc<Notify>,
    on_relaunch_needed: OnWorldsStaged,
    period: Duration,
    spread: Duration,
    gap: Duration,
    check: fn(&Path, &Installation) -> Checked,
) {
    tracing::info!(?installation, "staging updates for this installation");
    // The first check is spread too, so a fleet restarted together by a
    // reboot or a deploy does not arrive at the host together either.
    let mut delay = spread.mul_f64(draw()).min(period);
    let mut last_check: Option<std::time::Instant> = None;
    loop {
        let woken = tokio::select! {
            () = tokio::time::sleep(delay) => false,
            () = wake.notified() => true,
            _ = stop.changed() => return,
        };
        if *stop.borrow() {
            return;
        }
        if woken {
            // Spaced from the last check, whoever asked: a wake is a doorbell
            // and a doorbell can be leaned on.
            if let Some(since) = last_check.map(|at| at.elapsed()) {
                if since < gap {
                    tokio::select! {
                        () = tokio::time::sleep(gap.saturating_sub(since)) => {}
                        _ = stop.changed() => return,
                    }
                }
            }
            tracing::info!("a newer pointer was announced; checking now rather than on the period");
        }
        let (identity, installation) = (identity.clone(), installation.clone());
        match tokio::task::spawn_blocking(move || check(&identity, &installation)).await {
            Ok(checked) => {
                tracing::debug!(standing = ?checked.standing, "channel checked");
                if checked.relaunch {
                    tracing::info!("a fresh generation is needed to serve what was taken; relaunching the daemon");
                    on_relaunch_needed();
                }
            }
            Err(error) => tracing::warn!(%error, "the staging check panicked"),
        }
        last_check = Some(std::time::Instant::now());
        delay = next_delay(period, spread);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client shape most checks are written against.
    fn stub_tree(root: &Path) -> Installation {
        Installation::StubTree {
            root: root.to_path_buf(),
        }
    }

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
            &stub_tree(root.path()),
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
                &stub_tree(root.path()),
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
            &stub_tree(root.path()),
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
            &stub_tree(root.path()),
            Some(Standing::Staged {
                version: "0.9.0".into(),
                at: long_ago,
            }),
        );
        assert_eq!(
            standing,
            Standing::Staged {
                version: "0.9.0".into(),
                at: long_ago,
            },
            "re-observing the same staged release reset its clock"
        );
        assert!(
            standing.staged_for(long_ago + 604_800).expect("an age")
                >= std::time::Duration::from_secs(604_800),
            "a release waiting a week did not report a week"
        );
    }

    /// Every shape is recognised whole, and nothing else is a shape at all.
    ///
    /// The stub layout was once the only one recognised, so a macOS bundle
    /// answered `None` and its daemon never started the watcher — every
    /// component of the apply path correct, and unreachable. The service
    /// shape has the opposite failure available to it: a binary whose parent
    /// happens to be called `bin` must not be swapped over, so the record
    /// beside it is required, and a client beside it is a refusal.
    #[test]
    fn every_installed_shape_is_one_shape_and_a_bare_binary_is_none() {
        let root = tempfile::tempdir().expect("a scratch dir");
        let identity = root.path().join("identity");
        let stub_name = if cfg!(windows) {
            "astrolabe.exe"
        } else {
            "astrolabe"
        };

        // A service: `bin/lait` beside `bin/installed.json`.
        let service = root.path().join("var").join("lib").join("lait");
        let bin = service.join(SERVICE_BIN_DIR);
        std::fs::create_dir_all(&bin).expect("the service bin");
        let lait = bin.join("lait");
        std::fs::write(&lait, b"the service binary").expect("stage lait");
        assert_eq!(
            Installation::of(&lait, &identity),
            None,
            "a binary in a directory called bin was read as a service"
        );
        std::fs::write(bin.join(SERVICE_INSTALLED_FILE), b"{}").expect("the record");
        assert_eq!(
            Installation::of(&lait, &identity),
            Some(Installation::Service { root: service }),
            "the whole service shape was not recognised"
        );
        // A client beside the binary makes it that client's sidecar, and a
        // sidecar never replaces itself out from under its client.
        std::fs::write(bin.join(stub_name), b"a stray client").expect("stage the stray");
        assert_eq!(
            Installation::of(&lait, &identity),
            None,
            "a service root with a client beside the binary was still a service"
        );

        // A stub-managed installation stages inside its own root.
        let install = root.path().join("Programs").join("Astrolabe");
        std::fs::create_dir_all(install.join(tree::LIVE_DIR)).expect("the live tree");
        std::fs::write(install.join(stub_name), b"the stub").expect("stage the stub");
        assert_eq!(
            Installation::of(&install.join(tree::LIVE_DIR).join("lait"), &identity),
            Some(Installation::StubTree {
                root: install.clone()
            }),
            "a stub-managed installation did not find its own root"
        );

        // A developer's build is no shape and must stage nowhere.
        assert_eq!(
            Installation::of(
                &root.path().join("target").join("debug").join("lait"),
                &identity
            ),
            None,
            "a build tree was treated as an installation"
        );

        // A macOS bundle has no root of its own and stages beside the identity.
        #[cfg(target_os = "macos")]
        assert_eq!(
            Installation::of(
                &root
                    .path()
                    .join("Astrolabe.app")
                    .join("Contents")
                    .join("MacOS")
                    .join("lait"),
                &identity,
            ),
            Some(Installation::Bundle {
                staging: identity.join(BUNDLE_STAGING_DIR)
            }),
            "a bundle installation had nowhere to stage, so it would never update"
        );
    }

    /// The service check, end to end through the same sealed feed the
    /// single-binary chain is proven against: the signed pointer names the
    /// manifest, the manifest the artifact, and the bytes that come out of
    /// the archive are the bytes that land at `bin/lait`. Then the two rules
    /// a service adds — the relaunch is asked for on the check that swapped
    /// and on no other, and bytes that do not prove leave the binary alone.
    #[test]
    fn a_service_check_swaps_the_binary_and_asks_for_one_relaunch() {
        use crate::update::tests::{sealed_feed, windows_release_zip};
        let target = "x86_64-pc-windows-msvc";
        let url = "https://feed.example/releases/0.9.0/lait-x86_64-pc-windows-msvc.zip";
        let running = semver::Version::parse("0.8.0").unwrap();

        let root = tempfile::tempdir().expect("a service root");
        let installation = Installation::Service {
            root: root.path().to_path_buf(),
        };
        let bin = root.path().join(SERVICE_BIN_DIR);
        std::fs::create_dir_all(&bin).expect("the service bin");
        let lait = bin.join("lait");
        std::fs::write(&lait, b"the 0.8.0 binary that is running").expect("the live binary");

        let binary = b"lait v0.9.0 as the maintainer built";
        let archive = windows_release_zip(binary);
        let digest = blake3::hash(&archive).to_hex().to_string();
        let (objects, pubkey) = sealed_feed("0.9.0", url, &archive, archive.len() as u64, &digest);
        let fetch = |u: &str| {
            objects
                .get(u)
                .cloned()
                .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
        };
        let resolve = || {
            feed::resolve_with(
                fetch,
                feed::Channel::Test,
                "https://feed.example",
                &[pubkey],
                None,
            )
        };

        let first = check_with(
            resolve,
            |u, _| fetch(u),
            &running,
            target,
            &installation,
            None,
        );
        assert_eq!(
            std::fs::read(&lait).expect("the binary is still at its path"),
            binary,
            "the bytes at bin/lait must be exactly what the archive carried"
        );
        assert_eq!(first.staged_version(), Some("0.9.0"));
        assert!(
            swapped_this_check(None, &first),
            "the check that swapped the binary did not ask for a relaunch"
        );

        // The next period, still running the old image: the swap is on disk,
        // nothing is downloaded again, and the relaunch is not asked twice.
        let second = check_with(
            resolve,
            |_, _| panic!("a release already swapped in must not be downloaded again"),
            &running,
            target,
            &installation,
            Some(first.clone()),
        );
        assert_eq!(second, first, "re-observing the swap changed the standing");
        assert!(
            !swapped_this_check(Some(&first), &second),
            "a period that re-observed its own swap asked for another relaunch"
        );

        // A host serving different bytes of the same length: the digest is
        // the check that refuses it, the binary is untouched, and the answer
        // is a refusal rather than "try again later".
        let tampered = windows_release_zip(b"lait v0.9.0 with a back door added!");
        assert_eq!(
            tampered.len(),
            archive.len(),
            "the fixture must isolate the digest check"
        );
        let (objects, pubkey) = sealed_feed("0.9.0", url, &tampered, archive.len() as u64, &digest);
        let fetch = |u: &str| {
            objects
                .get(u)
                .cloned()
                .ok_or_else(|| feed::Failure::Unreachable(format!("no object at {u}")))
        };
        std::fs::write(&lait, b"the 0.8.0 binary that is running").expect("the live binary");
        let refused = check_with(
            || {
                feed::resolve_with(
                    fetch,
                    feed::Channel::Test,
                    "https://feed.example",
                    &[pubkey],
                    None,
                )
            },
            |u, _| fetch(u),
            &running,
            target,
            &installation,
            None,
        );
        assert!(
            matches!(&refused, Standing::Refused { why } if why.contains("digest verification failed")),
            "tampered bytes were not refused by name: {refused:?}"
        );
        assert_eq!(
            std::fs::read(&lait).expect("the binary is still at its path"),
            b"the 0.8.0 binary that is running",
            "bytes that did not prove reached the binary"
        );
        assert!(
            !swapped_this_check(None, &refused),
            "a refusal asked for a relaunch"
        );
    }

    /// The application is found by asking where this daemon runs from, not by
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

    fn counting_check(_identity: &Path, _installation: &Installation) -> Checked {
        REACHED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Checked {
            standing: Standing::Current {
                channel_version: "0.0.0".into(),
            },
            relaunch: false,
        }
    }

    /// A check that staged a World, once: the first call moves, the rest do
    /// not, which is what a real channel does after a relaunch.
    fn staging_check(_identity: &Path, _installation: &Installation) -> Checked {
        let first = REACHED.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        Checked {
            standing: Standing::Current {
                channel_version: "0.0.0".into(),
            },
            relaunch: first,
        }
    }

    fn nothing_on_staged() -> OnWorldsStaged {
        Arc::new(|| {})
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
            stub_tree(root.path()),
            receiver,
            Arc::new(Notify::new()),
            nothing_on_staged(),
            Duration::from_millis(5),
            Duration::from_millis(1),
            Duration::ZERO,
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

    /// A wake reaches the check well inside a period that would not have, and
    /// leaning on the doorbell is spaced by the gap rather than answered per
    /// ring: three wakes in quick succession are at most two checks inside
    /// one gap (the first, and the one permit `Notify` keeps).
    #[tokio::test]
    async fn a_wake_checks_now_and_a_burst_of_wakes_is_spaced_by_the_gap() {
        REACHED.store(0, std::sync::atomic::Ordering::SeqCst);
        let identity = tempfile::tempdir().expect("an identity directory");
        let root = tempfile::tempdir().expect("an install root");
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let wake = Arc::new(Notify::new());
        let period = Duration::from_secs(3600);
        let gap = Duration::from_millis(300);

        let loops = tokio::spawn(serve_checking(
            identity.path().to_path_buf(),
            stub_tree(root.path()),
            receiver,
            wake.clone(),
            nothing_on_staged(),
            period,
            Duration::ZERO,
            gap,
            counting_check,
        ));

        // The first check is spread by zero, so it lands at once. Wait for it,
        // then ring three times inside one gap.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while REACHED.load(std::sync::atomic::Ordering::SeqCst) < 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "the first check never ran"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let rung_at = std::time::Instant::now();
        wake.notify_one();
        wake.notify_one();
        wake.notify_one();
        while REACHED.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "a wake did not reach the check inside an hour-long period"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            rung_at.elapsed() >= gap.mul_f64(0.9),
            "the wake was answered inside the gap: {:?}",
            rung_at.elapsed()
        );
        // Let every permit drain and count: `Notify` coalesces to one, so a
        // burst of three is one more check, spaced by the gap, and not three.
        tokio::time::sleep(gap * 3).await;
        let reached = REACHED.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            reached <= 3,
            "three wakes became {reached} checks; the gap is not spacing them"
        );

        stop.send(true).expect("the stop signal is received");
        tokio::time::timeout(Duration::from_secs(10), loops)
            .await
            .expect("the loop did not end on the stop signal")
            .expect("the loop panicked");
    }

    /// A check that staged a World asks for the relaunch, and a check that
    /// did not does not: the whole reason a selected release stopped waiting
    /// for the next reboot.
    #[tokio::test]
    async fn a_staged_world_asks_for_the_generation_relaunch_once() {
        REACHED.store(0, std::sync::atomic::Ordering::SeqCst);
        let identity = tempfile::tempdir().expect("an identity directory");
        let root = tempfile::tempdir().expect("an install root");
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let relaunches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let on_staged: OnWorldsStaged = {
            let relaunches = relaunches.clone();
            Arc::new(move || {
                relaunches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        };

        let loops = tokio::spawn(serve_checking(
            identity.path().to_path_buf(),
            stub_tree(root.path()),
            receiver,
            Arc::new(Notify::new()),
            on_staged,
            Duration::from_millis(5),
            Duration::from_millis(1),
            Duration::ZERO,
            staging_check,
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while REACHED.load(std::sync::atomic::Ordering::SeqCst) < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "the loop did not come back round"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            relaunches.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the relaunch is asked for exactly when a World was staged"
        );
        stop.send(true).expect("the stop signal is received");
        tokio::time::timeout(Duration::from_secs(10), loops)
            .await
            .expect("the loop did not end on the stop signal")
            .expect("the loop panicked");
    }
}
