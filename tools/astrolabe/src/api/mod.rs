//! The boundary, and the only thing that crosses it.
//!
//! There is an interface over this Rust core, which means there is a boundary
//! — the one earlier revisions removed on purpose. Everything in this module
//! exists to make that boundary safe by construction rather than by care.
//!
//! The interface is **Tauri** (`apps/astrolabe-web`), and its host takes
//! [`ClientView`] apart by exhaustive destructuring, so a field added here
//! stops compiling there until somebody decides what the client does with it.
//! There is no generated binding any more: the Flutter interface and the
//! `flutter_rust_bridge` codegen it required are deprecated and unwired, which
//! is why nothing in this file is annotated for a generator and why
//! [`subscribe`] is the only way to watch the stream.
//!
//! ## What crosses, and in which direction
//!
//! Exactly two things. A [`ClientView`] goes out — a whole, immutable
//! projection of client state, built here and never assembled on the other
//! side. An [`ActionRequest`] comes back — a thing a person asked for. Nothing
//! else has a route across.
//!
//! ## Facts go out; words do not
//!
//! The view carries *what is true*, not what to say about it. A presence is
//! [`PresenceView::Offline`], never "not reachable"; a row that cannot be
//! opened carries an absent entry path, never the sentence explaining it. That
//! split is what keeps the lockdown rule honest: the interface owns wording
//! and layout, and cannot own a fact because it is never sent one it could
//! have derived differently.
//!
//! It is also why absence is modelled rather than encoded. `last_opened` is an
//! `Option<u64>` and not a `u64` where zero means never, because "never opened"
//! and "opened at the epoch" are different facts and a sentinel makes them the
//! same one. Unmeasured is absent, never zero — the rule survives the crossing
//! only if the wire format can say it.
//!
//! ## One model, still
//!
//! [`App`] stays the only model of client state. This module holds it, drains
//! the runtime into it, and projects it. The interface receives the projection
//! whole and keeps nothing of its own but drafts.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use crate::model::{App, StaleReason};
use crate::runtime::{Action, Runtime};
use crate::Config;

/// Everything a surface can draw, as of one moment.
///
/// Sent whole. There is no partial update and no patch protocol, because a
/// patch protocol is a second model in disguise: the receiver has to hold the
/// previous state and apply changes to it, and the moment it can do that it can
/// disagree with the sender.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientView {
    /// Nothing has been read yet. Distinct from "read, and there is nothing".
    pub loading: bool,
    /// Why what is on screen may not be current. `None` is fresh.
    pub stale: Option<Staleness>,
    /// `None` until the library has been read once — which is not the same as
    /// a device that serves nothing.
    pub library: Option<Vec<LibraryRow>>,
    pub host: Option<HostFacts>,
    /// The daemon-owned, self-hosted display coordinator. `None` until its
    /// first authoritative read lands.
    pub display: Option<DisplayFacts>,
    /// This machine as a screen. `Some` *is* Big Picture — whether or not it
    /// has drawn anything yet — so a surface reads presence here rather than
    /// keeping a mode flag of its own that could disagree.
    pub presentation: Option<PresentationFacts>,
    pub heads: Vec<HeadRow>,
    pub devices: Vec<DeviceRow>,
    pub storage: Vec<StorageRow>,
    /// This identity's Orbit registry, newest-opened first.
    pub orbits: Vec<OrbitRow>,
    /// The Space being administered, when one has been chosen. Choosing is an
    /// act — it costs a read — so this is absent until somebody chooses.
    pub space: Option<SpaceRow>,
    /// `None` until the book has been read once. Empty cards is a book
    /// that answered and holds nothing, not an unread book.
    pub book: Option<BookFacts>,
    /// This identity's correspondence — the mailbox and the arrival standing.
    /// `None` until read once, distinct from a mailbox that answered empty.
    pub correspondence: Option<CorrespondenceFacts>,
    /// This identity's profile: the devices that hold it, the code this one
    /// is showing if it is waiting to be added, and any device waiting on
    /// this person to confirm it. `None` until read once — which is not a
    /// profile with one device.
    pub profile: Option<ProfileFacts>,
    pub notices: Vec<NoticeRow>,
    pub failures: Vec<FailureRow>,
    /// The keys of actions asked for and not yet answered. A control whose key
    /// is in here is disabled — on the frame the click happened, not on
    /// whichever later frame the answer arrives.
    pub in_flight: Vec<String>,
    /// Last MCP binding this client authored or previewed. Absent until then.
    pub mcp: Option<McpBindingRow>,
    /// The staged image this client spawns from, when one was staged. Carries
    /// the roll-forward fact: the source was rebuilt after staging. `None`
    /// for a launch that never staged, which is not "current" — it is a
    /// launch with no image to be behind.
    pub image: Option<ImageRow>,
    /// The one thing a person is ever asked about this client's own updating
    /// (CLIENT-47): *when to restart*. `None` is the ordinary evergreen
    /// state — nothing to say — and covers a machine that has never
    /// completed a check, which is not "up to date".
    pub update: Option<UpdateRow>,
    /// An exit was carried out: the shell's cue to end the process. The
    /// report itself crossed as a notice.
    pub exited: bool,
}

/// What to put in front of a person about this client's own update.
///
/// A projection of `client::update::Intent`, flattened for the boundary. The
/// client is evergreen — nothing here asks whether to take an update, because
/// that is not a choice anyone is offered. Staging is silent and continuous;
/// applying happens at a moment no client is alive. This is only ever the
/// request to reach that moment, or news that is not an update at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateRow {
    /// A release is staged and waiting for a restart this machine will not
    /// take on its own.
    RestartRequested {
        /// The version that becomes live on restart.
        version: String,
        /// How hard to ask, by how long it has waited. Chrome's thresholds,
        /// counted from staging rather than from publication.
        urgency: UpdateUrgency,
    },
    /// Staged and ready, and something is holding the restart. Not "restart
    /// when you like" — this is why we have not, and it is being waited for.
    Waiting {
        version: String,
        /// What is holding it, in the words a surface should say.
        holding: Vec<String>,
    },
    /// Something happened that is not an update and is not silence: a
    /// signature that did not verify, a pointer that went backwards, or a
    /// release that requires a canonical reinstall.
    Attention { why: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpdateUrgency {
    Quiet,
    Insistent,
    Urgent,
}

/// The staged image this client spawns from — see `client::ImageStanding`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRow {
    /// Content hash of the staged bytes, as staged. Two processes reporting
    /// the same fingerprint run the same code, whatever their paths say.
    pub fingerprint: String,
    pub staged_at_ms: u64,
    /// The source was rebuilt after this image was staged: a roll-forward
    /// would change what runs.
    pub source_changed: bool,
}

/// What authoring an MCP binding produced. Bindings, not processes.
#[derive(Debug, Clone, PartialEq)]
pub struct McpBindingRow {
    pub path: String,
    pub detail: String,
    pub note: Option<String>,
    pub replaced: bool,
    pub agent: Option<String>,
    pub written: bool,
    /// Mount the binding was authored for. A surface showing another World
    /// must ignore this row.
    pub world: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Staleness {
    /// The first read has not landed.
    NeverLoaded,
    /// The stream said so, and said why.
    Signalled(String),
}

/// One row of the Library: an installed World.
///
/// Everything here is declared by the selected immutable release. No runner or
/// daemon probe produces it, so listing cannot make a World execute and cannot
/// go stale against a daemon. Which Spaces serve the World is the destination's
/// fact: the head's own front page carries the Space selector, and this row
/// deliberately does not pre-ask it.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryRow {
    /// Stable across re-reads: the mount. A selection keyed by index would
    /// silently follow whatever moved into that position.
    pub key: String,
    pub world_mount: String,
    /// The World's id — what a display assignment, an MCP binding or a
    /// capability names it by. The mount is what a person sees in a URL.
    pub world: String,
    /// Catalog presence is not installation. False draws Install and makes
    /// Open unavailable until a signed immutable release is selected.
    pub installed: bool,
    /// What the World calls itself. Always present — an installed package
    /// declares its name, so there is no unnamed row to draw.
    pub display_name: String,
    /// Where `Open` lands. `None` is a World that declared no entry path,
    /// which cannot be opened: `/` is not a guess to make on its behalf.
    pub opens_at: Option<String>,
    /// Reviewed implementation version for the hosted World.
    pub version: Option<u32>,
    /// One line saying what this World is for.
    pub tagline: Option<String>,
    /// Packed `0xRRGGBB`. A seed the interface derives a plate from locally;
    /// there is no asset here and nothing to fetch.
    pub accent: Option<u32>,
    /// People from the identity's book addressed in any Space this World is
    /// reachable in. `None` until the book has been read — which is not the
    /// same as a World nobody in the book is addressed near.
    pub people: Option<Vec<WorldPersonRow>>,
    /// What this machine last learned about the World's own channel. `None`
    /// when nothing has ever been checked — which is not "up to date", and
    /// draws exactly what this row drew before any of it existed.
    pub update: Option<WorldUpdateRow>,
    /// Live first-install progress. Separate from `update`: there is no serving
    /// release to update until this operation completes.
    pub install: Option<WorldInstallRow>,
    /// The channel this World follows by its own choice. `None` follows the
    /// node's — a different fact, and it must not draw as though the World
    /// had chosen what the node happens to be on.
    pub channel: Option<String>,
    /// The directory this World is read from, when it is a local one.
    ///
    /// `None` is a released World. A row carrying this is a tree somebody on
    /// this device is working on — unsealed, with an id and mount the host
    /// assigned it, and not the World it may have been copied from.
    pub source_dir: Option<String>,
    /// Whether a local World's tree still holds the bytes somebody agreed to.
    /// `None` for a released World, which is not the same question.
    pub source_standing: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldInstallRow {
    pub phase: String,
    pub received: Option<u64>,
    pub total: Option<u64>,
}

/// A World's channel, as this machine last found it.
///
/// Separate from the row's signed declaration because the two are different
/// kinds of fact: the list joins catalog and selected-installation state, while this is
/// measured and can. Keeping them apart is what stops the Library becoming a
/// surface that probes to draw itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldUpdateRow {
    /// The bundle version serving now. `None` means no valid release is selected.
    pub serving: Option<String>,
    /// The version the channel named when it was last asked.
    pub available: Option<String>,
    /// The channel holds a bundle this machine is not serving and this build
    /// can run. The only state that turns `Open` into `Update`.
    pub behind: bool,
    /// A newer bundle exists that this build cannot run, each unmet
    /// requirement named. Shown, never offered — pressing an update that
    /// would be refused on arrival teaches a person to distrust the control.
    pub unmet: Option<Vec<String>>,
    /// Durable native consent/progress, independent of channel standing.
    pub operation: Option<String>,
    pub phase: Option<String>,
    pub progress: Option<String>,
    pub message: Option<String>,
}

/// The artwork one selected World release ships, as bounded PNG bytes.
///
/// Not part of [`LibraryRow`], and the omission is the design: see
/// [`world_artwork`]. Both halves are optional and their absence is a real
/// answer — a World that ships neither is drawn from its accent, which is what
/// every World was drawn from before any of them shipped art.
///
/// No `Default` derive: the codegen turns one into a `default_()` on the Dart
/// class — an asynchronous round trip to the core to learn that two fields are
/// null, which `const WorldArtwork()` already says on the Dart side for free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldArtwork {
    pub mark: Option<Vec<u8>>,
    pub hero: Option<Vec<u8>>,
}

/// One person the book addresses near a World — the at-a-glance join between
/// the identity's own book and a Library row, across every Space the card
/// holds an address in. Not a roster: that is an authoritative read the
/// World's own head answers for, and this panel never places anything to
/// find out. My Card is excluded — the glance answers "who of mine is here",
/// and you are not a contact of yourself.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldPersonRow {
    pub name: String,
    /// The stored picture (`<mime>;base64,<data>`), or `None` for the
    /// default face — the same canonical face the book draws.
    pub picture: Option<String>,
    /// Best measured presence across the Spaces this card holds an address
    /// in, or `None` when none of them could be asked. A person is as
    /// present as their most reachable address.
    pub presence: Option<PresenceView>,
    /// Filed under the canonical agent group.
    pub agent: bool,
    /// Has this World open right now: a World-scoped Live row spoke for
    /// them when the Space was asked. The panel's nearest liveness — a
    /// launched World, not merely a reachable device.
    pub here: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostFacts {
    pub version: String,
    pub identity_home: String,
    pub spaces_root: String,
    pub orbit_count: u32,
}

/// Astrolabe's complete controller-facing projection of the local display
/// coordinator. Receiver proof keys and canonical assignment input remain on
/// the daemon side of the boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayFacts {
    pub instance: String,
    pub label: String,
    pub origin: String,
    pub certificate_sha256: String,
    pub certificate_pem: String,
    pub surfaces: Vec<DisplaySurfaceRow>,
    pub devices: Vec<DisplayReceiverRow>,
    pub assignments: Vec<DisplayAssignmentRow>,
    pub pending_pairings: Vec<DisplayPairingRow>,
    /// Codes minted for televisions to enter, not yet spent or expired.
    pub pending_rendezvous: Vec<DisplayRendezvousRow>,
    /// What each surface can show, per Orbit, as last listed. Absent until
    /// asked (`ActionRequest::DisplaySurfaceChoices`).
    pub choices: Vec<DisplayChoicesRow>,
    /// `None` from a daemon that predates the custody split — not reported, as
    /// distinct from reported-as-none.
    pub identifier_custody: Option<DisplayIdentifierCustodyRow>,
}

/// This machine as a screen. Present exactly when Big Picture is on.
///
/// Being a screen and showing something are separate facts, so `chosen` is
/// optional: a screen entered and not yet pointed at anything is a real state
/// with its own surface, not a half-built one.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentationFacts {
    pub chosen: Option<PresentationChoice>,
    /// The last verified render, kept across a failed re-ask so a screen goes
    /// stale rather than dark.
    pub program: Option<PresentedProgram>,
    /// Why the last attempt did not answer. Travels *beside* `program`, never
    /// instead of it — "stale, and here is why" and "nothing to show" are
    /// different things to tell somebody standing in front of a screen.
    pub failure: Option<String>,
}

/// What a screen was pointed at.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentationChoice {
    pub orbit: String,
    pub world: String,
    pub surface: String,
    /// What to call this on screen while it is loading or refusing.
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresentedProgram {
    /// `current`, `partial`, or `unavailable`.
    pub assessment: String,
    pub partial_reasons: Vec<String>,
    /// `hold_last`, `loop`, `poll_at_end`, or `blank_at_end`.
    pub cycle: String,
    pub refresh_after_ms: Option<u32>,
    pub items: Vec<PresentedItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresentedItem {
    pub id: String,
    pub duration_ms: Option<u32>,
    pub assessment: String,
    pub spoken_summary: Option<String>,
    pub scene: PresentedScene,
}

/// What one item draws.
///
/// `Unsupported` is a scene rather than an omission: a program that quietly
/// dropped what this screen cannot draw would be a shorter program nobody
/// authored.
#[derive(Debug, Clone, PartialEq)]
pub enum PresentedScene {
    Frame {
        /// `png`, `jpeg`, or `webp`.
        media_type: String,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    },
    Blank {
        /// `source_unavailable`, `unsupported`, or `program_ended`.
        reason: String,
    },
    Unsupported {
        output: String,
    },
}

/// How many ways back into this coordinator's identifier key exist, and whether
/// any survives the machine.
///
/// Carried on the ordinary status projection rather than a settings page: the
/// moment an operator wants this is after the machine is gone, and a fact only
/// reachable from the lost machine is not a fact they have.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayIdentifierCustodyRow {
    /// Kinds of unlock path, never material.
    pub slots: Vec<String>,
    pub portable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySurfaceRow {
    pub world: String,
    pub surface: String,
    pub title: String,
    pub contract_version: u32,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayReceiverRow {
    pub device: String,
    pub label: String,
    pub platform: String,
    pub build: String,
    pub issued_at_unix_ms: u64,
    pub revoked_at_unix_ms: Option<u64>,
    pub health: Option<DisplayHealthRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayHealthRow {
    pub revision: String,
    pub current_item: String,
    pub elapsed_ms: u32,
    pub connection: String,
    pub playback: String,
    pub last_error: String,
    pub staged_items: u16,
    pub staged_bytes: u32,
    pub drift_residual_ms: i32,
    pub correction_events: u32,
    pub pipeline_unobservable: bool,
    /// When the report arrived; `None` from a daemon that predates it.
    pub reported_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayAssignmentRow {
    pub assignment: String,
    pub device: String,
    pub orbit: String,
    pub space: String,
    pub program: String,
    pub world: String,
    pub surface: String,
    pub controller: String,
    pub theme: DisplayTheme,
    pub sync_group: Option<String>,
    pub sync_mode: Option<DisplaySyncMode>,
    pub static_delay_ms: i32,
    pub expires_at_unix_ms: Option<u64>,
    pub revoked_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayPairingRow {
    pub pairing: String,
    pub confirmation_phrase: Vec<String>,
    pub certificate_sha256: String,
    pub platform: String,
    pub build: String,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// A code minted for a television to enter.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRendezvousRow {
    pub rendezvous: String,
    /// Grouped for reading: `XXXX-XXXX`.
    pub code: String,
    /// The site the television resolves this coordinator by, when the
    /// identity publishes one.
    pub site: Option<String>,
    pub label: String,
    pub assignment: Option<DisplayRendezvousAssignmentRow>,
    pub state: DisplayRendezvousState,
    /// The receiver the code enrolled, once one has entered it.
    pub device: Option<String>,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// What one surface can show in one Orbit, as the World listed it.
///
/// `choices` is `None` when the list could not be taken and `unavailable`
/// says why; an empty list is a Space with nothing to show. Distinct on
/// purpose — a control offers a typed field for one and an empty picker for
/// the other.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayChoicesRow {
    pub orbit: String,
    pub world: String,
    pub surface: String,
    pub choices: Option<Vec<DisplayChoiceRow>>,
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayChoiceRow {
    /// What the surface's input takes: a screen id, a project key.
    pub id: String,
    pub title: String,
}

/// Where a code is in its life; a spent code is listed a while longer as
/// the receiver it became.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayRendezvousState {
    Waiting,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRendezvousAssignmentRow {
    pub orbit: String,
    pub world: String,
    pub surface: String,
}

/// What a code pins its television to once it connects: an assignment with
/// everything but the device, which does not exist until then.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRendezvousAssignment {
    pub orbit: String,
    pub world: String,
    pub surface: String,
    pub input_json: String,
    pub theme: DisplayTheme,
    pub stale_after_ms: u32,
    pub on_stale: DisplayStaleAction,
    pub sync_group: Option<String>,
    pub sync_mode: DisplaySyncMode,
    pub static_delay_ms: i32,
    pub expires_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayTheme {
    Light,
    Dark,
    HighContrast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStaleAction {
    KeepWithNativeBanner,
    Blank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySyncMode {
    StayInSync,
    Positional,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeadRow {
    pub id: String,
    /// Browser or MCP.
    pub kind: String,
    /// The Orbit it is bound to. `None` is a browser head serving every Orbit
    /// its identity has.
    pub orbit: Option<String>,
    /// The one World this head serves.
    ///
    /// `None` is a head from before the pin, which answers for every mounted
    /// World. It matches no row deliberately: a surface cannot say a definite
    /// thing about it, and saying an indefinite thing is the defect this field
    /// closes.
    pub world: Option<String>,
    /// The address, *without* the run credential its URL carries. A front page
    /// has no use for a credential — `Open` mints a single-use ticket of its
    /// own, which is what that ceremony is for.
    pub origin: Option<String>,
    pub owned: bool,
    /// What the supervisor can say about this head *now*.
    ///
    /// `running`, `exited` or `unknown`. Carried because without it a surface has
    /// only row *presence* to go on, and presence is not liveness: exited heads stay
    /// listed so a person can see the thing they opened died, so a surface counting
    /// rows paints a crashed head as Running. `HeadState` was added underneath and
    /// stopped here, one hop short of the only place the lie was visible.
    ///
    /// A string like `DeviceRow::state`, not the enum: this crosses a generated
    /// bridge, and a new variant should widen a match on the far side rather than
    /// break the binding.
    pub state: String,
    /// Why the state could not be established, when it could not.
    ///
    /// `Some` only for `unknown`, exactly as `DeviceRow::degraded` carries only a
    /// real degradation. A surface that can say *why* it cannot tell is the whole
    /// difference between a third state and a shrug.
    pub state_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRow {
    pub id: String,
    pub label: String,
    pub state: String,
    pub owned: bool,
    /// Whether observation of this device is degraded, and what went wrong.
    /// A sampling failure preserves the last good reading and says so; it is
    /// never drawn as "nothing there".
    pub degraded: Option<String>,
    pub home: String,
    /// `None` for a daemon this client did not spawn. Ownership is a boundary:
    /// there is no pid-based path to stopping something we do not own, and the
    /// absence of a pid here is that boundary crossing the bridge.
    pub pid: Option<u32>,
    /// Whether this client may force-stop it. Answered by the core's
    /// capabilities rather than inferred from ownership on this side, because
    /// the two can differ and only one of them is authoritative.
    pub can_force_stop: bool,
    pub last_error: Option<String>,
    /// Content hash of the image this device is actually running, when it was
    /// spawned from a staged copy. Reported rather than inferred: a staged
    /// run outlives the tree that produced it. Compare against
    /// `ClientView::image` to say "this node runs older code than the bench
    /// would start today".
    pub image_fingerprint: Option<String>,
}

/// What one Orbit is holding.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageRow {
    pub orbit: String,
    /// What the registry calls it. Advisory — a display name is owned by a
    /// World today — and carried as what it is rather than as truth.
    pub name: Option<String>,
    /// Every figure is optional, and that is the contract. A footprint nobody
    /// could measure is *absent*, never zero: an Orbit reported as holding 0
    /// bytes is a claim, and one nobody asked is not.
    pub bytes_on_disk: Option<u64>,
    pub object_count: Option<u64>,
    pub last_verified_ms: Option<u64>,
    /// Why there are no figures, when there are none. Two reasons, because
    /// "not up" and "could not be asked" are different facts.
    pub missing: Option<Missing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// It is not up. Not an error, and not something a listing corrects —
    /// measuring must not place what nobody asked to place.
    NotPlaced,
    /// It could not be asked.
    Unreachable,
}

/// One Orbit in this identity's registry.
#[derive(Debug, Clone, PartialEq)]
pub struct OrbitRow {
    pub space: String,
    pub name: String,
    pub path: String,
    /// `None` is never opened, for the same reason it is on a library row.
    pub last_opened: Option<u64>,
}

/// What a gate answered, when a diagnosis was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct GateRow {
    /// Stable machine id — `space`, `daemon`, `membership`, `peer`, `synced`.
    pub id: String,
    pub label: String,
    pub state: GateState,
    pub detail: String,
}

/// Five states, not two.
///
/// `Warn` is deliberately not blocking: a key-custody problem is urgent to fix
/// and irrelevant to whether somebody is onboarded, and a warning that hijacked
/// the blocker would tell a joiner they are stuck when they are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    Pass,
    Wait,
    Fail,
    Warn,
    Skip,
}

/// A diagnosis, when one was taken.
///
/// Absent when the Space could not be asked — which is *not* the same as every
/// gate passing, and is the whole reason this is an `Option` rather than an
/// empty list of gates.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosisRow {
    pub gates: Vec<GateRow>,
    /// The first non-passing gate: the one actionable blocker.
    pub blocked_on: Option<String>,
    pub summary: String,
}

/// The Space somebody is administering, as it last answered.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceRow {
    pub space: String,
    /// This actor's standing *here*. Per Orbit rather than per identity: one
    /// identity may hold very different standing in two Spaces, and a single
    /// answer would have to pick one and be wrong about the other.
    pub whoami: Option<String>,
    pub admin: bool,
    pub members: Vec<MemberRow>,
    pub devices: Vec<String>,
    pub diagnosis: Option<DiagnosisRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberRow {
    pub id: String,
    pub nick: Option<String>,
    /// Authored Card name that names this member, when one exists.
    /// Distinct from [`Self::nick`], which is Space-local and derived.
    pub authored_name: Option<String>,
    pub admin: bool,
}

/// The identity's address book, as last read.
#[derive(Debug, Clone, PartialEq)]
pub struct BookFacts {
    pub cards: Vec<CardRow>,
    pub migration_complete: bool,
    pub migration_pending: u32,
    pub migration_imported: u32,
    /// Staged card-exchange proposals awaiting review. Not in the book.
    pub suggestions: Vec<SuggestionRow>,
}

/// A person's correspondence, drawn as conversations rather than an inbox.
///
/// `None` on `ClientView` until it has been read once — the same
/// loading-versus-empty distinction the book keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct CorrespondenceFacts {
    /// This identity's own device id on the plane — the address a correspondent
    /// writes to. `None` until the plane is known.
    pub my_device: Option<String>,
    /// What this identity hands somebody so they can reach it, rendered for
    /// copying. `None` until something has been published. Not a Card (that is
    /// the address book's, and asserts nothing) and not an address (that is the
    /// directory's, and is short and spoken).
    pub my_reach: Option<String>,
    /// Which conversation is this identity's own, when the backend has one.
    pub me: Option<String>,
    /// The people this identity can reach. A person folds all their devices into
    /// one contact, and a click on one opens a chat.
    pub contacts: Vec<ContactRow>,
    /// One transcript per person, mixing sent and received.
    pub conversations: Vec<ConversationRow>,
    /// Which conversations are open as tabs, in tab order. Shared state, so a
    /// click in the address book opens the tab the chat window draws.
    pub open_tabs: Vec<String>,
    /// The focused tab, if any.
    pub active_tab: Option<String>,
}

/// A person's profile, drawn as the devices that are theirs.
///
/// Every device of a profile is the same person; there is nothing to approve
/// per Space and nothing to enrol twice. What a surface draws from this is
/// three things: this device's own standing, the devices already here, and
/// the one act — adding another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFacts {
    /// The profile's own address on the plane.
    pub profile: String,
    /// This machine's device id — the join key every row here uses. `None`
    /// when the daemon does not know which device it is.
    pub me: Option<String>,
    pub origin: Option<OriginRow>,
    /// The device set, `me` included, in the daemon's order.
    pub devices: Vec<OwnDeviceRow>,
    /// The set was not held. Carried rather than folded into an empty
    /// `devices`, because "not read" and "none" are different facts and only
    /// one of them is worth acting on.
    pub device_set_unknown: bool,
    /// The tunnel interface on this machine. `None` when the daemon carries
    /// no net plane at all — which is not the same as a machine that will not
    /// give one, and neither is a failure.
    pub interface: Option<InterfaceRow>,
    /// The code this device is showing while it waits to be added to a
    /// profile. `None` whenever it holds none.
    pub pairing: Option<PairingCodeRow>,
    /// Devices that answered a code entered here and are waiting on their six
    /// words being compared.
    pub offers: Vec<PairOfferRow>,
    /// The markers this person's device weighs, in its own order. A tier:
    /// nothing on this surface is withheld, disabled or refused because a
    /// marker is silent, and the list exists so that a device with no
    /// certification can be told apart from a marker that could not be
    /// asked.
    pub markers: Vec<MarkerRow>,
    /// One row per Space this machine holds, and where the last offer of it
    /// to each other device stood. This is the only place a surface can tell
    /// "a person said not on that machine" from "that machine could not be
    /// asked" from "nothing has offered it yet" — three absences that look
    /// identical on the device row, and only one of which is worth acting on.
    pub spaces: Vec<SpaceFanoutRow>,
}

/// One Space this machine holds, as the other devices of the profile stand
/// towards it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceFanoutRow {
    pub space: String,
    /// The devices the Space's own list names. Read from the Space, never
    /// inferred from how an offer went.
    pub on: Vec<String>,
    pub standings: Vec<DeviceStandingRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStandingRow {
    pub device: String,
    pub standing: StandingRow,
}

/// Where one Space stands towards one device.
///
/// Six facts, and absence is a seventh: no row at all means nothing has
/// offered it yet. `Excluded` is a person's decision and `Declined` is the
/// device's own answer; `CouldNotAsk` is neither, and drawing it as either
/// would report a network as a choice somebody made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StandingRow {
    /// The Space's own list names the device.
    Held,
    /// The device answered no.
    Declined { why: String },
    /// One side refused the exchange.
    Refused { why: String },
    /// Not askable yet for a reason on this machine.
    Deferred { why: String },
    /// The device did not answer.
    CouldNotAsk { why: String },
    /// A person said this Space is not held on that device. `told` is
    /// whether that machine has heard it yet — until it has, the decision
    /// stands here and is still owed to it.
    Excluded { told: bool },
}

/// How a device came to hold its profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginRow {
    Founded,
    Adopted { from: String, at: u64 },
}

/// One device of the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnDeviceRow {
    pub device: String,
    /// Whether this is the device answering.
    pub me: bool,
    pub liveness: LivenessRow,
    /// The Spaces this device is listed in. Empty is an answer; it is not
    /// "unknown", which is [`LivenessRow::NotProbed`]'s business.
    pub held: Vec<String>,
    /// The markers that have recorded this device, named as
    /// [`MarkerRow::marker`] names them so a surface joins on it. Empty is
    /// the ordinary answer for a device nobody has recorded yet, and it
    /// costs that device nothing.
    pub certified_by: Vec<String>,
    /// How the tunnel reaches this device. `None` is "nothing carried", which
    /// is not "unreachable" — and the three ways of not being reached below
    /// are not each other either.
    pub reach: Option<ReachRow>,
}

/// One marker this person's device weighs.
///
/// Evidence, never authority. A marker records what it was told and signs
/// what it recorded; nothing on this client asks one for permission, and no
/// control anywhere is enabled or disabled by what one says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerRow {
    /// Where the marker answers — the only part of one a person is shown.
    pub marker: String,
    pub standing: MarkerStandingRow,
}

/// How one marker's last check went, in the client's vocabulary.
///
/// Eight facts, kept apart. The three that matter most: a marker nothing has
/// asked yet, a marker that could not be reached, and a marker that answered.
/// Only the last of those can make a device "not listed" — for the other two
/// there is no answer to be listed in, and a surface that said "not listed"
/// for them would be reporting a network as a judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerStandingRow {
    /// It answered, and its record checked out.
    Answering,
    /// Nothing has asked it yet.
    NeverAsked,
    /// It could not be reached.
    CouldNotAsk,
    /// It answered as a device other than the one this identity follows.
    AnsweredAsAnother,
    /// It answered with a record older than the one this identity holds.
    AnsweredOlder,
    /// It could not show its record carries the held one forward.
    Unproven,
    /// It gave two records that cannot both be true.
    Contradicted,
    /// Its answer was not a record this device can read.
    Unreadable,
}

/// The tunnel interface on the machine this client is talking to.
///
/// `NotPermitted` is the one a person can act on: a daemon started by hand
/// holds no `CAP_NET_ADMIN`, and installing the service is what gives it one.
/// `Unsupported` is a platform whose seam is not built yet, and `Off` is the
/// operator's own switch. None of the three is a failure, and none of them
/// says anything about how many devices a person has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterfaceRow {
    Up { name: String, address: String },
    NotPermitted,
    Unsupported,
    Off,
}

/// How the tunnel reaches one own device.
///
/// `NoRoute` (nothing ever taught this device where that one is),
/// `Unreachable` (a dial was made and failed) and `Retired` (the set no
/// longer names it) are three different facts. Folding them would draw a
/// revoked device as a flaky one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachRow {
    /// `via` is the path the transport chose: `direct`, `relay`, `unknown`.
    Connected {
        via: String,
    },
    Dialing,
    NoRoute,
    /// Unix seconds.
    Unreachable {
        since: u64,
    },
    Retired,
}

/// What asking a device last learned.
///
/// `NotProbed` is neither "down" nor "could not be reached": nothing has
/// asked yet. Folding the three would be the false-disconnection defect, one
/// layer up from where it was already fixed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LivenessRow {
    Answered {
        version: String,
        at: u64,
    },
    CouldNotAsk {
        why: String,
    },
    #[default]
    NotProbed,
}

/// The code a device shows while it waits to be added, as a person reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCodeRow {
    /// Grouped, `XXXX-XXXX`.
    pub code: String,
    /// Addresses to enter beside the code when nothing relays between the two
    /// devices. Empty under a relay, and an empty list is not a failure.
    pub direct: Vec<String>,
    pub expires_at_ms: u64,
}

/// A device that answered a code entered here, waiting on confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairOfferRow {
    pub pairing: String,
    pub device: String,
    /// The name the device gave for itself.
    pub name: String,
    /// The six words the other device also shows. Confirming without them
    /// matching is the one mistake this ceremony exists to prevent, so they
    /// cross as the words themselves and never as a "verified" flag.
    pub phrase: Vec<String>,
    pub expires_at_ms: u64,
}

/// One person one can message, with each device that is them.
#[derive(Debug, Clone, PartialEq)]
pub struct ContactRow {
    pub id: String,
    pub name: String,
    pub devices: Vec<String>,
    /// In the book (a friend) vs an unadded stranger who wrote first. Parts the
    /// normal contact list from the incoming section.
    pub added: bool,
    /// An agent rather than a person — wears the AI mark.
    pub is_agent: bool,
    /// If this is a contact's agent, whose, and their name for the label.
    pub parent_id: Option<String>,
    pub parent_name: Option<String>,
    /// Unread received messages — the badge. Zero once opened.
    pub unread: u32,
}

/// One conversation: who it is with, and every message either way.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationRow {
    pub peer_id: String,
    pub peer_name: String,
    pub messages: Vec<ChatMessageRow>,
}

/// One message in a conversation. The chat draws a custom component per `kind`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessageRow {
    /// For an invitation, the link body it carries; `None` for a message.
    pub invitation: Option<String>,
    /// The deposit id for a received letter; `None` for one this identity sent.
    /// An invitation is acted on by naming this.
    pub id: Option<String>,
    /// True if this identity sent it — which side of the chat it is drawn on.
    pub mine: bool,
    /// `message` (text) or `invitation`. The chat draws each with its own
    /// component: one is read, the other acted on.
    pub kind: String,
    /// The text, for a message. `None` for an invitation.
    pub body: Option<String>,
    /// When it was written, unix seconds.
    pub sent_at: u64,
    /// The proven signer's device, for a received message.
    pub from_device: String,
    /// Whether the carrier's word matched the proof. `false` is not wrong but is
    /// worth surfacing rather than hiding.
    pub provenance_agrees: bool,
}

/// One staged suggestion from a card-exchange file. Review is the only way
/// into the book, so this carries exactly what the person must judge.
#[derive(Debug, Clone, PartialEq)]
pub struct SuggestionRow {
    pub suggestion: String,
    pub name: String,
    pub note: String,
    pub handles: Vec<String>,
}

/// Measured reachability for the identity a Card names, from this device's
/// vantage — the Neighbor registry's beacon-fed answer, never a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceView {
    Online,
    Away,
    /// A Space that names this identity answered, and nothing speaks for it
    /// right now. A measurement — distinct from the card-level `None`, which
    /// is "no Space that names it could be asked".
    Offline,
}

/// One authored Card. Handles are wire spellings, never reachability.
#[derive(Debug, Clone, PartialEq)]
pub struct CardRow {
    pub card: String,
    pub name: String,
    pub note: String,
    pub handles: Vec<String>,
    /// The phone-book reading of `handles`: addresses (`actor:` spellings),
    /// devices (bare device ids), and co-located agents (`agent:` spellings).
    pub addresses: Vec<String>,
    pub devices: Vec<String>,
    pub agents: Vec<String>,
    /// The stored picture (`<mime>;base64,<data>`), or `None` — the surface
    /// draws its default face for a card without one.
    pub picture: Option<String>,
    pub groups: Vec<String>,
    pub self_claim: bool,
    /// Measured presence joined over this card's handles, or `None` when no
    /// Space that names it could be asked. Unmeasured is absent, never
    /// `Offline` — the wire keeps the two apart so the surface can too.
    pub presence: Option<PresenceView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoticeRow {
    pub said: String,
    /// Where a browser was sent, when that is what happened.
    pub launched: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FailureRow {
    pub what: String,
    pub error: String,
    /// Whether asking again could plausibly work. A refusal is not a retry.
    pub retryable: bool,
    /// The action key this refused, so the surface that asked can answer
    /// beside its own control.
    pub key: Option<String>,
}

/// What a person asked for.
///
/// Every mutation crosses as one of these. There is no other route, and in
/// particular there is no "set this field" — a surface that could write a
/// field would be a second model of the thing it wrote.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionRequest {
    /// Read this machine again.
    Refresh,
    /// Hand a World to the person's browser.
    ///
    /// Names the mount as well as the path: a head serves one World, so opening
    /// says which rather than reaching for whichever head is up.
    Open {
        world: String,
        entry_path: String,
    },
    /// Restage from the rebuilt source and bring everything back on the new
    /// image — owned devices and heads through the supervisor, then the
    /// identity daemon. The inner-loop gesture: `cargo build`, then this.
    Reload,
    /// End the client, answering the one question quitting asks: does this
    /// device stay online? The report crosses as a notice, and
    /// [`ClientView::exited`] is the shell's cue that closing may happen.
    Exit {
        go_offline: bool,
    },
    /// Act on a `lait:` link the operating system delivered — at launch as an
    /// argument, or while running as an open-URL event.
    OpenLink {
        url: String,
    },
    /// Fetch this World's newest bundle now rather than at the next period.
    ///
    /// The daemon stages on a period measured in hours; a World is published
    /// in seconds. This is the control that closes that gap, and it is the
    /// whole reason a Library row ever draws an update affordance.
    UpdateWorld {
        world: String,
    },
    /// Install a World that is already visible in the Library catalog.
    InstallWorld {
        world: String,
    },
    /// Follow a channel for this World alone, or (`None`) follow the node's.
    FollowWorldChannel {
        world: String,
        channel: Option<String>,
    },
    /// Register a tree on this device as a local World of its own.
    ///
    /// The directory is the whole ask. What it is called is derived from what
    /// the tree already declares — asking for a name would be asking somebody
    /// to invent one for a thing that has one.
    RegisterLocalWorld {
        dir: String,
    },
    /// Forget one, by the key it was registered under. The tree is untouched:
    /// this device stops carrying a row for it, and nothing is deleted.
    ForgetLocalWorld {
        key: String,
    },
    StartDevice {
        id: String,
    },
    StopDevice {
        id: String,
    },
    RestartDevice {
        id: String,
    },
    /// Force-stop. Only ever offered for a daemon this client owns: ownership
    /// is the safety boundary, and there is no pid-based path across it.
    ForceStopDevice {
        id: String,
    },
    StopAllOwned,
    /// Forget a device. `delete_data` additionally destroys what it holds, and
    /// is the one flag here that cannot be undone.
    RemoveDevice {
        id: String,
        delete_data: bool,
    },
    /// Read one Space. Choosing a Space to administer is an act, not a
    /// listing — it is the read that makes the Members surface answerable.
    ReadSpace {
        orbit: String,
    },
    /// Start a browser head for this identity.
    StartHead,
    StopHead {
        id: String,
    },
    /// Send a message to a person, over the configured carrier.
    SendMessage {
        to: String,
        body: String,
    },
    /// Publish this identity's reach so it can be handed to somebody. Acts on
    /// nobody — showing a friend code is not befriending anyone.
    ShareReach,
    /// Take a correspondent in, by the announcement they handed over. The one
    /// of the pair that creates a relationship, which is why it is named for
    /// the person rather than for the artifact.
    AddCorrespondent {
        announcement: String,
    },
    /// Enter the Space an arriving invitation names.
    ///
    /// `message` is the invitation's deposit id in the transcript. Its
    /// coordinates verify against their own Space, so accepting is the same act
    /// as following an invite link — delivery was never admission.
    OpenInvitation {
        message: String,
    },
    /// Carry an invitation this identity already holds to a correspondent.
    ///
    /// `link` is an invite as a person receives one. Minting one is the Space's
    /// authority and stays there; this only carries what somebody was given.
    SendInvitation {
        to: String,
        link: String,
    },
    /// Ask the carrier for anything waiting, and file it into conversations.
    CollectMail,
    /// Add a device to this profile, by the code it is showing. `XXXX-XXXX`,
    /// or `XXXX-XXXX@host:port` when the two devices share a network and
    /// nothing relays between them.
    DevicePairEnter {
        code: String,
    },
    /// Answer a waiting device once its six words have been compared.
    /// Rejecting writes nothing and tells that device nothing.
    DevicePairConfirm {
        pairing: String,
        accept: bool,
    },
    /// Say whether one Space is held on one device of this profile.
    ///
    /// `excluded: true` takes it off that machine — removal, never deletion —
    /// and `false` puts it back through the same consent that put it there
    /// the first time.
    ReplicaExclude {
        device: String,
        space: String,
        excluded: bool,
    },
    /// Retire a device of this profile, from another one.
    ///
    /// The profile stops naming it, and every Space this daemon holds that
    /// listed it stops listing it. Nothing on that machine is deleted and
    /// nothing is asked of it — it may be off, or gone.
    DeviceRetire {
        device: String,
    },
    /// Block a person at the carrier, so no device of theirs lands again. Also
    /// how an incoming stranger is dismissed.
    BlockSender {
        person: String,
    },
    /// Accept an unknown correspondent into the address book.
    AcceptContact {
        person: String,
    },
    /// Open a conversation as a tab, and focus it. What a click in the address
    /// book asks for.
    OpenConversation {
        person: String,
    },
    /// Focus an already-open conversation tab.
    FocusConversation {
        person: String,
    },
    /// Close a conversation tab.
    CloseConversation {
        person: String,
    },
    /// Forget an Orbit. The store is left alone; this is registry-only.
    ForgetOrbit {
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
    BookImport {
        path: String,
    },
    BookAccept {
        suggestion: String,
    },
    BookDismiss {
        suggestion: String,
    },
    /// Author, or preview, an MCP binding for one World.
    ///
    /// Astrolabe writes the binding; the editor parents `lait mcp`. The World
    /// pin (`world`) is a selected installation's mount, not a path.
    InstallMcp {
        /// `claude` | `cursor` | `windsurf` | `generic`.
        client: String,
        /// `user` | `project`; `None` takes the client's default.
        scope: Option<String>,
        name: String,
        agent: Option<String>,
        no_agent: bool,
        project: String,
        /// World mount. `None` is the sole-World default.
        world: Option<String>,
        preview: bool,
    },
    /// Accept the receiver only after the person has compared the phrase and
    /// certificate fingerprint shown on both screens.
    DisplayPairingApprove {
        pairing: String,
        label: String,
    },
    DisplayPairingReject {
        pairing: String,
    },
    /// Mint a code a television enters to enrol without the two-screen
    /// ceremony — and, with an `assignment`, to show something the moment it
    /// does. The code works once and lasts minutes.
    DisplayRendezvousMint {
        label: String,
        assignment: Option<DisplayRendezvousAssignment>,
    },
    /// Withdraw a code before anything enters it.
    DisplayRendezvousRevoke {
        rendezvous: String,
    },
    /// Ask a World what one of its display surfaces can show in one Orbit.
    /// The answer lands in `DisplayFacts::choices`.
    DisplaySurfaceChoices {
        orbit: String,
        world: String,
        surface: String,
    },
    /// Assign one exact World display surface to an enrolled receiver. The
    /// input remains JSON here because each package owns its own input schema;
    /// the daemon canonicalizes and validates it before committing the pin.
    DisplayAssignmentPut {
        device: String,
        orbit: String,
        world: String,
        surface: String,
        input_json: String,
        theme: DisplayTheme,
        stale_after_ms: u32,
        on_stale: DisplayStaleAction,
        sync_group: Option<String>,
        sync_mode: DisplaySyncMode,
        static_delay_ms: i32,
        expires_at_unix_ms: Option<u64>,
    },
    DisplayAssignmentRevoke {
        assignment: String,
    },
    DisplayDeviceRevoke {
        device: String,
    },
    /// Add a passphrase as a second way into the coordinator's identifier key.
    ///
    /// The first slot is sealed to this daemon's device, which survives an
    /// operating-system profile but not the loss of the identity. A passphrase
    /// depends on neither, which is what makes it a second way in rather than a
    /// second copy of the first.
    DisplayIdentifierAdmitPassphrase {
        passphrase: String,
    },
    /// Make this machine a screen.
    ///
    /// Pressing the control is the whole of the consent — there is no dialog
    /// in front of it, because being asked *what to show* before you are a
    /// screen is the wrong order. Nothing is enrolled and nothing is
    /// committed: this client is already a member of the Space it will draw,
    /// so there is no stranger to issue a credential to. Leaving is
    /// [`ActionRequest::LeavePresentation`].
    EnterPresentation,
    /// Point this screen at one exact surface. Dispatched from inside the
    /// mode, never as a precondition for entering it.
    PresentHere {
        orbit: String,
        world: String,
        surface: String,
        input: String,
        title: String,
    },
    /// Ask the current selection again. What a refresh boundary and a manual
    /// nudge both do.
    PresentRefresh,
    /// Stop being a screen.
    LeavePresentation,
}

impl ActionRequest {
    fn into_action(self) -> Result<Action, String> {
        Ok(match self {
            Self::Refresh => Action::Refresh,
            Self::UpdateWorld { world } => Action::UpdateWorld { world },
            Self::FollowWorldChannel { world, channel } => {
                Action::FollowWorldChannel { world, channel }
            }
            Self::RegisterLocalWorld { dir } => Action::RegisterLocalWorld { dir },
            Self::ForgetLocalWorld { key } => Action::ForgetLocalWorld { key },
            Self::InstallWorld { world } => Action::InstallWorld { world },
            Self::Reload => Action::Reload,
            Self::Exit { go_offline } => Action::Exit(if go_offline {
                crate::lifecycle::ExitRequest::GoOffline
            } else {
                crate::lifecycle::ExitRequest::StayOnline
            }),
            Self::OpenLink { url } => Action::OpenLink { url },
            Self::Open { world, entry_path } => Action::OpenWorld { world, entry_path },
            Self::StartDevice { id } => Action::StartDevice(id),
            Self::StopDevice { id } => Action::StopDevice(id),
            Self::RestartDevice { id } => Action::RestartDevice(id),
            Self::ForceStopDevice { id } => Action::ForceStopDevice(id),
            Self::StopAllOwned => Action::StopAllOwned,
            Self::RemoveDevice { id, delete_data } => Action::RemoveDevice { id, delete_data },
            Self::ReadSpace { orbit } => Action::ReadSpace(space_ref(orbit)),
            Self::StartHead => Action::StartHead,
            Self::StopHead { id } => Action::StopHead(id),
            Self::SendMessage { to, body } => Action::SendMessage { to, body },
            Self::ShareReach => Action::ShareReach,
            Self::AddCorrespondent { announcement } => Action::AddCorrespondent { announcement },
            Self::OpenInvitation { message } => Action::OpenInvitation { message },
            Self::SendInvitation { to, link } => Action::SendInvitation { to, link },
            Self::CollectMail => Action::CollectMail,
            Self::DevicePairEnter { code } => Action::DevicePairEnter { code },
            Self::DevicePairConfirm { pairing, accept } => {
                Action::DevicePairConfirm { pairing, accept }
            }
            Self::DeviceRetire { device } => Action::DeviceRetire { device },
            Self::ReplicaExclude {
                device,
                space,
                excluded,
            } => Action::ReplicaExclude {
                device,
                space,
                excluded,
            },
            Self::BlockSender { person } => Action::BlockSender(person),
            Self::AcceptContact { person } => Action::AcceptContact(person),
            Self::OpenConversation { person } => Action::OpenConversation(person),
            Self::FocusConversation { person } => Action::FocusConversation(person),
            Self::CloseConversation { person } => Action::CloseConversation(person),
            Self::ForgetOrbit { space } => Action::OrbitForget { space },
            Self::BookPut { card, name, note } => Action::BookPut { card, name, note },
            Self::BookDelete { card } => Action::BookDelete { card },
            Self::BookSetPicture { card, path } => Action::BookSetPicture { card, path },
            Self::BookMerge { from, into } => Action::BookMerge { from, into },
            Self::BookClaimSelf { card } => Action::BookClaimSelf { card },
            Self::BookLink { card, handle } => Action::BookLink { card, handle },
            Self::BookUnlink { card, handle } => Action::BookUnlink { card, handle },
            Self::BookExport { path, cards } => Action::BookExport { path, cards },
            Self::BookImport { path } => Action::BookPropose { path },
            Self::BookAccept { suggestion } => Action::BookAccept { suggestion },
            Self::BookDismiss { suggestion } => Action::BookDismiss { suggestion },
            Self::InstallMcp {
                client,
                scope,
                name,
                agent,
                no_agent,
                project,
                world,
                preview,
            } => Action::InstallMcp {
                binding: Box::new(crate::client::heads::McpBinding {
                    client: parse_agent_client(&client)?,
                    scope: parse_mcp_scope(scope.as_deref()),
                    name,
                    agent,
                    no_agent,
                    project,
                    world,
                }),
                preview,
            },
            Self::DisplayPairingApprove { pairing, label } => {
                Action::DisplayPairingApprove { pairing, label }
            }
            Self::DisplayPairingReject { pairing } => Action::DisplayPairingReject(pairing),
            Self::DisplayRendezvousMint { label, assignment } => {
                let assignment = assignment
                    .map(|assignment| -> Result<_, String> {
                        Ok(crate::client::display::DisplayRendezvousAssignmentInput {
                            orbit: assignment.orbit,
                            world: assignment.world,
                            surface: assignment.surface,
                            input: serde_json::from_str(&assignment.input_json)
                                .map_err(|error| format!("invalid display input JSON: {error}"))?,
                            theme: match assignment.theme {
                                DisplayTheme::Light => lait::control::DisplayThemeSetting::Light,
                                DisplayTheme::Dark => lait::control::DisplayThemeSetting::Dark,
                                DisplayTheme::HighContrast => {
                                    lait::control::DisplayThemeSetting::HighContrast
                                }
                            },
                            stale_after_ms: assignment.stale_after_ms,
                            on_stale: match assignment.on_stale {
                                DisplayStaleAction::KeepWithNativeBanner => {
                                    lait::control::DisplayStaleActionSetting::KeepWithNativeBanner
                                }
                                DisplayStaleAction::Blank => {
                                    lait::control::DisplayStaleActionSetting::Blank
                                }
                            },
                            sync: assignment.sync_group.map(|group| {
                                lait::control::DisplayAssignmentSyncSetting {
                                    group,
                                    mode: match assignment.sync_mode {
                                        DisplaySyncMode::StayInSync => {
                                            lait::control::DisplaySyncModeSetting::StayInSync
                                        }
                                        DisplaySyncMode::Positional => {
                                            lait::control::DisplaySyncModeSetting::Positional
                                        }
                                    },
                                    static_delay_ms: assignment.static_delay_ms,
                                }
                            }),
                            expires_at_unix_ms: assignment.expires_at_unix_ms,
                        })
                    })
                    .transpose()?;
                Action::DisplayRendezvousMint(Box::new(
                    crate::client::display::DisplayRendezvousInput { label, assignment },
                ))
            }
            Self::DisplayRendezvousRevoke { rendezvous } => {
                Action::DisplayRendezvousRevoke(rendezvous)
            }
            Self::DisplaySurfaceChoices {
                orbit,
                world,
                surface,
            } => Action::DisplaySurfaceChoices {
                orbit,
                world,
                surface,
            },
            Self::DisplayAssignmentPut {
                device,
                orbit,
                world,
                surface,
                input_json,
                theme,
                stale_after_ms,
                on_stale,
                sync_group,
                sync_mode,
                static_delay_ms,
                expires_at_unix_ms,
            } => Action::DisplayAssignmentPut(Box::new(
                crate::client::display::DisplayAssignmentInput {
                    device,
                    orbit,
                    world,
                    surface,
                    input: serde_json::from_str(&input_json)
                        .map_err(|error| format!("invalid display input JSON: {error}"))?,
                    theme: match theme {
                        DisplayTheme::Light => lait::control::DisplayThemeSetting::Light,
                        DisplayTheme::Dark => lait::control::DisplayThemeSetting::Dark,
                        DisplayTheme::HighContrast => {
                            lait::control::DisplayThemeSetting::HighContrast
                        }
                    },
                    stale_after_ms,
                    on_stale: match on_stale {
                        DisplayStaleAction::KeepWithNativeBanner => {
                            lait::control::DisplayStaleActionSetting::KeepWithNativeBanner
                        }
                        DisplayStaleAction::Blank => {
                            lait::control::DisplayStaleActionSetting::Blank
                        }
                    },
                    sync: sync_group.map(|group| lait::control::DisplayAssignmentSyncSetting {
                        group,
                        mode: match sync_mode {
                            DisplaySyncMode::StayInSync => {
                                lait::control::DisplaySyncModeSetting::StayInSync
                            }
                            DisplaySyncMode::Positional => {
                                lait::control::DisplaySyncModeSetting::Positional
                            }
                        },
                        static_delay_ms,
                    }),
                    expires_at_unix_ms,
                },
            )),
            Self::DisplayAssignmentRevoke { assignment } => {
                Action::DisplayAssignmentRevoke(assignment)
            }
            Self::DisplayDeviceRevoke { device } => Action::DisplayDeviceRevoke(device),
            Self::DisplayIdentifierAdmitPassphrase { passphrase } => {
                Action::DisplayIdentifierAdmitPassphrase(passphrase)
            }
            Self::EnterPresentation => Action::EnterPresentation,
            Self::PresentHere {
                orbit,
                world,
                surface,
                input,
                title,
            } => Action::PresentHere(Box::new(crate::model::PresentationSelection {
                orbit,
                world,
                surface,
                input,
                title,
            })),
            Self::PresentRefresh => Action::PresentRefresh,
            Self::LeavePresentation => Action::LeavePresentation,
        })
    }
}

fn parse_agent_client(name: &str) -> Result<lait::install::Client, String> {
    match name {
        "claude" => Ok(lait::install::Client::Claude),
        "cursor" => Ok(lait::install::Client::Cursor),
        "windsurf" => Ok(lait::install::Client::Windsurf),
        "generic" => Ok(lait::install::Client::Generic),
        other => Err(format!(
            "unknown MCP client '{other}'; use claude, cursor, windsurf, or generic"
        )),
    }
}

fn parse_mcp_scope(scope: Option<&str>) -> Option<lait::install::Scope> {
    match scope {
        Some("user") => Some(lait::install::Scope::User),
        Some("project") => Some(lait::install::Scope::Project),
        _ => None,
    }
}

/// Resolve an Orbit id to the reference the Space plane needs.
///
/// The path comes from the registry the core already holds. An interface that
/// carried a store path would be holding a fact it could not have checked, and
/// would send a stale one the first time an Orbit moved.
fn space_ref(orbit: String) -> crate::client::space::SpaceRef {
    let path = CORE
        .get()
        .and_then(|core| core.lock().ok())
        .and_then(|core| {
            core.app.context().and_then(|context| {
                context
                    .orbits
                    .iter()
                    .find(|entry| entry.space == orbit)
                    .map(|entry| entry.path.clone())
            })
        })
        .unwrap_or_default();
    crate::client::space::SpaceRef { space: orbit, path }
}

/// The model, the background half, and the identity they were booted for.
struct Core {
    app: App,
    runtime: Runtime,
    state_root: PathBuf,
    sidecar: PathBuf,
    world_catalog: Option<PathBuf>,
}

static CORE: OnceLock<Mutex<Core>> = OnceLock::new();
/// Serialises the first boot so two isolates cannot each start a runtime
/// in the window between "not yet" and `CORE.set`.
static BOOT: Mutex<()> = Mutex::new(());
/// Told by the runtime's wake callback that something has arrived. A channel
/// rather than the mutex, because waking must not be able to block on the lock
/// the pump is holding while it drains.
static WOKEN: OnceLock<Sender<()>> = OnceLock::new();
static INSTANCE_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SUMMON: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Claim the single instance, before any window exists.
///
/// `false` means a client is already running and now holds this launch's
/// arguments — the caller has nothing left to do but end, windowless. `true`
/// makes this process the client: later launches arrive on the instance
/// channel, a `lait:` link among their arguments is dispatched as
/// [`ActionRequest::OpenLink`], and the host's [`on_second_launch`] hook is
/// called either way.
pub fn claim_single_instance() -> Result<bool, String> {
    match crate::single_instance::claim(std::env::args()).map_err(|error| format!("{error:#}"))? {
        crate::single_instance::Claim::Primary { guard, channel } => {
            // Forgotten, not stored: held for the life of the process.
            std::mem::forget(guard);
            INSTANCE_HELD.store(true, std::sync::atomic::Ordering::Release);
            if let Some(channel) = channel {
                drain_second_launches(channel);
            }
            Ok(true)
        }
        crate::single_instance::Claim::Forwarded => Ok(false),
    }
}

/// How the host raises the client when a later launch hands itself over.
pub fn on_second_launch(raise: impl Fn() + Send + Sync + 'static) {
    let _ = SUMMON.set(Box::new(raise));
}

fn drain_second_launches(channel: crate::single_instance::Channel) {
    std::thread::spawn(move || {
        for message in channel.messages() {
            // A click can race a cold start by the length of a boot; an
            // action dispatched before the core exists is dropped, not held.
            for _ in 0..300 {
                if CORE.get().is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if let Some(link) = crate::link::Link::from_args(message.lines().map(str::to_owned)) {
                dispatch(ActionRequest::OpenLink { url: link.to_url() });
            }
            if let Some(summon) = SUMMON.get() {
                summon();
            }
        }
    });
}

/// Start the core, or attach to the one that is already running.
///
/// The first isolate boots the runtime. Later ones attach: same App, same
/// supervisor, a new view stream. A second *boot* — a start against a
/// different identity — is refused rather than spawning a second supervisor
/// of the same devices.
pub fn start(state_root: Option<String>, sidecar: Option<String>) -> Result<(), String> {
    start_with_catalog(state_root, sidecar, None)
}

/// Start with reviewed first-party World catalog metadata carried by the
/// native host. The catalog contains no executable World payload.
pub fn start_with_catalog(
    state_root: Option<String>,
    sidecar: Option<String>,
    world_catalog: Option<String>,
) -> Result<(), String> {
    // Resolved here when the caller has no opinion, which is every real launch.
    // The interface must not compute either of these: a path worked out on the
    // far side of the bridge is a second opinion about where this installation
    // keeps its things, and the two would differ on exactly the machine where
    // it mattered. The arguments exist so a test can point the core somewhere
    // disposable, and for nothing else.
    let state_root = match state_root {
        Some(given) => PathBuf::from(given),
        None => crate::sidecar::state_root().map_err(|error| format!("{error:#}"))?,
    };
    let sidecar = match sidecar {
        Some(given) => PathBuf::from(given),
        None => crate::sidecar::resolve().map_err(|error| format!("{error:#}"))?,
    };

    if let Some(core) = CORE.get() {
        return attach_to(core, &state_root, &sidecar);
    }

    let _boot = BOOT
        .lock()
        .map_err(|_| "the core boot lock is poisoned".to_string())?;
    if let Some(core) = CORE.get() {
        return attach_to(core, &state_root, &sidecar);
    }

    // The backstop for a host that never called [`claim_single_instance`].
    if !INSTANCE_HELD.load(std::sync::atomic::Ordering::Acquire) {
        match crate::single_instance::acquire().map_err(|error| format!("{error:#}"))? {
            // Forgotten, not stored: held for the life of the process, and
            // the kernel releases it however that ends.
            crate::single_instance::Outcome::Held(guard) => {
                std::mem::forget(guard);
                INSTANCE_HELD.store(true, std::sync::atomic::Ordering::Release);
            }
            crate::single_instance::Outcome::AlreadyRunning => {
                return Err(
                    "another Astrolabe is already running on this machine; use its window".into(),
                );
            }
        }
    }

    let (woken, wakeups) = channel();
    let wake = woken.clone();
    let mut config = Config::new(state_root.clone(), sidecar.clone());
    config.world_catalog = world_catalog.clone().map(PathBuf::from);
    // A standalone launch, set by the environment: come up without the identity
    // daemon rather than waiting on one. `env_flag` so `1`, `true`, `on` and
    // `yes` all read the same, and an empty or absent value stays off.
    config.skip_sidecar = env_flag("LAIT_SKIP_SIDECAR");
    // Opt in to the in-process correspondence fixture — off by default, so
    // correspondence refuses honestly until a real carrier exists.
    config.correspondence_demo = env_flag("LAIT_CORRESPONDENCE_DEMO");
    // Carry real correspondence over a hosted Post when one resolves. Takes
    // precedence over the fixture: real carriage beats a loopback one. The
    // resolution is the daemon's own — env override, config key, then the
    // cloud built-in release builds carry — so the client and the daemon it
    // spawns cannot disagree about which Post this identity is on, which is
    // exactly the disagreement the env-only read used to permit.
    config.post_url = lait::config::Settings::load(None).post_url();
    let runtime = Runtime::start(config, move || {
        // A failed send means the pump is gone, which happens only on the
        // way out. Nothing to report and nobody to report it to.
        let _ = wake.send(());
    })
    .map_err(|error| error.to_string())?;

    let _ = CORE.set(Mutex::new(Core {
        app: App::new(),
        runtime,
        state_root,
        sidecar,
        world_catalog: world_catalog.map(PathBuf::from),
    }));
    let _ = WOKEN.set(woken);

    // The pump. It drains and projects; it never draws, and it never holds the
    // lock across the send.
    std::thread::Builder::new()
        .name("astrolabe-pump".into())
        .spawn(move || {
            while wakeups.recv().is_ok() {
                let view = {
                    let Some(core) = CORE.get() else { return };
                    let Ok(mut core) = core.lock() else { return };
                    let Core { app, runtime, .. } = &mut *core;
                    runtime.drain_into(app);
                    project(app)
                };
                emit(view);
            }
        })
        .map_err(|error| format!("start the projection pump: {error}"))?;
    Ok(())
}

/// Whether an environment variable reads as set. `1`, `true`, `on`, `yes`
/// (any case) are on; absent, empty, and everything else are off.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
}

#[cfg(test)]
mod env_flag_tests {
    use super::env_flag;

    /// The flag reads the truthy spellings and nothing else. A unique variable
    /// name per case keeps these independent of one another and of any other
    /// test touching the environment.
    #[test]
    fn env_flag_reads_the_truthy_spellings_only() {
        for (raw, want) in [
            ("1", true),
            ("TRUE", true),
            ("On", true),
            (" yes ", true),
            ("0", false),
            ("off", false),
            ("", false),
            ("nope", false),
        ] {
            let name = format!("LAIT_TEST_ENV_FLAG_{}", raw.trim().to_ascii_uppercase());
            // SAFETY: a test-only variable this test owns; no other test reads it.
            unsafe { std::env::set_var(&name, raw) };
            assert_eq!(env_flag(&name), want, "{raw:?} should read as {want}");
            unsafe { std::env::remove_var(&name) };
        }
        assert!(!env_flag("LAIT_TEST_ENV_FLAG_DEFINITELY_ABSENT"));
    }
}

fn attach_to(core: &Mutex<Core>, state_root: &Path, sidecar: &Path) -> Result<(), String> {
    let Ok(core) = core.lock() else {
        return Err("the core is poisoned".into());
    };
    attach_paths((&core.state_root, &core.sidecar), (state_root, sidecar))
}

/// Same identity attaches; a different one is a second boot, and is refused.
fn attach_paths(running: (&Path, &Path), asked: (&Path, &Path)) -> Result<(), String> {
    if running.0 == asked.0 && running.1 == asked.1 {
        Ok(())
    } else {
        Err(
            "the core is already running for a different identity — a second boot is refused"
                .into(),
        )
    }
}

/// Ask for something, and get back the view as it stands the instant it was
/// asked for.
///
/// Returning the view rather than nothing is what makes "a control is disabled
/// the moment it is clicked" true across a boundary. Waiting for the pump would
/// leave a live control for one round trip, which is long enough to press four
/// times and see three refusals.
pub fn dispatch(action: ActionRequest) -> ClientView {
    // Translated *before* the lock is taken: resolving an Orbit reads the
    // registry, which needs the same lock, and a translation done inside the
    // guard would deadlock on exactly the actions that touch a Space.
    let action = match action.into_action() {
        Ok(action) => action,
        Err(error) => {
            let Some(core) = CORE.get() else {
                return empty();
            };
            let Ok(mut core) = core.lock() else {
                return empty();
            };
            core.app.fail(
                "dispatch a client action",
                crate::client::ClientError::invalid(error),
            );
            let view = project(&core.app);
            emit(view.clone());
            return view;
        }
    };
    let Some(core) = CORE.get() else {
        return empty();
    };
    let Ok(mut core) = core.lock() else {
        return empty();
    };
    let Core { app, runtime, .. } = &mut *core;
    app.dispatched(&action);
    runtime.dispatch(action);
    let view = project(app);
    // Every watcher hears about the in-flight action now, not at completion:
    // with two windows on one model, the peer window's control must disable
    // on this dispatch, and the next pump only arrives when the action ends.
    emit(view.clone());
    view
}

/// The artwork one installed or catalogued World declares, by mount.
///
/// The one thing that crosses this boundary without being part of
/// [`ClientView`], and for a reason the view's own contract gives: the view is
/// pushed whole to every attached surface on every pump, and artwork is a
/// build constant that cannot change while the process runs. Riding in the view
/// it would be re-marshalled on every presence sample to repeat itself. Asked
/// for once and cached by the surface, it costs one copy for the life of the
/// window.
///
/// An unknown mount answers with no artwork, not an error.
pub fn world_artwork(mount: String) -> WorldArtwork {
    let catalog = CORE
        .get()
        .and_then(|core| core.lock().ok())
        .and_then(|core| core.world_catalog.clone());
    let art = crate::client::library::artwork(&mount, catalog.as_deref());
    WorldArtwork {
        mark: art.mark,
        hero: art.hero,
    }
}

/// The view as it stands, for a surface that has just been built.
pub fn current() -> ClientView {
    let Some(core) = CORE.get() else {
        return empty();
    };
    let Ok(core) = core.lock() else {
        return empty();
    };
    project(&core.app)
}

/// Attach a native host to the projection stream.
///
/// The host is an output-only subscriber: it gets every complete view and
/// cannot inspect or mutate the model. That is what keeps a desktop WebView
/// adapter from becoming a second client protocol beside this one.
///
/// This is the only way to watch the stream. The generated-binding half that
/// used to sit beside it — a `StreamSink` per Dart isolate — went with the
/// Flutter interface; see this module's header.
pub fn subscribe(listener: impl Fn(ClientView) + Send + Sync + 'static) {
    let sinks = SINKS.get_or_init(|| Mutex::new(Watchers::new()));
    let Ok(mut sinks) = sinks.lock() else {
        return;
    };
    sinks.attach(Box::new(CallbackSink(Arc::new(listener))), current());
}

static SINKS: OnceLock<Mutex<Watchers<Box<dyn ViewPush + Send>>>> = OnceLock::new();

fn emit(view: ClientView) {
    let Some(sinks) = SINKS.get() else {
        return;
    };
    let Ok(mut sinks) = sinks.lock() else {
        return;
    };
    sinks.emit(&view);
}

/// A sink that can take a view, or say it is gone.
trait ViewPush {
    fn push(&self, view: &ClientView) -> bool;
}

impl ViewPush for Box<dyn ViewPush + Send> {
    fn push(&self, view: &ClientView) -> bool {
        (**self).push(view)
    }
}

struct CallbackSink(Arc<dyn Fn(ClientView) + Send + Sync>);

impl ViewPush for CallbackSink {
    fn push(&self, view: &ClientView) -> bool {
        (self.0)(view.clone());
        true
    }
}

/// Every attached isolate's view stream.
struct Watchers<S> {
    sinks: Vec<S>,
}

impl<S: ViewPush> Watchers<S> {
    fn new() -> Self {
        Self { sinks: Vec::new() }
    }

    fn attach(&mut self, sink: S, current: ClientView) {
        if sink.push(&current) {
            self.sinks.push(sink);
        }
    }

    fn emit(&mut self, view: &ClientView) {
        self.sinks.retain(|sink| sink.push(view));
    }

    fn len(&self) -> usize {
        self.sinks.len()
    }
}

fn empty() -> ClientView {
    ClientView {
        loading: true,
        stale: Some(Staleness::NeverLoaded),
        library: None,
        host: None,
        display: None,
        presentation: None,
        heads: Vec::new(),
        devices: Vec::new(),
        storage: Vec::new(),
        orbits: Vec::new(),
        space: None,
        book: None,
        correspondence: None,
        profile: None,
        notices: Vec::new(),
        failures: Vec::new(),
        in_flight: Vec::new(),
        mcp: None,
        image: None,
        update: None,
        exited: false,
    }
}

/// Build the projection. The whole of the boundary's outbound half is this
/// function, which is deliberate: one place to read when asking what Dart can
/// possibly know.
fn project(app: &App) -> ClientView {
    let orbits = app.context().map(|context| context.orbits.as_slice());
    // Asked of the snapshot rather than assumed. Whether this build can
    // force-stop what it owns is a capability the supervisor answers, and an
    // interface that inferred it from ownership would offer a control the core
    // refuses.
    let capabilities = app
        .snapshot()
        .map(|snapshot| snapshot.capabilities.clone())
        .unwrap_or_default();

    ClientView {
        loading: app.is_loading(),
        stale: app.stale().map(|reason| match reason {
            StaleReason::NeverLoaded => Staleness::NeverLoaded,
            StaleReason::Signalled(why) => Staleness::Signalled(why.clone()),
        }),
        library: app.library().map(|entries| {
            entries
                .iter()
                .map(|entry| LibraryRow {
                    key: entry.world_mount.clone(),
                    world_mount: entry.world_mount.clone(),
                    world: entry.world.clone(),
                    installed: entry.installed,
                    display_name: entry.display_name.clone(),
                    opens_at: entry.entry_path.clone(),
                    version: entry.version,
                    tagline: entry.tagline.clone(),
                    accent: entry.accent,
                    people: world_people(app.book(), app.presence(), &entry.world),
                    update: app
                        .world_standing(&entry.world)
                        .map(|standing| WorldUpdateRow {
                            serving: standing.serving.clone(),
                            available: standing.available.clone(),
                            behind: standing.behind,
                            unmet: standing.unmet.clone(),
                            operation: standing.operation.clone(),
                            phase: standing.phase.clone(),
                            progress: standing.progress.clone(),
                            message: standing.message.clone(),
                        }),
                    install: app.world_install(&entry.world_mount).map(|progress| {
                        WorldInstallRow {
                            phase: progress.phase.clone(),
                            received: progress.received,
                            total: progress.total,
                        }
                    }),
                    channel: app
                        .world_standing(&entry.world)
                        .and_then(|standing| standing.channel.clone()),
                    source_dir: entry.source_dir.clone(),
                    source_standing: entry.source_standing.clone(),
                })
                .collect()
        }),
        host: app.context().map(|context| HostFacts {
            version: context.version.clone(),
            identity_home: context.identity_home.clone(),
            spaces_root: context.spaces_root.clone(),
            orbit_count: u32::try_from(context.orbits.len()).unwrap_or(u32::MAX),
        }),
        display: app.display().map(|display| DisplayFacts {
            instance: display.instance.clone(),
            label: display.label.clone(),
            origin: display.origin.clone(),
            certificate_sha256: display.certificate_sha256.clone(),
            certificate_pem: display.certificate_pem.clone(),
            surfaces: display
                .surfaces
                .iter()
                .map(|surface| DisplaySurfaceRow {
                    world: surface.world.clone(),
                    surface: surface.surface.clone(),
                    title: surface.title.clone(),
                    contract_version: surface.contract_version,
                    outputs: surface.outputs.clone(),
                })
                .collect(),
            devices: display
                .devices
                .iter()
                .map(|device| DisplayReceiverRow {
                    device: device.device.clone(),
                    label: device.label.clone(),
                    platform: device.platform.clone(),
                    build: device.build.clone(),
                    issued_at_unix_ms: device.issued_at_unix_ms,
                    revoked_at_unix_ms: device.revoked_at_unix_ms,
                    health: device.health.as_ref().map(|health| DisplayHealthRow {
                        revision: health.revision.clone(),
                        current_item: health.current_item.clone(),
                        elapsed_ms: health.elapsed_ms,
                        connection: health.connection.clone(),
                        playback: health.playback.clone(),
                        last_error: health.last_error.clone(),
                        staged_items: health.staged_items,
                        staged_bytes: health.staged_bytes,
                        drift_residual_ms: health.drift_residual_ms,
                        correction_events: health.correction_events,
                        pipeline_unobservable: health.pipeline_unobservable,
                        reported_at_unix_ms: health.reported_at_unix_ms,
                    }),
                })
                .collect(),
            assignments: display
                .assignments
                .iter()
                .map(|assignment| DisplayAssignmentRow {
                    assignment: assignment.assignment.clone(),
                    device: assignment.device.clone(),
                    orbit: assignment.orbit.clone(),
                    space: assignment.space.clone(),
                    program: assignment.program.clone(),
                    world: assignment.world.clone(),
                    surface: assignment.surface.clone(),
                    controller: assignment.controller.clone(),
                    theme: match assignment.theme {
                        lait::control::DisplayThemeSetting::Light => DisplayTheme::Light,
                        lait::control::DisplayThemeSetting::Dark => DisplayTheme::Dark,
                        lait::control::DisplayThemeSetting::HighContrast => {
                            DisplayTheme::HighContrast
                        }
                    },
                    sync_group: assignment.sync.as_ref().map(|sync| sync.group.clone()),
                    sync_mode: assignment.sync.as_ref().map(|sync| match sync.mode {
                        lait::control::DisplaySyncModeSetting::StayInSync => {
                            DisplaySyncMode::StayInSync
                        }
                        lait::control::DisplaySyncModeSetting::Positional => {
                            DisplaySyncMode::Positional
                        }
                    }),
                    static_delay_ms: assignment
                        .sync
                        .as_ref()
                        .map_or(0, |sync| sync.static_delay_ms),
                    expires_at_unix_ms: assignment.expires_at_unix_ms,
                    revoked_at_unix_ms: assignment.revoked_at_unix_ms,
                })
                .collect(),
            pending_pairings: display
                .pending_pairings
                .iter()
                .map(|pairing| DisplayPairingRow {
                    pairing: pairing.pairing.clone(),
                    confirmation_phrase: pairing.confirmation_phrase.clone(),
                    certificate_sha256: pairing.certificate_sha256.clone(),
                    platform: pairing.platform.clone(),
                    build: pairing.build.clone(),
                    created_at_unix_ms: pairing.created_at_unix_ms,
                    expires_at_unix_ms: pairing.expires_at_unix_ms,
                })
                .collect(),
            pending_rendezvous: display
                .pending_rendezvous
                .iter()
                .map(|minted| DisplayRendezvousRow {
                    rendezvous: minted.rendezvous.clone(),
                    code: minted.code.clone(),
                    site: minted.site.clone(),
                    label: minted.label.clone(),
                    assignment: minted.assignment.as_ref().map(|assignment| {
                        DisplayRendezvousAssignmentRow {
                            orbit: assignment.orbit.clone(),
                            world: assignment.world.clone(),
                            surface: assignment.surface.clone(),
                        }
                    }),
                    state: match minted.state {
                        lait::control::DisplayRendezvousState::Waiting => {
                            DisplayRendezvousState::Waiting
                        }
                        lait::control::DisplayRendezvousState::Connecting => {
                            DisplayRendezvousState::Connecting
                        }
                        lait::control::DisplayRendezvousState::Connected => {
                            DisplayRendezvousState::Connected
                        }
                    },
                    device: minted.device.clone(),
                    created_at_unix_ms: minted.created_at_unix_ms,
                    expires_at_unix_ms: minted.expires_at_unix_ms,
                })
                .collect(),
            identifier_custody: display.identifier_custody.as_ref().map(|custody| {
                DisplayIdentifierCustodyRow {
                    slots: custody.slots.clone(),
                    portable: custody.portable,
                }
            }),
            choices: app
                .display_choices()
                .map(|((orbit, _, _), listed)| DisplayChoicesRow {
                    orbit: orbit.clone(),
                    world: listed.world.clone(),
                    surface: listed.surface.clone(),
                    choices: listed.choices.as_ref().map(|choices| {
                        choices
                            .iter()
                            .map(|choice| DisplayChoiceRow {
                                id: choice.id.clone(),
                                title: choice.title.clone(),
                            })
                            .collect()
                    }),
                    unavailable: listed.unavailable.clone(),
                })
                .collect(),
        }),
        presentation: app.presentation().map(|presenting| PresentationFacts {
            chosen: presenting
                .selection
                .as_ref()
                .map(|selection| PresentationChoice {
                    orbit: selection.orbit.clone(),
                    world: selection.world.clone(),
                    surface: selection.surface.clone(),
                    title: selection.title.clone(),
                }),
            failure: presenting.failure.clone(),
            program: presenting.rendered.as_ref().map(|view| PresentedProgram {
                assessment: view.assessment.clone(),
                partial_reasons: view.partial_reasons.clone(),
                cycle: view.cycle.clone(),
                refresh_after_ms: view.refresh_after_ms,
                items: view
                    .items
                    .iter()
                    .map(|item| PresentedItem {
                        id: item.id.clone(),
                        duration_ms: item.duration_ms,
                        assessment: item.assessment.clone(),
                        spoken_summary: item.spoken_summary.clone(),
                        scene: match &item.scene {
                            lait::control::DisplayPresentationSceneView::Frame {
                                media_type,
                                width,
                                height,
                                bytes_base64,
                            } => PresentedScene::Frame {
                                media_type: media_type.clone(),
                                width: *width,
                                height: *height,
                                // Decoded once here rather than in Dart: the
                                // interface receives a whole view on every
                                // pump, and base64 is the transport's problem
                                // rather than the screen's.
                                bytes: data_encoding::BASE64
                                    .decode(bytes_base64.as_bytes())
                                    .unwrap_or_default(),
                            },
                            lait::control::DisplayPresentationSceneView::Blank { reason } => {
                                PresentedScene::Blank {
                                    reason: reason.clone(),
                                }
                            }
                            lait::control::DisplayPresentationSceneView::Unsupported { output } => {
                                PresentedScene::Unsupported {
                                    output: output.clone(),
                                }
                            }
                        },
                    })
                    .collect(),
            }),
        }),
        heads: app
            .heads()
            .iter()
            .map(|head| HeadRow {
                id: head.id.clone(),
                kind: format!("{:?}", head.kind).to_lowercase(),
                orbit: head.orbit.clone(),
                world: head.world.clone(),
                origin: head
                    .url
                    .as_deref()
                    .map(|url| url.split('?').next().unwrap_or(url).to_owned()),
                owned: matches!(head.ownership, lait_workbench::Ownership::Owned),
                state: match &head.state {
                    lait_workbench::HeadState::Running => "running".to_owned(),
                    lait_workbench::HeadState::Exited { .. } => "exited".to_owned(),
                    lait_workbench::HeadState::Unknown { .. } => "unknown".to_owned(),
                },
                state_detail: match &head.state {
                    lait_workbench::HeadState::Running => None,
                    lait_workbench::HeadState::Exited { status } => Some(status.clone()),
                    lait_workbench::HeadState::Unknown { why } => Some(why.clone()),
                },
            })
            .collect(),
        devices: app
            .devices()
            .iter()
            .map(|device| DeviceRow {
                id: device.id.clone(),
                label: device.label.clone(),
                state: format!("{:?}", device.state).to_lowercase(),
                owned: device.owned,
                home: device.home.clone(),
                pid: device.pid,
                can_force_stop: device.owned && capabilities.force_stop_owned_process,
                last_error: device.last_error.clone(),
                image_fingerprint: device.image.as_ref().map(|image| image.fingerprint.clone()),
                degraded: match device.observation.state {
                    lait_workbench::ObservationState::Degraded => Some(
                        device
                            .observation
                            .error
                            .clone()
                            .unwrap_or_else(|| "no reason given".into()),
                    ),
                    lait_workbench::ObservationState::Healthy => None,
                },
            })
            .collect(),
        storage: app
            .storage()
            .iter()
            .map(|facts| StorageRow {
                orbit: facts.orbit.clone(),
                name: facts.name.clone(),
                bytes_on_disk: facts.bytes_on_disk,
                object_count: facts.object_count,
                last_verified_ms: facts.last_verified_ms,
                missing: facts.missing.map(|missing| match missing {
                    crate::client::storage::Missing::NotPlaced => Missing::NotPlaced,
                    crate::client::storage::Missing::Unreachable => Missing::Unreachable,
                }),
            })
            .collect(),
        orbits: orbits
            .unwrap_or_default()
            .iter()
            .map(|orbit| OrbitRow {
                space: orbit.space.clone(),
                name: orbit.name.clone(),
                path: orbit.path.clone(),
                last_opened: (orbit.last_opened > 0).then_some(orbit.last_opened),
            })
            .collect(),
        space: app.space().map(|view| SpaceRow {
            space: view.space.clone(),
            whoami: view.standing.actor.clone(),
            admin: view.standing.role == "admin",
            members: view
                .members
                .iter()
                .map(|member| MemberRow {
                    id: member.key.clone(),
                    nick: Some(member.alias.clone()).filter(|alias| !alias.is_empty()),
                    authored_name: authored_name_for(app.book(), &member.key),
                    admin: member.role == "admin",
                })
                .collect(),
            devices: view
                .devices
                .iter()
                .map(|device| device.line.clone())
                .collect(),
            // Absent when the Space could not be diagnosed, which is not the
            // same as every gate passing.
            diagnosis: view.diagnosis.as_ref().map(|taken| DiagnosisRow {
                gates: taken
                    .gates
                    .iter()
                    .map(|gate| GateRow {
                        id: gate.id.clone(),
                        label: gate.label.clone(),
                        state: match gate.state {
                            lait::diagnose::GateState::Pass => GateState::Pass,
                            lait::diagnose::GateState::Wait => GateState::Wait,
                            lait::diagnose::GateState::Fail => GateState::Fail,
                            lait::diagnose::GateState::Warn => GateState::Warn,
                            lait::diagnose::GateState::Skip => GateState::Skip,
                        },
                        detail: gate.detail.clone(),
                    })
                    .collect(),
                blocked_on: taken.blocked_on.clone(),
                summary: taken.summary.clone(),
            }),
        }),
        book: app.book().map(|book| BookFacts {
            cards: book
                .cards
                .iter()
                .map(|card| CardRow {
                    card: card.card.clone(),
                    name: card.name.clone(),
                    note: card.note.clone(),
                    handles: card.handles.clone(),
                    addresses: card.addresses.clone(),
                    devices: card.devices.clone(),
                    agents: card.agents.clone(),
                    picture: card.picture.clone(),
                    groups: card.groups.clone(),
                    self_claim: card.self_claim,
                    presence: card_presence(app.presence(), card),
                })
                .collect(),
            migration_complete: book.migration_complete,
            migration_pending: u32::try_from(book.migration_pending).unwrap_or(u32::MAX),
            migration_imported: u32::try_from(book.migration_imported).unwrap_or(u32::MAX),
            suggestions: book
                .suggestions
                .iter()
                .map(|s| SuggestionRow {
                    suggestion: s.suggestion.clone(),
                    name: s.name.clone(),
                    note: s.note.clone(),
                    handles: s.handles.clone(),
                })
                .collect(),
        }),
        correspondence: app.correspondence().map(|corr| CorrespondenceFacts {
            my_device: corr.my_device.clone(),
            my_reach: corr.my_reach.clone(),
            me: corr.me.clone(),
            contacts: corr
                .contacts
                .iter()
                .map(|contact| ContactRow {
                    id: contact.id.clone(),
                    name: contact.name.clone(),
                    devices: contact.devices.clone(),
                    added: contact.added,
                    is_agent: contact.is_agent,
                    parent_id: contact.parent_id.clone(),
                    parent_name: contact.parent_name.clone(),
                    unread: contact.unread,
                })
                .collect(),
            conversations: corr
                .conversations
                .iter()
                .map(|conversation| ConversationRow {
                    peer_id: conversation.peer_id.clone(),
                    peer_name: conversation.peer_name.clone(),
                    messages: conversation
                        .messages
                        .iter()
                        .map(|message| ChatMessageRow {
                            invitation: message.invitation.clone(),
                            id: message.id.clone(),
                            mine: message.mine,
                            kind: message.kind.clone(),
                            body: message.body.clone(),
                            sent_at: message.sent_at,
                            from_device: message.from_device.clone(),
                            provenance_agrees: message.provenance_agrees,
                        })
                        .collect(),
                })
                .collect(),
            open_tabs: corr.open_tabs.clone(),
            active_tab: corr.active_tab.clone(),
        }),
        // Joined here from two reads of the one identity: the device set is
        // the reach view's and the waiting code and offers are orientation's.
        // Joining at the boundary rather than in the model keeps each read
        // the only writer of what it measured.
        profile: app.profile().map(|profile| {
            // A device's certification names the marker's device; a person is
            // shown where the marker answers. Joined once here rather than at
            // every row, and only for markers the book still weighs.
            let named: std::collections::BTreeMap<&str, &str> = profile
                .markers
                .iter()
                .filter_map(|marker| marker.by.as_deref().map(|by| (by, marker.base.as_str())))
                .collect();
            ProfileFacts {
                profile: profile.profile.clone(),
                me: profile.me.clone(),
                origin: profile.origin.as_ref().map(|origin| match origin {
                    lait::control::OriginView::Founded => OriginRow::Founded,
                    lait::control::OriginView::Adopted { from, at } => OriginRow::Adopted {
                        from: from.clone(),
                        at: *at,
                    },
                }),
                devices: profile
                    .devices
                    .iter()
                    .map(|device| OwnDeviceRow {
                        device: device.device.clone(),
                        me: device.me,
                        liveness: match &device.liveness {
                            lait::control::Liveness::Reported { version, at } => {
                                LivenessRow::Answered {
                                    version: version.clone(),
                                    at: *at,
                                }
                            }
                            lait::control::Liveness::CouldNotAsk { why } => {
                                LivenessRow::CouldNotAsk { why: why.clone() }
                            }
                            lait::control::Liveness::NotProbed => LivenessRow::NotProbed,
                        },
                        held: device.held.clone(),
                        // Resolved to the name the marker rows carry, so the
                        // surface joins on something a person can be shown. A
                        // certification by a marker the book no longer weighs is
                        // dropped: this reader's book is what this reader trusts,
                        // and a chip naming a marker with no row would be a
                        // claim with nothing behind it.
                        certified_by: device
                            .certified_by
                            .iter()
                            .filter_map(|by| named.get(by.as_str()).map(|base| (*base).to_owned()))
                            .collect(),
                        reach: device.reach.as_ref().map(|reach| match reach {
                            lait::control::ReachKind::Connected { via } => {
                                ReachRow::Connected { via: via.clone() }
                            }
                            lait::control::ReachKind::Dialing => ReachRow::Dialing,
                            lait::control::ReachKind::NoRoute => ReachRow::NoRoute,
                            lait::control::ReachKind::Unreachable { since } => {
                                ReachRow::Unreachable { since: *since }
                            }
                            lait::control::ReachKind::Retired => ReachRow::Retired,
                        }),
                    })
                    .collect(),
                device_set_unknown: profile.device_set_unknown,
                interface: profile.interface.as_ref().map(|interface| match interface {
                    lait::control::InterfaceView::Up { name, address } => InterfaceRow::Up {
                        name: name.clone(),
                        address: address.clone(),
                    },
                    lait::control::InterfaceView::NotPermitted => InterfaceRow::NotPermitted,
                    lait::control::InterfaceView::Unsupported => InterfaceRow::Unsupported,
                    lait::control::InterfaceView::Off => InterfaceRow::Off,
                }),
                pairing: app
                    .context()
                    .and_then(|context| context.pairing.as_ref())
                    .map(|pairing| PairingCodeRow {
                        code: pairing.code.clone(),
                        direct: pairing
                            .direct
                            .iter()
                            .map(std::string::ToString::to_string)
                            .collect(),
                        expires_at_ms: pairing.expires_at_ms,
                    }),
                offers: app
                    .context()
                    .map(|context| context.pair_offers.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|offer| PairOfferRow {
                        pairing: offer.pairing.clone(),
                        device: offer.device.clone(),
                        name: offer.name.clone(),
                        phrase: offer.phrase.clone(),
                        expires_at_ms: offer.expires_at_ms,
                    })
                    .collect(),
                spaces: profile
                    .spaces
                    .iter()
                    .map(|space| SpaceFanoutRow {
                        space: space.space.clone(),
                        on: space.on.clone(),
                        standings: space
                            .standings
                            .iter()
                            .map(|row| DeviceStandingRow {
                                device: row.device.clone(),
                                standing: match &row.standing {
                                    lait::control::FanoutStanding::Held => StandingRow::Held,
                                    lait::control::FanoutStanding::Declined { why } => {
                                        StandingRow::Declined { why: why.clone() }
                                    }
                                    lait::control::FanoutStanding::Refused { why } => {
                                        StandingRow::Refused { why: why.clone() }
                                    }
                                    lait::control::FanoutStanding::Deferred { why } => {
                                        StandingRow::Deferred { why: why.clone() }
                                    }
                                    lait::control::FanoutStanding::CouldNotAsk { why, .. } => {
                                        StandingRow::CouldNotAsk { why: why.clone() }
                                    }
                                    lait::control::FanoutStanding::Excluded { told } => {
                                        StandingRow::Excluded { told: *told }
                                    }
                                },
                            })
                            .collect(),
                    })
                    .collect(),
                markers: profile
                    .markers
                    .iter()
                    .map(|marker| MarkerRow {
                        marker: marker.base.clone(),
                        standing: match &marker.standing {
                            None => MarkerStandingRow::NeverAsked,
                            Some(standing) => match standing {
                                lait::control::MarkerStandingView::Pinned
                                | lait::control::MarkerStandingView::Unchanged
                                | lait::control::MarkerStandingView::Extended => {
                                    MarkerStandingRow::Answering
                                }
                                lait::control::MarkerStandingView::CouldNotAsk { .. } => {
                                    MarkerStandingRow::CouldNotAsk
                                }
                                lait::control::MarkerStandingView::WrongSigner => {
                                    MarkerStandingRow::AnsweredAsAnother
                                }
                                lait::control::MarkerStandingView::Rollback => {
                                    MarkerStandingRow::AnsweredOlder
                                }
                                lait::control::MarkerStandingView::Unproven => {
                                    MarkerStandingRow::Unproven
                                }
                                lait::control::MarkerStandingView::Diverged => {
                                    MarkerStandingRow::Contradicted
                                }
                                lait::control::MarkerStandingView::Refused { .. } => {
                                    MarkerStandingRow::Unreadable
                                }
                            },
                        },
                    })
                    .collect(),
            }
        }),
        notices: app
            .notices()
            .map(|notice| NoticeRow {
                said: notice.said.clone(),
                launched: notice.launched.as_ref().map(|ticket| ticket.url.clone()),
            })
            .collect(),
        failures: app
            .failures()
            .map(|failure| FailureRow {
                what: failure.what.clone(),
                error: failure.error.to_string(),
                retryable: failure.error.retryable,
                key: failure.key.clone(),
            })
            .collect(),
        in_flight: app.in_flight_keys(),
        mcp: app.mcp().map(|outcome| McpBindingRow {
            path: outcome.path.clone(),
            detail: outcome.detail.clone(),
            note: outcome.note.clone(),
            replaced: outcome.replaced,
            agent: outcome.agent.clone(),
            written: outcome.written,
            world: outcome.world.clone(),
        }),
        image: app.image().map(|image| ImageRow {
            fingerprint: image.fingerprint.clone(),
            staged_at_ms: image.staged_at_ms,
            source_changed: image.source_changed,
        }),
        // Derived here rather than held in the model, because the decision
        // depends on what is in flight *at the moment it is asked* — and this
        // is the moment, with the same list the view is about to report. A
        // stored decision would be one answered against a different machine.
        update: match crate::client::update::intent(
            app.update_standing(),
            crate::runtime::now_secs(),
            &app.in_flight_keys(),
            crate::client::update::running_version(),
        ) {
            crate::client::update::Intent::Nothing => None,
            crate::client::update::Intent::RestartRequested { version, urgency } => {
                Some(UpdateRow::RestartRequested {
                    version,
                    urgency: match urgency {
                        crate::client::update::Urgency::Quiet => UpdateUrgency::Quiet,
                        crate::client::update::Urgency::Insistent => UpdateUrgency::Insistent,
                        crate::client::update::Urgency::Urgent => UpdateUrgency::Urgent,
                    },
                })
            }
            crate::client::update::Intent::Waiting { version, why } => {
                let crate::client::update::Held::WorkInFlight { what } = why;
                Some(UpdateRow::Waiting {
                    version,
                    holding: what,
                })
            }
            crate::client::update::Intent::Attention { why } => Some(UpdateRow::Attention { why }),
        },
        exited: app.exit().is_some(),
    }
}

/// Measured presence for one card, joined over its handles.
///
/// A person online on one device is online, so the best observation wins.
/// `Offline` is only ever produced from a measurement: a Space that answered
/// with nothing speaking for the actor. A card none of whose Spaces could be
/// asked, and none of whose devices any registry has seen, answers `None` —
/// the "could not be asked" absence, kept apart from `Offline` all the way
/// to the wire. Local-agent handles measure nothing here: they name a
/// runtime on this machine, not a peer.
fn card_presence(
    presence: Option<&crate::client::presence::PresenceMap>,
    card: &crate::client::book::CardFacts,
) -> Option<PresenceView> {
    use crate::client::presence::Reach;
    let map = presence?;
    let mut best: Option<Reach> = None;
    let mut raise = |reach: Reach, best: &mut Option<Reach>| {
        *best = Some(best.map_or(reach, |held| held.max(reach)));
    };
    for address in &card.addresses {
        let Some((space, actor)) = actor_address(address) else {
            continue;
        };
        if let Some(reach) = map.actors.get(&(space.to_owned(), actor.to_owned())) {
            raise(*reach, &mut best);
        } else if map.asked.contains(space) {
            raise(Reach::Offline, &mut best);
        }
    }
    for device in &card.devices {
        // A device absent from every registry stays unmeasured: devices are
        // not Space-scoped, so no answered Space can vouch for their absence.
        if let Some(reach) = map.devices.get(device) {
            raise(*reach, &mut best);
        }
    }
    best.map(view_of)
}

/// The `(space, actor)` of an `actor:` wire spelling, or nothing. The one
/// place this side reads a handle's shape — Dart is never handed a spelling
/// it would have to parse.
fn actor_address(address: &str) -> Option<(&str, &str)> {
    let mut parts = address.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("actor"), Some(space), Some(actor)) => Some((space, actor)),
        _ => None,
    }
}

fn view_of(reach: crate::client::presence::Reach) -> PresenceView {
    use crate::client::presence::Reach;
    match reach {
        Reach::Online => PresenceView::Online,
        Reach::Away => PresenceView::Away,
        Reach::Offline => PresenceView::Offline,
    }
}

/// The book ∩ one Space: every non-self card holding an address there, with
/// presence measured in that Space alone — a person online elsewhere is not
/// online here. `None` before the book has been read; the distinction
/// between "unread" and "nobody addressed here" survives to the wire.
fn world_people(
    book: Option<&crate::client::book::BookSnapshot>,
    presence: Option<&crate::client::presence::PresenceMap>,
    world: &str,
) -> Option<Vec<WorldPersonRow>> {
    use crate::client::presence::Reach;
    let book = book?;
    Some(
        book.cards
            .iter()
            .filter(|card| !card.self_claim)
            .filter_map(|card| {
                // Every Space this card holds an address in. The row is a
                // World across all of them, so the glance aggregates: a person
                // is as present as their most reachable address, anywhere.
                let addresses: Vec<(&str, &str)> = card
                    .addresses
                    .iter()
                    .filter_map(|address| actor_address(address))
                    .collect();
                if addresses.is_empty() {
                    return None;
                }
                let mut best: Option<Reach> = None;
                let mut here = false;
                if let Some(map) = presence {
                    for (space, actor) in &addresses {
                        let reach = map
                            .actors
                            .get(&((*space).to_owned(), (*actor).to_owned()))
                            .copied()
                            .or_else(|| map.asked.contains(*space).then_some(Reach::Offline));
                        if let Some(reach) = reach {
                            best = Some(best.map_or(reach, |held| held.max(reach)));
                        }
                        // In THIS World, in any Space that serves it. A person
                        // with a different World open is holding, not here.
                        here |= map
                            .in_world
                            .iter()
                            .any(|(s, w, a)| s == space && a == actor && w == world);
                    }
                }
                Some(WorldPersonRow {
                    name: card.name.clone(),
                    picture: card.picture.clone(),
                    presence: best.map(view_of),
                    agent: card
                        .groups
                        .iter()
                        .any(|group| group == lait::control::AGENT_GROUP),
                    here,
                })
            })
            .collect(),
    )
}

fn authored_name_for(
    book: Option<&crate::client::book::BookSnapshot>,
    member_id: &str,
) -> Option<String> {
    let book = book?;
    book.cards
        .iter()
        .find(|card| {
            card.handles
                .iter()
                .any(|handle| handle == member_id || handle.ends_with(&format!(":{member_id}")))
        })
        .map(|card| card.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::library::LibraryEntry;
    use crate::runtime::{Action, Update};

    /// The client's own update decision crosses as a decision, and it is made
    /// here — against the same `in_flight` the view reports, which is the
    /// whole reason it is derived in the projection rather than stored.
    ///
    /// The policy itself is `client::update`'s and is tested there. What this
    /// pins is the wiring, which was the part that did not exist: the module
    /// was complete, correct and reachable from nothing, and a dead-code
    /// warning on one of its constants was the only thing saying so.
    #[test]
    fn a_staged_release_crosses_as_a_restart_request_and_yields_to_work_in_flight() {
        use lait::update::watch::Standing;

        let mut app = App::new();
        app.apply(Update::UpdateStanding(Some(Standing::Staged {
            version: "0.9.1".into(),
            at: crate::runtime::now_secs(),
        })));

        let Some(UpdateRow::RestartRequested { version, urgency }) = project(&app).update else {
            panic!("a staged release did not ask for a restart");
        };
        assert_eq!(version, "0.9.1");
        assert_eq!(
            urgency,
            UpdateUrgency::Quiet,
            "a release staged moments ago was drawn louder than quiet"
        );

        // Something in flight holds the restart, and says so rather than
        // asking for one that would lose the work.
        app.dispatched(&Action::Refresh);
        let Some(UpdateRow::Waiting { holding, .. }) = project(&app).update else {
            panic!("work in flight did not hold the restart");
        };
        assert!(!holding.is_empty(), "a hold named nothing holding it");
    }

    /// A signature that did not verify is not an update and is not silence.
    /// Folding it into either is the quiet the attack is buying.
    #[test]
    fn a_refused_pointer_crosses_as_attention_rather_than_as_no_update() {
        let mut app = App::new();
        app.apply(Update::UpdateStanding(Some(
            lait::update::watch::Standing::Refused {
                why: "feed signature verification failed".into(),
            },
        )));
        let Some(UpdateRow::Attention { why }) = project(&app).update else {
            panic!("a refused pointer was drawn as nothing to see");
        };
        assert!(
            why.contains("verification"),
            "the reason was not carried: {why}"
        );
    }

    /// A machine that has never completed a check says nothing — which is not
    /// "up to date", and is why this is an `Option` rather than a state.
    #[test]
    fn a_machine_that_has_never_checked_crosses_as_absent() {
        assert!(project(&App::new()).update.is_none());
    }

    /// Catalog and installation state cross the bridge as distinct facts,
    /// keyed by mount, with an undeclared entry path travelling as absent
    /// rather than as a guessed `/`.
    #[test]
    fn the_library_crosses_as_the_declaration_keyed_by_mount() {
        let mut app = App::new();
        app.absorb_library(vec![
            LibraryEntry {
                world_mount: "issues".into(),
                world: "com.lait.issues".into(),
                installed: true,
                display_name: "Issues".into(),
                entry_path: Some("/".into()),
                chrome: Default::default(),
                tagline: Some("Track the work".into()),
                accent: Some(0x00AA_66FF),
                version: Some(7),
                source_dir: None,
                source_standing: None,
            },
            LibraryEntry {
                world_mount: "notes".into(),
                world: "com.lait.notes".into(),
                installed: false,
                display_name: "Notes".into(),
                entry_path: None,
                chrome: Default::default(),
                tagline: None,
                accent: None,
                version: None,
                source_dir: None,
                source_standing: None,
            },
        ]);

        let rows = project(&app).library.expect("a library was read");
        assert_eq!(rows[0].key, "issues");
        assert_eq!(rows[0].display_name, "Issues");
        assert_eq!(rows[0].opens_at.as_deref(), Some("/"));
        assert_eq!(rows[0].version, Some(7));
        assert!(rows[0].installed);
        assert!(!rows[1].installed, "a catalog row crossed as installed");
        assert_eq!(
            rows[1].opens_at, None,
            "`/` was guessed on a World's behalf"
        );
    }

    /// Facts cross; words do not. A row that cannot be opened says *which*
    /// kind of cannot, and the sentence explaining it is Dart's business.
    /// Loading is not empty, and it survives the crossing as its own fact.
    #[test]
    fn the_book_crosses_as_authored_cards_not_as_reachability() {
        let mut app = App::new();
        app.absorb_book(crate::client::book::BookSnapshot {
            cards: vec![crate::client::book::CardFacts {
                card: "crd_one".into(),
                name: "Ada".into(),
                note: "colleague".into(),
                handles: vec!["actor:ws_one:act_ada".into()],
                addresses: vec!["actor:ws_one:act_ada".into()],
                devices: Vec::new(),
                agents: Vec::new(),
                picture: None,
                groups: Vec::new(),
                self_claim: true,
            }],
            migration_complete: false,
            migration_pending: 1,
            migration_imported: 0,
            suggestions: vec![crate::client::book::SuggestionFacts {
                suggestion: "sug_abc".into(),
                name: "Grace".into(),
                note: String::new(),
                handles: Vec::new(),
            }],
        });
        let view = project(&app);
        let book = view.book.expect("the book was read");
        assert_eq!(book.cards.len(), 1);
        assert_eq!(book.cards[0].name, "Ada");
        assert_eq!(book.cards[0].handles[0], "actor:ws_one:act_ada");
        assert!(book.cards[0].self_claim);
        assert_eq!(book.migration_pending, 1);
        assert_eq!(book.suggestions.len(), 1, "staged suggestions cross whole");
        assert_eq!(book.suggestions[0].suggestion, "sug_abc");
        assert_eq!(
            authored_name_for(app.book(), "act_ada").as_deref(),
            Some("Ada")
        );
        assert_eq!(
            authored_name_for(app.book(), "Ada"),
            None,
            "a Card name is not a handle"
        );
    }

    #[test]
    fn loading_and_empty_are_told_apart_across_the_bridge() {
        let loading = project(&App::new());
        assert!(loading.loading);
        assert_eq!(loading.stale, Some(Staleness::NeverLoaded));
        assert!(loading.library.is_none());

        let mut read = App::new();
        read.absorb_library(Vec::new());
        let read = project(&read);
        assert_eq!(
            read.library,
            Some(Vec::new()),
            "a device that answered and serves nothing looks unread"
        );
    }

    /// A second isolate attaches; a second *boot* against a different
    /// identity is refused. The paths are the whole identity: two
    /// supervisors of one device set is the failure this exists to stop.
    #[test]
    fn a_second_boot_against_a_different_identity_is_refused() {
        let home = Path::new("C:/ident");
        let sidecar = Path::new("C:/lait.exe");
        assert!(attach_paths((home, sidecar), (home, sidecar)).is_ok());
        let err = attach_paths((home, sidecar), (Path::new("C:/other"), sidecar))
            .expect_err("a different identity is a second boot");
        assert!(
            err.contains("second boot"),
            "the refusal must name the second boot, got: {err}"
        );
    }

    struct FakeSink {
        views: std::sync::Arc<Mutex<Vec<ClientView>>>,
        live: bool,
    }

    impl ViewPush for FakeSink {
        fn push(&self, view: &ClientView) -> bool {
            if !self.live {
                return false;
            }
            self.views.lock().expect("views").push(view.clone());
            true
        }
    }

    fn fake(live: bool) -> (FakeSink, std::sync::Arc<Mutex<Vec<ClientView>>>) {
        let views = std::sync::Arc::new(Mutex::new(Vec::new()));
        (
            FakeSink {
                views: views.clone(),
                live,
            },
            views,
        )
    }

    /// Every attached isolate receives the same view on every pump.
    #[test]
    fn every_watcher_receives_the_same_view() {
        let (a, a_views) = fake(true);
        let (b, b_views) = fake(true);
        let mut watchers = Watchers::new();
        let first = empty();
        watchers.attach(a, first.clone());
        watchers.attach(b, first.clone());
        let next = {
            let mut view = empty();
            view.loading = false;
            view
        };
        watchers.emit(&next);
        let a_views = a_views.lock().expect("a");
        let b_views = b_views.lock().expect("b");
        assert_eq!(a_views.as_slice(), &[first.clone(), next.clone()]);
        assert_eq!(b_views.as_slice(), &[first, next]);
    }

    /// A sink that goes away is dropped and does not stall the rest.
    #[test]
    fn a_dead_watcher_is_dropped_and_the_rest_keep_pumping() {
        let (dead, dead_views) = fake(false);
        let (live, live_views) = fake(true);
        let mut watchers = Watchers::new();
        watchers.attach(dead, empty());
        watchers.attach(live, empty());
        assert_eq!(
            dead_views.lock().expect("dead").len(),
            0,
            "a dead sink must not be kept"
        );
        assert_eq!(live_views.lock().expect("live").len(), 1);

        let next = {
            let mut view = empty();
            view.loading = false;
            view
        };
        watchers.emit(&next);
        assert_eq!(watchers.len(), 1);
        assert_eq!(live_views.lock().expect("live").len(), 2);
    }

    /// The at-a-glance panel is the book joined to one World across every
    /// Space it is served in: self excluded, presence aggregated to a card's
    /// most reachable address, `here` scoped to THIS World alone, and the two
    /// absences — an unread book and an unasked Space — kept apart from a
    /// measured Offline.
    #[test]
    fn the_glance_is_the_book_joined_to_one_world() {
        fn card(
            id: &str,
            name: &str,
            addresses: Vec<String>,
            groups: Vec<String>,
            self_claim: bool,
        ) -> crate::client::book::CardFacts {
            crate::client::book::CardFacts {
                card: id.into(),
                name: name.into(),
                note: String::new(),
                handles: addresses.clone(),
                addresses,
                devices: Vec::new(),
                agents: Vec::new(),
                picture: None,
                groups,
                self_claim,
            }
        }
        let book = crate::client::book::BookSnapshot {
            cards: vec![
                card(
                    "crd_me",
                    "Me",
                    vec!["actor:ws_one:act_me".into()],
                    vec![],
                    true,
                ),
                card(
                    "crd_moon",
                    "Moon",
                    vec!["actor:ws_one:act_moon".into()],
                    vec![],
                    false,
                ),
                card(
                    "crd_claude",
                    "claude",
                    vec!["actor:ws_one:act_claude".into()],
                    vec![lait::control::AGENT_GROUP.into()],
                    false,
                ),
                card(
                    "crd_far",
                    "Far",
                    vec!["actor:ws_two:act_far".into()],
                    vec![],
                    false,
                ),
            ],
            migration_complete: true,
            migration_pending: 0,
            migration_imported: 0,
            suggestions: Vec::new(),
        };
        let mut presence = crate::client::presence::PresenceMap::default();
        presence.asked.insert("ws_one".into());
        presence.actors.insert(
            ("ws_one".into(), "act_moon".into()),
            crate::client::presence::Reach::Online,
        );
        presence
            .in_world
            .insert(("ws_one".into(), "wrl_issues".into(), "act_moon".into()));

        let people =
            world_people(Some(&book), Some(&presence), "wrl_issues").expect("book was read");
        assert_eq!(
            people.len(),
            3,
            "self is excluded; every addressed Space counts"
        );
        let moon = people.iter().find(|p| p.name == "Moon").expect("moon");
        assert_eq!(moon.presence, Some(PresenceView::Online));
        assert!(!moon.agent);
        assert!(moon.here, "a World-scoped Live row is being here");
        let agent = people.iter().find(|p| p.name == "claude").expect("claude");
        assert!(agent.agent);
        assert_eq!(
            agent.presence,
            Some(PresenceView::Offline),
            "asked and absent is a measurement"
        );
        assert!(!agent.here);
        let far = people.iter().find(|p| p.name == "Far").expect("far");
        assert_eq!(
            far.presence, None,
            "an unasked Space is unmeasured, never offline"
        );
        assert!(!far.here);

        // A different World is not this one, whichever Space serves it.
        let elsewhere = world_people(Some(&book), Some(&presence), "wrl_other").expect("read");
        assert!(
            !elsewhere
                .iter()
                .find(|p| p.name == "Moon")
                .expect("moon")
                .here
        );

        assert!(
            world_people(None, None, "wrl_issues").is_none(),
            "an unread book joins to nothing"
        );
    }

    #[test]
    fn an_unknown_mcp_client_is_refused_rather_than_signed_as_claude() {
        let error = ActionRequest::InstallMcp {
            client: "grok".into(),
            scope: None,
            name: "lait-issues".into(),
            agent: None,
            no_agent: false,
            project: "D:/work".into(),
            world: Some("issues".into()),
            preview: true,
        }
        .into_action()
        .expect_err("an unknown client became Claude");
        assert!(
            error.contains("grok") && error.contains("claude"),
            "{error}"
        );

        let ok = ActionRequest::InstallMcp {
            client: "cursor".into(),
            scope: None,
            name: "lait-issues".into(),
            agent: None,
            no_agent: false,
            project: "D:/work".into(),
            world: Some("issues".into()),
            preview: false,
        }
        .into_action()
        .expect("a known client was refused");
        assert!(matches!(ok, Action::InstallMcp { .. }));
    }

    /// The Devices surface is a join of two reads, and both halves have to
    /// survive the crossing: the device set comes from the reach view, the
    /// waiting code and the offers from orientation. A projection that took
    /// only one of them would draw a person's devices with no way to add one,
    /// or a code with nobody to add it to.
    #[test]
    fn a_profile_crosses_with_its_devices_and_the_code_it_is_waiting_on() {
        use crate::client::reach::ProfileSnapshot;
        use lait::control::{
            DeviceStanding, FanoutStanding, Liveness, MarkerStandingView, MarkerView, OriginView,
            OwnDeviceView, PairOffer, PairingCode, SpaceFanout,
        };

        let mut app = App::new();
        let mut context = crate::client::host::HostContext {
            version: "0.9.3".into(),
            identity_home: "home".into(),
            spaces_root: "spaces".into(),
            worlds: Vec::new(),
            identities: Vec::new(),
            orbits: Vec::new(),
            asks: Vec::new(),
            pairing: None,
            pair_offers: Vec::new(),
        };
        context.pairing = Some(PairingCode {
            code: "ABCD-EFGH".into(),
            direct: vec!["127.0.0.1:7717".parse().expect("a socket address")],
            expires_at_ms: 900_000,
        });
        context.pair_offers = vec![PairOffer {
            pairing: "pai_7".into(),
            device: "dev_pi".into(),
            name: "raspberrypi".into(),
            phrase: "amber basil cedar delta ember flint"
                .split(' ')
                .map(str::to_owned)
                .collect(),
            expires_at_ms: 600_000,
        }];
        app.apply(Update::Context(Box::new(context)));
        app.apply(Update::Profile(Box::new(ProfileSnapshot {
            profile: "prf_1".into(),
            me: Some("dev_laptop".into()),
            origin: Some(OriginView::Founded),
            devices: vec![
                OwnDeviceView {
                    device: "dev_laptop".into(),
                    me: true,
                    liveness: Liveness::Reported {
                        version: "0.9.3".into(),
                        at: 5,
                    },
                    held: vec!["spc_a".into(), "spc_b".into()],
                    certified_by: vec!["dev_marker".into()],
                    reach: None,
                },
                OwnDeviceView {
                    device: "dev_pi".into(),
                    me: false,
                    liveness: Liveness::CouldNotAsk {
                        why: "no route".into(),
                    },
                    held: Vec::new(),
                    certified_by: Vec::new(),
                    reach: Some(lait::control::ReachKind::Unreachable { since: 12 }),
                },
            ],
            device_set_unknown: false,
            markers: vec![
                MarkerView {
                    base: "https://post.example".into(),
                    by: Some("dev_marker".into()),
                    standing: Some(MarkerStandingView::Extended),
                    checked_at: Some(9),
                },
                MarkerView {
                    base: "https://quiet.example".into(),
                    by: None,
                    standing: None,
                    checked_at: None,
                },
            ],
            interface: Some(lait::control::InterfaceView::NotPermitted),
            // Three Spaces, three reasons the Pi does not hold one: a person
            // said so, the Pi could not be asked, and nothing has offered it
            // yet — the last of which has no row at all.
            spaces: vec![
                SpaceFanout {
                    space: "spc_a".into(),
                    on: vec!["dev_laptop".into()],
                    standings: vec![DeviceStanding {
                        device: "dev_pi".into(),
                        standing: FanoutStanding::Excluded { told: true },
                    }],
                },
                SpaceFanout {
                    space: "spc_b".into(),
                    on: vec!["dev_laptop".into()],
                    standings: vec![DeviceStanding {
                        device: "dev_pi".into(),
                        standing: FanoutStanding::CouldNotAsk {
                            why: "no route".into(),
                            retry_at_ms: 99,
                        },
                    }],
                },
                SpaceFanout {
                    space: "spc_c".into(),
                    on: vec!["dev_laptop".into()],
                    standings: Vec::new(),
                },
            ],
        })));

        let profile = project(&app).profile.expect("the profile did not cross");
        assert_eq!(profile.me.as_deref(), Some("dev_laptop"));
        assert_eq!(profile.origin, Some(OriginRow::Founded));
        assert_eq!(profile.devices.len(), 2);
        assert_eq!(profile.devices[0].held, ["spc_a", "spc_b"]);
        // A device that could not be asked keeps its own kind of absence all
        // the way to the surface: it is neither answered nor unprobed, and
        // above all it is not missing from the list.
        assert_eq!(
            profile.devices[1].liveness,
            LivenessRow::CouldNotAsk {
                why: "no route".into()
            },
        );
        assert_eq!(
            profile.pairing.as_ref().map(|code| code.code.as_str()),
            Some("ABCD-EFGH"),
        );
        assert_eq!(
            profile.pairing.expect("a code").direct,
            ["127.0.0.1:7717"],
            "the addresses to enter beside the code were dropped",
        );
        assert_eq!(profile.offers.len(), 1);
        assert_eq!(profile.offers[0].phrase.len(), 6);
        // The three absences cross as three different things. Folding any two
        // would tell a person a machine refused a Space it was never offered,
        // or that a decision they made is a network fault.
        let standing = |space: &str| {
            profile
                .spaces
                .iter()
                .find(|row| row.space == space)
                .expect("the Space row crossed")
                .standings
                .iter()
                .find(|row| row.device == "dev_pi")
                .map(|row| row.standing.clone())
        };
        assert_eq!(
            standing("spc_a"),
            Some(StandingRow::Excluded { told: true })
        );
        assert_eq!(
            standing("spc_b"),
            Some(StandingRow::CouldNotAsk {
                why: "no route".into()
            })
        );
        assert_eq!(
            standing("spc_c"),
            None,
            "a Space nothing has offered yet must have no standing at all"
        );
        // The tier crosses named the way the marker rows name it, so a
        // surface can join the two without holding a second table.
        assert_eq!(profile.devices[0].certified_by, ["https://post.example"]);
        // A device no marker has recorded is not a problem and carries no
        // finding — it simply says nothing.
        assert!(profile.devices[1].certified_by.is_empty());
        // And a marker nothing has asked yet is its own standing, never
        // folded into the one that could not be reached.
        assert_eq!(
            profile
                .markers
                .iter()
                .map(|marker| (marker.marker.as_str(), marker.standing))
                .collect::<Vec<_>>(),
            [
                ("https://post.example", MarkerStandingRow::Answering),
                ("https://quiet.example", MarkerStandingRow::NeverAsked),
            ],
        );
        // A machine that will not give an interface is a fact about the
        // machine, and a device nothing has routed to is not the same as one
        // that was dialed and did not answer. Both cross whole.
        assert_eq!(profile.interface, Some(InterfaceRow::NotPermitted));
        assert_eq!(profile.devices[0].reach, None);
        assert_eq!(
            profile.devices[1].reach,
            Some(ReachRow::Unreachable { since: 12 })
        );
    }

    /// A device set nobody has restored is not a profile with no devices.
    #[test]
    fn an_unread_device_set_crosses_as_unread_rather_than_as_none() {
        use crate::client::reach::ProfileSnapshot;

        let mut app = App::new();
        assert!(
            project(&app).profile.is_none(),
            "a profile appeared before anything read one",
        );
        app.apply(Update::Profile(Box::new(ProfileSnapshot {
            profile: "prf_1".into(),
            device_set_unknown: true,
            ..ProfileSnapshot::default()
        })));
        let profile = project(&app).profile.expect("the profile did not cross");
        assert!(profile.devices.is_empty());
        assert!(profile.device_set_unknown);
    }
}
