//! Guards parity between the versioned DTO contract and the MCP surface.
//!
//! The MCP tools return the **same** versioned control `Response` DTO the local
//! app's HTTP surface emits, so the agent and human heads never drift. These tests are
//! the "check" half of "generate/check, don't hand-maintain twice": they fail
//! the build gate if a replica `Request` is added without a corresponding MCP
//! tool, or if a `Response` DTO stops round-tripping (a silent contract break).

use issues::dto::{
    ActivityEvent, BoardColumn, BoardView, IssueView, Priority, ProjectDto, Row, WorkflowState,
    SCHEMA_VERSION,
};
use issues::ids::{DocId, ProjectId, SpaceId, SystemUlidSource};
use issues_app::IssuesResponse as Response;
use lait::mcp::{MCP_TOOL_NAMES, REQUIRED_TRACKER_COMMANDS};

/// Every replica command an agent must drive has exactly one MCP tool. Adding a
/// `Request` variant to the replica surface without wiring an MCP tool for it
/// fails here (the parity guard).
#[test]
fn every_replica_command_has_an_mcp_tool() {
    for cmd in REQUIRED_TRACKER_COMMANDS {
        assert!(
            MCP_TOOL_NAMES.contains(cmd),
            "replica command `{cmd}` has no MCP tool — the agent surface drifted \
             from the Layer-B command surface"
        );
    }
}

/// Onboarding/transport tools an agent needs but that live outside the replica
/// CRUD set (and so aren't covered by `REQUIRED_TRACKER_COMMANDS`). Pinned here so
/// removing, say, the `doctor` tool — the guided-join verifier's agent surface —
/// fails the build instead of silently dropping a channel.
#[test]
fn onboarding_and_transport_tools_stay_wired() {
    for tool in ["status", "doctor", "who", "my_id", "join_room"] {
        assert!(
            MCP_TOOL_NAMES.contains(&tool),
            "MCP tool `{tool}` is missing — an agent-facing channel regressed"
        );
    }
}

/// What the server actually **serves**, asked of the running binary over stdio.
///
/// Every other guard in this file compares one constant against another, which
/// is why the declared surface and the served surface were free to diverge in
/// silence: `#[tool_handler]` defaults to the macro-generated `Self::tool_router()`,
/// which knows only the shell half, so the router merged with the World packages
/// was built, stored, and never read. All 56 `issues_*` tools vanished from the
/// wire — the whole product surface — while every constant-level assertion here
/// stayed green. A list is not a wire; this test asks the wire.
#[test]
fn the_served_tool_list_matches_the_declared_surface() {
    use crate::head::{temp_root, Head, Mcp};

    let root = temp_root("served");
    let config = root.join("cfg");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");

    // A store first: `lait mcp` binds one Orbit before it can serve a tool.
    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "PROJ",
        "nick": "Probe",
    }));
    assert_eq!(status, 200, "found: {founded}");

    let mut mcp = Mcp::start(&config, &home, None);
    let served: std::collections::BTreeSet<String> = mcp.tool_names().into_iter().collect();
    mcp.stop();
    let declared: std::collections::BTreeSet<String> =
        MCP_TOOL_NAMES.iter().map(|s| (*s).to_string()).collect();

    let missing: Vec<_> = declared.difference(&served).collect();
    let extra: Vec<_> = served.difference(&declared).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the served MCP surface drifted from the declared one
           declared but not served: {missing:?}
  served but not declared: {extra:?}"
    );

    head.stop();
    std::fs::remove_dir_all(&root).ok();
}

/// The MCP tool-name list has no duplicates (a copy-paste guard).
#[test]
fn mcp_tool_names_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for name in MCP_TOOL_NAMES {
        assert!(seen.insert(*name), "duplicate MCP tool name: {name}");
    }
}

/// Every read `Response` DTO round-trips through JSON unchanged — the versioned
/// contract is stable and self-consistent (what `--json` and MCP both emit).
#[test]
fn response_dtos_round_trip() {
    let ulid = SystemUlidSource;
    let doc_id = DocId::mint(&ulid);
    let project = ProjectDto {
        id: ProjectId::mint(&ulid),
        name: "Engineering".into(),
        key: "ENG".into(),
        color: "blue".into(),
        description: String::new(),
        lead: String::new(),
        start_date: None,
        target_date: None,
        archived: false,
        team: String::new(),
    };
    let row = Row {
        due_date: None,
        estimate: None,
        label_names: Vec::new(),
        milestone: None,
        child_done: None,
        child_total: None,
        reff: "iss_3f9ab2c".into(),
        doc_id: doc_id.clone(),
        project_id: project.id.clone(),
        key_alias: Some("ENG-142".into()),
        title: "fix login race".into(),
        status: "in_progress".into(),
        priority: Priority::High,
        assignee_summary: "you +1".into(),
        assignees: vec![
            issues::ids::ActorId::from_incept_hash(&"a".repeat(64)),
            issues::ids::ActorId::from_incept_hash(&"b".repeat(64)),
        ],
        tombstone: false,
        provisional: false,
    };

    let samples = vec![
        Response::Ok {
            message: Some("done".into()),
        },
        Response::Ref {
            reff: "iss_3f9ab2c".into(),
        },
        Response::List {
            rows: vec![row.clone()],
        },
        Response::Board(Box::new(BoardView {
            schema_version: SCHEMA_VERSION,
            project: project.clone(),
            columns: vec![BoardColumn {
                state: WorkflowState {
                    id: "backlog".into(),
                    name: "Backlog".into(),
                    category: issues::dto::StatusCategory::Backlog,
                    color: "gray".into(),
                },
                rows: vec![row.clone()],
            }],
        })),
        Response::Issue(Box::new(IssueView {
            due_date: None,
            estimate: None,
            document_schema: 0,
            schema_version: SCHEMA_VERSION,
            reff: "iss_3f9ab2c".into(),
            doc_id: doc_id.clone(),
            space_id: SpaceId::mint(&ulid),
            project_id: project.id.clone(),
            project_key: Some("ENG".into()),
            key_alias: Some("ENG-142".into()),
            title: "fix login race".into(),
            description: "body".into(),
            status: "in_progress".into(),
            priority: Priority::High,
            assignees: vec![],
            labels: vec![],
            label_names: vec!["bug".into()],
            comments: vec![],
            created_by: issues::ids::ActorId::from_incept_hash(&"a".repeat(64)),
            created_at: 1000,
            followers: vec![],
            milestone: None,
            cycle: None,
            baseline: None,
            attachments: vec![],
            provisional: false,
            corrupt_records: vec![],
        })),
        Response::Activity {
            events: vec![ActivityEvent {
                seq: 1,
                cursor: String::new(),
                doc_id: Some(doc_id.clone()),
                reff: "iss_3f9ab2c".into(),
                kind: "edited".into(),
                changes: vec![],
                actor: None,
                actor_nick: "you".into(),
                text: String::new(),
                ts: 1000,
                collision: false,
            }],
            last: String::new(),
        },
        Response::not_found("no issue matches 'ENG-9x'"),
    ];

    for resp in samples {
        let json = serde_json::to_string(&resp).expect("serialize response");
        let back: Response = serde_json::from_str(&json).expect("deserialize response");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json, json2, "response DTO must round-trip: {json}");
        // The internal tag is `kind` (not `status`, which would collide with
        // IssueView.status) — assert it so a tag rename can't slip through.
        assert!(
            json.contains("\"kind\""),
            "response must be tagged by kind: {json}"
        );
    }
}

/// The `Issue` response carries its own `status` field alongside the `kind` tag
/// without a serde collision (the bug that motivated the `kind` tag).
#[test]
fn issue_response_status_field_survives_the_kind_tag() {
    let ulid = SystemUlidSource;
    let resp = Response::Issue(Box::new(IssueView {
        due_date: None,
        estimate: None,
        document_schema: 0,
        schema_version: SCHEMA_VERSION,
        reff: "iss_x".into(),
        doc_id: DocId::mint(&ulid),
        space_id: SpaceId::mint(&ulid),
        project_id: ProjectId::mint(&ulid),
        project_key: None,
        key_alias: None,
        title: "t".into(),
        description: String::new(),
        status: "done".into(),
        priority: Priority::None,
        assignees: vec![],
        labels: vec![],
        label_names: vec![],
        comments: vec![],
        created_by: issues::ids::ActorId::from_incept_hash(&"a".repeat(64)),
        created_at: 0,
        followers: vec![],
        milestone: None,
        cycle: None,
        baseline: None,
        attachments: vec![],
        provisional: false,
        corrupt_records: vec![],
    }));
    let json = serde_json::to_string(&resp).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["kind"], "issue");
    assert_eq!(
        v["status"], "done",
        "IssueView.status must survive next to the kind tag"
    );
}
