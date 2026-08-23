//! The product-free in-process node, composed inside the application process.
//!
//! Desktop supervises a `lait` sidecar. iOS cannot spawn that process, so the
//! daemon runs as a task on an owned runtime and the exit watchdog is disarmed
//! because a library must never `exit()` its host. There is deliberately no
//! World head: this platform cannot install an independent native runner, and
//! an in-process first-party adapter would recreate the product coupling the
//! independent boundary removed.
//!
//! The protocol is untouched: joins, invites, and status all ride the same
//! `daemon::Client` IPC every desktop head uses. One composition, one wire.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use lait::config::{self, Selection};
use lait::control::{self, OrbitAddress, Probe, Request, Response};
use lait::daemon::Client;
use lait::orbital;
use runtime::coordinates::SignedCoordinates;

pub(crate) struct Node {
    rt: tokio::runtime::Runtime,
    client: Client,
    /// Store paths with a live admission driver, so re-arming is idempotent
    /// instead of starting a second dialer for the same pending admission.
    driving: Mutex<HashSet<String>>,
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
    // One starter at a time: multiple surfaces may need the node together, and
    // two `start_inner` runs would race two daemons onto one home — the loser
    // reporting a bind refusal as if the node were broken.
    static STARTING: Mutex<()> = Mutex::new(());
    let _gate = STARTING.lock().expect("start gate");
    if let Some(node) = NODE.get() {
        return Ok(node);
    }
    match start_inner(Selection::default()) {
        Ok(node) => {
            let _ = NODE.set(node);
            Ok(NODE.get().expect("just set"))
        }
        Err(error) => Err(format!("{error:#}")),
    }
}

#[derive(uniffi::Enum)]
pub enum NodeStart {
    Ready,
    Failed { reason: String },
}

/// Bring the product-free node up, idempotently. Blocks up to ~30s on first
/// call; the shell calls it off the main thread and renders a starting state.
#[uniffi::export]
pub fn node_start() -> NodeStart {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(reason) => return NodeStart::Failed { reason },
    };
    arm_pending_admissions(node);
    NodeStart::Ready
}

/// The name of the persisted invite beside an entered-but-unadmitted store.
///
/// Admission only ever arrives on a dial this device makes, so the pending
/// invite must survive relaunch — "the node keeps driving" has to be true
/// after the process it was promised in is gone. Its presence is also the
/// row's measured fact: joined, still waiting on the inviter.
pub(crate) const PENDING_INVITE: &str = "pending-invite.link";

/// Re-arm the admission driver for every store still waiting on its inviter.
/// Idempotent per store: a driver already dialing is left alone.
fn arm_pending_admissions(node: &'static Node) {
    for entry in lait::orbits::list() {
        let pending = std::path::Path::new(&entry.path).join(PENDING_INVITE);
        if let Ok(link) = std::fs::read_to_string(&pending) {
            drive_admission(node, entry.path.clone(), link.trim().to_owned());
        }
    }
}

fn start_inner(selection: Selection) -> anyhow::Result<Node> {
    // The embedder's standing declaration: this daemon is a guest in a
    // process and on a device it does not own — so the exit watchdog never
    // arms (a library must not `exit()` its host), and no machine-scoped
    // listener is hosted (a phone does not coordinate displays for the LAN).
    lait::daemon::embed_in_host_process();

    // Four workers, not two: `Station::contact` blocks its thread for up to
    // 35s, and the daemon's runtime starves under two overlapping contacts.
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
            if let Err(error) = lait::daemon::run_lait_daemon(
                lait::orbital::WorldPackages::new(),
                world_interface::WorldClientRegistry::new(),
                sel,
            )
            .await
            {
                *failure.lock().expect("daemon failure slot") = Some(format!("{error:#}"));
            }
        });
    }

    // Do not hand an unusable node to the native shell. The in-process daemon
    // must answer Healthy before startup is reported complete.
    let daemon_home = selection.daemon_home()?;
    rt.block_on(async {
        for _ in 0..200 {
            if matches!(control::probe(&daemon_home).await, Probe::Healthy { .. }) {
                return Ok(());
            }
            // Taken in its own statement so the guard drops before the bail —
            // a lock alive across an early return is how deadlocks start.
            let failed = daemon_failure.lock().expect("daemon failure slot").take();
            if let Some(reason) = failed {
                anyhow::bail!("the in-process daemon failed: {reason}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("the in-process daemon did not come up")
    })?;

    Ok(Node {
        rt,
        client,
        driving: Mutex::new(HashSet::new()),
    })
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
            reason: invalid.to_string(),
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
                reason: invalid.to_string(),
            }
        }
    };
    let space = facts.space.to_string();
    let hint = if facts.display_name_hint.is_empty() {
        facts.approach_nick_hint
    } else {
        facts.display_name_hint
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
///
/// One driver per store: a second arming while the first still dials is a
/// no-op, which is what lets every foreground re-arm without counting. A
/// pass that ends without admission leaves the pending invite in place — the
/// row keeps saying "waiting for admission", and the next foreground dials
/// again. Giving up silently was the defect; the file is the memory.
fn drive_admission(node: &'static Node, home: String, link: String) {
    {
        let mut driving = node.driving.lock().expect("driving set");
        if !driving.insert(home.clone()) {
            return;
        }
    }
    node.rt.spawn(async move {
        admission_pass(node, &home, &link).await;
        node.driving.lock().expect("driving set").remove(&home);
    });
}

/// One arming's worth of dials: up to ten minutes on the line.
async fn admission_pass(node: &Node, home: &str, link: &str) {
    let path = std::path::PathBuf::from(home);
    for _ in 0..60 {
        let orbital::SpaceStore::One(space_id) = orbital::discover_space(&path) else {
            return;
        };
        let route = control::station_route(OrbitAddress::for_store(&path, space_id));
        let connect = Request::Connect {
            ticket: link.to_owned(),
        };
        let _ = node.client.request(route.clone(), &connect, None).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        if let Ok(Response::Status(info)) = node.client.request(route, &Request::Status, None).await
        {
            if info.membership == "member" || info.membership == "admin" {
                let _ = std::fs::remove_file(path.join(PENDING_INVITE));
                return;
            }
        }
    }
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
        world: None,
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

/// A refusal the person can read. The daemon's typed error carries its
/// message verbatim; anything else names itself in its debug form — an answer
/// this build does not render is still an answer, not silence.
fn refusal(response: &Response) -> String {
    match response {
        Response::Error { message, .. } => message.clone(),
        other => format!("the daemon answered something this build does not render: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The iOS host starts its engine without manufacturing a World registry
    /// or a loopback World head. Joining and syncing Spaces remain available;
    /// opening a World waits for a platform-safe independent runner contract.
    #[test]
    fn the_product_free_embedded_node_starts_without_a_world_head() {
        let home = tempfile::tempdir().expect("a scratch identity home");
        let selection = Selection::for_identity(home.path());
        let daemon_home = selection.daemon_home().expect("the daemon home");
        let node = start_inner(selection).expect("the embedded node starts");

        assert!(
            matches!(
                node.rt.block_on(control::probe(&daemon_home)),
                Probe::Healthy { .. }
            ),
            "the product-free embedded daemon did not answer"
        );
    }
}
