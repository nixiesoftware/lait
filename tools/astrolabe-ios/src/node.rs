//! The in-process node: the daemon and the loopback head, composed inside the
//! application process.
//!
//! Desktop composes these as processes — the client supervises a `lait`
//! sidecar, the sidecar spawns the daemon, the head scrapes a readiness line
//! off stdout. iOS forbids every one of those moves, so this module is the
//! same composition with the process seams removed: the daemon runs as a task
//! on an owned runtime, the head announces itself through
//! [`serve::run_until`]'s callback instead of stdout, and the exit watchdog
//! is disarmed because a library must never `exit()` its host.
//!
//! One seam desktop never needed: **the head pauses and resumes.** iOS
//! suspends the process shortly after backgrounding and may reclaim listener
//! resources while it sleeps — a loopback listener carried into suspension
//! comes back dead, and dead reads as the false-disconnection defect. So the
//! head steps down before suspension and stands back up on foreground, each a
//! deliberate transition the shell drives from `scenePhase`. The daemon and
//! its runtime persist for the process lifetime; only the listener cycles.
//!
//! The protocol is untouched: joins, invites, and status all ride the same
//! `daemon::Client` IPC every desktop head uses. One composition, one wire.

use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use lait::config::{self, Selection};
use lait::control::{self, OrbitAddress, Probe, Request, Response};
use lait::daemon::Client;
use lait::{orbital, serve};
use runtime::coordinates::SignedCoordinates;

pub(crate) struct Node {
    rt: tokio::runtime::Runtime,
    client: Client,
    /// What the head restarts from: resolved once, reused on every resume.
    selection: Selection,
    /// The restartable half. `Down` between background and foreground is a
    /// deliberate state, not a failure.
    head: Mutex<HeadState>,
    /// Store paths with a live admission driver, so re-arming on every
    /// foreground is idempotent instead of a second dialer per wake.
    driving: Mutex<HashSet<String>>,
}

enum HeadState {
    Up(HeadUp),
    Down,
}

/// A serving head: its announcement, and the two ends of stopping it — the
/// trigger, and the handle whose completion IS "the drain finished".
struct HeadUp {
    ready: serve::Ready,
    stop: tokio::sync::oneshot::Sender<()>,
    drained: tokio::task::JoinHandle<()>,
}

impl Node {
    /// The head's announcement, when it is up. `None` while paused or
    /// starting — the surface renders that as its own state, never as an error.
    pub(crate) fn head_ready(&self) -> Option<serve::Ready> {
        match &*self.head.lock().expect("head state") {
            HeadState::Up(up) => Some(up.ready.clone()),
            HeadState::Down => None,
        }
    }
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
    // One starter at a time: launch fires `node_start` and the first
    // foreground transition near-simultaneously, and two `start_inner` runs
    // would race two daemons onto one home — the loser reporting a bind
    // refusal as if the node were broken.
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
    node_foreground()
}

/// The foreground transition: the node up, the head serving, every pending
/// admission being driven. Idempotent — at launch it IS the start, and after
/// a suspension it restarts only what suspension killed (the listener), so
/// the shell calls it on every `scenePhase == .active` without counting.
///
/// A restarted head is a *new* announcement — fresh port, fresh token — and
/// the returned fact is the one every open tab must re-authenticate against.
#[uniffi::export]
pub fn node_foreground() -> NodeStart {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(reason) => return NodeStart::Failed { reason },
    };
    arm_pending_admissions(node);
    match resume_head(node) {
        Ok(ready) => NodeStart::Ready {
            head: head_ready(&ready),
        },
        Err(reason) => NodeStart::Failed { reason },
    }
}

/// The background transition: the head steps down before suspension freezes
/// it. Close-then-suspend is the platform's own guidance — a listener carried
/// into suspension is reclaimed under the app and comes back dead — and the
/// shell holds a background-task assertion across this call so the drain
/// finishes before the freeze. The daemon stays; suspension merely pauses it.
///
/// A no-op when the node never started or the head is already down.
#[uniffi::export]
pub fn node_background() {
    if let Some(node) = node() {
        pause_head(node);
    }
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

fn head_ready(ready: &serve::Ready) -> HeadReady {
    HeadReady {
        url: ready.url.clone(),
        token: ready.token.clone(),
        port: ready.port,
    }
}

fn start_inner(selection: Selection) -> anyhow::Result<Node> {
    // The embedder's standing declaration: this daemon is a guest in a
    // process and on a device it does not own — so the exit watchdog never
    // arms (a library must not `exit()` its host), and no machine-scoped
    // listener is hosted (a phone does not coordinate displays for the LAN).
    lait::daemon::embed_in_host_process();

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
        let installation = crate::worlds::installation()?;
        rt.spawn(async move {
            if let Err(error) =
                lait::daemon::run_lait_daemon(installation.packages, installation.clients, sel)
                    .await
            {
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

    let up = start_head(&rt, selection.clone())?;
    Ok(Node {
        rt,
        client,
        selection,
        head: Mutex::new(HeadState::Up(up)),
        driving: Mutex::new(HashSet::new()),
    })
}

/// Start one head and wait for its announcement. Port 0: the OS picks; the
/// announcement carries the real one. Never `open` — there is no browser to
/// hand off to, WebKit is in-process.
fn start_head(rt: &tokio::runtime::Runtime, selection: Selection) -> anyhow::Result<HeadUp> {
    let (tx, rx) = mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let registry = crate::worlds::client_packages()?;
    let drained = rt.spawn(async move {
        let announce = move |ready: &serve::Ready| {
            let _ = tx.send(ready.clone());
        };
        // The shutdown future resolves on the trigger — or on its drop, so a
        // head whose handle is lost drains rather than serving unsupervised.
        let shutdown = async move {
            let _ = stopped.await;
        };
        // Named rather than left to the sole-World fallback. This crate links
        // the whole workspace, so "the only World" stopped being true the
        // moment a second one shipped — and the fallback answers that with a
        // refusal, which for an embedded node is a head that never comes up.
        //
        // One World at a time is the deliberate mobile shape, and pinning it to
        // this one is safe *only while nothing on the phone can ask for
        // another*: the shell already lists every platform-supplied World, and opening
        // them is CLIENT-58, still unbuilt. When that lands, this constant
        // becomes a shell that offers a World its head refuses.
        //
        // The seam is here rather than a rebuild: `resume_head` already mints a
        // fresh port and token on every foreground, and the shell is built to
        // re-authenticate against a new announcement. Serving a different World
        // is that same transition with a different pin — still one head, still
        // one at a time.
        let world = Some(crate::worlds::primary_mount().to_owned());
        if let Err(error) = serve::run_embedded_until(
            0,
            false,
            selection,
            world,
            registry,
            serve::head::Source::unavailable(),
            announce,
            shutdown,
        )
        .await
        {
            tracing::error!(%error, "in-process head exited");
        }
    });
    let ready = rx.recv_timeout(Duration::from_secs(20))?;
    Ok(HeadUp {
        ready,
        stop,
        drained,
    })
}

/// How long the background transition waits on the head's drain. The drain is
/// single-digit milliseconds in the ordinary case — the stop reaches the
/// never-ending responses before axum starts waiting on them — and the bound
/// exists so a wedged drain degrades to "suspension freezes it mid-close"
/// instead of holding the transition forever.
const DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Step the head down and see the drain finish. Idempotent.
fn pause_head(node: &Node) {
    // Taken in its own statement so the guard drops before the drain —
    // holding the state lock across `block_on` would deadlock every reader.
    let taken = std::mem::replace(&mut *node.head.lock().expect("head state"), HeadState::Down);
    if let HeadState::Up(up) = taken {
        let _ = up.stop.send(());
        let _ = node
            .rt
            .block_on(async { tokio::time::timeout(DRAIN_DEADLINE, up.drained).await });
    }
}

/// The head, serving: the one already up, or a fresh start. A fresh start is
/// a new announcement — new port, new token — never a resurrection of the old
/// one, whose resources suspension may have reclaimed.
fn resume_head(node: &Node) -> Result<serve::Ready, String> {
    if let Some(ready) = node.head_ready() {
        return Ok(ready);
    }
    let up = match start_head(&node.rt, node.selection.clone()) {
        Ok(up) => up,
        Err(error) => return Err(format!("{error:#}")),
    };
    let mut head = node.head.lock().expect("head state");
    match &*head {
        // Two foregrounds raced and the other one won: keep its head, stop
        // ours by dropping the trigger — the drain follows on its own.
        HeadState::Up(existing) => Ok(existing.ready.clone()),
        HeadState::Down => {
            let ready = up.ready.clone();
            *head = HeadState::Up(up);
            Ok(ready)
        }
    }
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
    use std::io::{Read, Write};
    use std::net::TcpStream;

    /// One HTTP status line off the announced head, over a raw socket — the
    /// proof that the listener accepts and the token is honored, with no
    /// client stack between the assertion and the wire.
    fn status_line(ready: &serve::Ready) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", ready.port))
            .expect("the announced port accepts a connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a bounded read");
        write!(
            stream,
            "GET /?token={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            ready.token, ready.port
        )
        .expect("the request goes out");
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let text = String::from_utf8_lossy(&raw);
        text.lines().next().unwrap_or_default().to_owned()
    }

    /// The chain, asserted end to end against the embedded composition — the
    /// class of failure `tools/astrolabe/tests/launch.rs` exists for, where
    /// every component is correct and the composition is wrong. Daemon up,
    /// head announced before accepting, pause actually closes the port,
    /// resume is a fresh working announcement.
    #[test]
    fn the_embedded_node_comes_up_steps_down_and_returns() {
        let home = tempfile::tempdir().expect("a scratch identity home");
        let node =
            start_inner(Selection::for_identity(home.path())).expect("the embedded node starts");

        let first = node.head_ready().expect("the head announced");
        let line = status_line(&first);
        assert!(
            line.starts_with("HTTP/1.1 "),
            "the announced head must answer HTTP, got: {line:?}"
        );

        // The background transition: down means the port is closed, not that
        // a stale announcement lingers.
        pause_head(&node);
        assert!(
            node.head_ready().is_none(),
            "paused must read as down, never as the old announcement"
        );
        assert!(
            TcpStream::connect(("127.0.0.1", first.port)).is_err(),
            "the paused head must not accept on its old port"
        );

        // The foreground transition: a fresh announcement that works. The old
        // token died with the old head; the new one must be honored.
        let second = resume_head(&node).expect("the head returns");
        assert_ne!(
            second.token, first.token,
            "a resumed head is a new announcement, not a resurrection"
        );
        let line = status_line(&second);
        assert!(
            line.starts_with("HTTP/1.1 "),
            "the resumed head must answer HTTP, got: {line:?}"
        );

        // Resume while up is the fast path: the same announcement, no churn.
        let third = resume_head(&node).expect("resume while up answers");
        assert_eq!(
            third.port, second.port,
            "resume while serving must keep the head it has"
        );
    }
}
