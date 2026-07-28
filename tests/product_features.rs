//! The 2026-07-23 product-feature batch, end to end through real orbital
//! daemons over their control sockets: followers (INBOX-9), milestones
//! (SCOPE-1) + cycles (BOARD-11), initiatives (SCOPE-8), teams (GOV-7),
//! triage (SCOPE-7), project delete (CUSTOM-10), and bounded attachments
//! (CREATE-5).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use issues_app::IssuesResponse as IssueResponse;
use lait::control::{request, ControlRoute, Request, Response};
use lait::daemon::OrbitAddress;
use lait::net::Network;
use lait::orbital::run_space_bridge_with;
use lait::transport::mem::MemNet;
use lait::transport::{Alpn, Transport, TransportFactory};

const FOUNDER_SEED: [u8; 32] = [241u8; 32];
const MEMBER_SEED: [u8; 32] = [242u8; 32];

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
    let dir = std::env::temp_dir().join(format!("lait-feat-{tag}-{}-{n}", std::process::id()));
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
        Ok::<IssueResponse, anyhow::Error>(serde_json::from_value(issues_app::decode_reply(
            &call, reply,
        )?)?)
    })
    .unwrap_or_else(|error| IssueResponse::err(format!("{error:#}")))
}

fn ok(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    request: issues_app::IssuesRequest,
) -> IssueResponse {
    let response = issue_req(rt, home, request.clone());
    if let IssueResponse::Error { message, .. } = &response {
        panic!("request {request:?} failed: {message}");
    }
    response
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
    assert!(
        poll_until(Duration::from_secs(20), || {
            matches!(req(rt, home, Request::Status), Response::Status(_)).then_some(())
        })
        .is_some(),
        "daemon never came online"
    );
}

fn new_issue(rt: &tokio::runtime::Runtime, home: &Path, project: &str, title: &str) -> String {
    let resp = ok(
        rt,
        home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: title.into(),
            project: Some(project.into()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    );
    match resp {
        IssueResponse::Ref { reff } => reff,
        other => panic!("IssueNew answered {other:?}"),
    }
}

#[test]
fn milestones_cycles_initiatives_teams_triage_delete_and_attachments() {
    let net = MemNet::new();
    let home = temp_home("solo");
    lait::orbital::form_space(&home, &FOUNDER_SEED, "Feature Space").unwrap();
    let handle = spawn_daemon(home.clone(), FOUNDER_SEED, net.clone());
    let client = tokio::runtime::Runtime::new().unwrap();
    wait_online(&client, &home);

    ok(
        &client,
        &home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Engine".into(),
            key: "eng".into(),
            color: None,
        },
    );
    let issue = new_issue(&client, &home, "eng", "carry a milestone");

    // ---- milestones (SCOPE-1): create, target, assign, progress. ----
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneSet {
            project: "eng".into(),
            milestone: None,
            name: Some("Beta".into()),
            target: Some("2026-09-01".into()),
            remove: false,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueMilestone {
            reff: issue.clone(),
            milestone: Some("Beta".into()),
        },
    );
    let IssueResponse::Milestones { milestones } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneList {
            project: "eng".into(),
        },
    ) else {
        panic!("expected Milestones");
    };
    assert_eq!(milestones.len(), 1);
    assert_eq!(milestones[0].name, "Beta");
    assert_eq!((milestones[0].done, milestones[0].total), (0, 1));
    // Completing the issue moves the derived progress.
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueDone {
            reff: issue.clone(),
        },
    );
    let IssueResponse::Milestones { milestones } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneList {
            project: "eng".into(),
        },
    ) else {
        panic!("expected Milestones");
    };
    assert_eq!((milestones[0].done, milestones[0].total), (1, 1));

    // ---- cycles (BOARD-11): box, schedule, counts. ----
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::CycleSet {
            project: "eng".into(),
            cycle: None,
            name: Some("Sprint 1".into()),
            start: Some("2026-08-01".into()),
            end: Some("2026-08-14".into()),
            remove: false,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueCycle {
            reff: issue.clone(),
            cycle: Some("Sprint 1".into()),
        },
    );
    let IssueResponse::Cycles { cycles } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::CycleList {
            project: "eng".into(),
        },
    ) else {
        panic!("expected Cycles");
    };
    assert_eq!(cycles.len(), 1);
    assert_eq!((cycles[0].done, cycles[0].total), (1, 1));
    assert!(cycles[0].start > 0 && cycles[0].end > cycles[0].start);

    // ---- initiatives (SCOPE-8): group projects, roll up. ----
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::InitiativeSet {
            initiative: None,
            name: Some("Q3 platform".into()),
            description: Some("everything ships".into()),
            owner: None,
            health: Some("on_track".into()),
            target: Some("2026-09-30".into()),
            add_projects: vec![],
            remove_projects: vec![],
            remove: false,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::InitiativeSet {
            initiative: Some("Q3 platform".into()),
            name: None,
            description: None,
            owner: None,
            health: None,
            target: None,
            add_projects: vec!["eng".into()],
            remove_projects: vec![],
            remove: false,
        },
    );
    let IssueResponse::Initiatives { initiatives } =
        ok(&client, &home, issues_app::IssuesRequest::InitiativeList)
    else {
        panic!("expected Initiatives");
    };
    assert_eq!(initiatives.len(), 1);
    assert_eq!(initiatives[0].projects, vec!["ENG".to_string()]);
    assert_eq!((initiatives[0].done, initiatives[0].total), (1, 1));

    // ---- teams (GOV-7): entity, membership, project ownership. ----
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::TeamSet {
            team: None,
            name: Some("Platform".into()),
            key: Some("plt".into()),
            icon: None,
            lead: None,
            add_members: vec![],
            remove_members: vec![],
            remove: false,
        },
    );
    let me = format!("act_{}", "a".repeat(64));
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::TeamSet {
            team: Some("PLT".into()),
            name: None,
            key: None,
            icon: None,
            lead: None,
            add_members: vec![me.clone()],
            remove_members: vec![],
            remove: false,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::ProjectEdit {
            project: "eng".into(),
            name: None,
            color: None,
            description: None,
            lead: None,
            start: None,
            target: None,
            team: Some("PLT".into()),
            archived: None,
        },
    );
    let IssueResponse::Teams { teams } = ok(&client, &home, issues_app::IssuesRequest::TeamList)
    else {
        panic!("expected Teams");
    };
    assert_eq!(teams.len(), 1);
    assert_eq!(teams[0].key, "PLT");
    assert_eq!(teams[0].members, vec![me]);
    assert_eq!(teams[0].projects, vec!["ENG".to_string()]);

    // ---- triage (SCOPE-7): submit, accept, decline, duplicate. ----
    let IssueResponse::Ref { reff: t_accept } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::TriageSubmit {
            title: "login breaks on refresh".into(),
            body: Some("steps: refresh twice".into()),
            source: None,
        },
    ) else {
        panic!("expected Ref");
    };
    let IssueResponse::Ref { reff: t_decline } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::TriageSubmit {
            title: "make it web scale".into(),
            body: None,
            source: Some("suggestion-box".into()),
        },
    ) else {
        panic!("expected Ref");
    };
    let IssueResponse::Ref { reff: t_dupe } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::TriageSubmit {
            title: "milestone thing again".into(),
            body: None,
            source: None,
        },
    ) else {
        panic!("expected Ref");
    };
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::TriageDecide {
            id: t_accept.clone(),
            outcome: "accepted".into(),
            project: Some("eng".into()),
            target: None,
            note: None,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::TriageDecide {
            id: t_decline.clone(),
            outcome: "declined".into(),
            project: None,
            target: None,
            note: Some("not actionable".into()),
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::TriageDecide {
            id: t_dupe.clone(),
            outcome: "duplicate".into(),
            project: None,
            target: Some(issue.clone()),
            note: None,
        },
    );
    // Deciding twice is refused.
    let resp = issue_req(
        &client,
        &home,
        issues_app::IssuesRequest::TriageDecide {
            id: t_decline.clone(),
            outcome: "accepted".into(),
            project: Some("eng".into()),
            target: None,
            note: None,
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Error { message, .. } if message.contains("already decided")),
        "double decide must refuse: {resp:?}"
    );
    let IssueResponse::TriageItems { items } =
        ok(&client, &home, issues_app::IssuesRequest::TriageList)
    else {
        panic!("expected TriageItems");
    };
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|i| !i.outcome.is_empty()), "{items:?}");
    let accepted = items.iter().find(|i| i.id == t_accept).unwrap();
    assert_eq!(accepted.outcome, "accepted");
    assert!(!accepted.reff.is_empty(), "accepted names its issue");
    // The accepted issue is a real, listed issue carrying the intake body.
    let IssueResponse::Issue(view) = ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueView {
            reff: accepted.reff.clone(),
        },
    ) else {
        panic!("expected Issue");
    };
    assert_eq!(view.title, "login breaks on refresh");
    assert_eq!(view.description, "steps: refresh twice");

    // ---- attachments (CREATE-5): attach, list, fetch, cap, detach. ----
    let payload = b"tiny attachment payload".to_vec();
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::Attach {
            reff: issue.clone(),
            name: "notes.txt".into(),
            mime: Some("text/plain".into()),
            data_b64: data_encoding::BASE64.encode(&payload),
            comment: None,
        },
    );
    let IssueResponse::Issue(view) = ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueView {
            reff: issue.clone(),
        },
    ) else {
        panic!("expected Issue");
    };
    assert_eq!(view.attachments.len(), 1);
    assert_eq!(view.attachments[0].name, "notes.txt");
    assert_eq!(view.attachments[0].size, payload.len() as u64);
    let att_id = view.attachments[0].id.clone();
    let IssueResponse::Attachment {
        name,
        mime,
        data_b64,
    } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::AttachmentGet {
            reff: issue.clone(),
            id: att_id.clone(),
        },
    )
    else {
        panic!("expected Attachment");
    };
    assert_eq!(name, "notes.txt");
    assert_eq!(mime, "text/plain");
    assert_eq!(
        data_encoding::BASE64.decode(data_b64.as_bytes()).unwrap(),
        payload
    );
    // Over the cap refuses loudly.
    let big = vec![0u8; lait::world::contract::MAX_ATTACHMENT_BYTES + 1];
    let resp = issue_req(
        &client,
        &home,
        issues_app::IssuesRequest::Attach {
            reff: issue.clone(),
            name: "big.bin".into(),
            mime: None,
            data_b64: data_encoding::BASE64.encode(&big),
            comment: None,
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Error { message, .. } if message.contains("KiB")),
        "oversize must refuse: {resp:?}"
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::Detach {
            reff: issue.clone(),
            id: att_id,
        },
    );
    let IssueResponse::Issue(view) = ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueView {
            reff: issue.clone(),
        },
    ) else {
        panic!("expected Issue");
    };
    assert!(view.attachments.is_empty());

    // ---- project delete (CUSTOM-10): refuse-if-referenced, then delete. ----
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Doomed".into(),
            key: "dmd".into(),
            color: None,
        },
    );
    let doomed_issue = new_issue(&client, &home, "dmd", "the last issue");
    let resp = issue_req(
        &client,
        &home,
        issues_app::IssuesRequest::ProjectDelete {
            project: "dmd".into(),
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Error { message, .. } if message.contains("still has issues")),
        "non-empty delete must refuse: {resp:?}"
    );
    // Even a TOMBSTONED issue keeps the project undeletable.
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueDelete {
            reff: doomed_issue.clone(),
        },
    );
    let resp = issue_req(
        &client,
        &home,
        issues_app::IssuesRequest::ProjectDelete {
            project: "dmd".into(),
        },
    );
    assert!(
        matches!(&resp, IssueResponse::Error { message, .. } if message.contains("still has issues")),
        "tombstoned issues still block: {resp:?}"
    );
    // Move it out; the emptied project deletes, and its initiative membership
    // is cleaned in the same transaction.
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueRestore {
            reff: doomed_issue.clone(),
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::InitiativeSet {
            initiative: Some("Q3 platform".into()),
            name: None,
            description: None,
            owner: None,
            health: None,
            target: None,
            add_projects: vec!["dmd".into()],
            remove_projects: vec![],
            remove: false,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueMove {
            reff: doomed_issue.clone(),
            project: Some("eng".into()),
            pos: None,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::ProjectDelete {
            project: "dmd".into(),
        },
    );
    let IssueResponse::Projects { projects } =
        ok(&client, &home, issues_app::IssuesRequest::ProjectList)
    else {
        panic!("expected Projects");
    };
    assert!(
        !projects.iter().any(|p| p.key == "DMD"),
        "the emptied project is gone"
    );
    let IssueResponse::Initiatives { initiatives } =
        ok(&client, &home, issues_app::IssuesRequest::InitiativeList)
    else {
        panic!("expected Initiatives");
    };
    assert_eq!(
        initiatives[0].projects,
        vec!["ENG".to_string()],
        "the initiative dropped the deleted project"
    );
    // The moved issue survived under its new project.
    let IssueResponse::List { rows } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::List {
            project: Some("eng".into()),
            filter: issues_app::Filter {
                all: true,
                ..Default::default()
            },
        },
    ) else {
        panic!("expected List");
    };
    assert!(rows.iter().any(|r| r.title == "the last issue"));

    let _ = req(&client, &home, Request::Stop);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// INBOX-9: a follower receives another actor's comment activity in their
/// inbox without being assigned.
#[test]
fn a_follower_hears_about_an_issue_they_are_not_assigned() {
    let net = MemNet::new();
    let founder_home = temp_home("f");
    lait::orbital::form_space(&founder_home, &FOUNDER_SEED, "Follow Space").unwrap();
    let founder_handle = spawn_daemon(founder_home.clone(), FOUNDER_SEED, net.clone());
    let client = tokio::runtime::Runtime::new().unwrap();
    wait_online(&client, &founder_home);
    ok(
        &client,
        &founder_home,
        issues_app::IssuesRequest::ProjectNew {
            name: "Core".into(),
            key: "core".into(),
            color: None,
        },
    );
    let issue = new_issue(&client, &founder_home, "core", "watched work");

    let Response::Ref { reff: invite } = req(
        &client,
        &founder_home,
        Request::Invite {
            role: None,
            reusable: false,
            ttl_hours: Some(24),
        },
    ) else {
        panic!("expected an invite");
    };
    let member_home = temp_home("m");
    lait::orbital::enter_space(&member_home, &MEMBER_SEED, &invite).unwrap();
    let member_handle = spawn_daemon(member_home.clone(), MEMBER_SEED, net.clone());
    wait_online(&client, &member_home);
    let founder_device = lait::crypto::device_from_seed(&FOUNDER_SEED).to_string();
    assert!(
        poll_until(Duration::from_secs(25), || {
            req(
                &client,
                &member_home,
                Request::Connect {
                    ticket: founder_device.clone(),
                },
            );
            match req(&client, &member_home, Request::Status) {
                Response::Status(info) if info.membership == "member" => Some(()),
                _ => None,
            }
        })
        .is_some(),
        "member never admitted"
    );

    // The member follows the founder's issue (never assigned to it) …
    assert!(
        poll_until(Duration::from_secs(10), || {
            matches!(
                issue_req(
                    &client,
                    &member_home,
                    issues_app::IssuesRequest::Follow {
                        reff: issue.clone(),
                        on: true,
                    },
                ),
                IssueResponse::Ref { .. }
            )
            .then_some(())
        })
        .is_some(),
        "follow never succeeded"
    );
    // … the founder comments …
    ok(
        &client,
        &founder_home,
        issues_app::IssuesRequest::Comment {
            reply_to: None,
            reff: issue.clone(),
            body: "news for the followers".into(),
        },
    );
    // … and the comment surfaces in the member's inbox without assignment
    // (converging ambient over the beacon plane; no manual Connect).
    assert!(
        poll_until(Duration::from_secs(15), || {
            match issue_req(
                &client,
                &member_home,
                issues_app::IssuesRequest::Inbox { watermark: 0 },
            ) {
                IssueResponse::Inbox { entries, .. }
                    if entries
                        .iter()
                        .any(|e| e.kind == "comment" && e.detail == "news for the followers") =>
                {
                    Some(())
                }
                _ => None,
            }
        })
        .is_some(),
        "the followed issue's comment never reached the follower's inbox"
    );

    let _ = req(&client, &member_home, Request::Stop);
    let _ = req(&client, &founder_home, Request::Stop);
    let _ = member_handle.join();
    let _ = founder_handle.join();
    let _ = std::fs::remove_dir_all(&founder_home);
    let _ = std::fs::remove_dir_all(&member_home);
}
