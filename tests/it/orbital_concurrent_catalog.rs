//! Multi-writer catalog convergence through two process-backed StationHosts: the JOINER
//! creates issues (a concurrent catalog write — the founder is also
//! registering issues), and the founder's product views must converge to the
//! union. This is the daemon-level pin for the constituent-head model
//! (`crates/replica/tests/concurrent_heads.rs` proves it at the replica
//! layer); it is exactly the flow the 32-actor reference corpus runs at scale.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use comms::mem::MemNet;
use comms::policy::Network;
use comms::{Transport, TransportFactory};
use issues_app::IssuesResponse as IssueResponse;
use lait::control::OrbitAddress;
use lait::control::{request, ControlRoute, Request, Response};
use lait::orbital::run_station_process_with;

const FOUNDER_SEED: [u8; 32] = [221u8; 32];
const JOINER_SEED: [u8; 32] = [222u8; 32];

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

fn temp_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-ccat-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
    assert!(online.is_some(), "daemon never answered Status");
}

fn list_titles(rt: &tokio::runtime::Runtime, home: &Path) -> Vec<String> {
    match issue_req(
        rt,
        home,
        issues_app::IssuesRequest::List {
            project: None,
            filter: issues_app::Filter {
                all: true,
                ..Default::default()
            },
        },
    ) {
        IssueResponse::List { rows } => rows.iter().map(|r| r.title.clone()).collect(),
        _ => vec![],
    }
}

#[test]
fn concurrent_issue_creation_converges_across_daemons() {
    let net = MemNet::new();
    let founder_home = temp_home("founder");
    lait::orbital::form_space(&founder_home, &FOUNDER_SEED, "Concurrent Catalog").unwrap();
    let _founder = spawn_daemon(founder_home.clone(), FOUNDER_SEED, net.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &founder_home);
    let founder_device = mechanics::actor::device_from_seed(&FOUNDER_SEED).to_string();
    let joiner_device = mechanics::actor::device_from_seed(&JOINER_SEED).to_string();

    // Founder authors one issue BEFORE the joiner exists.
    let resp = issue_req(
        &rt,
        &founder_home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "founder issue".into(),
            project: None,
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    );
    assert!(matches!(resp, IssueResponse::Ref { .. }), "{resp:?}");

    // Join + admit.
    let Response::Ref { reff: invite } = req(
        &rt,
        &founder_home,
        Request::Invite {
            role: Some("contributor".into()),
            reusable: false,
            ttl_hours: Some(24),
        },
    ) else {
        panic!("invite");
    };
    let joiner_home = temp_home("joiner");
    lait::orbital::enter_space(&joiner_home, &JOINER_SEED, &invite).unwrap();
    let _joiner = spawn_daemon(joiner_home.clone(), JOINER_SEED, net.clone());
    wait_online(&rt, &joiner_home);
    let admitted = poll_until(Duration::from_secs(30), || {
        req(
            &rt,
            &joiner_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
        req(
            &rt,
            &founder_home,
            Request::Connect {
                ticket: joiner_device.clone(),
            },
        );
        req(
            &rt,
            &joiner_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
        match req(&rt, &joiner_home, Request::Status) {
            Response::Status(info) if info.membership == "member" => Some(()),
            _ => None,
        }
    });
    assert!(admitted.is_some(), "joiner never admitted");

    // A SECOND joiner (viewer tier) is admitted before the concurrent
    // authoring — matching the reference corpus: its admission rotates the
    // epoch again, so the contributor authors under an epoch minted after its
    // own admission.
    const VIEWER_SEED: [u8; 32] = [223u8; 32];
    let Response::Ref { reff: vinvite } = req(
        &rt,
        &founder_home,
        Request::Invite {
            role: Some("viewer".into()),
            reusable: false,
            ttl_hours: Some(24),
        },
    ) else {
        panic!("viewer invite");
    };
    let viewer_home = temp_home("viewer");
    lait::orbital::enter_space(&viewer_home, &VIEWER_SEED, &vinvite).unwrap();
    let _viewer = spawn_daemon(viewer_home.clone(), VIEWER_SEED, net.clone());
    wait_online(&rt, &viewer_home);
    let viewer_device = mechanics::actor::device_from_seed(&VIEWER_SEED).to_string();
    let vadmitted = poll_until(Duration::from_secs(30), || {
        req(
            &rt,
            &viewer_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
        req(
            &rt,
            &founder_home,
            Request::Connect {
                ticket: viewer_device.clone(),
            },
        );
        req(
            &rt,
            &viewer_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
        match req(&rt, &viewer_home, Request::Status) {
            Response::Status(info) if info.membership == "member" => Some(()),
            _ => None,
        }
    });
    assert!(vadmitted.is_some(), "viewer never admitted");

    // Both sides now create issues CONCURRENTLY (catalog writes on each).
    for i in 0..3 {
        let resp = issue_req(
            &rt,
            &founder_home,
            issues_app::IssuesRequest::IssueNew {
                due: None,
                estimate: None,
                title: format!("founder concurrent {i}"),
                project: None,
                project_hint: None,
                assignees: vec![],
                priority: None,
                labels: vec![],
                body: None,
            },
        );
        assert!(matches!(resp, IssueResponse::Ref { .. }), "{resp:?}");
        let resp = issue_req(
            &rt,
            &joiner_home,
            issues_app::IssuesRequest::IssueNew {
                due: None,
                estimate: None,
                title: format!("joiner concurrent {i}"),
                project: None,
                project_hint: None,
                assignees: vec![],
                priority: None,
                labels: vec![],
                body: None,
            },
        );
        assert!(matches!(resp, IssueResponse::Ref { .. }), "{resp:?}");
    }

    // Pump Contact both ways until BOTH daemons list the union (8 issues).
    let converged = poll_until(Duration::from_secs(45), || {
        req(
            &rt,
            &joiner_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
        req(
            &rt,
            &founder_home,
            Request::Connect {
                ticket: joiner_device.clone(),
            },
        );
        let f = list_titles(&rt, &founder_home);
        let j = list_titles(&rt, &joiner_home);
        (f.len() >= 7 && j.len() >= 7).then_some((f, j))
    });
    let Some((f, j)) = converged else {
        panic!(
            "catalog union never converged: founder={:?} joiner={:?}",
            list_titles(&rt, &founder_home),
            list_titles(&rt, &joiner_home)
        );
    };
    for side in [&f, &j] {
        assert!(side.iter().any(|t| t == "founder issue"));
        assert!(side.iter().any(|t| t == "joiner concurrent 2"));
        assert!(side.iter().any(|t| t == "founder concurrent 2"));
    }
    // The read-only viewer converges the same union by pulling the founder.
    let vconverged = poll_until(Duration::from_secs(30), || {
        req(
            &rt,
            &viewer_home,
            Request::Connect {
                ticket: founder_device.clone(),
            },
        );
        let v = list_titles(&rt, &viewer_home);
        (v.len() >= 7).then_some(v)
    });
    let Some(v) = vconverged else {
        panic!(
            "viewer never converged: {:?}",
            list_titles(&rt, &viewer_home)
        );
    };
    assert!(v.iter().any(|t| t == "joiner concurrent 1"));
    let _ = req(&rt, &viewer_home, Request::Stop);

    let _ = req(&rt, &joiner_home, Request::Stop);
    let _ = req(&rt, &founder_home, Request::Stop);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::fs::remove_dir_all(&founder_home);
    let _ = std::fs::remove_dir_all(&joiner_home);
}
