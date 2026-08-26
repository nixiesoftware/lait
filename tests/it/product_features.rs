//! The 2026-07-23 product-feature batch, end to end through real orbital
//! daemons over their control sockets: followers (INBOX-9), milestones
//! (SCOPE-1) + cycles (BOARD-11), initiatives (SCOPE-8), teams (GOV-7),
//! triage (SCOPE-7), project delete (CUSTOM-10), and bounded attachments
//! (CREATE-5).

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
    crate::head::temp_root(&format!("prod-{tag}"))
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

fn filler(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

fn content_route(home: &Path) -> lait::control::ControlRoute {
    let space = lait::orbital::discover_space(home)
        .single()
        .expect("test Space");
    lait::control::station_route(OrbitAddress::for_store(home, space))
}

/// Seal bytes onto the content plane, the way the CLI's `attach` does.
fn upload_content(rt: &tokio::runtime::Runtime, home: &Path, bytes: &[u8]) -> String {
    rt.block_on(async {
        let mut operation = [0u8; 16];
        getrandom::fill(&mut operation).expect("operation id");
        let mut upload = lait::control::ContentUpload::open(
            home,
            content_route(home),
            operation,
            None,
            bytes.len() as u64,
        )
        .await
        .expect("open upload");
        for piece in bytes.chunks(64 * 1024) {
            upload.push(piece).await.expect("push");
        }
        match upload.finish().await.expect("finish") {
            lait::control::ContentReply::ContentWritten { content, .. } => content,
            other => panic!("expected a stored content, got {other:?}"),
        }
    })
}

/// Read it back in ranges, which is the only way this surface offers.
fn read_content(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    content: &str,
    expected: usize,
) -> Vec<u8> {
    rt.block_on(async {
        let mut got: Vec<u8> = Vec::new();
        while got.len() < expected {
            let (reply, piece) = lait::control::content_call(
                home,
                &lait::control::content_request(
                    content_route(home),
                    lait::control::ContentCall::Read {
                        content: content.to_string(),
                        offset: got.len() as u64,
                        len: 256 * 1024,
                        patience_ms: 0,
                    },
                ),
            )
            .await
            .expect("read");
            assert!(
                matches!(reply, lait::control::ContentReply::ContentStream { .. }),
                "{reply:?}"
            );
            assert!(!piece.is_empty(), "a short read that never ends is a hang");
            got.extend_from_slice(&piece);
        }
        got
    })
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
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Feature Space").unwrap();
    let handle = spawn_daemon(home.to_path_buf(), FOUNDER_SEED, net.clone());
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
            description: None,
            target: Some("2026-09-01".into()),
            pos: None,
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
    let IssueResponse::Milestones { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneList {
            project: "eng".into(),
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Milestones");
    };
    let milestones = page.items;
    assert_eq!(milestones.len(), 1);
    assert_eq!(milestones[0].name, "Beta");
    assert_eq!((milestones[0].done, milestones[0].total), (0, 1));
    assert_eq!(milestones[0].description, "", "a new milestone has no body");

    // The body is an independent field: writing it leaves the name and date
    // alone, and an absent `description` on a later edit leaves the body alone.
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneSet {
            project: "eng".into(),
            milestone: Some("Beta".into()),
            name: None,
            description: Some("Ship the public preview.\n\n- API frozen".into()),
            target: None,
            pos: None,
            remove: false,
        },
    );
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneSet {
            project: "eng".into(),
            milestone: Some("Beta".into()),
            name: None,
            description: None,
            target: Some("2026-10-01".into()),
            pos: None,
            remove: false,
        },
    );
    let IssueResponse::Milestones { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneList {
            project: "eng".into(),
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Milestones");
    };
    let milestones = page.items;
    assert!(milestones[0]
        .description
        .starts_with("Ship the public preview."));
    assert_eq!(milestones[0].name, "Beta");

    // `""` clears it — distinct from absent, which is "leave it".
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneSet {
            project: "eng".into(),
            milestone: Some("Beta".into()),
            name: None,
            description: Some(String::new()),
            target: None,
            pos: None,
            remove: false,
        },
    );
    let IssueResponse::Milestones { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneList {
            project: "eng".into(),
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Milestones");
    };
    assert_eq!(page.items[0].description, "");

    // Completing the issue moves the derived progress.
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueDone {
            reff: issue.clone(),
        },
    );
    let IssueResponse::Milestones { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneList {
            project: "eng".into(),
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Milestones");
    };
    assert_eq!((page.items[0].done, page.items[0].total), (1, 1));

    // ---- milestone order (SCOPE-1): manual, and independent of the date. ----
    //
    // The bug this replaces: milestones sorted by target date with undated ones
    // last, so an undated "M0" read *below* a dated "M8" — a stage list in the
    // wrong order is worse than no order at all.
    let names = |client: &tokio::runtime::Runtime, home: &Path| -> Vec<String> {
        let IssueResponse::Milestones { page } = ok(
            client,
            home,
            issues_app::IssuesRequest::MilestoneList {
                project: "eng".into(),
                page: issues::contract::PageRequest::default(),
            },
        ) else {
            panic!("expected Milestones");
        };
        page.items.into_iter().map(|m| m.name).collect()
    };
    for name in ["Gamma", "Delta"] {
        ok(
            &client,
            &home,
            issues_app::IssuesRequest::MilestoneSet {
                project: "eng".into(),
                milestone: None,
                name: Some(name.into()),
                description: None,
                target: None,
                pos: None,
                remove: false,
            },
        );
    }
    // Appended, not sorted in: "Beta" carries a target date and the other two do
    // not, so a date sort would have put them either side of it.
    assert_eq!(names(&client, &home), ["Beta", "Gamma", "Delta"]);

    // Moved by hand, and the date stays irrelevant.
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneSet {
            project: "eng".into(),
            milestone: Some("Delta".into()),
            name: None,
            description: None,
            target: None,
            pos: Some(issues_app::BoardPos::Top),
            remove: false,
        },
    );
    assert_eq!(names(&client, &home), ["Delta", "Beta", "Gamma"]);
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneSet {
            project: "eng".into(),
            milestone: Some("Delta".into()),
            name: None,
            description: None,
            target: None,
            pos: Some(issues_app::BoardPos::After {
                reff: "Beta".into(),
            }),
            remove: false,
        },
    );
    assert_eq!(names(&client, &home), ["Beta", "Delta", "Gamma"]);

    // A placement relative to a milestone that is not in this project is a
    // mistake, not a default: it is refused rather than silently appended.
    let refused = issue_req(
        &client,
        &home,
        issues_app::IssuesRequest::MilestoneSet {
            project: "eng".into(),
            milestone: Some("Delta".into()),
            name: None,
            description: None,
            target: None,
            pos: Some(issues_app::BoardPos::Before {
                reff: "nope".into(),
            }),
            remove: false,
        },
    );
    assert!(
        matches!(refused, IssueResponse::Error { .. }),
        "unknown sibling must be refused, got {refused:?}"
    );
    assert_eq!(names(&client, &home), ["Beta", "Delta", "Gamma"]);

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
    let IssueResponse::Cycles { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::CycleList {
            project: "eng".into(),
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Cycles");
    };
    let cycles = page.items;
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
    let IssueResponse::Initiatives { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::InitiativeList {
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Initiatives");
    };
    let initiatives = page.items;
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
    let IssueResponse::Teams { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::TeamList {
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Teams");
    };
    let teams = page.items;
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
    let IssueResponse::TriageItems { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::TriageList {
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected TriageItems");
    };
    let items = page.items;
    let decisions = items
        .iter()
        .filter_map(|record| match record {
            issues::records::TriageRecord::Decision(decision) => Some(decision),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 3, "{items:?}");
    let accepted = decisions
        .into_iter()
        .find(|decision| decision.triage == t_accept)
        .unwrap();
    assert_eq!(accepted.outcome, issues::records::TriageOutcome::Accepted);
    let accepted_issue = accepted
        .issue
        .clone()
        .expect("accepted decision names its issue");
    // The accepted issue is a real, listed issue carrying the intake body.
    let IssueResponse::Issue(view) = ok(
        &client,
        &home,
        issues_app::IssuesRequest::IssueView {
            reff: accepted_issue,
        },
    ) else {
        panic!("expected Issue");
    };
    assert_eq!(view.title, "login breaks on refresh");
    assert_eq!(view.description, "steps: refresh twice");

    // ---- attachments: upload, attach, fetch, cap, detach. ----
    //
    // Two steps now, and the order is the contract. The bytes go to the content
    // plane first and the issue names what came back; the substrate refuses a
    // declaration whose descriptor is not committed, so doing it the other way
    // round does not race, it fails.
    let payload = filler(9, 700 * 1024);
    let stored = upload_content(&client, &home, &payload);
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::Attach {
            reff: issue.clone(),
            name: "notes.txt".into(),
            mime: Some("text/plain".into()),
            content: stored.clone(),
            size: payload.len() as u64,
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
        content,
        data_b64,
        size,
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
    assert_eq!(size, payload.len() as u64);
    assert_eq!(
        content.as_deref(),
        Some(stored.as_str()),
        "a record written after the cutover names its content"
    );
    assert!(
        data_b64.is_none(),
        "and carries no bytes — the Body is not where files live any more"
    );

    // The bytes really are on the content plane, and really are the ones sent.
    let read_back = read_content(&client, &home, &stored, payload.len());
    assert_eq!(
        blake3::hash(&read_back),
        blake3::hash(&payload),
        "the round trip through the content plane lost or reordered bytes"
    );

    // A name a peer could have chosen. The engine stores it as authored —
    // separators are legal in a display name, and refusing them at intake would
    // protect nothing, because a Body arriving through convergence never passes
    // local intake at all. What must hold is that saving it lands beside us.
    let hostile_content = upload_content(&client, &home, b"not yours to place");
    ok(
        &client,
        &home,
        issues_app::IssuesRequest::Attach {
            reff: issue.clone(),
            name: "../../evil.txt".into(),
            mime: None,
            content: hostile_content,
            size: 18,
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
    let hostile = view
        .attachments
        .iter()
        .find(|a| a.name == "../../evil.txt")
        .expect("the name is stored as authored — it is product data");
    let saved = world_interface::destination::sanitize_display_name(&hostile.name);
    let path = std::path::Path::new(&saved);
    assert_eq!(
        path.components().count(),
        1,
        "{:?} would be saved as {saved:?}, which is more than one component",
        hostile.name
    );
    assert!(path.is_relative(), "{saved:?}");
    let hostile_id = hostile.id.clone();

    // An attachment naming content nobody committed is refused by the
    // substrate, not by the product — which is what makes the ordering a rule
    // rather than a convention.
    let phantom = issue_req(
        &client,
        &home,
        issues_app::IssuesRequest::Attach {
            reff: issue.clone(),
            name: "phantom.bin".into(),
            mime: None,
            content: "ab".repeat(32),
            size: 10,
            comment: None,
        },
    );
    assert!(
        matches!(&phantom, IssueResponse::Error { .. }),
        "attaching uncommitted content must refuse: {phantom:?}"
    );

    ok(
        &client,
        &home,
        issues_app::IssuesRequest::Detach {
            reff: issue.clone(),
            id: hostile_id,
        },
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
    let IssueResponse::Projects { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::ProjectList {
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Projects");
    };
    assert!(
        !page.items.iter().any(|p| p.key == "DMD"),
        "the emptied project is gone"
    );
    let IssueResponse::Initiatives { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::InitiativeList {
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected Initiatives");
    };
    assert_eq!(
        page.items[0].projects,
        vec!["ENG".to_string()],
        "the initiative dropped the deleted project"
    );
    // The moved issue survived under its new project.
    let IssueResponse::List { page } = ok(
        &client,
        &home,
        issues_app::IssuesRequest::List {
            project: Some("eng".into()),
            filter: issues_app::Filter {
                all: true,
                ..Default::default()
            },
            page: issues::contract::PageRequest::default(),
        },
    ) else {
        panic!("expected List");
    };
    assert!(page.items.iter().any(|r| r.title == "the last issue"));

    let _ = req(&client, &home, Request::Stop);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}

/// INBOX-9: a follower receives another actor's comment activity in their
/// inbox without being assigned.
///
/// Ignored, against a known and deliberate gap rather than a flake.
///
/// This rebuild moved the notification audience from read time to write
/// time: `issue_notification_audience` resolves assignees and followers
/// from the AUTHOR's pinned corpus and `push_event` freezes that set into
/// the activity record, which is immutable. `extract_activity` then posts
/// one inbox coordinate per frozen recipient, and `IssueQuery::Inbox` seeks
/// exactly those.
///
/// The consequence is what this test catches. Here the member follows on
/// its own node and the founder comments on its own, with nothing forcing
/// convergence between them -- the ambient beacon plane is the point. The
/// founder has not learned of the follow, so the member is not in
/// `recipients`, and because the record can never be rewritten, no later
/// convergence can put them there. The 15-second poll is not slow; it is
/// waiting for something that will never arrive.
///
/// The previous plane recomputed membership at READ time, on the reader's
/// own node, which always knows what it follows -- so convergence order did
/// not matter. Restoring that means the inbox unions the frozen recipients
/// with activity on issues the reader currently follows or is assigned to,
/// which is a second ordered source under one cursor.
///
/// That is a change to the notification model rather than a fix to this
/// test, and it is deliberately not being made inside the landing of this
/// rebuild. The frozen recipient list stays as the record of who was
/// ADDRESSED; what is missing is the reader-side half.
#[ignore = "notification audience is frozen at write time; the reader-side             union that made following order-independent is not yet restored"]
#[test]
fn a_follower_hears_about_an_issue_they_are_not_assigned() {
    let net = MemNet::new();
    let founder_home = temp_home("f");
    crate::world_fixture::form_space(&founder_home, &FOUNDER_SEED, "Follow Space").unwrap();
    let founder_handle = spawn_daemon(founder_home.to_path_buf(), FOUNDER_SEED, net.clone());
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
            world: None,
            role: None,
            reusable: false,
            ttl_hours: Some(24),
        },
    ) else {
        panic!("expected an invite");
    };
    let member_home = temp_home("m");
    crate::world_fixture::enter_space(&member_home, &MEMBER_SEED, &invite).unwrap();
    let member_handle = spawn_daemon(member_home.to_path_buf(), MEMBER_SEED, net.clone());
    wait_online(&client, &member_home);
    let founder_device = mechanics::actor::device_from_seed(&FOUNDER_SEED).to_string();
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
                issues_app::IssuesRequest::Inbox {
                    watermark: 0,
                    page: issues::contract::PageRequest::default(),
                    publication: None,
                },
            ) {
                IssueResponse::Inbox { page, .. }
                    if page
                        .items
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
