//! What Astrolabe can ask for, and what it gets back.
//!
//! This is the whole reach of the client: the supervisor library it embeds, and
//! the three control-protocol planes it speaks. It holds no interface types and
//! draws nothing, which is what lets every rule in here be tested without a
//! window.
//!
//! Astrolabe is a *control-protocol client that also supervises processes*, not
//! a process supervisor with some status attached. The supervisor is one of four
//! things it reaches, and the smallest.

pub mod book;
pub mod correspondence;
pub mod display;
pub mod error;
pub mod heads;
pub mod host;
pub mod http;
pub mod launch;
pub mod library;
pub mod presence;
pub mod reach;
pub mod space;
pub mod storage;
pub mod update;

pub use error::{ClientError, ClientResult};
pub use library::{Artwork, LaunchTicket, LibraryEntry};

use std::path::PathBuf;
use std::sync::Arc;

use lait_workbench::{Config as SupervisorConfig, Signals, Supervisor};

/// The planes a client speaks, behind one handle.
///
/// Cloneable, because every surface holds one and they must all be talking to
/// the same daemon and the same supervisor. Cloning is cheap: the state behind
/// it is shared, not copied.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    supervisor: Supervisor,
    /// The identity home this client is bound to. Every plane resolves through
    /// it, so a client can never straddle two identities by accident.
    identity: Option<PathBuf>,
    /// Whether this launch hosts an identity daemon at all. A standalone
    /// launch (`skip_sidecar`) deliberately has none, and a roll-forward must
    /// not stand one up that the launch chose not to have.
    hosted: bool,
}

impl Client {
    /// Start the supervisor and bind the control planes to one identity.
    ///
    /// Returns the signal stream alongside, and for the same reason the
    /// supervisor does: a consumer that could hold a client without already
    /// holding its stream would have a window in which events vanish.
    pub async fn start(config: Config) -> ClientResult<(Self, Signals)> {
        let selection = selection_for(config.identity.as_deref());
        // The supervisor first: starting it spawns nothing, and it is what
        // stages the image everything else runs from. The daemon below is
        // then spawned from the *staged* copy, so no long-lived process holds
        // the sidecar file a rebuild wants to replace.
        let (supervisor, signals) = Supervisor::start(SupervisorConfig {
            state_root: config.state_root,
            executable: config.executable.clone(),
            observation_interval: config.observation_interval,
            staging: config.staging,
        })
        .await
        .map_err(ClientError::from)?;
        // The sidecar is what makes this a hosted identity. Skipping it is a
        // deliberate standalone launch: the daemon-backed planes will report
        // unreachable, but the window comes up at once instead of waiting on a
        // daemon that is not meant to be there.
        if !config.skip_sidecar {
            let daemon_image = supervisor
                .image()
                .map(|image| PathBuf::from(image.staged_path))
                .unwrap_or(config.executable);
            lait::host_client::ensure_lait_daemon_with_executable(&selection, &daemon_image)
                .await
                .map_err(|error| {
                    let message = format!("start or attach to the identity daemon: {error:#}");
                    if error
                        .downcast_ref::<lait::control::ForeignDaemon>()
                        .is_some()
                    {
                        ClientError::refused(message)
                    } else {
                        ClientError::unreachable(message)
                    }
                })?;
        }
        Ok((
            Self {
                inner: Arc::new(Inner {
                    supervisor,
                    identity: config.identity,
                    hosted: !config.skip_sidecar,
                }),
            },
            signals,
        ))
    }

    /// Wrap a supervisor that is already running.
    ///
    /// The seam tests reach through: a test drives a fake supervisor and still
    /// exercises the real client rules on top of it.
    pub fn over(supervisor: Supervisor, identity: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                supervisor,
                identity,
                // A wrapped supervisor has no daemon of this client's making,
                // and a roll-forward must not invent one.
                hosted: false,
            }),
        }
    }

    pub fn supervisor(&self) -> &Supervisor {
        &self.inner.supervisor
    }

    pub fn identity(&self) -> Option<&std::path::Path> {
        self.inner.identity.as_deref()
    }

    /// The identity directory this client's daemon actually keeps state under.
    ///
    /// Not [`Self::identity`], and the difference is the whole reason this
    /// exists: that field is the *override* (`--home`), and it is `None` for
    /// the ordinary per-user identity — which is every normal launch. Reading
    /// a daemon-written fact through it would have found nothing on exactly
    /// the machines that have one.
    pub fn identity_dir(&self) -> Option<PathBuf> {
        selection_for(self.inner.identity.as_deref())
            .identity_dir()
            .ok()
    }

    /// A control-protocol client for this identity's daemon.
    ///
    /// Built per call rather than held: the daemon is an always-running local
    /// service that outlives every window, and a cached client would be a
    /// connection this process assumes is still good.
    pub fn daemon(&self) -> ClientResult<lait::daemon::Client> {
        // `default` is the ambient selection — whatever the environment and the
        // working directory already say — which is what the daemon itself comes
        // up with. Binding to the same one is what keeps a client and its daemon
        // talking about the same identity.
        let selection = selection_for(self.inner.identity.as_deref());
        lait::daemon::Client::for_selection(&selection)
            .map_err(|error| ClientError::unreachable(format!("reach the daemon: {error:#}")))
    }

    /// Stop observing and stop every daemon this client owns.
    pub async fn shutdown(&self) {
        self.inner.supervisor.shutdown().await;
    }

    /// The image everything here spawns from, and whether its source has been
    /// rebuilt since it was staged.
    ///
    /// The comparison is the source file's mtime against the staging moment —
    /// a stat, not a hash, because this runs on the once-a-second host tick
    /// and the answer it feeds is "offer a roll-forward", not "prove the
    /// bytes differ". The roll itself re-fingerprints; a same-bytes touch
    /// costs one restage and nothing else.
    ///
    /// `None` when nothing was ever staged, which is not "current" — it is a
    /// launch with no image to be behind.
    pub fn image_standing(&self) -> Option<ImageStanding> {
        let image = self.inner.supervisor.image()?;
        let source_changed = std::fs::metadata(&image.source_path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|modified| {
                u64::try_from(modified.as_millis()).unwrap_or(u64::MAX) > image.staged_at_ms
            });
        Some(ImageStanding {
            fingerprint: image.fingerprint,
            staged_at_ms: image.staged_at_ms,
            source_changed,
        })
    }

    /// Ask the identity daemon to stop, when this client stood one up.
    ///
    /// The going-offline half of an exit: `lifecycle::exit` stops what the
    /// supervisor owns, and the identity daemon is deliberately not that — so
    /// without this, "go offline" left the one process that keeps every Space
    /// converging, and the exit report told a person their device had stopped
    /// serving while it kept serving. A standalone launch made no daemon and
    /// asks nothing.
    pub async fn stop_identity_daemon(&self) {
        if !self.inner.hosted {
            return;
        }
        if let Ok(daemon) = self.daemon() {
            // HostRestart stops the daemon once the reply is on the wire, and
            // nothing here stands a fresh one up — which is what makes it a
            // stop. A daemon that was already down needs nothing.
            let _ = daemon
                .request(
                    lait::control::ControlRoute::Daemon,
                    &lait::control::Request::HostRestart,
                    None,
                )
                .await;
        }
    }

    /// Restart the identity daemon onto the currently staged image.
    ///
    /// The daemon is not supervisor-owned, and a same-version rebuild looks
    /// compatible to the attach probe — so after a rebuild it would keep
    /// serving old code indefinitely unless told. Telling is two steps: ask
    /// it to stop on its own terms (`HostRestart` stops once the reply is on
    /// the wire), then stand a fresh one up from the staged copy.
    ///
    /// A standalone launch has no daemon on purpose, and rolling forward must
    /// not stand one up: nothing happens and that is the correct nothing.
    pub async fn roll_identity_daemon(&self) -> ClientResult<()> {
        if !self.inner.hosted {
            return Ok(());
        }
        let selection = selection_for(self.inner.identity.as_deref());
        if let Ok(daemon) = self.daemon() {
            // A daemon that was not up cannot be asked to stop, and does not
            // need to be — the ensure below stands the fresh one up either way.
            let _ = daemon
                .request(
                    lait::control::ControlRoute::Daemon,
                    &lait::control::Request::HostRestart,
                    None,
                )
                .await;
        }
        let image = self
            .inner
            .supervisor
            .image()
            .map(|image| PathBuf::from(image.staged_path))
            .ok_or_else(|| {
                ClientError::refused("nothing is staged, so there is no image to roll onto")
            })?;
        lait::host_client::ensure_lait_daemon_with_executable(&selection, &image)
            .await
            .map_err(|error| {
                ClientError::unreachable(format!("restart the identity daemon: {error:#}"))
            })
    }
}

/// The staged image this client spawns from, as a fact a surface can draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageStanding {
    /// Content hash of the staged bytes. Two processes reporting the same
    /// fingerprint run the same code, whatever their paths say.
    pub fingerprint: String,
    pub staged_at_ms: u64,
    /// The source was rebuilt after this image was staged: a roll-forward
    /// would change what runs.
    pub source_changed: bool,
}

fn selection_for(identity: Option<&std::path::Path>) -> lait::config::Selection {
    match identity {
        Some(home) => lait::config::Selection::for_identity(home),
        None => lait::config::Selection::default(),
    }
}

/// What a client needs to exist.
#[derive(Clone, Debug)]
pub struct Config {
    pub state_root: PathBuf,
    pub executable: PathBuf,
    pub observation_interval: std::time::Duration,
    pub staging: lait_workbench::Staging,
    /// `None` selects the ordinary per-user identity.
    pub identity: Option<PathBuf>,
    /// Do not spawn or wait for the identity daemon (the `lait` sidecar).
    ///
    /// The client comes up standalone: the Library, the correspondence desk and
    /// anything else that does not need a hosted identity work; the parts that
    /// do show as unreachable rather than blocking the whole window for the
    /// twenty seconds `ensure_lait_daemon` waits. Set from `LAIT_SKIP_SIDECAR`
    /// for a demo or a dev run against a machine with no daemon — and off by
    /// default, because the ordinary launch *is* the identity host.
    pub skip_sidecar: bool,
    /// Stand up the in-process correspondence fixture, from
    /// `LAIT_CORRESPONDENCE_DEMO`. Off by default: correspondence is connected
    /// to no carrier until a real one exists, and the actions refuse honestly.
    /// On, it drives and validates the chat UI with no daemon.
    pub correspondence_demo: bool,
    /// The base URL of a hosted Post to carry **real** correspondence over, from
    /// `LAIT_POST_URL`. `Some` connects the client to a live `lait-post` and
    /// takes precedence over the demo fixture — real carriage beats a loopback
    /// one. `None` (the default) leaves correspondence on the fixture, or
    /// unconnected.
    pub post_url: Option<String>,
}

impl Config {
    pub fn new(state_root: PathBuf, executable: PathBuf) -> Self {
        // A development build spawns from a staged copy under the managed
        // root, so a rebuild never contends with a process holding the image
        // — the tax `lait_workbench::staging` exists to remove. A packaged
        // client has no build to contend with, and spawning the installed
        // executable in place keeps one fewer path to explain.
        let staging = if cfg!(debug_assertions) {
            lait_workbench::Staging::Staged {
                root: state_root.join("images"),
            }
        } else {
            lait_workbench::Staging::Direct
        };
        Self {
            state_root,
            executable,
            observation_interval: lait_workbench::OBSERVATION_INTERVAL,
            staging,
            identity: None,
            skip_sidecar: false,
            correspondence_demo: false,
            post_url: None,
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::Config;
    use std::path::PathBuf;

    /// The correspondence fixture is opt-in: absent from a default Config.
    #[test]
    fn the_correspondence_fixture_is_off_by_default() {
        let config = Config::new(PathBuf::from("state"), PathBuf::from("lait"));
        assert!(!config.correspondence_demo);
    }
}
