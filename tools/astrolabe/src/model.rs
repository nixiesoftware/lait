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
//!
//! ## What is in flight is not a third way
//!
//! The model records which actions this client has asked for and not yet heard
//! back about. That is not a claim about the machine — it is a claim about this
//! process's own outstanding requests, which nothing else can know and nothing
//! else can contradict. It is what lets a control disable itself while its own
//! action runs without any surface guessing at the result.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use lait_workbench::{
    ClientSignal, ConnectionHistoryPage, ConnectionSnapshot, DeviceSnapshot, EventHistoryPage,
    EventKind, HeadFacts, LogPage, ObservationState, SnapshotReason, WorkbenchSnapshot,
};

use crate::client::book::BookSnapshot;
use crate::client::heads::McpBindingOutcome;
use crate::client::host::HostContext;
use crate::client::library::{LaunchTicket, LibraryEntry, WorldStanding};
use crate::client::space::SpaceView;
use crate::client::storage::{StorageFacts, TransferFacts};
use crate::client::ClientError;
use crate::lifecycle::ExitReport;
use crate::notify::{self, Interruption};
use crate::runtime::{Action, Outcome, Read, Update};

/// Everything the interface draws.
#[derive(Debug, Default)]
pub struct App {
    /// The last authoritative snapshot. `None` before the first one arrives —
    /// which is *loading*, and is not the same as a machine with nothing on it.
    snapshot: Option<WorkbenchSnapshot>,
    library: Option<Vec<LibraryEntry>>,
    /// What the daemon has learned about each World's channel, keyed by World
    /// id. A World absent from this map has never been checked — which is not
    /// "up to date", and draws no update affordance at all.
    world_standings: BTreeMap<String, WorldStanding>,
    /// What each Space is holding. Empty until an engine read supplies it —
    /// and empty is drawn as "no Spaces", not as "zero bytes", because those
    /// are different claims.
    storage: Vec<StorageFacts>,
    transfers: Vec<TransferFacts>,
    heads: Vec<HeadFacts>,
    /// Orientation: this build, this identity, and the Orbits it has. `None`
    /// before the first read, for the same reason `snapshot` is.
    context: Option<HostContext>,
    /// Self-hosted receiver enrollment, assignments, and health. `None` until
    /// the identity daemon's display service has answered once.
    display: Option<lait::control::DisplayCoordinatorView>,
    /// This machine as a screen. `None` is not Big Picture; `Some` is, whether
    /// or not it has drawn anything yet.
    presentation: Option<Presentation>,
    /// The last MCP binding authored or previewed. Held because a preview is
    /// only useful if it stays on screen long enough to be read.
    mcp: Option<McpBindingOutcome>,
    /// The last page of each bounded read, and only the last: these are pages
    /// through something the supervisor owns, not a second copy of it.
    logs: Option<LogPage>,
    events: Option<EventHistoryPage>,
    transitions: Option<ConnectionHistoryPage>,
    /// The Space somebody is administering, as it last answered.
    space: Option<SpaceView>,
    /// The identity's address book. `None` until the first successful read.
    book: Option<BookSnapshot>,
    /// What passive presence sampling last measured. `None` until a pass has
    /// run; a pass that could not run leaves the last measurement in place,
    /// under the staleness the model already wears.
    presence: Option<crate::client::presence::PresenceMap>,
    /// How many times each device's log has been reported to have changed.
    ///
    /// A counter rather than a flag, and the *only* thing this model derives
    /// from an event's body. It is invalidation bookkeeping of exactly the kind
    /// `stale` already is: it says a read is due, never what the read would
    /// say. A surface following a log compares it with what it last acted on,
    /// which turns a stream of events into one read per change instead of one
    /// read per frame.
    log_changes: BTreeMap<String, u64>,
    /// What an exit did. Set once, on the way out, and read by the shell.
    exit: Option<ExitReport>,
    /// Set when a signal says this model can no longer be derived from what it
    /// has seen. Cleared only by taking a fresh snapshot.
    stale: Option<StaleReason>,
    /// The most recent failures, newest first, bounded. Errors are state a
    /// surface draws, not something logged and lost.
    failures: VecDeque<Failure>,
    /// What happened, newest first, bounded. The counterpart of `failures`: an
    /// action that worked and left no trace is indistinguishable from one that
    /// was never dispatched.
    notices: VecDeque<Notice>,
    /// Actions asked for and not yet answered.
    in_flight: BTreeSet<String>,
    /// What changed that somebody who is not looking might be told, oldest
    /// first.
    ///
    /// Recorded unfiltered. Muting is a policy about *interrupting*, not about
    /// observing — a model that dropped what a mute covers would make "unmute"
    /// mean "start noticing", which is not what a person who muted a Space for
    /// an hour asked for.
    unsaid: VecDeque<Interruption>,
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

/// Something that worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub said: String,
    /// Where a browser was sent, when that is what happened. Carried so the
    /// surface can show the address rather than only claim a window opened —
    /// a browser that came up behind another window is otherwise a click with
    /// no visible result.
    pub launched: Option<LaunchTicket>,
}

const FAILURE_CAPACITY: usize = 16;
const NOTICE_CAPACITY: usize = 16;

impl App {
    pub fn new() -> Self {
        Self {
            stale: Some(StaleReason::NeverLoaded),
            ..Self::default()
        }
    }

    /// Take one update from the background half.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Snapshot(snapshot) => self.absorb(*snapshot),
            Update::Library(entries) => self.absorb_library(entries),
            Update::WorldStandings(standings) => self.absorb_world_standings(standings),
            Update::Storage(facts) => self.absorb_storage(facts, Vec::new()),
            Update::Heads(heads) => self.heads = heads,
            Update::Context(context) => self.absorb_context(*context),
            Update::Display(display) => self.display = Some(*display),
            Update::Presentation(presentation) => self.absorb_presentation(*presentation),
            Update::PresentationEnded => self.presentation = None,
            Update::Book(book) => self.book = Some(book),
            Update::Presence(presence) => self.presence = Some(presence),
            Update::Signal(signal) => self.consume(&signal),
            Update::Done { key, outcome } => {
                self.in_flight.remove(&key);
                self.record(outcome);
            }
            Update::Failed { key, what, error } => {
                if let Some(key) = key {
                    self.in_flight.remove(&key);
                }
                self.fail(what, error);
            }
        }
    }

    fn record(&mut self, outcome: Outcome) {
        let notice = match outcome {
            Outcome::Silent => return,
            Outcome::Said(said) => Notice {
                said,
                launched: None,
            },
            Outcome::Launched(launch) => Notice {
                said: format!("opened {}", launch.url),
                launched: Some(launch),
            },
            Outcome::Mcp(outcome) => {
                let said = if outcome.written {
                    format!("wrote {}", outcome.path)
                } else {
                    format!("this is what would be written to {}", outcome.path)
                };
                self.mcp = Some(*outcome);
                Notice {
                    said,
                    launched: None,
                }
            }
            Outcome::Read(read) => {
                match read {
                    Read::Logs(page) => {
                        // A reset means the file was rotated or truncated under
                        // us, so what was on screen is not the beginning of this
                        // one. Kept as the page's own flag rather than smoothed
                        // over, and drawn.
                        self.logs = Some(*page);
                    }
                    Read::Events(page) => self.events = Some(*page),
                    Read::Transitions(page) => self.transitions = Some(*page),
                    Read::Space(view) => self.space = Some(*view),
                }
                return;
            }
            Outcome::Exited(report) => {
                let said = describe_exit(&report);
                self.exit = Some(*report);
                Notice {
                    said,
                    launched: None,
                }
            }
        };
        self.notices.push_front(notice);
        self.notices.truncate(NOTICE_CAPACITY);
    }

    /// Record that this client has asked for something.
    ///
    /// Called by whoever dispatches, at the moment of dispatch, so a control is
    /// disabled on the frame the click happened rather than on whichever later
    /// frame the background half gets round to answering.
    pub fn dispatched(&mut self, action: &Action) {
        self.in_flight.insert(action.key());
    }

    /// Whether this client is waiting on the action a control would dispatch.
    /// Every action asked for and not yet answered.
    ///
    /// The plural of [`Self::is_in_flight`], and it exists for the same reason
    /// that one does: a surface drawn across a boundary cannot ask "is this
    /// key in flight" one key at a time, so it is sent the set and asks
    /// locally. Sending the set rather than a per-control flag keeps the
    /// question the model's to answer.
    pub fn in_flight_keys(&self) -> Vec<String> {
        self.in_flight.iter().cloned().collect()
    }

    pub fn is_in_flight(&self, key: &str) -> bool {
        self.in_flight.contains(key)
    }

    /// Replace the model with an authoritative reading.
    ///
    /// What changed between this reading and the last is worked out *before*
    /// the replacement, because afterwards there is nothing to compare against.
    /// That diff is the whole of what this client can notify about without
    /// speaking a World's vocabulary — and it is a fact about two observations
    /// rather than an inference about events.
    pub fn absorb(&mut self, snapshot: WorkbenchSnapshot) {
        let changed = notify::between(
            self.snapshot
                .as_ref()
                .map(|was| (was.devices.as_slice(), was.connections.as_slice())),
            (snapshot.devices.as_slice(), snapshot.connections.as_slice()),
        );
        for notice in changed {
            push_bounded(&mut self.unsaid, notice);
        }
        self.snapshot = Some(snapshot);
        self.stale = None;
    }

    /// Take everything that has not been said, leaving nothing behind.
    ///
    /// A drain rather than a read: a notice is an event, not a state anybody
    /// can re-read, and answering the same one twice would interrupt somebody
    /// twice about one thing.
    pub fn take_unsaid(&mut self) -> Vec<Interruption> {
        self.unsaid.drain(..).collect()
    }

    pub fn absorb_library(&mut self, library: Vec<LibraryEntry>) {
        self.library = Some(library);
    }

    /// Replace wholesale rather than merge: a World that has dropped out of
    /// the map has stopped being known, and merging would keep drawing an
    /// update offer from a reading nothing stands behind any more.
    pub fn absorb_world_standings(&mut self, standings: BTreeMap<String, WorldStanding>) {
        self.world_standings = standings;
    }

    /// What is known about one World's channel, if anything is.
    pub fn world_standing(&self, world: &str) -> Option<&WorldStanding> {
        self.world_standings.get(world)
    }

    pub fn absorb_storage(&mut self, storage: Vec<StorageFacts>, transfers: Vec<TransferFacts>) {
        self.storage = storage;
        self.transfers = transfers;
    }

    pub fn absorb_heads(&mut self, heads: Vec<HeadFacts>) {
        self.heads = heads;
    }

    pub fn absorb_context(&mut self, context: HostContext) {
        let current: BTreeSet<(String, String)> = context
            .asks
            .iter()
            .map(|ask| (ask.space.clone(), ask.name.clone()))
            .collect();
        let previous = self.context.as_ref().map(|was| {
            was.asks
                .iter()
                .map(|ask| (ask.space.clone(), ask.name.clone()))
                .collect()
        });
        for notice in notify::asks_between(previous.as_ref(), &current) {
            push_bounded(&mut self.unsaid, notice);
        }
        self.context = Some(context);
    }

    pub fn absorb_book(&mut self, book: BookSnapshot) {
        self.book = Some(book);
    }

    pub fn storage(&self) -> &[StorageFacts] {
        &self.storage
    }

    pub fn transfers(&self) -> &[TransferFacts] {
        &self.transfers
    }

    pub fn heads(&self) -> &[HeadFacts] {
        &self.heads
    }

    pub fn context(&self) -> Option<&HostContext> {
        self.context.as_ref()
    }

    pub fn display(&self) -> Option<&lait::control::DisplayCoordinatorView> {
        self.display.as_ref()
    }

    pub fn presentation(&self) -> Option<&Presentation> {
        self.presentation.as_ref()
    }

    /// Keep the last verified render across a failed re-ask, and only for the
    /// *same* selection.
    ///
    /// A screen that has been showing a program should not go dark because one
    /// re-ask timed out; it should say it is stale. But a selection that
    /// changed has no claim on the previous one's pixels — inheriting them
    /// would draw one program under another program's name, which is worse
    /// than an empty screen because it is legible and wrong.
    fn absorb_presentation(&mut self, mut next: Presentation) {
        if next.rendered.is_none() && next.selection.is_some() {
            if let Some(held) = self.presentation.as_ref() {
                if held.selection == next.selection {
                    next.rendered.clone_from(&held.rendered);
                }
            }
        }
        self.presentation = Some(next);
    }

    pub fn mcp(&self) -> Option<&McpBindingOutcome> {
        self.mcp.as_ref()
    }

    pub fn logs(&self) -> Option<&LogPage> {
        self.logs.as_ref()
    }

    pub fn events(&self) -> Option<&EventHistoryPage> {
        self.events.as_ref()
    }

    pub fn transitions(&self) -> Option<&ConnectionHistoryPage> {
        self.transitions.as_ref()
    }

    pub fn space(&self) -> Option<&SpaceView> {
        self.space.as_ref()
    }

    pub fn book(&self) -> Option<&BookSnapshot> {
        self.book.as_ref()
    }

    pub fn presence(&self) -> Option<&crate::client::presence::PresenceMap> {
        self.presence.as_ref()
    }

    /// How many log changes this model has been told about for `device`.
    ///
    /// A surface that follows a log compares this with the value it last read
    /// at. Equal means nothing has happened since; different means one read is
    /// due, not a read per frame.
    pub fn log_changes(&self, device: &str) -> u64 {
        self.log_changes.get(device).copied().unwrap_or_default()
    }

    /// What an exit did, once one has happened. The shell's cue to close.
    pub fn exit(&self) -> Option<&ExitReport> {
        self.exit.as_ref()
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
            ClientSignal::Event(event) => {
                // An ordinary event means the snapshot is behind, not unusable.
                // It is still drawn — with the previous figures — until a fresh
                // one lands, because blanking a surface on every event is a
                // worse lie than a slightly old number.
                //
                // The one thing counted is that a log grew, and only so a
                // surface following it knows a read is due. Nothing about the
                // event's contents reaches state.
                if let (EventKind::LogChanged, Some(device)) =
                    (event.kind, event.device_id.as_ref())
                {
                    let seen = self.log_changes.entry(device.clone()).or_default();
                    *seen = seen.saturating_add(1);
                }
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

    pub fn notices(&self) -> impl Iterator<Item = &Notice> {
        self.notices.iter()
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

/// Keep the newest, drop the oldest. A queue that grew without bound would
/// hold a night's worth of peer arrivals for a window nobody opened.
fn push_bounded(queue: &mut VecDeque<Interruption>, notice: Interruption) {
    queue.push_back(notice);
    while queue.len() > NOTICE_CAPACITY {
        queue.pop_front();
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

/// What an exit did, in one line.
///
/// Every device it left running is named. "Closed" with three daemons still up
/// is true by omission and false as an account of what a person just did.
fn describe_exit(report: &ExitReport) -> String {
    let mut said = if report.stopped.is_empty() {
        "closed; nothing was stopped".to_owned()
    } else {
        format!("stopped {}", report.stopped.join(", "))
    };
    if !report.left_running.is_empty() {
        let left: Vec<&str> = report
            .left_running
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        said = format!("{said} — still running: {}", left.join(", "));
    }
    said
}

/// This machine acting as a screen.
///
/// The *member* profile: the client holds the Space these pixels came from, so
/// there is no pairing, no credential and no assignment behind this — only a
/// choice made here, which is why leaving is always available and revocation is
/// simply the Query no longer answering.
#[derive(Debug, Clone)]
pub struct Presentation {
    /// What this screen was told to show, or `None` for a screen that has been
    /// entered and not yet pointed at anything.
    ///
    /// Being a screen and showing something are separate facts. Requiring a
    /// selection to enter would make the mode a property of the content, which
    /// is backwards: a person presses the control to *become* a screen, and
    /// choosing is what they do once they are one.
    pub selection: Option<PresentationSelection>,
    /// The last successful render. Kept across a failed refresh, so a screen
    /// that briefly could not be re-asked keeps showing what it last verified
    /// instead of going blank on a stumble.
    pub rendered: Option<lait::control::DisplayPresentationView>,
    /// Why the last attempt did not answer. Held *beside* `rendered` rather
    /// than replacing it: "this is stale and here is why" and "there is nothing
    /// to show" are different things to tell somebody standing in front of a
    /// screen.
    pub failure: Option<String>,
}

/// What a member screen was told to show. Exactly the tuple an assignment
/// commits, minus everything an assignment adds for a stranger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationSelection {
    pub orbit: String,
    pub world: String,
    pub surface: String,
    /// The package's own input, uncanonicalized. The daemon hands it to the
    /// package's canonicalizer; nothing here inspects it.
    pub input: String,
    /// What to call this on screen while it is loading or refusing.
    pub title: String,
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

    /// What is in flight is cleared by the answer, whichever answer it is. A
    /// refusal that left a control disabled forever would make the safest
    /// possible outcome — being told no — the one that breaks the interface.
    #[test]
    fn a_refusal_clears_what_it_was_answering_just_as_a_success_does() {
        let mut app = App::new();
        let stop = Action::StopDevice("alice".into());
        app.dispatched(&stop);
        assert!(app.is_in_flight(&stop.key()));

        app.apply(Update::Failed {
            key: Some(stop.key()),
            what: stop.what(),
            error: ClientError::refused("device is not running"),
        });
        assert!(
            !app.is_in_flight(&stop.key()),
            "a refused action stayed in flight forever"
        );
        assert_eq!(app.failures().count(), 1);

        let start = Action::StartDevice("alice".into());
        app.dispatched(&start);
        app.apply(Update::Done {
            key: start.key(),
            outcome: Outcome::Said("alice is starting".into()),
        });
        assert!(!app.is_in_flight(&start.key()));
        assert_eq!(
            app.notices().next().map(|notice| notice.said.clone()),
            Some("alice is starting".to_owned())
        );
    }

    /// One device being busy must not disable another's controls.
    #[test]
    fn what_is_in_flight_is_per_control_rather_than_per_surface() {
        let mut app = App::new();
        app.dispatched(&Action::StopDevice("alice".into()));
        assert!(app.is_in_flight("device.stop:alice"));
        assert!(
            !app.is_in_flight("device.stop:bob"),
            "one device's action disabled another device's control"
        );
    }

    /// A re-read is not an event worth reporting. A record that gained a line
    /// every time a surface refreshed is a record nobody reads.
    #[test]
    fn a_silent_outcome_leaves_no_trace_but_still_clears_what_it_answered() {
        let mut app = App::new();
        app.dispatched(&Action::Refresh);
        app.apply(Update::Done {
            key: Action::Refresh.key(),
            outcome: Outcome::Silent,
        });
        assert!(!app.is_in_flight("refresh"));
        assert_eq!(app.notices().count(), 0);
    }

    /// An exit that left daemons running says so. "Closed" with three still up
    /// is true by omission and false as an account of what just happened.
    #[test]
    fn an_exit_names_what_it_left_running() {
        use crate::lifecycle::LeftRunning;

        let mut app = App::new();
        app.apply(Update::Done {
            key: "exit".into(),
            outcome: Outcome::Exited(Box::new(ExitReport {
                stopped: vec!["alice".into()],
                left_running: vec![("bob".into(), LeftRunning::NotOurs)],
            })),
        });
        let said = app
            .notices()
            .next()
            .map(|notice| notice.said.clone())
            .expect("an exit was recorded");
        assert!(said.contains("alice"), "{said}");
        assert!(
            said.contains("bob"),
            "the exit did not say what it left running: {said}"
        );
        assert!(app.exit().is_some(), "the shell has no cue to close on");
    }

    fn context_with(asks: Vec<lait::control::SponsorshipAsk>) -> HostContext {
        HostContext {
            version: "lait".into(),
            identity_home: "home".into(),
            spaces_root: "root".into(),
            worlds: Vec::new(),
            identities: Vec::new(),
            orbits: Vec::new(),
            asks,
        }
    }

    /// A pending ask is news the first time the host plane reports it, and
    /// not news the second time. Approving it (the list shrinks) is not an
    /// interruption — the action's own notice is the record of that.
    #[test]
    fn a_new_sponsorship_ask_is_unsaid_once() {
        let mut app = App::new();
        app.absorb_context(context_with(vec![lait::control::SponsorshipAsk {
            space: "ws_one".into(),
            name: "grok".into(),
            actor: None,
            asked_at_ms: 1,
        }]));
        let first = app.take_unsaid();
        assert_eq!(
            first,
            vec![Interruption::SponsorshipAsked {
                space: "ws_one".into(),
                agent: "grok".into()
            }]
        );
        app.absorb_context(context_with(vec![lait::control::SponsorshipAsk {
            space: "ws_one".into(),
            name: "grok".into(),
            actor: None,
            asked_at_ms: 1,
        }]));
        assert!(
            app.take_unsaid().is_empty(),
            "the same ask interrupted twice"
        );
        app.absorb_context(context_with(Vec::new()));
        assert!(
            app.take_unsaid().is_empty(),
            "clearing an ask was reported as news"
        );
    }
}
