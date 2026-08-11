#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the compiler indexes bounded vectors and saturates externally visible counts"
)]

//! Deterministic Issue geometry for Blueprint.
//!
//! Nothing in this module is replicated truth. It compiles the Issue graph and
//! ordinary metadata at one World generation into a phenotype: components,
//! dependency layers, hierarchy depth, facets, closure state, and typed
//! residual loci. Layout engines may bend this phenotype into an organic view,
//! but may not invent a node, edge, position, or gap.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::dto::{Row, StatusCategory};
use crate::views::{project_row, CatalogState, DerivedAliases, IssueState};

/// The compiled morphology of one Plan seed (or a whole project when unseeded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryView {
    pub schema_version: u32,
    /// Manifest root of the coherent World snapshot used for every field.
    pub generation: String,
    pub project: String,
    /// Canonical Issue ids requested as roots. Missing roots remain here and
    /// also appear as residual loci; absence is never silently normalized.
    pub roots: Vec<String>,
    pub nodes: Vec<GeometryNode>,
    pub edges: Vec<GeometryEdge>,
    pub components: Vec<GeometryComponent>,
    pub residuals: Vec<ResidualLocus>,
    pub closure: GeometryClosure,
}

/// One Issue and its deterministic coordinates in the compiled morphology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryNode {
    pub row: Row,
    pub component: String,
    /// Longest dependency distance from an unconstrained Issue. `None` means a
    /// loop (or its downstream wake) makes a topological position impossible.
    pub layer: Option<u32>,
    /// Stable order inside the layer, for initial placement before relaxation.
    pub ordinal: u32,
    pub hierarchy_depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub blocked_by: Vec<String>,
    pub blocks: Vec<String>,
    /// `closed` | `ready` | `blocked` | `cycle` | `stalled`.
    pub closure: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<u32>,
    pub facets: Vec<GeometryFacet>,
}

/// A canonical relation. `role` explains its geometric force without naming a
/// global shape: constraint, containment, equivalence, or association.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GeometryEdge {
    pub from: String,
    pub relation: String,
    pub role: String,
    pub to: String,
}

/// A connected patch of the morphology. Several patches are a forest, not a
/// malformed Plan; roots and terminals make each patch traversable directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryComponent {
    pub id: String,
    pub nodes: Vec<String>,
    pub roots: Vec<String>,
    pub terminals: Vec<String>,
    pub loops: Vec<Vec<String>>,
}

/// Ordinary Blueprint primitives acting as geometric fields around an Issue.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GeometryFacet {
    /// `project` | `team` | `status` | `label` | `milestone` | `cycle` |
    /// `assignee`.
    pub kind: String,
    pub id: String,
    pub label: String,
}

/// A missing or contradictory locus, with the position needed to repair it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidualLocus {
    /// `root_missing` | `dependency_cycle` | `blocked_frontier` |
    /// `due_order_conflict` | `unattached` | `closure_frontier`.
    pub kind: String,
    pub component: Option<String>,
    pub layer: Option<u32>,
    pub at: Vec<String>,
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryClosure {
    pub total: u32,
    pub closed: u32,
    pub ready: u32,
    pub blocked: u32,
    pub cyclic: u32,
    pub stalled: u32,
}

fn relation_role(relation: &str) -> &'static str {
    match relation {
        "blocks" => "constraint",
        "duplicates" => "equivalence",
        "contains" => "containment",
        _ => "association",
    }
}

fn neighbors(
    catalog: &CatalogState,
    candidates: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut result: BTreeMap<String, BTreeSet<String>> = candidates
        .iter()
        .map(|doc| (doc.clone(), BTreeSet::new()))
        .collect();
    for (from, _, to) in &catalog.edges {
        if candidates.contains(from) && candidates.contains(to) {
            result.entry(from.clone()).or_default().insert(to.clone());
            result.entry(to.clone()).or_default().insert(from.clone());
        }
    }
    for (child, parent) in &catalog.parents {
        if candidates.contains(child) && candidates.contains(parent) {
            result
                .entry(child.clone())
                .or_default()
                .insert(parent.clone());
            result
                .entry(parent.clone())
                .or_default()
                .insert(child.clone());
        }
    }
    result
}

fn selected_docs(
    candidates: &BTreeSet<String>,
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    roots: &[String],
) -> BTreeSet<String> {
    if roots.is_empty() {
        return candidates.clone();
    }
    let mut selected = BTreeSet::new();
    let mut queue: VecDeque<String> = roots
        .iter()
        .filter(|root| candidates.contains(*root))
        .cloned()
        .collect();
    while let Some(doc) = queue.pop_front() {
        if !selected.insert(doc.clone()) {
            continue;
        }
        for next in adjacency.get(&doc).into_iter().flatten() {
            if !selected.contains(next) {
                queue.push_back(next.clone());
            }
        }
    }
    selected
}

fn connected_components(
    selected: &BTreeSet<String>,
    adjacency: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut components = Vec::new();
    for start in selected {
        if seen.contains(start) {
            continue;
        }
        let mut queue = VecDeque::from([start.clone()]);
        let mut component = Vec::new();
        while let Some(doc) = queue.pop_front() {
            if !seen.insert(doc.clone()) {
                continue;
            }
            component.push(doc.clone());
            for next in adjacency.get(&doc).into_iter().flatten() {
                if selected.contains(next) && !seen.contains(next) {
                    queue.push_back(next.clone());
                }
            }
        }
        component.sort();
        components.push(component);
    }
    components.sort_by(|a, b| a.first().cmp(&b.first()));
    components
}

fn finishing_order(docs: &[String], onward: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::with_capacity(docs.len());
    for start in docs {
        if !seen.insert(start.clone()) {
            continue;
        }
        let mut stack = vec![(start.clone(), 0usize)];
        while let Some((doc, offset)) = stack.last_mut() {
            let next = onward
                .get(doc)
                .and_then(|edges| edges.get(*offset))
                .cloned();
            if let Some(next) = next {
                *offset = offset.saturating_add(1);
                if seen.insert(next.clone()) {
                    stack.push((next, 0));
                }
            } else if let Some((finished, _)) = stack.pop() {
                order.push(finished);
            }
        }
    }
    order
}

fn strongly_connected(docs: &[String], onward: &BTreeMap<String, Vec<String>>) -> Vec<Vec<String>> {
    let mut reverse: BTreeMap<String, Vec<String>> =
        docs.iter().map(|doc| (doc.clone(), Vec::new())).collect();
    for (from, tos) in onward {
        for to in tos {
            reverse.entry(to.clone()).or_default().push(from.clone());
        }
    }
    for edges in reverse.values_mut() {
        edges.sort();
        edges.dedup();
    }
    let mut seen = BTreeSet::new();
    let mut components = Vec::new();
    for start in finishing_order(docs, onward).into_iter().rev() {
        if !seen.insert(start.clone()) {
            continue;
        }
        let mut stack = vec![start];
        let mut component = Vec::new();
        while let Some(doc) = stack.pop() {
            component.push(doc.clone());
            for next in reverse.get(&doc).into_iter().flatten().rev() {
                if seen.insert(next.clone()) {
                    stack.push(next.clone());
                }
            }
        }
        component.sort();
        components.push(component);
    }
    components.sort_by(|a, b| a.first().cmp(&b.first()));
    components
}

fn hierarchy_depth(doc: &str, parents: &BTreeMap<String, String>) -> u32 {
    let mut depth = 0u32;
    let mut cursor = doc;
    let mut seen = BTreeSet::new();
    while seen.insert(cursor) {
        let Some(parent) = parents.get(cursor) else {
            break;
        };
        depth = depth.saturating_add(1);
        cursor = parent;
    }
    depth
}

fn facets(catalog: &CatalogState, issue: &IssueState) -> Vec<GeometryFacet> {
    let mut result = Vec::new();
    if let Some(project) = catalog.projects.get(&issue.project) {
        result.push(GeometryFacet {
            kind: "project".into(),
            id: issue.project.clone(),
            label: project.name.clone(),
        });
        if !project.team.is_empty() {
            let label = catalog
                .teams
                .get(&project.team)
                .map(|team| team.name.clone())
                .unwrap_or_else(|| project.team.clone());
            result.push(GeometryFacet {
                kind: "team".into(),
                id: project.team.clone(),
                label,
            });
        }
    }
    result.push(GeometryFacet {
        kind: "status".into(),
        id: issue.status.clone(),
        label: catalog
            .workflow_state(&issue.status)
            .map(|state| state.name.clone())
            .unwrap_or_else(|| issue.status.clone()),
    });
    for label in &issue.labels {
        result.push(GeometryFacet {
            kind: "label".into(),
            id: label.clone(),
            label: catalog
                .labels
                .get(label)
                .map(|meta| meta.name.clone())
                .unwrap_or_else(|| label.clone()),
        });
    }
    if let Some(milestone) = &issue.milestone {
        let label = catalog
            .milestones
            .get(&issue.project)
            .and_then(|items| items.get(milestone))
            .map(|item| item.name.clone())
            .unwrap_or_else(|| milestone.clone());
        result.push(GeometryFacet {
            kind: "milestone".into(),
            id: milestone.clone(),
            label,
        });
    }
    if let Some(cycle) = &issue.cycle {
        let label = catalog
            .cycles
            .get(&issue.project)
            .and_then(|items| items.get(cycle))
            .map(|item| item.name.clone())
            .unwrap_or_else(|| cycle.clone());
        result.push(GeometryFacet {
            kind: "cycle".into(),
            id: cycle.clone(),
            label,
        });
    }
    for actor in &issue.assignees {
        result.push(GeometryFacet {
            kind: "assignee".into(),
            id: actor.as_str().to_string(),
            label: actor.short(),
        });
    }
    result.sort();
    result.dedup();
    result
}

/// Compile one coherent snapshot. Runtime supplies `generation`; Blueprint
/// supplies all remaining semantics.
pub fn compile(
    catalog: &CatalogState,
    aliases: &DerivedAliases,
    issues: &BTreeMap<String, std::sync::Arc<IssueState>>,
    project: &str,
    roots: &[String],
    generation: String,
) -> GeometryView {
    let candidates: BTreeSet<String> = issues
        .iter()
        .filter(|(doc, issue)| issue.project == project && !catalog.tombstones.contains(*doc))
        .map(|(doc, _)| doc.clone())
        .collect();
    let adjacency = neighbors(catalog, &candidates);
    let selected = selected_docs(&candidates, &adjacency, roots);
    let component_docs = connected_components(&selected, &adjacency);
    let mut component_of = BTreeMap::new();
    for (offset, docs) in component_docs.iter().enumerate() {
        let id = format!("component-{}", offset.saturating_add(1));
        for doc in docs {
            component_of.insert(doc.clone(), id.clone());
        }
    }

    let mut blocked_by: BTreeMap<String, Vec<String>> = selected
        .iter()
        .map(|doc| (doc.clone(), Vec::new()))
        .collect();
    let mut blocks = blocked_by.clone();
    let mut edges = BTreeSet::new();
    for (from, relation, to) in &catalog.edges {
        if !selected.contains(from) || !selected.contains(to) {
            continue;
        }
        edges.insert(GeometryEdge {
            from: from.clone(),
            relation: relation.clone(),
            role: relation_role(relation).into(),
            to: to.clone(),
        });
        if relation == "blocks" {
            blocked_by.entry(to.clone()).or_default().push(from.clone());
            blocks.entry(from.clone()).or_default().push(to.clone());
        }
    }
    for (child, parent) in &catalog.parents {
        if selected.contains(child) && selected.contains(parent) {
            edges.insert(GeometryEdge {
                from: parent.clone(),
                relation: "contains".into(),
                role: "containment".into(),
                to: child.clone(),
            });
        }
    }
    for values in blocked_by.values_mut().chain(blocks.values_mut()) {
        values.sort();
        values.dedup();
    }

    // Longest-path dependency layer (Kahn). This is O(V+E); no transitive
    // closure is materialized.
    let mut indegree: BTreeMap<String, usize> = blocked_by
        .iter()
        .map(|(doc, values)| (doc.clone(), values.len()))
        .collect();
    let mut layer = BTreeMap::<String, u32>::new();
    let mut ready: BTreeSet<String> = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(doc, _)| doc.clone())
        .collect();
    while let Some(doc) = ready.pop_first() {
        let at = layer.get(&doc).copied().unwrap_or(0);
        layer.insert(doc.clone(), at);
        for dependent in blocks.get(&doc).into_iter().flatten() {
            let next_layer = at.saturating_add(1);
            layer
                .entry(dependent.clone())
                .and_modify(|current| *current = (*current).max(next_layer))
                .or_insert(next_layer);
            if let Some(count) = indegree.get_mut(dependent) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.insert(dependent.clone());
                }
            }
        }
    }
    let undrained: Vec<String> = selected
        .iter()
        .filter(|doc| !layer.contains_key(*doc))
        .cloned()
        .collect();
    let undrained_set: BTreeSet<String> = undrained.iter().cloned().collect();
    let onward: BTreeMap<String, Vec<String>> = undrained
        .iter()
        .map(|doc| {
            let next = blocks
                .get(doc)
                .into_iter()
                .flatten()
                .filter(|candidate| undrained_set.contains(*candidate))
                .cloned()
                .collect();
            (doc.clone(), next)
        })
        .collect();
    let sccs = strongly_connected(&undrained, &onward);
    let mut cyclic = BTreeSet::new();
    let mut loops = Vec::new();
    for component in sccs {
        let self_loop = component
            .first()
            .is_some_and(|doc| blocks.get(doc).is_some_and(|values| values.contains(doc)));
        if component.len() > 1 || self_loop {
            cyclic.extend(component.iter().cloned());
            loops.push(component);
        }
    }
    let stalled: BTreeSet<String> = undrained_set.difference(&cyclic).cloned().collect();

    let max_layer = layer.values().copied().max().unwrap_or(0);
    let mut onward_depth = BTreeMap::<String, u32>::new();
    let mut placed: Vec<String> = layer.keys().cloned().collect();
    placed.sort_by_key(|doc| std::cmp::Reverse(layer.get(doc).copied().unwrap_or(0)));
    for doc in &placed {
        let depth = blocks
            .get(doc)
            .into_iter()
            .flatten()
            .filter_map(|next| onward_depth.get(next).map(|depth| depth.saturating_add(1)))
            .max()
            .unwrap_or(0);
        onward_depth.insert(doc.clone(), depth);
    }
    let slack: BTreeMap<String, u32> = placed
        .iter()
        .map(|doc| {
            let earliest = layer.get(doc).copied().unwrap_or(0);
            let onward = onward_depth.get(doc).copied().unwrap_or(0);
            (
                doc.clone(),
                max_layer.saturating_sub(earliest.saturating_add(onward)),
            )
        })
        .collect();

    let mut children: BTreeMap<String, Vec<String>> = selected
        .iter()
        .map(|doc| (doc.clone(), Vec::new()))
        .collect();
    for (child, parent) in &catalog.parents {
        if selected.contains(child) && selected.contains(parent) {
            children
                .entry(parent.clone())
                .or_default()
                .push(child.clone());
        }
    }
    for values in children.values_mut() {
        values.sort();
    }

    let mut ordinal = BTreeMap::new();
    let mut layers: BTreeMap<Option<u32>, Vec<String>> = BTreeMap::new();
    for doc in &selected {
        layers
            .entry(layer.get(doc).copied())
            .or_default()
            .push(doc.clone());
    }
    for docs in layers.values_mut() {
        docs.sort_by_key(|doc| {
            (
                hierarchy_depth(doc, &catalog.parents),
                issues.get(doc).and_then(|issue| issue.milestone.clone()),
                doc.clone(),
            )
        });
        for (offset, doc) in docs.iter().enumerate() {
            ordinal.insert(doc.clone(), u32::try_from(offset).unwrap_or(u32::MAX));
        }
    }

    let mut residuals = Vec::new();
    for root in roots {
        if !candidates.contains(root) {
            residuals.push(ResidualLocus {
                kind: "root_missing".into(),
                component: None,
                layer: None,
                at: vec![root.clone()],
                requires: Vec::new(),
            });
        }
    }
    for loop_docs in &loops {
        residuals.push(ResidualLocus {
            kind: "dependency_cycle".into(),
            component: loop_docs
                .first()
                .and_then(|doc| component_of.get(doc))
                .cloned(),
            layer: None,
            at: loop_docs.clone(),
            requires: loop_docs.clone(),
        });
    }

    let done = |doc: &str| {
        issues
            .get(doc)
            .is_some_and(|issue| catalog.status_category(&issue.status) == StatusCategory::Done)
    };
    let mut closure = GeometryClosure {
        total: u32::try_from(selected.len()).unwrap_or(u32::MAX),
        ..GeometryClosure::default()
    };
    let mut nodes = Vec::with_capacity(selected.len());
    for doc in &selected {
        let Some(issue) = issues.get(doc) else {
            continue;
        };
        let blockers = blocked_by.get(doc).cloned().unwrap_or_default();
        let open_blockers: Vec<String> = blockers
            .iter()
            .filter(|blocker| !done(blocker))
            .cloned()
            .collect();
        let state = if done(doc) {
            closure.closed = closure.closed.saturating_add(1);
            "closed"
        } else if cyclic.contains(doc) {
            closure.cyclic = closure.cyclic.saturating_add(1);
            "cycle"
        } else if stalled.contains(doc) {
            closure.stalled = closure.stalled.saturating_add(1);
            "stalled"
        } else if open_blockers.is_empty() {
            closure.ready = closure.ready.saturating_add(1);
            "ready"
        } else {
            closure.blocked = closure.blocked.saturating_add(1);
            "blocked"
        };
        if state == "blocked" {
            residuals.push(ResidualLocus {
                kind: "blocked_frontier".into(),
                component: component_of.get(doc).cloned(),
                layer: layer.get(doc).copied(),
                at: vec![doc.clone()],
                requires: open_blockers.clone(),
            });
        }
        if let Some(due) = issue.duedate {
            let conflicts: Vec<String> = blockers
                .iter()
                .filter(|blocker| {
                    issues
                        .get(*blocker)
                        .and_then(|candidate| candidate.duedate)
                        .is_some_and(|blocker_due| blocker_due >= due)
                })
                .cloned()
                .collect();
            if !conflicts.is_empty() {
                residuals.push(ResidualLocus {
                    kind: "due_order_conflict".into(),
                    component: component_of.get(doc).cloned(),
                    layer: layer.get(doc).copied(),
                    at: vec![doc.clone()],
                    requires: conflicts,
                });
            }
        }
        let structurally_alone = adjacency.get(doc).is_none_or(BTreeSet::is_empty);
        if structurally_alone && !roots.contains(doc) && selected.len() > 1 {
            residuals.push(ResidualLocus {
                kind: "unattached".into(),
                component: component_of.get(doc).cloned(),
                layer: layer.get(doc).copied(),
                at: vec![doc.clone()],
                requires: roots.to_vec(),
            });
        }
        if !done(doc)
            && blocks.get(doc).is_none_or(Vec::is_empty)
            && children.get(doc).is_none_or(Vec::is_empty)
        {
            residuals.push(ResidualLocus {
                kind: "closure_frontier".into(),
                component: component_of.get(doc).cloned(),
                layer: layer.get(doc).copied(),
                at: vec![doc.clone()],
                requires: Vec::new(),
            });
        }
        nodes.push(GeometryNode {
            row: project_row(catalog, aliases, doc, Some(issue), None),
            component: component_of
                .get(doc)
                .cloned()
                .unwrap_or_else(|| "component-0".into()),
            layer: layer.get(doc).copied(),
            ordinal: ordinal.get(doc).copied().unwrap_or(0),
            hierarchy_depth: hierarchy_depth(doc, &catalog.parents),
            parent: catalog
                .parents
                .get(doc)
                .filter(|parent| selected.contains(*parent))
                .cloned(),
            children: children.get(doc).cloned().unwrap_or_default(),
            blocked_by: blockers,
            blocks: blocks.get(doc).cloned().unwrap_or_default(),
            closure: state.into(),
            slack: slack.get(doc).copied(),
            facets: facets(catalog, issue),
        });
    }
    nodes.sort_by_key(|node| {
        (
            node.component.clone(),
            node.layer,
            node.ordinal,
            node.row.doc_id.to_string(),
        )
    });
    residuals.sort_by_key(|locus| {
        (
            locus.component.clone(),
            locus.layer,
            locus.kind.clone(),
            locus.at.clone(),
        )
    });

    let components = component_docs
        .into_iter()
        .enumerate()
        .map(|(offset, docs)| {
            let members: BTreeSet<&str> = docs.iter().map(String::as_str).collect();
            let roots = docs
                .iter()
                .filter(|doc| {
                    blocked_by.get(*doc).is_none_or(|values| {
                        values.iter().all(|value| !members.contains(value.as_str()))
                    })
                })
                .cloned()
                .collect();
            let terminals = docs
                .iter()
                .filter(|doc| {
                    blocks.get(*doc).is_none_or(|values| {
                        values.iter().all(|value| !members.contains(value.as_str()))
                    })
                })
                .cloned()
                .collect();
            let component_loops = loops
                .iter()
                .filter(|loop_docs| {
                    loop_docs
                        .first()
                        .is_some_and(|doc| members.contains(doc.as_str()))
                })
                .cloned()
                .collect();
            GeometryComponent {
                id: format!("component-{}", offset.saturating_add(1)),
                nodes: docs,
                roots,
                terminals,
                loops: component_loops,
            }
        })
        .collect();

    GeometryView {
        schema_version: crate::contract::VIEW_SCHEMA_VERSION,
        generation,
        project: project.into(),
        roots: roots.to_vec(),
        nodes,
        edges: edges.into_iter().collect(),
        components,
        residuals,
        closure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::Priority;
    use crate::spec::PlanData;
    use crate::views::{LabelMeta, Milestone, ProjectMeta, Team};

    fn issue(project: &str, status: &str) -> std::sync::Arc<IssueState> {
        std::sync::Arc::new(IssueState {
            project: project.into(),
            title: status.into(),
            status: status.into(),
            priority: Priority::None,
            ..IssueState::default()
        })
    }

    fn fixture() -> (
        CatalogState,
        DerivedAliases,
        BTreeMap<String, std::sync::Arc<IssueState>>,
    ) {
        let project = "prj_01k1k8q6c6t0g0000000000000";
        let a = "iss_01k1k8q6c6t0g0000000000000";
        let b = "iss_01k1k8q6c6t0g0000000000001";
        let c = "iss_01k1k8q6c6t0g0000000000002";
        let mut catalog = CatalogState::default();
        catalog.workflow = crate::dto::default_workflow();
        catalog.projects.insert(
            project.into(),
            ProjectMeta {
                name: "Client".into(),
                key: "CLIENT".into(),
                ..ProjectMeta::default()
            },
        );
        catalog.labels.insert(
            "lbl_ui".into(),
            LabelMeta {
                name: "UI".into(),
                color: "blue".into(),
            },
        );
        catalog.edges.insert((a.into(), "blocks".into(), b.into()));
        catalog.edges.insert((a.into(), "blocks".into(), c.into()));
        catalog.parents.insert(c.into(), b.into());
        let issues: BTreeMap<String, std::sync::Arc<IssueState>> = [
            (a.into(), issue(project, "done")),
            (b.into(), issue(project, "backlog")),
            (c.into(), issue(project, "backlog")),
        ]
        .into_iter()
        .collect();
        let aliases = crate::views::derive_aliases(&catalog, |doc| {
            issues
                .get(doc)
                .map(|value: &std::sync::Arc<IssueState>| value.project.as_str())
        });
        (catalog, aliases, issues)
    }

    #[test]
    fn a_seed_compiles_branching_order_and_closure_without_stored_layout() {
        let (catalog, aliases, issues) = fixture();
        let root = "iss_01k1k8q6c6t0g0000000000000".to_string();
        let view = compile(
            &catalog,
            &aliases,
            &issues,
            "prj_01k1k8q6c6t0g0000000000000",
            &[root],
            "01".repeat(32),
        );
        assert_eq!(view.nodes.len(), 3);
        assert_eq!(
            view.nodes.iter().map(|node| node.layer).collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(1)]
        );
        assert_eq!(view.closure.closed, 1);
        assert_eq!(view.closure.ready, 2);
        assert!(view.edges.iter().any(|edge| edge.role == "containment"));
    }

    #[test]
    fn cycles_are_loci_and_downstream_work_is_stalled_not_mislabeled() {
        let (mut catalog, aliases, mut issues) = fixture();
        let a = "iss_01k1k8q6c6t0g0000000000000";
        let b = "iss_01k1k8q6c6t0g0000000000001";
        let c = "iss_01k1k8q6c6t0g0000000000002";
        catalog.edges.insert((b.into(), "blocks".into(), a.into()));
        std::sync::Arc::make_mut(issues.get_mut(a).expect("a")).status = "backlog".into();
        let view = compile(
            &catalog,
            &aliases,
            &issues,
            "prj_01k1k8q6c6t0g0000000000000",
            &[],
            "02".repeat(32),
        );
        assert!(view
            .residuals
            .iter()
            .any(|locus| locus.kind == "dependency_cycle"));
        assert_eq!(
            view.nodes
                .iter()
                .find(|node| node.row.doc_id.as_str() == c)
                .map(|node| node.closure.as_str()),
            Some("stalled")
        );
    }

    #[test]
    fn client_one_plan_emerges_from_issue_facts_and_moves_its_open_locus() {
        const PROJECT: &str = "prj_01k1k8q6c6t0g0000000000000";
        const SHELL: &str = "iss_01k1k8q6c6t0g0000000000000";
        const CONNECT: &str = "iss_01k1k8q6c6t0g0000000000001";
        const WORLDS: &str = "iss_01k1k8q6c6t0g0000000000002";
        const COMMANDS: &str = "iss_01k1k8q6c6t0g0000000000003";
        const WORKSPACE: &str = "iss_01k1k8q6c6t0g0000000000004";
        const RECOVERY: &str = "iss_01k1k8q6c6t0g0000000000005";

        // This is all the Plan stores. The execution structure below is not a
        // second list hidden in the document; it is compiled from Issue facts.
        let plan = PlanData {
            roots: vec![SHELL.into()],
        };
        assert_eq!(
            serde_json::to_value(&plan).expect("plan json"),
            serde_json::json!({ "roots": [SHELL] })
        );

        let mut catalog = CatalogState::default();
        catalog.workflow = crate::dto::default_workflow();
        catalog.projects.insert(
            PROJECT.into(),
            ProjectMeta {
                name: "Lait Client v1: the local client for served Worlds".into(),
                key: "CLIENT".into(),
                team: "team_client".into(),
                ..ProjectMeta::default()
            },
        );
        catalog.teams.insert(
            "team_client".into(),
            Team {
                id: "team_client".into(),
                name: "Client".into(),
                key: "CLIENT".into(),
                ..Team::default()
            },
        );
        catalog.labels.insert(
            "lbl_local_first".into(),
            LabelMeta {
                name: "local-first".into(),
                color: "blue".into(),
            },
        );
        catalog
            .milestones
            .entry(PROJECT.into())
            .or_default()
            .insert(
                "mls_v1".into(),
                Milestone {
                    id: "mls_v1".into(),
                    project_id: PROJECT.into(),
                    name: "Client v1".into(),
                    ..Milestone::default()
                },
            );

        // One foundation branches into World discovery and local commands;
        // both converge on the usable workspace. Recovery is a child concern,
        // so containment—not stored Plan placement—keeps it in the phenotype.
        for (from, to) in [
            (SHELL, CONNECT),
            (CONNECT, WORLDS),
            (CONNECT, COMMANDS),
            (WORLDS, WORKSPACE),
            (COMMANDS, WORKSPACE),
        ] {
            catalog
                .edges
                .insert((from.into(), "blocks".into(), to.into()));
        }
        catalog.parents.insert(RECOVERY.into(), WORKSPACE.into());

        let mut issues: BTreeMap<String, std::sync::Arc<IssueState>> = [
            (SHELL, "done", "Desktop shell"),
            (CONNECT, "done", "Connect to a served World"),
            (WORLDS, "backlog", "Browse served Worlds"),
            (COMMANDS, "done", "Run local commands"),
            (WORKSPACE, "backlog", "Operate the Issue workspace"),
            (RECOVERY, "backlog", "Recover local state"),
        ]
        .into_iter()
        .map(|(doc, status, title)| {
            let mut value = (*issue(PROJECT, status)).clone();
            value.title = title.into();
            value.labels = vec!["lbl_local_first".into()];
            value.milestone = Some("mls_v1".into());
            (doc.into(), std::sync::Arc::new(value))
        })
        .collect();
        let aliases = crate::views::derive_aliases(&catalog, |doc| {
            issues.get(doc).map(|value| value.project.as_str())
        });

        let first = compile(
            &catalog,
            &aliases,
            &issues,
            PROJECT,
            &plan.roots,
            "11".repeat(32),
        );
        assert_eq!(first.nodes.len(), 6);
        assert_eq!(first.components.len(), 1);
        assert_eq!(
            first
                .nodes
                .iter()
                .find(|node| node.row.doc_id.as_str() == WORKSPACE)
                .map(|node| (node.layer, node.closure.as_str())),
            Some((Some(3), "blocked"))
        );
        let workspace_facets = &first
            .nodes
            .iter()
            .find(|node| node.row.doc_id.as_str() == WORKSPACE)
            .expect("workspace node")
            .facets;
        assert!(workspace_facets
            .iter()
            .any(|facet| facet.kind == "team" && facet.label == "Client"));
        assert!(workspace_facets
            .iter()
            .any(|facet| facet.kind == "label" && facet.label == "local-first"));
        assert!(workspace_facets
            .iter()
            .any(|facet| facet.kind == "milestone" && facet.label == "Client v1"));
        assert!(first.residuals.iter().any(|locus| {
            locus.kind == "blocked_frontier"
                && locus.at == vec![WORKSPACE.to_string()]
                && locus.requires == vec![WORLDS.to_string()]
        }));

        // Changing one canonical Issue fact moves the locus without revising
        // the Plan. Its generation changes; its roots and topology do not.
        std::sync::Arc::make_mut(issues.get_mut(WORLDS).expect("World browser")).status =
            "done".into();
        let second = compile(
            &catalog,
            &aliases,
            &issues,
            PROJECT,
            &plan.roots,
            "22".repeat(32),
        );
        assert_eq!(second.roots, first.roots);
        assert_eq!(second.edges, first.edges);
        assert_eq!(second.generation, "22".repeat(32));
        assert_eq!(
            second
                .nodes
                .iter()
                .find(|node| node.row.doc_id.as_str() == WORKSPACE)
                .map(|node| node.closure.as_str()),
            Some("ready")
        );
        assert!(!second.residuals.iter().any(|locus| {
            locus.kind == "blocked_frontier" && locus.at == vec![WORKSPACE.to_string()]
        }));
    }

    /// A release-mode gate for the largest ordinary project shape. Ignored in
    /// the default suite because it is a performance fixture, not a functional
    /// assertion; run it explicitly before changing the compiler.
    #[test]
    #[ignore = "50k-node release performance fixture"]
    fn fifty_thousand_issue_morphology_stays_bounded_and_linear_in_shape() {
        let project = "prj_01k1k8q6c6t0g0000000000000";
        let mut catalog = CatalogState::default();
        catalog.workflow = crate::dto::default_workflow();
        catalog.projects.insert(
            project.into(),
            ProjectMeta {
                name: "Large project".into(),
                key: "LARGE".into(),
                ..ProjectMeta::default()
            },
        );
        let docs: Vec<String> = (0..50_000u64)
            .map(|index| format!("iss_{index:026}"))
            .collect();
        let issues: BTreeMap<String, std::sync::Arc<IssueState>> = docs
            .iter()
            .map(|doc| (doc.clone(), issue(project, "backlog")))
            .collect();
        for pair in docs.windows(2) {
            catalog
                .edges
                .insert((pair[0].clone(), "blocks".into(), pair[1].clone()));
        }
        let aliases = crate::views::derive_aliases(&catalog, |doc| {
            issues.get(doc).map(|value| value.project.as_str())
        });
        let started = std::time::Instant::now();
        let view = compile(&catalog, &aliases, &issues, project, &[], "03".repeat(32));
        let elapsed = started.elapsed();
        assert_eq!(view.nodes.len(), 50_000);
        assert_eq!(view.edges.len(), 49_999);
        assert_eq!(view.components.len(), 1);
        assert_eq!(view.closure.total, 50_000);
        assert_eq!(view.closure.ready, 1);
        assert_eq!(view.closure.blocked, 49_999);
        if !cfg!(debug_assertions) {
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "50k compile took {elapsed:?}"
            );
        }
    }
}
