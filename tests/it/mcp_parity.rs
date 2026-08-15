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
use lait::mcp::{declared_tool_names, shell_tool_names, REQUIRED_SHELL_COMMANDS};

fn tool_error_text(reply: &serde_json::Value) -> String {
    if let Some(message) = reply["result"]["structuredContent"]["message"].as_str() {
        return message.to_string();
    }
    reply["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every shell command an agent must drive is on the shell router. World
/// verbs are the package's to cover (`every_protocol_command_is_reachable_through_a_tool`).
#[test]
fn every_required_shell_command_is_on_the_shell() {
    let names = shell_tool_names();
    for cmd in REQUIRED_SHELL_COMMANDS {
        assert!(
            names.iter().any(|name| name == cmd),
            "shell command `{cmd}` has no MCP tool — the agent surface drifted \
             from the Mechanics surface"
        );
    }
}

/// Onboarding/transport tools an agent needs but that live outside membership.
/// Pinned here so removing, say, the `doctor` tool — the guided-join
/// verifier's agent surface — fails the build instead of silently dropping a
/// channel.
#[test]
fn onboarding_and_transport_tools_stay_wired() {
    let names = shell_tool_names();
    for tool in ["status", "doctor", "who", "my_id", "join_room"] {
        assert!(
            names.iter().any(|name| name == tool),
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
    let declared: std::collections::BTreeSet<String> = declared_tool_names(None)
        .expect("sole World pin")
        .into_iter()
        .collect();

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

/// The declared surface has no duplicates (a copy-paste / merge guard).
#[test]
fn mcp_tool_names_are_unique() {
    let names = declared_tool_names(None).expect("sole World pin");
    let mut seen = std::collections::HashSet::new();
    for name in &names {
        assert!(
            seen.insert(name.as_str()),
            "duplicate MCP tool name: {name}"
        );
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
        Response::Check {
            reff: "iss_3f9ab2c".into(),
            run: "71".repeat(16),
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
            checks: vec![],
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

/// A generic Work lookup failure remains a caller-actionable not-found across
/// the real stdio server. It must not be collapsed through the package adapter
/// into JSON-RPC internal_error("invalid client operation").
#[test]
fn missing_work_run_is_not_an_internal_mcp_error() {
    use crate::head::{temp_root, Head, Mcp};

    let root = temp_root("work-missing");
    let config = root.join("cfg");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "PROJ",
        "nick": "Probe",
    }));
    assert_eq!(status, 200, "found: {founded}");

    let mut mcp = Mcp::start(&config, &home, None);
    let reply = mcp.call_raw(
        "issues_work",
        serde_json::json!({
            "action": "inspect",
            "run": "71".repeat(16),
        }),
    );
    assert!(
        reply.get("error").is_none(),
        "a missing Run is a tool error, not a protocol error: {reply}"
    );
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let message = tool_error_text(&reply);
    assert!(message.contains("no Runtime Run matches"), "{reply}");
    assert!(!message.contains("invalid client operation"), "{reply}");

    mcp.stop();
    head.stop();
    std::fs::remove_dir_all(&root).ok();
}

/// Package-owned exact-id parsing reports the repairable diagnostic over the
/// dynamic package-tool route instead of the opaque Failure display string.
#[test]
fn package_tool_argument_diagnostic_survives_stdio() {
    use crate::head::{temp_root, Head, Mcp};

    let root = temp_root("work-argument");
    let config = root.join("cfg");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "PROJ",
        "nick": "Probe",
    }));
    assert_eq!(status, 200, "found: {founded}");

    let mut mcp = Mcp::start(&config, &home, None);
    let reply = mcp.call_raw(
        "issues_work",
        serde_json::json!({"action": "inspect", "run": "short"}),
    );
    assert!(
        reply.get("error").is_none(),
        "malformed arguments are a tool error, not a protocol error: {reply}"
    );
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let message = tool_error_text(&reply);
    assert!(
        message.contains("expected 32 lowercase hex characters"),
        "{reply}"
    );
    assert!(!message.contains("invalid client operation"), "{reply}");

    mcp.stop();
    head.stop();
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_stdio_head_answers_discover_and_serves_planning_tools() {
    use crate::head::{temp_root, Head, Mcp};

    let root = temp_root("discover");
    let config = root.join("cfg");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "PROJ",
        "nick": "Probe",
    }));
    assert_eq!(status, 200, "found: {founded}");

    let mut mcp = Mcp::start(&config, &home, None);
    let discover = mcp.request(
        "server/discover",
        serde_json::json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }),
    );
    assert!(discover.get("error").is_none(), "{discover}");
    let versions = discover["result"]["supportedVersions"]
        .as_array()
        .expect("supportedVersions");
    let versions: Vec<&str> = versions
        .iter()
        .filter_map(|version| version.as_str())
        .collect();
    assert!(
        versions.contains(&"2026-07-28") && versions.contains(&"2024-11-05"),
        "{versions:?}"
    );

    let listed = mcp.request("tools/list", serde_json::json!({}));
    assert!(listed.get("error").is_none(), "{listed}");
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in [
        "issues_milestone_set",
        "issues_milestone_list",
        "issues_issue_milestone",
        "issues_team_set",
        "issues_team_list",
        "issues_project_edit",
        "issues_spec_state",
        "issues_baseline_new",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    assert!(
        !names.iter().any(|name| name.contains("geometry")),
        "geometry is compiled Blueprint output, not an agent verb: {names:?}"
    );

    let projects = mcp.call("issues_project_list", serde_json::json!({}));
    assert!(
        projects.get("kind").is_some() || projects.get("projects").is_some(),
        "{projects}"
    );

    mcp.stop();
    head.stop();
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_baseline_of_an_unissued_spec_names_the_lifecycle() {
    use crate::head::{temp_root, Head, Mcp};

    let root = temp_root("baseline-lifecycle");
    let config = root.join("cfg");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let head = Head::start(&config, Some(&home));
    let (status, founded) = head.host(serde_json::json!({
        "cmd": "host_space_found",
        "home": home.display().to_string(),
        "name": "PROJ",
        "nick": "Probe",
    }));
    assert_eq!(status, 200, "found: {founded}");

    let mut mcp = Mcp::start(&config, &home, None);
    let project = mcp.call(
        "issues_project_new",
        serde_json::json!({ "name": "Engineering", "key": "ENG" }),
    );
    assert!(project.get("error").is_none(), "{project}");
    let spec = mcp.call(
        "issues_spec_new",
        serde_json::json!({
            "project": "ENG",
            "kind": "plan",
            "title": "The tree as it stands",
        }),
    );
    let spec_id = spec["spec"]["spec"]
        .as_str()
        .or_else(|| spec["spec"].as_str())
        .unwrap_or_else(|| panic!("spec id in {spec}"))
        .to_string();
    let revision = spec["spec"]["revision"]
        .as_str()
        .or_else(|| spec["revision"].as_str())
        .unwrap_or_else(|| panic!("revision in {spec}"))
        .to_string();

    let reply = mcp.call_raw(
        "issues_baseline_new",
        serde_json::json!({
            "project": "ENG",
            "name": "E0",
            "members": [{ "spec": spec_id, "revision": revision }],
        }),
    );
    assert!(
        reply.get("error").is_none(),
        "an unissued member is a tool error, not a protocol error: {reply}"
    );
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let message = tool_error_text(&reply);
    assert!(
        message.contains("not an issued Spec revision") && message.contains("spec_state"),
        "{reply}"
    );

    mcp.stop();
    head.stop();
    std::fs::remove_dir_all(&root).ok();
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
        checks: vec![],
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
