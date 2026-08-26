//! `issues_policy_designer` — role/access/workflow authoring parity over the
//! real process-backed StationHost control surface: built-in and custom roles,
//! revision heads and expected-revision refusal, tombstones, exact-expansion
//! assignment/revoke through Mechanics, deterministic workflow replacement,
//! and gate enforcement — a transition whose template grants no admin
//! override denies an admin until the matching role is assigned.

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
use lait::control::{request, AssignmentSpec, ControlRoute, Request, Response};

const FOUNDER_SEED: [u8; 32] = [111u8; 32];

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
fn temp_home() -> crate::head::TempRoot {
    crate::head::temp_root("policy")
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

fn grant_role(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    actor: String,
    role: String,
    project: Option<String>,
) -> Response {
    match issue_req(
        rt,
        home,
        issues_app::IssuesRequest::AccessPlan { role, project },
    ) {
        IssueResponse::AccessPlan { assignments } => req(
            rt,
            home,
            Request::AssignmentGrant {
                actor,
                assignments: assignments
                    .into_iter()
                    .map(|assignment| AssignmentSpec {
                        world: assignment.world,
                        capability: assignment.capability,
                        resource: assignment.resource,
                    })
                    .collect(),
            },
        ),
        IssueResponse::Error { message, .. } => Response::err(message),
        other => Response::err(format!("unexpected access plan: {other:?}")),
    }
}

/// The JSON body of a read, however the surface hands it back.
///
/// Listing roles answers with a typed `Roles { page }` now rather than JSON
/// inside a `Text`; the page serializes to the same shape these assertions
/// navigate, so the only thing that changed is where the value comes from.
/// `RoleShow` and `WorkflowShow` still answer with `Text`.
fn text_of(resp: IssueResponse) -> serde_json::Value {
    match resp {
        IssueResponse::Text { text } => serde_json::from_str(&text).expect("json text"),
        IssueResponse::Roles { page } => serde_json::to_value(page).expect("roles page json"),
        other => panic!("expected a read body, got {other:?}"),
    }
}

fn ok_msg(resp: &IssueResponse) -> &str {
    match resp {
        IssueResponse::Ok { message } => message.as_deref().unwrap_or(""),
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn err_msg(resp: &IssueResponse) -> &str {
    match resp {
        IssueResponse::Error { message, .. } => message,
        other => panic!("expected Error, got {other:?}"),
    }
}

fn write_identity(home: &Path, seed: &[u8; 32]) {
    std::fs::write(
        home.join("secret.key"),
        data_encoding::HEXLOWER.encode(seed),
    )
    .unwrap();
}

#[test]
fn role_access_and_workflow_authoring_round_trip_over_the_daemon() {
    let home = temp_home();
    let net = MemNet::new();
    std::fs::create_dir_all(&home).unwrap();
    write_identity(&home, &FOUNDER_SEED);
    crate::world_fixture::form_space(&home, &FOUNDER_SEED, "Policy Space").unwrap();

    let daemon_home = home.to_path_buf();
    let daemon_net = net.clone();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            if let Err(e) =
                run_station_process_with(daemon_home, FOUNDER_SEED, &MemFactory(daemon_net)).await
            {
                eprintln!("DAEMON ERR: {e:#}");
            }
        });
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let online = {
        let start = Instant::now();
        loop {
            if matches!(
                req(&rt, &home, Request::Status),
                Response::Status(status) if !status.counts_unavailable
            ) {
                break true;
            }
            if start.elapsed() > Duration::from_secs(20) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    assert!(online, "daemon online");

    // ---- built-ins are listed, immutable, and shown with revisions --------
    let roles = text_of(issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleList {
            page: issues::contract::PageRequest {
                limit: 100,
                cursor: None,
            },
        },
    ));
    let ids: Vec<&str> = roles
        .get("items")
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .map(|r| r["summary"]["role_id"].as_str().unwrap())
        .collect();
    for built_in in ["lait.administrator", "lait.contributor", "lait.viewer"] {
        assert!(ids.contains(&built_in), "{built_in} listed");
    }
    let viewer = text_of(issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleShow {
            role: "lait.viewer".into(),
        },
    ));
    assert_eq!(viewer["summary"]["built_in"], true);
    let resp = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleEdit {
            role: "lait.viewer".into(),
            expect_revision: viewer["revision"]["revision_id"]
                .as_str()
                .unwrap()
                .to_string(),
            name: Some("Weakened".into()),
            description: None,
            capabilities: None,
        },
    );
    assert!(
        err_msg(&resp).contains("invalid"),
        "built-ins are immutable: {resp:?}"
    );

    // ---- custom role lifecycle: create → edit (exact head) → assign -------
    let project_key = match issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::ProjectList {
            page: issues::contract::PageRequest::default(),
        },
    ) {
        IssueResponse::Projects { page } => page.items.first().unwrap().key.clone(),
        other => panic!("{other:?}"),
    };
    let created = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleCreate {
            name: "Reviewer".into(),
            description: Some("Can pass reviews".into()),
            project: Some(project_key.clone()),
            capabilities: vec!["workflow.transition.ship".into()],
        },
    );
    let role_id = ok_msg(&created)
        .rsplit(' ')
        .next()
        .unwrap()
        .trim()
        .to_string();
    assert!(role_id.starts_with("role_"), "{role_id}");
    let shown = text_of(issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleShow {
            role: role_id.clone(),
        },
    ));
    let head = shown["revision"]["revision_id"]
        .as_str()
        .unwrap()
        .to_string();

    // A stale expected revision refuses; the exact head succeeds.
    let stale = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleEdit {
            role: role_id.clone(),
            expect_revision: "ab".repeat(32),
            name: Some("Renamed".into()),
            description: None,
            capabilities: None,
        },
    );
    assert!(matches!(stale, IssueResponse::Error { .. }), "{stale:?}");
    let edited = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleEdit {
            role: role_id.clone(),
            expect_revision: head.clone(),
            name: Some("Reviewer+".into()),
            description: None,
            capabilities: None,
        },
    );
    assert!(matches!(edited, IssueResponse::Ok { .. }), "{edited:?}");
    let after = text_of(issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleShow {
            role: role_id.clone(),
        },
    ));
    let head2 = after["revision"]["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(head, head2, "an edit advances the head");
    assert_eq!(after["revision"]["body"]["name"], "Reviewer+");

    // An unregistered capability refuses at creation.
    let bogus = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleCreate {
            name: "Bogus".into(),
            description: None,
            project: None,
            capabilities: vec!["nuke.everything".into()],
        },
    );
    assert!(matches!(bogus, IssueResponse::Error { .. }), "{bogus:?}");

    // ---- workflow: replace the default with a gated edge ------------------
    let wf = text_of(issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::WorkflowShow {
            project: project_key.clone(),
        },
    ));
    let wf_head = wf["revision"]["revision_id"].as_str().unwrap().to_string();
    let project_id = wf["project_id"].as_str().unwrap().to_string();
    let mut body = wf["revision"]["body"].clone();
    // Remove the backlog→done edge entirely, and strip the admin/contributor
    // override from backlog→in_progress: only the qualified transition
    // capability may take it.
    {
        let transitions = body["transitions"].as_array_mut().unwrap();
        transitions.retain(|t| t["transition_id"] != "default.backlog.done");
        for t in transitions.iter_mut() {
            if t["transition_id"] == "default.backlog.in_progress" {
                t["demand_template"] = serde_json::json!({
                    "op": "require",
                    "capability": "workflow.transition.ship",
                    "resource": {"kind": "project"},
                });
            }
        }
    }
    // An invalid body refuses before any commit.
    let mut broken = body.clone();
    broken["transitions"].as_array_mut().unwrap()[0]["destination_state_id"] =
        serde_json::json!("nowhere");
    let invalid = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::WorkflowValidate {
            body_json: broken.to_string(),
        },
    );
    assert!(
        matches!(invalid, IssueResponse::Error { .. }),
        "{invalid:?}"
    );
    let valid = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::WorkflowValidate {
            body_json: body.to_string(),
        },
    );
    assert!(matches!(valid, IssueResponse::Ok { .. }), "{valid:?}");
    let set = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::WorkflowSet {
            project: project_id.clone(),
            expect_heads: vec![wf_head],
            body_json: body.to_string(),
        },
    );
    assert!(matches!(set, IssueResponse::Ok { .. }), "{set:?}");

    // ---- gate enforcement: the removed edge is refused; the stripped edge
    // denies even the admin until the matching role is assigned -------------
    let filed = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::IssueNew {
            due: None,
            estimate: None,
            title: "Gated".into(),
            project: Some(project_id.clone()),
            project_hint: None,
            assignees: vec![],
            priority: None,
            labels: vec![],
            body: None,
        },
    );
    let reff = match &filed {
        IssueResponse::Ref { reff } => reff.clone(),
        other => panic!("{other:?}"),
    };
    // backlog → done: the edge does not exist in the replaced workflow.
    let no_edge = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::IssueEdit {
            due: None,
            estimate: None,
            reff: reff.clone(),
            title: None,
            status: Some("done".into()),
            priority: None,
            description: None,
        },
    );
    assert!(
        matches!(no_edge, IssueResponse::Error { .. }),
        "{no_edge:?}"
    );
    // backlog → in_progress: exists, but its template grants no admin
    // override — even the founder is denied until the role is assigned.
    let denied = issue_req(
        &rt,
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
    );
    assert!(
        err_msg(&denied).contains("view-only") || err_msg(&denied).contains("membership"),
        "the deterministic gate denies without the transition capability: {denied:?}"
    );

    // Assign the custom role (Project-scoped) to the founder, then the same
    // transition authorizes — role authoring + Mechanics assignment + gate.
    let me = match req(&rt, &home, Request::Members) {
        Response::Members { members } => members.into_iter().find(|m| m.me).unwrap().key,
        other => panic!("{other:?}"),
    };
    let granted = grant_role(
        &rt,
        &home,
        me.clone(),
        role_id.clone(),
        Some(project_id.clone()),
    );
    assert!(matches!(granted, Response::Ok { .. }), "{granted:?}");
    let rows = match req(
        &rt,
        &home,
        Request::AssignmentList {
            actor: Some(me.clone()),
        },
    ) {
        Response::Assignments { rows } => rows,
        other => panic!("{other:?}"),
    };
    let grant = rows
        .iter()
        .find(|r| r.capability == "workflow.transition.ship")
        .expect("the exact expansion landed");
    assert_eq!(grant.resource, vec![project_id.clone()]);
    let allowed = issue_req(
        &rt,
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
    );
    assert!(
        matches!(allowed, IssueResponse::Ref { .. }),
        "the assigned transition capability authorizes the gate: {allowed:?}"
    );

    // ---- revoke: the assignment disappears and the gate denies again ------
    let revoked = req(
        &rt,
        &home,
        Request::AssignmentRevoke {
            grant_id: grant.grant_id.clone(),
        },
    );
    assert!(matches!(revoked, Response::Ok { .. }), "{revoked:?}");
    let rows = match req(
        &rt,
        &home,
        Request::AssignmentList {
            actor: Some(me.clone()),
        },
    ) {
        Response::Assignments { rows } => rows,
        other => panic!("{other:?}"),
    };
    assert!(
        !rows
            .iter()
            .any(|r| r.capability == "workflow.transition.ship"),
        "revocation removed the assignment"
    );
    let denied_again = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::IssueEdit {
            due: None,
            estimate: None,
            reff: reff.clone(),
            title: None,
            status: Some("backlog".into()),
            priority: None,
            description: None,
        },
    );
    // in_progress → backlog keeps the default (admin-overridable) template, so
    // this still succeeds; the STRIPPED edge denies again after revocation.
    assert!(matches!(denied_again, IssueResponse::Ref { .. }));
    let stripped = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::IssueEdit {
            due: None,
            estimate: None,
            reff,
            title: None,
            status: Some("in_progress".into()),
            priority: None,
            description: None,
        },
    );
    assert!(
        matches!(stripped, IssueResponse::Error { .. }),
        "{stripped:?}"
    );

    // ---- tombstone: a deleted role no longer assigns ----------------------
    let deleted = issue_req(
        &rt,
        &home,
        issues_app::IssuesRequest::RoleDelete {
            role: role_id.clone(),
            expect_revision: head2,
        },
    );
    assert!(matches!(deleted, IssueResponse::Ok { .. }), "{deleted:?}");
    let refused = grant_role(&rt, &home, me, role_id, Some(project_id));
    assert!(
        matches!(&refused, Response::Error { message, .. } if message.contains("tombstoned")),
        "a tombstoned role assigns nothing: {refused:?}"
    );

    let _ = req(&rt, &home, Request::Stop);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&home);
}
