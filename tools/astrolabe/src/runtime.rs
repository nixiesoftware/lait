//! Where the background work lives, and how it reaches the frame loop.
//!
//! Supervision, control-protocol traffic and sampling never run on the UI
//! thread. They run on a Tokio runtime owned by this module and reach the frame
//! loop only as [`Update`]s on a channel the interface drains once per frame.
//!
//! That is one boundary, and it is a channel rather than a bridge. Nothing is
//! serialized across it and nothing is mirrored: an `Update` carries the
//! authoritative value itself, moved, and the model on the other side takes
//! ownership. There is still exactly one model of client state.
//!
//! ## Actions go the other way, and never return a value
//!
//! A surface asks for something by handing back an [`Action`]; it never receives
//! an answer inline. Every outcome arrives as an `Update` like any other, which
//! is what keeps the "no optimistic local mutation" rule structural rather than
//! a thing to remember: there is no return value a surface could write down.
//!
//! Each action runs as its own task. Starting a head can take twenty seconds,
//! and an action queue that ran on the same task as the signal drain would stall
//! the stream long enough to lag the consumer — turning "the person clicked
//! Open" into a spurious snapshot-required.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::{
    error::TryRecvError, unbounded_channel, UnboundedReceiver, UnboundedSender,
};

use lait_workbench::{
    ClientSignal, ConnectionHistoryPage, EventHistoryPage, HeadFacts, HistoryQuery, LogPage,
    RemoveDeviceRequest, Signals, UpdateDeviceRequest, WorkbenchSnapshot,
};

use crate::client::display::DisplayAssignmentInput;
use crate::client::heads::{McpBinding, McpBindingOutcome};
use crate::client::host::HostContext;
use crate::client::library::{LaunchTicket, LibraryEntry};
use crate::client::space::{SpaceOp, SpaceRef, SpaceView};
use crate::client::storage::StorageFacts;
use crate::client::{Client, ClientError, ClientResult, Config};
use crate::lifecycle::{ExitReport, ExitRequest};
use crate::model::App;

/// Something a surface asked for.
///
/// Every variant is a request, never a result. A surface that could learn the
/// outcome here would be holding a second reading of state the model already
/// owns, disagreeing with it in exactly the case that matters — when the action
/// was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Re-read everything. What a signal asks for, and what a person asks for
    /// when a surface has gone stale and they would rather not wait.
    Refresh,
    /// Hand a World's own head to the person's browser.
    ///
    /// Carries the mount as well as the path: the head is per World now, so
    /// opening names which one rather than reaching for whichever is up.
    OpenWorld {
        world: String,
        entry_path: String,
    },
    /// Fetch one World's newest bundle now.
    UpdateWorld {
        world: String,
    },
    /// Become a screen. Pressing the control *is* the consent; nothing is
    /// chosen yet and nothing needs to be.
    EnterPresentation,
    /// Point this screen at one exact surface.
    PresentHere(Box<crate::model::PresentationSelection>),
    /// Re-ask the current selection.
    PresentRefresh,
    LeavePresentation,
    StartDevice(String),
    StopDevice(String),
    RestartDevice(String),
    ForceStopDevice(String),
    /// Forget a device. `delete_data` additionally destroys what it holds, and
    /// is the one flag in this enum that cannot be undone.
    RemoveDevice {
        id: String,
        delete_data: bool,
    },
    CreateDevice {
        id: String,
        label: String,
    },
    /// Rename a device. Safe at any lifecycle state: a label names the device
    /// to a person and nothing resolves by it.
    RenameDevice {
        id: String,
        label: String,
    },
    /// Stop everything this client owns, and nothing it does not.
    StopAllOwned,
    /// One page of a device's log, from `cursor`.
    ///
    /// Paged through the bounded cursor rather than tailed: a renderer in the
    /// same process as the supervisor makes an unbounded tail *easier* to write
    /// and no less of a way to hold a log file's worth of lines in a frame loop.
    ReadLogs {
        device: String,
        cursor: Option<u64>,
    },
    /// One page of the event timeline, after `revision`.
    ReadEvents {
        after: Option<u64>,
    },
    /// One page of connection transitions, after `revision`.
    ReadTransitions {
        after: Option<u64>,
    },
    /// Everything one Space says about itself.
    ///
    /// Placing, unlike every listing in this client: a person has chosen this
    /// Space, and reading its membership means asking it.
    ReadSpace(SpaceRef),
    /// Ask a Space to do something.
    Administer {
        at: SpaceRef,
        operation: Box<SpaceOp>,
    },
    /// Start the browser head this client opens Worlds through.
    StartHead,
    StopHead(String),
    SendMessage {
        to: String,
        body: String,
    },
    CollectMail,
    /// Publish this identity's reach and report the card to hand over.
    ShareReach,
    /// Take in a correspondent's announcement.
    AddCorrespondent {
        announcement: String,
    },
    BlockSender(String),
    AcceptContact(String),
    OpenConversation(String),
    FocusConversation(String),
    CloseConversation(String),
    SpaceFound {
        home: String,
        name: String,
        nick: Option<String>,
    },
    SpaceEnter {
        link: String,
        home: String,
        nick: Option<String>,
    },
    DeviceConsent {
        token: String,
    },
    OrbitForget {
        space: String,
    },
    BookPut {
        card: Option<String>,
        name: String,
        note: Option<String>,
    },
    BookDelete {
        card: String,
    },
    /// Set a card's picture from a file on this machine; `None` clears it.
    BookSetPicture {
        card: String,
        path: Option<String>,
    },
    BookMerge {
        from: String,
        into: String,
    },
    BookClaimSelf {
        card: String,
    },
    BookLink {
        card: String,
        handle: String,
    },
    BookUnlink {
        card: String,
        handle: String,
    },
    BookExport {
        path: String,
        cards: Option<Vec<String>>,
    },
    BookPropose {
        path: String,
    },
    BookAccept {
        suggestion: String,
    },
    BookDismiss {
        suggestion: String,
    },
    OrbitRebuild {
        orbit: String,
    },
    /// Author, or preview, an MCP binding.
    InstallMcp {
        binding: Box<McpBinding>,
        preview: bool,
    },
    DisplayPairingApprove {
        pairing: String,
        label: String,
    },
    DisplayPairingReject(String),
    DisplayAssignmentPut(Box<DisplayAssignmentInput>),
    DisplayAssignmentRevoke(String),
    DisplayDeviceRevoke(String),
    /// Add a passphrase as a second unlock path for the identifier key.
    DisplayIdentifierAdmitPassphrase(String),
    Exit(ExitRequest),
}

impl Action {
    /// What is in flight, keyed so a surface can disable the one control that
    /// caused it rather than every control on the page.
    pub fn key(&self) -> String {
        match self {
            Self::Refresh => "refresh".into(),
            Self::OpenWorld { world, .. } => format!("open:{world}"),
            Self::UpdateWorld { world } => format!("world.update:{world}"),
            Self::StartDevice(id) => format!("device.start:{id}"),
            Self::StopDevice(id) => format!("device.stop:{id}"),
            Self::RestartDevice(id) => format!("device.restart:{id}"),
            Self::ForceStopDevice(id) => format!("device.force-stop:{id}"),
            Self::RemoveDevice { id, .. } => format!("device.remove:{id}"),
            Self::CreateDevice { id, .. } => format!("device.create:{id}"),
            Self::RenameDevice { id, .. } => format!("device.rename:{id}"),
            Self::StopAllOwned => "device.stop-all".into(),
            Self::ReadLogs { device, .. } => format!("logs:{device}"),
            Self::ReadEvents { .. } => "history.events".into(),
            Self::ReadTransitions { .. } => "history.connections".into(),
            Self::ReadSpace(at) => format!("space.read:{}", at.space),
            Self::EnterPresentation => "present.enter".into(),
            Self::PresentHere(_) => "present.choose".into(),
            Self::PresentRefresh => "present.refresh".into(),
            Self::LeavePresentation => "present.leave".into(),
            Self::Administer { at, operation } => format!("space:{}:{}", at.space, operation.key()),
            Self::StartHead => "head.start".into(),
            Self::StopHead(id) => format!("head.stop:{id}"),
            Self::SendMessage { to, .. } => format!("correspondence.send:{to}"),
            Self::CollectMail => "correspondence.collect".into(),
            Self::ShareReach => "reach.share".into(),
            Self::AddCorrespondent { .. } => "reach.add".into(),
            Self::BlockSender(person) => format!("correspondence.block:{person}"),
            Self::AcceptContact(person) => format!("correspondence.accept:{person}"),
            Self::OpenConversation(person) => format!("correspondence.open:{person}"),
            Self::FocusConversation(person) => format!("correspondence.focus:{person}"),
            Self::CloseConversation(person) => format!("correspondence.close:{person}"),
            Self::SpaceFound { home, .. } => format!("space.found:{home}"),
            Self::SpaceEnter { home, .. } => format!("space.enter:{home}"),
            Self::DeviceConsent { .. } => "device.consent".into(),
            Self::OrbitForget { space } => format!("orbit.forget:{space}"),
            Self::OrbitRebuild { orbit } => format!("orbit.rebuild:{orbit}"),
            Self::BookPut {
                card: Some(card), ..
            } => format!("book.put:{card}"),
            Self::BookPut { .. } => "book.put".into(),
            Self::BookDelete { card } => format!("book.delete:{card}"),
            Self::BookSetPicture { card, .. } => format!("book.picture:{card}"),
            Self::BookMerge { from, into } => format!("book.merge:{from}:{into}"),
            Self::BookClaimSelf { card } => format!("book.claim:{card}"),
            Self::BookLink { card, .. } => format!("book.link:{card}"),
            Self::BookUnlink { card, .. } => format!("book.unlink:{card}"),
            Self::BookExport { .. } => "book.export".into(),
            Self::BookPropose { .. } => "book.import".into(),
            Self::BookAccept { suggestion } => format!("book.accept:{suggestion}"),
            Self::BookDismiss { suggestion } => format!("book.dismiss:{suggestion}"),
            Self::InstallMcp { preview, .. } => {
                if *preview {
                    "mcp.preview".into()
                } else {
                    "mcp.install".into()
                }
            }
            Self::DisplayPairingApprove { pairing, .. } => {
                format!("display.pairing.approve:{pairing}")
            }
            Self::DisplayPairingReject(pairing) => format!("display.pairing.reject:{pairing}"),
            Self::DisplayAssignmentPut(assignment) => {
                format!("display.assignment.put:{}", assignment.device)
            }
            Self::DisplayAssignmentRevoke(assignment) => {
                format!("display.assignment.revoke:{assignment}")
            }
            Self::DisplayDeviceRevoke(device) => format!("display.device.revoke:{device}"),
            Self::DisplayIdentifierAdmitPassphrase(_) => "display.identifier.admit".into(),
            Self::Exit(_) => "exit".into(),
        }
    }

    /// What this was, in words, for a failure or a confirmation. Phrased as the
    /// thing attempted so both readings work: "open ORB … failed", and "open
    /// ORB" as a line in the record of what happened.
    pub fn what(&self) -> String {
        match self {
            Self::Refresh => "re-read this machine".into(),
            Self::ShareReach => "publish how to reach you".into(),
            Self::AddCorrespondent { .. } => "add a correspondent".into(),
            Self::OpenWorld { world, .. } => format!("open {world}"),
            Self::UpdateWorld { world } => format!("update {world}"),
            Self::StartDevice(id) => format!("start {id}"),
            Self::StopDevice(id) => format!("stop {id}"),
            Self::RestartDevice(id) => format!("restart {id}"),
            Self::ForceStopDevice(id) => format!("force-stop {id}"),
            Self::RemoveDevice {
                id,
                delete_data: true,
            } => format!("remove {id} and delete its data"),
            Self::RemoveDevice { id, .. } => format!("remove {id}"),
            Self::CreateDevice { id, .. } => format!("add device {id}"),
            Self::EnterPresentation => "become a screen".into(),
            Self::PresentHere(selection) => format!("present {}", selection.title),
            Self::PresentRefresh => "refresh what this screen shows".into(),
            Self::LeavePresentation => "leave Big Picture".into(),
            Self::RenameDevice { id, label } => format!("rename {id} to '{label}'"),
            Self::StopAllOwned => "stop everything this client owns".into(),
            Self::ReadLogs { device, .. } => format!("read {device}'s log"),
            Self::ReadEvents { .. } => "read the timeline".into(),
            Self::ReadTransitions { .. } => "read connection transitions".into(),
            Self::ReadSpace(at) => format!("read {}", at.space),
            Self::Administer { operation, .. } => operation.what(),
            Self::StartHead => "start a head".into(),
            Self::StopHead(id) => format!("stop head {id}"),
            Self::SendMessage { to, .. } => format!("send a message to {to}"),
            Self::CollectMail => "collect what is waiting".into(),
            Self::BlockSender(person) => format!("block {person}"),
            Self::AcceptContact(person) => format!("accept {person}"),
            Self::OpenConversation(person) => format!("open a chat with {person}"),
            Self::FocusConversation(person) => format!("focus the chat with {person}"),
            Self::CloseConversation(person) => format!("close the chat with {person}"),
            Self::SpaceFound { name, .. } => format!("found the Space '{name}'"),
            Self::SpaceEnter { .. } => "enter a Space from an invite".into(),
            Self::DeviceConsent { .. } => "sign this machine's consent".into(),
            Self::OrbitForget { space } => format!("forget {space}"),
            Self::OrbitRebuild { orbit } => format!("rebuild {orbit}"),
            Self::BookPut { name, .. } => format!("save the card '{name}'"),
            Self::BookDelete { card } => format!("delete card {card}"),
            Self::BookSetPicture {
                card,
                path: Some(_),
            } => format!("set the picture on {card}"),
            Self::BookSetPicture { card, path: None } => {
                format!("clear the picture on {card}")
            }
            Self::BookMerge { from, into } => format!("merge {from} into {into}"),
            Self::BookClaimSelf { card } => format!("claim {card} as My Card"),
            Self::BookLink { card, handle } => format!("link {handle} to {card}"),
            Self::BookUnlink { card, handle } => format!("unlink {handle} from {card}"),
            Self::BookExport { path, .. } => format!("export cards to {path}"),
            Self::BookPropose { path } => format!("stage suggestions from {path}"),
            Self::BookAccept { .. } => "accept a suggested card".to_owned(),
            Self::BookDismiss { .. } => "dismiss a suggested card".to_owned(),
            Self::InstallMcp { preview: true, .. } => "preview an MCP binding".into(),
            Self::InstallMcp { .. } => "write an MCP binding".into(),
            Self::DisplayPairingApprove { label, .. } => {
                format!("approve the display '{label}'")
            }
            Self::DisplayPairingReject(_) => "reject a display pairing".into(),
            Self::DisplayAssignmentPut(assignment) => {
                format!("assign display {}", assignment.device)
            }
            Self::DisplayAssignmentRevoke(assignment) => {
                format!("revoke display assignment {assignment}")
            }
            Self::DisplayDeviceRevoke(device) => format!("revoke display device {device}"),
            // Deliberately says nothing about the passphrase itself: this
            // string reaches a notice, a log line and a failure record.
            Self::DisplayIdentifierAdmitPassphrase(_) => {
                "add a passphrase to the identifier key".into()
            }
            Self::Exit(ExitRequest::GoOffline) => "go offline and exit".into(),
            Self::Exit(ExitRequest::StayOnline) => "close and stay online".into(),
        }
    }
}

/// Something that happened, on its way to the model.
pub enum Update {
    Snapshot(Box<WorkbenchSnapshot>),
    Library(Vec<LibraryEntry>),
    /// What the daemon has learned about each World's channel, keyed by World
    /// id. Separate from `Library` because the two are different kinds of
    /// fact: the list is compiled in and cannot go stale, this is measured and
    /// can.
    WorldStandings(std::collections::BTreeMap<String, crate::client::library::WorldStanding>),
    Storage(Vec<StorageFacts>),
    Heads(Vec<HeadFacts>),
    Context(Box<HostContext>),
    Display(Box<lait::control::DisplayCoordinatorView>),
    /// What this machine's own screen is presenting, and what it last answered.
    ///
    /// Carries the selection as well as the render, because a presentation that
    /// failed still has to say *what* it was trying to show — a fullscreen
    /// surface reporting only "something went wrong" is the absence that does
    /// not say which kind.
    Presentation(Box<crate::model::Presentation>),
    /// Big Picture is over. Distinct from an empty presentation, which is a
    /// screen showing nothing rather than a client that is no longer a screen.
    PresentationEnded,
    Book(crate::client::book::BookSnapshot),
    /// This identity's mailbox and arrival standing, after a correspondence
    /// action moved it. A whole snapshot, like [`Book`]: the model is pushed,
    /// never mutated in place by a surface.
    Correspondence(crate::model::Correspondence),
    /// What passive presence sampling measured this pass — including which
    /// Spaces answered at all, so absence keeps its kind.
    Presence(crate::client::presence::PresenceMap),
    Signal(ClientSignal),
    /// An action finished, and what it produced.
    Done {
        key: String,
        outcome: Outcome,
    },
    Failed {
        /// `None` for work nobody asked for — the periodic re-read, most of it.
        key: Option<String>,
        what: String,
        error: ClientError,
    },
}

/// What an action produced, beyond having worked.
pub enum Outcome {
    /// It worked and there is nothing to say about it. A re-read that announced
    /// itself would put a line in the record every time a surface refreshed,
    /// which is how a record of what happened becomes a thing nobody reads.
    Silent,
    /// Nothing but the fact that it worked, said in words.
    Said(String),
    /// A browser was handed a World's head, at this URL.
    Launched(LaunchTicket),
    /// An MCP binding was authored, or previewed.
    Mcp(Box<McpBindingOutcome>),
    /// An exit happened, and this is what it did.
    Exited(Box<ExitReport>),
    /// A page a surface asked for.
    ///
    /// An outcome rather than an update of its own, so one path clears what is
    /// in flight and lands the value. A read produces no line in the record —
    /// "read the timeline" happening is not news.
    Read(Read),
}

/// A page of something bounded.
pub enum Read {
    Logs(Box<LogPage>),
    Events(Box<EventHistoryPage>),
    Transitions(Box<ConnectionHistoryPage>),
    Space(Box<SpaceView>),
}

/// The background half of the client.
///
/// Owns the Tokio runtime, the client, and the task draining the signal stream.
/// Dropping it asks the runtime to stop and waits for the thread, so a closed
/// window does not leave a sampler running against a supervisor nobody reads.
pub struct Runtime {
    updates: UnboundedReceiver<Update>,
    actions: UnboundedSender<Action>,
    /// Wakes the UI thread when an update lands, so the interface is not
    /// obliged to poll at a fixed rate to feel responsive.
    _worker: std::thread::JoinHandle<()>,
}

impl Runtime {
    /// Start the background half.
    ///
    /// `wake` is called whenever an update is queued. The Flutter pump uses
    /// it to emit a fresh `ClientView`; in a test it can be a no-op, which is
    /// the reason it is a parameter rather than a captured context.
    pub fn start(config: Config, wake: impl Fn() + Send + Sync + 'static) -> ClientResult<Self> {
        let (updates_out, updates) = unbounded_channel();
        let (actions, actions_in) = unbounded_channel();
        let worker = std::thread::Builder::new()
            .name("astrolabe-client".into())
            .spawn(move || {
                let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        send(
                            &updates_out,
                            wake.as_ref(),
                            Update::Failed {
                                key: None,
                                what: "start the client runtime".into(),
                                error: ClientError::internal(error.to_string()),
                            },
                        );
                        return;
                    }
                };
                runtime.block_on(serve(config, updates_out, wake, actions_in));
            })
            .map_err(|error| {
                // A machine that cannot spawn a thread at startup cannot run
                // this program, and there is no channel to report it through
                // yet — the reporting channel is what the thread would own. So
                // it goes back to the caller, which still has a way to say so.
                ClientError::internal(format!("start the client thread: {error}"))
            })?;

        Ok(Self {
            updates,
            actions,
            _worker: worker,
        })
    }

    /// Ask for something. Returns immediately; the outcome arrives as an update.
    ///
    /// A send that finds nobody listening means the background half has already
    /// stopped, which happens exactly once, on the way out. There is nothing to
    /// report it to and nothing useful to do about it.
    pub fn dispatch(&self, action: Action) {
        let _ = self.actions.send(action);
    }

    /// Apply everything that has arrived since the last frame.
    ///
    /// Drains rather than taking one: a frame that applied a single update
    /// would fall behind a busy stream and draw state that was already several
    /// signals stale, which is the opposite of what the freshness rules are for.
    pub fn drain_into(&mut self, app: &mut App) {
        loop {
            match self.updates.try_recv() {
                Ok(update) => app.apply(update),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }
}

/// The background half's own handle on everything an action needs.
struct Worker {
    client: Client,
    updates: UnboundedSender<Update>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// What this machine's screen is currently told to show.
    ///
    /// Held here rather than read back from a surface: a refresh must ask for
    /// the same thing the person chose, and a selection that round-tripped
    /// through the interface is a second copy that can disagree with the first
    /// exactly when a render is failing.
    presenting: std::sync::Mutex<Option<crate::model::PresentationSelection>>,
    /// Where the screen preference lives, so a machine that was a screen comes
    /// back as one.
    state_root: std::path::PathBuf,
    /// This identity's correspondence backend — a real hosted-Post plane under
    /// `LAIT_POST_URL`, else the opt-in front-end fixture under
    /// `LAIT_CORRESPONDENCE_DEMO`. When neither is set, correspondence is not
    /// connected to any carrier and every correspondence action refuses
    /// honestly. Behind a `Mutex` because the `Worker` is shared across the
    /// tasks that answer actions.
    correspondence: Option<std::sync::Mutex<crate::client::correspondence::Correspondent>>,
}

/// Unix seconds, for the clocks the correspondence crate takes as arguments.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Choose the correspondence backend: a real hosted-Post plane when one is
/// named, else the fixture when opted in, else none (which refuses honestly).
/// Post wins — real carriage beats a loopback one.
/// Public because the composition is the thing worth testing.
///
/// A test that stood its own plane up would prove the plane and miss the wiring
/// — and the two times this client's seams have been wrong, every component was
/// correct and the composition was not. `identity_home` resolving the wrong
/// directory is exactly that class of defect, and it is invisible to anything
/// that does not come through here.
pub fn build_correspondent(
    identity: Option<&std::path::Path>,
    post_url: Option<String>,
    demo: bool,
) -> Option<std::sync::Mutex<crate::client::correspondence::Correspondent>> {
    use crate::client::correspondence::{Correspondent, DemoCarrier, PostBackend};
    if let Some(base) = post_url {
        // The identity's own seeds, so the address survives a restart. An
        // identity with no key is not one this client may mint: leave
        // correspondence unconnected, which refuses in words.
        let Ok(home) = identity_home(identity) else {
            return None;
        };
        let Ok(seeds) = lait::config::load_or_create_kinship_seeds(&home) else {
            return None;
        };
        // A corrupt reach file is not "no correspondents": re-founding over it
        // would change the address a person has already handed out.
        let Ok(held) = addressbook::ReachStore::at(&home).load() else {
            return None;
        };
        return match PostBackend::restore_at(seeds, held, base, now_secs(), Some(home)) {
            Ok(backend) => Some(std::sync::Mutex::new(Correspondent::Post(backend))),
            Err(_) => None,
        };
    }
    demo.then(|| std::sync::Mutex::new(Correspondent::Demo(DemoCarrier::new(now_secs()))))
}

/// Where this identity's durable state lives. Public for the same reason as
/// [`build_correspondent`]: a test that computes this itself is not testing it.
pub fn identity_home(identity: Option<&std::path::Path>) -> Result<std::path::PathBuf, ()> {
    match identity {
        Some(home) => lait::config::Selection::for_identity(home),
        None => lait::config::Selection::default(),
    }
    .identity_dir()
    .map_err(|_| ())
}

async fn serve(
    config: Config,
    updates: UnboundedSender<Update>,
    wake: Arc<dyn Fn() + Send + Sync>,
    mut actions: UnboundedReceiver<Action>,
) {
    // Read before the supervisor starts, because the config is moved into it.
    let state_root = config.state_root.clone();
    let correspondence_demo = config.correspondence_demo;
    let post_url = config.post_url.clone();
    let identity = config.identity.clone();
    let remembered = crate::screen::load(&state_root);
    let (client, signals) = match Client::start(config).await {
        Ok(started) => started,
        Err(error) => {
            send(
                &updates,
                wake.as_ref(),
                Update::Failed {
                    key: None,
                    what: "start the supervisor".into(),
                    error,
                },
            );
            // Actions still have to be answered, or every control a person
            // touches from here on would sit in flight forever with no
            // explanation. Drained and refused, one message each.
            while let Some(action) = actions.recv().await {
                send(
                    &updates,
                    wake.as_ref(),
                    Update::Failed {
                        key: Some(action.key()),
                        what: action.what(),
                        error: ClientError::unreachable(
                            "the client's supervisor did not start, so nothing can be reached",
                        ),
                    },
                );
            }
            return;
        }
    };

    let worker = Arc::new(Worker {
        client,
        updates,
        wake,
        presenting: std::sync::Mutex::new(
            remembered
                .as_ref()
                .filter(|held| held.presenting)
                .and_then(|held| held.selection.clone())
                .map(Into::into),
        ),
        state_root,
        correspondence: build_correspondent(identity.as_deref(), post_url, correspondence_demo),
    });

    // A machine that was a screen comes back as one, before the first read.
    // Ordering matters: coming up as the Library and *then* switching would
    // show a person the client for a frame, which reads as a client that
    // forgot and changed its mind.
    if remembered.is_some_and(|held| held.presenting) {
        worker.restore_presentation().await;
    }

    // The first read happens *after* the stream exists, which `Client::start`
    // guarantees by handing both back together. Reading first would open a
    // window in which events vanish between the snapshot and the subscription.
    worker.refresh().await;
    drain(worker, signals, actions).await;
}

/// How often the host plane is re-read for pending sponsorship asks.
///
/// The workbench sampler already ticks at this cadence for devices; asks are
/// host-plane state and would otherwise wait for F5 or an action.
const HOST_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Consume the stream forever, re-reading whenever it says to, and carry out
/// whatever a surface asks for while it does.
async fn drain(worker: Arc<Worker>, mut signals: Signals, mut actions: UnboundedReceiver<Action>) {
    let mut ticks = tokio::time::interval(HOST_SAMPLE_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The first tick is immediate; refresh() already read the host plane.
    ticks.tick().await;
    loop {
        tokio::select! {
            signal = signals.recv() => {
                let Some(signal) = signal else { return };
                let rebaseline = matches!(signal, ClientSignal::SnapshotRequired(_));
                worker.send(Update::Signal(signal));
                // A re-read on *every* event would make a busy log a request
                // storm. Only a snapshot-required says the model cannot be
                // derived from what it has seen, and only that is worth a round
                // trip.
                if rebaseline {
                    let worker = Arc::clone(&worker);
                    tokio::spawn(async move { worker.refresh().await });
                }
            }
            action = actions.recv() => {
                let Some(action) = action else { return };
                let worker = Arc::clone(&worker);
                // Its own task. Starting a head takes seconds, and an action
                // carried out on this loop would hold the signal drain for all
                // of them — long enough to lag the consumer and produce a
                // snapshot-required that nothing but the click caused.
                tokio::spawn(async move { worker.carry_out(action).await });
            }
            _ = ticks.tick() => {
                let worker = Arc::clone(&worker);
                tokio::spawn(async move { worker.sample_host().await });
            }
        }
    }
}

impl Worker {
    fn send(&self, update: Update) {
        send(&self.updates, self.wake.as_ref(), update);
    }

    /// Mutate the correspondence fixture under its lock and push the fresh
    /// snapshot. One place holds the lock, snapshots, and emits, so every
    /// correspondence handler is one line and none forgets to push the view.
    ///
    /// With the fixture absent — the default, no `LAIT_CORRESPONDENCE_DEMO` —
    /// correspondence is connected to no carrier, so this refuses honestly
    /// rather than pretending an action landed.
    fn correspond(
        &self,
        act: impl FnOnce(&mut crate::client::correspondence::Correspondent) -> ClientResult<()>,
    ) -> ClientResult<()> {
        let Some(fixture) = self.correspondence.as_ref() else {
            return Err(ClientError::refused("correspondence is not connected yet"));
        };
        let snapshot = {
            let mut correspondence = fixture
                .lock()
                .map_err(|_| ClientError::internal("the correspondence lock is poisoned"))?;
            act(&mut correspondence)?;
            correspondence.snapshot()
        };
        self.send(Update::Correspondence(snapshot));
        Ok(())
    }

    /// Re-read everything that is not delivered by the stream.
    async fn refresh(&self) {
        // When the fixture is present, the chat shows its world as it stands,
        // before anything that awaits: it depends on neither the supervisor nor
        // a daemon, so the conversations are there the instant the window opens
        // even while the identity is still attaching. Absent, there is nothing
        // to show and no carrier to ask.
        if let Some(fixture) = self.correspondence.as_ref() {
            if let Ok(correspondence) = fixture.lock() {
                self.send(Update::Correspondence(correspondence.snapshot()));
            }
        }
        self.send(Update::Snapshot(Box::new(
            self.client.supervisor().snapshot().await,
        )));
        self.send(Update::Heads(self.client.heads()));
        // The Library is compiled in — the install list — so reading it can
        // neither fail nor go stale against a daemon.
        self.send(Update::Library(self.client.get_library()));
        match self.client.get_storage().await {
            Ok(facts) => self.send(Update::Storage(facts)),
            Err(error) => self.fail(None, "read storage", error),
        }
        match self.client.host_context().await {
            Ok(context) => {
                // Presence is asked of the Orbits the context just listed —
                // passively, so this read places nothing. A pass that could
                // not read the context measures nothing, and the model keeps
                // its last measurement under the staleness it already wears.
                let presence = self.client.presence(&context.orbits).await;
                self.send(Update::Context(Box::new(context)));
                self.send(Update::Presence(presence));
            }
            Err(error) => self.fail(None, "read host context", error),
        }
        match self.client.display_status().await {
            Ok(display) => self.send(Update::Display(Box::new(display))),
            Err(error) => self.fail(None, "read display coordinator", error),
        }
        match self.client.book_list().await {
            Ok(book) => self.send(Update::Book(book)),
            Err(error) => self.fail(None, "read the address book", error),
        }
    }

    fn fail(&self, key: Option<String>, what: &str, error: ClientError) {
        self.send(Update::Failed {
            key,
            what: what.to_owned(),
            error,
        });
    }

    /// Re-read orientation so a pending sponsorship ask reaches the model
    /// without waiting for F5. A missed sample keeps the last context —
    /// flooding a failure every second is not a sampling failure, it is noise.
    async fn sample_host(&self) {
        // What each World's channel last said. Sampled here rather than only
        // on a re-read because CLIENT-66 asks for the staged state to be
        // visible *the moment staging completes* — a fact that only arrived on
        // F5 would be one a person had to know to go looking for, which is the
        // opposite of an evergreen client.
        //
        // Two small file reads beside two control round trips already on this
        // tick. It is a disk read rather than a subscription for the reason
        // the standing is a file at all: the daemon writes it whether or not a
        // client is running, so there is no event to have missed.
        self.send(Update::WorldStandings(
            crate::client::library::world_standings(self.client.identity_dir().as_deref()),
        ));
        if let Ok(context) = self.client.host_context().await {
            self.send(Update::Context(Box::new(context)));
        }
        if let Ok(display) = self.client.display_status().await {
            self.send(Update::Display(Box::new(display)));
        }
    }

    fn set_presenting(&self, selection: Option<crate::model::PresentationSelection>) {
        if let Ok(mut held) = self.presenting.lock() {
            held.clone_from(&selection);
        }
        // A preference is not worth failing an action over: the person asked
        // to present, and remembering it is the lesser half of that.
        if let Err(error) = crate::screen::save(&self.state_root, selection.as_ref()) {
            tracing::warn!(%error, "could not record this machine as a screen");
        }
    }

    /// Come back as the screen this machine was.
    ///
    /// The mode is restored first and the render follows, so a person whose
    /// selection no longer resolves lands on a screen saying why rather than on
    /// a Library that silently dropped their choice.
    async fn restore_presentation(&self) {
        let held = self.presenting.lock().ok().and_then(|held| held.clone());
        match held {
            Some(selection) => self.render_presentation(selection).await,
            None => self.send(Update::Presentation(Box::new(crate::model::Presentation {
                selection: None,
                rendered: None,
                failure: None,
            }))),
        }
    }

    /// Ask the daemon for one render and publish whatever came back.
    ///
    /// A failure is published rather than raised, and *beside* whatever was
    /// last drawn. A screen that has been showing a program for an hour should
    /// not go dark because one re-ask timed out — but it must not pretend the
    /// re-ask succeeded either, which is why both halves travel together.
    async fn render_presentation(&self, selection: crate::model::PresentationSelection) {
        let rendered = self.client.display_present(&selection).await;
        let (view, failure) = match rendered {
            Ok(view) => (Some(view), None),
            Err(error) => (None, Some(format!("{error}"))),
        };
        self.send(Update::Presentation(Box::new(crate::model::Presentation {
            selection: Some(selection),
            rendered: view,
            failure,
        })));
    }

    async fn carry_out(&self, action: Action) {
        let key = action.key();
        let what = action.what();
        match self.perform(&action).await {
            Ok(outcome) => {
                self.send(Update::Done { key, outcome });
                // Every action above changes something a read would report, so
                // the re-read is here rather than in each arm. It is what makes
                // the model move by snapshot rather than by the surface writing
                // down what it expected.
                if action.rereads() {
                    self.refresh().await;
                }
            }
            Err(error) => self.fail(Some(key), &what, error),
        }
    }

    async fn perform(&self, action: &Action) -> ClientResult<Outcome> {
        let client = &self.client;
        match action {
            // The re-read itself is the effect, and `rereads` is what
            // carries it out. Nothing to say beyond that it happened.
            Action::Refresh => Ok(Outcome::Silent),
            // Consent is one bounded durable daemon write. Fetch, staging and
            // per-Space migration happen later on the daemon's admitted lane;
            // the UI gets its operation coordinate before any of that work.
            Action::UpdateWorld { world } => {
                let job = client.world_update(world).await?;
                Ok(Outcome::Said(format!(
                    "update accepted ({})",
                    job.operation_hex()
                )))
            }
            Action::OpenWorld { world, entry_path } => {
                let launch = client.open_world(world, entry_path).await?;
                // The browser is the person's, and this is the only place in
                // the client that starts something it does not own. It happens
                // last: a launch URL composed and never opened is recoverable,
                // and a browser opened at a ticket that was never minted is not.
                crate::browser::open(&launch.url)?;
                Ok(Outcome::Launched(launch))
            }
            Action::StartDevice(id) => {
                client.supervisor().start_device(id).await?;
                Ok(Outcome::Said(format!("{id} is starting")))
            }
            Action::StopDevice(id) => {
                client.supervisor().stop_device(id).await?;
                Ok(Outcome::Said(format!("{id} stopped")))
            }
            Action::RestartDevice(id) => {
                client.supervisor().restart_device(id).await?;
                Ok(Outcome::Said(format!("{id} restarted")))
            }
            Action::ForceStopDevice(id) => {
                client.supervisor().force_stop_device(id).await?;
                Ok(Outcome::Said(format!("{id} was force-stopped")))
            }
            Action::RemoveDevice { id, delete_data } => {
                client
                    .supervisor()
                    .remove_device(
                        id,
                        RemoveDeviceRequest {
                            delete_data: *delete_data,
                            // Naming the device *is* the confirmation, and the
                            // surface has already asked for it. Passing it here
                            // rather than letting the supervisor infer it keeps
                            // the deletion refusable at its own layer.
                            confirm: delete_data.then(|| id.clone()),
                        },
                    )
                    .await?;
                Ok(Outcome::Said(if *delete_data {
                    format!("{id} was removed and its data deleted")
                } else {
                    format!("{id} was removed; its data is still on disk")
                }))
            }
            Action::CreateDevice { id, label } => {
                client
                    .supervisor()
                    .create_device(id.clone(), label.clone())
                    .await?;
                Ok(Outcome::Said(format!("added {id}")))
            }
            Action::RenameDevice { id, label } => {
                client
                    .supervisor()
                    .update_device(
                        id,
                        UpdateDeviceRequest {
                            label: Some(label.clone()),
                        },
                    )
                    .await?;
                Ok(Outcome::Said(format!("{id} is now called '{label}'")))
            }
            Action::StopAllOwned => {
                // Stops what this client spawned and leaves everything else
                // running. The same boundary the exit policy draws, reachable
                // without leaving.
                client.supervisor().stop_all_owned().await;
                Ok(Outcome::Said(
                    "stopped every device this client owns".into(),
                ))
            }
            Action::ReadLogs { device, cursor } => {
                let page = client
                    .supervisor()
                    .logs(device, *cursor, None)
                    .await
                    .map_err(ClientError::from)?;
                Ok(Outcome::Read(Read::Logs(Box::new(page))))
            }
            Action::ReadEvents { after } => {
                let page = client
                    .supervisor()
                    .event_history(&HistoryQuery {
                        after_revision: *after,
                        ..HistoryQuery::default()
                    })
                    .map_err(ClientError::from)?;
                Ok(Outcome::Read(Read::Events(Box::new(page))))
            }
            Action::ReadTransitions { after } => {
                let page = client
                    .supervisor()
                    .connection_history(&HistoryQuery {
                        after_revision: *after,
                        ..HistoryQuery::default()
                    })
                    .map_err(ClientError::from)?;
                Ok(Outcome::Read(Read::Transitions(Box::new(page))))
            }
            Action::ReadSpace(at) => {
                let view = client.space_view(at).await?;
                Ok(Outcome::Read(Read::Space(Box::new(view))))
            }
            Action::Administer { at, operation } => {
                // The reply is carried rather than swallowed: an invite link, a
                // device-enrolment token and a consent blob *are* their reply,
                // and a verb that answered "ok" would have produced the thing a
                // person came for and thrown it away.
                let said = client.space_do(at, (**operation).clone()).await?;
                Ok(Outcome::Said(said))
            }
            Action::StartHead => {
                // "Bring a head up", not "open that World" — so the mount is
                // resolved here rather than left unspecified. This is the layer
                // that knows which build this is, and the supervisor's key
                // cannot be built without an answer: an `Option` here is how one
                // World ended up with two heads.
                let head = client.head(lait::composition::PRODUCT_WORLD_MOUNT).await?;
                Ok(Outcome::Said(format!("a head is serving at {}", head.base)))
            }
            Action::StopHead(id) => {
                // Say which success it was. "stopped" for a head that had already
                // crashed is a true statement about the current state and a false
                // one about what just happened, and the difference is the only way
                // a person learns their World fell over by itself.
                match client.stop_head(id).await? {
                    crate::client::heads::Stopped::Stopped => {
                        Ok(Outcome::Said(format!("head {id} stopped")))
                    }
                    // Named, because a forced stop means the head did not run its
                    // ordered shutdown: a browser saw a reset and an in-flight
                    // transfer was cut. Reporting it as an ordinary stop would hide
                    // the only evidence that anything was lost.
                    crate::client::heads::Stopped::Forced => Ok(Outcome::Said(format!(
                        "head {id} did not stop when asked and was forced"
                    ))),
                    crate::client::heads::Stopped::WasAlreadyGone { status } => Ok(Outcome::Said(
                        format!("head {id} had already exited ({status})"),
                    )),
                }
            }
            Action::SendMessage { to, body } => {
                // The loopback carrier holds the real seam: compose, sign, seal,
                // and deposit under an unforgeable egress witness. When the
                // daemon hands the client a Space-hosted carrier this same call
                // points at it — the surface never learns which is underneath.
                let now = now_secs();
                self.correspond(|correspondence| correspondence.send(to, body, now))?;
                Ok(Outcome::Silent)
            }
            // Publishing and learning both change durable reach state, so they go
            // through `correspond` like every other correspondence act — which
            // is also what persists them.
            Action::ShareReach => {
                // The card itself reaches the surface as `CorrespondenceFacts.
                // my_reach`; the notice says what happened. Putting a kilobyte of
                // base32 through a one-line status channel would be a second
                // copy of a value the view already holds, in the place least
                // able to show it.
                self.correspond(|correspondence| correspondence.announce().map(|_| ()))?;
                Ok(Outcome::Said("ready to hand over".into()))
            }
            Action::AddCorrespondent { announcement } => {
                let mut learned = String::new();
                self.correspond(|correspondence| {
                    learned = correspondence.learn(announcement)?;
                    Ok(())
                })?;
                Ok(Outcome::Said(format!("added {learned}")))
            }
            Action::CollectMail => {
                let now = now_secs();
                // A carrier that could not be asked answers here as a retryable
                // failure rather than as silence. Silence is what an empty
                // mailbox looks like, and the two are not the same fact.
                self.correspond(|correspondence| correspondence.collect(now))?;
                Ok(Outcome::Silent)
            }
            Action::BlockSender(person) => {
                let now = now_secs();
                self.correspond(|correspondence| correspondence.block(person, now))?;
                Ok(Outcome::Said(format!("blocked {person}")))
            }
            // The four below say what they did only after the backend has said
            // it did it. `correspond` propagates a refusal, so a backend that
            // cannot carry the operation stops the notice with it — the shape
            // `BlockSender` already had, and the reason it was the only one of
            // the five that never claimed an act it had not performed.
            Action::AcceptContact(person) => {
                self.correspond(|correspondence| correspondence.accept(person))?;
                Ok(Outcome::Said(format!("added {person} to contacts")))
            }
            // Opening, focusing and closing tabs are shared-model navigation:
            // a click in one window moves the tab the chat window then draws.
            Action::OpenConversation(person) => {
                self.correspond(|correspondence| correspondence.open(person))?;
                Ok(Outcome::Silent)
            }
            Action::FocusConversation(person) => {
                self.correspond(|correspondence| correspondence.focus(person))?;
                Ok(Outcome::Silent)
            }
            Action::CloseConversation(person) => {
                self.correspond(|correspondence| correspondence.close(person))?;
                Ok(Outcome::Silent)
            }
            Action::SpaceFound { home, name, nick } => {
                client.space_found(home, name, nick.clone()).await?;
                Ok(Outcome::Said(format!("founded '{name}' in {home}")))
            }
            Action::SpaceEnter { link, home, nick } => {
                client.space_enter(link, home, nick.clone()).await?;
                Ok(Outcome::Said(format!("entered a Space into {home}")))
            }
            Action::DeviceConsent { token } => {
                client.device_consent(token).await?;
                Ok(Outcome::Said(
                    "this machine's consent is signed; hand it back to the device that invited it"
                        .into(),
                ))
            }
            Action::OrbitForget { space } => {
                client.orbit_forget(space).await?;
                Ok(Outcome::Said(format!(
                    "{space} is no longer registered here; its store is untouched"
                )))
            }
            Action::OrbitRebuild { orbit } => {
                client.orbit_rebuild(orbit).await?;
                Ok(Outcome::Said(format!("rebuilt {orbit}")))
            }
            Action::BookPut { card, name, note } => {
                let _ = client
                    .book_put(card.clone(), name.clone(), note.clone())
                    .await?;
                Ok(Outcome::Said(format!("saved '{name}'")))
            }
            Action::BookDelete { card } => {
                let _ = client.book_delete(card.clone()).await?;
                Ok(Outcome::Said(format!("deleted {card}")))
            }
            Action::BookSetPicture { card, path } => {
                let _ = client.book_set_picture(card.clone(), path.clone()).await?;
                Ok(Outcome::Said(match path {
                    Some(_) => format!("set the picture on {card}"),
                    None => format!("cleared the picture on {card}"),
                }))
            }
            Action::BookMerge { from, into } => {
                let _ = client.book_merge(from.clone(), into.clone()).await?;
                Ok(Outcome::Said(format!("merged {from} into {into}")))
            }
            Action::BookClaimSelf { card } => {
                let _ = client.book_claim_self(card.clone()).await?;
                Ok(Outcome::Said(format!("{card} is My Card")))
            }
            Action::BookLink { card, handle } => {
                let _ = client.book_link(card.clone(), handle.clone()).await?;
                Ok(Outcome::Said(format!("linked {handle}")))
            }
            Action::BookUnlink { card, handle } => {
                let _ = client.book_unlink(card.clone(), handle.clone()).await?;
                Ok(Outcome::Said(format!("unlinked {handle}")))
            }
            Action::BookExport { path, cards } => {
                let _ = client.book_export(path.clone(), cards.clone()).await?;
                Ok(Outcome::Said(format!("exported cards to {path}")))
            }
            Action::BookPropose { path } => {
                let _ = client.book_propose(path.clone()).await?;
                Ok(Outcome::Said(format!("staged suggestions from {path}")))
            }
            Action::BookAccept { suggestion } => {
                let _ = client.book_accept(suggestion.clone()).await?;
                Ok(Outcome::Said("accepted the suggested card".to_owned()))
            }
            Action::BookDismiss { suggestion } => {
                let _ = client.book_dismiss(suggestion.clone()).await?;
                Ok(Outcome::Said("dismissed the suggestion".to_owned()))
            }
            Action::InstallMcp { binding, preview } => {
                let outcome = client.install_mcp_head(binding, *preview).await?;
                Ok(Outcome::Mcp(Box::new(outcome)))
            }
            Action::DisplayPairingApprove { pairing, label } => {
                client
                    .display_pairing_approve(pairing.clone(), label.clone())
                    .await?;
                Ok(Outcome::Said(format!("approved the display '{label}'")))
            }
            Action::DisplayPairingReject(pairing) => {
                client.display_pairing_reject(pairing.clone()).await?;
                Ok(Outcome::Said("rejected the display pairing".into()))
            }
            Action::DisplayAssignmentPut(assignment) => {
                client
                    .display_assignment_put((**assignment).clone())
                    .await?;
                Ok(Outcome::Said(format!(
                    "assigned display {}",
                    assignment.device
                )))
            }
            Action::DisplayAssignmentRevoke(assignment) => {
                client.display_assignment_revoke(assignment.clone()).await?;
                Ok(Outcome::Said(format!(
                    "revoked display assignment {assignment}"
                )))
            }
            Action::DisplayDeviceRevoke(device) => {
                client.display_device_revoke(device.clone()).await?;
                Ok(Outcome::Said(format!("revoked display device {device}")))
            }
            Action::DisplayIdentifierAdmitPassphrase(passphrase) => {
                client
                    .display_identifier_admit_passphrase(passphrase.clone())
                    .await?;
                Ok(Outcome::Said(
                    "the identifier key now opens with a passphrase too".into(),
                ))
            }
            Action::EnterPresentation => {
                self.set_presenting(None);
                // A screen with nothing on it yet, published so the surface
                // has something to draw the moment the control is pressed.
                self.send(Update::Presentation(Box::new(crate::model::Presentation {
                    selection: None,
                    rendered: None,
                    failure: None,
                })));
                Ok(Outcome::Silent)
            }
            Action::PresentHere(selection) => {
                self.set_presenting(Some((**selection).clone()));
                self.render_presentation((**selection).clone()).await;
                Ok(Outcome::Silent)
            }
            Action::PresentRefresh => {
                let held = self.presenting.lock().ok().and_then(|held| held.clone());
                // Refusing rather than guessing: a refresh with nothing
                // selected is a surface asking for a screen that was already
                // left, and drawing *something* would be worse than nothing.
                if let Some(selection) = held {
                    self.render_presentation(selection).await;
                }
                Ok(Outcome::Silent)
            }
            Action::LeavePresentation => {
                if let Ok(mut held) = self.presenting.lock() {
                    *held = None;
                }
                // Recorded rather than forgotten: leaving is a decision, and a
                // cleared file says so where a deleted one would be
                // indistinguishable from never having entered.
                if let Err(error) = crate::screen::clear(&self.state_root) {
                    tracing::warn!(%error, "could not record leaving Big Picture");
                }
                self.send(Update::PresentationEnded);
                Ok(Outcome::Silent)
            }
            Action::Exit(request) => {
                let report = crate::lifecycle::exit(client.supervisor(), *request).await;
                Ok(Outcome::Exited(Box::new(report)))
            }
        }
    }
}

impl Action {
    /// Whether carrying this out changed something a read would report.
    ///
    /// A preview writes nothing and an exit is on the way out; re-reading after
    /// either would be a round trip for a model nobody is going to draw again.
    const fn rereads(&self) -> bool {
        !matches!(
            self,
            Self::InstallMcp { preview: true, .. }
                | Self::Exit(_)
                | Self::ReadLogs { .. }
                | Self::ReadEvents { .. }
                | Self::ReadTransitions { .. }
                | Self::ReadSpace(_)
                // Presenting reads a World and writes nothing, and it already
                // publishes its own update. A full re-read behind every frame
                // boundary would put a snapshot of the whole machine on the
                // path of a screen refresh.
                | Self::PresentHere(_)
                | Self::PresentRefresh
                | Self::LeavePresentation
        )
    }
}

/// A send whose receiver is gone means the window closed. Not an error, and
/// nothing to report to — the only correct response is to stop trying.
fn send(sender: &UnboundedSender<Update>, wake: &(impl Fn() + ?Sized), update: Update) {
    if sender.send(update).is_ok() {
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lait_workbench::{
        BackendEvent, Capabilities, EnvironmentSnapshot, EventKind, SnapshotReason,
    };

    fn snapshot() -> WorkbenchSnapshot {
        WorkbenchSnapshot {
            schema_version: 1,
            revision: 1,
            environment: EnvironmentSnapshot {
                state_root: "root".into(),
                executable: "lait".into(),
                server_pid: 1,
            },
            capabilities: Capabilities::default(),
            devices: Vec::new(),
            connections: Vec::new(),
        }
    }

    /// A runtime whose background half is not running, so a test can put
    /// updates in by hand and drain them the way a frame does.
    fn detached() -> (UnboundedSender<Update>, Runtime) {
        let (updates_out, updates) = unbounded_channel();
        let (actions, _actions_in) = unbounded_channel();
        (
            updates_out,
            Runtime {
                updates,
                actions,
                _worker: std::thread::spawn(|| {}),
            },
        )
    }

    /// A frame applies everything waiting, not one thing. A drain that took a
    /// single update would draw state that was already several signals old on a
    /// busy stream — precisely when being current matters most.
    #[test]
    fn a_frame_applies_every_update_that_is_waiting() {
        let (sender, mut runtime) = detached();

        sender.send(Update::Snapshot(Box::new(snapshot()))).ok();
        for index in 0..5 {
            sender
                .send(Update::Signal(ClientSignal::Event(BackendEvent {
                    revision: index + 2,
                    at_ms: 0,
                    kind: EventKind::LogChanged,
                    device_id: None,
                    message: "log grew".into(),
                })))
                .ok();
        }
        sender
            .send(Update::Signal(ClientSignal::SnapshotRequired(
                SnapshotReason::Reloaded,
            )))
            .ok();

        let mut app = App::new();
        runtime.drain_into(&mut app);

        assert_eq!(app.consumed(), 6, "the frame stopped short of the queue");
        assert!(
            !app.is_loading(),
            "the snapshot in the queue was not applied"
        );
        assert!(
            app.stale().is_some(),
            "the last signal said to re-baseline and the model did not record it"
        );
    }

    /// Failures reach the model as state rather than being logged and lost.
    #[test]
    fn a_failure_becomes_something_the_surface_can_draw() {
        let (sender, mut runtime) = detached();
        sender
            .send(Update::Failed {
                key: None,
                what: "read the library".into(),
                error: ClientError::unreachable("no daemon"),
            })
            .ok();

        let mut app = App::new();
        runtime.drain_into(&mut app);
        let failure = app.failures().next().expect("a failure reached the model");
        assert_eq!(failure.what, "read the library");
        assert!(
            failure.error.retryable,
            "an unreachable daemon is retryable"
        );
    }

    /// A closed window drops the receiver. The background half must treat that
    /// as "stop", not as an error worth reporting to a channel nobody holds.
    #[test]
    fn sending_to_a_closed_window_is_not_an_error() {
        let (sender, updates) = unbounded_channel::<Update>();
        drop(updates);
        let woken = std::sync::atomic::AtomicBool::new(false);
        send(
            &sender,
            &|| woken.store(true, std::sync::atomic::Ordering::SeqCst),
            Update::Signal(ClientSignal::SnapshotRequired(SnapshotReason::Reloaded)),
        );
        assert!(
            !woken.load(std::sync::atomic::Ordering::SeqCst),
            "a send to a closed window still asked for a repaint"
        );
    }

    /// Two actions on two different devices are two different things in flight.
    /// A key that did not distinguish them would disable both rows' controls
    /// because one of them was busy.
    #[test]
    fn what_is_in_flight_is_keyed_to_the_control_that_caused_it() {
        assert_ne!(
            Action::StopDevice("alice".into()).key(),
            Action::StopDevice("bob".into()).key()
        );
        assert_ne!(
            Action::StopDevice("alice".into()).key(),
            Action::StartDevice("alice".into()).key()
        );
        assert_ne!(
            Action::OpenWorld {
                world: "issues".into(),
                entry_path: "/".into(),
            }
            .key(),
            Action::OpenWorld {
                world: "signage".into(),
                entry_path: "/".into(),
            }
            .key()
        );
    }

    /// A deletion says it is a deletion. The record of what happened is read
    /// after the fact, when the difference between "removed" and "removed and
    /// erased" is the only thing that matters.
    #[test]
    fn a_destructive_action_describes_itself_as_one() {
        let destructive = Action::RemoveDevice {
            id: "alice".into(),
            delete_data: true,
        };
        assert!(
            destructive.what().contains("delete"),
            "{}",
            destructive.what()
        );

        let ordinary = Action::RemoveDevice {
            id: "alice".into(),
            delete_data: false,
        };
        assert!(!ordinary.what().contains("delete"), "{}", ordinary.what());
    }

    /// Neither a preview nor an exit changes anything a read would report.
    /// Re-reading after them is a round trip for a model nobody draws again.
    #[test]
    fn only_the_actions_that_changed_something_ask_for_a_re_read() {
        assert!(Action::StopDevice("alice".into()).rereads());
        assert!(Action::BookPut {
            card: None,
            name: "Ada".into(),
            note: None,
        }
        .rereads());
        assert!(
            Action::Refresh.rereads(),
            "the one action that is nothing but a re-read does not re-read"
        );
        assert!(!Action::Exit(ExitRequest::StayOnline).rereads());
        assert!(!Action::InstallMcp {
            binding: Box::new(crate::client::heads::McpBinding {
                client: lait::install::Client::Generic,
                scope: None,
                name: "lait".into(),
                agent: None,
                no_agent: false,
                project: ".".into(),
                world: None,
            }),
            preview: true,
        }
        .rereads());
    }
}
