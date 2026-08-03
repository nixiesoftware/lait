#![allow(
    clippy::as_conversions,
    reason = "projection counts are bounded by validated product collections"
)]
//! Issues-specific projections used by host-owned status, inbox, and
//! observation delivery.
//!
//! A host supplies a docked Session and local facts. It does not know which
//! World queries, Body ids, catalog planes, or row shapes produce these views.

use std::collections::{BTreeMap, HashMap};

use issues::contract::{self, IssueQuery, RingDigestView};
use issues::dto::{CatalogScope, InboxEntry, ProjectRef};
use issues::ids::SpaceId;
use replica::body::{BodyId, BodyKey};
use runtime::world::{DirtyPlane, DirtyScope, Invalidation, ScopeRef};
use runtime::{world::Query, Session};

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
    let snapshot = query_json(session, IssueQuery::Snapshot)?;
    let catalog = snapshot.get("catalog")?;
    let projects = catalog.get("projects")?.as_object().map(|map| map.len())?;
    let name = catalog
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or("")
        .to_string();
    let description = catalog
        .get("description")
        .and_then(|description| description.as_str())
        .unwrap_or("")
        .to_string();
    let issues = query_json(
        session,
        IssueQuery::List {
            project: None,
            label: None,
            status: None,
            milestone: None,
            mine: None,
            all: true,
            me: None,
        },
    )
    .and_then(|value| value.as_array().map(|rows| rows.len()))?;
    Some(StatusProjection {
        issues,
        projects,
        name,
        description,
    })
}

pub struct InboxProjection {
    pub entries: Vec<InboxEntry>,
    pub unread: u64,
}

/// Project the addressed-to-you inbox from World history.
pub fn inbox(
    session: &Session,
    actor: &str,
    exclude_device: &str,
    watermark: u64,
) -> InboxProjection {
    let Some(rows) = query_json(
        session,
        IssueQuery::Inbox {
            actor: actor.to_string(),
            exclude_device: Some(exclude_device.to_string()),
        },
    ) else {
        return InboxProjection {
            entries: Vec::new(),
            unread: 0,
        };
    };
    let mut entries = Vec::new();
    for entry in rows
        .as_array()
        .map(|entries| entries.as_slice())
        .unwrap_or_default()
    {
        entries.push(InboxEntry {
            ts: entry["ts"].as_u64().unwrap_or(0),
            kind: entry["kind"].as_str().unwrap_or_default().to_string(),
            reff: entry["reff"].as_str().unwrap_or_default().to_string(),
            doc_id: entry["doc_id"].as_str().unwrap_or_default().to_string(),
            title: entry["title"].as_str().unwrap_or_default().to_string(),
            detail: entry["detail"].as_str().unwrap_or_default().to_string(),
            actor: entry["actor"].as_str().map(String::from),
            actor_nick: None,
        });
    }
    entries.truncate(200);
    let unread = entries.iter().filter(|entry| entry.ts > watermark).count() as u64;
    InboxProjection { entries, unread }
}

/// Query one Issues projection as JSON through a pinned Session.
pub fn query_json(session: &Session, query: IssueQuery) -> Option<serde_json::Value> {
    let bytes = session
        .query(Query {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            payload: query.to_json(),
        })
        .ok()?
        .bytes;
    serde_json::from_slice(&bytes).ok()
}

/// One ring's committed Issues state, read from one `RingDigest` query.
pub struct RingState {
    docs: HashMap<BodyId, (String, ProjectRef)>,
    pub planes: BTreeMap<CatalogScope, String>,
}

pub fn ring_state(session: &Session) -> Option<RingState> {
    let value = query_json(session, IssueQuery::RingDigest)?;
    let view: RingDigestView = serde_json::from_value(value).ok()?;
    let docs = view
        .docs
        .into_iter()
        .map(|doc| {
            let project = ProjectRef {
                project_id: doc.project_id,
                project_key: doc.project_key,
            };
            (contract::issue_body_id(&doc.doc), (doc.doc, project))
        })
        .collect();
    let planes = view
        .planes
        .into_iter()
        .map(|plane| (plane.plane, plane.digest))
        .collect();
    Some(RingState { docs, planes })
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
    let catalog_body = contract::catalog_body_id(space);
    let mut catalog_dirty = false;
    let mut docs = Vec::new();
    for key in bodies {
        if key.body == catalog_body {
            catalog_dirty = true;
        } else {
            docs.push(&key.body);
        }
    }
    if !catalog_dirty && docs.is_empty() {
        return Default::default();
    }

    let Some(state) = ring_state(session) else {
        return Default::default();
    };
    let (dirty, missed) = resolve_docs(&state.docs, &docs);
    let mut planes = Vec::new();
    if catalog_dirty || missed {
        match baseline.as_ref() {
            Some(previous) => {
                for (scope, digest) in &state.planes {
                    if previous.get(scope) != Some(digest) {
                        planes.push(scope.clone());
                    }
                }
                for scope in previous.keys() {
                    if !state.planes.contains_key(scope) {
                        planes.push(scope.clone());
                    }
                }
            }
            None => planes.extend(state.planes.keys().cloned()),
        }
        *baseline = Some(state.planes);
        // A changed Body that is not an issue row leaves `missed` set. That used
        // to mean "ring the row index and hope", which was right while every
        // non-catalog Body *was* an issue — and became a lie once Specs arrived,
        // since every spec write then invalidated boards, rows and status too.
        //
        // A plane that moved already explains the miss, so the fallback is for
        // the case where nothing does: an unrecognised Body, where ringing
        // coarsely is still the honest answer.
        if missed && planes.is_empty() {
            planes.push(CatalogScope::Docs);
        }
    }
    Invalidation {
        dirty,
        planes: planes.into_iter().map(dirty_plane).collect(),
    }
}

fn resolve_docs(
    index: &HashMap<BodyId, (String, ProjectRef)>,
    docs: &[&BodyId],
) -> (Vec<DirtyScope>, bool) {
    let mut by_project: BTreeMap<ProjectRef, Vec<String>> = BTreeMap::new();
    let mut missed = false;
    for body in docs {
        match index.get(*body) {
            Some((doc, project)) => by_project
                .entry(project.clone())
                .or_default()
                .push(doc.clone()),
            None => missed = true,
        }
    }
    let dirty = by_project
        .into_iter()
        .map(|(project, docs)| DirtyScope {
            kind: SCOPE_PROJECT.into(),
            id: project.project_id,
            label: Some(project.project_key),
            docs,
        })
        .collect();
    (dirty, missed)
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
