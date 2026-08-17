//! Gate-first bounded evaluation over one immutable read corpus.
//!
//! This module owns no authority facts. Its caller supplies the exact granted
//! Gate set for the request's authority frontier. A denied node, field,
//! feature, or edge is removed before it can affect traversal, filtering,
//! scoring, counts, packing, or result order.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};

use replica::body::BodyKey;
use serde::{Deserialize, Serialize};

use crate::{
    corpus::Corpus,
    find::{
        self, Atom, Bound, Direction, EdgeRef, Emit, ExtractedField, ExtractedNode, FeatureRef,
        FieldRange, FieldRef, GateRef, Keep, MergeMethod, MissingFeature, Mode, NodeKey, Op, Pack,
        Predicate, Query, RangeEndpoint, RankBy, Seek, Step, StepId, Term, Test, Unique, Value,
        Walk, WalkOrder,
    },
};

/// Authority-derived gates admitted for exactly one evaluation.
#[derive(Debug, Clone, Default)]
pub(crate) struct GrantedGates(BTreeSet<GateRef>);

impl GrantedGates {
    pub fn new(gates: impl IntoIterator<Item = GateRef>) -> Self {
        Self(gates.into_iter().collect())
    }

    fn allows(&self, gate: &Option<GateRef>) -> bool {
        gate.as_ref().is_none_or(|gate| self.0.contains(gate))
    }

    fn allows_ref(&self, gate: Option<&GateRef>) -> bool {
        gate.is_none_or(|gate| self.0.contains(gate))
    }

    fn contains(&self, gate: &GateRef) -> bool {
        self.0.contains(gate)
    }
}

/// Exact feature implementation used by an augmented query.
///
/// Runtime never invents a fallback score. If a query needs this callback and
/// none is installed, evaluation fails with [`Failure::FeatureUnavailable`].
pub(crate) trait FeatureScorer {
    fn score(
        &self,
        feature: &FeatureRef,
        stored: &[u8],
        probe: &[u8],
    ) -> Result<FeatureScore, FeatureFailure>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeatureScore {
    /// Greater relevance sorts first.
    pub relevance: i64,
    /// Smaller distance sorts first.
    pub distance: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeatureFailure(pub &'static str);

/// Exact token accounting for packed material.
pub(crate) trait TokenCounter {
    fn count(&self, bytes: &[u8]) -> Result<u64, &'static str>;
}

pub(crate) struct Evaluation<'a> {
    pub query: &'a Query,
    pub corpus: &'a Corpus,
    pub gates: &'a GrantedGates,
    pub admitted_bound: Bound,
    pub cursor_position: Option<Vec<u8>>,
    pub feature_scorer: Option<&'a dyn FeatureScorer>,
    pub token_counter: &'a dyn TokenCounter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dimension {
    PostingsRead,
    EdgesVisited,
    NodesVisited,
    PathsRetained,
    CandidatesPerBranch,
    ScoreEvaluations,
    ProjectedBytes,
    PackedTokens,
    WallMillis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    Invalid(find::Invalid),
    BoundExceeded(Dimension),
    MissingStep(StepId),
    WrongFlow(&'static str),
    ContinuationUnavailable,
    FeatureUnavailable(FeatureRef),
    DistanceUnavailable,
    FeatureFailed(FeatureRef, FeatureFailure),
    TokenCounting(&'static str),
}

impl From<find::Invalid> for Failure {
    fn from(value: find::Invalid) -> Self {
        Self::Invalid(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeHit {
    pub key: NodeKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PathHop {
    pub edge: EdgeRef,
    pub from: NodeKey,
    pub to: NodeKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathHit {
    pub nodes: Vec<NodeKey>,
    pub hops: Vec<PathHop>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RankedHit {
    pub key: NodeKey,
    pub path: Option<PathHit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackedField {
    pub reference: FieldRef,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackedNode {
    pub key: NodeKey,
    pub fields: Vec<PackedField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Output {
    Nodes(Vec<NodeHit>),
    Paths(Vec<PathHit>),
    Ranked(Vec<RankedHit>),
    Context(Vec<PackedNode>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    pub output: Output,
    pub usage: Bound,
    pub next_position: Option<Vec<u8>>,
    pub matched_total: Option<u64>,
}

const PAGE_POSITION_VERSION: u8 = 1;

/// The first not-yet-returned admitted candidate. Storing the look-ahead row,
/// rather than the last emitted row, lets resume use an inclusive B+tree seek
/// without duplicates and without rescanning hidden suffixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PagePosition {
    Schema { key: NodeKey },
    Field { value: Value, key: NodeKey },
    Term { key: NodeKey },
    Body { body: u32, row: u32 },
    Id { next: u32 },
}

impl PagePosition {
    fn encode(&self) -> Result<Vec<u8>, Failure> {
        postcard::to_stdvec(&(PAGE_POSITION_VERSION, self))
            .map_err(|_| Failure::Invalid(find::Invalid::InvalidCursor))
    }

    fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let (version, position): (u8, Self) = postcard::from_bytes(bytes)
            .map_err(|_| Failure::Invalid(find::Invalid::InvalidCursor))?;
        if version != PAGE_POSITION_VERSION
            || postcard::to_stdvec(&(version, &position))
                .map_err(|_| Failure::Invalid(find::Invalid::InvalidCursor))?
                != bytes
        {
            return Err(Failure::Invalid(find::Invalid::InvalidCursor));
        }
        Ok(position)
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    key: NodeKey,
    path: Option<PathHit>,
    term_scores: BTreeMap<FieldRef, u64>,
    feature_scores: BTreeMap<FeatureRef, FeatureScore>,
    merge_score: i128,
    order: Vec<RankValue>,
}

impl Candidate {
    fn node(key: NodeKey) -> Self {
        Self {
            key,
            path: None,
            term_scores: BTreeMap::new(),
            feature_scores: BTreeMap::new(),
            merge_score: 0,
            order: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
enum Flow {
    Nodes(Vec<Candidate>),
    Paths(Vec<Candidate>),
    Ranked(Vec<Candidate>),
    Context(Vec<PackedNode>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RankValue {
    Value(Value),
    Descending(u64),
    DescendingSigned(i64),
    Ascending(u64),
    Missing,
}

impl Ord for RankValue {
    fn cmp(&self, other: &Self) -> Ordering {
        use RankValue::{Ascending, Descending, DescendingSigned, Missing, Value};
        match (self, other) {
            (Value(left), Value(right)) => left.cmp(right),
            (Descending(left), Descending(right)) => right.cmp(left),
            (DescendingSigned(left), DescendingSigned(right)) => right.cmp(left),
            (Ascending(left), Ascending(right)) => left.cmp(right),
            (Missing, Missing) => Ordering::Equal,
            (Missing, _) => Ordering::Greater,
            (_, Missing) => Ordering::Less,
            (Value(_), _) => Ordering::Less,
            (_, Value(_)) => Ordering::Greater,
            (Descending(_), _) => Ordering::Less,
            (_, Descending(_)) => Ordering::Greater,
            (DescendingSigned(_), _) => Ordering::Less,
            (_, DescendingSigned(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for RankValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn evaluate(request: Evaluation<'_>) -> Result<Answer, Failure> {
    request.query.validate()?;
    if let Some(answer) = evaluate_linear_page(&request)? {
        return Ok(answer);
    }
    let limit = request.admitted_bound.intersection(request.query.bound);
    let mut evaluator = Evaluator {
        corpus: request.corpus,
        gates: request.gates,
        feature_scorer: request.feature_scorer,
        token_counter: request.token_counter,
        meter: Meter::new(limit),
    };
    let mut flows = BTreeMap::new();
    for step in &request.query.steps {
        evaluator.meter.start_step(step.bound);
        let inputs = step
            .input
            .iter()
            .map(|id| flows.get(id).cloned().ok_or(Failure::MissingStep(*id)))
            .collect::<Result<Vec<_>, _>>()?;
        let flow = evaluator.step(request.query, step, inputs)?;
        evaluator.meter.finish_step()?;
        flows.insert(step.id, flow);
    }
    let flow = flows
        .remove(&request.query.output)
        .ok_or(Failure::MissingStep(request.query.output))?;
    evaluator.meter.finish()?;
    let output = into_output(flow);
    let rows = output_len(&output);
    if rows > request.query.page_size as usize {
        return Err(Failure::ContinuationUnavailable);
    }
    Ok(Answer {
        output,
        usage: evaluator.meter.usage,
        next_position: None,
        matched_total: None,
    })
}

struct LinearPlan<'a> {
    seek: &'a Seek,
    keeps: Vec<&'a Keep>,
    pack: Option<&'a Pack>,
}

fn linear_plan(query: &Query) -> Option<LinearPlan<'_>> {
    if query.steps.last()?.id != query.output {
        return None;
    }
    let Op::Seek(seek) = &query.steps.first()?.op else {
        return None;
    };
    if !matches!(
        seek,
        Seek::Source
            | Seek::Bodies(_)
            | Seek::Ids(_)
            | Seek::Field(_)
            | Seek::FieldRange(_)
            | Seek::Term {
                kind: Term::Token,
                ..
            }
    ) {
        return None;
    }
    let mut keeps = Vec::new();
    let mut pack = None;
    for (offset, step) in query.steps.iter().enumerate().skip(1) {
        if step.input.as_slice() != [query.steps[offset - 1].id] {
            return None;
        }
        match &step.op {
            Op::Keep(keep) if pack.is_none() => keeps.push(keep),
            Op::Pack(next_pack) if pack.is_none() && offset + 1 == query.steps.len() => {
                pack = Some(next_pack);
            }
            Op::Seek(_) | Op::Keep(_) | Op::Walk(_) | Op::Rank(_) | Op::Merge(_) | Op::Pack(_) => {
                return None;
            }
        }
    }
    Some(LinearPlan { seek, keeps, pack })
}

fn evaluate_linear_page(request: &Evaluation<'_>) -> Result<Option<Answer>, Failure> {
    let Some(plan) = linear_plan(request.query) else {
        if request.cursor_position.is_some() {
            return Err(Failure::ContinuationUnavailable);
        }
        return Ok(None);
    };
    let resume = request
        .cursor_position
        .as_deref()
        .map(PagePosition::decode)
        .transpose()?;
    let mut limit = request.admitted_bound.intersection(request.query.bound);
    for step in &request.query.steps {
        limit = limit.intersection(step.bound);
    }
    let mut evaluator = Evaluator {
        corpus: request.corpus,
        gates: request.gates,
        feature_scorer: request.feature_scorer,
        token_counter: request.token_counter,
        meter: Meter::new(limit),
    };
    let page_size = request.query.page_size as usize;
    let mut candidates = Vec::with_capacity(page_size);
    let mut next = None;
    let mut failure = None;
    let corpus = evaluator.corpus;
    let gates = evaluator.gates.clone();
    let matched_total = if plan.keeps.is_empty() {
        match plan.seek {
            Seek::Source => {
                Some(corpus.count_schema(&request.query.schema, |gate| gates.allows_ref(gate)))
            }
            Seek::Field(predicate) if predicate.test == Test::Equal => Some(corpus.count_exact(
                &predicate.field,
                &atom_value(&predicate.value),
                |gate| gates.allows_ref(gate),
            )),
            Seek::Term {
                field,
                text,
                kind: Term::Token,
            } => Some(corpus.count_term(field, text.as_bytes(), |gate| gates.allows_ref(gate))),
            _ => None,
        }
    } else {
        None
    }
    .map(usize_u64);

    match plan.seek {
        Seek::Source => {
            let start = match resume {
                None => None,
                Some(PagePosition::Schema { key }) => Some(key),
                Some(_) => return Err(Failure::Invalid(find::Invalid::InvalidCursor)),
            };
            corpus.scan_schema(
                &request.query.schema,
                start,
                |gate| gates.allows_ref(gate),
                |key, body, row| {
                    match evaluator.page_allowed(body, row, None, &plan.keeps) {
                        Ok(true) => {}
                        Ok(false) => return true,
                        Err(error) => {
                            failure = Some(error);
                            return false;
                        }
                    }
                    if candidates.len() == page_size {
                        next = Some(PagePosition::Schema { key });
                        return false;
                    }
                    candidates.push(Candidate::node(row.key.clone()));
                    if let Err(error) = evaluator.observe_candidates(candidates.len()) {
                        failure = Some(error);
                        return false;
                    }
                    true
                },
            );
        }
        Seek::Field(predicate) => {
            let start = match resume {
                None => None,
                Some(PagePosition::Field { value, key }) => Some((value, key)),
                Some(_) => return Err(Failure::Invalid(find::Invalid::InvalidCursor)),
            };
            let expected = atom_value(&predicate.value);
            corpus.scan_field_range(
                &predicate.field,
                predicate.test,
                &expected,
                start,
                |gate| gates.allows_ref(gate),
                |value, key, body, row| {
                    match evaluator.page_allowed(body, row, Some(predicate), &plan.keeps) {
                        Ok(true) => {}
                        Ok(false) => return true,
                        Err(error) => {
                            failure = Some(error);
                            return false;
                        }
                    }
                    if candidates.len() == page_size {
                        next = Some(PagePosition::Field { value, key });
                        return false;
                    }
                    candidates.push(Candidate::node(row.key.clone()));
                    if let Err(error) = evaluator.observe_candidates(candidates.len()) {
                        failure = Some(error);
                        return false;
                    }
                    true
                },
            );
        }
        Seek::FieldRange(range) => {
            let start = match resume {
                None => None,
                Some(PagePosition::Field { value, key }) => Some((value, key)),
                Some(_) => return Err(Failure::Invalid(find::Invalid::InvalidCursor)),
            };
            let lower = atom_value(range.lower.atom());
            let upper = atom_value(range.upper.atom());
            let lower_bound = match &range.lower {
                RangeEndpoint::Inclusive(_) => std::ops::Bound::Included(&lower),
                RangeEndpoint::Exclusive(_) => std::ops::Bound::Excluded(&lower),
            };
            let upper_bound = match &range.upper {
                RangeEndpoint::Inclusive(_) => std::ops::Bound::Included(&upper),
                RangeEndpoint::Exclusive(_) => std::ops::Bound::Excluded(&upper),
            };
            corpus.scan_field_interval(
                &range.field,
                lower_bound,
                upper_bound,
                start,
                |gate| gates.allows_ref(gate),
                |value, key, body, row| {
                    match evaluator.page_allowed(body, row, None, &plan.keeps) {
                        Ok(true) => {}
                        Ok(false) => return true,
                        Err(error) => {
                            failure = Some(error);
                            return false;
                        }
                    }
                    if candidates.len() == page_size {
                        next = Some(PagePosition::Field { value, key });
                        return false;
                    }
                    candidates.push(Candidate::node(row.key.clone()));
                    if let Err(error) = evaluator.observe_candidates(candidates.len()) {
                        failure = Some(error);
                        return false;
                    }
                    true
                },
            );
        }
        Seek::Bodies(bodies) => {
            let (start_body, start_row) = match resume {
                None => (0usize, 0u32),
                Some(PagePosition::Body { body, row }) => (
                    usize::try_from(body)
                        .map_err(|_| Failure::Invalid(find::Invalid::InvalidCursor))?,
                    row,
                ),
                Some(_) => return Err(Failure::Invalid(find::Invalid::InvalidCursor)),
            };
            'bodies: for (body_offset, body) in bodies.iter().enumerate().skip(start_body) {
                let row = if body_offset == start_body {
                    start_row
                } else {
                    0
                };
                corpus.scan_body(
                    body,
                    &request.query.schema,
                    row,
                    |gate| gates.allows_ref(gate),
                    |row, source, node| {
                        match evaluator.page_allowed(source, node, None, &plan.keeps) {
                            Ok(true) => {}
                            Ok(false) => return true,
                            Err(error) => {
                                failure = Some(error);
                                return false;
                            }
                        }
                        if candidates.len() == page_size {
                            next = Some(PagePosition::Body {
                                body: u32::try_from(body_offset).unwrap_or(u32::MAX),
                                row,
                            });
                            return false;
                        }
                        candidates.push(Candidate::node(node.key.clone()));
                        if let Err(error) = evaluator.observe_candidates(candidates.len()) {
                            failure = Some(error);
                            return false;
                        }
                        true
                    },
                );
                if failure.is_some() || next.is_some() {
                    break 'bodies;
                }
            }
        }
        Seek::Ids(ids) => {
            let start = match resume {
                None => 0usize,
                Some(PagePosition::Id { next }) => usize::try_from(next)
                    .map_err(|_| Failure::Invalid(find::Invalid::InvalidCursor))?,
                Some(_) => return Err(Failure::Invalid(find::Invalid::InvalidCursor)),
            };
            for (offset, id) in ids.iter().enumerate().skip(start) {
                let key = NodeKey {
                    schema: request.query.schema.clone(),
                    node: id.clone(),
                };
                let Some(row) = evaluator.read_node(&key)? else {
                    continue;
                };
                if !evaluator.node_allowed(&row)
                    || plan
                        .keeps
                        .iter()
                        .flat_map(|keep| &keep.predicates)
                        .any(|predicate| !evaluator.row_matches(&row, predicate))
                {
                    continue;
                }
                if candidates.len() == page_size {
                    next = Some(PagePosition::Id {
                        next: u32::try_from(offset).unwrap_or(u32::MAX),
                    });
                    break;
                }
                candidates.push(Candidate::node(key));
                evaluator.observe_candidates(candidates.len())?;
            }
        }
        Seek::Term { field, text, kind } => {
            let start = match resume {
                None => None,
                Some(PagePosition::Term { key }) => Some(key),
                Some(_) => return Err(Failure::Invalid(find::Invalid::InvalidCursor)),
            };
            corpus.scan_term(
                field,
                text.as_bytes(),
                *kind == Term::Prefix,
                start,
                |gate| gates.allows_ref(gate),
                |key, body, row, frequency| {
                    match evaluator.page_allowed(body, row, None, &plan.keeps) {
                        Ok(true) => {}
                        Ok(false) => return true,
                        Err(error) => {
                            failure = Some(error);
                            return false;
                        }
                    }
                    if candidates.len() == page_size {
                        next = Some(PagePosition::Term { key });
                        return false;
                    }
                    let mut candidate = Candidate::node(row.key.clone());
                    candidate
                        .term_scores
                        .insert(field.clone(), u64::from(frequency));
                    candidates.push(candidate);
                    if let Err(error) = evaluator.observe_candidates(candidates.len()) {
                        failure = Some(error);
                        return false;
                    }
                    true
                },
            );
        }
        Seek::Feature { .. } => return Ok(None),
    }
    if let Some(error) = failure {
        return Err(error);
    }
    let flow = if let Some(pack) = plan.pack {
        evaluator.pack(&candidates, &pack.fields)?
    } else if matches!(plan.seek, Seek::Term { .. }) {
        Flow::Ranked(candidates)
    } else {
        Flow::Nodes(candidates)
    };
    evaluator.meter.finish()?;
    Ok(Some(Answer {
        output: into_output(flow),
        usage: evaluator.meter.usage,
        next_position: next.map(|position| position.encode()).transpose()?,
        matched_total,
    }))
}

struct Evaluator<'a> {
    corpus: &'a Corpus,
    gates: &'a GrantedGates,
    feature_scorer: Option<&'a dyn FeatureScorer>,
    token_counter: &'a dyn TokenCounter,
    meter: Meter,
}

impl Evaluator<'_> {
    fn page_allowed(
        &mut self,
        body: &BodyKey,
        row: &ExtractedNode,
        seek: Option<&Predicate>,
        keeps: &[&Keep],
    ) -> Result<bool, Failure> {
        if !self.node_allowed(row) {
            return Ok(false);
        }
        self.visit_posting_row(body, row)?;
        Ok(
            seek.is_none_or(|predicate| self.row_matches(row, predicate))
                && keeps
                    .iter()
                    .flat_map(|keep| &keep.predicates)
                    .all(|predicate| self.row_matches(row, predicate)),
        )
    }

    fn step(&mut self, query: &Query, step: &Step, inputs: Vec<Flow>) -> Result<Flow, Failure> {
        match &step.op {
            Op::Seek(seek) => {
                if !inputs.is_empty() {
                    return Err(Failure::WrongFlow("Seek input"));
                }
                self.seek(query, seek)
            }
            Op::Keep(keep) => {
                let [input] = inputs.as_slice() else {
                    return Err(Failure::WrongFlow("Keep input"));
                };
                self.keep(input.clone(), keep)
            }
            Op::Walk(walk) => {
                let [Flow::Nodes(input)] = inputs.as_slice() else {
                    return Err(Failure::WrongFlow("Walk input"));
                };
                self.walk(input, walk)
            }
            Op::Rank(rank) => {
                let [input] = inputs.as_slice() else {
                    return Err(Failure::WrongFlow("Rank input"));
                };
                self.rank(query.mode, input.clone(), &rank.by)
            }
            Op::Merge(merge) => self.merge(inputs, merge.method),
            Op::Pack(pack) => {
                let [input] = inputs.as_slice() else {
                    return Err(Failure::WrongFlow("Pack input"));
                };
                match input {
                    Flow::Nodes(input) | Flow::Ranked(input) => self.pack(input, &pack.fields),
                    Flow::Paths(_) | Flow::Context(_) => Err(Failure::WrongFlow("Pack input")),
                }
            }
        }
    }

    fn seek(&mut self, query: &Query, seek: &Seek) -> Result<Flow, Failure> {
        match seek {
            Seek::Source => {
                let candidates = self.visit_schema(&query.schema)?;
                Ok(Flow::Nodes(candidates))
            }
            Seek::Bodies(bodies) => {
                let candidates = self.visit_bodies(&query.schema, bodies)?;
                Ok(Flow::Nodes(candidates))
            }
            Seek::Ids(ids) => {
                let mut candidates = Vec::new();
                for id in ids {
                    let key = NodeKey {
                        schema: query.schema.clone(),
                        node: id.clone(),
                    };
                    if let Some(row) = self.read_node(&key)? {
                        if self.node_allowed(&row) {
                            candidates.push(Candidate::node(key));
                            self.observe_candidates(candidates.len())?;
                        }
                    }
                }
                canonical_candidates(&mut candidates);
                Ok(Flow::Nodes(candidates))
            }
            Seek::Field(predicate) => {
                let candidates = self.visit_field(predicate)?;
                Ok(Flow::Nodes(candidates))
            }
            Seek::FieldRange(range) => {
                let candidates = self.visit_field_interval(range)?;
                Ok(Flow::Nodes(candidates))
            }
            Seek::Term { field, text, kind } => {
                let candidates = self.visit_term(field, text.as_bytes(), *kind)?;
                Ok(Flow::Ranked(candidates))
            }
            Seek::Feature { feature, probe } => {
                let candidates = self.visit_feature(query.mode, feature, probe)?;
                Ok(Flow::Ranked(candidates))
            }
        }
    }

    fn visit_schema(&mut self, schema: &find::SchemaRef) -> Result<Vec<Candidate>, Failure> {
        let mut candidates = Vec::new();
        let mut failure = None;
        let gates = self.gates;
        self.corpus.visit_schema(
            schema,
            usize::MAX,
            |gate| gates.allows_ref(gate),
            |body, row| {
                if !self.node_allowed(row) {
                    return true;
                }
                if let Err(error) = self.visit_posting_row(body, row) {
                    failure = Some(error);
                    return false;
                }
                candidates.push(Candidate::node(row.key.clone()));
                if let Err(error) = self.observe_candidates(candidates.len()) {
                    failure = Some(error);
                    return false;
                }
                true
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        canonical_candidates(&mut candidates);
        Ok(candidates)
    }

    fn visit_bodies(
        &mut self,
        schema: &find::SchemaRef,
        bodies: &[BodyKey],
    ) -> Result<Vec<Candidate>, Failure> {
        let mut candidates = Vec::new();
        for body in bodies {
            let mut failure = None;
            let gates = self.gates;
            self.corpus.visit_body(
                body,
                schema,
                usize::MAX,
                |gate| gates.allows_ref(gate),
                |source, row| {
                    if !self.node_allowed(row) {
                        return true;
                    }
                    if let Err(error) = self.visit_posting_row(source, row) {
                        failure = Some(error);
                        return false;
                    }
                    candidates.push(Candidate::node(row.key.clone()));
                    if let Err(error) = self.observe_candidates(candidates.len()) {
                        failure = Some(error);
                        return false;
                    }
                    true
                },
            );
            if let Some(error) = failure {
                return Err(error);
            }
        }
        canonical_candidates(&mut candidates);
        Ok(candidates)
    }

    fn visit_field(&mut self, predicate: &Predicate) -> Result<Vec<Candidate>, Failure> {
        let exact = atom_value(&predicate.value);
        let corpus = self.corpus;
        let gates = self.gates;
        let mut candidates = Vec::new();
        let mut failure = None;
        let mut accept = |body: &BodyKey, row: &ExtractedNode| {
            if !self.node_allowed(row) {
                return true;
            }
            if let Err(error) = self.visit_posting_row(body, row) {
                failure = Some(error);
                return false;
            }
            if row.fields.iter().any(|field| {
                self.field_allowed(field)
                    && field.reference == predicate.field
                    && predicate_matches(&field.value, predicate.test, &exact)
            }) {
                candidates.push(Candidate::node(row.key.clone()));
                if let Err(error) = self.observe_candidates(candidates.len()) {
                    failure = Some(error);
                    return false;
                }
            }
            true
        };
        if predicate.test == Test::Equal {
            corpus.visit_exact(
                &predicate.field,
                &exact,
                usize::MAX,
                |gate| gates.allows_ref(gate),
                &mut accept,
            );
        } else {
            corpus.visit_field_range(
                &predicate.field,
                predicate.test,
                &exact,
                usize::MAX,
                |gate| gates.allows_ref(gate),
                &mut accept,
            );
        }
        if let Some(error) = failure {
            return Err(error);
        }
        canonical_candidates(&mut candidates);
        Ok(candidates)
    }

    fn visit_field_interval(&mut self, range: &FieldRange) -> Result<Vec<Candidate>, Failure> {
        let lower = atom_value(range.lower.atom());
        let upper = atom_value(range.upper.atom());
        let lower_bound = match &range.lower {
            RangeEndpoint::Inclusive(_) => std::ops::Bound::Included(&lower),
            RangeEndpoint::Exclusive(_) => std::ops::Bound::Excluded(&lower),
        };
        let upper_bound = match &range.upper {
            RangeEndpoint::Inclusive(_) => std::ops::Bound::Included(&upper),
            RangeEndpoint::Exclusive(_) => std::ops::Bound::Excluded(&upper),
        };
        let corpus = self.corpus;
        let gates = self.gates;
        let mut candidates = Vec::new();
        let mut failure = None;
        corpus.scan_field_interval(
            &range.field,
            lower_bound,
            upper_bound,
            None,
            |gate| gates.allows_ref(gate),
            |_, _, body, row| {
                if !self.node_allowed(row) {
                    return true;
                }
                if let Err(error) = self.visit_posting_row(body, row) {
                    failure = Some(error);
                    return false;
                }
                candidates.push(Candidate::node(row.key.clone()));
                if let Err(error) = self.observe_candidates(candidates.len()) {
                    failure = Some(error);
                    return false;
                }
                true
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        canonical_candidates(&mut candidates);
        Ok(candidates)
    }

    fn visit_term(
        &mut self,
        field: &FieldRef,
        probe: &[u8],
        kind: Term,
    ) -> Result<Vec<Candidate>, Failure> {
        let corpus = self.corpus;
        let gates = self.gates;
        let mut candidates = Vec::new();
        let mut failure = None;
        let mut accept = |body: &BodyKey, row: &ExtractedNode, frequency: u32| {
            if !self.node_allowed(row) {
                return true;
            }
            if let Err(error) = self.visit_posting_row(body, row) {
                failure = Some(error);
                return false;
            }
            if frequency != 0 {
                let mut candidate = Candidate::node(row.key.clone());
                candidate
                    .term_scores
                    .insert(field.clone(), u64::from(frequency));
                candidates.push(candidate);
                if let Err(error) = self.observe_candidates(candidates.len()) {
                    failure = Some(error);
                    return false;
                }
            }
            true
        };
        corpus.visit_term(
            field,
            probe,
            kind == Term::Prefix,
            usize::MAX,
            |gate| gates.allows_ref(gate),
            &mut accept,
        );
        if let Some(error) = failure {
            return Err(error);
        }
        candidates.sort_by(candidate_term_order);
        Ok(candidates)
    }

    fn visit_feature(
        &mut self,
        mode: Mode,
        feature: &FeatureRef,
        probe: &[u8],
    ) -> Result<Vec<Candidate>, Failure> {
        let scorer = self
            .feature_scorer
            .ok_or_else(|| Failure::FeatureUnavailable(feature.clone()))?;
        let mut candidates = Vec::new();
        let mut failure = None;
        let gates = self.gates;
        self.corpus.visit_feature(
            feature,
            usize::MAX,
            |gate| gates.allows_ref(gate),
            |body, row| {
                if !self.node_allowed(row) {
                    return true;
                }
                if let Err(error) = self.visit_posting_row(body, row) {
                    failure = Some(error);
                    return false;
                }
                let Some(stored) = row
                    .features
                    .iter()
                    .find(|stored| stored.reference == *feature && self.gates.allows(&stored.gate))
                else {
                    return true;
                };
                if let Err(error) = self.meter.charge(Dimension::ScoreEvaluations, 1) {
                    failure = Some(error);
                    return false;
                }
                match scorer.score(feature, &stored.value, probe) {
                    Ok(score) => {
                        let mut candidate = Candidate::node(row.key.clone());
                        candidate.feature_scores.insert(feature.clone(), score);
                        candidates.push(candidate);
                        if let Err(error) = self.observe_candidates(candidates.len()) {
                            failure = Some(error);
                            return false;
                        }
                    }
                    Err(error) => match mode {
                        Mode::Augmented {
                            missing: MissingFeature::Drop | MissingFeature::Continue,
                        } => {}
                        Mode::Exact
                        | Mode::Augmented {
                            missing: MissingFeature::Refuse,
                        } => {
                            failure = Some(Failure::FeatureFailed(feature.clone(), error));
                            return false;
                        }
                    },
                }
                true
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        candidates.sort_by(|left, right| {
            let left_score = feature_score(left, feature);
            let right_score = feature_score(right, feature);
            right_score
                .relevance
                .cmp(&left_score.relevance)
                .then_with(|| left_score.distance.cmp(&right_score.distance))
                .then_with(|| left.key.cmp(&right.key))
        });
        Ok(candidates)
    }

    fn keep(&mut self, flow: Flow, keep: &Keep) -> Result<Flow, Failure> {
        match flow {
            Flow::Nodes(nodes) => Ok(Flow::Nodes(self.filter(nodes, keep)?)),
            Flow::Paths(paths) => Ok(Flow::Paths(self.filter(paths, keep)?)),
            Flow::Ranked(ranked) => Ok(Flow::Ranked(self.filter(ranked, keep)?)),
            Flow::Context(_) => Err(Failure::WrongFlow("Keep Context")),
        }
    }

    fn filter(
        &mut self,
        candidates: Vec<Candidate>,
        keep: &Keep,
    ) -> Result<Vec<Candidate>, Failure> {
        let mut retained = Vec::new();
        for candidate in candidates {
            let Some(row) = self.read_node(&candidate.key)? else {
                continue;
            };
            if self.node_allowed(&row)
                && keep
                    .predicates
                    .iter()
                    .all(|predicate| self.row_matches(&row, predicate))
            {
                retained.push(candidate);
                self.observe_candidates(retained.len())?;
            }
        }
        Ok(retained)
    }

    fn row_matches(&self, row: &ExtractedNode, predicate: &Predicate) -> bool {
        let expected = atom_value(&predicate.value);
        row.fields.iter().any(|field| {
            self.field_allowed(field)
                && field.reference == predicate.field
                && predicate_matches(&field.value, predicate.test, &expected)
        })
    }

    fn walk(&mut self, starts: &[Candidate], walk: &Walk) -> Result<Flow, Failure> {
        if !self.gates.contains(&walk.gate) {
            return Ok(match walk.emit {
                Emit::Nodes => Flow::Nodes(Vec::new()),
                Emit::Edges | Emit::Paths => Flow::Paths(Vec::new()),
            });
        }
        let wanted: BTreeSet<EdgeRef> = walk.edges.iter().cloned().collect();
        let mut emitted_nodes = BTreeMap::<NodeKey, Candidate>::new();
        let mut emitted_paths = Vec::new();
        for start in starts {
            let initial = PathHit {
                nodes: vec![start.key.clone()],
                hops: Vec::new(),
            };
            let mut frontier = VecDeque::from([initial]);
            let mut branch_candidates = 0usize;
            while let Some(path) = match walk.order {
                WalkOrder::Depth => frontier.pop_back(),
                WalkOrder::Breadth | WalkOrder::Shortest => frontier.pop_front(),
            } {
                let hops = u16::try_from(path.hops.len()).unwrap_or(u16::MAX);
                if hops >= walk.min_hops {
                    let Some(endpoint) = path.nodes.last().cloned() else {
                        continue;
                    };
                    branch_candidates = branch_candidates.saturating_add(1);
                    self.observe_candidates(branch_candidates)?;
                    match walk.emit {
                        Emit::Nodes => {
                            emitted_nodes
                                .entry(endpoint.clone())
                                .or_insert_with(|| Candidate::node(endpoint));
                        }
                        Emit::Edges => {
                            if let Some(hop) = path.hops.last().cloned() {
                                emitted_paths.push(PathHit {
                                    nodes: vec![hop.from.clone(), hop.to.clone()],
                                    hops: vec![hop],
                                });
                            }
                        }
                        Emit::Paths => emitted_paths.push(path.clone()),
                    }
                }
                if hops >= walk.max_hops {
                    continue;
                }
                let Some(endpoint) = path.nodes.last() else {
                    continue;
                };
                let neighbors = self.neighbors(endpoint, &wanted, walk)?;
                for hop in neighbors {
                    if !path_accepts(&path, &hop, walk.unique) {
                        continue;
                    }
                    let mut extended = path.clone();
                    extended.nodes.push(hop.to.clone());
                    extended.hops.push(hop);
                    frontier.push_back(extended);
                    self.meter.observe(
                        Dimension::PathsRetained,
                        usize_u64(frontier.len().saturating_add(emitted_paths.len())),
                    )?;
                }
            }
        }
        match walk.emit {
            Emit::Nodes => Ok(Flow::Nodes(emitted_nodes.into_values().collect())),
            Emit::Edges | Emit::Paths => {
                emitted_paths.sort_by(path_order);
                emitted_paths.dedup();
                Ok(Flow::Paths(
                    emitted_paths
                        .into_iter()
                        .filter_map(|path| {
                            path.nodes.last().cloned().map(|key| Candidate {
                                key,
                                path: Some(path),
                                term_scores: BTreeMap::new(),
                                feature_scores: BTreeMap::new(),
                                merge_score: 0,
                                order: Vec::new(),
                            })
                        })
                        .collect(),
                ))
            }
        }
    }

    fn neighbors(
        &mut self,
        node: &NodeKey,
        wanted: &BTreeSet<EdgeRef>,
        walk: &Walk,
    ) -> Result<Vec<PathHop>, Failure> {
        let mut result = Vec::new();
        if matches!(walk.direction, Direction::Out | Direction::Both) {
            if let Some(row) = self.read_node(node)? {
                if self.node_allowed(&row) {
                    for edge in &row.edges {
                        if !wanted.contains(&edge.reference)
                            || !self.gates.contains(&edge.gate)
                            || edge.gate != walk.gate
                        {
                            continue;
                        }
                        for target in &edge.targets {
                            self.meter.charge(Dimension::EdgesVisited, 1)?;
                            if let Some(target_row) = self.read_node(target)? {
                                if self.node_allowed(&target_row) {
                                    result.push(PathHop {
                                        edge: edge.reference.clone(),
                                        from: node.clone(),
                                        to: target.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        if matches!(walk.direction, Direction::In | Direction::Both) {
            for edge in wanted {
                let mut failure = None;
                let gates = self.gates;
                self.corpus.visit_incoming(
                    edge,
                    node,
                    usize::MAX,
                    |gate| gates.allows_ref(gate),
                    |body, source| {
                        if !self.node_allowed(source) {
                            return true;
                        }
                        if let Err(error) = self.visit_posting_row(body, source) {
                            failure = Some(error);
                            return false;
                        }
                        let allowed = source.edges.iter().any(|candidate| {
                            candidate.reference == *edge
                                && candidate.gate == walk.gate
                                && self.gates.contains(&candidate.gate)
                                && candidate.targets.contains(node)
                        });
                        if allowed {
                            if let Err(error) = self.meter.charge(Dimension::EdgesVisited, 1) {
                                failure = Some(error);
                                return false;
                            }
                            result.push(PathHop {
                                edge: edge.clone(),
                                from: node.clone(),
                                to: source.key.clone(),
                            });
                        }
                        true
                    },
                );
                if let Some(error) = failure {
                    return Err(error);
                }
            }
        }
        result.sort_by(hop_order);
        result.dedup();
        Ok(result)
    }

    fn rank(&mut self, mode: Mode, flow: Flow, methods: &[RankBy]) -> Result<Flow, Failure> {
        let mut candidates = match flow {
            Flow::Nodes(candidates) | Flow::Paths(candidates) | Flow::Ranked(candidates) => {
                candidates
            }
            Flow::Context(_) => return Err(Failure::WrongFlow("Rank Context")),
        };
        let mut retained = Vec::new();
        for mut candidate in candidates.drain(..) {
            let Some(row) = self.read_node(&candidate.key)? else {
                continue;
            };
            if !self.node_allowed(&row) {
                continue;
            }
            candidate.order.clear();
            let mut drop_candidate = false;
            for method in methods {
                self.meter.charge(Dimension::ScoreEvaluations, 1)?;
                let rank = match method {
                    RankBy::Field(field) => row
                        .fields
                        .iter()
                        .find(|candidate| {
                            candidate.reference == *field && self.field_allowed(candidate)
                        })
                        .map(|field| RankValue::Value(field.value.clone())),
                    RankBy::Term(field) => candidate
                        .term_scores
                        .get(field)
                        .copied()
                        .map(RankValue::Descending),
                    RankBy::Feature(feature) => candidate
                        .feature_scores
                        .get(feature)
                        .map(|score| RankValue::DescendingSigned(score.relevance)),
                    RankBy::Distance => candidate
                        .feature_scores
                        .values()
                        .next()
                        .map(|score| RankValue::Ascending(score.distance)),
                };
                match rank {
                    Some(rank) => candidate.order.push(rank),
                    None => match mode {
                        Mode::Augmented {
                            missing: MissingFeature::Drop,
                        } if matches!(method, RankBy::Feature(_) | RankBy::Distance) => {
                            drop_candidate = true;
                            break;
                        }
                        Mode::Augmented {
                            missing: MissingFeature::Continue,
                        } => candidate.order.push(RankValue::Missing),
                        Mode::Exact
                        | Mode::Augmented {
                            missing: MissingFeature::Refuse,
                        } if matches!(method, RankBy::Feature(_) | RankBy::Distance) => {
                            return match method {
                                RankBy::Feature(feature) => {
                                    Err(Failure::FeatureUnavailable(feature.clone()))
                                }
                                RankBy::Distance => Err(Failure::DistanceUnavailable),
                                RankBy::Field(_) | RankBy::Term(_) => {
                                    Err(Failure::WrongFlow("feature rank state"))
                                }
                            };
                        }
                        _ => candidate.order.push(RankValue::Missing),
                    },
                }
            }
            if !drop_candidate {
                retained.push(candidate);
                self.observe_candidates(retained.len())?;
            }
        }
        retained.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| right.merge_score.cmp(&left.merge_score))
                .then_with(|| left.key.cmp(&right.key))
        });
        Ok(Flow::Ranked(retained))
    }

    fn merge(&mut self, flows: Vec<Flow>, method: MergeMethod) -> Result<Flow, Failure> {
        if flows.len() < 2 {
            return Err(Failure::WrongFlow("Merge inputs"));
        }
        let mut branches = Vec::new();
        for flow in flows {
            let Flow::Ranked(branch) = flow else {
                return Err(Failure::WrongFlow("Merge input"));
            };
            branches.push(branch);
        }
        let branch_count = branches.len();
        let mut combined = BTreeMap::<NodeKey, (Candidate, usize, i128)>::new();
        for branch in branches {
            let mut seen = BTreeSet::new();
            for (offset, candidate) in branch.into_iter().enumerate() {
                if !seen.insert(candidate.key.clone()) {
                    continue;
                }
                self.meter.charge(Dimension::ScoreEvaluations, 1)?;
                let rank = usize_u64(offset).saturating_add(1);
                let contribution = match method {
                    MergeMethod::ReciprocalRank => i128::from(
                        1_000_000u64
                            .checked_div(rank.saturating_add(60))
                            .unwrap_or(0),
                    ),
                    MergeMethod::Union | MergeMethod::Intersection => 1,
                };
                combined
                    .entry(candidate.key.clone())
                    .and_modify(|(_, count, score)| {
                        *count = count.saturating_add(1);
                        *score = score.saturating_add(contribution);
                    })
                    .or_insert((candidate, 1, contribution));
            }
        }
        let mut result = combined
            .into_values()
            .filter_map(|(mut candidate, count, score)| {
                if method == MergeMethod::Intersection && count != branch_count {
                    None
                } else {
                    candidate.merge_score = score;
                    Some(candidate)
                }
            })
            .collect::<Vec<_>>();
        self.observe_candidates(result.len())?;
        result.sort_by(|left, right| {
            right
                .merge_score
                .cmp(&left.merge_score)
                .then_with(|| left.key.cmp(&right.key))
        });
        Ok(Flow::Ranked(result))
    }

    fn pack(&mut self, candidates: &[Candidate], fields: &[FieldRef]) -> Result<Flow, Failure> {
        let wanted: BTreeSet<FieldRef> = fields.iter().cloned().collect();
        let mut packed = Vec::new();
        for candidate in candidates {
            let Some(row) = self.read_node(&candidate.key)? else {
                continue;
            };
            if !self.node_allowed(&row) {
                continue;
            }
            let mut values = Vec::new();
            for field in &row.fields {
                if !wanted.contains(&field.reference) || !self.field_allowed(field) {
                    continue;
                }
                let bytes = packed_field_bytes(field);
                self.meter
                    .charge(Dimension::ProjectedBytes, usize_u64(bytes.len()))?;
                let tokens = self
                    .token_counter
                    .count(&bytes)
                    .map_err(Failure::TokenCounting)?;
                self.meter.charge(Dimension::PackedTokens, tokens)?;
                values.push(PackedField {
                    reference: field.reference.clone(),
                    value: field.value.clone(),
                });
            }
            packed.push(PackedNode {
                key: row.key.clone(),
                fields: values,
            });
            self.observe_candidates(packed.len())?;
        }
        Ok(Flow::Context(packed))
    }

    fn visit_posting_row(&mut self, body: &BodyKey, _row: &ExtractedNode) -> Result<(), Failure> {
        self.meter.charge(Dimension::PostingsRead, 1)?;
        self.meter.charge(Dimension::NodesVisited, 1)?;
        let _ = body;
        Ok(())
    }

    fn read_node(&mut self, key: &NodeKey) -> Result<Option<ExtractedNode>, Failure> {
        let gates = self.gates;
        let Some(row) = self
            .corpus
            .node_admitted(key, |gate| gates.allows_ref(gate))
        else {
            return Ok(None);
        };
        if !self.node_allowed(&row) {
            return Ok(None);
        }
        self.meter.charge(Dimension::NodesVisited, 1)?;
        Ok(Some(row))
    }

    fn node_allowed(&self, node: &ExtractedNode) -> bool {
        self.gates.allows(&node.gate)
    }

    fn field_allowed(&self, field: &ExtractedField) -> bool {
        self.gates.allows(&field.gate)
    }

    fn observe_candidates(&mut self, candidates: usize) -> Result<(), Failure> {
        self.meter
            .observe(Dimension::CandidatesPerBranch, usize_u64(candidates))
    }
}

struct Meter {
    limit: Bound,
    usage: Bound,
    step_limit: Bound,
    step_usage: Bound,
    started: Instant,
    step_started: Instant,
}

impl Meter {
    fn new(limit: Bound) -> Self {
        let now = Instant::now();
        Self {
            limit,
            usage: zero_bound(),
            step_limit: limit,
            step_usage: zero_bound(),
            started: now,
            step_started: now,
        }
    }

    fn start_step(&mut self, limit: Bound) {
        self.step_limit = self.limit.intersection(limit);
        self.step_usage = zero_bound();
        self.step_started = Instant::now();
    }

    fn charge(&mut self, dimension: Dimension, amount: u64) -> Result<(), Failure> {
        self.refresh_time()?;
        let global = dimension_value(&mut self.usage, dimension);
        *global = global.saturating_add(amount);
        let step = dimension_value(&mut self.step_usage, dimension);
        *step = step.saturating_add(amount);
        self.check(dimension)
    }

    fn observe(&mut self, dimension: Dimension, value: u64) -> Result<(), Failure> {
        self.refresh_time()?;
        let global = dimension_value(&mut self.usage, dimension);
        *global = (*global).max(value);
        let step = dimension_value(&mut self.step_usage, dimension);
        *step = (*step).max(value);
        self.check(dimension)
    }

    fn refresh_time(&mut self) -> Result<(), Failure> {
        self.usage.wall_millis = elapsed_millis(self.started);
        self.step_usage.wall_millis = elapsed_millis(self.step_started);
        if self.usage.wall_millis > self.limit.wall_millis
            || self.step_usage.wall_millis > self.step_limit.wall_millis
        {
            Err(Failure::BoundExceeded(Dimension::WallMillis))
        } else {
            Ok(())
        }
    }

    fn check(&self, dimension: Dimension) -> Result<(), Failure> {
        if bound_value(self.usage, dimension) > bound_value(self.limit, dimension)
            || bound_value(self.step_usage, dimension) > bound_value(self.step_limit, dimension)
        {
            Err(Failure::BoundExceeded(dimension))
        } else {
            Ok(())
        }
    }

    fn finish_step(&mut self) -> Result<(), Failure> {
        self.refresh_time()
    }

    fn finish(&mut self) -> Result<(), Failure> {
        self.refresh_time()
    }
}

fn zero_bound() -> Bound {
    Bound {
        // Extraction/Body decoding was paid once when this immutable Corpus
        // publication was built; evaluating it performs no Body decode.
        decoded_bodies: 0,
        postings_read: 0,
        edges_visited: 0,
        nodes_visited: 0,
        paths_retained: 0,
        candidates_per_branch: 0,
        score_evaluations: 0,
        projected_bytes: 0,
        packed_tokens: 0,
        wall_millis: 0,
    }
}

fn dimension_value(bound: &mut Bound, dimension: Dimension) -> &mut u64 {
    match dimension {
        Dimension::PostingsRead => &mut bound.postings_read,
        Dimension::EdgesVisited => &mut bound.edges_visited,
        Dimension::NodesVisited => &mut bound.nodes_visited,
        Dimension::PathsRetained => &mut bound.paths_retained,
        Dimension::CandidatesPerBranch => &mut bound.candidates_per_branch,
        Dimension::ScoreEvaluations => &mut bound.score_evaluations,
        Dimension::ProjectedBytes => &mut bound.projected_bytes,
        Dimension::PackedTokens => &mut bound.packed_tokens,
        Dimension::WallMillis => &mut bound.wall_millis,
    }
}

fn bound_value(bound: Bound, dimension: Dimension) -> u64 {
    match dimension {
        Dimension::PostingsRead => bound.postings_read,
        Dimension::EdgesVisited => bound.edges_visited,
        Dimension::NodesVisited => bound.nodes_visited,
        Dimension::PathsRetained => bound.paths_retained,
        Dimension::CandidatesPerBranch => bound.candidates_per_branch,
        Dimension::ScoreEvaluations => bound.score_evaluations,
        Dimension::ProjectedBytes => bound.projected_bytes,
        Dimension::PackedTokens => bound.packed_tokens,
        Dimension::WallMillis => bound.wall_millis,
    }
}

fn elapsed_millis(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn atom_value(atom: &Atom) -> Value {
    match atom {
        Atom::Bool(value) => Value::Bool(*value),
        Atom::Signed(value) => Value::Signed(*value),
        Atom::Unsigned(value) => Value::Unsigned(*value),
        Atom::Bytes(value) => Value::bytes(value.clone()),
        Atom::Text(value) => Value::text(value.clone()),
    }
}

fn predicate_matches(actual: &Value, test: Test, expected: &Value) -> bool {
    match test {
        Test::Equal => actual == expected,
        Test::Less => same_kind(actual, expected).is_some_and(|order| order.is_lt()),
        Test::LessOrEqual => same_kind(actual, expected).is_some_and(|order| order.is_le()),
        Test::Greater => same_kind(actual, expected).is_some_and(|order| order.is_gt()),
        Test::GreaterOrEqual => same_kind(actual, expected).is_some_and(|order| order.is_ge()),
        Test::Contains => match (actual, expected) {
            (Value::Text(actual), Value::Text(expected)) => actual.contains(expected.as_ref()),
            (Value::Bytes(actual), Value::Bytes(expected)) => contains_bytes(actual, expected),
            _ => false,
        },
        Test::Prefix => match (actual, expected) {
            (Value::Text(actual), Value::Text(expected)) => actual.starts_with(expected.as_ref()),
            (Value::Bytes(actual), Value::Bytes(expected)) => actual.starts_with(expected),
            _ => false,
        },
    }
}

fn same_kind(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Some(left.cmp(right)),
        (Value::Signed(left), Value::Signed(right)) => Some(left.cmp(right)),
        (Value::Unsigned(left), Value::Unsigned(right)) => Some(left.cmp(right)),
        (Value::Bytes(left), Value::Bytes(right)) => Some(left.cmp(right)),
        (Value::Text(left), Value::Text(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn path_accepts(path: &PathHit, hop: &PathHop, unique: Unique) -> bool {
    match unique {
        Unique::Walk => true,
        Unique::Trail => !path.hops.contains(hop),
        Unique::Acyclic => !path.nodes.contains(&hop.to),
    }
}

fn canonical_candidates(candidates: &mut Vec<Candidate>) {
    candidates.sort_by(|left, right| left.key.cmp(&right.key));
    candidates.dedup_by(|left, right| left.key == right.key);
}

fn candidate_term_order(left: &Candidate, right: &Candidate) -> Ordering {
    let left_score = left.term_scores.values().copied().sum::<u64>();
    let right_score = right.term_scores.values().copied().sum::<u64>();
    right_score
        .cmp(&left_score)
        .then_with(|| left.key.cmp(&right.key))
}

fn feature_score(candidate: &Candidate, feature: &FeatureRef) -> FeatureScore {
    candidate
        .feature_scores
        .get(feature)
        .copied()
        .unwrap_or(FeatureScore {
            relevance: i64::MIN,
            distance: u64::MAX,
        })
}

fn packed_field_bytes(field: &ExtractedField) -> Vec<u8> {
    let mut bytes = field.reference.name.as_bytes().to_vec();
    match &field.value {
        Value::Bool(value) => bytes.push(u8::from(*value)),
        Value::Signed(value) => bytes.extend_from_slice(&value.to_be_bytes()),
        Value::Unsigned(value) => bytes.extend_from_slice(&value.to_be_bytes()),
        Value::Bytes(value) => bytes.extend_from_slice(value),
        Value::Text(value) => bytes.extend_from_slice(value.as_bytes()),
    }
    bytes
}

fn path_order(left: &PathHit, right: &PathHit) -> Ordering {
    left.nodes
        .cmp(&right.nodes)
        .then_with(|| left.hops.cmp(&right.hops))
}

fn hop_order(left: &PathHop, right: &PathHop) -> Ordering {
    left.edge
        .cmp(&right.edge)
        .then_with(|| left.from.cmp(&right.from))
        .then_with(|| left.to.cmp(&right.to))
}

fn into_output(flow: Flow) -> Output {
    match flow {
        Flow::Nodes(nodes) => Output::Nodes(
            nodes
                .into_iter()
                .map(|candidate| NodeHit { key: candidate.key })
                .collect(),
        ),
        Flow::Paths(paths) => Output::Paths(
            paths
                .into_iter()
                .filter_map(|candidate| candidate.path)
                .collect(),
        ),
        Flow::Ranked(ranked) => Output::Ranked(
            ranked
                .into_iter()
                .map(|candidate| RankedHit {
                    key: candidate.key,
                    path: candidate.path,
                })
                .collect(),
        ),
        Flow::Context(context) => Output::Context(context),
    }
}

fn output_len(output: &Output) -> usize {
    match output {
        Output::Nodes(rows) => rows.len(),
        Output::Paths(rows) => rows.len(),
        Output::Ranked(rows) => rows.len(),
        Output::Context(rows) => rows.len(),
    }
}

fn usize_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        corpus::{CorpusDelta, Limits},
        find::{
            BodyExtraction, Emit, ExtractedEdge, ExtractedFeature, FieldRef, ModeSet, OpSet, Pack,
            Policy, Rank, SchemaRef, SourceRef,
        },
        publication::{
            ExtractorSchemaDigest, MaterializationId, PublicationId, WorldPublicationId,
        },
    };
    use replica::body::{BodyId, SchemaId, WorldId};
    use std::sync::Arc;

    struct BytesAreTokens;

    impl TokenCounter for BytesAreTokens {
        fn count(&self, bytes: &[u8]) -> Result<u64, &'static str> {
            Ok(usize_u64(bytes.len()))
        }
    }

    fn bound() -> Bound {
        Policy::default().bound
    }

    fn schema() -> SchemaRef {
        SchemaRef {
            name: SchemaId::parse("issues.issue").expect("schema"),
            version: 1,
        }
    }

    fn gate(name: &str) -> GateRef {
        GateRef {
            schema: schema(),
            name: SchemaId::parse(name).expect("gate"),
        }
    }

    fn field() -> FieldRef {
        FieldRef {
            schema: schema(),
            name: SchemaId::parse("title").expect("field"),
        }
    }

    fn edge() -> EdgeRef {
        EdgeRef {
            schema: schema(),
            name: SchemaId::parse("relates").expect("edge"),
        }
    }

    fn node(number: u8) -> NodeKey {
        NodeKey {
            schema: schema(),
            node: find::NodeId::new(vec![number]).expect("node"),
        }
    }

    fn body(number: u8) -> BodyKey {
        BodyKey::new(
            WorldId::parse("dev.lait.issues").expect("world"),
            BodyId::from_bytes([number; 16]),
        )
    }

    fn row(number: u8, title: &str, required: Option<GateRef>) -> ExtractedNode {
        ExtractedNode {
            key: node(number),
            gate: required,
            fields: vec![ExtractedField {
                reference: field(),
                value: Value::text(title),
                gate: None,
                terms: vec![Arc::from(title.as_bytes())],
            }],
            edges: Vec::new(),
            features: Vec::new(),
        }
    }

    fn coordinate(root: u8) -> WorldPublicationId {
        WorldPublicationId::new(
            PublicationId::new(
                [root; 32],
                [2; 32],
                ExtractorSchemaDigest::from_digest([3; 32]),
            ),
            MaterializationId::from_u64(u64::from(root)).expect("materialization"),
        )
    }

    fn corpus(rows: Vec<ExtractedNode>) -> Corpus {
        let bodies: Vec<BodyExtraction> = rows
            .into_iter()
            .enumerate()
            .map(|(offset, node)| BodyExtraction {
                body: body(u8::try_from(offset).unwrap_or_default().saturating_add(1)),
                stamp: vec![1],
                nodes: vec![node],
            })
            .collect();
        let snapshot = crate::corpus::snapshot_for_test(&bodies);
        Corpus::build(coordinate(1), Limits::default(), snapshot, bodies)
            .expect("corpus")
            .0
    }

    fn step(id: u32, input: Vec<StepId>, op: Op) -> Step {
        Step {
            id: StepId::new(id).expect("step"),
            input,
            op,
            bound: bound(),
        }
    }

    fn query(mut steps: Vec<Step>) -> Query {
        let stable = steps.last().is_some_and(|step| {
            matches!(
                step.op,
                Op::Seek(Seek::Term { .. } | Seek::Feature { .. })
                    | Op::Rank(_)
                    | Op::Merge(_)
                    | Op::Pack(_)
            )
        });
        if !stable {
            let prior = steps.last().expect("step").id;
            let id = u32::try_from(steps.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1);
            steps.push(step(
                id,
                vec![prior],
                Op::Rank(Rank {
                    by: vec![RankBy::Field(field())],
                }),
            ));
        }
        Query {
            schema: schema(),
            publication: None,
            mode: Mode::Exact,
            output: steps.last().expect("step").id,
            steps,
            bound: bound(),
            page_size: 100,
            cursor: None,
        }
    }

    fn run(
        corpus: &Corpus,
        query: &Query,
        gates: &GrantedGates,
        admitted_bound: Bound,
    ) -> Result<Answer, Failure> {
        evaluate(Evaluation {
            query,
            corpus,
            gates,
            admitted_bound,
            cursor_position: None,
            feature_scorer: None,
            token_counter: &BytesAreTokens,
        })
    }

    fn page_query(page_size: u32) -> Query {
        let first = step(
            1,
            Vec::new(),
            Op::Seek(Seek::Field(Predicate {
                field: field(),
                test: Test::Prefix,
                value: Atom::Text("a".to_owned()),
            })),
        );
        let second = step(
            2,
            vec![first.id],
            Op::Keep(Keep {
                predicates: vec![Predicate {
                    field: field(),
                    test: Test::GreaterOrEqual,
                    value: Atom::Text("aa".to_owned()),
                }],
            }),
        );
        Query {
            schema: schema(),
            publication: None,
            mode: Mode::Exact,
            steps: vec![first, second],
            output: StepId::new(2).expect("output"),
            bound: bound(),
            page_size,
            cursor: None,
        }
    }

    fn run_page(
        corpus: &Corpus,
        query: &Query,
        gates: &GrantedGates,
        position: Option<Vec<u8>>,
    ) -> Result<Answer, Failure> {
        evaluate(Evaluation {
            query,
            corpus,
            gates,
            admitted_bound: bound(),
            cursor_position: position,
            feature_scorer: None,
            token_counter: &BytesAreTokens,
        })
    }

    fn output_keys(answer: &Answer) -> Vec<NodeKey> {
        match &answer.output {
            Output::Nodes(rows) => rows.iter().map(|row| row.key.clone()).collect(),
            Output::Context(rows) => rows.iter().map(|row| row.key.clone()).collect(),
            Output::Ranked(rows) => rows.iter().map(|row| row.key.clone()).collect(),
            Output::Paths(_) => Vec::new(),
        }
    }

    #[test]
    fn ordered_field_pages_do_not_skip_duplicate_or_rescan_hidden_rows() {
        let secret = gate("secret");
        let corpus = corpus(vec![
            row(1, "aa", None),
            row(2, "aa", Some(secret)),
            row(3, "aa", None),
            row(4, "ab", None),
            row(5, "ba", None),
        ]);
        let query = page_query(2);
        let gates = GrantedGates::default();

        let first = run_page(&corpus, &query, &gates, None).expect("first page");
        assert_eq!(output_keys(&first), vec![node(1), node(3)]);
        assert_eq!(
            first.usage.postings_read, 3,
            "two visible and one visible look-ahead; denied partitions are not read"
        );
        let position = first.next_position.clone().expect("continuation");
        assert!(matches!(
            PagePosition::decode(&position).expect("position"),
            PagePosition::Field { value: Value::Text(value), key } if value.as_ref() == "ab" && key == node(4)
        ));

        let second = run_page(&corpus, &query, &gates, Some(position)).expect("second page");
        assert_eq!(output_keys(&second), vec![node(4)]);
        assert_eq!(
            second.usage.postings_read, 1,
            "resume seeks to the look-ahead tuple"
        );
        assert_eq!(second.next_position, None);
    }

    #[test]
    fn exact_page_boundary_has_no_spurious_cursor() {
        let corpus = corpus(vec![row(1, "aa", None), row(2, "ab", None)]);
        let answer = run_page(&corpus, &page_query(2), &GrantedGates::default(), None)
            .expect("one exact page");
        assert_eq!(output_keys(&answer), vec![node(1), node(2)]);
        assert_eq!(answer.next_position, None);
        assert_eq!(answer.usage.postings_read, 2);
    }

    #[test]
    fn bounded_field_range_stops_before_later_postings_and_resumes_exactly() {
        let corpus = corpus(vec![
            row(1, "aa", None),
            row(2, "ab", None),
            row(3, "ac", None),
            row(4, "ad", None),
            row(5, "zz", None),
        ]);
        let query = Query {
            schema: schema(),
            publication: None,
            mode: Mode::Exact,
            steps: vec![step(
                1,
                Vec::new(),
                Op::Seek(Seek::FieldRange(FieldRange {
                    field: field(),
                    lower: RangeEndpoint::Inclusive(Atom::Text("ab".to_owned())),
                    upper: RangeEndpoint::Exclusive(Atom::Text("ad".to_owned())),
                })),
            )],
            output: StepId::new(1).expect("output"),
            bound: bound(),
            page_size: 1,
            cursor: None,
        };

        let first =
            run_page(&corpus, &query, &GrantedGates::default(), None).expect("first interval page");
        assert_eq!(output_keys(&first), vec![node(2)]);
        assert_eq!(
            first.usage.postings_read, 2,
            "one row plus exact look-ahead"
        );
        let second = run_page(
            &corpus,
            &query,
            &GrantedGates::default(),
            first.next_position,
        )
        .expect("second interval page");
        assert_eq!(output_keys(&second), vec![node(3)]);
        assert_eq!(second.usage.postings_read, 1);
        assert_eq!(second.next_position, None);
        assert_eq!(
            first.usage.postings_read + second.usage.postings_read,
            3,
            "the evaluator never visits the exclusive upper endpoint or later values"
        );
    }

    #[test]
    fn denied_partitions_cannot_change_page_cursor_usage_or_refusal() {
        let secret = gate("secret");
        let visible_rows = vec![row(1, "aa", None), row(2, "aa", None), row(3, "ab", None)];
        let visible = corpus(visible_rows.clone());
        let mut mixed_rows = visible_rows;
        mixed_rows.extend((4..=103).map(|number| row(number, "aa", Some(secret.clone()))));
        let mixed = corpus(mixed_rows);
        let query = page_query(2);
        let gates = GrantedGates::default();

        let visible_started = Instant::now();
        let visible_answer = run_page(&visible, &query, &gates, None).expect("visible page");
        let visible_elapsed = visible_started.elapsed();
        let mixed_started = Instant::now();
        let mixed_answer = run_page(&mixed, &query, &gates, None).expect("mixed page");
        let mixed_elapsed = mixed_started.elapsed();

        assert_eq!(output_keys(&mixed_answer), output_keys(&visible_answer));
        assert_eq!(mixed_answer.next_position, visible_answer.next_position);
        assert_eq!(mixed_answer.usage, visible_answer.usage);
        assert_eq!(mixed_answer.matched_total, visible_answer.matched_total);
        assert!(visible_elapsed < std::time::Duration::from_secs(1));
        assert!(mixed_elapsed < std::time::Duration::from_secs(1));

        let mut bounded = query.clone();
        bounded.bound.postings_read = 2;
        for step in &mut bounded.steps {
            step.bound.postings_read = 2;
        }
        assert_eq!(
            run_page(&visible, &bounded, &gates, None),
            run_page(&mixed, &bounded, &gates, None),
            "a denied population cannot create or avoid policy refusal"
        );
    }

    #[test]
    fn token_pages_use_deduped_canonical_postings() {
        let corpus = corpus(vec![
            row(3, "alpha", None),
            row(1, "alpha", None),
            row(4, "beta", None),
            row(2, "alpha", None),
        ]);
        let query = Query {
            schema: schema(),
            publication: None,
            mode: Mode::Exact,
            steps: vec![step(
                1,
                Vec::new(),
                Op::Seek(Seek::Term {
                    field: field(),
                    text: "alpha".to_owned(),
                    kind: Term::Token,
                }),
            )],
            output: StepId::new(1).expect("output"),
            bound: bound(),
            page_size: 2,
            cursor: None,
        };
        let first =
            run_page(&corpus, &query, &GrantedGates::default(), None).expect("first token page");
        assert_eq!(output_keys(&first), vec![node(1), node(2)]);
        assert_eq!(first.usage.postings_read, 3);
        assert_eq!(first.matched_total, Some(3));
        let second = run_page(
            &corpus,
            &query,
            &GrantedGates::default(),
            first.next_position,
        )
        .expect("second token page");
        assert_eq!(output_keys(&second), vec![node(3)]);
        assert_eq!(second.usage.postings_read, 1);
        assert_eq!(second.matched_total, Some(3));
        assert_eq!(second.next_position, None);
    }

    #[test]
    fn exact_field_total_uses_admitted_partition_metadata() {
        let secret = gate("secret");
        let corpus = corpus(vec![
            row(1, "open", None),
            row(2, "open", Some(secret)),
            row(3, "open", None),
            row(4, "closed", None),
        ]);
        let query = Query {
            schema: schema(),
            publication: None,
            mode: Mode::Exact,
            steps: vec![step(
                1,
                Vec::new(),
                Op::Seek(Seek::Field(Predicate {
                    field: field(),
                    test: Test::Equal,
                    value: Atom::Text("open".to_owned()),
                })),
            )],
            output: StepId::new(1).expect("output"),
            bound: bound(),
            page_size: 1,
            cursor: None,
        };
        let answer = run_page(&corpus, &query, &GrantedGates::default(), None).expect("page");
        assert_eq!(output_keys(&answer), vec![node(1)]);
        assert_eq!(answer.matched_total, Some(2));
        assert!(answer.next_position.is_some());
    }

    #[test]
    fn denied_node_and_field_never_affect_seek_rank_or_pack() {
        let secret = gate("secret");
        let corpus = corpus(vec![
            row(1, "visible", None),
            row(2, "secret", Some(secret)),
        ]);
        let query = query(vec![
            step(1, Vec::new(), Op::Seek(Seek::Source)),
            step(
                2,
                vec![StepId::new(1).expect("step")],
                Op::Rank(Rank {
                    by: vec![RankBy::Field(field())],
                }),
            ),
            step(
                3,
                vec![StepId::new(2).expect("step")],
                Op::Pack(Pack {
                    fields: vec![field()],
                }),
            ),
        ]);
        let answer = run(&corpus, &query, &GrantedGates::default(), bound()).expect("evaluate");
        let Output::Context(context) = answer.output else {
            panic!("context");
        };
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].key, node(1));
    }

    #[test]
    fn keep_and_merge_compose_ranked_branches_deterministically() {
        let corpus = corpus(vec![row(1, "visible", None), row(2, "other", None)]);
        let query = query(vec![
            step(1, Vec::new(), Op::Seek(Seek::Source)),
            step(
                2,
                vec![StepId::new(1).expect("step")],
                Op::Keep(Keep {
                    predicates: vec![Predicate {
                        field: field(),
                        test: Test::Prefix,
                        value: Atom::Text("vis".into()),
                    }],
                }),
            ),
            step(
                3,
                vec![StepId::new(2).expect("step")],
                Op::Rank(Rank {
                    by: vec![RankBy::Field(field())],
                }),
            ),
            step(
                4,
                Vec::new(),
                Op::Seek(Seek::Term {
                    field: field(),
                    text: "visible".into(),
                    kind: Term::Token,
                }),
            ),
            step(
                5,
                vec![StepId::new(3).expect("step"), StepId::new(4).expect("step")],
                Op::Merge(find::Merge {
                    method: MergeMethod::Union,
                }),
            ),
            step(
                6,
                vec![StepId::new(5).expect("step")],
                Op::Pack(Pack {
                    fields: vec![field()],
                }),
            ),
        ]);
        let answer = run(&corpus, &query, &GrantedGates::default(), bound()).expect("evaluate");
        let Output::Context(context) = answer.output else {
            panic!("context");
        };
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].key, node(1));
    }

    #[test]
    fn body_seek_visits_only_named_sources_and_still_applies_node_gates() {
        let secret = gate("secret");
        let corpus = corpus(vec![
            row(1, "one", None),
            row(2, "two", None),
            row(3, "three", Some(secret.clone())),
        ]);
        let query = query(vec![step(
            1,
            Vec::new(),
            Op::Seek(Seek::Bodies(vec![body(2), body(3)])),
        )]);
        let denied = run(&corpus, &query, &GrantedGates::default(), bound()).expect("denied");
        assert_eq!(
            denied.output,
            Output::Ranked(vec![RankedHit {
                key: node(2),
                path: None,
            }])
        );
        let granted = run(&corpus, &query, &GrantedGates::new([secret]), bound()).expect("granted");
        assert_eq!(
            granted.output,
            Output::Ranked(vec![
                RankedHit {
                    key: node(3),
                    path: None,
                },
                RankedHit {
                    key: node(2),
                    path: None,
                },
            ])
        );
    }

    #[test]
    fn walk_requires_both_query_and_edge_gate_before_target_influences_output() {
        let read = gate("read");
        let mut first = row(1, "one", None);
        first.edges.push(ExtractedEdge {
            reference: edge(),
            gate: read.clone(),
            targets: vec![node(2)],
        });
        let corpus = corpus(vec![first, row(2, "two", None)]);
        let query = query(vec![
            step(1, Vec::new(), Op::Seek(Seek::Ids(vec![node(1).node]))),
            step(
                2,
                vec![StepId::new(1).expect("step")],
                Op::Walk(Walk {
                    edges: vec![edge()],
                    direction: Direction::Out,
                    min_hops: 1,
                    max_hops: 1,
                    unique: Unique::Acyclic,
                    order: WalkOrder::Breadth,
                    emit: Emit::Nodes,
                    gate: read.clone(),
                }),
            ),
        ]);
        let denied = run(&corpus, &query, &GrantedGates::default(), bound()).expect("denied");
        assert_eq!(denied.output, Output::Ranked(Vec::new()));
        let granted = run(&corpus, &query, &GrantedGates::new([read]), bound()).expect("granted");
        assert_eq!(
            granted.output,
            Output::Ranked(vec![RankedHit {
                key: node(2),
                path: None,
            }])
        );
    }

    #[test]
    fn denied_incoming_adjacency_cannot_change_walk_work_or_answer() {
        let read = gate("read");
        let secret = gate("secret");
        let mut visible_source = row(1, "visible-source", None);
        visible_source.edges.push(ExtractedEdge {
            reference: edge(),
            gate: read.clone(),
            targets: vec![node(2)],
        });
        let target = row(2, "target", None);
        let visible = corpus(vec![visible_source.clone(), target.clone()]);
        let mut mixed_rows = vec![visible_source, target];
        for number in 3..=203 {
            let mut hidden_source = row(number, "hidden-source", None);
            hidden_source.edges.push(ExtractedEdge {
                reference: edge(),
                gate: secret.clone(),
                targets: vec![node(2)],
            });
            mixed_rows.push(hidden_source);
        }
        let mixed = corpus(mixed_rows);
        let query = query(vec![
            step(1, Vec::new(), Op::Seek(Seek::Ids(vec![node(2).node]))),
            step(
                2,
                vec![StepId::new(1).expect("step")],
                Op::Walk(Walk {
                    edges: vec![edge()],
                    direction: Direction::In,
                    min_hops: 1,
                    max_hops: 1,
                    unique: Unique::Acyclic,
                    order: WalkOrder::Breadth,
                    emit: Emit::Nodes,
                    gate: read.clone(),
                }),
            ),
        ]);
        let gates = GrantedGates::new([read]);
        let visible_started = Instant::now();
        let visible_answer = run(&visible, &query, &gates, bound()).expect("visible incoming");
        let visible_elapsed = visible_started.elapsed();
        let mixed_started = Instant::now();
        let mixed_answer = run(&mixed, &query, &gates, bound()).expect("mixed incoming");
        let mixed_elapsed = mixed_started.elapsed();
        assert_eq!(mixed_answer.output, visible_answer.output);
        assert_eq!(mixed_answer.usage, visible_answer.usage);
        assert!(visible_elapsed < std::time::Duration::from_secs(1));
        assert!(mixed_elapsed < std::time::Duration::from_secs(1));
    }

    #[test]
    fn denied_node_gate_precedes_large_outgoing_target_materialization() {
        let read = gate("read");
        let secret = gate("secret");
        let base = corpus(vec![row(1, "hidden", Some(secret.clone()))]);
        let mut large = row(1, "hidden", Some(secret));
        large.edges.push(ExtractedEdge {
            reference: edge(),
            gate: read.clone(),
            targets: (0u32..100_000)
                .map(|number| NodeKey {
                    schema: schema(),
                    node: find::NodeId::new(number.to_be_bytes().to_vec()).expect("target"),
                })
                .collect(),
        });
        let large = corpus(vec![large]);
        let query = query(vec![step(
            1,
            Vec::new(),
            Op::Seek(Seek::Ids(vec![node(1).node])),
        )]);
        let gates = GrantedGates::new([read]);
        let base_started = Instant::now();
        let base_answer = run(&base, &query, &gates, bound()).expect("small denied node");
        let base_elapsed = base_started.elapsed();
        let large_started = Instant::now();
        let large_answer = run(&large, &query, &gates, bound()).expect("large denied node");
        let large_elapsed = large_started.elapsed();
        assert_eq!(large_answer.output, base_answer.output);
        assert_eq!(large_answer.usage, base_answer.usage);
        assert!(base_elapsed < std::time::Duration::from_secs(1));
        assert!(large_elapsed < std::time::Duration::from_secs(1));
    }

    #[test]
    fn every_hard_dimension_refuses_at_its_ceiling() {
        let corpus = corpus(vec![row(1, "one", None), row(2, "two", None)]);
        let query = query(vec![step(1, Vec::new(), Op::Seek(Seek::Source))]);
        let mut tight = bound();
        tight.nodes_visited = 1;
        assert_eq!(
            run(&corpus, &query, &GrantedGates::default(), tight),
            Err(Failure::BoundExceeded(Dimension::NodesVisited))
        );
    }

    #[test]
    fn feature_seek_without_exact_scorer_is_typed_failure() {
        let mut featured = row(1, "one", None);
        let feature = FeatureRef {
            schema: schema(),
            name: SchemaId::parse("semantic").expect("feature"),
        };
        featured.features.push(ExtractedFeature {
            reference: feature.clone(),
            gate: None,
            value: Arc::from([1u8, 2, 3]),
        });
        let corpus = corpus(vec![featured]);
        let mut query = query(vec![step(
            1,
            Vec::new(),
            Op::Seek(Seek::Feature {
                feature: feature.clone(),
                probe: vec![9],
            }),
        )]);
        query.mode = Mode::Augmented {
            missing: MissingFeature::Refuse,
        };
        assert_eq!(
            run(&corpus, &query, &GrantedGates::default(), bound()),
            Err(Failure::FeatureUnavailable(feature))
        );
    }

    #[test]
    fn changed_publication_corpus_is_not_part_of_evaluator_mutation() {
        let corpus = corpus(vec![row(1, "one", None)]);
        let (next, _) = corpus
            .apply(CorpusDelta {
                base: coordinate(1),
                next: coordinate(2),
                snapshot: corpus.snapshot(),
                bodies: Vec::new(),
            })
            .expect("coordinate delta");
        let query = query(vec![step(1, Vec::new(), Op::Seek(Seek::Source))]);
        assert!(run(&next, &query, &GrantedGates::default(), bound()).is_ok());
        assert_eq!(corpus.coordinate(), coordinate(1));
    }

    #[test]
    fn query_fixture_types_remain_world_declaration_compatible() {
        let _ = (OpSet::ALL, ModeSet::ALL);
        let _ = SourceRef {
            name: SchemaId::parse("issues.issue-body").expect("source"),
            version: 1,
        };
    }
}
