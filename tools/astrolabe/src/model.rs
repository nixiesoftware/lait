//! The App-owned model: one entity, consuming one ordered stream.
//!
//! This is the *only* model of client state. Nothing mirrors it, nothing
//! re-encodes it, and no parallel copy exists to drift — because there is no
//! boundary left to hand a copy across.
//!
//! State moves in exactly two ways: a snapshot replaces it wholesale, and a
//! signal invalidates it. There is no third way, and in particular there is no
//! optimistic local mutation. A surface that wrote what it *expected* an action
//! to do would be a second model of the same state, disagreeing with the first
//! whenever the action was refused — which is the case that matters.

use std::collections::VecDeque;

use lait_workbench::{
    ClientSignal, ConnectionSnapshot, DeviceSnapshot, ObservationState, SnapshotReason,
    WorkbenchSnapshot,
};

use crate::client::library::LibraryEntry;
use crate::client::ClientError;

/// Everything the interface draws.
#[derive(Debug, Default)]
pub struct App {
    /// The last authoritative snapshot. `None` before the first one arrives —
    /// which is *loading*, and is not the same as a machine with nothing on it.
    snapshot: Option<WorkbenchSnapshot>,
    library: Option<Vec<LibraryEntry>>,
    /// Set when a signal says this model can no longer be derived from what it
    /// has seen. Cleared only by taking a fresh snapshot.
    stale: Option<StaleReason>,
    /// The most recent failures, newest first, bounded. Errors are state a
    /// surface draws, not something logged and lost.
    failures: VecDeque<Failure>,
    /// How many signals this model has consumed. Lets a test assert that a
    /// stream was actually drained rather than merely opened.
    consumed: u64,
}

/// Why the model needs a fresh snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// Nothing has been read yet.
    NeverLoaded,
    /// The stream said so, and said why.
    Signalled(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub what: String,
    pub error: ClientError,
}

const FAILURE_CAPACITY: usize = 16;

impl App {
    pub fn new() -> Self {
        Self {
            stale: Some(StaleReason::NeverLoaded),
            ..Self::default()
        }
    }

    /// Replace the model with an authoritative reading.
    pub fn absorb(&mut self, snapshot: WorkbenchSnapshot) {
        self.snapshot = Some(snapshot);
        self.stale = None;
    }

    pub fn absorb_library(&mut self, library: Vec<LibraryEntry>) {
        self.library = Some(library);
    }

    /// Consume one signal.
    ///
    /// Events do not carry state — they invalidate it. The model records that a
    /// re-read is due and lets whoever owns the reading decide when; a model
    /// that fetched inside this call would make every event a round trip and
    /// would do it on whatever thread happened to deliver the signal.
    pub fn consume(&mut self, signal: &ClientSignal) {
        self.consumed = self.consumed.saturating_add(1);
        match signal {
            ClientSignal::Event(_) => {
                // An ordinary event means the snapshot is behind, not unusable.
                // It is still drawn — with the previous figures — until a fresh
                // one lands, because blanking a surface on every event is a
                // worse lie than a slightly old number.
            }
            ClientSignal::SnapshotRequired(reason) => {
                self.stale = Some(StaleReason::Signalled(describe(reason)));
            }
            ClientSignal::WorldCall(_) => {
                // CLIENT-19, and v-next. The variant is matched exhaustively so
                // that landing it is a compile error here rather than a silent
                // drop.
            }
        }
    }

    pub fn fail(&mut self, what: impl Into<String>, error: ClientError) {
        self.failures.push_front(Failure {
            what: what.into(),
            error,
        });
        self.failures.truncate(FAILURE_CAPACITY);
    }

    pub fn snapshot(&self) -> Option<&WorkbenchSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn library(&self) -> Option<&[LibraryEntry]> {
        self.library.as_deref()
    }

    pub fn devices(&self) -> &[DeviceSnapshot] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.devices.as_slice())
    }

    pub fn connections(&self) -> &[ConnectionSnapshot] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.connections.as_slice())
    }

    pub fn failures(&self) -> impl Iterator<Item = &Failure> {
        self.failures.iter()
    }

    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    pub fn stale(&self) -> Option<&StaleReason> {
        self.stale.as_ref()
    }

    /// Nothing has been read yet. Distinct from "read, and there was nothing" —
    /// the two look identical on screen unless a surface is told them apart.
    pub fn is_loading(&self) -> bool {
        self.snapshot.is_none()
    }

    /// Any device whose figures are known to be out of date.
    ///
    /// A surface draws these as degraded rather than as absent. Rendering a
    /// sampling failure as "no peers" is a defect the release gate tests for
    /// directly, and this is the query that makes drawing it correctly easy.
    pub fn degraded(&self) -> impl Iterator<Item = &DeviceSnapshot> {
        self.devices()
            .iter()
            .filter(|device| device.observation.state == ObservationState::Degraded)
    }
}

fn describe(reason: &SnapshotReason) -> String {
    match reason {
        SnapshotReason::ConsumerLagged { dropped } => {
            format!("{dropped} signal(s) were dropped before this one could be read")
        }
        SnapshotReason::DeviceRestarted { device_id } => {
            format!("device '{device_id}' restarted")
        }
        SnapshotReason::Reloaded => "the fleet was rebuilt and restarted".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lait_workbench::{
        BackendEvent, Capabilities, EnvironmentSnapshot, EventKind, LifecycleState,
        ObservationHealth,
    };

    fn snapshot(devices: Vec<DeviceSnapshot>) -> WorkbenchSnapshot {
        WorkbenchSnapshot {
            schema_version: 1,
            revision: 1,
            environment: EnvironmentSnapshot {
                state_root: "root".into(),
                executable: "lait".into(),
                server_pid: 1,
            },
            capabilities: Capabilities::default(),
            devices,
            connections: Vec::new(),
        }
    }

    fn device(id: &str, observation: ObservationHealth) -> DeviceSnapshot {
        DeviceSnapshot {
            id: id.into(),
            label: id.into(),
            home: "home".into(),
            log_path: "log".into(),
            state: LifecycleState::Running,
            pid: Some(1),
            owned: true,
            started_at_ms: None,
            last_error: None,
            facts: None,
            observation,
            image: None,
        }
    }

    /// Loading and empty are different states, and a model that cannot tell
    /// them apart guarantees a surface that draws "no devices" at a machine it
    /// has not finished asking.
    #[test]
    fn loading_is_not_the_same_as_empty() {
        let mut app = App::new();
        assert!(app.is_loading());
        assert_eq!(app.stale(), Some(&StaleReason::NeverLoaded));

        app.absorb(snapshot(Vec::new()));
        assert!(!app.is_loading(), "an answered read still reads as loading");
        assert!(app.devices().is_empty());
        assert!(app.stale().is_none());
    }

    /// An ordinary event does not blank the model. A surface that cleared on
    /// every event would flicker through empty on the way to the same numbers.
    #[test]
    fn an_event_leaves_the_last_good_figures_standing() {
        let mut app = App::new();
        app.absorb(snapshot(vec![device(
            "alice",
            ObservationHealth::default(),
        )]));
        app.consume(&ClientSignal::Event(BackendEvent {
            revision: 2,
            at_ms: 0,
            kind: EventKind::LogChanged,
            device_id: Some("alice".into()),
            message: "log grew".into(),
        }));
        assert_eq!(app.devices().len(), 1);
        assert!(app.stale().is_none());
        assert_eq!(app.consumed(), 1);
    }

    /// A snapshot-required is the one thing that marks the model underivable,
    /// and it carries why, because "reload" with no reason is not something a
    /// person can act on.
    #[test]
    fn a_snapshot_required_marks_the_model_stale_and_says_why() {
        let mut app = App::new();
        app.absorb(snapshot(Vec::new()));
        app.consume(&ClientSignal::SnapshotRequired(
            SnapshotReason::ConsumerLagged { dropped: 12 },
        ));
        let StaleReason::Signalled(reason) = app.stale().expect("stale") else {
            panic!("a lag did not mark the model stale");
        };
        assert!(
            reason.contains("12"),
            "the reason lost the figure: {reason}"
        );

        // The old figures are still drawn until a fresh read lands: stale is
        // not blank.
        assert!(!app.is_loading());
        app.absorb(snapshot(Vec::new()));
        assert!(
            app.stale().is_none(),
            "a fresh snapshot did not clear stale"
        );
    }

    /// The query a surface uses to draw degraded as degraded rather than as
    /// absence.
    #[test]
    fn degraded_devices_are_findable_without_inspecting_every_field() {
        let mut app = App::new();
        app.absorb(snapshot(vec![
            device("alice", ObservationHealth::default()),
            device(
                "bob",
                ObservationHealth {
                    state: ObservationState::Degraded,
                    sampled_at_ms: Some(10),
                    stale_since_ms: Some(20),
                    error: Some("control channel refused".into()),
                },
            ),
        ]));
        let degraded: Vec<&str> = app.degraded().map(|device| device.id.as_str()).collect();
        assert_eq!(degraded, vec!["bob"]);
    }

    #[test]
    fn failures_are_state_the_surface_can_draw_and_are_bounded() {
        let mut app = App::new();
        for index in 0..(FAILURE_CAPACITY + 4) {
            app.fail(
                format!("action {index}"),
                ClientError::refused("device is running"),
            );
        }
        assert_eq!(app.failures().count(), FAILURE_CAPACITY);
        assert_eq!(
            app.failures().next().map(|failure| failure.what.clone()),
            Some(format!("action {}", FAILURE_CAPACITY + 3)),
            "the newest failure is not the one a surface shows first"
        );
    }
}
