#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "The control wire codec validates frame lengths before bounded byte operations; these conversions preserve the existing versioned JSON and content encodings."
)]

//! Layer B — the local control protocol. Newline-delimited JSON over
//! the cross-platform local IPC channel (a Unix-domain socket on unix, a named
//! pipe on Windows; see [`control_name`]). One request → one response, plus the
//! streaming [`Request::Subscribe`] mode that writes [`Doorbell`] frames until
//! the client disconnects.
//!
//! This is the stable, versioned host façade for daemon, Space, Mechanics,
//! Station, Observation, and lifecycle operations. Product commands and
//! responses travel separately in opaque [`Call`] / [`Reply`]
//! envelopes owned by installed client packages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

pub use crate::daemon::scope::OrbitAddress;

use anyhow::{anyhow, Context, Result};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    Name,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::diagnose::DiagnosisView;
use crate::dto::{MemberDto, MemberLogEntry, SeedDto};
use runtime::poison::LockRecovering;
use runtime::world::call::{Call, Reply};

/// The identity-scoped local control service.
///
/// `Endpoint` owns listener framing, connection tasks, content streaming, and
/// delegation. Orbit catalog, placement, Station, and World state remain owned
/// by [`crate::orbits::Router`].
pub struct Endpoint {
    listener: Arc<crate::daemon::host::Listener>,
}

impl Endpoint {
    pub(crate) fn new(
        router: Arc<crate::orbits::Router>,
        display: Arc<crate::display::DisplayRuntime>,
    ) -> Self {
        Self {
            listener: Arc::new(crate::daemon::host::Listener::new(router, display)),
        }
    }

    pub(crate) fn begin_stop(&self) {
        self.listener.begin_stop();
    }

    pub(crate) fn subscribe_stop(&self) -> tokio::sync::watch::Receiver<bool> {
        self.listener.subscribe_stop()
    }

    pub(crate) async fn serve(&self, home: &Path) -> Result<()> {
        self.listener.clone().serve(home).await
    }
}

/// The control-plane protocol version this build **speaks** — the head ↔ daemon
/// channel, exchanged in the [`Request::Hello`] handshake.
///
/// The third plane to get one. The sync plane has [`crate::sync::PROTOCOL_VERSION`]
/// and the store has `dto::SCHEMA_VERSION`; the control channel had nothing, so a
/// client meeting a daemon of another vintage found out by failing to decode its
/// answer — which `ensure_daemon` read as "no daemon", spawned a doomed second one
/// over the held lock, and finally blamed a timeout. Same rules as the sync plane:
/// bump this for a backward-compatible change, raise
/// [`MIN_SUPPORTED_CONTROL_PROTOCOL`] only when dropping support for an old one.
///
/// Version 1 is the first: a daemon that does not answer `hello` at all predates
/// the handshake (v0.4.8 and earlier) and is reported as such.
///
/// **v2:** the space-vocabulary flag day renamed fields carried on this plane —
/// `Diagnose.expected_space`, `StatusInfo.space`, `IssueView.space_id`. A v1
/// daemon cannot answer them, so v1 is retired rather than tolerated: a client
/// that decoded a v1 answer would read absent fields as absent state.
///
/// **v3:** explicit Space/World routes name the local Orbit as well as the
/// expected Space. A v2 per-home route selected the Orbit only out-of-band via
/// its socket, which cannot address two local Orbits in the same Space through
/// one future daemon endpoint.
///
/// **v4:** product calls use a versioned opaque [`Call`] envelope at the
/// identity-scoped daemon.
///
/// **v5:** attached StationHost processes accept that same opaque envelope
/// directly. Typed product requests and the root-owned compatibility codec are
/// retired, so v4 processes cannot remain attached across this boundary.
///
/// **v6:** product host projections, including Issues inbox, leave root
/// `Request`/`Response`. Their local facilities now wrap opaque World calls.
///
/// **v7:** a third envelope. Until now every exchange on this channel was one
/// JSON line each way, which is the wrong shape for a 256 MiB attachment: it
/// would have to be base64'd, held whole in memory on both sides, and parsed as
/// one token. The content envelope declares a byte length in its header line
/// and then sends exactly that many raw bytes, in both directions. A v6 process
/// cannot serve it — it would read the body as a malformed second request — so
/// v6 is retired rather than tolerated.
///
/// **v8:** Live views gain a standing subscription. A v7 Station can answer a
/// one-shot `live` request but cannot provide the event-driven stream the web
/// viewer now uses, so it must be replaced rather than silently leaving a room
/// with no updates.
///
/// **v9:** doorbell invalidations are World-declared and grouped by World id.
/// A v8 frame uses Issues-specific field names, so accepting it would silently
/// lose row refreshes instead of producing a useful incompatibility error.
///
/// **v10:** World calls are framed the way content already was — a header line
/// declaring a byte length, then exactly those bytes — instead of base64'd into
/// the header itself. The encoding was costing a third more bytes and two
/// passes each way over every board, list, and comment on the channel. A v9
/// process reads the payload that follows as a malformed second request, which
/// is precisely why this cannot be tolerated across the boundary: the failure
/// would not be a decode error, it would be a desynchronised connection.
/// v11: the identity-scoped address book — twelve `Book*` requests and the
/// `Book`/`BookResolution` responses on the daemon route. Framing unchanged;
/// the vocabulary grew, and a v10 daemon answers these verbs with "unknown
/// variant" rather than a version complaint unless the handshake names it.
///
/// v12: card exchange stages rather than mutates — `BookPropose`,
/// `BookSuggestAccept`, `BookSuggestDismiss`, and the suggestions carried on
/// the Book view.
///
/// v13: the identity daemon owns the self-hosted display coordinator. Native
/// Astrolabe clients can inspect its receiver surfaces, two-party enrollment,
/// exact assignments, and receiver health, and can approve, assign, revoke,
/// or reject through the daemon-scoped display verbs.
pub const CONTROL_PROTOCOL_VERSION: u32 = 13;

/// Which build a daemon is, for deciding whether to reuse it or take over.
///
/// Not a security claim and not an attestation — a daemon reports this about
/// itself, and anything that can answer the control channel can say anything.
/// It exists for exactly one situation: a developer rebuilt the binary and the
/// daemon from the previous build is still holding the home. The protocol
/// handshake cannot see that, because both builds speak the same protocol.
///
/// `built` is the executable's mtime, which is what actually moves on a rebuild.
/// `version` alone would not: two local builds of the same commit — or of two
/// different commits between releases — carry the same semver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildFingerprint {
    pub version: String,
    /// The daemon's own executable path.
    pub exe: String,
    /// Unix seconds of that file's mtime, or 0 where it could not be read.
    pub built: u64,
}

impl BuildFingerprint {
    /// This process's own fingerprint, resolved once and then remembered.
    ///
    /// Sampled at first call — in practice startup — and never re-read, because
    /// the question is *which build is this process running*, and that is fixed
    /// the moment it is loaded. Asking the filesystem again later answers a
    /// different question and answers it wrongly: replacing a binary under a
    /// live daemon is the ordinary upgrade path (`host_update` renames the old
    /// file aside and writes the new one in its place), and on Windows a running
    /// process resolves its own path to the *current* name of the file it holds
    /// open. A daemon that re-read it after such an upgrade would report
    /// `lait.old` — a path no client will ever match — and the stale daemon it
    /// is would become invisible at exactly the moment it started to matter.
    pub fn here() -> Self {
        static HERE: std::sync::OnceLock<BuildFingerprint> = std::sync::OnceLock::new();
        HERE.get_or_init(|| {
            let exe = std::env::current_exe().unwrap_or_default();
            let built = std::fs::metadata(&exe)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Self {
                version: env!("LAIT_VERSION_SEMVER").to_string(),
                exe: exe.display().to_string(),
                built,
            }
        })
        .clone()
    }

    /// Whether `self` is a build this one should displace.
    ///
    /// **The same executable, restamped.** That is the whole rule, and it is
    /// narrow on purpose. It catches the situation this exists for — the binary
    /// was rebuilt and the daemon from the previous build is still holding the
    /// home, answering every request while running code that is no longer on
    /// disk — and it catches nothing else.
    ///
    /// Two things it deliberately does *not* do. It does not act on a *different*
    /// path: a client is not always the binary it would spawn (an integration
    /// test is its own executable, and so is anything embedding this crate), so
    /// treating "not me" as "stale" would have every such client evict a
    /// perfectly good daemon it did not start. And it does not act on age alone
    /// in either direction — without the path check, two binaries run in turn
    /// would each evict the other's daemon at startup and a machine running both
    /// would never keep one up.
    ///
    /// Everything it declines to displace is still *reused*, not refused: the
    /// protocol already matched, so talking to it works.
    pub fn supersedes(&self, other: &Self) -> bool {
        self.exe == other.exe && self.built > other.built
    }
}

/// The oldest control protocol a client still talks to. Raising this retires a
/// version; the gap to [`CONTROL_PROTOCOL_VERSION`] is the mixed-version window.
///
/// Protocol v10 was a deliberate compatibility cutoff: a v9 process cannot
/// read a framed World call, and connections are reused now, so a single
/// misinterpreted payload would poison every request that followed it rather
/// than failing once. The minimum moves with the version rather than trailing
/// it — a v10 daemon answers the book's verbs with "unknown variant" instead
/// of a version complaint, which is a worse failure than being told to stop.
pub const MIN_SUPPORTED_CONTROL_PROTOCOL: u32 = 13;

/// Whether this build can talk to a daemon advertising control protocol `peer`.
///
/// Pure, so the window policy is unit-testable without a daemon — the same shape
/// as `sync::check_sync_protocol`. Returns a human-facing reason on refusal:
/// which side is behind decides who has to act.
pub fn check_control_protocol(peer: u32) -> Result<()> {
    if peer < MIN_SUPPORTED_CONTROL_PROTOCOL {
        return Err(anyhow!(
            "the daemon speaks control protocol v{peer}, older than the minimum \
             this build supports (v{MIN_SUPPORTED_CONTROL_PROTOCOL}); \
             stop that daemon so this build can start its own"
        ));
    }
    if peer > CONTROL_PROTOCOL_VERSION {
        return Err(anyhow!(
            "the daemon speaks control protocol v{peer}, newer than this build's \
             v{CONTROL_PROTOCOL_VERSION}; upgrade lait to that build"
        ));
    }
    Ok(())
}

/// The OS name of the control channel for a home (unix socket / Windows named
/// pipe). Daemon and clients derive it from the same home so they agree.
pub fn control_name(home: &Path) -> Result<Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        crate::config::socket_path(home)
            .to_fs_name::<GenericFilePath>()
            .context("build control socket name")
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        format!("lait-{}.sock", crate::config::home_hash(home))
            .to_ns_name::<GenericNamespaced>()
            .context("build control pipe name")
    }
}

/// One product-neutral Mechanics assignment supplied by a World client package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssignmentSpec {
    pub world: String,
    pub capability: String,
    #[serde(default)]
    pub resource: Vec<String>,
}

/// One browser caret in a document field.
///
/// Positions are Unicode-scalar offsets in the World's collaborative text, not
/// DOM or UTF-16 offsets. The Station turns them into CRDT-relative anchors
/// before the Live plane sends them to peers; the declaration itself remains
/// transient and is never journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchingCaret {
    pub issue: String,
    pub field: String,
    pub anchor: u64,
    #[serde(default)]
    pub focus: Option<u64>,
}

/// One cumulative optimistic text preview from a browser's durable base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchingPreview {
    pub issue: String,
    pub field: String,
    pub base: String,
    pub result: String,
    pub index: u64,
    pub delete: u64,
    pub insert: String,
    #[serde(default)]
    pub anchor: Option<u64>,
    #[serde(default)]
    pub focus: Option<u64>,
}

/// One coarse browser typing declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchingTyping {
    pub issue: String,
    pub field: String,
}

/// The one canonical group an agent's card is filed under. Part of the
/// book's wire vocabulary: the daemon stamps it at provisioning and heals it
/// from rosters, and clients that part or mark agents key on this name — a
/// contract, not a display string.
pub const AGENT_GROUP: &str = "Agents";

/// Astrolabe's controller-facing view of the identity daemon's display service.
/// Receiver credentials and canonical package input never cross this plane.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayCoordinatorView {
    pub instance: String,
    pub label: String,
    pub origin: String,
    pub certificate_sha256: String,
    pub certificate_pem: String,
    pub surfaces: Vec<DisplaySurfaceView>,
    pub devices: Vec<DisplayDeviceView>,
    pub assignments: Vec<DisplayAssignmentView>,
    pub pending_pairings: Vec<DisplayPairingView>,
    /// How exposed this coordinator's identifier key is — `None` from a daemon
    /// that predates the custody split.
    ///
    /// Additive and optional, per `docs/COMPATIBILITY.md`: a required field
    /// here makes an older daemon's reply undecodable, which presents as a
    /// coordinator that never answers rather than as a version mismatch.
    ///
    /// `Option` rather than a defaulted value, because a default would have to
    /// invent a measurement. An empty slot list means *this build reports no
    /// unlock paths*, which is a fact worth a warning; a daemon that was never
    /// asked has no such fact, and rendering the two the same way would raise
    /// an alarm about a coordinator nobody has examined.
    #[serde(default)]
    pub identifier_custody: Option<DisplayIdentifierCustodyView>,
}

/// One rendered surface, for a member screen to present.
///
/// The receiver protocol's program snapshot commits an assignment, a revision,
/// a freshness policy and opaque asset handles. None of that appears here,
/// because every one of them exists to make bytes safe for a participant that
/// is not in the Space. A member screen is, so what crosses is the rendered
/// output and the product's own assessment of it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayPresentationView {
    pub world: String,
    pub surface: String,
    /// `current`, `partial`, or `unavailable` — the product's assessment,
    /// never collapsed into presence-or-absence of items.
    pub assessment: String,
    pub partial_reasons: Vec<String>,
    /// `hold_last`, `loop`, `poll_at_end`, or `blank_at_end`.
    pub cycle: String,
    pub refresh_after_ms: Option<u32>,
    pub items: Vec<DisplayPresentationItemView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayPresentationItemView {
    pub id: String,
    pub duration_ms: Option<u32>,
    pub assessment: String,
    pub spoken_summary: Option<String>,
    pub scene: DisplayPresentationSceneView,
}

/// What one item draws.
///
/// `Unsupported` is a scene rather than an error, and it is what a live-media
/// item becomes on this path: the live edge is coordinator machinery a member
/// screen does not run. The receiver invariants say an unsupported output kind
/// refuses visibly rather than reinterpreting itself, and that rule is not
/// about receivers — it is about not drawing something other than what was
/// asked for.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DisplayPresentationSceneView {
    Frame {
        /// `png`, `jpeg`, or `webp`.
        media_type: String,
        width: u32,
        height: u32,
        /// Standard base64. The control plane is JSON, and a byte array encoded
        /// as a list of numbers costs roughly four times the transfer for the
        /// same pixels.
        bytes_base64: String,
    },
    Blank {
        /// `source_unavailable`, `unsupported`, or `program_ended`.
        reason: String,
    },
    Unsupported {
        /// What the surface asked for that this screen does not draw.
        output: String,
    },
}

/// How exposed this coordinator's identifier key is to the loss of its machine.
///
/// Present on every status read rather than behind a settings page, because the
/// moment an operator wants this fact is *after* the machine is gone, and by
/// then it is unreadable. Losing the key does not merely inconvenience a
/// restore: it invalidates every assignment-bound item and asset identifier
/// this coordinator has issued, so receivers holding them stop resolving.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayIdentifierCustodyView {
    /// One entry per independent way in — `recovery-key`, `passphrase`,
    /// `windows-dpapi`. Kinds, never material.
    pub slots: Vec<String>,
    /// Whether any path survives leaving this machine.
    pub portable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplaySurfaceView {
    pub world: String,
    pub surface: String,
    pub title: String,
    pub contract_version: u32,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayDeviceView {
    pub device: String,
    pub label: String,
    pub platform: String,
    pub build: String,
    pub issued_at_unix_ms: u64,
    pub revoked_at_unix_ms: Option<u64>,
    pub health: Option<DisplayHealthView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayHealthView {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayAssignmentView {
    pub assignment: String,
    pub device: String,
    pub orbit: String,
    pub space: String,
    pub program: String,
    pub world: String,
    pub surface: String,
    pub controller: String,
    pub theme: DisplayThemeSetting,
    pub sync: Option<DisplayAssignmentSyncView>,
    pub expires_at_unix_ms: Option<u64>,
    pub revoked_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayAssignmentSyncView {
    pub group: String,
    pub mode: DisplaySyncModeSetting,
    pub static_delay_ms: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayPairingView {
    pub pairing: String,
    pub confirmation_phrase: Vec<String>,
    pub certificate_sha256: String,
    pub platform: String,
    pub build: String,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DisplayThemeSetting {
    Light,
    Dark,
    HighContrast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DisplayStaleActionSetting {
    KeepWithNativeBanner,
    Blank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DisplaySyncModeSetting {
    StayInSync,
    Positional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayAssignmentSyncSetting {
    pub group: String,
    pub mode: DisplaySyncModeSetting,
    pub static_delay_ms: i32,
}

/// A request from a client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    // ---- identity-scoped Astrolabe display coordination ----
    DisplayStatus,
    DisplayPairingApprove {
        pairing: String,
        label: String,
    },
    DisplayPairingReject {
        pairing: String,
    },
    /// Commit an exact package display pin for one enrolled receiver. The
    /// daemon derives Space, implementation and contract digests from its
    /// trusted registry rather than accepting those facts from the controller.
    DisplayAssignmentPut {
        device: String,
        orbit: String,
        world: String,
        surface: String,
        #[schemars(with = "serde_json::Value")]
        input: serde_json::Value,
        theme: DisplayThemeSetting,
        stale_after_ms: u32,
        on_stale: DisplayStaleActionSetting,
        #[serde(default)]
        sync: Option<DisplayAssignmentSyncSetting>,
        #[serde(default)]
        expires_at_unix_ms: Option<u64>,
    },
    DisplayAssignmentRevoke {
        assignment: String,
    },
    DisplayDeviceRevoke {
        device: String,
    },
    /// Add a passphrase as a second way into this coordinator's identifier key.
    ///
    /// The first slot is sealed to the daemon's own device, which survives an
    /// operating-system profile but not the loss of the identity itself. A
    /// passphrase is the one unlock path that depends on neither, which is what
    /// makes it the honest second slot rather than a third copy of the first.
    ///
    /// The passphrase is never stored — it wraps the data-encryption key and is
    /// forgotten. Losing it costs this path and nothing else.
    DisplayIdentifierAdmitPassphrase {
        passphrase: String,
    },
    /// Render one exact surface for a screen that is a **member of the Space**.
    ///
    /// Distinct from [`Request::DisplayAssignmentPut`] in what it does not do:
    /// nothing is committed, no receiver is named, and no assignment exists
    /// afterwards. A member holds the Space these bytes came from, so there is
    /// no credential to bind them to and nothing to revoke later — losing
    /// standing simply stops the Query.
    DisplayPresent {
        orbit: String,
        world: String,
        surface: String,
        #[schemars(with = "serde_json::Value")]
        input: serde_json::Value,
        theme: DisplayThemeSetting,
        /// The presenting window, in physical pixels.
        width: u32,
        height: u32,
        scale_milli: u16,
        locale: String,
    },

    // ---- membership and authorization ----
    MemberAdd {
        who: String,
        #[serde(default)]
        admin: bool,
        /// Optional local petname to attach to the resolved key (never synced).
        #[serde(default)]
        as_name: Option<String>,
    },
    MemberRemove {
        who: String,
    },
    /// Elevate or demote an existing member (admin-only): a signed
    /// `SetGrants` on the ACL. `admin: true` promotes, `false` demotes to a
    /// plain writing member. Refused for agents and for the last admin.
    MemberSetRole {
        who: String,
        admin: bool,
    },
    /// Sponsor an agent keypair whose inception is already known here. Any human
    /// member may sponsor; the agent is sealed the space key and holds content
    /// authority (`Grant::Write`) but never membership authority, and its
    /// standing dies with the sponsor.
    AgentAdd {
        /// The agent's ed25519 public key (64-hex).
        key: String,
    },
    /// Provision a **co-located** agent identity by name, in one step: mint (or
    /// reuse) its seed under this home, self-incept it into the shared store's
    /// actor plane, and sponsor it with content authority. This is the seamless
    /// "sponsor once" flow for an agent on the same machine — no Contact round,
    /// no second daemon, no second store copy. Afterwards a client acts as it
    /// via the `act_as` selector (`$LAIT_AS`, or an MCP client's own binding).
    AgentProvision {
        /// The local name for this agent identity.
        name: String,
    },
    KeyRotate,
    /// Revoke an outstanding invite so it can no longer admit anyone (admin-
    /// only). Accepts the invite ticket or its 32-hex nonce.
    InviteRevoke {
        /// The invite ticket, or its raw 32-hex nonce.
        invite: String,
    },
    /// Print a device-enrollment token for adding another device to *this*
    /// actor (lait/actor/1). The new machine consumes it with `device accept`.
    DeviceInvite,
    /// Add a device to our actor from its consent blob (produced by
    /// `device accept`), sealing it the space key.
    DeviceAdd {
        /// Hex-encoded consent binding from the joining device.
        consent: String,
    },
    /// Revoke a device from our actor and rotate the key to fence it.
    DeviceRevoke {
        device: String,
    },
    /// List the device keys currently bound to our actor.
    DeviceList,
    /// Break-glass **space** recovery (lait/space/1 W5): re-root the space
    /// to this device using the offline space recovery keys, as threshold
    /// `Recover` events. Distinct from [`Recover`](Self::Recover), which resets a
    /// single actor's devices.
    SpaceRecover,
    /// Elevate the space recovery authority from a solo bootstrap key to a
    /// `k`-of-N FROST group key over `cofounders` (device keys) + this device,
    /// via a dealer-free DKG that rides the synced ceremony bulletin board.
    SpaceElevate {
        cofounders: Vec<String>,
        k: u16,
    },
    /// Co-sign a pending break-glass recovery request as a holder of the current
    /// K-of-N group recovery key. Explicit per-request consent: the holder has
    /// checked out-of-band that `session` re-roots to the agreed party.
    SpaceRecoverApprove {
        session: String,
        /// The actor(s) the holder expects this recovery to re-root to — consent
        /// binds to the roots, so an injected request that re-roots elsewhere is
        /// refused before any share is contributed.
        expect: Vec<String>,
    },
    /// Co-sign a pending authority grant as a holder of the current group key,
    /// authorizing a replacement ceremony. Consent binds to the PROPOSAL, not to
    /// the session id: a request for a different proposal is refused.
    SpaceElevateApprove {
        session: String,
        proposal: String,
    },
    /// Reshare the standing group recovery key onto a new K-of-N arrangement
    /// **without changing the key** (same-key share redistribution) — the
    /// participant-replacement path. The current holders authorize it exactly
    /// like an elevation (`SpaceElevateApprove`), the redistribution advances
    /// on sync, and the current group threshold-signs the installation.
    SpaceReshare {
        participants: Vec<String>,
        k: u16,
    },
    /// Export this device's recovery share as a portable, passphrase-protected
    /// package, verify it by reopening, and attest that on the board. An
    /// all-holders arrangement will not install until every custodian has done
    /// this.
    SpaceCustodyExport {
        path: String,
        passphrase: String,
    },
    /// Restore a recovery share from a portable package written by
    /// `SpaceCustodyExport`. Refuses to replace a readable share unless `force`.
    SpaceCustodyImport {
        path: String,
        passphrase: String,
        force: bool,
    },
    /// Recover our actor with the offline recovery key: reset the device set to
    /// this device (identity is restored; content-key access is re-sealed lazily
    /// by an admin/peer).
    Recover,
    Members,
    /// The membership audit log: the signed ACL DAG replayed in causal order
    /// with each op's authorization verdict (cryptographic provenance).
    MemberLog,

    /// Identity-scoped address book. Daemon route only; never places an Orbit.
    /// The book is the one namer: member and presence rows leave the Station
    /// bare and the daemon decorates them from Cards — the `MemberAlias` verb
    /// and `aliases.json` it wrote are gone (2026-08-13).
    BookList,
    BookGet {
        card: String,
    },
    BookPut {
        #[serde(default)]
        card: Option<String>,
        name: String,
        #[serde(default)]
        note: Option<String>,
    },
    BookDelete {
        card: String,
    },
    /// Set (or clear, with an empty string) a card's picture, in the stored
    /// `<mime>;base64,<data>` form. The engine validates shape and size; a
    /// stored picture is always drawable.
    BookSetPicture {
        card: String,
        picture: String,
    },
    BookLink {
        card: String,
        handle: String,
    },
    BookUnlink {
        card: String,
        handle: String,
    },
    BookMerge {
        from: String,
        into: String,
    },
    BookClaimSelf {
        card: String,
    },
    BookLookup {
        handle: String,
    },
    /// Scoped decoration: an Orbit this head already authorized, plus handles
    /// present in that answer. The daemon re-filters independently.
    BookResolve {
        orbit: String,
        handles: Vec<String>,
    },
    BookMigrateStatus,
    BookMigrate,
    /// Stage a card-exchange bundle as pending suggestions. The bundle is the
    /// file's JSON, carried by the head that owns the file dialog — the daemon
    /// never reads a caller-named path. Nothing imports without review.
    BookPropose {
        bundle: String,
    },
    /// Accept one staged suggestion: mint the Card, link its handles, retire
    /// the suggestion. Refusal leaves the suggestion staged.
    BookSuggestAccept {
        suggestion: String,
    },
    /// Discard one staged suggestion without touching the book.
    BookSuggestDismiss {
        suggestion: String,
    },

    // ---- generic Space authority capabilities ----
    /// Effective scoped assignments (Mechanics history, not Catalog state).
    AssignmentList {
        #[serde(default)]
        actor: Option<String>,
    },
    /// Install exact package-planned assignments, authority-first and
    /// all-or-nothing. Mechanics validates every World/resource/capability.
    AssignmentGrant {
        actor: String,
        assignments: Vec<AssignmentSpec>,
    },
    /// Revoke one effective assignment by its grant id (64-hex).
    AssignmentRevoke {
        grant_id: String,
    },
    /// Activate this build's reviewed implementation for one hosted World
    /// (admin-authored ACL action; idempotent when already active).
    /// The activation is what receipts pin — a build whose descriptor differs
    /// from the active one should run this before writing.
    WorldActivate {
        world: String,
    },
    /// Product-neutral durable Exec lifecycle facility.
    ///
    /// The root control protocol transports Runtime's exact type and never
    /// interprets product payloads or invents product verbs.
    Work {
        #[schemars(with = "serde_json::Value")]
        request: runtime::exec::WorkRequest,
        /// Host-minted 128-bit persistent idempotency coordinate (32 hex).
        operation: String,
    },
    /// Which Worlds this Orbit has activated, with what a client needs to draw
    /// and open each one.
    ///
    /// The read counterpart [`Request::WorldActivate`] never had. It is routed
    /// to an Orbit and answered only by one that is *already placed* — see
    /// `request_if_running`. That is the whole design: listing what a device
    /// serves must never place a Station, or a Library that draws ten rows
    /// mounts ten stores to do it, and listing costs what opening costs.
    WorldsActive,
    /// What this Orbit's store is holding: bytes on disk, how many Bodies, and
    /// when its integrity was last verified.
    ///
    /// Routed to an Orbit and answered only by one that is *already placed* —
    /// the same rule [`Request::WorldsActive`] follows, and for the same reason.
    /// A caller reaches it through `request_if_running`, so a vacant Orbit
    /// answers "not running" instead of being placed; otherwise a surface
    /// showing storage for ten Spaces would mount ten stores to draw a column,
    /// and looking would cost what opening costs.
    ///
    /// Per-Orbit, never machine-wide: the figures are attributable to one
    /// Space, because a person deciding what to keep is deciding about one
    /// Space at a time.
    Storage,
    /// Streaming dirty notifications for live clients. Turns the one-shot handler into a
    /// stream of [`Doorbell`] frames until the client disconnects.
    Subscribe {
        #[serde(default)]
        since: u64,
    },

    // ---- transport / presence ----
    Status,
    /// Guided-join verifier (`docs/UI.md`, joining): project live
    /// node state into an ordered list of onboarding gates so a stalled joiner
    /// gets one legible blocker instead of a blank board. `expected_space`
    /// (supplied by `HostSpaceEnter` from the invite ticket) lets it catch a
    /// directory/store mismatch; `None` when nothing was expected.
    Diagnose {
        #[serde(default)]
        expected_space: Option<String>,
    },
    Id,
    /// One-shot identity + standing + view-completeness report (the MCP
    /// `whoami` tool, the viewer's own identity panel). A read: the full version of `Id`'s actor line —
    /// actor, `did:key`, role, capabilities, sponsor, space, and the **loud**
    /// partial-view signal — so neither a human nor an agent ever *infers* "who
    /// am I / what may I do / is my view complete."
    Whoami,
    /// Watch the host-plane sponsorship wait for the acting agent.
    ///
    /// Exec Watch's comparison, not a live stream: pass the heads the last
    /// reading returned; matching heads answer `WaitReply::Unchanged`. A
    /// grant moves the heads. There is no Work `Start` here — the wait is
    /// opened by `Whoami` as an unsponsored named agent.
    SponsorWatch {
        #[serde(default)]
        heads: Vec<String>,
    },
    /// Converge now and report what moved and what is still divergent — the
    /// request that supersedes a hand-aimed `Connect`. Surfaces missing-
    /// epoch / partial-read state **loudly** instead of silently showing fewer
    /// issues (the 141-vs-154 inference this initiative kills).
    Sync,
    /// Mint an invite link. It always carries a signed admission capability:
    /// the joiner's explicit acceptance IS the approval, and redemption is
    /// automatic over Contact — there is no approval queue.
    Invite {
        /// The role the invite admits as (`viewer` | `contributor` |
        /// `administrator`); defaults to `contributor`. The capability carries
        /// the role's exact expanded assignments in its signed evidence.
        #[serde(default)]
        role: Option<String>,
        /// Let the capability admit a whole team (valid until expiry) instead
        /// of one person (single-use).
        #[serde(default)]
        reusable: bool,
        /// Lifetime in hours before the capability expires (default 168 = 7 days).
        #[serde(default)]
        ttl_hours: Option<u64>,
    },
    Join {
        ticket: String,
    },
    Connect {
        ticket: String,
    },
    /// Pin an always-on seed peer. `arg` is a room ticket (adopt the
    /// space + backfill) or a bare endpoint id (pin only). Sticky across
    /// restarts; grants no trust.
    SeedAdd {
        arg: String,
    },
    /// List pinned seeds and their current reachability.
    SeedList,
    /// Unpin a seed by endpoint id (or id-prefix) or nick.
    SeedRemove {
        who: String,
    },
    /// Presence and transport event log.
    Log {
        since: u64,
    },
    Who,
    /// What this Station currently believes about who is doing what — the Live
    /// plane's transient table, resolved to actors.
    ///
    /// Distinct from [`Request::Who`], which reports durable neighbours and
    /// their reachability. This reports what is on screen right now: who is
    /// looking at an issue, where a caret is, who is typing. None of it is
    /// durable and none of it survives the session that published it.
    /// Say what this node is looking at, so peers can be told.
    ///
    /// **Replace-all**, and the whole set every time: this is a snapshot of what
    /// somebody has open, and an incremental form would let a client that
    /// navigates faster than its messages arrive publish a set neither side
    /// agrees on. An empty list is a node that is looking at nothing, which is
    /// how presence stops.
    ///
    /// The counterpart of [`Request::Live`], which asks what *others* are doing.
    /// Two verbs rather than one because they fail differently: this one is
    /// lossy by nature — a declaration that does not arrive is corrected by the
    /// next one — and a read that silently returned a stale table would not be.
    Watching {
        /// The `iss_` doc ids, never project aliases. The Body id is derived
        /// from the string as given, so an alias publishes presence on a Body
        /// nothing reads and nobody sees a face.
        #[serde(default)]
        issues: Vec<String>,
        /// Current browser selections, aggregated across this server's tabs.
        #[serde(default)]
        carets: Vec<WatchingCaret>,
        /// Fields in which a browser has produced input recently.
        #[serde(default)]
        typing: Vec<WatchingTyping>,
        /// Display-only cumulative splices awaiting durable peer convergence.
        #[serde(default)]
        previews: Vec<WatchingPreview>,
    },
    Live {
        /// The generation the caller already holds. When it still stands the
        /// answer is [`Response::LiveUnchanged`], so a poll that finds nothing
        /// new costs a `u64` comparison instead of a re-serialised table.
        ///
        /// `None` rather than a defaulted `0`: generation starts at zero, so a
        /// caller that has never asked would otherwise be indistinguishable
        /// from one holding an empty table, and its first read would answer
        /// "unchanged" about a view it has never seen.
        #[serde(default)]
        since_generation: Option<u64>,
        /// Narrow to one issue, by its `iss_` doc id.
        ///
        /// Every scope about that issue's Body and not the viewing one alone:
        /// who is looking at it, where their carets are, who is typing. Those
        /// are three scopes over one Body, and a caller that named the issue
        /// asked about all three.
        ///
        /// A doc id, never a project alias. The Body id is derived from the
        /// string as given, so `ENG-12` hashes to a Body nothing publishes
        /// under and the answer is an empty table rather than an error.
        ///
        /// `None` is the whole table, which carries Body ids from every hosted
        /// World. A browser cannot map those back to anything it displays — the
        /// derivation runs one way — so the scoped form is the one a viewer
        /// uses and the unscoped one is for an operator.
        ///
        /// It narrows the rows, not the generation: the counter belongs to the
        /// whole table, so a caller watching one issue is told to re-read when
        /// anything anywhere moves. That costs a wasted read and never a missed
        /// one, which is the right way round for a surface about who is here.
        #[serde(default)]
        issue: Option<String>,
    },
    /// Stream the current Live view and every superseding generation.
    ///
    /// This is the standing counterpart to [`Request::Live`]. It preserves the
    /// same narrowing and response vocabulary while removing the adapter's
    /// polling interval from cursor and optimistic-text delivery.
    LiveSubscribe {
        /// Narrow to one issue by its `iss_` doc id; `None` streams the table.
        #[serde(default)]
        issue: Option<String>,
    },
    /// Take every signal delivered since the last call, and leave the queue
    /// empty.
    ///
    /// A drain rather than a read. A signal is an event, not a state anyone can
    /// re-read, so answering the same one twice would have a client act on it
    /// twice — an invitation accepted, a file offered, a person's attention
    /// asked for.
    Signals,
    /// Re-read the layered local settings (`HostConfigSet` sends this
    /// best-effort so a daemon-read key like `user.nick` applies live instead
    /// of silently waiting for a restart). Transport-plane like `Stop` — not
    /// part of the MCP tool surface.
    ConfigReload,
    Stop,

    // ---- the host plane: bootstrap, node-local state, orientation ----
    //
    // Every request below routes to [`ControlRoute::Daemon`], and every one of
    // them can run before any Orbit exists. That is why they are here rather
    // than behind a Station: the daemon is identity-scoped and is built from an
    // identity directory (`daemon::run_lait_daemon`), so it is the only party
    // that exists early enough to host formation — and, because it is the party
    // that would otherwise be holding the store lock, the only one that can run
    // formation and rebuild without racing itself for it.
    //
    // Paths are carried explicitly. The daemon's working directory is not the
    // caller's, and a head serving several callers out of one process has no
    // working directory worth consulting at all.
    /// Found a new Space into an explicit store directory, and register the
    /// resulting Orbit.
    HostSpaceFound {
        /// The store directory to form into, created if absent.
        home: String,
        /// The Space's display name.
        name: String,
        /// Optional `user.nick` written into the new store's config layer.
        #[serde(default)]
        nick: Option<String>,
    },
    /// Bootstrap a joiner's store from an invite link, and register the Orbit.
    ///
    /// The transport half remains [`Request::Connect`]: this readies the store
    /// the joiner's Station will occupy, so the daemon only ever opens a
    /// well-formed store already bound to the invite's Space.
    HostSpaceEnter {
        /// A Coordinates v1 invite link (or its bare ticket).
        link: String,
        /// The store directory to bootstrap into, created if absent.
        home: String,
        #[serde(default)]
        nick: Option<String>,
    },
    /// Sign this machine's consent to join an existing actor, from a
    /// `device invite` token (`<actor_id> <space_id>`).
    ///
    /// The one host request that touches no store: the machine running it has
    /// no membership anywhere yet, which is the whole point of enrolment.
    HostDeviceConsent {
        token: String,
    },
    /// Every recognized local setting, with its effective value and origin.
    HostConfigList {
        /// The store whose layer participates; `None` reads the global layer
        /// alone.
        #[serde(default)]
        home: Option<String>,
    },
    /// One local setting's effective value.
    HostConfigGet {
        key: String,
        #[serde(default)]
        home: Option<String>,
    },
    /// Write one local setting.
    HostConfigSet {
        key: String,
        value: String,
        /// Write the global layer instead of the store layer.
        #[serde(default)]
        global: bool,
        #[serde(default)]
        home: Option<String>,
    },
    /// Clear one local setting from the layer a write would target.
    HostConfigUnset {
        key: String,
        #[serde(default)]
        global: bool,
        #[serde(default)]
        home: Option<String>,
    },
    /// Deregister local Orbits from the catalog. Never touches a store.
    HostOrbitForget {
        /// A store path, a Space id, or a unique Space-id prefix.
        selector: String,
    },
    /// Drop every registry row whose store is gone from disk.
    HostOrbitPrune,
    /// Rebuild one Orbit's implicit prior journal representation as an explicit
    /// current generation.
    ///
    /// The daemon releases its own placement for that Orbit first. Run from a
    /// client this was a store-lock race against whatever the daemon had open.
    HostOrbitRebuild {
        /// A local Orbit id, a store path, a Space id, or a display name.
        orbit: String,
    },
    /// Register the lait MCP server in an agent client's config file.
    ///
    /// A head cannot answer this itself: writing the file that tells an agent
    /// how to reach lait is bootstrapping, the same class as founding a Space
    /// or signing device consent, and it must work before any store exists.
    ///
    /// `dir` is the project directory a project-scoped config lands in, carried
    /// explicitly because the daemon's working directory is not the caller's.
    HostInstallMcp {
        /// `claude` | `cursor` | `windsurf` | `generic`.
        client: crate::install::Client,
        /// `user` | `project`; `None` takes the client's own default.
        #[serde(default)]
        scope: Option<crate::install::Scope>,
        /// The server name to write under (`lait` unless overridden).
        name: String,
        /// The sponsored agent identity its work signs as; `None` derives one
        /// from the client.
        #[serde(default)]
        agent: Option<String>,
        /// Decline an agent identity, leaving the work signed by the human.
        #[serde(default)]
        no_agent: bool,
        /// Return the would-be file contents instead of writing them.
        #[serde(default)]
        print: bool,
        /// The project directory for a project-scoped config.
        dir: String,
        /// Mount of the World this binding pins (`issues`, `signage`, …).
        /// `None` lets `lait mcp` take the sole World this build hosts.
        #[serde(default)]
        world: Option<String>,
    },
    /// Replace the installed binary with the release this node's channel
    /// points at, resolved from the signed first-party feed (`src/update/`),
    /// never a forge API.
    ///
    /// Node maintenance, not a command: the daemon is the process that knows
    /// which build it is running, and the atomic self-replace works on a live
    /// executable (it renames rather than overwrites), so the swap lands and
    /// takes effect at the next restart.
    HostUpdate,
    /// Stop this daemon once the reply is on the wire, so the next request
    /// starts a fresh one.
    ///
    /// The only way a swapped binary or a raised control protocol takes effect
    /// now that no terminal can stop anything: [`Request::HostUpdate`] renames
    /// the executable out from under a live process, and
    /// `check_control_protocol` tells a newer build to "stop that daemon so this
    /// build can start its own". Every head stands a daemon back up on the first
    /// send that finds nobody listening, so a stop *is* the restart.
    ///
    /// Deliberately not [`Request::Stop`], which the host plane refuses: `Stop`
    /// reaches whatever process is on the other end of the socket, and a page
    /// that could send it could kill the server answering it. This one is only
    /// ever the daemon *under* a head, and that head survives to re-spawn it.
    HostRestart,
    /// Orientation: this identity, the Worlds this build hosts, and the local
    /// Orbits and named identities that exist.
    HostContext,
    /// Version handshake (see [`CONTROL_PROTOCOL_VERSION`]). The first thing a
    /// client sends, and the only request whose reply must stay decodable
    /// forever — it is what tells two mismatched builds *why* they can't talk
    /// instead of leaving them to fail at decoding something else.
    Hello {
        /// The client's version. Unused today (the client decides), but it is
        /// what a future daemon would need to refuse an ancient client, and it
        /// cannot be added later without another flag day.
        #[serde(default)]
        protocol_version: u32,
    },
}

/// An explicit path through local orchestration.
///
/// `None` on [`ClientRequest::route`] is the legacy per-home path: the socket
/// already identifies one Space. The general Lait daemon uses an explicit
/// route so one endpoint can reject cross-Space or cross-World confusion before
/// dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ControlRoute {
    /// Address the process-level Orbit directory/control router.
    Daemon,
    /// Address Space mechanics, Station, observations, or Space lifecycle.
    #[serde(rename = "space")]
    Orbit {
        #[serde(flatten)]
        address: OrbitAddress,
    },
    /// Address one World hosted by one Space.
    World {
        #[serde(flatten)]
        address: OrbitAddress,
        world: String,
    },
}

/// The wire envelope a client sends: a [`Request`], an optional routing
/// [`ControlRoute`], passive-dispatch intent, and an optional **acting
/// identity** selector.
///
/// `act_as` names a local identity (an agent profile name, actor id, or device
/// id) the daemon holds a seed for. `None` is the primary human identity. Both
/// Optional modifiers are skipped when absent, so a legacy request serializes
/// to exactly the bare `{"cmd":…}` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRequest {
    /// The route this request is allowed to traverse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<ControlRoute>,
    /// Ask the identity-scoped host to dispatch only when the addressed
    /// Station already has a live compatibility adapter. This is a passive
    /// catalog probe, not an authorization claim.
    #[serde(default, skip_serializing_if = "is_false")]
    pub if_running: bool,
    /// The local identity to sign+attribute this request as. `None` = the
    /// daemon's primary (human) identity, exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act_as: Option<String>,
    #[serde(flatten)]
    pub request: Request,
}

impl ClientRequest {
    /// A plain request as the primary identity (the pre-B behavior).
    pub fn plain(request: Request) -> Self {
        Self {
            route: None,
            if_running: false,
            act_as: None,
            request,
        }
    }
    /// A request acting as a named local identity.
    pub fn acting_as(request: Request, act_as: Option<String>) -> Self {
        Self {
            route: None,
            if_running: false,
            act_as,
            request,
        }
    }

    /// A request with an explicit route.
    pub fn routed(request: Request, route: ControlRoute, act_as: Option<String>) -> Self {
        Self {
            route: Some(route),
            if_running: false,
            act_as,
            request,
        }
    }

    /// A routed request that must not activate a vacant Orbit.
    pub fn routed_if_running(request: Request, route: ControlRoute) -> Self {
        Self {
            route: Some(route),
            if_running: true,
            act_as: None,
            request,
        }
    }
}

/// Product-neutral request sent to the identity-scoped daemon.
///
/// The identity daemon forwards this same envelope unchanged to an attached
/// StationHost. Its explicit `call` field keeps every routing layer independent
/// of the product payload and protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldClientRequest {
    pub route: ControlRoute,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act_as: Option<String>,
    pub call: Call,
}

impl WorldClientRequest {
    pub fn new(route: ControlRoute, call: Call, act_as: Option<String>) -> Self {
        Self {
            route,
            act_as,
            call,
        }
    }
}

/// The header of a framed World call: everything except the payload, which
/// follows it as raw bytes.
///
/// [`WorldClientRequest`] still encodes the payload inside its JSON, because a
/// [`Call`] must be serializable wherever one is written down. On *this*
/// channel it is not written down, it is carried — and a channel that can
/// declare a length has no reason to spend a base64 pass and a third more bytes
/// to smuggle bytes through a format that cannot hold them.
///
/// The discriminant is unchanged: this still has a `call` field, which is what
/// tells a reader it is looking at a World call rather than a control request.
/// Only that field's shape moved, which is why it costs a protocol version.
///
/// This mirrors `ContentReply::ContentStream`, the other framed thing on this
/// wire, deliberately: header line declaring a length, then exactly that many
/// bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldCallFrame {
    pub route: ControlRoute,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act_as: Option<String>,
    pub call: CallFrame,
}

/// A [`Call`] with its payload replaced by that payload's length.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallFrame {
    pub world: replica::body::WorldId,
    pub operation: String,
    pub version: u32,
    /// How many bytes follow the header line.
    pub len: u64,
}

impl CallFrame {
    /// Describe a call without moving its payload.
    pub fn of(call: &Call) -> Self {
        Self {
            world: call.world().clone(),
            operation: call.operation().to_string(),
            version: call.version(),
            len: call.payload().len() as u64,
        }
    }
}

/// A [`Reply`] with its payload replaced by that payload's length.
///
/// An error carries no bytes and declares no length — the failure *is* the
/// answer, and a length of zero would be a second way to say so.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyFrame {
    pub world: replica::body::WorldId,
    pub operation: String,
    pub version: u32,
    #[serde(flatten)]
    pub outcome: ReplyFrameOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReplyFrameOutcome {
    Ok {
        len: u64,
    },
    Error {
        error: runtime::world::call::Failure,
    },
}

/// The framed form of a reply, and the bytes that follow it.
pub fn frame_reply(reply: Reply) -> (ReplyFrame, Vec<u8>) {
    let (world, operation, version, outcome) = reply.into_parts();
    match outcome {
        Ok(payload) => (
            ReplyFrame {
                world,
                operation,
                version,
                outcome: ReplyFrameOutcome::Ok {
                    len: payload.len() as u64,
                },
            },
            payload,
        ),
        Err(error) => (
            ReplyFrame {
                world,
                operation,
                version,
                outcome: ReplyFrameOutcome::Error { error },
            },
            Vec::new(),
        ),
    }
}

/// A declared payload length this channel will not allocate for.
///
/// Checked *before* the allocation, against the same ceiling the payload itself
/// is bound by, so a header claiming more than a reply may ever contain is
/// refused rather than believed.
pub fn refuse_oversized_payload(len: u64) -> Result<usize> {
    let ceiling = runtime::world::call::MAX_WORLD_REPLY_PAYLOAD;
    if len > ceiling as u64 {
        return Err(anyhow!(
            "a World payload of {len} bytes was declared, past the {ceiling} this \
             channel carries"
        ));
    }
    usize::try_from(len).map_err(|_| anyhow!("a World payload of {len} bytes does not fit here"))
}

fn is_false(value: &bool) -> bool {
    !value
}

/// The longest **content** header line this build will read.
///
/// A content header is a JSON object naming a call and a byte count, and
/// nothing else — the bytes travel after it, not inside it. So this is small on
/// purpose, and the body it declares is bounded separately by the Station's own
/// `max_content_len`.
pub const MAX_CONTROL_FRAME_BYTES: u64 = 64 * 1024;

/// The longest **request** line this build will read on the control channel.
///
/// Nothing bounded these at all: `AsyncBufReadExt::read_line` grows until it
/// finds a newline or the sender stops, so a client that opens the socket and
/// sends no newline was a memory attack that needed no authorization.
///
/// It is this much larger than [`MAX_CONTROL_FRAME_BYTES`] because an ordinary
/// request still carries its whole payload inline, and the largest legitimate
/// one today is an attachment: 256 KiB of bytes is about 342 KB of base64 plus
/// an envelope. That is the number this bound is sized against, and it is the
/// reason the bound is generous rather than tight — a limit that refuses
/// something the product still does is not a limit, it is a bug.
///
/// It shrinks toward the frame bound when the inline attachment write path goes
/// away and the only large thing on this channel is a declared body.
pub const MAX_CONTROL_LINE_BYTES: u64 = 4 * 1024 * 1024;

/// One call on the content plane.
///
/// Deliberately not a `Request` variant. Every other call on this channel is a
/// JSON line whose answer is a JSON line; these are the only ones that carry
/// bytes, and framing them the same way would mean every reader on the channel
/// has to know when a line is followed by a body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ContentCall {
    /// What is known about one content, and how much of it is here.
    Stat { content: String },
    /// One bounded range of plaintext. The answer carries a body.
    Read {
        content: String,
        offset: u64,
        len: u64,
    },
    /// Seal and commit the bytes this request's body carries.
    Write {
        /// The operation id, so a resumed or replayed upload is the same
        /// operation rather than a second one.
        operation: String,
    },
    /// Drop the local bytes and keep the name.
    Forget { content: String },
}

/// The wire envelope for a content call: the call, the route it may traverse,
/// the acting identity, and how many raw bytes follow this line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentClientRequest {
    pub content: ContentCall,
    pub route: ControlRoute,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act_as: Option<String>,
    /// Exactly how many raw bytes follow the newline. Authoritative: a body
    /// that ends early is an error and a body with one byte too many is
    /// refused, because "however much arrives" is indistinguishable from a
    /// truncated upload and would commit a permanently wrong content that
    /// hashes fine.
    #[serde(default)]
    pub body_len: u64,
}

/// Why a content call was refused, in the vocabulary a local surface maps to
/// its own status codes.
///
/// Typed rather than a message, because the caller has to *act* differently:
/// a missing chunk is worth retrying after a transfer, an unknown content
/// never will be, and a refusal names a demand the caller can go and satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentErrorCode {
    /// Authorization refused. The message names the demand.
    Denied,
    /// No descriptor here — and deliberately the same answer whether the
    /// content never existed or this Station simply never heard of it.
    Unknown,
    /// The descriptor is here and the bytes are not. Retryable, after a fetch.
    NotResident,
    /// A range past the content, or a length past what one call may return.
    Bounds,
    /// The store or the cache failed. Ours, not the caller's.
    Storage,
    /// The request did not make sense: a malformed id, a body that disagreed
    /// with its declaration.
    Invalid,
}

/// The answer to a [`ContentCall`].
///
/// `ContentStream` is the only variant followed by raw bytes, and it says how
/// many. Everything else is a complete answer on its own line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentReply {
    /// What is known about one content.
    ContentStatus {
        content: String,
        plaintext_len: u64,
        chunk_count: u32,
        resident_chunks: u32,
        pinned: bool,
    },
    /// `len` raw bytes follow this line.
    ContentStream { len: u64 },
    /// The content this upload became.
    ContentWritten { content: String, plaintext_len: u64 },
    /// The bytes are gone and the name is kept.
    ContentForgotten,
    ContentError {
        code: ContentErrorCode,
        message: String,
    },
}

impl ContentReply {
    pub fn error(code: ContentErrorCode, message: impl Into<String>) -> Self {
        Self::ContentError {
            code,
            message: message.into(),
        }
    }
}

/// The terminal owner of a control request — the single orbital plane that
/// serves it (plan 01, "External architecture"):
///
/// - **Mechanics** — membership/admission/ceremony/custody/device work through
///   the active Orbit/Station's mechanics;
/// - **Station** — connect/neighbor/Contact operations;
/// - **Observation** — status and subscription projections;
/// - **Lifecycle** — Runtime/Orbit/Station/daemon process concerns and
///   node-local configuration adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOwner {
    Mechanics,
    Station,
    Work,
    Observation,
    Lifecycle,
}

impl RequestOwner {
    /// The stable lowercase label (the generated routing table's column).
    pub fn label(&self) -> &'static str {
        match self {
            RequestOwner::Mechanics => "mechanics",
            RequestOwner::Station => "station",
            RequestOwner::Work => "work",
            RequestOwner::Observation => "observation",
            RequestOwner::Lifecycle => "lifecycle",
        }
    }
}

/// The PRODUCTION exhaustive request classifier. The daemon dispatches from
/// this value; the classification test and the generated routing table call
/// this same function. The match is exhaustive with no wildcard arm, so
/// adding a `Request` variant fails compilation until its terminal owner is
/// explicit.
pub fn classify(req: &Request) -> RequestOwner {
    use RequestOwner::*;
    match req {
        // ---- Mechanics: membership, admission, ceremonies, custody, devices ----
        Request::MemberAdd { .. }
        | Request::MemberRemove { .. }
        | Request::MemberSetRole { .. }
        | Request::Members
        | Request::MemberLog
        | Request::AgentAdd { .. }
        | Request::AgentProvision { .. }
        | Request::KeyRotate
        | Request::InviteRevoke { .. }
        | Request::DeviceInvite
        | Request::DeviceAdd { .. }
        | Request::DeviceRevoke { .. }
        | Request::DeviceList
        | Request::SpaceRecover
        | Request::SpaceElevate { .. }
        | Request::SpaceRecoverApprove { .. }
        | Request::SpaceElevateApprove { .. }
        | Request::SpaceReshare { .. }
        | Request::SpaceCustodyExport { .. }
        | Request::SpaceCustodyImport { .. }
        | Request::Recover
        | Request::Invite { .. }
        | Request::Join { .. }
        | Request::AssignmentList { .. }
        | Request::AssignmentGrant { .. }
        | Request::AssignmentRevoke { .. }
        | Request::WorldActivate { .. }
        | Request::WorldsActive
        | Request::Id
        | Request::Whoami
        | Request::SponsorWatch { .. } => Mechanics,

        // ---- Work: Runtime-owned durable Run lifecycle ----
        Request::Work { .. } => Work,

        // ---- Station: connect/neighbor/Contact ----
        // Live and Signals sit here for the same reason Who does: both read
        // state the Station's own delivery planes hold, and neither is a
        // projection of anything durable.
        Request::Connect { .. }
        | Request::Who
        | Request::Sync
        | Request::Live { .. }
        | Request::LiveSubscribe { .. }
        | Request::Watching { .. }
        | Request::Signals => Station,

        // ---- Observation: generic status and subscription surfaces ----
        // Storage belongs here and not with Mechanics: it projects what the
        // durable store already holds and signs, admits and changes nothing —
        // the same kind of answer `Status` gives, about a different plane.
        Request::Status | Request::Storage | Request::Subscribe { .. } => Observation,

        // ---- Lifecycle/deployment: daemon process + node-local config ----
        Request::Diagnose { .. }
        | Request::DisplayStatus
        | Request::DisplayPairingApprove { .. }
        | Request::DisplayPairingReject { .. }
        | Request::DisplayAssignmentPut { .. }
        | Request::DisplayAssignmentRevoke { .. }
        | Request::DisplayDeviceRevoke { .. }
        | Request::DisplayPresent { .. }
        | Request::DisplayIdentifierAdmitPassphrase { .. }
        | Request::SeedAdd { .. }
        | Request::SeedList
        | Request::SeedRemove { .. }
        | Request::Log { .. }
        | Request::ConfigReload
        | Request::Stop
        | Request::Hello { .. }
        | Request::BookList
        | Request::BookGet { .. }
        | Request::BookPut { .. }
        | Request::BookDelete { .. }
        | Request::BookSetPicture { .. }
        | Request::BookLink { .. }
        | Request::BookUnlink { .. }
        | Request::BookMerge { .. }
        | Request::BookClaimSelf { .. }
        | Request::BookLookup { .. }
        | Request::BookResolve { .. }
        | Request::BookMigrateStatus
        | Request::BookMigrate
        | Request::BookPropose { .. }
        | Request::BookSuggestAccept { .. }
        | Request::BookSuggestDismiss { .. }
        // The host plane is lifecycle by definition: node-local state and the
        // two verbs that bring a Space into existence on this machine. None of
        // them has a Station to be owned by — most run before one could exist.
        | Request::HostSpaceFound { .. }
        | Request::HostSpaceEnter { .. }
        | Request::HostDeviceConsent { .. }
        | Request::HostConfigList { .. }
        | Request::HostConfigGet { .. }
        | Request::HostConfigSet { .. }
        | Request::HostConfigUnset { .. }
        | Request::HostOrbitForget { .. }
        | Request::HostOrbitPrune
        | Request::HostOrbitRebuild { .. }
        | Request::HostInstallMcp { .. }
        | Request::HostUpdate
        | Request::HostRestart
        | Request::HostContext => Lifecycle,
    }
}

/// Select the terminal Space/World route for an already-authorized Orbit.
///
/// Process-level requests such as stopping the Lait daemon are chosen by the
/// client surface itself; this helper covers requests whose owner lives behind
/// a Station.
pub fn station_route(address: OrbitAddress) -> ControlRoute {
    ControlRoute::Orbit { address }
}

/// One representative instance per `Request` variant — the enumeration the
/// generated routing table and classification tests iterate. Kept beside
/// [`classify`] so both evolve together; the classifier's exhaustive match is
/// the compile-time guard for new variants.
pub fn representative_requests() -> Vec<Request> {
    let s = String::new;
    vec![
        Request::DisplayStatus,
        Request::DisplayPairingApprove {
            pairing: s(),
            label: s(),
        },
        Request::DisplayPairingReject { pairing: s() },
        Request::DisplayAssignmentPut {
            device: s(),
            orbit: s(),
            world: s(),
            surface: s(),
            input: serde_json::Value::Null,
            theme: DisplayThemeSetting::Dark,
            stale_after_ms: 0,
            on_stale: DisplayStaleActionSetting::Blank,
            sync: None,
            expires_at_unix_ms: None,
        },
        Request::DisplayAssignmentRevoke { assignment: s() },
        Request::DisplayDeviceRevoke { device: s() },
        Request::DisplayIdentifierAdmitPassphrase { passphrase: s() },
        Request::DisplayPresent {
            orbit: s(),
            world: s(),
            surface: s(),
            input: serde_json::Value::Null,
            theme: DisplayThemeSetting::Dark,
            width: 1920,
            height: 1080,
            scale_milli: 1000,
            locale: "en".into(),
        },
        Request::AssignmentList { actor: None },
        Request::AssignmentGrant {
            actor: s(),
            assignments: vec![],
        },
        Request::AssignmentRevoke { grant_id: s() },
        Request::WorldActivate { world: s() },
        Request::Work {
            request: runtime::exec::WorkRequest::Inspect {
                #[allow(
                    clippy::expect_used,
                    reason = "a compile-time literal in canonical reverse-domain form"
                )]
                world: replica::body::WorldId::parse("com.example.work")
                    .expect("a well-formed representative World id"),
                run: runtime::exec::RunId::from_bytes([0; 16]),
            },
            operation: s(),
        },
        Request::MemberAdd {
            who: s(),
            admin: false,
            as_name: None,
        },
        Request::MemberRemove { who: s() },
        Request::MemberSetRole {
            who: s(),
            admin: false,
        },
        Request::AgentAdd { key: s() },
        Request::AgentProvision { name: s() },
        Request::KeyRotate,
        Request::InviteRevoke { invite: s() },
        Request::DeviceInvite,
        Request::DeviceAdd { consent: s() },
        Request::DeviceRevoke { device: s() },
        Request::DeviceList,
        Request::SpaceRecover,
        Request::SpaceElevate {
            cofounders: vec![],
            k: 0,
        },
        Request::SpaceRecoverApprove {
            session: s(),
            expect: vec![],
        },
        Request::SpaceElevateApprove {
            session: s(),
            proposal: s(),
        },
        Request::SpaceReshare {
            participants: vec![],
            k: 0,
        },
        Request::SpaceCustodyExport {
            path: s(),
            passphrase: s(),
        },
        Request::SpaceCustodyImport {
            path: s(),
            passphrase: s(),
            force: false,
        },
        Request::Recover,
        Request::Members,
        Request::MemberLog,
        Request::BookList,
        Request::BookGet { card: s() },
        Request::BookPut {
            card: None,
            name: s(),
            note: None,
        },
        Request::BookDelete { card: s() },
        Request::BookSetPicture {
            card: s(),
            picture: s(),
        },
        Request::BookLink {
            card: s(),
            handle: s(),
        },
        Request::BookUnlink {
            card: s(),
            handle: s(),
        },
        Request::BookMerge {
            from: s(),
            into: s(),
        },
        Request::BookClaimSelf { card: s() },
        Request::BookLookup { handle: s() },
        Request::BookResolve {
            orbit: s(),
            handles: vec![],
        },
        Request::BookMigrateStatus,
        Request::BookMigrate,
        Request::BookPropose { bundle: s() },
        Request::BookSuggestAccept { suggestion: s() },
        Request::BookSuggestDismiss { suggestion: s() },
        Request::Subscribe { since: 0 },
        Request::Status,
        Request::Storage,
        Request::Diagnose {
            expected_space: None,
        },
        Request::Id,
        Request::Whoami,
        Request::SponsorWatch { heads: vec![] },
        Request::Sync,
        Request::Invite {
            role: None,
            reusable: false,
            ttl_hours: None,
        },
        Request::Join { ticket: s() },
        Request::Connect { ticket: s() },
        Request::SeedAdd { arg: s() },
        Request::SeedList,
        Request::SeedRemove { who: s() },
        Request::Log { since: 0 },
        Request::Who,
        Request::Live {
            since_generation: None,
            issue: None,
        },
        Request::LiveSubscribe { issue: None },
        Request::Signals,
        Request::ConfigReload,
        Request::Stop,
        Request::Hello {
            protocol_version: 0,
        },
        Request::HostSpaceFound {
            home: s(),
            name: s(),
            nick: None,
        },
        Request::HostSpaceEnter {
            link: s(),
            home: s(),
            nick: None,
        },
        Request::HostDeviceConsent { token: s() },
        Request::HostConfigList { home: None },
        Request::HostConfigGet {
            key: s(),
            home: None,
        },
        Request::HostConfigSet {
            key: s(),
            value: s(),
            global: false,
            home: None,
        },
        Request::HostConfigUnset {
            key: s(),
            global: false,
            home: None,
        },
        Request::HostOrbitForget { selector: s() },
        Request::HostOrbitPrune,
        Request::HostOrbitRebuild { orbit: s() },
        Request::HostInstallMcp {
            client: crate::install::Client::Generic,
            scope: None,
            name: s(),
            agent: None,
            no_agent: false,
            print: true,
            dir: s(),
            world: None,
        },
        Request::HostUpdate,
        Request::HostRestart,
        Request::HostContext,
    ]
}

/// The generated routing rows: `(wire command tag, owner label)` per variant,
/// derived from [`representative_requests`] and [`classify`].
pub fn routing_rows() -> Vec<(String, &'static str)> {
    representative_requests()
        .iter()
        .map(|req| {
            let tag = serde_json::to_value(req)
                .ok()
                .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(String::from))
                .unwrap_or_default();
            (tag, classify(req).label())
        })
        .collect()
}

/// One live Card as the daemon reports it. Authored fields only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookCardView {
    pub card: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// Every handle in its wire spelling, whatever its kind. The categorized
    /// triplet below is the same set split the way a phone book reads —
    /// add-only fields, so older clients keep reading this one.
    #[serde(default)]
    pub handles: Vec<String>,
    /// `actor:<space>:<actor>` spellings — where this person is someone.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// Bare device ids — the machines that answer as them.
    #[serde(default)]
    pub devices: Vec<String>,
    /// `agent:<store>:<name>` spellings — co-located agents, never shared.
    #[serde(default)]
    pub agents: Vec<String>,
    /// The stored picture (`<mime>;base64,<data>`), or `None` when the card
    /// has none — in which case a client draws its default face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub self_claim: bool,
}

/// Alias-migration progress. `complete` is only true after every selector was
/// resolved or discarded — not after the first pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BookMigrationView {
    pub complete: bool,
    pub pending: usize,
    pub imported: usize,
    pub files: usize,
}

/// One staged card-exchange suggestion, awaiting review. Never part of the
/// book until accepted; carries only what the bundle was allowed to carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookSuggestionView {
    pub suggestion: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub handles: Vec<String>,
}

/// The identity's book, plus how far legacy alias import has got.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookView {
    #[serde(default)]
    pub cards: Vec<BookCardView>,
    #[serde(default)]
    pub migration: BookMigrationView,
    /// Staged card-exchange proposals. Review is the only way in.
    #[serde(default)]
    pub suggestions: Vec<BookSuggestionView>,
}

/// One authored hit that survived the daemon's independent Orbit filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookHitView {
    pub card: String,
    pub handle: String,
    /// Authored Card name. Empty only if the card id no longer projects,
    /// which is not a name and must not be drawn as one.
    #[serde(default)]
    pub name: String,
    /// The card's stored picture (`<mime>;base64,<data>`), or `None`. This is
    /// how an application resolves a face for a handle: through the book,
    /// never through its own name-matched table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
}

/// Scoped decoration. `coverage` is `unavailable` when the Orbit is vacant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookResolutionView {
    #[serde(default)]
    pub hits: Vec<BookHitView>,
    #[serde(default)]
    pub coverage: Option<String>,
}

/// A response from the daemon or Space host. Internally tagged by `kind`;
/// product response schemas are not members of this enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Reply to [`Request::Hello`] — the daemon's control protocol version.
    ///
    /// Read by the client **before** any typed decoding (as raw JSON), so a
    /// version mismatch reports itself instead of surfacing as a decode error on
    /// some unrelated field. That means this variant's shape is load-bearing:
    /// `kind` and `protocol_version` must keep their names for as long as any
    /// supported version exists.
    Hello {
        protocol_version: u32,
        /// Which build is answering. Optional so the reply stays decodable by
        /// every client that ever read it — the one promise this variant makes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build: Option<BuildFingerprint>,
    },
    Ok {
        message: Option<String>,
    },
    /// Identity-scoped display coordination state for Astrolabe.
    Display(Box<DisplayCoordinatorView>),
    /// One rendered surface for a member screen to present.
    DisplayPresentation(Box<DisplayPresentationView>),
    /// A write echoes the resolved canonical handle.
    Ref {
        reff: String,
    },
    Members {
        members: Vec<MemberDto>,
    },
    /// Effective scoped assignments (reply to [`Request::AssignmentList`]).
    Assignments {
        rows: Vec<mechanics::assignment::AssignmentDto>,
    },
    /// Runtime-owned lifecycle answer to [`Request::Work`]. Product packages
    /// receive Runtime's exact type; no World payload crosses this route.
    Work {
        reply: runtime::exec::WorkReply,
    },
    /// The membership audit log (reply to [`Request::MemberLog`]).
    MemberLog {
        entries: Vec<MemberLogEntry>,
    },
    /// Identity-scoped address book (reply to the `Book*` requests).
    Book(Box<BookView>),
    /// Scoped handle decoration. Never a Card-existence bit for a handle
    /// outside the named Orbit's non-placing snapshot.
    BookResolution(Box<BookResolutionView>),
    /// Pinned seeds ("remotes") and their reachability.
    Seeds {
        seeds: Vec<SeedDto>,
    },

    // ---- transport / presence ----
    // Boxed like `Issue`/`Board`: `StatusInfo` is the largest variant, and keeping
    // it inline makes `Response` (used as the `Err` type of the resolve helpers)
    // trip clippy's `result_large_err`.
    Status(Box<StatusInfo>),
    /// The guided-join verifier's ordered gate list (reply to [`Request::Diagnose`]).
    Diagnosis(Box<DiagnosisView>),
    Text {
        text: String,
    },
    Events {
        events: Vec<Event>,
        last: u64,
    },
    Who {
        peers: Vec<PresenceEntry>,
    },
    /// Reply to [`Request::WorldsActive`]: the World ids this Space activated.
    ///
    /// Ids and nothing else, deliberately. *Which* Worlds an Orbit serves is
    /// the Space's authority and is what the daemon can answer; how to draw and
    /// open one is a declaration of whichever build is asking, and that build
    /// has its own client registry to join in. A daemon that answered display
    /// metadata would be answering for a package it does not hold.
    Worlds {
        worlds: Vec<String>,
    },
    /// Reply to [`Request::Storage`]: what this Orbit's store is holding.
    ///
    /// **Every figure is optional, and absent is a real answer.** A number
    /// nobody measured is reported missing, never estimated and never defaulted
    /// to zero — a synthesised figure that makes a surface look populated is
    /// the observation-failure defect wearing different clothes, and it is
    /// harder to spot because it looks like data. `null` here means "not
    /// measured"; it never means "measured, and it is nothing".
    Storage {
        /// Bytes on disk for this Space, or `null` when the measurement could
        /// not be taken. The whole store directory — Bodies, ledger, content
        /// cache, superseded generations — because those are all bytes this
        /// Space is occupying.
        #[serde(default)]
        bytes_on_disk: Option<u64>,
        /// How many Bodies the store holds, interpreted and opaque alike.
        #[serde(default)]
        object_count: Option<u64>,
        /// When integrity was last verified, in milliseconds since the unix
        /// epoch, or `null` when it never has been. A store that has never been
        /// checked says so; it does not report the epoch.
        #[serde(default)]
        last_verified_ms: Option<u64>,
    },
    /// The Live plane's transient table (reply to [`Request::Live`]).
    Live {
        /// Bumped on every change. A reader that sees the same number saw the
        /// same view and does not have to diff to find that out. It wraps, so
        /// equality is the only comparison it admits.
        generation: u64,
        /// This Station is not hearing from everyone it could be — over its
        /// session cap, or dropping scopes at a gate.
        ///
        /// Carried rather than inferred. Awareness is allowed to be incomplete
        /// and durable convergence is not, so the surface that can be partial
        /// has to say when it is; a viewer showing three of five people with no
        /// indication is telling a confident lie.
        partial: bool,
        entries: Vec<LiveEntry>,
    },
    /// The generation the caller already held still stands (reply to
    /// [`Request::Live`]).
    ///
    /// Its own variant rather than an absent `entries` on [`Response::Live`].
    /// This enum is tagged by `kind` and a client branches on that tag; "nothing
    /// changed" spelled as a missing field is a thing a client has to remember
    /// to notice, and an empty table and an unchanged one would then look alike.
    LiveUnchanged {
        generation: u64,
    },
    /// The signals drained by [`Request::Signals`], oldest first.
    Signals {
        signals: Vec<SignalEntry>,
        /// How many were dropped for want of room, oldest first.
        ///
        /// A signal is not replaceable the way a caret is, so the queue does not
        /// drop the newest the way the progress lane does: what has not been
        /// seen yet is what somebody is about to act on. Reported rather than
        /// swallowed — a client that lost an invitation can at least say so.
        dropped: u64,
    },
    /// The one-shot identity + standing + view-completeness projection.
    Whoami(crate::dto::WhoamiDto),
    /// Work-shaped sponsorship wait ([`Request::SponsorWatch`]).
    Wait(WaitReply),
    /// The result of a `sync`: whether the view is now whole, and the same loud
    /// divergence lines `whoami` reports (empty when converged and complete).
    Sync {
        /// True when this node's view is complete (holds every authorized epoch
        /// key and its history) after converging.
        whole: bool,
        /// Human-readable divergence lines — what epoch/history is still
        /// missing. Empty when whole.
        divergence: Vec<String>,
        /// A short human summary of what the sync did/found.
        message: String,
        /// Peers this sync actually reached.
        ///
        /// `whole` answers a *local* question — does this device hold every
        /// epoch key it is authorized for — and it answers `true` on a node
        /// that has spoken to nobody, including one holding zero items. That
        /// read as "you are up to date" for a full day of debugging while a
        /// replica sat empty. These three fields are the other half: whether
        /// anyone was asked, what they said, and whether anything arrived.
        #[serde(default)]
        peers_reached: usize,
        /// Peers that could not be reached, each with the reason.
        #[serde(default)]
        peers_failed: Vec<String>,
        /// Whether the round brought in material this node did not have.
        #[serde(default)]
        advanced: bool,
    },
    /// The answer to a host-plane request (`Request::Host*`).
    Host(HostReply),
    Error {
        message: String,
        // Named `error_kind`, not `kind`: the enum's internal tag is `kind`
        // (`#[serde(tag = "kind")]`), so a variant field of that name collides.
        #[serde(default)]
        error_kind: ErrorKind,
    },
}

/// What a host-plane request produced.
///
/// One `Response` arm rather than eleven. These are results of node-local
/// operations, not projections of Space state, and giving each its own
/// top-level variant would grow the surface every client matches on without
/// letting any of them say anything new. The inner tag is `host`, so a client
/// still branches on one string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "host", rename_all = "snake_case")]
pub enum HostReply {
    /// A Space was founded and its Orbit registered.
    Founded {
        space: String,
        /// The store directory it was formed into.
        home: String,
        /// This machine's device id in the new Space.
        device: String,
        name: String,
        /// The initial scope the bundled World seeded, so a caller can name
        /// where the first item will land.
        project_key: String,
        project_name: String,
    },
    /// A joiner's store was bootstrapped from an invite, and admission driven.
    Entered {
        space: String,
        home: String,
        device: String,
        /// The approach Station `Connect` was driven against, and the one to
        /// retry with when admission has not landed yet.
        approach: String,
        /// The inviter's nick from the ticket. May be empty.
        host_nick: String,
        /// False when the store already held this Space (a re-join).
        fresh: bool,
        /// Whether standing landed before the wait ran out. False is not a
        /// failure — the inviter may simply be offline — but it is the
        /// difference between "you're in" and "the board stays encrypted until
        /// they come online", and a surface that cannot tell them apart shows a
        /// blank board and calls it a space.
        admitted: bool,
        /// Whether the inviter answered at all.
        contacted: bool,
        /// The last refusal seen while it had not.
        #[serde(default)]
        last_error: Option<String>,
    },
    /// The hex consent blob to hand back to `device add`.
    DeviceConsent { consent: String },
    /// Effective local settings — the whole table, or one row for a `get`.
    Config { rows: Vec<crate::config::ConfigRow> },
    /// One completed settings write.
    ConfigWritten {
        write: crate::config::ConfigWrite,
        /// Whether a running Station took the change live.
        ///
        /// `None` when nothing could have: the key is not daemon-read, or the
        /// write targeted a layer with no Orbit behind it. The three-way answer
        /// exists so a surface never promises a restart that is not pending —
        /// a silent no-op is the failure this reports, and so is a warning
        /// about one that cannot happen.
        #[serde(default)]
        applied: Option<bool>,
    },
    /// Registry rows deregistered by `HostOrbitForget`. Stores untouched.
    Forgotten { entries: Vec<crate::orbits::Entry> },
    /// Registry rows dropped by `HostOrbitPrune` because their store is gone.
    Pruned { entries: Vec<crate::orbits::Entry> },
    /// The generation a rebuild selected, and what it covered.
    Rebuilt {
        generation: String,
        effects: u64,
        bodies: u64,
        receipts: u64,
        /// Hex digest binding the rebuilt representation.
        evidence: String,
    },
    /// An MCP client config was written (or, under `print`, rendered).
    McpInstalled {
        /// The config file this landed in — or would have.
        path: String,
        /// The file contents under `print`, else the human summary.
        detail: String,
        /// The client-specific caveat, when there is one. Carried rather than
        /// written to stderr: under `print` nobody is reading our stderr, and
        /// "this entry shadows the bundled plugin" is the whole reason the
        /// client has to be named.
        #[serde(default)]
        note: Option<String>,
        /// Whether an entry under this name already existed.
        replaced: bool,
        /// The agent identity the entry signs its work as, if any.
        #[serde(default)]
        agent: Option<String>,
    },
    /// The outcome of a self-update, and the channel facts learned resolving
    /// it — the readable surface SUB-13 names, which is what a client's update
    /// facts sample rather than asking the feed themselves.
    Updated {
        /// The version this node was running.
        from: String,
        /// The version now on disk (equal to `from` when already current).
        to: String,
        /// False when the node was already on the channel's release.
        replaced: bool,
        /// The channel this node follows (`stable` unless it opted in).
        #[serde(default)]
        channel: String,
        /// The newest release the channel points at.
        #[serde(default)]
        available: Option<String>,
        /// Set when this daemon is a client's sidecar and declined to replace
        /// itself. Carries the client's path — the thing that *does* update it.
        #[serde(default)]
        managed_by: Option<String>,
        /// The published compatibility floor — the lowest version still
        /// permitted to run — when the release declares a satisfiable one.
        #[serde(default)]
        floor: Option<String>,
    },
    /// The daemon accepted the reply's own last instruction and is stopping.
    Restarting {
        /// The process that is going away, so an operator can confirm it did.
        #[serde(default)]
        pid: Option<u32>,
    },
    /// Orientation for the identity this daemon runs as.
    Context {
        /// The build answering, in the form releases are identified by
        /// (`LAIT_VERSION_LONG`). This is the only place a running lait says
        /// which binary it is, and support for two builds in the field starts
        /// with being able to ask.
        version: String,
        identity_home: String,
        /// Where a head should offer to put a new store when the person has no
        /// opinion. A browser has no working directory to default to, so
        /// without this every founding form starts with an empty path box.
        spaces_root: String,
        /// World ids this build hosts.
        worlds: Vec<String>,
        /// Named identities registered on this machine.
        identities: Vec<String>,
        /// Every durable local Orbit known to this identity.
        orbits: Vec<crate::orbits::Entry>,
        /// Unsponsored agents that have asked this identity to sponsor them.
        ///
        /// Host-plane state, not a World signal: the client samples it the
        /// same way it samples orientation, and a second drain of `Signals`
        /// never has to exist for this to reach the person who can approve.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        asks: Vec<SponsorshipAsk>,
    },
}

/// A co-located agent that attached without standing, waiting on a person.
///
/// Keyed by `(space, name)` — `name` is the local identity (`LAIT_AGENT`),
/// which is what [`Request::AgentProvision`] takes. `actor` is filled in when
/// the agent has already incepted and is still unsponsored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SponsorshipAsk {
    pub space: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub asked_at_ms: u64,
}

/// A sponsorship that was approved and has not yet been delivered to the agent.
///
/// The counterpart of [`SponsorshipAsk`]: the ask is the person's decision,
/// the wake is the agent's. [`Request::SponsorWatch`] consumes it the same
/// way Work `Watch` consumes a head change — once, then it is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SponsorshipWake {
    pub space: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub granted_at_ms: u64,
}

/// Work-shaped answer to [`Request::SponsorWatch`].
///
/// Same comparison Exec Watch uses: known heads in, `Unchanged` if they
/// still match, otherwise the new state. Not a live stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "wait", rename_all = "snake_case")]
pub enum WaitReply {
    Unchanged {
        heads: Vec<String>,
    },
    Waiting {
        heads: Vec<String>,
        space: String,
        name: String,
    },
    Granted {
        heads: Vec<String>,
        space: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<String>,
    },
    /// Nothing is open for this caller.
    Idle,
}

/// Classifies a [`Response::Error`] so the process exit code is
/// derived from a **typed kind**, never by string-matching the human message.
/// `NotFound` (a ref / registry entry didn't resolve) maps to exit `2`;
/// everything else is a plain error → exit `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    #[default]
    Error,
    /// The request reached the right capability but its arguments or current
    /// lifecycle state do not admit the requested operation.
    Invalid,
    NotFound,
    /// The caller's identity lacks the standing this action needs (write access,
    /// admin, sponsorship). A **typed** authorization failure so a client — the
    /// MCP agent surface especially — can render an actionable next step ("ask
    /// your sponsor to grant write access") instead of an opaque blob, and so it
    /// is never confused with a transient/internal error. Exit `1` like `Error`.
    Denied,
}

impl Response {
    /// A generic failure — usage, validation, internal (exit `1`).
    pub fn err(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
            error_kind: ErrorKind::Error,
        }
    }
    /// A ref or registry lookup that resolved to **nothing** (exit `2`).
    pub fn not_found(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
            error_kind: ErrorKind::NotFound,
        }
    }
    /// A caller-correctable validation or lifecycle refusal (exit `1`).
    pub fn invalid(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
            error_kind: ErrorKind::Invalid,
        }
    }
    /// The caller lacks the standing this action requires — an authorization
    /// failure, not an internal one. The message should say what is missing and
    /// how to get it, so the agent surface can surface a next step.
    pub fn denied(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
            error_kind: ErrorKind::Denied,
        }
    }
}

/// The streamed frame: the repeated reply to [`Request::Subscribe`].
/// A **batched, World-declared dirty-set**, never state. The client re-reads
/// the authoritative projection for each dirty scope and plane; it never
/// patches from a doorbell. The `kind` and `plane` strings are the hosting
/// World's own vocabulary — the control plane carries them and does not
/// interpret them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Doorbell {
    /// Per-daemon-boot nonce; a change means restart and requires a `Reset`.
    pub epoch: u64,
    /// Per-session cursor. Never persisted.
    pub seq: u64,
    /// `true` means ignore the rest and rebaseline from a fresh snapshot.
    pub reset: bool,
    /// Product invalidations grouped by stable World id. Container kinds and
    /// plane names are only meaningful inside that boundary.
    #[serde(default)]
    pub invalidations: Vec<RoutedInvalidation>,
    /// Membership, roles, devices or keys advanced.
    ///
    /// Its own flag, not a catalog scope: authority is not in the catalog Body,
    /// it converges as signed authority records, and it can move with no Body
    /// touched at all. Calling it a catalog plane was a convenient lie that made
    /// the dirty-set describe the wrong thing.
    #[serde(default)]
    pub authority_advanced: bool,
    /// New feed rows exist; pull via `Activity{since}`. Rows are never streamed.
    pub activity_advanced: bool,
    /// New presence or join rows exist; pull via `Log{since}`. Rows are never
    /// streamed: like every other plane this is a dirty *flag*, not the events.
    /// The presence plane rings independently of the replica dirty-set, so a
    /// peer coming online wakes a subscriber even when no doc moved.
    /// `default` so a frame from a pre-plane daemon (stale across an update)
    /// still decodes because fields are add-only and absence means default.
    #[serde(default)]
    pub presence_advanced: bool,
}

/// The dirty-set vocabulary is World-opaque and defined by `runtime`, so the
/// control plane can carry a World it does not understand. It only carries it.
pub use runtime::world::{DirtyPlane, DirtyScope, RoutedInvalidation, ScopeRef};

/// A presence or transport log entry kept in the daemon's ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub kind: EventKind,
    pub id: String,
    pub nick: String,
    pub text: String,
    pub ts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Join,
    Presence,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceEntry {
    pub id: String,
    pub nick: String,
    /// The actor this device speaks for in the routed Space, resolved through
    /// the Station's authority view — the same resolution [`LiveEntry::actor`]
    /// rides on, carried here so a presence consumer can answer "is this
    /// *person* reachable" without a second request. `None` when the Station
    /// resolves no actor (a peer that lost standing, or one never admitted),
    /// which is an absence and travels as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Three-state presence: `online`, `away`, or `offline`.
    pub state: String,
    pub online: bool,
    pub last_seen_secs: u64,
    /// Whether the Contact scheduler would dial this Neighbor right now.
    ///
    /// The rest of this struct says whether a peer *seems* to be there; this
    /// says whether we will ever go and ask. They are different questions, and
    /// conflating them is how a node with no connectivity reads as healthy: a
    /// peer can look reachable, hold a fresh presence row, and still be one no
    /// scheduler will ever dial again.
    #[serde(default)]
    pub dialable: bool,
    /// When `dialable` is false, which of `eligible`'s three conditions failed —
    /// in its own words, so the answer does not have to be reconstructed from
    /// the numbers below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    /// Whether anything has queued a Contact. Cleared on success, and re-armed
    /// only by a newsworthy Beacon, a local commit, or a newly learned route.
    #[serde(default)]
    pub pending: bool,
    /// Seconds until the backoff floor lifts; 0 when it already has.
    #[serde(default)]
    pub due_in_secs: u64,
    /// Seconds of route lease remaining; 0 means expired, which suppresses
    /// dialing on its own.
    #[serde(default)]
    pub route_lease_secs: u64,
    /// Consecutive failed Contacts.
    #[serde(default)]
    pub failures: u32,
}

/// What a transient item is about — the wire mirror of
/// `runtime::transient::Target`.
///
/// Ids are rendered, not raw. This channel is JSON, where a `[u8; 16]` arrives
/// as a list of sixteen numbers; a Body takes the lowercase base32 the rest of
/// the tree renders Body ids in, and a content id takes hex like every other
/// content id here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum LiveScope {
    /// Somebody is looking at this Body.
    #[serde(rename = "issue_view")]
    Body { world: String, body: String },
    /// Somebody is looking at this materialized Body.
    #[serde(rename = "document_view")]
    Material { world: String, body: String },
    /// Somebody's cursor is in this field of this Body.
    #[serde(rename = "text_caret")]
    Field {
        world: String,
        body: String,
        field: String,
    },
    /// A display-only optimistic splice in this field.
    #[serde(rename = "text_preview")]
    Preview {
        world: String,
        body: String,
        field: String,
    },
    /// Somebody is typing in this field.
    Typing {
        world: String,
        body: String,
        field: String,
    },
    /// How much of this content a peer holds. A hint about who to ask first,
    /// never a promise.
    #[serde(rename = "content_residency")]
    Content { content: String },
    /// A World's own scope, uninterpreted here.
    #[serde(rename = "custom_world")]
    World {
        world: String,
        schema: String,
        key: String,
    },
}

/// Where a peer's cursor is, as of this read — the wire mirror of
/// `runtime::plane::live::CaretState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "caret", rename_all = "snake_case")]
pub enum CaretPosition {
    /// A position in the Body as it stands now.
    At { position: u64 },
    /// The material this position was attached to is gone, or the anchor
    /// predates what this node retains.
    Drifted,
    /// Nothing was available to resolve against. Distinct from `Drifted`, which
    /// is an answer — this is the absence of one, and a renderer that conflated
    /// them would draw a live caret as lost.
    Unresolved,
}

/// A display-only optimistic text splice carried by Live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPreview {
    pub base: String,
    pub result: String,
    pub index: u64,
    pub delete: u64,
    pub insert: String,
    pub anchor: Option<u64>,
    pub focus: Option<u64>,
}

/// One thing a peer is currently doing — the wire mirror of
/// `runtime::plane::live::LiveEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveEntry {
    /// The **actor**, resolved daemon-side through the Station's authority view.
    ///
    /// Never the device id the transient table is keyed by. [`PresenceEntry::id`]
    /// is a device and `MemberDto.key` is an actor, and the viewer colours an
    /// avatar by hashing whatever string it is handed — so a device id here
    /// draws one person in two colours on one screen, on a surface whose whole
    /// job is telling people apart. An entry whose Station does not resolve is
    /// omitted rather than carried under an invented identity.
    pub actor: String,
    pub scope: LiveScope,
    /// `presence` | `caret` | `selection` | `preview` | `typing` | `residency`.
    pub kind: String,
    /// How long ago **this** Station saw it. Ours, not theirs — a peer's clock
    /// is a peer's claim.
    pub age_ms: u64,
    /// Past the caret grace window. Still shown, and shown as uncertain: a caret
    /// whose Body has moved under it is not wrong yet, but it is no longer known
    /// to be right.
    pub uncertain: bool,
    pub caret: Option<CaretPosition>,
    /// A selection's far end.
    pub focus: Option<CaretPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<TextPreview>,
}

/// What a signal says — the wire mirror of `runtime::plane::Signal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum SignalBody {
    /// Are you there. The one signal that expects an answer.
    Ping { nonce: String },
    /// I am.
    Acknowledge { nonce: String },
    /// Look at this.
    Attention { scope: LiveScope },
    /// Come and work on this with me. `invite` is the kind of collaboration
    /// offered.
    SessionInvite { invite: String, scope: LiveScope },
    /// I have a file you may want. An offer, not a transfer: nothing has moved
    /// and nothing will until somebody decides.
    FileOffer {
        content: String,
        plaintext_len: u64,
        /// What the sender calls it. Peer-supplied and never sanitised here — a
        /// name is sanitised at the point it becomes a path, and rewriting it in
        /// flight would mean the thing shown to a person is not the thing sent.
        display_name: String,
        media_type: String,
    },
    /// A World's own signal. Opaque to the substrate, so it arrives base64
    /// rather than as a shape this module pretends to understand.
    WorldSignal {
        world: String,
        schema: String,
        payload_b64: String,
    },
}

/// One signal this Station received — the wire mirror of
/// `runtime::signal::DeliveredSignal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalEntry {
    /// The sender's **actor**, resolved daemon-side. Same rule as
    /// [`LiveEntry::actor`]: unresolved is omitted, never invented.
    pub actor: String,
    /// The session it arrived on, 32-hex. Two of these are compared and never
    /// ordered — the only answerable question is whether this is still the open
    /// one.
    pub connection_id: String,
    pub connection_epoch: String,
    pub signal: SignalBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub id: String,
    pub nick: String,
    /// The space display name (synced catalog value; empty on a joiner
    /// whose catalog hasn't arrived yet).
    pub name: String,
    /// The space overview description (synced catalog value; empty when unset).
    /// Additive so pre-SCOPE-2 clients decode the status unchanged.
    #[serde(default)]
    pub description: String,
    pub online_peers: usize,
    pub space: Option<String>,
    pub items: usize,
    pub scopes: usize,
    /// Whether the World counts below are UNAVAILABLE (undocked or a
    /// failed projection query). `true` means the zeros are not data — never
    /// read them as an empty space.
    #[serde(default)]
    pub counts_unavailable: bool,
    /// This node's standing in the space ACL: `admin` | `member` | `pending`.
    /// `pending` means we joined from an invite but an admin hasn't approved us
    /// yet, so we cannot decrypt the board. Lets `status` tell a joiner
    /// the truth instead of implying the join already succeeded.
    #[serde(default)]
    pub membership: String,
    /// Recovery shares this device holds that exist but cannot be used.
    ///
    /// Structured, not preformatted: each head renders it
    /// differently, and a rendered string would force one of them to parse
    /// prose. Persistent rather than recovery-only — an operator must be able to
    /// learn their founder share is unusable *before* the day they need it,
    /// which is exactly the day it is too late to fix.
    #[serde(default)]
    pub degraded_recovery: Vec<mechanics::recovery::DegradedHolder>,
    /// This device's recovery readiness: the standing authority's shape and our
    /// own custody standing. Reports what THIS node knows; it deliberately makes
    /// no claim about whether other holders still have their shares.
    #[serde(default)]
    pub recovery: Option<mechanics::recovery::State>,
}

/// What probing a home's control channel found. These three must be told apart
/// before deciding to spawn: treating them alike is how "a daemon is right there
/// but speaks a different wire shape" gets misreported as "no daemon", which then
/// spawns a doomed second daemon over a held lock and waits out the full timeout.
#[derive(Debug)]
pub enum Probe {
    /// Answered, and we understood the answer.
    Healthy,
    /// Nothing is listening: no daemon for this home. Safe to spawn.
    Absent,
    /// Something is listening, but we can't talk to it — a daemon from a
    /// different lait (it holds the lock, so spawning over it cannot work).
    Foreign {
        /// The handshake's diagnosis, including the way out.
        why: String,
        /// Whether stopping it and taking over is the right repair.
        ///
        /// **False when the other side is ahead of us.** Replacing a newer daemon
        /// with this build is a downgrade, and if it has already written the store
        /// at a newer `dto::SCHEMA_VERSION` then `store::check_schema_version`
        /// refuses to open it — so "helpfully" killing it stops the node dead.
        /// Also false for anything we can't identify: `daemon_pid` is only a claim
        /// from a file, and signalling a pid on a hunch is how you kill a stranger.
        replaceable: bool,
    },
}

/// A daemon is listening on this home that this build cannot talk to — in
/// practice a version skew (the binary was upgraded, the daemon wasn't restarted).
///
/// The error form of [`Probe::Foreign`], carrying the same diagnosis plus the
/// home it came from. It lives here, beside the probe that produces it, rather
/// than in a client renderer: the orbit router raises it too, and an error type
/// owned by one presentation surface makes every other producer depend on that
/// surface.
///
/// Typed rather than a message, so the repair can be offered from the error path
/// (see `cli::heal_from_error`) instead of probing eagerly on every command that
/// will never need it. Exit code `3`: unreachable in the sense that matters —
/// something is there, and no request will ever get through to it.
#[derive(Debug)]
pub struct ForeignDaemon {
    pub home: std::path::PathBuf,
    /// The handshake's own diagnosis; already carries the way out.
    pub why: String,
    /// Whether replacing it is the right repair — false when it is ahead of us.
    pub replaceable: bool,
}

impl std::fmt::Display for ForeignDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the Lait daemon is already running, but {why}  (home: {home})",
            why = self.why,
            home = self.home.display(),
        )
    }
}

impl std::error::Error for ForeignDaemon {}

/// A request that provably never reached the daemon.
///
/// **The failure this prevents: applying a write twice.** A caller that stands a
/// daemon up from a send failure has to decide whether to send again, and the
/// only safe basis for that is *where* the failure happened, not whether a
/// daemon happened to be listening a moment later. A connect that never opened
/// carried no bytes, so re-sending applies the request once. A failure after the
/// request line went in — the daemon applied the effect and then exited, taking
/// the reply with it — is indistinguishable from a lost reply, and re-sending
/// there applies it twice.
///
/// Only [`connect_bounded`] mints this, which is exactly the set of failures
/// that happen before anything is written.
#[derive(Debug)]
pub struct Undelivered(String);

impl std::fmt::Display for Undelivered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Undelivered {}

/// Whether `error` reports a request that never left this process.
///
/// Reads the type rather than the prose, and through `.context()`, for the same
/// reason exit codes do: callers wrap freely and a wrapped undelivered is still
/// undelivered.
pub fn undelivered(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Undelivered>().is_some()
}

/// Probe a home's control channel without spawning anything.
///
/// Two deliberate choices make this survive the very skew it exists to detect:
///
/// * **Absent vs present is decided at the transport level.** Whether `connect`
///   succeeds is a fact no protocol change can alter.
/// * **The version is read as raw JSON, before any typed decode.** Probing with a
///   typed request would mean a mismatched daemon fails on whatever field
///   happened to change (it was `StatusInfo.name`) and reports *that* instead of
///   the version. Only `kind` and `protocol_version` need to hold still.
pub async fn probe(home: &Path) -> Probe {
    // A probe that can hang defeats its own purpose: it exists to *diagnose* a
    // daemon that isn't answering, so it must not become the thing that isn't
    // answering. Neither side of the exchange is guaranteed to fail fast —
    // connecting to a Windows named pipe with no free instance parks rather than
    // erroring (see the teardown note in `node::run_daemon`) — and a local IPC
    // round trip that takes seconds is already broken by any measure.
    match tokio::time::timeout(PROBE_TIMEOUT, probe_inner(home)).await {
        Ok(p) => p,
        Err(_) => Probe::Foreign {
            why: format!(
                "it is not answering (no reply within {}s) — it may be wedged or \
                 shutting down; stop it and re-run",
                PROBE_TIMEOUT.as_secs()
            ),
            // A daemon we never heard from is not one we can identify, and an
            // unidentified pid is not a safe signal target.
            replaceable: false,
        },
    }
}

/// How long a local control round trip may take before the daemon counts as
/// unresponsive. Generous: the healthy path is sub-millisecond.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn probe_inner(home: &Path) -> Probe {
    let Ok(name) = control_name(home) else {
        return Probe::Absent;
    };
    // Connect failing is the real "no daemon" signal (no socket / nothing
    // accepting). Anything past this point means someone answered the door.
    let Ok(stream) = Stream::connect(name).await else {
        return Probe::Absent;
    };
    let line = match exchange_raw(
        stream,
        &ClientRequest::plain(Request::Hello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
        }),
    )
    .await
    {
        Ok(l) => l,
        Err(e) => {
            return Probe::Foreign {
                why: format!("{e:#}"),
                replaceable: false,
            }
        }
    };
    // `Value`, not `Response`: this must parse regardless of what the rest of the
    // schema looks like on the other side.
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
        return Probe::Foreign {
            why: "it answered with something that isn't JSON — this may not be a \
                  lait daemon at all"
                .into(),
            replaceable: false,
        };
    };
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("hello") => match v.get("protocol_version").and_then(|p| p.as_u64()) {
            Some(peer) => match check_control_protocol(peer as u32) {
                // The protocol matches, so this daemon is talkable-to. The only
                // remaining question is whether it is *ours* — a rebuilt binary
                // leaves the previous build's daemon holding the home, and it
                // answers every request perfectly while running code that is no
                // longer on disk. That was invisible here until now.
                Ok(()) => {
                    let ours = BuildFingerprint::here();
                    match v
                        .get("build")
                        .and_then(|b| serde_json::from_value::<BuildFingerprint>(b.clone()).ok())
                    {
                        Some(theirs) if ours.supersedes(&theirs) => Probe::Foreign {
                            why: format!(
                                "it is an older build of lait ({} from {}) than the one you \
                                 ran ({} from {}) — stopping it so this build can serve",
                                theirs.version, theirs.exe, ours.version, ours.exe
                            ),
                            replaceable: true,
                        },
                        // Same build, a newer one, or one that does not say:
                        // reuse it. Talking works, and evicting a daemon we are
                        // not ahead of is how two binaries run in turn come to
                        // kill each other's on every start.
                        _ => Probe::Healthy,
                    }
                }
                Err(e) => Probe::Foreign {
                    why: format!("{e:#}"),
                    // Only take over from a daemon that is *behind* us.
                    replaceable: (peer as u32) < CONTROL_PROTOCOL_VERSION,
                },
            },
            // Said hello without a version: not a shape we ever shipped.
            None => Probe::Foreign {
                why: "it answered `hello` without a protocol version".into(),
                replaceable: false,
            },
        },
        // A daemon that doesn't know `hello` rejects it as an unknown variant —
        // which is itself the answer: it predates the handshake (v0.4.8 or
        // earlier), so there is no version to negotiate. Definitively older,
        // therefore safe to replace.
        _ => Probe::Foreign {
            why: "it predates the control-protocol handshake (lait v0.4.8 or \
                  earlier), so this build cannot talk to it"
                .into(),
            replaceable: true,
        },
    }
}

/// Read and validate the peer's control protocol version.
///
/// Clients use the exact negotiated value to ensure the peer speaks the current
/// generic World envelope.
pub async fn peer_protocol_version(home: &Path) -> Result<u32> {
    let stream = connect_bounded(home).await?;
    let line = exchange_raw(
        stream,
        &ClientRequest::plain(Request::Hello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
        }),
    )
    .await?;
    let value: serde_json::Value =
        serde_json::from_str(&line).context("decode protocol handshake")?;
    if value.get("kind").and_then(|kind| kind.as_str()) != Some("hello") {
        return Err(anyhow!("daemon did not answer the protocol handshake"));
    }
    let version = value
        .get("protocol_version")
        .and_then(|version| version.as_u64())
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| anyhow!("daemon hello omitted a valid protocol version"))?;
    check_control_protocol(version)?;
    Ok(version)
}

/// Send one request to the daemon and read one response (one-shot path), as the
/// primary (human) identity.
pub async fn request(home: &Path, req: &Request) -> Result<Response> {
    request_as(home, req, None).await
}

/// Send one request acting as a named local identity (`act_as`) — the
/// multi-tenant path. `None` is identical to [`request`].
pub async fn request_as(home: &Path, req: &Request, act_as: Option<&str>) -> Result<Response> {
    request_as_routed(home, req, None, act_as).await
}

/// Send a request through an explicit route.
pub async fn request_routed(home: &Path, req: &Request, route: ControlRoute) -> Result<Response> {
    request_as_routed(home, req, Some(route), None).await
}

/// Send a request with both an explicit route and acting identity.
pub async fn request_as_routed(
    home: &Path,
    req: &Request,
    route: Option<ControlRoute>,
    act_as: Option<&str>,
) -> Result<Response> {
    send(
        home,
        &ClientRequest {
            route,
            if_running: false,
            act_as: act_as.map(str::to_string),
            request: req.clone(),
        },
    )
    .await
}

/// Send an envelope the caller already owns.
///
/// Split out because a caller that may have to send the same request twice — a
/// head that discovers, from the failure, that no daemon was listening — would
/// otherwise have to clone the whole `Request` to keep a copy. Building the
/// envelope once and lending it is what keeps the retry off the happy path's
/// bill.
pub async fn send(home: &Path, env: &ClientRequest) -> Result<Response> {
    let line = exchange_pooled(home, env).await?;
    serde_json::from_str(line.trim()).context("decode response")
}

/// How long the daemon leaves a connection open with nothing on it.
///
/// Read by both sides from here: the client's reuse window
/// ([`MAX_IDLE_AGE`]) is deliberately a fraction of it, and a fraction of a
/// number is only meaningful next to the number itself.
pub const IDLE_CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a connection may sit idle here before it is replaced rather than
/// reused.
///
/// Well under [`IDLE_CONNECTION_TIMEOUT`], and the gap is the point. If the
/// client could hand out a connection at the moment the daemon was reaping it,
/// every such race would surface as a request whose re-send has to be judged —
/// and that judgement is exactly what cannot be made safely from here. A
/// connect is cheaper than a wrong answer to it.
pub const MAX_IDLE_AGE: std::time::Duration = std::time::Duration::from_secs(30);

/// Idle connections one home keeps. Concurrent requests each need their own
/// stream — nothing multiplexes on this wire — so this is the fan-out the pool
/// absorbs before a request pays for a connect.
const MAX_IDLE_PER_HOME: usize = 4;

/// Control connections parked for reuse, keyed by the home they reach.
///
/// A head answers a browser that fans out — board, status, members, inbox, all
/// at once, then again on the next doorbell — and each of those used to open
/// its own socket. The daemon now serves many requests per connection, so the
/// connection is worth keeping.
///
/// `std::sync::Mutex`, not tokio's: every critical section is one `Vec` push or
/// pop with no await inside it, and an async mutex would add a scheduler hop to
/// the path that exists to remove one.
static IDLE_CONNECTIONS: LazyLock<Mutex<HashMap<PathBuf, Vec<Idle>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One parked connection and when it was parked.
struct Idle {
    io: BufReader<Stream>,
    since: std::time::Instant,
}

/// A connection to send on, and whether it has already carried a request.
///
/// That flag is the whole reason this is not just a connect: it is what
/// licenses the one re-send below, and it must never be set on a connection
/// this call opened itself.
async fn checkout(home: &Path) -> Result<(BufReader<Stream>, bool)> {
    match take_idle(home) {
        Some(io) => Ok((io, true)),
        None => Ok((BufReader::new(connect_bounded(home).await?), false)),
    }
}

fn take_idle(home: &Path) -> Option<BufReader<Stream>> {
    let mut pool = IDLE_CONNECTIONS.lock_recovering();
    let parked = pool.get_mut(home)?;
    while let Some(idle) = parked.pop() {
        if idle.since.elapsed() < MAX_IDLE_AGE {
            return Some(idle.io);
        }
        // Past the reuse window. Dropping it closes it, which is what we want:
        // the daemon is about to do the same from its side.
    }
    None
}

/// Park a connection that answered cleanly. One that did not is dropped, because
/// a stream whose framing may be mid-response poisons every request after it.
fn checkin(home: &Path, io: BufReader<Stream>) {
    let mut pool = IDLE_CONNECTIONS.lock_recovering();
    let parked = pool.entry(home.to_path_buf()).or_default();
    if parked.len() < MAX_IDLE_PER_HOME {
        parked.push(Idle {
            io,
            since: std::time::Instant::now(),
        });
    }
}

/// Where a round trip stopped — which is the only question that matters when
/// the connection was reused.
enum Interrupted {
    /// The request never landed. Nothing on the far side has seen it.
    Undelivered(anyhow::Error),
    /// The daemon closed without writing a byte.
    Closed,
    /// The daemon was written to and the read then failed. Whether it ran is
    /// unknown, so this is never re-sent.
    Failed(anyhow::Error),
}

impl Interrupted {
    /// The error a caller sees. Only [`Interrupted::Undelivered`] becomes the
    /// [`Undelivered`] type, because only it is a licence to re-send.
    fn into_error(self) -> anyhow::Error {
        match self {
            Interrupted::Undelivered(error) => Undelivered(format!("{error:#}")).into(),
            Interrupted::Closed => {
                anyhow!("the daemon closed the connection without answering")
            }
            Interrupted::Failed(error) => error,
        }
    }
}

/// One request and one response on a connection the caller owns.
async fn round_trip<T: Serialize>(
    io: &mut BufReader<Stream>,
    env: &T,
) -> std::result::Result<String, Interrupted> {
    let mut line = match serde_json::to_string(env) {
        Ok(line) => line,
        // Re-encoding will fail identically, but nothing was written, and the
        // caller's licence is about the wire rather than about the retry.
        Err(error) => return Err(Interrupted::Undelivered(anyhow!("encode request: {error}"))),
    };
    line.push('\n');
    if let Err(error) = io.write_all(line.as_bytes()).await {
        return Err(Interrupted::Undelivered(anyhow!("write request: {error}")));
    }
    if let Err(error) = io.flush().await {
        return Err(Interrupted::Undelivered(anyhow!("flush request: {error}")));
    }
    let mut response = String::new();
    match io.read_line(&mut response).await {
        Ok(0) => Err(Interrupted::Closed),
        Ok(_) => Ok(response),
        Err(error) => Err(unanswered("read response", &response, error)),
    }
}

/// Classify a failed read by whether any of the answer had arrived.
///
/// **Not by the error code.** A connection the daemon has already closed does
/// not announce itself the same way on every platform: a Windows pipe reports it
/// on the write, a Unix socket that still holds bytes we sent reports
/// `ECONNRESET` on the read, and a clean close reports end-of-file. Keying on
/// any one of those spellings means the other two surface a reaped connection as
/// a failed request — which is what happened, on Linux, to a test that restarts
/// its daemon.
///
/// The fact that actually decides it is whether the daemon answered. Nothing
/// received means nothing was answered, and since a request is only ever
/// dispatched after being read in full, nothing was answered means nothing ran.
fn unanswered(what: &str, received: &str, error: std::io::Error) -> Interrupted {
    if received.is_empty() {
        Interrupted::Closed
    } else {
        Interrupted::Failed(anyhow!("{what}: {error}"))
    }
}

/// A round trip on a pooled connection, with one re-send if a *reused* one was
/// already gone.
///
/// The re-send is the same rule an HTTP client applies to a keep-alive
/// connection, and it rests on the same fact: a connection this process parked
/// and the daemon then closed never carried the request, so sending it again
/// cannot repeat anything. A connection opened by *this* call gets no such
/// licence — its failure is a real failure and is reported as one, exactly as
/// before the pool existed.
async fn exchange_pooled<T: Serialize>(home: &Path, env: &T) -> Result<String> {
    let (mut io, reused) = checkout(home).await?;
    match round_trip(&mut io, env).await {
        Ok(line) => {
            checkin(home, io);
            return Ok(line);
        }
        Err(interrupted) => {
            if !may_resend(reused, &interrupted) {
                return Err(interrupted.into_error());
            }
        }
    }
    let mut io = BufReader::new(connect_bounded(home).await?);
    let line = round_trip(&mut io, env)
        .await
        .map_err(Interrupted::into_error)?;
    checkin(home, io);
    Ok(line)
}

/// Whether this failure licenses sending the same request a second time.
///
/// The whole rule, in one place, because both pooled paths must answer it
/// identically and because it is the only thing standing between a re-send and
/// a repeated mutation.
fn may_resend(reused: bool, interrupted: &Interrupted) -> bool {
    reused
        && matches!(
            interrupted,
            Interrupted::Undelivered(_) | Interrupted::Closed
        )
}

/// One framed World call: header line, payload bytes, and the same in return.
///
/// Mirrors [`exchange_pooled`] — same pool, same one re-send under
/// [`may_resend`] — and differs only in what crosses the wire between the
/// newlines.
async fn exchange_framed(
    home: &Path,
    frame: &WorldCallFrame,
    payload: &[u8],
) -> Result<(ReplyFrame, Vec<u8>)> {
    let (mut io, reused) = checkout(home).await?;
    match framed_round_trip(&mut io, frame, payload).await {
        Ok(answer) => {
            checkin(home, io);
            return Ok(answer);
        }
        Err(interrupted) => {
            if !may_resend(reused, &interrupted) {
                return Err(interrupted.into_error());
            }
        }
    }
    let mut io = BufReader::new(connect_bounded(home).await?);
    let answer = framed_round_trip(&mut io, frame, payload)
        .await
        .map_err(Interrupted::into_error)?;
    checkin(home, io);
    Ok(answer)
}

/// A framed call and its framed answer on a connection the caller owns.
///
/// **Nothing here may return early between the header and its bytes.** A
/// connection is parked for reuse only on the `Ok` path, so a framing mistake
/// costs one connection rather than every request that would have followed on
/// it — which is the hazard that arrived the moment connections started being
/// reused, and the reason the length is checked before it is believed.
async fn framed_round_trip(
    io: &mut BufReader<Stream>,
    frame: &WorldCallFrame,
    payload: &[u8],
) -> std::result::Result<(ReplyFrame, Vec<u8>), Interrupted> {
    use tokio::io::AsyncReadExt;

    let mut line = match serde_json::to_string(frame) {
        Ok(line) => line,
        Err(error) => {
            return Err(Interrupted::Undelivered(anyhow!(
                "encode World call: {error}"
            )))
        }
    };
    line.push('\n');
    if let Err(error) = io.write_all(line.as_bytes()).await {
        return Err(Interrupted::Undelivered(anyhow!(
            "write World call: {error}"
        )));
    }
    if let Err(error) = io.write_all(payload).await {
        // Undelivered, and the receiver is what makes that true rather than the
        // ordering here: a World call is dispatched only after its declared
        // bytes have been read in full, so a header that arrives without its
        // payload is read, found short, and dropped. Nothing ran.
        //
        // Getting this wrong is not theoretical. Classifying it as delivered
        // made every parked-connection reap on a *write* surface as a failed
        // request — "the pipe is being closed" — because a first write into a
        // closed pipe can succeed and only the second one reports it.
        return Err(Interrupted::Undelivered(anyhow!(
            "write World payload: {error}"
        )));
    }
    if let Err(error) = io.flush().await {
        return Err(Interrupted::Undelivered(anyhow!(
            "flush World call: {error}"
        )));
    }

    let mut header = String::new();
    {
        let mut bounded = (&mut *io).take(MAX_CONTROL_LINE_BYTES);
        match bounded.read_line(&mut header).await {
            Ok(0) => return Err(Interrupted::Closed),
            Ok(_) => {}
            Err(error) => return Err(unanswered("read World reply", &header, error)),
        }
    }
    let reply: ReplyFrame = match serde_json::from_str(header.trim()) {
        Ok(reply) => reply,
        Err(error) => return Err(Interrupted::Failed(anyhow!("decode World reply: {error}"))),
    };
    let ReplyFrameOutcome::Ok { len } = reply.outcome else {
        return Ok((reply, Vec::new()));
    };
    let len = refuse_oversized_payload(len).map_err(Interrupted::Failed)?;
    let mut payload = vec![0u8; len];
    if let Err(error) = io.read_exact(&mut payload).await {
        return Err(Interrupted::Failed(anyhow!("read World payload: {error}")));
    }
    Ok((reply, payload))
}

/// Open the control channel, or give up saying so.
///
/// **The failure this prevents:** connecting to a Windows named pipe with no
/// free instance *parks* rather than erroring — the same fact
/// [`probe`] is wrapped in a timeout for. Since the send is now the probe
/// (nothing runs before it on the daemon path), an unbounded connect is a head
/// that hangs forever instead of a request that fails in five seconds, and it
/// hangs the axum handler or MCP tool call with it.
///
/// The *reply* is deliberately left unbounded. A host request legitimately
/// holds the socket open for as long as its work takes — entering a Space waits
/// out `ADMISSION_DEADLINE`, a self-update downloads a release — and a ceiling
/// low enough to catch a wedged daemon would abort those instead. Diagnosing a
/// daemon that answers the door and then says nothing stays [`probe`]'s job,
/// which the error path already runs.
///
/// Every failure here is an [`Undelivered`], because nothing has been written
/// yet — that type is what licenses a caller's re-send.
async fn connect_bounded(home: &Path) -> Result<Stream> {
    let name = control_name(home)?;
    match tokio::time::timeout(PROBE_TIMEOUT, Stream::connect(name)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(error)) => Err(Undelivered(match error.kind() {
            // A control channel exists only while a daemon is listening on it:
            // a Windows named pipe and a unix socket both answer a connect with
            // `NotFound` (or `ConnectionRefused`, for a socket file nobody is
            // accepting on) when there is nobody there.
            //
            // Said in the daemon's own words rather than the OS's, because "the
            // system cannot find the file specified" sends somebody looking for
            // a missing *file* — and the daemon home they will go and inspect is
            // full of files, all present and all irrelevant. The channel is not
            // one of them.
            //
            // The home is named because the channel's identity is derived from
            // it: two processes that disagree about `LAIT_HOME` derive two
            // different channels and never see each other, with exactly this
            // error and nothing on screen to say which home either one meant.
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => format!(
                "no Lait daemon is running for this identity ({}) — start one with `lait daemon`",
                home.display()
            ),
            _ => format!("connect to daemon: {error}"),
        })
        .into()),
        Err(_) => Err(Undelivered(format!(
            "the Lait daemon is not answering (no connection within {}s) — it may be \
             wedged or shutting down",
            PROBE_TIMEOUT.as_secs()
        ))
        .into()),
    }
}

/// Send a routed request that must not activate a vacant Orbit.
pub async fn request_routed_if_running(
    home: &Path,
    req: &Request,
    route: ControlRoute,
) -> Result<Response> {
    let line = exchange_pooled(home, &ClientRequest::routed_if_running(req.clone(), route)).await?;
    serde_json::from_str(line.trim()).context("decode response")
}

/// Send one product-neutral World call through the identity-scoped daemon.
pub async fn call_world(
    home: &Path,
    route: ControlRoute,
    call: Call,
    act_as: Option<&str>,
) -> Result<Reply> {
    call_world_envelope(
        home,
        &WorldClientRequest::new(route, call, act_as.map(str::to_string)),
    )
    .await
}

/// Send a World envelope the caller already owns.
///
/// The World-call path is the latency-critical one, so it must never copy a
/// call payload just to hold a spare for a retry it will almost certainly not
/// need. See [`send`] for the same reasoning on the control path.
pub async fn call_world_envelope(home: &Path, env: &WorldClientRequest) -> Result<Reply> {
    let frame = WorldCallFrame {
        route: env.route.clone(),
        act_as: env.act_as.clone(),
        call: CallFrame::of(&env.call),
    };
    let (reply, payload) = exchange_framed(home, &frame, env.call.payload()).await?;
    let outcome = match reply.outcome {
        ReplyFrameOutcome::Ok { .. } => Ok(payload),
        ReplyFrameOutcome::Error { error } => Err(error),
    };
    Reply::from_parts(reply.world, reply.operation, reply.version, outcome)
        .map_err(|error| anyhow!("decode World reply: {error}"))
}

/// The same round trip, stopping at the raw response line.
///
/// Split from [`exchange`] for [`probe`]: typed decoding is exactly what a
/// version-mismatched daemon breaks, so the handshake has to look at the bytes
/// before serde gets an opinion about them.
async fn exchange_raw<T: Serialize>(stream: Stream, env: &T) -> Result<String> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut line = serde_json::to_string(env).context("encode request")?;
    line.push('\n');
    write_half
        .write_all(line.as_bytes())
        .await
        .context("write request")?;
    write_half.flush().await.ok();

    let mut reader = BufReader::new(read_half);
    let mut resp_line = String::new();
    reader
        .read_line(&mut resp_line)
        .await
        .context("read response")?;
    Ok(resp_line)
}

/// Send one content call and read the whole answer.
///
/// For calls whose answer is bounded: a status, a forget, or a range read,
/// which one call may never return more than
/// [`runtime::plane::freight::content::MAX_RANGE_BYTES`] of. An upload does not come
/// through here — see [`ContentUpload`], which streams.
///
/// The caller builds the whole envelope rather than passing a call and having
/// this fill in the rest. That is what makes the adversarial shapes
/// expressible: a header declaring a body it never sends is exactly the request
/// a daemon has to refuse without reading, and a helper that always wrote
/// `body_len: 0` could not ask for it.
pub async fn content_call(
    home: &Path,
    request: &ContentClientRequest,
) -> Result<(ContentReply, Vec<u8>)> {
    let stream = connect_bounded(home).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_header(&mut write_half, request).await?;
    let mut reader = BufReader::new(read_half);
    read_content_reply(&mut reader).await
}

/// One content call with no body, which is every call but a write.
pub fn content_request(route: ControlRoute, call: ContentCall) -> ContentClientRequest {
    ContentClientRequest {
        content: call,
        route,
        act_as: None,
        body_len: 0,
    }
}

/// A streaming upload on the control channel.
///
/// Open, push, finish. The length is declared up front and is authoritative on
/// both sides — a caller that pushes fewer bytes than it promised gets an
/// error rather than a truncated content, and the daemon refuses the first byte
/// past the declaration rather than reading to the end to find out.
///
/// Exists rather than a `Vec<u8>` parameter because the whole point of moving
/// attachments off the inline path is not to hold one in memory. A caller
/// forwarding an HTTP body forwards it.
pub struct ContentUpload {
    write_half: tokio::io::WriteHalf<Stream>,
    reader: BufReader<tokio::io::ReadHalf<Stream>>,
    declared: u64,
    sent: u64,
}

impl ContentUpload {
    /// Open an upload of exactly `declared_len` bytes.
    pub async fn open(
        home: &Path,
        route: ControlRoute,
        operation: [u8; 16],
        act_as: Option<&str>,
        declared_len: u64,
    ) -> Result<Self> {
        let stream = connect_bounded(home).await?;
        let (read_half, mut write_half) = tokio::io::split(stream);
        write_header(
            &mut write_half,
            &ContentClientRequest {
                content: ContentCall::Write {
                    operation: data_encoding::HEXLOWER.encode(&operation),
                },
                route,
                act_as: act_as.map(str::to_string),
                body_len: declared_len,
            },
        )
        .await?;
        Ok(Self {
            write_half,
            reader: BufReader::new(read_half),
            declared: declared_len,
            sent: 0,
        })
    }

    /// Push the next piece. Refuses locally rather than sending a byte past the
    /// declaration, so the caller's own bug is reported where it happened
    /// instead of arriving as a remote refusal.
    pub async fn push(&mut self, bytes: &[u8]) -> Result<()> {
        let next = self.sent.saturating_add(bytes.len() as u64);
        if next > self.declared {
            return Err(anyhow!(
                "upload declared {} bytes and tried to send {next}",
                self.declared
            ));
        }
        self.write_half
            .write_all(bytes)
            .await
            .context("write content body")?;
        self.sent = next;
        Ok(())
    }

    /// Finish and read the answer.
    pub async fn finish(mut self) -> Result<ContentReply> {
        if self.sent != self.declared {
            return Err(anyhow!(
                "upload declared {} bytes and sent {}",
                self.declared,
                self.sent
            ));
        }
        self.write_half.flush().await.ok();
        let (reply, _) = read_content_reply(&mut self.reader).await?;
        Ok(reply)
    }
}

async fn write_header<T: Serialize>(
    write_half: &mut tokio::io::WriteHalf<Stream>,
    header: &T,
) -> Result<()> {
    let mut line = serde_json::to_string(header).context("encode content request")?;
    line.push('\n');
    write_half
        .write_all(line.as_bytes())
        .await
        .context("write content request")?;
    write_half.flush().await.ok();
    Ok(())
}

/// Read one content answer: its header line, and the body the header declares.
async fn read_content_reply(
    reader: &mut BufReader<tokio::io::ReadHalf<Stream>>,
) -> Result<(ContentReply, Vec<u8>)> {
    use tokio::io::AsyncReadExt;

    let mut line = String::new();
    {
        // The header is bounded, so read it bounded. An answer with no newline
        // is a daemon that stopped mid-sentence, not an invitation to keep
        // allocating.
        let mut bounded = reader.take(MAX_CONTROL_FRAME_BYTES);
        bounded
            .read_line(&mut line)
            .await
            .context("read content reply")?;
    }
    if line.trim().is_empty() {
        return Err(anyhow!("the daemon closed without answering"));
    }
    let reply: ContentReply = serde_json::from_str(line.trim()).context("decode content reply")?;
    let ContentReply::ContentStream { len } = reply else {
        return Ok((reply, Vec::new()));
    };
    if len > runtime::plane::freight::content::MAX_RANGE_BYTES as u64 {
        return Err(anyhow!(
            "the daemon offered {len} bytes in one answer, past the {} this \
             channel carries",
            runtime::plane::freight::content::MAX_RANGE_BYTES
        ));
    }
    let mut body = vec![0u8; len as usize];
    reader
        .read_exact(&mut body)
        .await
        .context("read content body")?;
    Ok((reply, body))
}

/// How much of an upload is in flight between the socket and the sealer.
///
/// Two pieces of a quarter-megabyte chunk, so the sealer never waits on the
/// socket for a chunk it could already be working on, and the socket cannot run
/// ahead into unbounded memory. Backpressure is the point: without it a fast
/// client on a loopback socket outruns a disk and the difference accumulates in
/// this process.
const UPLOAD_PIECES_IN_FLIGHT: usize = 2;

/// One piece of an upload as it crosses from the socket to the sealer.
type UploadPiece = std::io::Result<Vec<u8>>;

/// The sealer's end of a streaming upload: a synchronous [`std::io::Read`] fed
/// by an async task pumping the socket.
///
/// The content plane seals from a `Read` on a blocking thread, and the socket
/// is async — so something has to cross that line, and this is it rather than
/// a buffer holding the whole upload. A 256 MiB attachment never exists in this
/// process as one allocation.
///
/// Cancellation arrives by the sender being dropped: the next read sees the
/// channel closed before its declared length arrived and returns
/// `UnexpectedEof`, which fails the ingest, which leaves nothing durable. That
/// is why the length is declared up front — "as much as arrived" and "all of
/// it" are otherwise the same thing, and a truncated upload would commit a
/// permanently wrong content that hashes perfectly well.
pub struct UploadReader {
    pieces: tokio::sync::mpsc::Receiver<UploadPiece>,
    current: Vec<u8>,
    at: usize,
    outstanding: u64,
}

impl UploadReader {
    fn new(pieces: tokio::sync::mpsc::Receiver<UploadPiece>, declared: u64) -> Self {
        Self {
            pieces,
            current: Vec::new(),
            at: 0,
            outstanding: declared,
        }
    }
}

impl std::io::Read for UploadReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.current.len() {
            if self.outstanding == 0 {
                return Ok(0);
            }
            match self.pieces.blocking_recv() {
                Some(Ok(piece)) => {
                    self.current = piece;
                    self.at = 0;
                }
                Some(Err(error)) => return Err(error),
                // The pump stopped before the declared length arrived: a
                // truncated body, or a caller that went away mid-upload. Both
                // are the same answer, and neither may look like EOF.
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("upload ended {} bytes early", self.outstanding),
                    ))
                }
            }
        }
        let take = out.len().min(self.current.len() - self.at);
        out[..take].copy_from_slice(&self.current[self.at..self.at + take]);
        self.at += take;
        self.outstanding = self.outstanding.saturating_sub(take as u64);
        Ok(take)
    }
}

/// Open the two ends of a streaming upload: the sealer's reader, and the task
/// that feeds it exactly `declared` bytes from `reader`.
///
/// The body must be read through whatever reader consumed the header line. A
/// `BufReader` has already pulled the first bytes of the body into its buffer
/// while looking for the newline, so reading from the raw half instead silently
/// drops them — and the content that commits is wrong in a way that hashes
/// fine. A small body hides this completely, which is why it is stated here.
pub fn upload_body<R>(
    mut reader: R,
    declared: u64,
) -> (UploadReader, impl std::future::Future<Output = R>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncReadExt;

    let (tx, rx) = tokio::sync::mpsc::channel::<UploadPiece>(UPLOAD_PIECES_IN_FLIGHT);
    let pump = async move {
        let mut left = declared;
        while left > 0 {
            let want = left.min(UPLOAD_PIECE_BYTES as u64) as usize;
            let mut piece = vec![0u8; want];
            match reader.read_exact(&mut piece).await {
                Ok(_) => {
                    if tx.send(Ok(piece)).await.is_err() {
                        break;
                    }
                    left -= want as u64;
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            }
        }
        reader
    };
    (UploadReader::new(rx, declared), pump)
}

/// One piece of an upload as it moves across the socket.
const UPLOAD_PIECE_BYTES: usize = 256 * 1024;

/// A live dirty-notification subscription — the client side of [`Request::Subscribe`]
/// stream. Holds the whole duplex stream (never split, so nothing
/// leaks); the subscribe verb is write-once, then read-many.
pub struct Subscription {
    reader: BufReader<Stream>,
}

/// A standing Live projection stream. Each frame is a complete [`Response::Live`]
/// view, so a slow or reconnecting adapter can replace rather than replay.
pub struct LiveSubscription {
    reader: BufReader<Stream>,
}

impl LiveSubscription {
    pub async fn next(&mut self) -> Result<Option<Response>> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .await
            .context("read Live view")?;
        if n == 0 {
            return Ok(None);
        }
        serde_json::from_str(line.trim())
            .context("decode Live view")
            .map(Some)
    }
}

impl Subscription {
    /// Read the next [`Doorbell`] frame. Returns `None` at EOF (daemon stopped).
    pub async fn next(&mut self) -> Result<Option<Doorbell>> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .await
            .context("read doorbell")?;
        if n == 0 {
            return Ok(None);
        }
        let db: Doorbell = serde_json::from_str(line.trim()).context("decode doorbell")?;
        Ok(Some(db))
    }
}

/// Open a streaming [`Request::Subscribe`] connection.
pub async fn subscribe(home: &Path, since: u64) -> Result<Subscription> {
    subscribe_routed(home, since, None).await
}

/// Open a subscription through an explicit Space route.
pub async fn subscribe_routed(
    home: &Path,
    since: u64,
    route: Option<ControlRoute>,
) -> Result<Subscription> {
    let mut stream = connect_bounded(home).await?;
    let envelope = ClientRequest {
        route,
        if_running: false,
        act_as: None,
        request: Request::Subscribe { since },
    };
    let mut line = serde_json::to_string(&envelope).context("encode subscribe")?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .context("write subscribe")?;
    stream.flush().await.ok();
    Ok(Subscription {
        reader: BufReader::new(stream),
    })
}

/// Open an event-driven Live stream through an explicit Space route.
pub async fn subscribe_live_routed(
    home: &Path,
    route: ControlRoute,
    issue: Option<String>,
) -> Result<LiveSubscription> {
    let mut stream = connect_bounded(home).await?;
    let envelope = ClientRequest {
        route: Some(route),
        if_running: false,
        act_as: None,
        request: Request::LiveSubscribe { issue },
    };
    let mut line = serde_json::to_string(&envelope).context("encode Live subscribe")?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .context("write Live subscribe")?;
    stream.flush().await.ok();
    Ok(LiveSubscription {
        reader: BufReader::new(stream),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_request_envelope_is_wire_backward_compatible() {
        // No route or selector serializes to EXACTLY the bare request an older
        // client sends (the flatten + skip_serializing_if contract).
        let bare = Request::MemberRemove { who: "abc".into() };
        let env = ClientRequest::plain(bare.clone());
        let env_json = serde_json::to_value(&env).unwrap();
        let bare_json = serde_json::to_value(&bare).unwrap();
        assert_eq!(
            env_json, bare_json,
            "a no-selector envelope IS the bare request"
        );
        // A bare request decodes as an envelope with no selector.
        let decoded: ClientRequest = serde_json::from_value(bare_json.clone()).unwrap();
        assert!(decoded.route.is_none());
        assert!(!decoded.if_running);
        assert!(decoded.act_as.is_none());
        assert_eq!(serde_json::to_value(&decoded.request).unwrap(), bare_json);
    }

    #[test]
    fn client_request_flatten_round_trips_with_route_selector_and_scalars() {
        // The flatten gotcha: a route and selector alongside a Request carrying
        // scalar fields must survive a JSON round trip.
        for req in [
            Request::Invite {
                role: Some("contributor".into()),
                reusable: true,
                ttl_hours: Some(48),
            },
            Request::Status,
            Request::Whoami,
        ] {
            let route = ControlRoute::Orbit {
                address: OrbitAddress::for_store(
                    Path::new("/tmp/test-orbit"),
                    mechanics::ids::SpaceId::from_digest([4; 16]),
                ),
            };
            let env = ClientRequest::routed(req.clone(), route.clone(), Some("agent-x".into()));
            let json = serde_json::to_string(&env).unwrap();
            assert!(
                json.contains("\"scope\":\"space\""),
                "route present: {json}"
            );
            assert!(
                json.contains("\"orbit\":\"orb_"),
                "local Orbit address present: {json}"
            );
            assert!(
                json.contains("\"act_as\":\"agent-x\""),
                "selector present: {json}"
            );
            assert!(
                !json.contains("if_running"),
                "ordinary dispatch omits passive intent: {json}"
            );
            assert!(
                !json.contains("allowed_orbits") && !json.contains("default_orbit"),
                "trusted ClientScope must not become a wire claim: {json}"
            );
            let back: ClientRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(back.route, Some(route));
            assert_eq!(back.act_as.as_deref(), Some("agent-x"));
            assert_eq!(
                serde_json::to_value(&back.request).unwrap(),
                serde_json::to_value(&req).unwrap(),
                "the flattened request must survive: {json}"
            );
        }
    }

    /// An unmeasured storage figure stays unmeasured on the wire.
    ///
    /// This is the release-gate rule in its smallest form. If `None` encoded as
    /// `0` — or decoded back as one — a Storage surface would draw a confident
    /// `0 B / 0 objects / verified at the epoch` for a Space nobody managed to
    /// measure, and there would be nothing in the reply to distrust.
    #[test]
    fn an_unmeasured_storage_figure_is_absent_on_the_wire_and_never_a_zero() {
        let unmeasured = Response::Storage {
            bytes_on_disk: None,
            object_count: None,
            last_verified_ms: None,
        };
        let json = serde_json::to_value(&unmeasured).unwrap();
        assert_eq!(json["bytes_on_disk"], serde_json::Value::Null, "{json}");
        assert_eq!(json["object_count"], serde_json::Value::Null, "{json}");
        assert_eq!(json["last_verified_ms"], serde_json::Value::Null, "{json}");

        // And the two states stay distinguishable in both directions: a store
        // measured at zero bytes is a different claim from one nobody measured.
        let measured_empty = serde_json::to_value(Response::Storage {
            bytes_on_disk: Some(0),
            object_count: Some(0),
            last_verified_ms: Some(0),
        })
        .unwrap();
        assert_ne!(json, measured_empty);

        let back: Response = serde_json::from_value(json).unwrap();
        let Response::Storage {
            bytes_on_disk,
            object_count,
            last_verified_ms,
        } = back
        else {
            panic!("a storage reply must decode as one");
        };
        assert_eq!(bytes_on_disk, None);
        assert_eq!(object_count, None);
        assert_eq!(last_verified_ms, None);
    }

    #[test]
    fn passive_dispatch_intent_is_explicit_and_round_trips() {
        let route = ControlRoute::Orbit {
            address: OrbitAddress::for_store(
                Path::new("/tmp/passive-orbit"),
                mechanics::ids::SpaceId::from_digest([5; 16]),
            ),
        };
        let env = ClientRequest::routed_if_running(Request::Status, route.clone());
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"if_running\":true"), "{json}");
        let back: ClientRequest = serde_json::from_str(&json).unwrap();
        assert!(back.if_running);
        assert_eq!(back.route, Some(route));
        assert!(matches!(back.request, Request::Status));
    }

    #[test]
    fn control_protocol_window_accepts_supported_and_refuses_outside() {
        // Everything in [MIN_SUPPORTED_CONTROL_PROTOCOL, CONTROL_PROTOCOL_VERSION]
        // is accepted — the mixed-version window.
        assert!(check_control_protocol(CONTROL_PROTOCOL_VERSION).is_ok());
        assert!(check_control_protocol(MIN_SUPPORTED_CONTROL_PROTOCOL).is_ok());

        // A daemon newer than we understand: we must upgrade, so say so.
        let newer = check_control_protocol(CONTROL_PROTOCOL_VERSION + 1).unwrap_err();
        assert!(
            newer.to_string().contains("upgrade lait"),
            "an out-of-window daemon must name the way out; got: {newer}",
        );

        // A daemon older than the window: it must be restarted onto this build.
        let older = check_control_protocol(MIN_SUPPORTED_CONTROL_PROTOCOL - 1).unwrap_err();
        assert!(
            older.to_string().contains("stop that daemon"),
            "an out-of-window daemon must name the way out; got: {older}",
        );
    }

    fn build(version: &str, exe: &str, built: u64) -> BuildFingerprint {
        BuildFingerprint {
            version: version.into(),
            exe: exe.into(),
            built,
        }
    }

    /// Only an *older* daemon is displaced.
    ///
    /// The failure this guards is not a wrong answer, it is a livelock: if
    /// "different" were enough, two binaries run in turn would each evict the
    /// other's daemon at startup and a machine running both would never keep one
    /// up. Age gives a total order, so at most one direction ever acts.
    #[test]
    fn only_an_older_daemon_is_displaced() {
        let new = build("0.7.2", "/t/lait", 200);
        let old = build("0.7.2", "/t/lait", 100);
        assert!(new.supersedes(&old), "a newer build takes over");
        assert!(
            !old.supersedes(&new),
            "an older build must never evict a newer"
        );
        assert!(
            !new.supersedes(&new),
            "the same build is reused, not restarted"
        );
    }

    /// Same age, different file, is *not* a takeover.
    ///
    /// Two binaries stamped the same second (a scripted rebuild does this) have
    /// no order between them. Evicting on inequality alone would put exactly the
    /// pair most likely to alternate into the livelock above.
    #[test]
    fn an_equal_timestamp_is_never_a_takeover() {
        let a = build("0.7.2", "/t/a", 100);
        let b = build("0.7.2", "/t/b", 100);
        assert!(!a.supersedes(&b));
        assert!(!b.supersedes(&a));
    }

    /// A client is not always the binary it would spawn.
    ///
    /// The integration-test harness is its own executable and spawns `lait` as
    /// the daemon; anything embedding this crate is in the same position. Judging
    /// a daemon stale because it is *a different file* would have every one of
    /// those evict a daemon it did not start and has no quarrel with — which is
    /// exactly what happened the first time this rule was written, and it took
    /// the launcher-safety test down with it.
    #[test]
    fn a_different_executable_is_never_stale_however_new_we_are() {
        let harness = build("0.7.2", "/t/lait-it.exe", 9_999);
        let daemon = build("0.7.2", "/t/lait.exe", 1);
        assert!(
            !harness.supersedes(&daemon),
            "a client that is a different binary must reuse the daemon, not evict it"
        );
    }

    /// A rebuild of the same path at the same version still counts — which is
    /// the entire point, and the case a version comparison alone would miss.
    #[test]
    fn a_rebuild_of_the_same_path_is_detected() {
        let before = build("0.7.2-dev.abc", "/t/lait", 10);
        let after = build("0.7.2-dev.abc", "/t/lait", 11);
        assert_ne!(before, after);
        assert!(after.supersedes(&before));
    }

    /// The handshake's own shape is the one thing that can never be allowed to
    /// drift: `probe` reads `kind` and `protocol_version` out of raw JSON, so
    /// renaming either would silently turn every version mismatch back into the
    /// unreadable failure this exists to replace.
    #[test]
    fn the_hello_reply_keeps_the_field_names_probe_reads_raw() {
        let json = serde_json::to_value(Response::Hello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            build: None,
        })
        .unwrap();
        assert_eq!(json["kind"], "hello");
        assert_eq!(json["protocol_version"], CONTROL_PROTOCOL_VERSION);
    }

    /// A pre-handshake daemon (v0.4.8 and earlier) rejects `hello` as an unknown
    /// variant. That rejection is load-bearing: it is how `probe` recognises a
    /// daemon too old to have a version at all.
    #[test]
    fn hello_serializes_as_the_cmd_a_pre_handshake_daemon_will_reject() {
        let json = serde_json::to_value(Request::Hello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
        })
        .unwrap();
        assert_eq!(json["cmd"], "hello");
    }

    /// Skew is **not** symmetric, and the repair must not pretend it is.
    ///
    /// Taking over from an older daemon is a fix; taking over from a newer one is
    /// a downgrade — and if it has written the store at a newer `SCHEMA_VERSION`,
    /// `store::check_schema_version` then refuses to open it and the node is
    /// stuck. So `replaceable` must be false for everything except a daemon we can
    /// positively identify as behind us.
    #[test]
    fn only_a_daemon_behind_us_is_ever_replaceable() {
        let foreign = |v: serde_json::Value| -> bool {
            // Mirrors probe's decision on a parsed hello reply.
            let peer = v["protocol_version"].as_u64().unwrap() as u32;
            assert!(
                check_control_protocol(peer).is_err(),
                "must be out of window"
            );
            peer < CONTROL_PROTOCOL_VERSION
        };
        assert!(
            !foreign(serde_json::json!({"protocol_version": CONTROL_PROTOCOL_VERSION + 1})),
            "a daemon ahead of this build must never be offered up for replacement",
        );
        // The mirror case only exists once the window has moved past v1; assert it
        // the moment it can be expressed, so raising MIN doesn't silently skip it.
        if MIN_SUPPORTED_CONTROL_PROTOCOL > 1 {
            assert!(foreign(
                serde_json::json!({"protocol_version": MIN_SUPPORTED_CONTROL_PROTOCOL - 1})
            ));
        }
    }

    /// The generation a caller does not have is absent, not zero.
    ///
    /// `Option<u64>` rather than a defaulted `u64` because the daemon's counter
    /// starts at zero: a first read spelled `since_generation: 0` would be
    /// answered `live_unchanged` about a view nobody has seen. Pinned on the
    /// wire because that is where the distinction has to survive.
    #[test]
    fn a_first_live_read_carries_no_generation_at_all() {
        let json = serde_json::to_value(Request::Live {
            since_generation: None,
            issue: None,
        })
        .unwrap();
        assert_eq!(json["cmd"], "live");
        assert!(json["since_generation"].is_null());

        let held = serde_json::to_value(Request::Live {
            since_generation: Some(0),
            issue: Some("iss_01".into()),
        })
        .unwrap();
        assert_eq!(held["since_generation"], 0);
        assert_eq!(held["issue"], "iss_01");

        // And absence decodes back to absence rather than to zero.
        let decoded: Request = serde_json::from_value(serde_json::json!({"cmd": "live"})).unwrap();
        match decoded {
            Request::Live {
                since_generation,
                issue,
            } => {
                assert!(since_generation.is_none());
                assert!(issue.is_none());
            }
            other => panic!("decoded as {other:?}"),
        }
    }

    /// "Unchanged" is a tag, never a missing field.
    ///
    /// A client branches on `kind` everywhere else on this channel. Spelling
    /// "nothing moved" as an absent `entries` would make an empty table and an
    /// unchanged one arrive looking alike, and leave the difference to whether
    /// somebody remembered to check.
    #[test]
    fn an_unchanged_live_view_is_its_own_kind() {
        let unchanged = serde_json::to_value(Response::LiveUnchanged { generation: 9 }).unwrap();
        assert_eq!(unchanged["kind"], "live_unchanged");
        assert!(unchanged.get("entries").is_none());

        let empty = serde_json::to_value(Response::Live {
            generation: 9,
            partial: false,
            entries: vec![],
        })
        .unwrap();
        assert_eq!(empty["kind"], "live");
        assert_eq!(empty["entries"], serde_json::json!([]));
    }

    /// The shapes a browser branches on, pinned by their tags.
    #[test]
    fn live_rows_name_an_actor_and_tag_every_nested_union() {
        let entry = LiveEntry {
            actor: "act_ab".into(),
            scope: LiveScope::Field {
                world: "com.lait.issues".into(),
                body: "aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                field: "description".into(),
            },
            kind: "caret".into(),
            age_ms: 40,
            uncertain: false,
            caret: Some(CaretPosition::At { position: 12 }),
            focus: Some(CaretPosition::Drifted),
            preview: None,
        };
        let json = serde_json::to_value(Response::Live {
            generation: 1,
            partial: true,
            entries: vec![entry],
        })
        .unwrap();
        let row = &json["entries"][0];
        assert_eq!(row["actor"], "act_ab");
        assert_eq!(row["scope"]["scope"], "text_caret");
        assert_eq!(
            row["caret"],
            serde_json::json!({"caret": "at", "position": 12})
        );
        assert_eq!(row["focus"], serde_json::json!({"caret": "drifted"}));
        assert_eq!(json["partial"], true);
    }

    #[test]
    fn a_drained_signal_says_how_many_were_lost() {
        let json = serde_json::to_value(Response::Signals {
            signals: vec![SignalEntry {
                actor: "act_ab".into(),
                connection_id: "00".repeat(16),
                connection_epoch: "11".repeat(16),
                signal: SignalBody::FileOffer {
                    content: "22".repeat(32),
                    plaintext_len: 9,
                    display_name: "notes.md".into(),
                    media_type: "text/markdown".into(),
                },
            }],
            dropped: 3,
        })
        .unwrap();
        assert_eq!(json["kind"], "signals");
        assert_eq!(json["signals"][0]["signal"]["signal"], "file_offer");
        assert_eq!(json["signals"][0]["actor"], "act_ab");
        assert_eq!(json["dropped"], 3);
    }
}
