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
}

impl Client {
    /// Start the supervisor and bind the control planes to one identity.
    ///
    /// Returns the signal stream alongside, and for the same reason the
    /// supervisor does: a consumer that could hold a client without already
    /// holding its stream would have a window in which events vanish.
    pub async fn start(config: Config) -> ClientResult<(Self, Signals)> {
        let selection = selection_for(config.identity.as_deref());
        // The sidecar is what makes this a hosted identity. Skipping it is a
        // deliberate standalone launch: the daemon-backed planes will report
        // unreachable, but the window comes up at once instead of waiting on a
        // daemon that is not meant to be there.
        if !config.skip_sidecar {
            lait::host_client::ensure_lait_daemon_with_executable(&selection, &config.executable)
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
        let (supervisor, signals) = Supervisor::start(SupervisorConfig {
            state_root: config.state_root,
            executable: config.executable,
            observation_interval: config.observation_interval,
            staging: config.staging,
        })
        .await
        .map_err(ClientError::from)?;
        Ok((
            Self {
                inner: Arc::new(Inner {
                    supervisor,
                    identity: config.identity,
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
}

impl Config {
    pub fn new(state_root: PathBuf, executable: PathBuf) -> Self {
        Self {
            state_root,
            executable,
            observation_interval: lait_workbench::OBSERVATION_INTERVAL,
            staging: lait_workbench::Staging::Direct,
            identity: None,
            skip_sidecar: false,
            correspondence_demo: false,
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
