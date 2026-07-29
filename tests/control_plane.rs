//! End-to-end tests for control-plane dirty notifications and `Reset`
//! recovery, driven through a process-backed **SpaceBridge** over its real IPC control
//! socket with an in-memory transport (no network sockets). See
//! `docs/PROTOCOL.md`.
//!
//! Formation is `orbital::form_space` (the `lait init` heir); the SpaceBridge
//! then serves `control::Request`/`Response` — including the `Subscribe`
//! doorbell stream sourced from the Station's `ObservationStream` — exactly as
//! the CLI/serve/MCP clients speak it. Two behaviors are proven: a wildly stale
//! `since` still rebaselines with a `Reset` first frame and a later live edit
//! rings a real (non-reset) doorbell; and a rejected write (validate-then-commit)
//! rings nothing at all.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use issues_app::IssuesResponse as IssueResponse;
use lait::control::{
    request, request_routed, subscribe, CatalogScope, ControlRoute, Request, Response,
};
use lait::daemon::OrbitAddress;
use lait::ids::SpaceId;
use lait::net::Network;
use lait::orbital::run_space_bridge_with;
use lait::transport::mem::MemNet;
use lait::transport::{Alpn, Transport, TransportFactory};

const FOUNDER_SEED: [u8; 32] = [113u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct MemFactory(MemNet);

#[async_trait]
impl TransportFactory for MemFactory {
    async fn build(
        &self,
        identity_seed: &[u8; 32],
        _network: &Network,
        _alpns: &[Alpn],
    ) -> Result<Arc<dyn Transport>> {
        Ok(Arc::new(
            self.0.peer(lait::crypto::device_from_seed(identity_seed)),
        ))
    }
}

fn temp_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-ctrl-{tag}-{}-{n}", std::process::id()));
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

/// The docs a frame names under a project KEY, in frame order.
fn named_docs(frame: &lait::control::Doorbell, key: &str) -> Vec<String> {
    frame
        .dirty_by_project
        .iter()
        .filter(|d| d.project_key == key)
        .flat_map(|d| d.docs.clone())
        .collect()
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
        "SpaceBridge at {} never came online",
        home.display()
    );
}

#[test]
fn explicit_routes_cannot_cross_space_or_world_boundaries() {
    let net = MemNet::new();
    let home = temp_home("routes");
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Route Space").unwrap();
    let space = lait::orbital::discover_space_id(&home).unwrap();
    let address = OrbitAddress::for_store(&home, space.clone());
    let handle = spawn_daemon(home.clone(), FOUNDER_SEED, net);
    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &home);

    let response = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Space {
                address: address.clone(),
            },
        ))
        .unwrap();
    assert!(matches!(response, Response::Status(_)));

    let wrong_space = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Space {
                address: OrbitAddress {
                    orbit: address.orbit.clone(),
                    space: SpaceId::from_digest([0xEE; 16]),
                },
            },
        ))
        .unwrap();
    assert!(matches!(
        wrong_space,
        Response::Error { message, .. } if message.contains("this bridge owns")
    ));

    let wrong_orbit = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Space {
                address: OrbitAddress::for_store(&home.with_extension("sibling"), space.clone()),
            },
        ))
        .unwrap();
    assert!(matches!(
        wrong_orbit,
        Response::Error { message, .. } if message.contains("this bridge occupies")
    ));

    let files_call = lait::orbital::WorldCall::new(
        replica::ids::WorldId::parse("com.example.files").unwrap(),
        "files.list",
        1,
        Vec::new(),
    )
    .unwrap();
    let missing_world = rt
        .block_on(lait::control::call_world(
            &home,
            ControlRoute::World {
                address: address.clone(),
                world: "com.example.files".into(),
            },
            files_call,
            None,
        ))
        .unwrap();
    assert!(matches!(
        missing_world.into_result(),
        Err(error) if error.message.contains("is not enabled")
    ));

    let issues_call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList).unwrap();
    let wrong_level = rt
        .block_on(lait::control::call_world(
            &home,
            ControlRoute::Space {
                address: address.clone(),
            },
            issues_call,
            None,
        ))
        .unwrap();
    assert!(matches!(
        wrong_level.into_result(),
        Err(error) if error.message.contains("requires an explicit World route")
    ));

    let rejected_stop = rt
        .block_on(request_routed(
            &home,
            &Request::Stop,
            ControlRoute::Space {
                address: OrbitAddress {
                    orbit: address.orbit.clone(),
                    space: SpaceId::from_digest([0xEF; 16]),
                },
            },
        ))
        .unwrap();
    assert!(matches!(
        rejected_stop,
        Response::Error { message, .. } if message.contains("this bridge owns")
    ));
    let still_online = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Space { address },
        ))
        .unwrap();
    assert!(
        matches!(still_online, Response::Status(_)),
        "a rejected Stop must leave the addressed SpaceBridge online"
    );

    rt.block_on(async {
        let _ = request(&home, &Request::Stop).await;
    });
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// Seed a project + one issue and return the issue's canonical ref (e.g.
/// `ENG-1`). Exercises the World submit path that feeds the doorbell.
fn seed_project_and_issue(rt: &tokio::runtime::Runtime, home: &Path) -> String {
    let resp = issue_req(
        rt,
        home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Eng".into(),
            key: "ENG".into(),
            color: None,
        },
    );
    assert!(
        matches!(resp, IssueResponse::Ref { .. }),
        "projects new should echo a Ref, got {resp:?}"
    );
    let resp = issue_req(
        rt,
        home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "t1".into(),
            project: Some("ENG".into()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    );
    match resp {
        IssueResponse::Ref { reff } => reff,
        other => panic!("issue new should echo a Ref, got {other:?}"),
    }
}

/// A deliberately-stale `since` must not cause silent deafness: the daemon
/// always rebaselines a new Subscribe with a `Reset` first frame at the current
/// sequence, and a subsequent live edit then rings a real, non-reset doorbell.
#[test]
fn stale_since_after_restart_yields_reset() {
    let net = MemNet::new();
    let home = temp_home("stale");
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.clone(), FOUNDER_SEED, net.clone());

    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &home);

    let reff = seed_project_and_issue(&rt, &home);

    rt.block_on(async {
        // A wildly stale `since` — this must NOT make the stream deaf.
        let mut sub = subscribe(&home, 999_999)
            .await
            .expect("open subscribe stream");

        // First frame is ALWAYS a Reset (rebaseline from a fresh snapshot).
        let first = sub
            .next()
            .await
            .expect("read first frame")
            .expect("first frame present");
        assert!(
            first.reset,
            "first Subscribe frame must be a Reset even for a stale since, got {first:?}"
        );

        // A live edit rings a real doorbell: non-reset, advancing activity.
        let resp = issues_request(
            &home,
            issues_app::IssuesRequest::IssueEdit {
                due: None,
                estimate: None,
                reff: reff.clone(),
                title: None,
                status: Some("in_progress".into()),
                priority: None,
                description: None,
            },
        )
        .await
        .expect("issue edit");
        assert!(
            matches!(resp, IssueResponse::Ref { .. }),
            "valid edit should echo a Ref, got {resp:?}"
        );

        let ring = sub
            .next()
            .await
            .expect("read edit doorbell")
            .expect("edit doorbell present");
        assert!(
            !ring.reset,
            "a live edit should ring a normal (non-reset) doorbell, got {ring:?}"
        );
        assert!(
            ring.activity_advanced,
            "the edit doorbell must advance activity, got {ring:?}"
        );

        let _ = request(&home, &Request::Stop).await;
    });

    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// A doorbell must say *what* moved, not merely that something did.
///
/// The Observation the Station publishes names Bodies; a client re-reads by
/// project and doc. Without the translation in between, every live frame is an
/// empty dirty-set — the client has news it cannot act on, so a board sits stale
/// until something else forces a rebaseline. That is what this pins:
///
/// - a field edit names its doc under its project KEY, and touches no catalog
///   plane (the edit is confined to the issue Body);
/// - a create, which does move the catalog, rings the catalog scopes too — and
///   still names the brand-new doc, which only works if a miss rebuilds the
///   index rather than dropping the scope.
#[test]
fn doorbell_names_the_dirty_project_and_doc() {
    let net = MemNet::new();
    let home = temp_home("dirty");
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.clone(), FOUNDER_SEED, net.clone());

    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &home);

    let reff = seed_project_and_issue(&rt, &home);
    let doc = match issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::List {
            project: None,
            filter: Default::default(),
        },
    ) {
        // The only row there is — `reff` may be the `ENG-1` alias rather than the
        // canonical handle the row carries, so match on the seeding, not the text.
        IssueResponse::List { rows } => match rows.as_slice() {
            [row] => row.doc_id.as_str().to_string(),
            other => panic!("expected exactly the seeded issue, got {other:?}"),
        },
        other => panic!("list should echo rows, got {other:?}"),
    };

    rt.block_on(async {
        let mut sub = subscribe(&home, 0).await.expect("open subscribe stream");
        let first = sub
            .next()
            .await
            .expect("read first frame")
            .expect("first frame present");
        assert!(first.reset, "first Subscribe frame must be a Reset");

        // A field edit: one issue Body, no catalog plane.
        issues_request(
            &home,
            issues_app::IssuesRequest::IssueEdit {
                due: None,
                estimate: None,
                reff: reff.clone(),
                title: Some("renamed".into()),
                status: None,
                priority: None,
                description: None,
            },
        )
        .await
        .expect("issue edit");

        let ring = sub
            .next()
            .await
            .expect("read edit doorbell")
            .expect("edit doorbell present");
        let named = named_docs(&ring, "ENG");
        assert_eq!(
            named,
            vec![doc.clone()],
            "the edit doorbell must name the edited doc under its project, got {ring:?}"
        );
        assert!(
            ring.dirty_catalog.is_empty(),
            "a field edit touches no catalog plane, got {ring:?}"
        );

        // A create: a new issue Body *and* the catalog (aliases, seqs, board).
        let created = match issues_request(
            &home,
            issues_app::IssuesRequest::IssueNew {
                due: None,
                estimate: None,
                title: "t2".into(),
                project: Some("ENG".into()),
                project_hint: None,
                assignees: vec![],
                priority: None,
                labels: vec![],
                body: None,
            },
        )
        .await
        .expect("issue new")
        {
            IssueResponse::Ref { reff } => reff,
            other => panic!("issue new should echo a Ref, got {other:?}"),
        };

        let ring = sub
            .next()
            .await
            .expect("read create doorbell")
            .expect("create doorbell present");
        let named = named_docs(&ring, "ENG");
        assert!(
            named.len() == 1 && named[0] != doc,
            "the create must name the NEW doc ({created}), got {ring:?}"
        );
        // Precision, not just presence. A create adds a row and puts it on a
        // board; it does not touch the label registry, the workflow, or any
        // other project. Ringing those would be the coarseness this replaced.
        assert!(
            ring.dirty_catalog.contains(&CatalogScope::Docs),
            "a create moves the row index, got {ring:?}"
        );
        assert!(
            ring.dirty_catalog.iter().any(
                |s| matches!(s, CatalogScope::Boards { project_key, .. } if project_key == "ENG")
            ),
            "a create puts the row on ENG's board, got {ring:?}"
        );
        // A project is named by its stable id as well as its display key, so a
        // dependency can match on something a rename cannot move.
        assert!(
            ring.dirty_catalog.iter().any(|s| matches!(
                s,
                CatalogScope::Boards { project_id, .. } if project_id.starts_with("prj_")
            )),
            "the board plane must carry the project's stable id, got {ring:?}"
        );
        assert!(
            !ring.authority_advanced,
            "a create moves no membership, got {ring:?}"
        );
        for untouched in [
            CatalogScope::Labels,
            CatalogScope::Workflow,
            CatalogScope::Teams,
        ] {
            assert!(
                !ring.dirty_catalog.contains(&untouched),
                "a create rang {untouched:?}, which it does not touch: {ring:?}"
            );
        }

        let _ = request(&home, &Request::Stop).await;
    });

    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// Two subscribers, one commit, identical frames.
///
/// The dirty-set is computed once per Observation and fanned out, not recomputed
/// per subscriber. That is an efficiency win today — a viewer and a `lait watch`
/// used to cost two full catalog reads per commit — and a correctness
/// requirement the moment the translation holds any state carried between rings:
/// two subscribers each advancing one shared baseline would leave the second
/// seeing nothing changed.
#[test]
fn every_subscriber_sees_the_same_frame_for_one_commit() {
    let net = MemNet::new();
    let home = temp_home("fanout");
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.clone(), FOUNDER_SEED, net.clone());

    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &home);
    let reff = seed_project_and_issue(&rt, &home);

    rt.block_on(async {
        let mut a = subscribe(&home, 0).await.expect("subscriber A");
        let mut b = subscribe(&home, 0).await.expect("subscriber B");
        for (who, sub) in [("A", &mut a), ("B", &mut b)] {
            let first = sub.next().await.expect("read").expect("present");
            assert!(first.reset, "{who}'s first frame must be a Reset");
        }

        issues_request(
            &home,
            issues_app::IssuesRequest::IssueEdit {
                due: None,
                estimate: None,
                reff: reff.clone(),
                title: Some("fanned out".into()),
                status: None,
                priority: None,
                description: None,
            },
        )
        .await
        .expect("issue edit");

        let fa = a.next().await.expect("A reads").expect("A has a frame");
        let fb = b.next().await.expect("B reads").expect("B has a frame");
        assert_eq!(
            (fa.seq, &fa.dirty_by_project, &fa.dirty_catalog),
            (fb.seq, &fb.dirty_by_project, &fb.dirty_catalog),
            "subscribers disagreed about one commit: A={fa:?} B={fb:?}"
        );
        assert!(
            !fa.dirty_by_project.is_empty(),
            "both agreed, but on an empty dirty-set — that would pass for the \
             wrong reason: {fa:?}"
        );

        let _ = request(&home, &Request::Stop).await;
    });

    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// A rejected write rings nothing: validate-then-commit means an invalid
/// `IssueEdit` returns an `Error` having touched nothing and produced no
/// dirty-set, so no doorbell arrives. We drain the initial Reset, send a bad
/// status, and assert the stream stays silent for a grace window.
#[test]
fn validate_then_commit_rings_no_doorbell() {
    let net = MemNet::new();
    let home = temp_home("reject");
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.clone(), FOUNDER_SEED, net.clone());

    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &home);

    let reff = seed_project_and_issue(&rt, &home);

    rt.block_on(async {
        let mut sub = subscribe(&home, 0).await.expect("open subscribe stream");

        // Drain the initial Reset frame.
        let first = sub
            .next()
            .await
            .expect("read first frame")
            .expect("first frame present");
        assert!(first.reset, "first Subscribe frame must be a Reset");

        // An invalid status is rejected pre-commit.
        let resp = issues_request(
            &home,
            issues_app::IssuesRequest::IssueEdit {
                due: None,
                estimate: None,
                reff: reff.clone(),
                title: None,
                status: Some("definitely-not-a-status".into()),
                priority: None,
                description: None,
            },
        )
        .await
        .expect("issue edit request round-trips");
        assert!(
            matches!(resp, IssueResponse::Error { .. }),
            "an invalid status must be rejected, got {resp:?}"
        );

        // No doorbell must arrive for the rejected write. The only acceptable
        // outcome is the read timing out (nothing rang).
        match tokio::time::timeout(Duration::from_millis(400), sub.next()).await {
            Err(_elapsed) => { /* good: the stream stayed silent */ }
            Ok(Ok(Some(db))) => panic!("a rejected write rang a doorbell: {db:?}"),
            Ok(Ok(None)) => panic!("subscription closed unexpectedly (daemon gone?)"),
            Ok(Err(e)) => panic!("subscription read errored: {e:#}"),
        }

        let _ = request(&home, &Request::Stop).await;
    });

    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}
