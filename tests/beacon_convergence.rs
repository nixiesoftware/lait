//! W4 — the Beacon initiative's acceptance harness (docket 06).
//!
//! Real process-backed SpaceBridges over their control sockets on an in-memory transport,
//! proving the plane the initiative exists for:
//!
//! 1. **Steady-state convergence without re-join** (exit criterion 1 /
//!    LOCAL-11): once admitted, a fresh write moves between idle nodes with no
//!    `Connect` and no re-join — edge-triggered beacon → pending mark →
//!    scheduler Contact.
//! 2. **Dead-hub survival** (exit criterion 2 / LOCAL-8): with the approach
//!    Station's daemon stopped, the surviving members keep converging through
//!    each other.
//! 3. **Presence agreement** (exit criterion 3 / BEACON-10): `who` and
//!    `status.online_peers` project the same reconciled truth, non-empty when
//!    peers exist.
//!
//! Flood/churn bounds (criterion 4) are covered at the unit seam:
//! `neighbors.rs` (registry cap, eviction, coalesced persistence) and
//! `independent_world.rs` (the eclipse fence's quarantine).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use issues_app::IssuesResponse as IssueResponse;
use lait::control::{request, subscribe, ControlRoute, Request, Response};
use lait::daemon::OrbitAddress;
use lait::net::Network;
use lait::orbital::run_space_bridge_with;
use lait::transport::mem::MemNet;
use lait::transport::{Transport, TransportFactory};

const FOUNDER_SEED: [u8; 32] = [221u8; 32];
const MEMBER_A_SEED: [u8; 32] = [222u8; 32];
const MEMBER_B_SEED: [u8; 32] = [223u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct MemFactory(MemNet);

#[async_trait]
impl TransportFactory for MemFactory {
    async fn build(
        &self,
        identity_seed: &[u8; 32],
        _network: &Network,
        _protocols: comms::Protocols<'_>,
    ) -> Result<Arc<dyn Transport>> {
        Ok(Arc::new(
            self.0.peer(lait::crypto::device_from_seed(identity_seed)),
        ))
    }
}

fn temp_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-beacon-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn req(rt: &tokio::runtime::Runtime, home: &Path, r: Request) -> Response {
    rt.block_on(async { request(home, &r).await })
        .unwrap_or_else(|e| Response::err(format!("{e:#}")))
}

async fn issues_request(home: &Path, request: issues_app::IssuesRequest) -> Result<IssueResponse> {
    let space = lait::orbital::discover_space_id(home).expect("test Space");
    let call = issues_app::encode_call(&request)?;
    let reply = lait::control::call_world(
        home,
        ControlRoute::World {
            address: OrbitAddress::for_store(home, space),
            world: call.world().as_str().to_string(),
        },
        call.clone(),
        None,
    )
    .await?;
    Ok(serde_json::from_value(issues_app::decode_reply(
        &call, reply,
    )?)?)
}

fn issue_req(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    request: issues_app::IssuesRequest,
) -> IssueResponse {
    rt.block_on(issues_request(home, request))
        .unwrap_or_else(|error| IssueResponse::err(format!("{error:#}")))
}

fn poll_until<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = check() {
            return Some(v);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_daemon(home: PathBuf, seed: [u8; 32], net: MemNet) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            if let Err(e) = run_space_bridge_with(home, seed, &MemFactory(net)).await {
                eprintln!("DAEMON ERR: {e:#}");
            }
        });
    })
}

fn wait_online(rt: &tokio::runtime::Runtime, home: &Path) {
    let online = poll_until(Duration::from_secs(20), || {
        matches!(req(rt, home, Request::Status), Response::Status(_)).then_some(())
    });
    assert!(
        online.is_some(),
        "daemon at {} never came online",
        home.display()
    );
}

/// Mint a single-use invite at the founder and bootstrap `home`'s store from
/// it (the `lait join` store entry; the caller spawns the daemon after).
fn admit(client: &tokio::runtime::Runtime, home: &Path, seed: &[u8; 32], founder_home: &Path) {
    let Response::Ref { reff: invite } = req(
        client,
        founder_home,
        Request::Invite {
            role: None,
            reusable: false,
            ttl_hours: Some(24),
        },
    ) else {
        panic!("expected an invite link");
    };
    lait::orbital::enter_space(home, seed, &invite).unwrap();
}

fn drive_admission(client: &tokio::runtime::Runtime, joiner_home: &Path, founder_device: &str) {
    let admitted = poll_until(Duration::from_secs(25), || {
        req(
            client,
            joiner_home,
            Request::Connect {
                ticket: founder_device.to_string(),
            },
        );
        match req(client, joiner_home, Request::Status) {
            Response::Status(info) if info.membership == "member" => Some(()),
            _ => None,
        }
    });
    assert!(admitted.is_some(), "the joiner was never admitted");
}

#[test]
fn a_fresh_write_converges_with_no_rejoin_and_presence_surfaces_agree() {
    let net = MemNet::new();
    let founder_home = temp_home("f");
    lait::orbital::form_space(&founder_home, &FOUNDER_SEED, "Beacon Space").unwrap();
    let founder_handle = spawn_daemon(founder_home.clone(), FOUNDER_SEED, net.clone());
    let client = tokio::runtime::Runtime::new().unwrap();
    wait_online(&client, &founder_home);

    let member_home = temp_home("m");
    admit(&client, &member_home, &MEMBER_A_SEED, &founder_home);
    let member_handle = spawn_daemon(member_home.clone(), MEMBER_A_SEED, net.clone());
    wait_online(&client, &member_home);
    let founder_device = lait::crypto::device_from_seed(&FOUNDER_SEED).to_string();
    drive_admission(&client, &member_home, &founder_device);

    // ---- Exit criterion 1: steady-state convergence, hands off. ----
    // The founder files a project + issue. NO Connect is issued from here on;
    // the write must reach the member through the plane alone (edge beacon →
    // pending mark → scheduler Contact).
    let resp = issue_req(
        &client,
        &founder_home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Beacon".into(),
            key: "bcn".into(),
            color: None,
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Ref { reff } if reff == "BCN"),
        "{resp:?}"
    );
    let resp = issue_req(
        &client,
        &founder_home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "Ambient news".into(),
            project: Some("bcn".into()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: Some("arrived without a re-join".into()),
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Ref { reff } if reff == "BCN-1"),
        "{resp:?}"
    );

    let started = Instant::now();
    let converged = poll_until(Duration::from_secs(10), || {
        match issue_req(
            &client,
            &member_home,
            issues_app::IssuesRequest::IssueView {
                reff: "BCN-1".into(),
            },
        ) {
            IssueResponse::Issue(v) if v.title == "Ambient news" => Some(()),
            _ => None,
        }
    });
    assert!(
        converged.is_some(),
        "the founder's write never reached the member without a re-join"
    );
    // Edge-triggered, not interval-bound: well inside one 10 s beacon floor.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "convergence took a full floor interval — the edge trigger did not fire"
    );

    // And the reverse direction, still hands off.
    issue_req(
        &client,
        &member_home,
        issues_app::IssuesRequest::Comment {
            reply_to: None,
            reff: "BCN-1".into(),
            body: "heard you ambiently".into(),
        },
    );
    let back = poll_until(Duration::from_secs(10), || {
        match issue_req(
            &client,
            &founder_home,
            issues_app::IssuesRequest::IssueView {
                reff: "BCN-1".into(),
            },
        ) {
            IssueResponse::Issue(v)
                if v.comments.iter().any(|c| c.body == "heard you ambiently") =>
            {
                Some(())
            }
            _ => None,
        }
    });
    assert!(
        back.is_some(),
        "the member's comment never converged back without a re-join"
    );

    // ---- Exit criterion 3: `who` and `status` agree, non-empty. ----
    for home in [&founder_home, &member_home] {
        // Presence is a beat, not something the convergence above settles.
        // `online` is derived from liveness on its own interval, so sampling it
        // once — the moment a comment finished converging — is a race, and the
        // two checks above already say this file knows that: both poll. This one
        // did not, so it passed on an idle machine and lost on a loaded CI
        // runner, reporting peers that existed with none of them online.
        //
        // Bounded like its neighbours. The assertion is unchanged in meaning:
        // presence must become non-empty, not merely happen to be non-empty at
        // the first instant anyone looked.
        let settled = poll_until(Duration::from_secs(10), || {
            let Response::Who { peers } = req(&client, home, Request::Who) else {
                panic!("expected Who");
            };
            let Response::Status(info) = req(&client, home, Request::Status) else {
                panic!("expected Status");
            };
            let who_online = peers.iter().filter(|p| p.online).count();
            (who_online >= 1).then_some((who_online, info.online_peers, peers))
        });
        let Some((who_online, status_online, peers)) = settled else {
            panic!("peers exist but presence is empty at {}", home.display());
        };
        // Checked on the settled sample rather than folded into the poll, so a
        // persistent disagreement fails as a disagreement instead of as a
        // timeout that says nothing about which surface was wrong.
        assert_eq!(
            who_online,
            status_online,
            "who ({peers:?}) and status disagree at {}",
            home.display()
        );
    }

    let _ = req(&client, &member_home, Request::Stop);
    let _ = req(&client, &founder_home, Request::Stop);
    let _ = member_handle.join();
    let _ = founder_handle.join();
    let _ = std::fs::remove_dir_all(&founder_home);
    let _ = std::fs::remove_dir_all(&member_home);
}

/// Converging is only half of steady state: a watching client has to be *told*
/// what moved, or it sits on a stale board next to a replica that is already
/// right.
///
/// A peer's incorporated change publishes an Observation on the same stream a
/// local commit does, so the member's doorbell must name the founder's doc under
/// its project KEY — the same frame a local edit would ring. This is the shape
/// the viewer re-reads by; a frame that merely says "something happened" leaves
/// every board on screen untouched until something else forces a rebaseline.
#[test]
fn a_peers_change_rings_a_doorbell_that_names_what_moved() {
    let net = MemNet::new();
    let founder_home = temp_home("ring-f");
    lait::orbital::form_space(&founder_home, &FOUNDER_SEED, "Beacon Space").unwrap();
    let founder_handle = spawn_daemon(founder_home.clone(), FOUNDER_SEED, net.clone());
    let client = tokio::runtime::Runtime::new().unwrap();
    wait_online(&client, &founder_home);

    let member_home = temp_home("ring-m");
    admit(&client, &member_home, &MEMBER_A_SEED, &founder_home);
    let member_handle = spawn_daemon(member_home.clone(), MEMBER_A_SEED, net.clone());
    wait_online(&client, &member_home);
    let founder_device = lait::crypto::device_from_seed(&FOUNDER_SEED).to_string();
    drive_admission(&client, &member_home, &founder_device);

    issue_req(
        &client,
        &founder_home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Beacon".into(),
            key: "bcn".into(),
            color: None,
        },
    );
    issue_req(
        &client,
        &founder_home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "before".into(),
            project: Some("bcn".into()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    );
    // Wait for the issue itself to land, so the frame under test is the *edit*
    // rather than the arrival of a doc the member had never seen.
    let doc = poll_until(Duration::from_secs(15), || {
        match issue_req(
            &client,
            &member_home,
            issues_app::IssuesRequest::List {
                project: None,
                filter: Default::default(),
            },
        ) {
            IssueResponse::List { rows } => rows.first().map(|r| r.doc_id.as_str().to_string()),
            _ => None,
        }
    })
    .expect("the founder's issue never reached the member");

    client.block_on(async {
        let mut sub = subscribe(&member_home, 0)
            .await
            .expect("open the member's subscribe stream");
        let first = sub
            .next()
            .await
            .expect("read first frame")
            .expect("first frame present");
        assert!(first.reset, "first Subscribe frame must be a Reset");

        // The founder edits. Nothing is issued at the member from here on: the
        // change arrives through the plane, and the doorbell is the only news.
        issues_request(
            &founder_home,
            issues_app::IssuesRequest::IssueEdit {
                due: None,
                estimate: None,
                reff: "BCN-1".into(),
                title: Some("after".into()),
                status: None,
                priority: None,
                description: None,
            },
        )
        .await
        .expect("issue edit at the founder");

        // Incorporation may ring more than once (authority material converges
        // alongside the doc), so read until a frame names the doc rather than
        // demanding it be the first one.
        let deadline = Instant::now() + Duration::from_secs(20);
        let named = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break None;
            }
            match tokio::time::timeout(remaining, sub.next()).await {
                Ok(Ok(Some(frame))) => {
                    let named = frame
                        .dirty_by_project
                        .iter()
                        .any(|d| d.project_key == "BCN" && d.docs.contains(&doc));
                    if named {
                        break Some(frame);
                    }
                }
                _ => break None,
            }
        };
        assert!(
            named.is_some(),
            "the member converged but no doorbell named {doc} under BCN — a watching \
             client has no way to know the board moved"
        );

        let _ = request(&member_home, &Request::Stop).await;
        let _ = request(&founder_home, &Request::Stop).await;
    });

    let _ = member_handle.join();
    let _ = founder_handle.join();
    let _ = std::fs::remove_dir_all(&founder_home);
    let _ = std::fs::remove_dir_all(&member_home);
}

/// Membership news must ring, not merely arrive.
///
/// `acl` is not a catalog path: membership lives in mechanics authority
/// material, which converges through `staged.authority_records` rather than
/// through Bodies. An authority-only exchange returns `units: Vec::new()`, so
/// `incorporate_units` reports `ConvergenceOutcome::unchanged`, `advanced()` is
/// false, and `contact_driver` never publishes an Observation — which would mean
/// a peer's admission reaches every other node as *data* and never as *news*.
///
/// A already knows the plane works (`surviving_members_converge_…` proves the
/// list converges). This pins the half that proof does not cover: that a client
/// watching A is told.
#[test]
fn a_peers_admission_rings_a_doorbell_at_the_other_members() {
    let net = MemNet::new();
    let founder_home = temp_home("acl-f");
    lait::orbital::form_space(&founder_home, &FOUNDER_SEED, "ACL Space").unwrap();
    let founder_handle = spawn_daemon(founder_home.clone(), FOUNDER_SEED, net.clone());
    let client = tokio::runtime::Runtime::new().unwrap();
    wait_online(&client, &founder_home);
    let founder_device = lait::crypto::device_from_seed(&FOUNDER_SEED).to_string();

    let a_home = temp_home("acl-a");
    admit(&client, &a_home, &MEMBER_A_SEED, &founder_home);
    let a_handle = spawn_daemon(a_home.clone(), MEMBER_A_SEED, net.clone());
    wait_online(&client, &a_home);
    drive_admission(&client, &a_home, &founder_device);

    // A is watched from here on, from its own thread: everything after this
    // point is news A must be *told*, not news A could discover by asking.
    let frames: Arc<std::sync::Mutex<Vec<lait::control::Doorbell>>> = Default::default();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let watcher = {
        let (frames, stop, home) = (frames.clone(), stop.clone(), a_home.clone());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let mut sub = subscribe(&home, 0)
                    .await
                    .expect("open A's subscribe stream");
                let first = sub.next().await.expect("read").expect("present");
                assert!(first.reset, "first Subscribe frame must be a Reset");
                ready_tx.send(()).expect("hand back the baton");
                while !stop.load(Ordering::SeqCst) {
                    match tokio::time::timeout(Duration::from_millis(250), sub.next()).await {
                        Ok(Ok(Some(frame))) => frames.lock().expect("frames").push(frame),
                        Ok(_) => break,     // stream closed
                        Err(_) => continue, // idle window
                    }
                }
            });
        })
    };
    ready_rx
        .recv()
        .expect("watcher subscribed and drained its Reset");

    let b_home = temp_home("acl-b");
    admit(&client, &b_home, &MEMBER_B_SEED, &founder_home);
    let b_handle = spawn_daemon(b_home.clone(), MEMBER_B_SEED, net.clone());
    wait_online(&client, &b_home);
    drive_admission(&client, &b_home, &founder_device);

    // Did the membership itself converge to A?
    let converged = poll_until(Duration::from_secs(15), || {
        match req(&client, &a_home, Request::Members) {
            Response::Members { members } if members.len() >= 3 => Some(members.len()),
            _ => None,
        }
    });
    // A grace window so a late ring counts as a ring.
    std::thread::sleep(Duration::from_secs(2));
    stop.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    let rang = frames.lock().expect("frames").clone();

    let _ = req(&client, &b_home, Request::Stop);
    let _ = b_handle.join();
    let _ = std::fs::remove_dir_all(&b_home);

    let _ = req(&client, &a_home, Request::Stop);
    let _ = req(&client, &founder_home, Request::Stop);
    let _ = a_handle.join();
    let _ = founder_handle.join();
    let _ = std::fs::remove_dir_all(&founder_home);
    let _ = std::fs::remove_dir_all(&a_home);

    assert!(
        converged.is_some(),
        "A never learned B's admission at all — a different bug than the one under test"
    );
    assert!(
        !rang.is_empty(),
        "A's membership converged ({converged:?} members) but nothing rang: a client \
         watching A has no way to learn a peer joined. Authority material converges \
         outside the Body plane, so it publishes no Observation."
    );
    // And it must ring as *membership*, not as some incidental Body frame that
    // happened to arrive in the same window — otherwise this passes for the
    // wrong reason the moment anything else converges alongside.
    assert!(
        rang.iter().any(|f| f.authority_advanced),
        "something rang at A but no frame carried the authority plane, so a \
         client would re-read everything except the membership that actually \
         changed: {rang:?}"
    );
}

#[test]
fn surviving_members_converge_after_the_approach_station_dies() {
    let net = MemNet::new();
    let founder_home = temp_home("hub");
    lait::orbital::form_space(&founder_home, &FOUNDER_SEED, "Dead Hub Space").unwrap();
    let founder_handle = spawn_daemon(founder_home.clone(), FOUNDER_SEED, net.clone());
    let client = tokio::runtime::Runtime::new().unwrap();
    wait_online(&client, &founder_home);
    let founder_device = lait::crypto::device_from_seed(&FOUNDER_SEED).to_string();

    // Two members, admitted one after the other through the founder.
    let a_home = temp_home("a");
    admit(&client, &a_home, &MEMBER_A_SEED, &founder_home);
    let a_handle = spawn_daemon(a_home.clone(), MEMBER_A_SEED, net.clone());
    wait_online(&client, &a_home);
    drive_admission(&client, &a_home, &founder_device);

    let b_home = temp_home("b");
    admit(&client, &b_home, &MEMBER_B_SEED, &founder_home);
    let b_handle = spawn_daemon(b_home.clone(), MEMBER_B_SEED, net.clone());
    wait_online(&client, &b_home);
    drive_admission(&client, &b_home, &founder_device);

    // A must learn B's membership (authority news travels the same plane)
    // before the hub dies, so B's beacons pass A's eclipse fence.
    let a_knows_b = poll_until(Duration::from_secs(15), || {
        match req(&client, &a_home, Request::Members) {
            Response::Members { members } if members.len() >= 3 => Some(()),
            _ => None,
        }
    });
    assert!(
        a_knows_b.is_some(),
        "A never learned B's admission over the plane"
    );

    // ---- Exit criterion 2: kill the hub; survivors keep converging. ----
    let _ = req(&client, &founder_home, Request::Stop);
    let _ = founder_handle.join();

    let resp = issue_req(
        &client,
        &b_home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Orphaned".into(),
            key: "orp".into(),
            color: None,
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Ref { reff } if reff == "ORP"),
        "{resp:?}"
    );
    let resp = issue_req(
        &client,
        &b_home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "The hub is gone".into(),
            project: Some("orp".into()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Ref { reff } if reff == "ORP-1"),
        "{resp:?}"
    );

    let survived = poll_until(Duration::from_secs(15), || {
        match issue_req(
            &client,
            &a_home,
            issues_app::IssuesRequest::IssueView {
                reff: "ORP-1".into(),
            },
        ) {
            IssueResponse::Issue(v) if v.title == "The hub is gone" => Some(()),
            _ => None,
        }
    });
    assert!(
        survived.is_some(),
        "B's write never reached A once the approach station died — the space partitioned"
    );

    let _ = req(&client, &a_home, Request::Stop);
    let _ = req(&client, &b_home, Request::Stop);
    let _ = a_handle.join();
    let _ = b_handle.join();
    let _ = std::fs::remove_dir_all(&founder_home);
    let _ = std::fs::remove_dir_all(&a_home);
    let _ = std::fs::remove_dir_all(&b_home);
}
