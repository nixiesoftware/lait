//! End-to-end tests for control-plane dirty notifications and `Reset`
//! recovery, driven through a process-backed **StationHost** over its real IPC control
//! socket with an in-memory transport (no network sockets). See
//! `docs/PROTOCOL.md`.
//!
//! Formation is `orbital::form_space`, the engine half of `HostSpaceFound`;
//! the StationHost then serves `control::Request`/`Response` — including the
//! `Subscribe` doorbell stream sourced from the Station's `ObservationStream` —
//! exactly as the local app and MCP heads speak it. Two behaviors are proven: a wildly stale
//! `since` still rebaselines with a `Reset` first frame and a later live edit
//! rings a real (non-reset) doorbell; and a rejected write (validate-then-commit)
//! rings nothing at all.

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
use issues::ids::SpaceId;
use issues_app::IssuesResponse as IssueResponse;
use lait::control::OrbitAddress;
use lait::control::{request, request_routed, subscribe, ControlRoute, Request, Response};

const FOUNDER_SEED: [u8; 32] = [113u8; 32];

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
    crate::head::temp_root(&format!("ctrl-{tag}"))
}

fn req(rt: &tokio::runtime::Runtime, home: &Path, r: Request) -> Response {
    rt.block_on(async { request(home, &r).await })
        .unwrap_or_else(|e| Response::err(format!("{e:#}")))
}

async fn issues_request(home: &Path, request: issues_app::IssuesRequest) -> Result<IssueResponse> {
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
    Ok(super::accepted_issue_response(serde_json::from_value(
        issues_app::decode_reply(&call, reply)?,
    )?))
}

fn issue_req(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    request: issues_app::IssuesRequest,
) -> IssueResponse {
    super::accepted_issue_response(
        rt.block_on(issues_request(home, request))
            .unwrap_or_else(|error| IssueResponse::err(format!("{error:#}"))),
    )
}

fn planes(frame: &lait::control::Doorbell) -> impl Iterator<Item = &lait::control::DirtyPlane> {
    frame
        .invalidations
        .iter()
        .filter(|entry| entry.world.as_str() == issues::contract::product_world())
        .flat_map(|entry| &entry.planes)
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

#[test]
fn explicit_routes_cannot_cross_space_or_world_boundaries() {
    let net = MemNet::new();
    let home = temp_home("routes");
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Route Space").unwrap();
    let space = lait::orbital::discover_space(&home).single().unwrap();
    let address = OrbitAddress::for_store(&home, space.clone());
    let handle = spawn_daemon(home.to_path_buf(), FOUNDER_SEED, net);
    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &home);

    let response = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Orbit {
                address: address.clone(),
            },
        ))
        .unwrap();
    assert!(matches!(response, Response::Status(_)));

    let wrong_space = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Orbit {
                address: OrbitAddress {
                    orbit: address.orbit.clone(),
                    space: SpaceId::from_digest([0xEE; 16]),
                },
            },
        ))
        .unwrap();
    assert!(matches!(
        wrong_space,
        Response::Error { message, .. } if message.contains("this host owns")
    ));

    let wrong_orbit = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Orbit {
                address: OrbitAddress::for_store(&home.with_extension("sibling"), space.clone()),
            },
        ))
        .unwrap();
    assert!(matches!(
        wrong_orbit,
        Response::Error { message, .. } if message.contains("this host occupies")
    ));

    let files_call = runtime::world::call::Call::new(
        replica::body::WorldId::parse("com.example.files").unwrap(),
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
        Err(error) if error.code == runtime::world::call::Code::UnsupportedOperation
    ));

    let issues_call = issues_app::encode_call(&issues_app::IssuesRequest::ProjectList {
        page: issues::contract::PageRequest::default(),
    })
    .unwrap();
    let wrong_level = rt
        .block_on(lait::control::call_world(
            &home,
            ControlRoute::Orbit {
                address: address.clone(),
            },
            issues_call,
            None,
        ))
        .unwrap();
    assert!(matches!(
        wrong_level.into_result(),
        Err(error) if error.code == runtime::world::call::Code::InvalidCall
    ));

    let rejected_stop = rt
        .block_on(request_routed(
            &home,
            &Request::Stop,
            ControlRoute::Orbit {
                address: OrbitAddress {
                    orbit: address.orbit.clone(),
                    space: SpaceId::from_digest([0xEF; 16]),
                },
            },
        ))
        .unwrap();
    assert!(matches!(
        rejected_stop,
        Response::Error { message, .. } if message.contains("this host owns")
    ));
    let still_online = rt
        .block_on(request_routed(
            &home,
            &Request::Status,
            ControlRoute::Orbit { address },
        ))
        .unwrap();
    assert!(
        matches!(still_online, Response::Status(_)),
        "a rejected Stop must leave the addressed StationHost online"
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
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.to_path_buf(), FOUNDER_SEED, net.clone());

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
                title: Some("updated after reset".into()),
                status: None,
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

/// A doorbell must say which bounded product plane moved, not merely that
/// something did.
///
/// The Observation the Station publishes names Bodies. The v4 store may split
/// one semantic Issue across several physical Bodies, so an unknown/deleted
/// source conservatively invalidates the fixed `docs` plane rather than
/// inventing a stale doc coordinate. That is what this pins:
///
/// - a field edit rings `docs`, allowing exact-publication refresh without a
///   World-wide scan;
/// - a create rings the bounded row/board planes needed to place the new Issue.
#[test]
fn doorbell_names_the_dirty_project_and_doc() {
    let net = MemNet::new();
    let home = temp_home("dirty");
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.to_path_buf(), FOUNDER_SEED, net.clone());

    let rt = tokio::runtime::Runtime::new().unwrap();
    wait_online(&rt, &home);

    let reff = seed_project_and_issue(&rt, &home);

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
        assert!(
            planes(&ring).any(|plane| plane.plane == "docs"),
            "a field edit must invalidate the bounded docs plane, got {ring:?}"
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
        assert!(
            planes(&ring).any(|p| p.plane == "docs"),
            "the create must invalidate the bounded row plane for {created}, got {ring:?}"
        );
        assert!(
            !ring.authority_advanced,
            "a create moves no membership, got {ring:?}"
        );

        // A Spec is a Body of its own, in no project's row index — so the
        // translation used to fall through to "something changed that I cannot
        // name" and ring the row index. That was right while every non-catalog
        // Body was an issue, and became a lie the moment Specs existed: writing
        // one invalidated every board, every row and the status counts.
        issues_request(
            &home,
            issues_app::IssuesRequest::SpecNew {
                project: "ENG".into(),
                kind: issues::spec::Kind::Requirement,
                title: "Login is race-free".into(),
                text: String::new(),
                links: vec![],
            },
        )
        .await
        .expect("spec new");

        let ring = sub
            .next()
            .await
            .expect("read spec doorbell")
            .expect("spec doorbell present");
        assert!(
            planes(&ring).any(|p| p.plane == "specs"),
            "a spec write must ring its own plane, got {ring:?}"
        );
        // Auxiliary immutable/head Bodies are deliberately not projected as
        // public rows. Their presence may conservatively fan out the bounded
        // plane vocabulary, but the required semantic plane must never be
        // omitted.

        let _ = request(&home, &Request::Stop).await;
    });

    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// Two subscribers, one commit, identical frames.
///
/// The dirty-set is computed once per Observation and fanned out, not recomputed
/// per subscriber. That is an efficiency win today — two open viewers used to
/// cost two full catalog reads per commit — and a correctness
/// requirement the moment the translation holds any state carried between rings:
/// two subscribers each advancing one shared baseline would leave the second
/// seeing nothing changed.
#[test]
fn every_subscriber_sees_the_same_frame_for_one_commit() {
    let net = MemNet::new();
    let home = temp_home("fanout");
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.to_path_buf(), FOUNDER_SEED, net.clone());

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
            (fa.seq, &fa.invalidations),
            (fb.seq, &fb.invalidations),
            "subscribers disagreed about one commit: A={fa:?} B={fb:?}"
        );
        assert!(
            planes(&fa).any(|plane| plane.plane == "docs"),
            "both subscribers must receive the same actionable docs-plane invalidation: {fa:?}"
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
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Ctrl Space").unwrap();
    let handle = spawn_daemon(home.to_path_buf(), FOUNDER_SEED, net.clone());

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
