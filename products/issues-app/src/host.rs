//! Typed host capabilities requested by Issues client interfaces.
//!
//! These operations need facilities outside a World Session (the working tree,
//! local files, read watermarks, or Space authority). The product owns their
//! vocabulary and validation; the navigation shell only supplies the facility.

use serde_json::Value;
use world_interface::InterfaceError;

use crate::cli::{
    LOCAL_ACCESS, LOCAL_ATTACH, LOCAL_ATTACHMENT_GET, LOCAL_FOCUS, LOCAL_INBOX, LOCAL_NEW_START,
    LOCAL_WORK_STATE, LOCAL_WORLD_UPGRADE,
};
use crate::IssuesRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkStateAction {
    Start,
    Done,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessRequest {
    List {
        actor: Option<String>,
    },
    Grant {
        actor: String,
        role: String,
        project: Option<String>,
    },
    Revoke {
        grant_id: String,
    },
}

#[derive(Debug, Clone)]
pub enum IssuesHostRequest {
    Focus,
    NewStart(IssuesRequest),
    WorkState {
        action: WorkStateAction,
        reff: String,
        no_branch: bool,
    },
    Inbox {
        clear: bool,
    },
    WorldUpgrade,
    Access(AccessRequest),
    Attach {
        reff: String,
        file: String,
        comment: Option<String>,
    },
    AttachmentGet {
        reff: String,
        id: String,
        out: Option<String>,
    },
}

pub struct AccessGrantPlan {
    pub assignments: Vec<(
        mechanics::demand::PolicyCapability,
        mechanics::demand::PolicyResource,
    )>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessPlanError {
    NotFound(String),
    Invalid(String),
}

impl std::fmt::Display for AccessPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) | Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AccessPlanError {}

/// Decode one package-emitted local invocation at the product/host boundary.
pub fn decode(operation: &str, input: Value) -> Result<IssuesHostRequest, InterfaceError> {
    match operation {
        LOCAL_FOCUS => Ok(IssuesHostRequest::Focus),
        LOCAL_NEW_START => serde_json::from_value(input)
            .map(IssuesHostRequest::NewStart)
            .map_err(|error| InterfaceError::new(format!("decode Issues new/start: {error}"))),
        LOCAL_WORK_STATE => {
            let action = match required(&input, "action")?.as_str() {
                "start" => WorkStateAction::Start,
                "done" => WorkStateAction::Done,
                "stop" => WorkStateAction::Stop,
                other => {
                    return Err(InterfaceError::new(format!(
                        "unsupported Issues work-state action '{other}'"
                    )));
                }
            };
            Ok(IssuesHostRequest::WorkState {
                action,
                reff: required(&input, "reff")?,
                no_branch: input
                    .get("no_branch")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }
        LOCAL_INBOX => Ok(IssuesHostRequest::Inbox {
            clear: input.get("clear").and_then(Value::as_bool).unwrap_or(false),
        }),
        LOCAL_WORLD_UPGRADE => Ok(IssuesHostRequest::WorldUpgrade),
        LOCAL_ACCESS => {
            let action = input.get("action").and_then(Value::as_str).unwrap_or("ls");
            let request = match action {
                "access" | "ls" => AccessRequest::List {
                    actor: optional(&input, "actor"),
                },
                "grant" => AccessRequest::Grant {
                    actor: required(&input, "actor")?,
                    role: required(&input, "role")?,
                    project: optional(&input, "project"),
                },
                "revoke" => AccessRequest::Revoke {
                    grant_id: required(&input, "grant_id")?,
                },
                other => {
                    return Err(InterfaceError::new(format!(
                        "unsupported Issues access action '{other}'"
                    )));
                }
            };
            Ok(IssuesHostRequest::Access(request))
        }
        LOCAL_ATTACH => Ok(IssuesHostRequest::Attach {
            reff: required(&input, "reff")?,
            file: required(&input, "file")?,
            comment: optional(&input, "comment"),
        }),
        LOCAL_ATTACHMENT_GET => Ok(IssuesHostRequest::AttachmentGet {
            reff: required(&input, "reff")?,
            id: required(&input, "id")?,
            out: optional(&input, "out"),
        }),
        other => Err(InterfaceError::new(format!(
            "unsupported Issues host capability '{other}'"
        ))),
    }
}

/// Expand one pinned Issues role into the exact authority assignments that the
/// Space host must commit.
pub fn plan_access_grant(
    session: &runtime::Session,
    role: &str,
    project: Option<&str>,
) -> Result<AccessGrantPlan, AccessPlanError> {
    let Some(view) = crate::projections::query_json(
        session,
        issues::contract::IssueQuery::RoleShow {
            role: role.to_string(),
        },
    ) else {
        return Err(AccessPlanError::NotFound(format!(
            "no role `{role}` in this space"
        )));
    };
    let conflicts = view["conflict_heads"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !conflicts.is_empty() {
        return Err(AccessPlanError::Invalid(format!(
            "role `{role}` has {} concurrent revision heads — resolve them with \
             `lait issues role resolve` before assigning",
            conflicts.len()
        )));
    }
    let Some(revision) = view.get("revision").filter(|revision| !revision.is_null()) else {
        return Err(AccessPlanError::NotFound(format!(
            "role `{role}` has no usable revision"
        )));
    };
    let body = &revision["body"];
    if body["tombstone"].as_bool() == Some(true) {
        return Err(AccessPlanError::Invalid(format!(
            "role `{role}` is tombstoned"
        )));
    }
    let scope_kind = body["scope_kind"].as_str().unwrap_or("space");
    let world = issues::contract::PRODUCT_WORLD;
    let resource = match (scope_kind, project) {
        ("space", None) => mechanics::demand::PolicyResource::space(world),
        ("space", Some(_)) => {
            return Err(AccessPlanError::Invalid(
                "that is a Space role — it takes no --project".into(),
            ));
        }
        ("project", Some(selector)) => {
            let Some(snapshot) =
                crate::projections::query_json(session, issues::contract::IssueQuery::Snapshot)
            else {
                return Err(AccessPlanError::Invalid(
                    "the Issues catalog is unavailable".into(),
                ));
            };
            let projects = snapshot["catalog"]["projects"].as_object().cloned();
            let resolved = projects.and_then(|projects| {
                let upper = selector.to_ascii_uppercase();
                if projects.contains_key(selector) {
                    return Some(selector.to_string());
                }
                projects
                    .iter()
                    .find(|(_, metadata)| metadata["key"].as_str() == Some(upper.as_str()))
                    .map(|(id, _)| id.clone())
            });
            let Some(id) = resolved else {
                return Err(AccessPlanError::NotFound(format!(
                    "no project matches '{selector}'"
                )));
            };
            mechanics::demand::PolicyResource::project(world, &id)
        }
        ("project", None) => {
            return Err(AccessPlanError::Invalid(
                "that is a Project role — pass -p <project>".into(),
            ));
        }
        _ => {
            return Err(AccessPlanError::Invalid(
                "unrecognized Issues role scope".into(),
            ));
        }
    };
    let assignments: Vec<(
        mechanics::demand::PolicyCapability,
        mechanics::demand::PolicyResource,
    )> = body["capabilities"]
        .as_array()
        .map(|capabilities| {
            capabilities
                .iter()
                .filter_map(Value::as_str)
                .map(|capability| {
                    (
                        mechanics::demand::PolicyCapability::new(world, capability),
                        resource.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if assignments.is_empty() {
        return Err(AccessPlanError::Invalid(format!(
            "role `{role}` expands to no capabilities"
        )));
    }
    Ok(AccessGrantPlan { assignments })
}

fn required(value: &Value, field: &str) -> Result<String, InterfaceError> {
    optional(value, field)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| InterfaceError::new(format!("Issues invocation is missing '{field}'")))
}

fn optional(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn decodes_work_state_at_the_product_boundary() {
        let request = decode(
            LOCAL_WORK_STATE,
            json!({"action": "start", "reff": "ENG-7", "no_branch": true}),
        )
        .unwrap();
        assert!(matches!(
            request,
            IssuesHostRequest::WorkState {
                action: WorkStateAction::Start,
                ref reff,
                no_branch: true,
            } if reff == "ENG-7"
        ));
    }

    #[test]
    fn rejects_incomplete_access_grants_before_the_host_sees_them() {
        let error = decode(LOCAL_ACCESS, json!({"action": "grant", "actor": "alice"})).unwrap_err();
        assert!(error.to_string().contains("missing 'role'"));
    }
}
