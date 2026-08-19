//! Bounded durable-change descriptions shared by every access path.
//!
//! A browser, CLI, controller, and agent all ultimately submit the same
//! [`SignedWorldAction`](crate::action::SignedWorldAction).  This module keeps
//! the corresponding live feedback on that same substrate: it summarizes the
//! exact LAIT Body operations without copying their values into a second state
//! stream. Consumers use exact stable locators where the substrate supplies
//! them, or invalidate a narrow resource and read the committed publication.
//! Local text splices are stabilized against the prepared candidate
//! publication before durability: the feedback carries both server-resolvable
//! Fabric anchors and the exact scalar offsets those anchors resolve to in the
//! stamped publication. Remote material without operation detail remains a
//! narrow dirty invalidation.
//!
//! Summaries are deliberately lossy under adversarial fan-out.  A Body with too
//! many operations becomes [`Detail::Dirty`] as one explicit unit; a partial
//! list must never masquerade as the whole change.

use std::collections::BTreeMap;

use mechanics::ids::{ActorId, DeviceId};
use replica::body::{BodyKey, Op};
use serde::{Deserialize, Serialize};

/// Maximum exact operation descriptions retained for one Body in one durable
/// change. The transaction itself may contain more; those changes remain fully
/// durable and the feedback record becomes a coarse invalidation.
pub const MAX_EXACT_CHANGES_PER_BODY: usize = 256;

/// Authenticated authorship of a locally submitted durable change.
///
/// `operation` is the signed action's persistent idempotency coordinate. The
/// actor and device come from the same verified header and are never accepted
/// as unsigned feedback metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribution {
    pub operation: [u8; 16],
    pub actor: ActorId,
    pub device: DeviceId,
}

/// A value-free operation class. Product payloads never ride the feedback ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mutation {
    Atomic,
    Register,
    Map,
    List,
    Text,
    Set,
    Counter,
    Lifecycle,
    Tree,
    Log,
}

/// A text range stabilized against the candidate publication that is about to
/// become durable.
///
/// `start`/`end` let a browser that does not embed the convergence engine draw
/// the immediate highlight. The anchors let Runtime re-resolve the same range
/// after later publications. A client applies the offsets only to the
/// Observation's exact [`WorldPublicationId`](crate::publication::WorldPublicationId);
/// a publication mismatch requires a fresh projection even when the portable
/// Manifest root happens to match across two local materializations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StableTextRange {
    /// Inclusive Unicode-scalar start in the committed publication.
    pub start: u64,
    /// Exclusive Unicode-scalar end in the committed publication.
    pub end: u64,
    /// Canonical Fabric anchor bytes for `start`.
    pub start_anchor: Vec<u8>,
    /// Canonical Fabric anchor bytes for `end`.
    pub end_anchor: Vec<u8>,
    /// Unicode scalars removed by the operation before later splices were
    /// transformed through it.
    pub deleted: u64,
    /// Unicode scalars inserted by the operation.
    pub inserted: u64,
    /// UTF-8 bytes inserted, for bounded client accounting.
    pub inserted_bytes: u64,
}

/// One exact, value-free operation description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathChange {
    pub mutation: Mutation,
    /// Collaborative root path. `None` means a whole-Body lifecycle or atomic
    /// replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Stable map key, list element, or tree node when the operation names one.
    /// New list/tree elements have no substrate identity until commit and leave
    /// this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<StableTextRange>,
}

/// Whether a Body's feedback is exact or intentionally coarse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Detail {
    Exact(Vec<PathChange>),
    Dirty,
}

/// The bounded change to one durable Body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyChange {
    pub body: BodyKey,
    pub detail: Detail,
}

/// One durable semantic change as observed after publication.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableChange {
    /// Absent for a reset, authority-only change, or remote material whose
    /// signed transaction attribution was not available at this seam.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<Attribution>,
    pub bodies: Vec<BodyChange>,
}

impl DurableChange {
    /// Summarize a verified local operation batch. Bodies are returned in
    /// canonical key order; operation order inside one Body is preserved.
    pub fn from_operations(attribution: Attribution, operations: &[(BodyKey, Op)]) -> Self {
        let mut by_body: BTreeMap<BodyKey, Detail> = BTreeMap::new();
        for (body, operation) in operations {
            let detail = by_body
                .entry(body.clone())
                .or_insert_with(|| Detail::Exact(Vec::new()));
            let Detail::Exact(changes) = detail else {
                continue;
            };
            if matches!(operation, Op::TextSplice { .. }) {
                // A scalar offset is meaningful only against the operation's
                // input version. Concurrent Fabric integration can move it in
                // the committed publication, so advertising it as exact would
                // move a viewer cursor or highlight to the wrong text. The
                // prepared-action path will eventually replace this with
                // publication-relative start/end anchors.
                *detail = Detail::Dirty;
                continue;
            }
            if changes.len() == MAX_EXACT_CHANGES_PER_BODY {
                *detail = Detail::Dirty;
                continue;
            }
            changes.push(summarize(operation));
        }
        Self {
            attribution: Some(attribution),
            bodies: by_body
                .into_iter()
                .map(|(body, detail)| BodyChange { body, detail })
                .collect(),
        }
    }

    /// Rebuild local feedback from the exact prepared candidate. Text
    /// operation-time offsets are transformed through every later splice on
    /// the same Body/path before anchors are minted. If any anchor cannot be
    /// minted or does not resolve back to the computed candidate offset, the
    /// complete Body is marked dirty; a partial exact list is never emitted.
    pub fn stabilize_prepared(
        &mut self,
        operations: &[(BodyKey, Op)],
        candidate: &replica::ReadSnapshot,
    ) {
        let Some(attribution) = self.attribution.clone() else {
            return;
        };
        *self = Self::from_prepared_operations(attribution, operations, candidate);
    }

    fn from_prepared_operations(
        attribution: Attribution,
        operations: &[(BodyKey, Op)],
        candidate: &replica::ReadSnapshot,
    ) -> Self {
        let mut by_body: BTreeMap<BodyKey, Detail> = BTreeMap::new();
        for (position, (body, operation)) in operations.iter().enumerate() {
            let detail = by_body
                .entry(body.clone())
                .or_insert_with(|| Detail::Exact(Vec::new()));
            let Detail::Exact(changes) = detail else {
                continue;
            };
            if changes.len() == MAX_EXACT_CHANGES_PER_BODY {
                *detail = Detail::Dirty;
                continue;
            }
            let change = match operation {
                Op::TextSplice {
                    path,
                    index,
                    delete,
                    insert,
                } => stable_text_change(
                    operations, position, body, path, *index, *delete, insert, candidate,
                ),
                _ => Some(summarize(operation)),
            };
            match change {
                Some(change) => changes.push(change),
                None => *detail = Detail::Dirty,
            }
        }
        Self {
            attribution: Some(attribution),
            bodies: by_body
                .into_iter()
                .map(|(body, detail)| BodyChange { body, detail })
                .collect(),
        }
    }

    /// Coarse but complete feedback for a change whose operation descriptions
    /// are unavailable (for example, an older remote transaction).
    pub fn dirty(bodies: impl IntoIterator<Item = BodyKey>) -> Self {
        let mut bodies: Vec<_> = bodies.into_iter().collect();
        bodies.sort();
        bodies.dedup();
        Self {
            attribution: None,
            bodies: bodies
                .into_iter()
                .map(|body| BodyChange {
                    body,
                    detail: Detail::Dirty,
                })
                .collect(),
        }
    }

    /// Ensure every semantically affected Body is represented. A World may
    /// name a coarse invalidation Body in addition to the Bodies with explicit
    /// operations; that extra scope is honestly dirty, never silently absent.
    pub fn cover_bodies(&mut self, bodies: impl IntoIterator<Item = BodyKey>) {
        let mut by_body: BTreeMap<BodyKey, Detail> = std::mem::take(&mut self.bodies)
            .into_iter()
            .map(|change| (change.body, change.detail))
            .collect();
        for body in bodies {
            by_body.entry(body).or_insert(Detail::Dirty);
        }
        self.bodies = by_body
            .into_iter()
            .map(|(body, detail)| BodyChange { body, detail })
            .collect();
    }
}

fn path(value: &str) -> Option<String> {
    Some(value.to_owned())
}

fn exact(
    mutation: Mutation,
    path: Option<String>,
    locator: Option<String>,
    text: Option<StableTextRange>,
) -> PathChange {
    PathChange {
        mutation,
        path,
        locator,
        text,
    }
}

#[derive(Clone, Copy)]
enum Affinity {
    Before,
    After,
}

fn map_position_through_splice(
    position: u64,
    index: u64,
    deleted: u64,
    inserted: u64,
    affinity: Affinity,
) -> u64 {
    let removed_end = index.saturating_add(deleted);
    if position < index {
        return position;
    }
    if position > removed_end {
        return position.saturating_sub(deleted).saturating_add(inserted);
    }
    if deleted == 0 && position == index {
        return match affinity {
            Affinity::Before => index,
            Affinity::After => index.saturating_add(inserted),
        };
    }
    if position == removed_end {
        return index.saturating_add(inserted);
    }
    match affinity {
        Affinity::Before => index,
        Affinity::After => index.saturating_add(inserted),
    }
}

#[allow(clippy::too_many_arguments)]
fn stable_text_change(
    operations: &[(BodyKey, Op)],
    position: usize,
    body: &BodyKey,
    path: &str,
    index: u64,
    deleted: u64,
    insert: &str,
    candidate: &replica::ReadSnapshot,
) -> Option<PathChange> {
    let inserted = u64::try_from(insert.chars().count()).ok()?;
    let mut start = index;
    let mut end = index.checked_add(inserted)?;
    for (later_body, later) in operations.get(position.saturating_add(1)..)? {
        let Op::TextSplice {
            path: later_path,
            index: later_index,
            delete: later_deleted,
            insert: later_insert,
        } = later
        else {
            continue;
        };
        if later_body != body || later_path != path {
            continue;
        }
        let later_inserted = u64::try_from(later_insert.chars().count()).ok()?;
        start = map_position_through_splice(
            start,
            *later_index,
            *later_deleted,
            later_inserted,
            Affinity::After,
        );
        end = map_position_through_splice(
            end,
            *later_index,
            *later_deleted,
            later_inserted,
            Affinity::Before,
        );
        if end < start {
            end = start;
        }
    }
    let start_anchor = candidate.anchor(body, path, start)?;
    let end_anchor = candidate.anchor(body, path, end)?;
    if candidate.resolve_anchor(body, &start_anchor) != fabric::AnchorResolution::Resolved(start)
        || candidate.resolve_anchor(body, &end_anchor) != fabric::AnchorResolution::Resolved(end)
    {
        return None;
    }
    Some(exact(
        Mutation::Text,
        Some(path.to_owned()),
        None,
        Some(StableTextRange {
            start,
            end,
            start_anchor: start_anchor.encode(),
            end_anchor: end_anchor.encode(),
            deleted,
            inserted,
            inserted_bytes: u64::try_from(insert.len()).ok()?,
        }),
    ))
}

fn summarize(operation: &Op) -> PathChange {
    match operation {
        Op::ReplaceAtomic { .. } => exact(Mutation::Atomic, None, None, None),
        Op::RegisterSet { path: root, .. } | Op::RegisterClear { path: root } => {
            exact(Mutation::Register, path(root), None, None)
        }
        Op::MapSet {
            path: root, key, ..
        }
        | Op::MapRemove { path: root, key } => {
            exact(Mutation::Map, path(root), Some(key.clone()), None)
        }
        Op::ListInsert {
            path: root, index, ..
        } => exact(
            Mutation::List,
            path(root),
            Some(format!("index:{index}")),
            None,
        ),
        Op::ListRemove {
            path: root,
            element,
        }
        | Op::ListMove {
            path: root,
            element,
            ..
        } => exact(Mutation::List, path(root), Some(element.clone()), None),
        // `from_operations` downgrades the complete Body before reaching this
        // branch; retain a total helper without ever promising an exact range.
        Op::TextSplice { path: root, .. } => exact(Mutation::Text, path(root), None, None),
        Op::SetAdd { path: root, .. } | Op::SetRemove { path: root, .. } => {
            exact(Mutation::Set, path(root), None, None)
        }
        Op::CounterAdd { path: root, .. } => exact(Mutation::Counter, path(root), None, None),
        Op::Create | Op::Tombstone => exact(Mutation::Lifecycle, None, None, None),
        Op::TreeInsert { path: root, .. } => exact(Mutation::Tree, path(root), None, None),
        Op::TreeMove {
            path: root, node, ..
        }
        | Op::TreeRemove {
            path: root, node, ..
        }
        | Op::TreeSet {
            path: root, node, ..
        }
        | Op::TreeUnset {
            path: root, node, ..
        } => exact(Mutation::Tree, path(root), Some(node.clone()), None),
        Op::TreeAnchor {
            path: root, anchor, ..
        } => exact(Mutation::Tree, path(root), Some(anchor.clone()), None),
        Op::LogAppend { path: root, .. } => exact(Mutation::Log, path(root), None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::body::{BodyId, WorldId};

    fn body(raw: u8) -> BodyKey {
        BodyKey::new(
            WorldId::parse("com.example.changes").unwrap(),
            BodyId::from_bytes([raw; 16]),
        )
    }

    fn attribution() -> Attribution {
        Attribution {
            operation: [9; 16],
            actor: ActorId::from_incept_hash(&"a".repeat(64)),
            device: mechanics::actor::device_from_seed(&[7; 32]),
        }
    }

    #[test]
    fn summaries_are_value_free_and_positional_text_is_explicitly_dirty() {
        let change = DurableChange::from_operations(
            attribution(),
            &[
                (
                    body(2),
                    Op::RegisterSet {
                        path: "title".into(),
                        value: b"secret title".to_vec(),
                    },
                ),
                (
                    body(2),
                    Op::TextSplice {
                        path: "description".into(),
                        index: 4,
                        delete: 2,
                        insert: "á🦀".into(),
                    },
                ),
            ],
        );
        let encoded = postcard::to_stdvec(&change).unwrap();
        assert!(!encoded.windows(6).any(|window| window == b"secret"));
        assert_eq!(change.bodies[0].detail, Detail::Dirty);
    }

    #[test]
    fn too_many_operations_become_one_explicit_dirty_body() {
        let operations: Vec<_> = (0..=MAX_EXACT_CHANGES_PER_BODY)
            .map(|index| {
                (
                    body(1),
                    Op::ListInsert {
                        path: "items".into(),
                        index: u64::try_from(index).unwrap(),
                        value: Vec::new(),
                    },
                )
            })
            .collect();
        let change = DurableChange::from_operations(attribution(), &operations);
        assert_eq!(change.bodies[0].detail, Detail::Dirty);
    }

    #[test]
    fn dirty_remote_body_sets_are_canonical_and_deduplicated() {
        let change = DurableChange::dirty([body(2), body(1), body(2)]);
        assert_eq!(change.bodies.len(), 2);
        assert_eq!(change.bodies[0].body, body(1));
        assert!(change
            .bodies
            .iter()
            .all(|body| body.detail == Detail::Dirty));
    }
}
