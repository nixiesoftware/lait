//! The in-process node: the daemon and the loopback head, composed inside the
//! application process.
//!
//! Desktop composes these as processes — the client supervises a `lait`
//! sidecar, the sidecar spawns the daemon, the head scrapes a readiness line
//! off stdout. iOS forbids every one of those moves, so this module is the
//! same composition with the process seams removed: the daemon runs as a task
//! on an owned runtime, the head announces itself through
//! [`serve::run_announced`]'s callback instead of stdout, and the exit
//! watchdog is disarmed because a library must never `exit()` its host.
//!
//! The protocol is untouched: joins, invites, and status all ride the same
//! `daemon::Client` IPC every desktop head uses. One composition, one wire.

use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use lait::config::{self, Selection};
use lait::control::{self, OrbitAddress, Probe, Request, Response};
use lait::daemon::Client;
use lait::{orbital, serve, world};
use runtime::coordinates::SignedCoordinates;

pub(crate) struct Node {
    rt: tokio::runtime::Runtime,
    client: Client,
    pub(crate) ready: serve::Ready,
}

static NODE: OnceLock<Node> = OnceLock::new();

pub(crate) fn node() -> Option<&'static Node> {
    NODE.get()
}

/// The node, started if it is not yet running. Every act that needs the node
/// goes through here, so "the node is not running" is never an answer — only
/// the real startup failure can refuse.
fn ensure_node() -> Result<&'static Node, String> {
    if let Some(node) = NODE.get() {
        return Ok(node);
    }
    match start_inner() {
        Ok(node) => {
            let _ = NODE.set(node);
            Ok(NODE.get().expect("just set"))
        }
        Err(error) => Err(format!("{error:#}")),
    }
}

/// What the head answered when it came up: everything a WebKit tab needs.
#[derive(uniffi::Record, Clone)]
pub struct HeadReady {
    pub url: String,
    pub token: String,
    pub port: u16,
}

#[derive(uniffi::Enum)]
pub enum NodeStart {
    Ready { head: HeadReady },
    Failed { reason: String },
}

/// Bring the node up, idempotently. Blocks up to ~30s on first call; the
/// shell calls it off the main thread and renders a starting state.
#[uniffi::export]
pub fn node_start() -> NodeStart {
    match ensure_node() {
        Ok(node) => {
            arm_pending_admissions(node);
            NodeStart::Ready {
                head: head_ready(&node.ready),
            }
        }
        Err(reason) => NodeStart::Failed { reason },
    }
}

/// The name of the persisted invite beside an entered-but-unadmitted store.
///
/// Admission only ever arrives on a dial this device makes, so the pending
/// invite must survive relaunch — "the node keeps driving" has to be true
/// after the process it was promised in is gone.
const PENDING_INVITE: &str = "pending-invite.link";

/// Re-arm the admission driver for every store still waiting on its inviter.
fn arm_pending_admissions(node: &'static Node) {
    for entry in lait::orbits::list() {
        let pending = std::path::Path::new(&entry.path).join(PENDING_INVITE);
        if let Ok(link) = std::fs::read_to_string(&pending) {
            drive_admission(node, entry.path.clone(), link.trim().to_owned());
        }
    }
}

fn head_ready(ready: &serve::Ready) -> HeadReady {
    HeadReady {
        url: ready.url.clone(),
        token: ready.token.clone(),
        port: ready.port,
    }
}

fn start_inner() -> anyhow::Result<Node> {
    // The daemon's shutdown watchdog hard-exits the process 30s into any
    // drain. In a process we do not own, that is a crash; zero disarms it —
    // the documented escape, not a hack.
    std::env::set_var("LAIT_SHUTDOWN_DEADLINE_SECS", "0");

    // The ambient selection: identity and stores resolve under this app's own
    // container ($HOME *is* the sandbox on iOS), the same way the desktop
    // daemon resolves under the user profile.
    let selection = Selection::default();

    // Four workers, not two: `Station::contact` blocks its thread for up to
    // 35s, and a runtime shared by the daemon, the Station, and the head
    // starves under two overlapping contacts.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;

    let client = Client::for_selection(&selection)?;

    // The daemon's failure, if any, captured where the timeout below can read
    // it — a refusal must carry its reason, and tracing goes nowhere here.
    let daemon_failure = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let sel = selection.clone();
        let failure = daemon_failure.clone();
        rt.spawn(async move {
            if let Err(error) = lait::daemon::run_lait_daemon(world::packages(), sel).await {
                *failure.lock().expect("daemon failure slot") = Some(format!("{error:#}"));
            }
        });
    }

    // The head probes for a daemon before serving, and on a miss it would
    // spawn one as a child process — the one thing this platform forbids. So
    // the daemon must already answer Healthy before the head starts.
    let daemon_home = selection.daemon_home()?;
    rt.block_on(async {
        for _ in 0..200 {
            if matches!(control::probe(&daemon_home).await, Probe::Healthy) {
                return Ok(());
            }
            if let Some(reason) = daemon_failure.lock().expect("daemon failure slot").take() {
                anyhow::bail!("the in-process daemon failed: {reason}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("the in-process daemon did not come up")
    })?;

    let (tx, rx) = mpsc::channel();
    {
        let sel = selection.clone();
        rt.spawn(async move {
            let announce = move |ready: &serve::Ready| {
                let _ = tx.send(ready.clone());
            };
            // Port 0: the OS picks; the announcement carries the real one.
            // Never `open` — there is no browser to hand off to, WebKit is
            // in-process. This future only returns on failure; on iOS no
            // termination signal ever fires.
            if let Err(error) = serve::run_announced(0, false, sel, announce).await {
                tracing::error!(%error, "in-process head exited");
            }
        });
    }
    let ready = rx.recv_timeout(Duration::from_secs(20))?;

    Ok(Node { rt, client, ready })
}

/// What an invite says, before anything is created. The confirm screen renders
/// this; nothing is created implicitly.
#[derive(uniffi::Record)]
pub struct TicketFacts {
    /// The Space id (`ws_…`) — canonical, monospace, one tap behind the label.
    pub space_id: String,
    /// The Space display name the inviter's ticket carries. May be empty.
    pub name_hint: String,
    /// The inviter's nick. May be empty. A hint, never an authority.
    pub host_nick_hint: String,
}

#[derive(uniffi::Enum)]
pub enum TicketRead {
    Valid { facts: TicketFacts },
    Invalid { reason: String },
}

/// Parse and verify an invite link without touching disk. Self-authenticating
/// by design — a stranger can check it with no prior state.
#[uniffi::export]
pub fn read_ticket(link: String) -> TicketRead {
    match SignedCoordinates::parse_link(&link).and_then(|c| c.verify()) {
        Ok(verified) => TicketRead::Valid {
            facts: TicketFacts {
                space_id: verified.space.to_string(),
                name_hint: verified.display_name_hint,
                host_nick_hint: verified.approach_nick_hint,
            },
        },
        Err(invalid) => TicketRead::Invalid {
            reason: format!("{invalid:?}"),
        },
    }
}

/// The joiner's reply, verbatim from the host plane. `admitted: false` is not
/// a failure — the inviter may be offline — and the surface must keep the
/// difference between "you're in" and "the board stays encrypted until they
/// come online".
#[derive(uniffi::Record)]
pub struct Entered {
    pub space_id: String,
    pub home: String,
    pub device: String,
    pub host_nick: String,
    pub fresh: bool,
    pub admitted: bool,
    pub contacted: bool,
    pub last_error: Option<String>,
}

#[derive(uniffi::Enum)]
pub enum EnterOutcome {
    Entered { entered: Entered },
    Refused { reason: String },
}

/// Enter a Space from an invite — the same `HostSpaceEnter` request the
/// desktop Welcome flow sends, against the in-process daemon. Blocks for up to
/// the engine's admission deadline (~30s); call off the main thread.
#[uniffi::export]
pub fn enter_space(link: String, nick: Option<String>) -> EnterOutcome {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(reason) => return EnterOutcome::Refused { reason },
    };
    // The store lands under spaces_root/<slug>, the same shape the desktop
    // Welcome flow suggests: readable, and disambiguated by the space id so
    // two invites with one nick never collide.
    let facts = match SignedCoordinates::parse_link(&link).and_then(|c| c.verify()) {
        Ok(verified) => verified,
        Err(invalid) => {
            return EnterOutcome::Refused {
                reason: format!("{invalid:?}"),
            }
        }
    };
    let space = facts.space.to_string();
    let hint = if facts.display_name_hint.is_empty() {
        facts.approach_nick_hint.clone()
    } else {
        facts.display_name_hint.clone()
    };
    let home = config::spaces_root().join(slug(&hint, &space));

    let original_link = link.clone();
    let request = Request::HostSpaceEnter {
        link,
        home: home.to_string_lossy().into_owned(),
        nick,
    };
    let response = node.rt.block_on(async {
        node.client
            .request(control::ControlRoute::Daemon, &request, None)
            .await
    });
    match response {
        Ok(Response::Host(control::HostReply::Entered {
            space,
            home,
            device,
            approach,
            host_nick,
            fresh,
            admitted,
            contacted,
            last_error,
        })) => {
            if !admitted {
                // The engine's invite is auto-approving; what admission needs
                // is the joiner staying on the line — membership and keys only
                // ever arrive on a dial THIS device makes. Persist the invite
                // so relaunch re-arms the driver, and drive with the full link
                // so every dial re-learns the ticket's routes.
                let _ = std::fs::write(
                    std::path::Path::new(&home).join(PENDING_INVITE),
                    &original_link,
                );
                drive_admission(node, home.clone(), original_link.clone());
            }
            let _ = approach;
            EnterOutcome::Entered {
                entered: Entered {
                    space_id: space,
                    home,
                    device,
                    host_nick,
                    fresh,
                    admitted,
                    contacted,
                    last_error,
                },
            }
        }
        Ok(other) => EnterOutcome::Refused {
            reason: refusal(&other),
        },
        Err(error) => EnterOutcome::Refused {
            reason: format!("{error:#}"),
        },
    }
}

/// Keep re-driving Contact toward the inviter until membership lands.
///
/// The full invite link, deliberately: `connect` re-learns the ticket's
/// routes on every pass, so a transport that lost its way after suspension
/// gets fresh addresses each dial instead of leaning on discovery alone. The
/// cadence respects the engine's 30s contact deadline — a faster loop just
/// collects `Capacity` refusals from the attempt already in flight.
fn drive_admission(node: &'static Node, home: String, link: String) {
    node.rt.spawn(async move {
        let path = std::path::PathBuf::from(&home);
        for _ in 0..60 {
            let orbital::SpaceStore::One(space_id) = orbital::discover_space(&path) else {
                return;
            };
            let route = control::station_route(OrbitAddress::for_store(&path, space_id));
            let connect = Request::Connect {
                ticket: link.clone(),
            };
            let _ = node.client.request(route.clone(), &connect, None).await;
            tokio::time::sleep(Duration::from_secs(10)).await;
            if let Ok(Response::Status(info)) =
                node.client.request(route, &Request::Status, None).await
            {
                if info.membership == "member" || info.membership == "admin" {
                    let _ = std::fs::remove_file(path.join(PENDING_INVITE));
                    return;
                }
            }
        }
    });
}

#[derive(uniffi::Enum)]
pub enum SyncOutcome {
    Report { message: String },
    Refused { reason: String },
}

/// Converge one Space now and answer verbatim — the product's own diagnostic.
/// "No known peers", "reached none of N", and "reached N, refused material"
/// are three different problems, and the report names which one this is.
#[uniffi::export]
pub fn sync_space(space_path: String) -> SyncOutcome {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(reason) => return SyncOutcome::Refused { reason },
    };
    let path = std::path::PathBuf::from(&space_path);
    let orbital::SpaceStore::One(space_id) = orbital::discover_space(&path) else {
        return SyncOutcome::Refused {
            reason: format!("no single space store at {space_path}"),
        };
    };
    let route = control::station_route(OrbitAddress::for_store(&path, space_id));
    let response = node
        .rt
        .block_on(async { node.client.request(route, &Request::Sync, None).await });
    match response {
        Ok(Response::Sync { message, .. }) => SyncOutcome::Report { message },
        Ok(other) => SyncOutcome::Refused {
            reason: refusal(&other),
        },
        Err(error) => SyncOutcome::Refused {
            reason: format!("{error:#}"),
        },
    }
}

/// The live membership fact for one joined store, asked passively — answers
/// only if the Orbit is already placed, and never places one to ask.
pub(crate) fn membership_of(path: &std::path::Path) -> Option<String> {
    let node = NODE.get()?;
    let orbital::SpaceStore::One(space_id) = orbital::discover_space(path) else {
        return None;
    };
    let route = control::station_route(OrbitAddress::for_store(path, space_id));
    let response = node.rt.block_on(async {
        node.client
            .request_if_running(route, &Request::Status)
            .await
    });
    match response {
        Ok(Response::Status(info)) => Some(info.membership.clone()),
        _ => None,
    }
}

#[derive(uniffi::Enum)]
pub enum InviteOutcome {
    Minted { link: String },
    Refused { reason: String },
}

/// Mint a single-use invite for a joined Space and answer with the full
/// `lait://join/…` link. Places the Orbit — minting needs a live Station, and
/// that is the user's explicit act, never a listing side effect.
#[uniffi::export]
pub fn mint_invite(space_path: String) -> InviteOutcome {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(reason) => return InviteOutcome::Refused { reason },
    };
    let path = std::path::PathBuf::from(&space_path);
    let space_id = match orbital::discover_space(&path) {
        orbital::SpaceStore::One(space_id) => space_id,
        other => {
            return InviteOutcome::Refused {
                reason: format!("no single space store at {space_path}: {other:?}"),
            }
        }
    };
    let route = control::station_route(OrbitAddress::for_store(&path, space_id));
    let request = Request::Invite {
        role: None,
        reusable: false,
        ttl_hours: None,
    };
    let response = node
        .rt
        .block_on(async { node.client.request(route, &request, None).await });
    match response {
        Ok(Response::Ref { reff }) => InviteOutcome::Minted {
            link: format!("lait://join/{reff}"),
        },
        Ok(other) => InviteOutcome::Refused {
            reason: refusal(&other),
        },
        Err(error) => InviteOutcome::Refused {
            reason: format!("{error:#}"),
        },
    }
}

/// A store-directory slug: the human hint, sanitized, plus enough of the space
/// id to never collide.
fn slug(hint: &str, space: &str) -> String {
    let mut base: String = hint
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    base.truncate(24);
    let base = base.trim_matches('-');
    let tail: String = space
        .chars()
        .rev()
        .take(6)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if base.is_empty() {
        format!("space-{tail}")
    } else {
        format!("{base}-{tail}")
    }
}

/// A refusal the person can read. The daemon's error variants carry their
/// message; anything else degrades to its debug form rather than silence.
fn refusal(response: &Response) -> String {
    format!("{response:?}")
}
