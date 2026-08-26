//! Restart durability over a process-backed **StationHost** (in-process, in-memory
//! transport): a joiner that is admitted and converged, then has its host
//! killed and restarted on the SAME home, must come back holding its persisted
//! membership and reconverge with a peer that files new content while it was
//! down.
//!
//! `orbital_two_node.rs` proves the cold form → invite → enter → admit →
//! converge arc. This adds the restart in the middle: after admission, the
//! joiner host is dropped, the founder files a new issue, and the joiner
//! host is respawned on its persisted store. It must re-dock from persisted
//! membership and, once Contact is re-driven, converge to the post-restart
//! issue — proving the orbital store survives a crash and rejoins.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::world_fixture::run_station_process_with;
use anyhow::Result;
use async_trait::async_trait;
use comms::mem::MemNet;
use comms::policy::Network;
use comms::{Transport, TransportFactory};
use issues_app::IssuesResponse as IssueResponse;
use lait::control::OrbitAddress;
use lait::control::{request, ControlRoute, Request, Response};

const FOUNDER_SEED: [u8; 32] = [151u8; 32];
const JOINER_SEED: [u8; 32] = [152u8; 32];

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
            self.0
                .peer(mechanics::actor::device_from_seed(identity_seed)),
        ))
    }
}

/// A throwaway root that removes itself — see [`crate::head::temp_root`],
/// which is the one place that knows how.
fn temp_home(tag: &str) -> crate::head::TempRoot {
    crate::head::temp_root(&format!("restart-{tag}"))
}

fn req(rt: &tokio::runtime::Runtime, home: &Path, r: Request) -> Response {
    rt.block_on(async { request(home, &r).await })
        .unwrap_or_else(|e| Response::err(format!("{e:#}")))
}

fn issue_req(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    request: issues_app::IssuesRequest,
) -> IssueResponse {
    super::accepted_issue_response(
        rt.block_on(async {
            let space = lait::orbital::discover_space(home)
                .single()
                .expect("test Space");
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
            Ok::<IssueResponse, anyhow::Error>(serde_json::from_value(issues_app::decode_reply(
                &call, reply,
            )?)?)
        })
        .unwrap_or_else(|error| IssueResponse::err(format!("{error:#}"))),
    )
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
            if let Err(e) = run_station_process_with(home, seed, &MemFactory(net)).await {
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
        "StationHost at {} never came online",
        home.display()
    );
}

fn list_titles(rt: &tokio::runtime::Runtime, home: &Path) -> Vec<String> {
    match issue_req(
        rt,
        home,
        issues_app::IssuesRequest::List {
            project: None,
            filter: issues_app::protocol::Filter::default(),
            page: issues::contract::PageRequest::default(),
        },
    ) {
        IssueResponse::List { page } => page.items.into_iter().map(|r| r.title).collect(),
        _ => Vec::new(),
    }
}

fn new_issue(rt: &tokio::runtime::Runtime, home: &Path, title: &str) -> IssueResponse {
    issue_req(
        rt,
        home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: title.into(),
            project: Some("ENG".into()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    )
}

#[test]
fn restarted_joiner_daemon_reconverges_from_its_persisted_store() {
    let net = MemNet::new();

    // Founder: form, seed a project + first issue, mint an auto-approving invite.
    let founder_home = temp_home("founder");
    crate::world_fixture::found_space(&founder_home, &FOUNDER_SEED, "Restart Space").unwrap();
    let founder_handle = spawn_daemon(founder_home.to_path_buf(), FOUNDER_SEED, net.clone());

    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &founder_home);

    assert!(
        matches!(
            issue_req(
                &rt,
                &founder_home,
                issues_app::IssuesRequest::ProjectNew {
                    name: "Engineering".into(),
                    key: "ENG".into(),
                    color: None,
                }
            ),
            IssueResponse::Ref { .. }
        ),
        "founder: projects new"
    );
    assert!(
        matches!(
            new_issue(&rt, &founder_home, "before restart"),
            IssueResponse::Ref { .. }
        ),
        "founder: first issue"
    );

    let Response::Ref { reff: invite } = req(
        &rt,
        &founder_home,
        Request::Invite {
            world: None,
            role: None,
            reusable: false,
            ttl_hours: Some(24),
        },
    ) else {
        panic!("expected an invite link");
    };

    // Joiner: bootstrap the store from the invite, serve, drive admission.
    let joiner_home = temp_home("joiner");
    crate::world_fixture::enter_space(&joiner_home, &JOINER_SEED, &invite).unwrap();
    let mut joiner_handle = Some(spawn_daemon(
        joiner_home.to_path_buf(),
        JOINER_SEED,
        net.clone(),
    ));
    wait_online(&rt, &joiner_home);

    let founder_device = mechanics::actor::device_from_seed(&FOUNDER_SEED).to_string();
    let joiner_device = mechanics::actor::device_from_seed(&JOINER_SEED).to_string();

    let drive_contact = |rt: &tokio::runtime::Runtime| {
        req(
            rt,
            &joiner_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
        req(
            rt,
            &founder_home,
            Request::Connect {
                ticket: joiner_device.clone(),
            },
        );
        req(
            rt,
            &joiner_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
    };

    let admitted = poll_until(Duration::from_secs(20), || {
        drive_contact(&rt);
        match req(&rt, &joiner_home, Request::Status) {
            Response::Status(info) if info.membership == "member" => Some(()),
            _ => None,
        }
    });
    assert!(admitted.is_some(), "the joiner was never admitted");

    // The joiner converges the founder's pre-restart issue.
    assert!(
        poll_until(Duration::from_secs(20), || {
            drive_contact(&rt);
            list_titles(&rt, &joiner_home)
                .iter()
                .any(|t| t == "before restart")
                .then_some(())
        })
        .is_some(),
        "pre-restart: the joiner did not converge to the founder's first issue"
    );

    // Crash the joiner (kill its daemon thread) — its home/store survive.
    let _ = req(&rt, &joiner_home, Request::Stop);
    let _ = joiner_handle.take().unwrap().join();

    // While the joiner is down, the founder files a new issue under the same key.
    // The founder may still be installing the final Contact publication when
    // the peer goes offline. That is live bounded contention, not shutdown:
    // retry only the typed pre-admission answer and never replay an unknown
    // durable outcome.
    let post_restart = poll_until(Duration::from_secs(20), || {
        match new_issue(&rt, &founder_home, "after restart") {
            response @ IssueResponse::Ref { .. } => Some(response),
            IssueResponse::Error {
                error_kind: issues_app::IssuesErrorKind::Retry,
                ..
            } => None,
            other => panic!("founder: post-restart issue failed terminally: {other:?}"),
        }
    })
    .expect("founder publication did not become writable after Contact");
    assert!(
        matches!(post_restart, IssueResponse::Ref { .. }),
        "founder: post-restart issue: {post_restart:?}"
    );

    // Restart the joiner on the SAME home. It re-docks from its persisted
    // membership (no re-admission) and, once Contact is re-driven, reconverges.
    let joiner_handle = spawn_daemon(joiner_home.to_path_buf(), JOINER_SEED, net.clone());
    wait_online(&rt, &joiner_home);

    // It comes back already a member — membership is persisted, not re-earned.
    match req(&rt, &joiner_home, Request::Status) {
        Response::Status(info) => assert_eq!(
            info.membership, "member",
            "the restarted joiner must reload its membership from the persisted store"
        ),
        other => panic!("status returned {other:?}"),
    }

    assert!(
        poll_until(Duration::from_secs(25), || {
            drive_contact(&rt);
            list_titles(&rt, &joiner_home)
                .iter()
                .any(|t| t == "after restart")
                .then_some(())
        })
        .is_some(),
        "post-restart: the joiner did not rejoin and converge to the founder's new issue"
    );

    let _ = req(&rt, &joiner_home, Request::Stop);
    let _ = req(&rt, &founder_home, Request::Stop);
    let _ = joiner_handle.join();
    let _ = founder_handle.join();
    let _ = std::fs::remove_dir_all(&founder_home);
    let _ = std::fs::remove_dir_all(&joiner_home);
}
