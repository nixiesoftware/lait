//! The process-backed StationHost serves the product control surface over the
//! real IPC control socket through the orbital Runtime.
//!
//! Formation happens via `SpaceAuthority::form`, the engine half of
//! `HostSpaceFound`; the StationHost then serves `control::Request`/`Response`
//! exactly as the local app and MCP heads speak it. This drives the issue family end to end
//! (project/new/view/list/board/comment) plus status and invite over the wire,
//! with an in-memory transport (no network sockets).

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
use lait::orbital::run_station_process;

const FOUNDER_SEED: [u8; 32] = [101u8; 32];

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

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-odaemon-{}-{n}", std::process::id()));
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

#[test]
fn the_station_host_serves_the_issue_surface_over_the_control_socket() {
    // The daemon runs on a dedicated OS thread with its own runtime (it holds a
    // blocking control accept loop); the test drives it with a separate client
    // runtime, exactly as the real head/daemon split works.
    let home = temp_home();
    let net = MemNet::new();

    // Formation, the engine half of `HostSpaceFound`. Also seed the orbital
    // identity file the daemon reads, so the daemon and formation share one
    // device seed.
    std::fs::create_dir_all(&home).unwrap();
    write_identity(&home, &FOUNDER_SEED);
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Station Host Space").unwrap();

    // Run the daemon on its own thread.
    let daemon_home = home.clone();
    let daemon_net = net.clone();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            if let Err(e) = run_station_process(daemon_home, &MemFactory(daemon_net)).await {
                eprintln!("DAEMON ERR: {e:#}");
            }
        });
    });

    let client_rt = tokio::runtime::Runtime::new().unwrap();

    // Wait for the daemon to answer control requests.
    let online = poll_until(Duration::from_secs(20), || {
        matches!(req(&client_rt, &home, Request::Status), Response::Status(_)).then_some(())
    });
    assert!(online.is_some(), "the StationHost never answered Status");

    // Status reports the founder's real standing — admin, not a flattened
    // "member" (the documented contract is `admin | member | pending`).
    let status = req(&client_rt, &home, Request::Status);
    let Response::Status(info) = status else {
        panic!("expected Status");
    };
    assert!(info.space.is_some());
    assert_eq!(info.membership, "admin");

    // Create a project.
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Engineering".into(),
            key: "eng".into(),
            color: None,
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Ref { reff } if reff == "ENG"),
        "{resp:?}"
    );

    // File an issue; it routes through the World and returns the canonical reff.
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::IssueNew {
            title: "Served over the socket".into(),
            // Formation seeded the default project, so the space has two —
            // pick the explicit one.
            project: Some("eng".into()),
            project_hint: None,
            assignees: vec![],
            priority: Some("high".into()),
            labels: vec![],
            body: Some("through the Station host".into()),
            due: None,
            estimate: None,
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Ref { reff } if reff == "ENG-1"),
        "{resp:?}"
    );

    // View it back.
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::IssueView {
            reff: "ENG-1".into(),
        },
    );
    let IssueResponse::Issue(view) = resp else {
        panic!("expected Issue, got {resp:?}");
    };
    assert_eq!(view.title, "Served over the socket");
    assert_eq!(
        view.description,
        format!(
            "{}through the Station host",
            issues::contract::DOCUMENT_PREFIX
        )
    );
    assert_eq!(view.priority, issues::dto::Priority::High);

    // Comment routes too.
    issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::Comment {
            reff: "ENG-1".into(),
            body: "a socket comment".into(),
            reply_to: None,
        },
    );
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::IssueView {
            reff: "ENG-1".into(),
        },
    );
    let IssueResponse::Issue(view) = resp else {
        panic!("expected Issue");
    };
    assert_eq!(view.comments.len(), 1);
    assert_eq!(view.comments[0].body, "a socket comment");

    // The space-wide activity feed serves through daemon dispatch (this pins
    // the classification/routing defect where an activity request was refused
    // with "request not routed to the issues world"): the created issue and the
    // comment appear as feed rows, and re-pulling from the returned cursor
    // yields nothing new.
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::Activity { since: 0 },
    );
    let IssueResponse::Activity { events, last } = resp else {
        panic!("expected Activity, got {resp:?}");
    };
    assert!(last >= 2, "created + comment rows expected, last={last}");
    assert!(events.iter().any(|e| e.kind == "created"));
    assert!(events
        .iter()
        .any(|e| e.kind == "commented" && e.text == "a socket comment"));
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::Activity { since: last },
    );
    let IssueResponse::Activity { events, last: l2 } = resp else {
        panic!("expected Activity, got {resp:?}");
    };
    assert!(events.is_empty(), "cursor resume must yield no repeats");
    assert_eq!(l2, last);

    // List reflects it.
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::List {
            project: None,
            filter: issues_app::protocol::Filter::default(),
        },
    );
    let IssueResponse::List { rows } = resp else {
        panic!("expected List");
    };
    assert!(rows.iter().any(|r| r.title == "Served over the socket"));

    // Board renders columns.
    let resp = issue_req(
        &client_rt,
        &home,
        issues_app::IssuesRequest::Board {
            project: Some("eng".into()),
            project_hint: None,
        },
    );
    assert!(matches!(resp, IssueResponse::Board(_)), "{resp:?}");

    // Members reports the founder as an admin over the signed ACL roster.
    let resp = req(&client_rt, &home, Request::Members);
    let Response::Members { members } = resp else {
        panic!("expected Members, got {resp:?}");
    };
    assert_eq!(members.len(), 1, "just the founder");
    assert_eq!(members[0].role, "admin");
    assert!(members[0].me, "the founder is this device's actor");

    // The membership audit log replays the signed ACL DAG. A freshly formed
    // Space founds membership by Genesis and mints epoch-0, so the log carries
    // the founder-authored mint (not an AddMember), and every op is recognized.
    let resp = req(&client_rt, &home, Request::MemberLog);
    let Response::MemberLog { entries } = resp else {
        panic!("expected MemberLog, got {resp:?}");
    };
    assert!(!entries.is_empty(), "the audit log is non-empty");
    assert!(
        entries.iter().all(|e| e.authorized),
        "every founding op is authorized: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.kind == "mint_epoch" && !e.actor.is_empty()),
        "the founder-authored epoch-0 mint is present: {entries:?}"
    );

    // Adding a well-formed but unknown actor is refused (its inception is not
    // known locally — no Contact has imported it).
    let unknown = format!("act_{}", "ab".repeat(32));
    let resp = req(
        &client_rt,
        &home,
        Request::MemberAdd {
            who: unknown,
            admin: false,
            as_name: None,
        },
    );
    assert!(
        matches!(resp, Response::Error { .. }),
        "adding an unknown actor is refused, got {resp:?}"
    );

    // Invite mints a Coordinates link (not a SpaceTicket).
    let resp = req(
        &client_rt,
        &home,
        Request::Invite {
            role: None,
            reusable: false,
            ttl_hours: Some(24),
        },
    );
    let Response::Ref { reff: link } = resp else {
        panic!("expected an invite Ref, got {resp:?}");
    };
    assert!(!link.is_empty());
    // It parses back as Coordinates v1.
    assert!(runtime::coordinates::SignedCoordinates::parse_link(&link).is_ok());

    // A ceremony request is served by mechanics, not refused: the founder
    // holds the solo space-recovery key, so break-glass recovery re-roots the
    // space to it and re-keys (M3 — no catch-all "not yet available").
    let resp = req(&client_rt, &home, Request::SpaceRecover);
    assert!(
        matches!(resp, Response::Ok { ref message } if message.as_deref().unwrap_or_default().contains("recovered the space")),
        "{resp:?}"
    );

    // Stop the compatibility process host.
    let _ = req(&client_rt, &home, Request::Stop);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// Write the orbital identity seed where the Station runner's `load_or_create_identity`
/// expects it (the same file founding a Space provisions).
fn write_identity(home: &Path, seed: &[u8; 32]) {
    // The runner reads config::identity_dir(); a $LAIT_HOME-scoped run collapses
    // it onto `home`, so the seed file lives at `home/secret.key`.
    std::env::set_var("LAIT_HOME", home);
    std::fs::write(
        home.join("secret.key"),
        data_encoding::HEXLOWER.encode(seed),
    )
    .unwrap();
}
