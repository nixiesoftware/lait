#![allow(
    clippy::as_conversions,
    reason = "projection counts are bounded by validated product collections"
)]
//! Issues-specific projections used by host-owned status, inbox, and
//! observation delivery.
//!
//! A host supplies a docked Session and local facts. It does not know which
//! World queries, Body ids, catalog planes, or row shapes produce these views.

use std::collections::{BTreeMap, BTreeSet};

use issues::contract::{self, IssueQuery};
use issues::dto::CatalogScope;
use issues::ids::SpaceId;
use replica::body::BodyKey;
use runtime::world::{DirtyPlane, DirtyScope, Invalidation, ScopeRef};
use runtime::{find as find_api, world::Query, Session};

/// Issues' one container kind. World-declared vocabulary: the host carries this
/// string and never interprets it.
const SCOPE_PROJECT: &str = "project";

/// The Issues portion of a host status response.
pub struct StatusProjection {
    pub issues: usize,
    pub projects: usize,
    pub name: String,
    pub description: String,
}

/// Read issue/project counts and the product-owned Space metadata.
pub fn status(session: &Session) -> Option<StatusProjection> {
    let space = status_find(session, "space", None, true)?;
    let publication = Some(space.coordinates().publication());
    let row = space.rows().first()?;
    let name = find_text(row, issues::find::field::TITLE).unwrap_or_default();
    let description = find_text(row, issues::find::field::TEXT).unwrap_or_default();
    let projects =
        usize::try_from(status_find(session, "project", publication, false)?.matched_total()?)
            .ok()?;
    let issues =
        usize::try_from(status_find(session, "issue", publication, false)?.matched_total()?)
            .ok()?;
    Some(StatusProjection {
        issues,
        projects,
        name,
        description,
    })
}

fn status_find(
    session: &Session,
    kind: &str,
    publication: Option<runtime::publication::PublicationId>,
    pack_space: bool,
) -> Option<find_api::Answer> {
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: 4,
        edges_visited: 1,
        nodes_visited: 4,
        paths_retained: 1,
        candidates_per_branch: 4,
        score_evaluations: 1,
        projected_bytes: 16 * 1_024,
        packed_tokens: 16,
        wall_millis: 1_000,
    };
    let seek = find_api::StepId::new(1)?;
    let mut steps = vec![find_api::Step {
        id: seek,
        input: Vec::new(),
        op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
            field: issues::find::field_ref(issues::find::field::KIND),
            test: find_api::Test::Equal,
            value: find_api::Atom::Text(kind.into()),
        })),
        bound,
    }];
    let output = if pack_space {
        let pack = find_api::StepId::new(2)?;
        let mut fields = [
            issues::find::field_ref(issues::find::field::TITLE),
            issues::find::field_ref(issues::find::field::TEXT),
        ]
        .to_vec();
        fields.sort();
        steps.push(find_api::Step {
            id: pack,
            input: vec![seek],
            op: find_api::Op::Pack(find_api::Pack { fields }),
            bound,
        });
        pack
    } else {
        seek
    };
    session
        .find(find_api::Query {
            schema: issues::find::entity_schema_ref(),
            publication,
            mode: find_api::Mode::Exact,
            steps,
            output,
            bound,
            page_size: 1,
            cursor: None,
        })
        .ok()
}

fn find_text(row: &find_api::ResultRow, field: &str) -> Option<String> {
    row.fields.iter().find_map(|value| {
        (value.reference == issues::find::field_ref(field))
            .then_some(&value.value)
            .and_then(|value| match value {
                find_api::Value::Text(value) => Some(value.to_string()),
                _ => None,
            })
    })
}

/// Query one Issues projection as JSON through a pinned Session.
pub fn query_json(session: &Session, query: IssueQuery) -> Option<serde_json::Value> {
    let bytes = session
        .query(Query {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            payload: query.to_json(),
            publication: None,
        })
        .ok()?
        .bytes;
    serde_json::from_slice(&bytes).ok()
}

/// Compatibility shell for the host observation lifecycle. Plane digests are
/// no longer a second product snapshot: changed Bodies are classified through
/// Find and a missing/removed fact invalidates the fixed plane vocabulary.
pub struct RingState {
    pub planes: BTreeMap<CatalogScope, String>,
}

pub fn ring_state(session: &Session) -> Option<RingState> {
    let _ = session;
    Some(RingState {
        planes: BTreeMap::new(),
    })
}

/// Translate generic changed Bodies into the Issues doorbell dirty-set.
///
/// `baseline` is host-owned lifecycle state but its contents and update rules
/// are product-defined. A missing projection conservatively returns no named
/// Bodies; the generic Observation reset/authority flags still pass through.
pub fn observation(
    session: &Session,
    space: &SpaceId,
    bodies: &[BodyKey],
    baseline: &mut Option<BTreeMap<CatalogScope, String>>,
) -> Invalidation {
    if bodies.is_empty() {
        return Default::default();
    }
    let Some(rows) = seek_body_facts(session, bodies) else {
        return conservative_observation(space, bodies, baseline);
    };
    classify_body_facts(space, bodies, &rows, baseline)
}

fn observation_bound(body_count: usize) -> find_api::Bound {
    let bodies = u64::try_from(body_count).unwrap_or(4_096).clamp(1, 4_096);
    let candidate_rows = bodies.saturating_mul(512).min(100_000).max(1);
    find_api::Bound {
        decoded_bodies: bodies,
        // `Seek::Bodies` visits the admitted entity rows sourced by every
        // changed physical Body. A v4 semantic write commonly changes several
        // record Bodies, and each can project more than one entity row; one
        // posting was therefore an under-declaration that forced every such
        // change into the conservative `docs` fallback.
        postings_read: candidate_rows,
        edges_visited: 1,
        nodes_visited: candidate_rows,
        paths_retained: 1,
        candidates_per_branch: bodies.saturating_mul(512).min(10_000).max(1),
        score_evaluations: 1,
        projected_bytes: bodies.saturating_mul(16_384).min(8 * 1_024 * 1_024).max(1),
        packed_tokens: bodies.saturating_mul(3_072).min(32_768).max(1),
        wall_millis: 10_000,
    }
}

fn seek_body_facts(session: &Session, bodies: &[BodyKey]) -> Option<Vec<find_api::ResultRow>> {
    if bodies.len() > 4_096 {
        return None;
    }
    // Seek's wire contract is canonical: callers may deliver a change-set in
    // commit order, whereas Find requires stable sorted, unique coordinates.
    let mut bodies = bodies.to_vec();
    bodies.sort();
    bodies.dedup();
    let bound = observation_bound(bodies.len());
    let page_size = u32::try_from(bodies.len().saturating_mul(512))
        .unwrap_or(10_000)
        .clamp(1, 10_000);
    let seek = find_api::StepId::new(1)?;
    let pack = find_api::StepId::new(2)?;
    let mut fields = [
        issues::find::field::ID,
        issues::find::field::KIND,
        issues::find::field::PROJECT,
        issues::find::field::SOURCE_ID,
        issues::find::field::TARGET_ID,
    ]
    .into_iter()
    .map(issues::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let answer = session
        .find(find_api::Query {
            schema: issues::find::entity_schema_ref(),
            publication: None,
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Bodies(bodies)),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![seek],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size,
            cursor: None,
        })
        .ok()?;
    Some(answer.rows().to_vec())
}

fn text_field<'a>(row: &'a find_api::ResultRow, name: &str) -> Option<&'a str> {
    row.fields.iter().find_map(|field| {
        (field.reference == issues::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                find_api::Value::Text(value) => Some(value.as_ref()),
                _ => None,
            })
    })
}

fn classify_body_facts(
    space: &SpaceId,
    bodies: &[BodyKey],
    rows: &[find_api::ResultRow],
    baseline: &mut Option<BTreeMap<CatalogScope, String>>,
) -> Invalidation {
    let mut by_source = BTreeMap::<BodyKey, Vec<&find_api::ResultRow>>::new();
    for row in rows {
        by_source.entry(row.source.clone()).or_default().push(row);
    }
    let mut dirty_docs = BTreeMap::<String, BTreeSet<String>>::new();
    let mut planes = BTreeSet::<(String, Option<String>)>::new();
    let mut missed = false;
    for body in bodies {
        let Some(facts) = by_source.get(body) else {
            missed = true;
            continue;
        };
        for row in facts {
            let kind = text_field(row, issues::find::field::KIND).unwrap_or_default();
            let project = text_field(row, issues::find::field::PROJECT);
            let id = text_field(row, issues::find::field::ID);
            if let Some(project) = project {
                let docs = dirty_docs.entry(project.into()).or_default();
                if kind == "issue" {
                    if let Some(id) = id {
                        docs.insert(id.into());
                    }
                } else if kind == "relation" {
                    if let Some(source) = text_field(row, issues::find::field::SOURCE_ID) {
                        if source.starts_with("iss_") {
                            docs.insert(source.into());
                        }
                    }
                }
            }
            match kind {
                "space" => {
                    for plane in ["space", "projects", "labels"] {
                        planes.insert((plane.into(), None));
                    }
                }
                "space_governance" => {
                    planes.insert(("roles".into(), None));
                }
                "project" => {
                    planes.insert(("projects".into(), None));
                }
                "project_workflow" => {
                    planes.insert(("workflow".into(), project.map(str::to_owned)));
                }
                "milestone" => {
                    planes.insert(("milestones".into(), project.map(str::to_owned)));
                }
                "cycle" => {
                    planes.insert(("cycles".into(), project.map(str::to_owned)));
                }
                "project_update" => {
                    planes.insert(("updates".into(), project.map(str::to_owned)));
                }
                "relation" => {
                    planes.insert(("relations".into(), project.map(str::to_owned)));
                }
                "initiative" => {
                    planes.insert(("initiatives".into(), None));
                }
                "team" => {
                    planes.insert(("teams".into(), None));
                }
                "triage" | "triage_decision" | "triage_resolution" => {
                    planes.insert(("triage".into(), None));
                }
                "spec" | "baseline" => {
                    planes.insert(("specs".into(), project.map(str::to_owned)));
                }
                "comment" | "reaction" | "activity" => {
                    planes.insert(("docs".into(), None));
                }
                "issue" => {}
                _ => missed = true,
            }
        }
    }
    if missed {
        // A removed node or an auxiliary physical record has no row in the new
        // publication. Its semantic owner cannot be recovered from that Body
        // hash alone, so invalidate the fixed product vocabulary. This is a
        // bounded 15-plane fan-out, never a World-wide digest query, and it
        // avoids misclassifying an unprojected Spec head as an Issue edit.
        for plane in all_plane_names() {
            planes.insert(((*plane).into(), None));
        }
    }
    Invalidation {
        dirty: dirty_docs
            .into_iter()
            .map(|(project, docs)| DirtyScope {
                kind: SCOPE_PROJECT.into(),
                id: project,
                label: None,
                docs: docs.into_iter().collect(),
            })
            .collect(),
        planes: planes
            .into_iter()
            .map(|(plane, project)| DirtyPlane {
                plane,
                scope: project.map(|id| ScopeRef {
                    kind: SCOPE_PROJECT.into(),
                    id,
                    label: None,
                }),
            })
            .collect(),
    }
}

fn conservative_observation(
    space: &SpaceId,
    bodies: &[BodyKey],
    _baseline: &Option<BTreeMap<CatalogScope, String>>,
) -> Invalidation {
    let _ = (space, bodies);
    let planes = all_plane_names()
        .iter()
        .map(|plane| DirtyPlane {
            plane: (*plane).into(),
            scope: None,
        })
        .collect();
    Invalidation {
        dirty: Vec::new(),
        planes,
    }
}

const fn all_plane_names() -> &'static [&'static str] {
    &[
        "space",
        "projects",
        "labels",
        "workflow",
        "boards",
        "milestones",
        "cycles",
        "updates",
        "initiatives",
        "teams",
        "triage",
        "roles",
        "specs",
        "docs",
        "relations",
    ]
}

/// Issues' catalog planes in the World-declared vocabulary the host carries.
///
/// An exhaustive match with no wildcard on purpose: a plane added to
/// [`CatalogScope`] must fail to compile here rather than silently project
/// nothing, which is a resource that never refreshes.
fn dirty_plane(scope: CatalogScope) -> DirtyPlane {
    let (plane, project) = match scope {
        CatalogScope::Space => ("space", None),
        CatalogScope::Projects => ("projects", None),
        CatalogScope::Labels => ("labels", None),
        CatalogScope::Workflow => ("workflow", None),
        CatalogScope::Boards {
            project_id,
            project_key,
        } => ("boards", Some((project_id, project_key))),
        CatalogScope::Milestones {
            project_id,
            project_key,
        } => ("milestones", Some((project_id, project_key))),
        CatalogScope::Cycles {
            project_id,
            project_key,
        } => ("cycles", Some((project_id, project_key))),
        CatalogScope::Updates {
            project_id,
            project_key,
        } => ("updates", Some((project_id, project_key))),
        CatalogScope::Initiatives => ("initiatives", None),
        CatalogScope::Teams => ("teams", None),
        CatalogScope::Triage => ("triage", None),
        CatalogScope::Roles => ("roles", None),
        CatalogScope::Specs => ("specs", None),
        CatalogScope::Docs => ("docs", None),
        CatalogScope::Relations => ("relations", None),
    };
    DirtyPlane {
        plane: plane.into(),
        scope: project.map(|(id, key)| ScopeRef {
            kind: SCOPE_PROJECT.into(),
            id,
            label: Some(key),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source: BodyKey, id: &str, kind: &str, project: Option<&str>) -> find_api::ResultRow {
        let mut fields = vec![
            find_api::ResultField {
                reference: issues::find::field_ref(issues::find::field::ID),
                value: find_api::Value::Text(id.into()),
            },
            find_api::ResultField {
                reference: issues::find::field_ref(issues::find::field::KIND),
                value: find_api::Value::Text(kind.into()),
            },
        ];
        if let Some(project) = project {
            fields.push(find_api::ResultField {
                reference: issues::find::field_ref(issues::find::field::PROJECT),
                value: find_api::Value::Text(project.into()),
            });
        }
        find_api::ResultRow {
            source,
            key: find_api::NodeKey {
                schema: issues::find::entity_schema_ref(),
                node: find_api::NodeId::new(id.as_bytes().to_vec()).expect("fixture node"),
            },
            fields,
            path: None,
        }
    }

    #[test]
    fn body_facts_name_only_the_changed_issue_and_its_project() {
        let space = SpaceId::from_digest([7; 16]);
        let doc = "iss_01k1k8q6c6t0g0000000000001";
        let project = "prj_01k1k8q6c6t0g0000000000001";
        let body = contract::issue_key(doc);
        let rows = vec![row(body.clone(), doc, "issue", Some(project))];
        let mut baseline = None;

        let invalidation = classify_body_facts(&space, &[body], &rows, &mut baseline);

        assert_eq!(invalidation.dirty.len(), 1);
        assert_eq!(invalidation.dirty[0].id, project);
        assert_eq!(invalidation.dirty[0].docs, vec![doc]);
        assert!(invalidation.planes.is_empty());
    }

    #[test]
    fn a_removed_body_falls_back_without_a_world_scan() {
        let space = SpaceId::from_digest([8; 16]);
        let body = contract::issue_key("iss_01k1k8q6c6t0g0000000000002");
        let mut baseline = None;

        let invalidation = classify_body_facts(&space, &[body], &[], &mut baseline);

        assert!(invalidation.dirty.is_empty());
        assert_eq!(invalidation.planes.len(), 1);
        assert_eq!(invalidation.planes[0].plane, "docs");
    }
}
