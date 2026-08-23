#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "router handlers consume the validated CLI/MCP argument shapes and bounded product DTOs"
)]
//! The Issues application router (C4.3 / C5 step 5 routing).
//!
//! `IssueRouter` maps the product's [`IssuesRequest`] onto the semantic
//! [`issues::IssuesWorld`] through a docked [`runtime::Session`]: it resolves
//! refs/projects/labels through exact product-owned Corpus selectors, mints ids
//! and stamps timestamps (the World is pure), submits the
//! mapped intent, and returns the product-owned [`IssuesResponse`] projection.
//! The host supplies only the docked Session and principal facts.

use issues::contract::{
    self, IssueIntent, IssueQuery, NewLabel, Pos, ResolveEntity, ResolvedEntity, WorkAction,
};
use issues::dto::{IssueView, LabelDto, ProjectDto, Row};
use issues::ids::{DocId, LabelId, SystemUlidSource, UlidSource};
use runtime::world::call::{Access, Call, Code, Context, Failure, Handler, Nudge, Reply};
use runtime::world::{Conflict as SessionConflict, Failure as SessionFailure};
use runtime::{
    world::call::{IdentityAccess, SessionAccess},
    world::DeniedCause,
    world::Intent,
    world::Query,
    world::Rejection,
    world::RequestId,
};
use serde::de::DeserializeOwned;

use crate::{
    decode_call, encode_reply, BoardPos, IssuesRequest as Request, IssuesResponse as Response,
    OPERATION, VERSION,
};

/// How far the sole-live-project fallback will look before it gives up.
///
/// Only the *shape* of the answer matters — one live project or not one — so
/// this needs to be large enough that a real Space answers within a single
/// page, not large enough to enumerate everything. A Space that exceeds it
/// refuses and asks for `-p`, which is the same answer it would give for any
/// other ambiguity.
///
/// It may not simply be raised: `find_kind_page` derives the query bound from
/// this limit as `limit * 64 KiB` of `projected_bytes`, and the Station policy
/// ceiling is 8 MiB (`runtime::find::Policy::default`). Anything above 128 is
/// refused whole as `InvalidRequest` rather than truncated, so this rides the
/// product-wide page size and stays well inside that ceiling.
const SOLE_PROJECT_SCAN: u32 = issues::contract::DEFAULT_PAGE_SIZE;

/// The daemon facts the router needs per request: who is acting and the
/// project-choice inputs. (Membership/standing itself is enforced by the
/// Session's mechanics guard.)
pub struct RouterFacts {
    /// The acting device's canonical string (advisory attribution).
    pub device: String,
    /// The acting actor's canonical string (comment/create authorship).
    pub actor: String,
    /// The environment project hint (the CLI's git-branch key), if any.
    pub project_hint: Option<String>,
    /// The configured default project id, if any.
    pub default_project: Option<String>,
    /// Unix seconds now.
    pub now: u64,
}

/// IssuesWorld's application execution half.
///
/// The semantic [`issues::IssuesWorld`] remains a pure Runtime World. This
/// adapter owns user-facing request resolution, local id/time minting, retry
/// policy, and response construction. A host registers this handler beside the
/// semantic World without learning the product protocol.
#[derive(Debug, Default)]
pub struct IssuesCallHandler;

impl IssuesCallHandler {
    pub const OPERATION: &'static str = OPERATION;
    pub const VERSION: u32 = VERSION;

    fn decode_request(call: &Call) -> Result<Request, Failure> {
        decode_call(call)
    }

    fn route_request(&self, request: Request, context: &Context<'_>) -> Response {
        static CLOCK: std::sync::OnceLock<SystemUlidSource> = std::sync::OnceLock::new();

        let router = IssueRouter::new(
            context.session,
            context.identity,
            CLOCK.get_or_init(|| SystemUlidSource),
        );
        let facts = RouterFacts {
            device: context.device.to_string(),
            actor: context.actor.to_string(),
            // A hint travels on the request (`IssuesRequest::IssueNew.project_hint`)
            // where a head that knows the working tree can put one. Nothing has
            // ever set the environment variable this used to read.
            project_hint: None,
            default_project: None,
            now: mechanics::wallclock::now_secs(),
        };
        // Retryable refusals are transient by definition -- the mutation lane
        // is taken, the read capacity is momentarily full, the authority moved
        // under the signature. Retrying them INSTANTLY asks the same question
        // inside the same microsecond and gets the same answer, so what looked
        // like four attempts was one attempt made four times. Wait a little
        // between them, growing, so the condition has a chance to clear.
        let deadline = std::time::Instant::now() + RETRY_DEADLINE;
        let mut waited = RETRY_BACKOFF;
        let mut response = router.route(request.clone(), &facts).0;
        while matches!(
            &response,
            Response::Error {
                error_kind: crate::IssuesErrorKind::Retry,
                ..
            }
        ) {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep(waited.min(deadline - now));
            waited = waited.saturating_mul(2).min(RETRY_BACKOFF_CAP);
            response = router.route(request.clone(), &facts).0;
        }
        // Hand back the refusal that actually happened. This used to answer
        // "membership changed repeatedly" for every exhausted retry, which
        // named one cause out of several and was usually the wrong one: a
        // busy node reported a governance change nobody had made.
        response
    }
}

/// Reactions asked for beside one page of comments.
///
/// Four per comment across a default page. That covers an ordinary thread
/// outright, and anything past it is reported by `reactions_complete`
/// rather than drawn as absence -- which is what makes a figure here a
/// tuning choice rather than a correctness one.
///
/// It is not the maximum page. A page declares a projection budget that
/// grows with its limit, and asking for the maximum exceeded the grant and
/// refused the whole view -- so the number that was meant to make reactions
/// complete instead made the Issue unreadable.
const REACTIONS_PER_VIEW: u32 = 4 * issues::contract::DEFAULT_PAGE_SIZE;

/// How long a retryable refusal is waited out, and how the waiting grows.
///
/// The thing usually holding the mutation lane is convergence incorporating a
/// peer's work, and on a loaded machine that can hold it for far longer than
/// a handful of milliseconds. A budget counted in ATTEMPTS gets shorter
/// exactly when the machine is slower, which is backwards; this is counted in
/// time, so a busy node is waited out rather than reported as a failure the
/// caller has to understand.
///
/// Bounded, because a request that cannot be admitted should eventually say
/// so rather than hang.
const RETRY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(4);
const RETRY_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_millis(200);

impl Handler for IssuesCallHandler {
    fn access(&self, call: &Call) -> Result<Access, Failure> {
        let request = Self::decode_request(call)?;
        Ok(request.access())
    }

    fn call(&self, call: &Call, context: &Context<'_>) -> Reply {
        let request = match Self::decode_request(call) {
            Ok(request) => request,
            Err(error) => return Reply::error(call, error.code, error.message()),
        };
        let response = self.route_request(request, context);
        match serde_json::to_value(response) {
            Ok(response) => encode_reply(call, &response),
            Err(error) => Reply::error(
                call,
                Code::Internal,
                format!("encode Issues response: {error}"),
            ),
        }
    }

    /// Who should hear about this, and which signal says so.
    ///
    /// Two calls answer at all. Assigning tells the people on the issue that it
    /// moved to them; commenting tells them somebody said something. Everything
    /// else changes durable state that converges on its own, and a signal for
    /// each would be a notification for every field edit anybody makes.
    ///
    /// The acting identity is filtered out. Nobody is told about their own
    /// action — Linear does not, and a person notified of everything they did
    /// stops reading notifications.
    fn nudges(&self, call: &Call, reply: &Reply, context: &Context<'_>) -> Vec<Nudge> {
        // Asked only about work that happened. A refused call changed nothing,
        // and an idempotent replay changed nothing twice.
        if !reply.succeeded() {
            return Vec::new();
        }
        let Ok(request) = Self::decode_request(call) else {
            return Vec::new();
        };
        let (reff, schema) = match &request {
            Request::Assign { reff, .. } => (reff, contract::signal::ASSIGNED),
            Request::Comment { reff, .. } | Request::CommentAt { reff, .. } => {
                (reff, contract::signal::COMMENTED)
            }
            _ => return Vec::new(),
        };
        static CLOCK: std::sync::OnceLock<SystemUlidSource> = std::sync::OnceLock::new();
        let router = IssueRouter::new(
            context.session,
            context.identity,
            CLOCK.get_or_init(|| SystemUlidSource),
        );
        let Some((doc, actors)) = router.interested(reff) else {
            return Vec::new();
        };
        let payload = contract::IssueNudge { issue: doc }.encode();
        actors
            .into_iter()
            .filter(|actor| actor != context.actor)
            .map(|actor| Nudge {
                actor,
                schema: schema.to_string(),
                payload: payload.clone(),
            })
            .collect()
    }
}

/// Cheap selector adapter over the same publication-pinned Corpus used by
/// Find, Geometry, live invalidation, and agent search. It owns no aggregate
/// product state and cannot become a second tracker selectors.
struct Selectors<'a> {
    session: &'a dyn SessionAccess,
}

impl Selectors<'_> {
    fn one(
        &self,
        entity: ResolveEntity,
        selector: &str,
        project: Option<&str>,
    ) -> Option<ResolvedEntity> {
        let bytes = self
            .session
            .query(Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: IssueQuery::Resolve {
                    entity,
                    selector: selector.into(),
                    project: project.map(str::to_owned),
                }
                .to_json(),
                publication: None,
            })
            .ok()?
            .bytes;
        serde_json::from_slice(&bytes).ok()
    }

    fn resolve_issue(&self, reff: &str) -> RefOutcome {
        self.one(ResolveEntity::Issue, reff, None)
            .map_or(RefOutcome::None, |resolved| RefOutcome::One(resolved.id))
    }

    fn resolve_project(&self, reff: &str) -> Option<String> {
        self.one(ResolveEntity::Project, reff, None)
            .map(|resolved| resolved.id)
    }

    fn resolve_milestone(&self, project: &str, reff: &str) -> Option<String> {
        self.one(ResolveEntity::Milestone, reff, Some(project))
            .map(|resolved| resolved.id)
    }

    fn resolve_cycle(&self, project: &str, reff: &str) -> Option<String> {
        self.one(ResolveEntity::Cycle, reff, Some(project))
            .map(|resolved| resolved.id)
    }

    fn resolve_initiative(&self, reff: &str) -> Option<(String, serde_json::Value)> {
        self.one(ResolveEntity::Initiative, reff, None)
            .map(|resolved| (resolved.id, resolved.record))
    }

    fn resolve_team(&self, reff: &str) -> Option<(String, serde_json::Value)> {
        self.one(ResolveEntity::Team, reff, None)
            .map(|resolved| (resolved.id, resolved.record))
    }

    fn resolve_label(&self, reff: &str) -> Option<String> {
        self.one(ResolveEntity::Label, reff, None)
            .map(|resolved| resolved.id)
    }
}

/// The outcome of resolving an issue ref.
enum RefOutcome {
    One(String),
    Many,
    None,
}

/// The router.
/// One assembled Issue from a Detail projection's first pages.
///
/// Reactions are paged beside the comments rather than inside them, so they
/// are put back on the comment they mark. A reaction naming a comment this
/// page does not carry is dropped rather than invented -- the comment page is
/// the bound, and a reaction with nothing to attach to is not a fact about
/// this answer.
fn assemble(detail: issues::contract::IssueDetailProjection) -> IssueView {
    let mut view = detail.issue;
    let mut comments = detail.comments.items;
    for comment in &mut comments {
        comment.reactions.clear();
        let Some(id) = comment.id.clone() else {
            continue;
        };
        for record in detail
            .reactions
            .items
            .iter()
            .filter(|record| record.comment == id && record.on)
        {
            let Some(actor) = issues::ids::ActorId::parse(&record.actor) else {
                continue;
            };
            match comment
                .reactions
                .iter_mut()
                .find(|existing| existing.emoji == record.emoji)
            {
                Some(existing) => existing.actors.push(actor),
                None => comment.reactions.push(issues::dto::ReactionDto {
                    emoji: record.emoji.clone(),
                    actors: vec![actor],
                }),
            }
        }
    }
    view.comments = comments;
    view.attachments = detail.attachments.items;
    view.checks = detail.checks.items;
    // Say where this answer stops. A first page that cannot be told apart
    // from a whole discussion is the wrong answer, not a smaller one.
    view.more_comments = detail.comments.next_cursor;
    view.reactions_complete = detail.reactions.next_cursor.is_none();
    view
}

pub struct IssueRouter<'a> {
    session: &'a dyn SessionAccess,
    identity: &'a dyn IdentityAccess,
    clock: &'a dyn UlidSource,
    accepted: std::cell::RefCell<Option<crate::OperationReceipt>>,
}

impl<'a> IssueRouter<'a> {
    pub fn new(
        session: &'a dyn SessionAccess,
        identity: &'a dyn IdentityAccess,
        clock: &'a dyn UlidSource,
    ) -> Self {
        Self {
            session,
            identity,
            clock,
            accepted: std::cell::RefCell::new(None),
        }
    }

    fn selectors(&self) -> Selectors<'_> {
        Selectors {
            session: self.session,
        }
    }

    fn submit(&self, intent: &IssueIntent) -> Result<contract::IssueEffect, SessionFailure> {
        self.submit_with_request(intent, RequestId::mint())
    }

    fn submit_with_request(
        &self,
        intent: &IssueIntent,
        request: RequestId,
    ) -> Result<contract::IssueEffect, SessionFailure> {
        let action = self.identity.sign_action(
            self.session,
            request,
            Intent {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: intent.to_json(),
            },
        )?;
        let committed = self.session.submit(action)?;
        self.accepted.replace(Some(crate::OperationReceipt {
            operation: data_encoding::HEXLOWER.encode(&committed.operation),
            phase: crate::OperationPhase::Accepted,
            publication: committed.publication,
        }));
        Ok(
            contract::IssueEffect::from_json(&committed.effect).unwrap_or(contract::IssueEffect {
                doc: None,
                run: None,
                unchanged: false,
                results: Vec::new(),
            }),
        )
    }

    /// Lower one convenience command through the same product ChangeSet
    /// planner used by browser batches and agents. Product references remain
    /// unresolved until the World evaluates the action's pinned publication.
    fn submit_change(
        &self,
        operation: contract::ChangeOperation,
        ts: u64,
    ) -> Result<contract::IssueEffect, SessionFailure> {
        self.submit(&IssueIntent::ChangeSet {
            operations: vec![operation],
            ts,
        })
    }

    /// Who is on this issue, and what it is called.
    ///
    /// Read *after* the commit, so the answer is the set as it now stands rather
    /// than the set the caller named — assigning somebody tells them, and it
    /// also tells whoever was already there.
    ///
    /// Assignees and followers together: Linear subscribes you on assignment and
    /// keeps you subscribed after, and separating the two here would mean a
    /// person who asked to follow an issue heard less about it than one who was
    /// put on it and left.
    pub fn interested(&self, reff: &str) -> Option<(String, Vec<String>)> {
        // Most requests already carry canonical product coordinates or lower
        // directly into the shared ChangeSet/Find paths. Legacy selector
        // adapters hydrate their selectors only when an arm actually derefs it;
        // a canonical action must never pay a whole-tracker read before the
        // request is even classified.
        let selectors = self.selectors();
        let doc = self.resolve(&selectors, reff).ok()?;
        let view: IssueView = self
            .query(&IssueQuery::View {
                doc: doc.clone(),
                me: None,
            })
            .ok()?;
        let mut actors: Vec<String> = view
            .assignees
            .iter()
            .chain(view.followers.iter())
            .map(|actor| actor.as_str().to_string())
            .collect();
        actors.sort();
        actors.dedup();
        Some((doc, actors))
    }

    fn query<T: DeserializeOwned>(&self, query: &IssueQuery) -> Result<T, SessionFailure> {
        let bytes = self
            .session
            .query(Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: query.to_json(),
                publication: None,
            })?
            .bytes;
        serde_json::from_slice(&bytes)
            .map_err(|_| SessionFailure::Rejected(Rejection::InvalidRequest))
    }

    fn query_pinned<T: DeserializeOwned>(
        &self,
        query: &IssueQuery,
        publication: runtime::publication::PublicationId,
    ) -> Result<T, SessionFailure> {
        let bytes = self
            .session
            .query(Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: query.to_json(),
                publication: Some(publication),
            })?
            .bytes;
        serde_json::from_slice(&bytes)
            .map_err(|_| SessionFailure::Rejected(Rejection::InvalidRequest))
    }

    fn query_exact<T: DeserializeOwned>(
        &self,
        query: &IssueQuery,
        publication: crate::protocol::WorldPublicationCoordinate,
    ) -> Result<T, SessionFailure> {
        let publication = publication
            .parse()
            .ok_or(SessionFailure::Rejected(Rejection::InvalidRequest))?;
        let bytes = self
            .session
            .query_at(
                publication,
                Query {
                    schema: contract::issue_schema(),
                    schema_version: contract::ISSUE_SCHEMA_VERSION,
                    payload: query.to_json(),
                    publication: Some(publication.publication),
                },
            )?
            .bytes;
        serde_json::from_slice(&bytes)
            .map_err(|_| SessionFailure::Rejected(Rejection::InvalidRequest))
    }

    fn query_coordinate<T: DeserializeOwned>(
        &self,
        query: &IssueQuery,
        publication: Option<crate::protocol::PublicationCoordinate>,
    ) -> Result<T, SessionFailure> {
        match publication {
            Some(publication) => self.query_pinned(
                query,
                publication
                    .parse()
                    .ok_or(SessionFailure::Rejected(Rejection::InvalidRequest))?,
            ),
            None => match query {
                IssueQuery::History { page, .. }
                | IssueQuery::Relations { page, .. }
                | IssueQuery::Comments { page, .. }
                | IssueQuery::Reactions { page, .. }
                | IssueQuery::Attachments { page, .. }
                | IssueQuery::Checks { page, .. }
                | IssueQuery::Inbox { page, .. } => self.query_page(query, page),
                _ => self.query(query),
            },
        }
    }

    /// Enter the exact World publication carried by an Issues continuation
    /// before asking the World to evaluate its inner Runtime cursor. A first
    /// page has no coordinate and intentionally reads the current publication;
    /// every continuation is exact or rejected.
    fn query_page<T: DeserializeOwned>(
        &self,
        query: &IssueQuery,
        page: &issues::contract::PageRequest,
    ) -> Result<T, SessionFailure> {
        match page.cursor.as_deref() {
            None => self.query(query),
            Some(cursor) => {
                let (publication, _) = issues::contract::decode_page_cursor(cursor)
                    .ok_or(SessionFailure::Rejected(Rejection::InvalidRequest))?;
                self.query_pinned(query, publication.publication)
            }
        }
    }

    /// The current full collision-safe human rendering for one Issue id.
    fn reff_for(&self, selectors: &Selectors<'_>, doc: &str) -> String {
        selectors
            .one(ResolveEntity::Issue, doc, None)
            .map(|resolved| resolved.display)
            .unwrap_or_else(|| doc.to_string())
    }

    /// Choose a project id from the legacy precedence: explicit → env hint →
    /// default → sole → error.
    fn choose_project(
        &self,
        selectors: &Selectors<'_>,
        explicit: Option<&str>,
        facts: &RouterFacts,
    ) -> Result<String, Response> {
        if let Some(p) = explicit {
            return selectors
                .resolve_project(p)
                .ok_or_else(|| Response::not_found(format!("no project matches {p:?}")));
        }
        if let Some(hint) = &facts.project_hint {
            if let Some(id) = selectors.resolve_project(hint) {
                return Ok(id);
            }
        }
        if let Some(default) = &facts.default_project {
            if let Some(id) = selectors.resolve_project(default) {
                return Ok(id);
            }
        }
        // Auto-selection skips archived projects: a soft-hidden project must not
        // become the default board just because it is the only live-looking one
        // (CUSTOM-9). Explicit refs above still resolve it.
        //
        // This is the last link of the documented chain and it is load-bearing:
        // a freshly founded Space has exactly one project, so every first issue
        // filed without `-p` arrives here. Reached only after the three cheaper
        // links miss, so the enumeration is not on the common path.
        let page = issues::contract::PageRequest {
            limit: SOLE_PROJECT_SCAN,
            cursor: None,
        };
        let projects: issues::contract::Page<ProjectDto> = self
            .query_page(&IssueQuery::Projects { page: page.clone() }, &page)
            .map_err(Self::effect_err)?;
        let mut live = projects.items.iter().filter(|project| !project.archived);
        if let Some(only) = live.next() {
            // A second live project means the answer is genuinely ambiguous; a
            // truncated scan means we cannot prove it is not, and refusing is
            // the safe direction in both cases.
            if live.next().is_none() && projects.next_cursor.is_none() {
                return Ok(only.id.to_string());
            }
        }
        Err(Response::err(
            "no project chosen and no single default — pass -p <project>",
        ))
    }

    /// Resolve a ref to a DocId or a mapped error response.
    fn resolve(&self, selectors: &Selectors<'_>, reff: &str) -> Result<String, Response> {
        match selectors.resolve_issue(reff) {
            RefOutcome::One(doc) => Ok(doc),
            RefOutcome::Many => Err(Response::not_found(format!("{reff:?} is ambiguous"))),
            RefOutcome::None => Err(Response::not_found(format!("no issue matches {reff:?}"))),
        }
    }

    fn map_pos(&self, selectors: &Selectors<'_>, pos: BoardPos) -> Result<Pos, Response> {
        Ok(match pos {
            BoardPos::Top => Pos::Top,
            BoardPos::Bottom => Pos::Bottom,
            BoardPos::Before { reff } => Pos::Before {
                doc: self.resolve(selectors, &reff)?,
            },
            BoardPos::After { reff } => Pos::After {
                doc: self.resolve(selectors, &reff)?,
            },
        })
    }

    fn issuing_denied(kind: &str, capability: &str) -> Response {
        Response::denied(format!(
            "issuing or withdrawing a {kind} needs the {capability} capability \
             (a project grant or space.admin). space.contributor can draft and \
             send for review; it cannot make governing truth. Ask an admin to \
             issue it, or to grant you {capability} on this project; nothing \
             was changed"
        ))
    }

    fn baseline_member_not_issued(
        member: &issues::spec::SpecRef,
        view: &issues::spec::SpecView,
    ) -> Response {
        Response::invalid(format!(
            "Baseline member {}@{} is not an issued Spec revision — current \
             head is {}, state {}. A Baseline is a named set of exact issued \
             revisions; draft and review heads cannot enter it. Issue the Spec \
             with spec_state state=issued (needs spec.issue or space.admin), \
             then retry",
            member.spec,
            member.revision,
            view.revision,
            view.state.as_str()
        ))
    }

    fn require_issued_members(&self, members: &[issues::spec::SpecRef]) -> Result<(), Response> {
        for member in members {
            let view: issues::spec::SpecView = self
                .query(&IssueQuery::Spec {
                    spec: member.spec.clone(),
                })
                .map_err(|failure| match failure {
                    SessionFailure::Rejected(Rejection::InvalidRequest) => {
                        Response::not_found(format!("no Spec matches {}", member.spec))
                    }
                    other => Self::effect_err(other),
                })?;
            if !view
                .issued
                .iter()
                .any(|revision| revision == &member.revision)
            {
                return Err(Self::baseline_member_not_issued(member, &view));
            }
        }
        Ok(())
    }

    fn lifecycle_err(e: SessionFailure, capability: &str, kind: &str) -> Response {
        match e {
            SessionFailure::Rejected(Rejection::Denied(DeniedCause::DemandUnsatisfied)) => {
                Self::issuing_denied(kind, capability)
            }
            other => Self::effect_err(other),
        }
    }

    fn effect_err(e: SessionFailure) -> Response {
        match e {
            // Each denial cause names its own remedy. The collapsed form told
            // a member whose grant had not synced yet to ask an admin for a
            // grant they already held, and rendered read refusals and ledger
            // failures in write-standing words.
            SessionFailure::Rejected(Rejection::Denied(DeniedCause::NotAMember)) => {
                Response::denied(
                    "this device isn't recognized as a member of this space at its current \
                     local view — if you were just invited or promoted, that change may not \
                     have synced to this node yet (run sync and retry); otherwise ask an \
                     admin to admit or re-admit you; nothing was changed",
                )
            }
            SessionFailure::Rejected(Rejection::Denied(DeniedCause::DemandUnsatisfied)) => {
                Response::denied(
                    "you don't hold the capability this change demands — a view-only \
                     member needs an admin to grant write access, a sponsored agent needs \
                     its human sponsor to grant it, and a scoped member may be writing \
                     outside the projects their grant covers; nothing was changed",
                )
            }
            SessionFailure::Rejected(Rejection::Denied(DeniedCause::PrincipalMismatch)) => {
                Response::denied(
                    "your identity changed between signing and committing this change — \
                     retry; nothing was changed",
                )
            }
            SessionFailure::Rejected(Rejection::Denied(DeniedCause::ReadRefused)) => {
                Response::denied(
                    "you can't read this — your grants don't cover this query's scope; \
                     an admin can widen them",
                )
            }
            // NOT a denial: authority state could not be evaluated at all.
            SessionFailure::AuthorityUnavailable(detail) => Response::err(format!(
                "this node could not evaluate authority state ({detail}) — a local \
                 ledger problem, not a permissions problem; run sync (or doctor) and \
                 retry; nothing was changed"
            )),
            // NOT a standing problem, and it must not be phrased as one: this
            // state cost a debugging day partly because an admin holding every
            // grant was told they lacked write standing.
            SessionFailure::Rejected(Rejection::NoActiveImplementation) => Response::denied(
                "no World implementation is active at this space's frontier, so no \
                 write can be authorized for anyone — reopen this Space and approve \
                 the reviewed World update in the launcher; nothing was changed",
            ),
            SessionFailure::Rejected(Rejection::ImplementationUnavailable) => Response::err(
                "the exact World implementation active for this space is not installed on this node",
            ),
            SessionFailure::Rejected(Rejection::Conflict)
            | SessionFailure::Conflict(SessionConflict::Body) => {
                Response::err("that change conflicts with the current state")
            }
            SessionFailure::Conflict(SessionConflict::Request) => {
                Response::err("duplicate request")
            }
            SessionFailure::Rejected(Rejection::InvalidRequest | Rejection::ContractViolation) => {
                Response::invalid("invalid request")
            }
            SessionFailure::Rejected(
                Rejection::UnsupportedSchema | Rejection::UnsupportedSchemaVersion,
            ) => Response::err("unsupported request"),
            SessionFailure::Rejected(Rejection::LimitExceeded) => {
                Response::err("request exceeds a limit")
            }
            SessionFailure::Conflict(SessionConflict::AuthorityChanged) => {
                Response::retry("membership changed — retry")
            }
            SessionFailure::Interrupted => Response::err("the space is shutting down"),
            SessionFailure::GenerationUnavailable => {
                Response::not_found("that World generation is not available on this node")
            }
            SessionFailure::PublicationExpired(_) => Response::err(
                "that exact World publication is no longer retained — reopen the current view; the operation receipt was not rewritten",
            ),
            SessionFailure::ReadCapacity => Response::retry(
                "this node's publication read capacity is currently full — retry shortly",
            ),
            SessionFailure::Busy => Response::retry(
                "this node is busy before accepting the operation — retry shortly",
            ),
            SessionFailure::OutcomeUnknown => Response::indeterminate(
                "the operation's durable outcome is indeterminate — keep the pending view and reconcile by operation id; do not blindly replay",
            ),
            SessionFailure::PersistenceCause { operation, reason } => {
                Response::err(format!("persistence failed while {operation}: {reason}"))
            }
            SessionFailure::Persistence | SessionFailure::CallbackPanicked => {
                Response::err("internal error")
            }
            SessionFailure::Reset => Response::err("state reset — re-query"),
            SessionFailure::Rejected(Rejection::BodyRead(failure)) => match failure {
                runtime::world::BodyReadFailure::CapabilityUnavailable => Response::err(
                    "this World callback was installed without the Body-read capability this operation requires",
                ),
                runtime::world::BodyReadFailure::Opaque(_) => Response::err(
                    "this exact publication contains an opaque Body that the active implementation cannot interpret",
                ),
                runtime::world::BodyReadFailure::NotCollaborative(_) => Response::err(
                    "the active implementation requested a collaborative view of a non-collaborative Body — operator attention is required",
                ),
                runtime::world::BodyReadFailure::SchemaAhead(_) => Response::err(
                    "this exact publication contains a newer Body schema than the active implementation can read",
                ),
                runtime::world::BodyReadFailure::KeyUnavailable(_) => Response::err(
                    "the key for this exact publication is unavailable — sync authority material, then re-query the current publication",
                ),
                runtime::world::BodyReadFailure::Corrupt(_) => Response::err(
                    "an authenticated Body in this exact publication is corrupt — operator attention is required",
                ),
                runtime::world::BodyReadFailure::Capacity(_) => Response::retry(
                    "this node's Body-image read capacity is currently full — retry shortly",
                ),
                runtime::world::BodyReadFailure::MaterialUnavailable(_) => Response::err(
                    "this exact publication's Body material is not available locally — sync, then re-query the current publication",
                ),
                runtime::world::BodyReadFailure::PublicationExpired(_) => Response::err(
                    "that exact publication is no longer retained — re-query from the current publication",
                ),
                runtime::world::BodyReadFailure::Interrupted(_) => {
                    Response::retry("the publication read was interrupted — retry")
                }
                _ => Response::err("the exact publication could not be read — re-query or contact an operator"),
            },
            SessionFailure::Rejected(Rejection::StateCorrupt) => Response::err(
                "the space's issue catalog is corrupt (missing, duplicated, or mis-bound) — \
                 this store needs operator attention; nothing was changed",
            ),
        }
    }

    /// Whether the router handles this request (the issue family). Membership,
    /// transport, and daemon-local requests are dispatched elsewhere.
    pub fn handles(req: &Request) -> bool {
        matches!(
            req,
            Request::Inbox { .. }
                | Request::AccessPlan { .. }
                | Request::IssueNew { .. }
                | Request::IssueEdit { .. }
                | Request::IssueTextSplice { .. }
                | Request::IssueDocumentUpgrade { .. }
                | Request::IssueTextCheckpoint { .. }
                | Request::IssueMove { .. }
                | Request::Assign { .. }
                | Request::Label { .. }
                | Request::Comment { .. }
                | Request::CommentAt { .. }
                | Request::React { .. }
                | Request::IssueDelete { .. }
                | Request::IssueRestore { .. }
                | Request::IssueLink { .. }
                | Request::IssueUnlink { .. }
                | Request::IssueParent { .. }
                | Request::IssueStart { .. }
                | Request::IssueDone { .. }
                | Request::IssueStop { .. }
                | Request::Verify { .. }
                | Request::AcceptCheck { .. }
                | Request::Geometry { .. }
                | Request::IssueView { .. }
                | Request::IssueDetail { .. }
                | Request::List { .. }
                | Request::Board { .. }
                | Request::History { .. }
                | Request::IssueRelations { .. }
                | Request::IssueComments { .. }
                | Request::IssueReactions { .. }
                | Request::IssueAttachments { .. }
                | Request::IssueChecks { .. }
                | Request::ProjectNew { .. }
                | Request::ProjectList { .. }
                | Request::ProjectEdit { .. }
                | Request::ProjectUpdates { .. }
                | Request::ProjectUpdatePost { .. }
                | Request::ProjectDelete { .. }
                | Request::Follow { .. }
                | Request::MilestoneList { .. }
                | Request::MilestoneSet { .. }
                | Request::IssueMilestone { .. }
                | Request::CycleList { .. }
                | Request::CycleSet { .. }
                | Request::IssueCycle { .. }
                | Request::InitiativeList { .. }
                | Request::InitiativeSet { .. }
                | Request::TeamList { .. }
                | Request::TeamSet { .. }
                | Request::TriageList { .. }
                | Request::TriageSubmit { .. }
                | Request::TriageDecide { .. }
                | Request::Attach { .. }
                | Request::Detach { .. }
                | Request::AttachmentGet { .. }
                | Request::LabelNew { .. }
                | Request::LabelList { .. }
                | Request::LabelShow { .. }
                | Request::LabelEdit { .. }
                | Request::LabelDelete { .. }
                | Request::SpaceRename { .. }
                | Request::SpaceDescribe { .. }
                | Request::Activity { .. }
                | Request::RoleList { .. }
                | Request::RoleShow { .. }
                | Request::RoleCreate { .. }
                | Request::RoleEdit { .. }
                | Request::RoleDelete { .. }
                | Request::RoleResolve { .. }
                | Request::WorkflowShow { .. }
                | Request::WorkflowValidate { .. }
                | Request::WorkflowSet { .. }
                | Request::SpecList { .. }
                | Request::SpecShow { .. }
                | Request::SpecHistory { .. }
                | Request::SpecReferences { .. }
                | Request::SpecObservations { .. }
                | Request::BaselineHistory { .. }
                | Request::SpecNew { .. }
                | Request::SpecRevise { .. }
                | Request::SpecDocumentUpgrade { .. }
                | Request::SpecState { .. }
                | Request::SpecResolve { .. }
                | Request::SpecObserve { .. }
                | Request::SpecRetract { .. }
                | Request::BaselineList { .. }
                | Request::BaselineShow { .. }
                | Request::BaselineNew { .. }
                | Request::BaselineRevise { .. }
                | Request::BaselineState { .. }
                | Request::BaselineResolve { .. }
                | Request::IssueBaseline { .. }
                | Request::Packet { .. }
        )
    }

    /// Route one issue-family request. Returns the mapped response and whether
    /// it committed a change (the daemon rings the doorbell / re-announces).
    pub fn route(&self, req: Request, facts: &RouterFacts) -> (Response, bool) {
        self.accepted.replace(None);
        match self.route_inner(req, facts) {
            Ok((resp, changed)) => match (self.accepted.take(), changed) {
                (Some(receipt), _) => (
                    Response::Operation {
                        receipt,
                        response: Box::new(resp),
                    },
                    changed,
                ),
                (None, true) => (
                    Response::err(
                        "the operation changed durable state without returning its receipt",
                    ),
                    false,
                ),
                (None, false) => (resp, false),
            },
            Err(resp) => (resp, false),
        }
    }

    fn route_inner(&self, req: Request, facts: &RouterFacts) -> Result<(Response, bool), Response> {
        let selectors = self.selectors();
        match req {
            request @ (Request::ChangeSet { .. } | Request::OperationStatus { .. }) => {
                let status_only = matches!(&request, Request::OperationStatus { .. });
                let (operation, timestamp, operations) = match request {
                    Request::ChangeSet {
                        operation,
                        timestamp,
                        operations,
                    } => (operation, timestamp, operations),
                    Request::OperationStatus {
                        operation,
                        timestamp,
                        operations,
                    } => (Some(operation), Some(timestamp), operations),
                    _ => return Err(Response::err("invalid operation request")),
                };
                let operations = operations
                    .into_iter()
                    .map(|operation| -> Result<contract::ChangeOperation, Response> {
                        Ok(match operation {
                            crate::ChangeOperation::ProjectCreate { name, key, color } => {
                                contract::ChangeOperation::ProjectCreate { name, key, color }
                            }
                            crate::ChangeOperation::SpecCreate {
                                project,
                                kind,
                                title,
                                text,
                                links,
                            } => contract::ChangeOperation::SpecCreate {
                                project: match project {
                                    crate::ChangeProject::Existing { project } => {
                                        contract::ChangeProject::Existing { project }
                                    }
                                    crate::ChangeProject::Created { operation } => {
                                        contract::ChangeProject::Created { operation }
                                    }
                                },
                                kind,
                                title,
                                text: if text.starts_with(contract::DOCUMENT_PREFIX) {
                                    text
                                } else {
                                    crate::document::plain_document(&text)
                                },
                                links,
                            },
                            crate::ChangeOperation::IssueCreate {
                                project,
                                title,
                                priority,
                                status,
                                parent,
                                assignees,
                                labels,
                                body,
                                due,
                                estimate,
                            } => contract::ChangeOperation::IssueCreate {
                                project: match project {
                                    crate::ChangeProject::Existing { project } => {
                                        contract::ChangeProject::Existing { project }
                                    }
                                    crate::ChangeProject::Created { operation } => {
                                        contract::ChangeProject::Created { operation }
                                    }
                                },
                                title,
                                priority: priority.unwrap_or_else(|| "none".into()),
                                status,
                                parent,
                                assignees,
                                labels: labels
                                    .into_iter()
                                    .map(|label| match label {
                                        crate::ChangeLabel::Existing { label } => {
                                            contract::ChangeLabel::Existing { label }
                                        }
                                        crate::ChangeLabel::Created { operation } => {
                                            contract::ChangeLabel::Created { operation }
                                        }
                                    })
                                    .collect(),
                                body: body.map(|body| {
                                    if body.starts_with(contract::DOCUMENT_PREFIX) {
                                        body
                                    } else {
                                        crate::document::plain_document(&body)
                                    }
                                }),
                                due,
                                estimate,
                            },
                            crate::ChangeOperation::IssueBoard {
                                issue,
                                status,
                                position,
                            } => contract::ChangeOperation::IssueBoard {
                                issue,
                                status,
                                position: position.map(|position| match position {
                                    crate::ChangePosition::Top => contract::ChangePosition::Top,
                                    crate::ChangePosition::Bottom => {
                                        contract::ChangePosition::Bottom
                                    }
                                    crate::ChangePosition::Before { issue } => {
                                        contract::ChangePosition::Before { issue }
                                    }
                                    crate::ChangePosition::After { issue } => {
                                        contract::ChangePosition::After { issue }
                                    }
                                }),
                            },
                            crate::ChangeOperation::IssuePatch {
                                issue,
                                title,
                                status,
                                priority,
                                due,
                                clear_due,
                                estimate,
                                clear_estimate,
                                assignees,
                                labels,
                            } => contract::ChangeOperation::IssuePatch {
                                issue,
                                title,
                                status,
                                priority,
                                due,
                                clear_due,
                                estimate,
                                clear_estimate,
                                assignees,
                                labels: labels.map(|labels| {
                                    labels
                                        .into_iter()
                                        .map(|label| match label {
                                            crate::ChangeLabel::Existing { label } => {
                                                contract::ChangeLabel::Existing { label }
                                            }
                                            crate::ChangeLabel::Created { operation } => {
                                                contract::ChangeLabel::Created { operation }
                                            }
                                        })
                                        .collect()
                                }),
                            },
                            crate::ChangeOperation::IssueWork { issue, action } => {
                                contract::ChangeOperation::IssueWork {
                                    issue,
                                    action: match action {
                                        crate::ChangeWorkAction::Start => WorkAction::Start,
                                        crate::ChangeWorkAction::Done => WorkAction::Done,
                                        crate::ChangeWorkAction::Stop => WorkAction::Stop,
                                    },
                                }
                            }
                            crate::ChangeOperation::IssueTombstone { issue, on } => {
                                contract::ChangeOperation::IssueTombstone { issue, on }
                            }
                            crate::ChangeOperation::IssueComment {
                                issue,
                                body,
                                parent,
                            } => contract::ChangeOperation::IssueComment {
                                issue,
                                body,
                                parent,
                            },
                            crate::ChangeOperation::IssueCommentAt {
                                issue,
                                body,
                                field,
                                start,
                                end,
                                parent,
                                source,
                            } => contract::ChangeOperation::IssueCommentAt {
                                issue,
                                body,
                                field,
                                start,
                                end,
                                parent,
                                source: source.parse().ok_or_else(|| {
                                    Response::invalid("source must be an exact world publication")
                                })?,
                            },
                            crate::ChangeOperation::IssueReaction {
                                issue,
                                comment,
                                emoji,
                                on,
                            } => contract::ChangeOperation::IssueReaction {
                                issue,
                                comment,
                                emoji,
                                on,
                            },
                            crate::ChangeOperation::IssueLink {
                                issue,
                                kind,
                                target,
                                on,
                            } => contract::ChangeOperation::IssueLink {
                                issue,
                                kind,
                                target,
                                on,
                            },
                            crate::ChangeOperation::IssueParent { issue, parent } => {
                                contract::ChangeOperation::IssueParent { issue, parent }
                            }
                            crate::ChangeOperation::IssueMove {
                                issue,
                                project,
                                position,
                            } => contract::ChangeOperation::IssueMove {
                                issue,
                                project: project.map(|project| match project {
                                    crate::ChangeProject::Existing { project } => {
                                        contract::ChangeProject::Existing { project }
                                    }
                                    crate::ChangeProject::Created { operation } => {
                                        contract::ChangeProject::Created { operation }
                                    }
                                }),
                                position: position.map(|position| match position {
                                    crate::ChangePosition::Top => contract::ChangePosition::Top,
                                    crate::ChangePosition::Bottom => {
                                        contract::ChangePosition::Bottom
                                    }
                                    crate::ChangePosition::Before { issue } => {
                                        contract::ChangePosition::Before { issue }
                                    }
                                    crate::ChangePosition::After { issue } => {
                                        contract::ChangePosition::After { issue }
                                    }
                                }),
                            },
                            crate::ChangeOperation::IssueMilestone { issue, milestone } => {
                                contract::ChangeOperation::IssueMilestone { issue, milestone }
                            }
                            crate::ChangeOperation::LabelCreate { name, color } => {
                                contract::ChangeOperation::LabelCreate { name, color }
                            }
                            crate::ChangeOperation::LabelEdit { label, name, color } => {
                                contract::ChangeOperation::LabelEdit { label, name, color }
                            }
                            crate::ChangeOperation::LabelDelete { label } => {
                                contract::ChangeOperation::LabelDelete { label }
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let request = match operation {
                    Some(operation) => {
                        let bytes = data_encoding::HEXLOWER
                            .decode(operation.as_bytes())
                            .map_err(|_| {
                                Response::invalid("operation must be 32 lowercase hex digits")
                            })?;
                        let raw: [u8; 16] = bytes.try_into().map_err(|_| {
                            Response::invalid("operation must be 32 lowercase hex digits")
                        })?;
                        RequestId::from_bytes(raw)
                    }
                    None => RequestId::mint(),
                };
                let semantic = IssueIntent::ChangeSet {
                    operations,
                    ts: timestamp.unwrap_or(facts.now),
                };
                if status_only {
                    let intent = Intent {
                        schema: contract::issue_schema(),
                        schema_version: contract::ISSUE_SCHEMA_VERSION,
                        payload: semantic.to_json(),
                    };
                    let status = self
                        .session
                        .operation_status_for(request, &intent)
                        .map_err(Self::effect_err)?;
                    let operation = data_encoding::HEXLOWER.encode(&request.as_bytes());
                    return Ok((
                        match status {
                            runtime::OperationStatus::Absent => Response::OperationStatus {
                                operation,
                                readiness: crate::OperationReadiness::Absent,
                                publication: None,
                                results: Vec::new(),
                            },
                            runtime::OperationStatus::Found {
                                receipt,
                                publication,
                            } => {
                                let effect = contract::IssueEffect::from_json(&receipt.effect)
                                    .ok_or_else(|| {
                                        Response::err("the durable operation receipt is corrupt")
                                    })?;
                                let (readiness, publication) = match publication {
                                    runtime::OperationPublication::Ready(publication) => {
                                        (crate::OperationReadiness::Ready, Some(publication))
                                    }
                                    runtime::OperationPublication::Building => {
                                        (crate::OperationReadiness::Building, None)
                                    }
                                    runtime::OperationPublication::Capacity => {
                                        (crate::OperationReadiness::Capacity, None)
                                    }
                                    runtime::OperationPublication::ImplementationUnavailable => {
                                        (crate::OperationReadiness::ImplementationUnavailable, None)
                                    }
                                    runtime::OperationPublication::GenerationUnavailable => {
                                        (crate::OperationReadiness::GenerationUnavailable, None)
                                    }
                                    runtime::OperationPublication::Unavailable => {
                                        (crate::OperationReadiness::Unavailable, None)
                                    }
                                };
                                Response::OperationStatus {
                                    operation,
                                    readiness,
                                    publication,
                                    results: effect
                                        .results
                                        .into_iter()
                                        .map(|result| crate::ChangeEffect {
                                            operation: result.operation,
                                            kind: result.kind,
                                            id: result.id,
                                        })
                                        .collect(),
                                }
                            }
                        },
                        false,
                    ));
                }
                let effect = self
                    .submit_with_request(&semantic, request)
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::ChangeSet {
                        results: effect
                            .results
                            .into_iter()
                            .map(|result| crate::ChangeEffect {
                                operation: result.operation,
                                kind: result.kind,
                                id: result.id,
                            })
                            .collect(),
                    },
                    !effect.unchanged,
                ))
            }
            Request::Inbox {
                watermark,
                page,
                publication,
            } => {
                let page: issues::contract::Page<issues::dto::InboxEntry> = self
                    .query_coordinate(
                        &IssueQuery::Inbox {
                            exclude_device: Some(facts.device.clone()),
                            page,
                        },
                        publication,
                    )
                    .map_err(Self::effect_err)?;
                let unread_on_page = page
                    .items
                    .iter()
                    .filter(|entry| entry.ts > watermark)
                    .count() as u64;
                Ok((
                    Response::Inbox {
                        page,
                        unread_on_page,
                    },
                    false,
                ))
            }
            Request::AccessPlan { role, project } => {
                let plan = crate::host::plan_access_grant(self.session, &role, project.as_deref())
                    .map_err(|error| match error {
                        crate::host::AccessRefusal::NotFound(message) => {
                            Response::not_found(message)
                        }
                        crate::host::AccessRefusal::Invalid(message) => Response::err(message),
                    })?;
                Ok((
                    Response::AccessPlan {
                        assignments: plan.assignments,
                    },
                    false,
                ))
            }
            Request::IssueNew {
                title,
                project,
                project_hint: _,
                assignees,
                priority,
                labels,
                body,
                due,
                estimate,
            } => {
                let project = self.choose_project(&selectors, project.as_deref(), facts)?;
                let duedate = match due.as_deref() {
                    None | Some("none") => None,
                    Some(text) => Some(parse_due(text).ok_or_else(bad_due)?),
                };
                let resolved_assignees: Vec<String> = assignees.to_vec();
                let mut operations = Vec::new();
                let mut change_labels = Vec::new();
                for label in &labels {
                    match selectors.resolve_label(label) {
                        Some(id) => {
                            change_labels.push(contract::ChangeLabel::Existing { label: id })
                        }
                        None => {
                            let operation = u16::try_from(operations.len())
                                .map_err(|_| Response::err("too many labels for one issue"))?;
                            operations.push(contract::ChangeOperation::LabelCreate {
                                name: label.clone(),
                                color: "gray".into(),
                            });
                            change_labels.push(contract::ChangeLabel::Created { operation });
                        }
                    }
                }
                // A body that already declares itself a document is passed
                // through, because escaping it would render an author's
                // headings and emphasis as literal text. Passed through is not
                // the same as unchecked: it must at least compile, or the issue
                // is created carrying a document nothing can render.
                //
                // What this cannot check is *canonicality* — whether the source
                // survives the editor's projection round-trip — because that is
                // defined by a serializer that lives in the viewer and has no
                // counterpart here. A non-canonical document is safe (the World
                // refuses a splice whose `base_len` disagrees) but not editable
                // in the browser until it is normalized through
                // `IssueDocumentUpgrade`.
                let body = match body {
                    Some(body) if body.starts_with(contract::DOCUMENT_PREFIX) => {
                        if !crate::document::compile_document(&body).valid {
                            return Err(Response::err(
                                "the description declares a document that does not compile",
                            ));
                        }
                        Some(body)
                    }
                    Some(body) => Some(crate::document::plain_document(&body)),
                    None => None,
                };
                operations.push(contract::ChangeOperation::IssueCreate {
                    project: contract::ChangeProject::Existing { project },
                    title,
                    priority: priority.unwrap_or_else(|| "none".into()),
                    status: None,
                    parent: None,
                    assignees: resolved_assignees,
                    labels: change_labels,
                    body,
                    due: duedate,
                    estimate,
                });
                let effect = self
                    .submit(&IssueIntent::ChangeSet {
                        operations,
                        ts: facts.now,
                    })
                    .map_err(Self::effect_err)?;
                let doc = effect
                    .results
                    .iter()
                    .find(|result| result.kind == "issue")
                    .map(|result| result.id.clone())
                    .ok_or_else(|| Response::err("issue create returned no issue result"))?;
                let reff = self.reff_for(&self.selectors(), &doc);
                Ok((Response::Ref { reff }, true))
            }
            Request::IssueEdit {
                reff,
                title,
                status,
                priority,
                description,
                due,
                estimate,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let description = if let Some(description) = description {
                    let issue: IssueView = self
                        .query(&IssueQuery::View {
                            doc: doc.clone(),
                            me: None,
                        })
                        .map_err(Self::effect_err)?;
                    let description = if issue.document_schema == contract::DOCUMENT_SCHEMA_VERSION
                        && !description.starts_with(contract::DOCUMENT_PREFIX)
                    {
                        crate::document::plain_document(&description)
                    } else {
                        description
                    };
                    if description.starts_with(contract::DOCUMENT_PREFIX)
                        && !crate::document::compile_document(&description).valid
                    {
                        return Err(Response::err(
                            "the description declares a document that does not compile",
                        ));
                    }
                    Some(description)
                } else {
                    None
                };
                // `none` clears; absent leaves the field untouched — the
                // double-option the intent carries.
                let duedate = match due.as_deref() {
                    None => None,
                    Some("none") => Some(None),
                    Some(text) => Some(Some(parse_due(text).ok_or_else(bad_due)?)),
                };
                let estimate = match estimate.as_deref() {
                    None => None,
                    Some("none") => Some(None),
                    Some(text) => Some(Some(text.parse::<u32>().map_err(|_| {
                        Response::err("estimate must be a whole number of points, or `none`")
                    })?)),
                };
                self.submit(&IssueIntent::IssueEdit {
                    doc: doc.clone(),
                    title,
                    status,
                    priority,
                    description,
                    duedate,
                    estimate,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::IssueTextSplice {
                reff,
                index,
                delete,
                insert,
                base_len,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                self.submit(&IssueIntent::IssueTextSplice {
                    doc: doc.clone(),
                    index,
                    delete,
                    insert,
                    base_len,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::IssueDocumentUpgrade {
                reff,
                expected,
                splices,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let upgraded = apply_document_splices(&expected, &splices)
                    .ok_or_else(|| Response::err("document upgrade could not be completed"))?;
                if !crate::document::compile_document(&upgraded).valid {
                    return Err(Response::err("document upgrade could not be completed"));
                }
                self.submit(&IssueIntent::IssueDocumentUpgrade {
                    doc: doc.clone(),
                    expected,
                    splices: splices
                        .into_iter()
                        .map(|splice| issues::contract::DocumentSplice {
                            index: splice.index,
                            delete: splice.delete,
                            insert: splice.insert,
                        })
                        .collect(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::IssueTextCheckpoint { reff } => {
                let doc = self.resolve(&selectors, &reff)?;
                self.submit(&IssueIntent::IssueTextCheckpoint {
                    doc: doc.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::IssueMove { reff, project, pos } => {
                let position = pos.map(|position| match position {
                    BoardPos::Top => contract::ChangePosition::Top,
                    BoardPos::Bottom => contract::ChangePosition::Bottom,
                    BoardPos::Before { reff } => contract::ChangePosition::Before { issue: reff },
                    BoardPos::After { reff } => contract::ChangePosition::After { issue: reff },
                });
                let effect = self
                    .submit_change(
                        contract::ChangeOperation::IssueMove {
                            issue: reff.clone(),
                            project: project
                                .map(|project| contract::ChangeProject::Existing { project }),
                            position,
                        },
                        facts.now,
                    )
                    .map_err(Self::effect_err)?;
                let doc = effect.doc.unwrap_or(reff);
                Ok((self.ref_response(&doc), true))
            }
            Request::Assign { reff, who, add } => {
                let doc = self.resolve(&selectors, &reff)?;
                self.submit(&IssueIntent::Assign {
                    doc: doc.clone(),
                    who,
                    add,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::Label { reff, add, remove } => {
                let doc = self.resolve(&selectors, &reff)?;
                let mut add_ids = Vec::new();
                let mut new_labels = Vec::new();
                for label in &add {
                    match selectors.resolve_label(label) {
                        Some(id) => add_ids.push(id),
                        None => new_labels.push(NewLabel {
                            id: LabelId::mint(self.clock).as_str().to_string(),
                            name: label.clone(),
                            color: "gray".into(),
                        }),
                    }
                }
                let remove_ids: Vec<String> = remove
                    .iter()
                    .filter_map(|l| selectors.resolve_label(l))
                    .collect();
                self.submit(&IssueIntent::Label {
                    doc: doc.clone(),
                    add: add_ids,
                    new_labels,
                    remove: remove_ids,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::Comment {
                reff,
                body,
                reply_to,
            } => {
                self.submit_change(
                    contract::ChangeOperation::IssueComment {
                        issue: reff.clone(),
                        body,
                        parent: reply_to,
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff }, true))
            }
            Request::CommentAt {
                reff,
                body,
                field,
                start,
                end,
                reply_to,
                source,
            } => {
                self.submit_change(
                    contract::ChangeOperation::IssueCommentAt {
                        issue: reff.clone(),
                        body,
                        field,
                        start,
                        end,
                        parent: reply_to,
                        source: source.parse().ok_or_else(|| {
                            Response::invalid("source must be an exact world publication")
                        })?,
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff }, true))
            }
            Request::React {
                reff,
                comment,
                emoji,
                on,
            } => {
                self.submit_change(
                    contract::ChangeOperation::IssueReaction {
                        issue: reff.clone(),
                        comment,
                        emoji,
                        on,
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff }, true))
            }
            Request::IssueDelete { reff } => {
                self.submit_change(
                    contract::ChangeOperation::IssueTombstone {
                        issue: reff.clone(),
                        on: true,
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some(format!("deleted {reff}")),
                    },
                    true,
                ))
            }
            Request::IssueRestore { reff } => {
                self.submit_change(
                    contract::ChangeOperation::IssueTombstone {
                        issue: reff.clone(),
                        on: false,
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some(format!("restored {reff}")),
                    },
                    true,
                ))
            }
            Request::IssueLink { reff, kind, target } => self.link(reff, kind, target, true, facts),
            Request::IssueUnlink { reff, kind, target } => {
                self.link(reff, kind, target, false, facts)
            }
            Request::IssueParent { reff, parent } => {
                self.submit_change(
                    contract::ChangeOperation::IssueParent {
                        issue: reff.clone(),
                        parent,
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff }, true))
            }
            Request::IssueStart { reff } => self.work(&selectors, reff, WorkAction::Start, facts),
            Request::IssueDone { reff } => self.work(&selectors, reff, WorkAction::Done, facts),
            Request::IssueStop { reff } => self.work(&selectors, reff, WorkAction::Stop, facts),
            Request::Verify {
                reff,
                source,
                build,
            } => {
                let package_filled = build.trim().is_empty();
                let build = if package_filled {
                    issues::contract::verify_build_hex(crate::lifecycle::implementation_id())
                } else {
                    build
                };
                let doc = self.resolve(&selectors, &reff)?;
                let request = RequestId::mint();
                let run = runtime::exec::derive_run_id(
                    self.session.space_id(),
                    self.session.world_id(),
                    self.identity.device(),
                    request.as_bytes(),
                    0,
                );
                let run = data_encoding::HEXLOWER.encode(&run.as_bytes());
                let effect = self
                    .submit_with_request(
                        &IssueIntent::Verify {
                            doc: doc.clone(),
                            run: run.clone(),
                            source,
                            build,
                            package_filled,
                            actor: facts.actor.clone(),
                            device: facts.device.clone(),
                            ts: facts.now,
                        },
                        request,
                    )
                    .map_err(Self::effect_err)?;
                if effect.run.as_deref() != Some(run.as_str()) {
                    return Err(Response::err(
                        "verification committed without its expected Run binding",
                    ));
                }
                Ok((
                    Response::Check {
                        reff: self.reff_for(&selectors, &doc),
                        run,
                    },
                    true,
                ))
            }
            Request::AcceptCheck {
                reff,
                run,
                attempt,
                report,
                verdict,
                move_to_done,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let effect = self
                    .submit(&IssueIntent::AcceptCheck {
                        doc: doc.clone(),
                        run: run.clone(),
                        attempt,
                        report,
                        verdict,
                        move_to_done,
                        id: issues::ids::mint_attachment_id(self.clock),
                        actor: facts.actor.clone(),
                        device: facts.device.clone(),
                        ts: facts.now,
                    })
                    .map_err(Self::effect_err)?;
                if effect.run.as_deref() != Some(run.as_str()) {
                    return Err(Response::err(
                        "check acceptance committed without its expected Run binding",
                    ));
                }
                Ok((
                    Response::Check {
                        reff: self.reff_for(&selectors, &doc),
                        run,
                    },
                    true,
                ))
            }
            Request::IssueView { reff } => {
                let doc = self.resolve(&selectors, &reff)?;
                // "Show me this Issue" means the Issue, not a summary of it
                // with its discussion and attachments left blank. The World
                // keeps those bounded by serving them as separate pages --
                // `IssueQuery::View` is that bounded core and says so -- and
                // assembling the first page of each into one answer is this
                // layer's job. `IssueDetail` remains the way to page further
                // into any one of them.
                // Reactions are paged independently of the comments they
                // mark, so the default page would leave a busy thread's later
                // reactions unread and those comments would draw as though
                // nobody had reacted. Ask for as many as this Issue is
                // allowed to have across the comment page it accompanies;
                // what is still missing after that is reported rather than
                // rendered as absence.
                let mut pages = issues::contract::IssueDetailPages::default();
                pages.reactions.limit = REACTIONS_PER_VIEW;
                let detail: issues::contract::IssueDetailProjection = self
                    .query(&IssueQuery::Detail {
                        doc,
                        me: Some(facts.actor.clone()),
                        pages,
                    })
                    .map_err(Self::effect_err)?;
                Ok((Response::Issue(Box::new(assemble(detail))), false))
            }
            Request::IssueDetail { reff, publication } => {
                let doc = self.resolve(&selectors, &reff)?;
                let query = IssueQuery::Detail {
                    doc,
                    me: Some(facts.actor.clone()),
                    pages: issues::contract::IssueDetailPages::default(),
                };
                let view: issues::contract::IssueDetailProjection = match publication {
                    Some(publication) => self.query_exact(&query, publication),
                    None => self.query(&query),
                }
                .map_err(Self::effect_err)?;
                Ok((Response::IssueDetail(Box::new(view)), false))
            }
            Request::List {
                project,
                filter,
                page,
            } => {
                let project = match project {
                    Some(p) => Some(
                        selectors
                            .resolve_project(&p)
                            .ok_or_else(|| Response::not_found(format!("no project {p:?}")))?,
                    ),
                    None => None,
                };
                // A milestone belongs to exactly one project, so there is no
                // catalog to resolve the name against without one — and silently
                // listing everything would be the worst answer to a filter the
                // caller asked for. Both misses are loud.
                let milestone = match &filter.milestone {
                    Some(m) => {
                        let project = project.as_deref().ok_or_else(|| {
                            Response::not_found(
                                "a milestone filter needs a project to resolve it in".to_string(),
                            )
                        })?;
                        Some(
                            selectors.resolve_milestone(project, m).ok_or_else(|| {
                                Response::not_found(format!("no milestone {m:?}"))
                            })?,
                        )
                    }
                    None => None,
                };
                let query = IssueQuery::List {
                    project,
                    label: filter.label.and_then(|l| selectors.resolve_label(&l)),
                    status: filter.status,
                    milestone,
                    mine: filter.mine.then(|| facts.actor.clone()),
                    all: filter.all,
                    me: Some(facts.actor.clone()),
                    page: page.clone(),
                };
                let page: issues::contract::Page<Row> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::List { page }, false))
            }
            Request::Board {
                project,
                project_hint: _,
                page,
            } => {
                let project = self.choose_project(&selectors, project.as_deref(), facts)?;
                let query = IssueQuery::Board {
                    project,
                    me: Some(facts.actor.clone()),
                    page: page.clone(),
                };
                let view: issues::dto::BoardPage =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Board(Box::new(view)), false))
            }
            Request::Geometry {
                project,
                roots,
                publication,
                page,
            } => {
                let id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                let query = IssueQuery::Geometry {
                    project: id,
                    roots,
                    page,
                };
                let view: issues::contract::GeometryProjection = match publication {
                    Some(publication) => self.query_pinned(
                        &query,
                        publication.parse().ok_or_else(|| {
                            Response::invalid("publication digests must be 64 lowercase hex bytes")
                        })?,
                    ),
                    None => self.query(&query),
                }
                .map_err(Self::effect_err)?;
                Ok((Response::Geometry(Box::new(view)), false))
            }
            Request::History {
                reff,
                publication,
                page,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let page: issues::contract::Page<issues::dto::ActivityEvent> = self
                    .query_coordinate(&IssueQuery::History { doc, page }, publication)
                    .map_err(Self::effect_err)?;
                Ok((Response::Activity { page }, false))
            }
            Request::IssueRelations {
                reff,
                direction,
                publication,
                page,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let query = IssueQuery::Relations {
                    doc,
                    direction,
                    page,
                };
                let page: issues::contract::Page<issues::dto::IssueRelationDto> = self
                    .query_coordinate(&query, publication)
                    .map_err(Self::effect_err)?;
                Ok((Response::Relations { page }, false))
            }
            Request::IssueComments {
                reff,
                publication,
                page,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let query = IssueQuery::Comments { doc, page };
                let page: issues::contract::Page<issues::dto::CommentDto> = self
                    .query_coordinate(&query, publication)
                    .map_err(Self::effect_err)?;
                Ok((Response::Comments { page }, false))
            }
            Request::IssueReactions {
                reff,
                publication,
                page,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let query = IssueQuery::Reactions { doc, page };
                let page: issues::contract::Page<issues::records::ReactionRecord> = self
                    .query_coordinate(&query, publication)
                    .map_err(Self::effect_err)?;
                Ok((Response::Reactions { page }, false))
            }
            Request::IssueAttachments {
                reff,
                publication,
                page,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let query = IssueQuery::Attachments { doc, page };
                let page: issues::contract::Page<issues::dto::AttachmentMetaDto> = self
                    .query_coordinate(&query, publication)
                    .map_err(Self::effect_err)?;
                Ok((Response::Attachments { page }, false))
            }
            Request::IssueChecks {
                reff,
                publication,
                page,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                let query = IssueQuery::Checks { doc, page };
                let page: issues::contract::Page<issues::dto::CheckDto> = self
                    .query_coordinate(&query, publication)
                    .map_err(Self::effect_err)?;
                Ok((Response::Checks { page }, false))
            }
            Request::Activity { page } => {
                let query = IssueQuery::Activity { page: page.clone() };
                let page: issues::contract::Page<issues::dto::ActivityEvent> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Activity { page }, false))
            }
            Request::ProjectNew { name, key, color } => {
                self.submit(&IssueIntent::ChangeSet {
                    operations: vec![contract::ChangeOperation::ProjectCreate {
                        name,
                        key: key.clone(),
                        color: color.unwrap_or_else(|| "blue".into()),
                    }],
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ref {
                        reff: key.trim().to_ascii_uppercase(),
                    },
                    true,
                ))
            }
            Request::ProjectList { page } => {
                let query = IssueQuery::Projects { page: page.clone() };
                let page: issues::contract::Page<ProjectDto> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Projects { page }, false))
            }
            Request::ProjectEdit {
                project,
                name,
                color,
                description,
                lead,
                start,
                target,
                archived,
                team,
            } => {
                let id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                let parse_date = |v: Option<String>| -> Result<Option<Option<u64>>, Response> {
                    match v.as_deref() {
                        None => Ok(None),
                        Some("none") | Some("") => Ok(Some(None)),
                        Some(text) => Ok(Some(Some(parse_due(text).ok_or_else(bad_due)?))),
                    }
                };
                let lead = lead.map(|l| {
                    let l = l.trim();
                    if l.eq_ignore_ascii_case("none") {
                        String::new()
                    } else {
                        l.to_string()
                    }
                });
                let team = match team.as_deref().map(str::trim) {
                    None => None,
                    Some("") | Some("none") => Some(String::new()),
                    Some(sel) => Some(
                        selectors
                            .resolve_team(sel)
                            .ok_or_else(|| Response::not_found(format!("no team matches {sel:?}")))?
                            .0,
                    ),
                };
                self.submit(&IssueIntent::ProjectEdit {
                    id,
                    name,
                    color,
                    description,
                    lead,
                    start_date: parse_date(start)?,
                    target_date: parse_date(target)?,
                    archived,
                    team,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: project }, true))
            }
            Request::ProjectDelete { project } => {
                let id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                self.submit(&IssueIntent::ProjectDelete {
                    id,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(|e| match e {
                    SessionFailure::Rejected(Rejection::Conflict)
                    | SessionFailure::Conflict(SessionConflict::Body) => Response::err(
                        "that project still has issues (live or deleted) — move them with \
                         `issue move`, or archive the project instead; only an empty project \
                         can be hard-deleted",
                    ),
                    other => Self::effect_err(other),
                })?;
                Ok((
                    Response::Ok {
                        message: Some(format!("deleted project {project} (it was empty)")),
                    },
                    true,
                ))
            }
            Request::Follow { reff, on } => {
                let doc = self.resolve(&selectors, &reff)?;
                self.submit(&IssueIntent::Follow {
                    doc: doc.clone(),
                    actor: facts.actor.clone(),
                    on,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::MilestoneList { project, page } => {
                let id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                let query = IssueQuery::Milestones {
                    project: id,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::dto::MilestoneDto> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Milestones { page }, false))
            }
            Request::MilestoneSet {
                project,
                milestone,
                name,
                description,
                target,
                pos,
                remove,
            } => {
                let project_id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                let id = match &milestone {
                    Some(sel) => {
                        selectors
                            .resolve_milestone(&project_id, sel)
                            .ok_or_else(|| {
                                Response::not_found(format!("no milestone matches {sel:?}"))
                            })?
                    }
                    None => issues::ids::mint_milestone_id(self.clock),
                };
                let target_date = match target.as_deref() {
                    None => None,
                    Some("none") | Some("") => Some(None),
                    Some(text) => Some(Some(parse_due(text).ok_or_else(bad_due)?)),
                };
                // `Before`/`After` name a sibling milestone, resolved in the same
                // project — the World takes ids, and a name is this layer's job.
                let resolve_sibling = |reff: &str| {
                    selectors
                        .resolve_milestone(&project_id, reff)
                        .ok_or_else(|| {
                            Response::not_found(format!("no milestone matches {reff:?}"))
                        })
                };
                let pos = match &pos {
                    None => None,
                    Some(BoardPos::Top) => Some(Pos::Top),
                    Some(BoardPos::Bottom) => Some(Pos::Bottom),
                    Some(BoardPos::Before { reff }) => Some(Pos::Before {
                        doc: resolve_sibling(reff)?,
                    }),
                    Some(BoardPos::After { reff }) => Some(Pos::After {
                        doc: resolve_sibling(reff)?,
                    }),
                };
                self.submit(&IssueIntent::MilestoneSet {
                    project_id,
                    id: id.clone(),
                    name,
                    description,
                    target_date,
                    pos,
                    tombstone: remove.then_some(true),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: id }, true))
            }
            Request::IssueMilestone { reff, milestone } => {
                self.submit_change(
                    contract::ChangeOperation::IssueMilestone {
                        issue: reff.clone(),
                        milestone: milestone.and_then(|milestone| {
                            let milestone = milestone.trim();
                            (!milestone.is_empty() && milestone != "none")
                                .then(|| milestone.to_string())
                        }),
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&reff), true))
            }
            Request::CycleList { project, page } => {
                let id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                let query = IssueQuery::Cycles {
                    project: id,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::dto::CycleDto> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Cycles { page }, false))
            }
            Request::CycleSet {
                project,
                cycle,
                name,
                start,
                end,
                remove,
            } => {
                let project_id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                let id = match &cycle {
                    Some(sel) => selectors
                        .resolve_cycle(&project_id, sel)
                        .ok_or_else(|| Response::not_found(format!("no cycle matches {sel:?}")))?,
                    None => issues::ids::mint_cycle_id(self.clock),
                };
                let parse_edge = |v: Option<String>| -> Result<Option<Option<u64>>, Response> {
                    match v.as_deref() {
                        None => Ok(None),
                        Some("none") | Some("") => Ok(Some(None)),
                        Some(text) => Ok(Some(Some(parse_due(text).ok_or_else(bad_due)?))),
                    }
                };
                self.submit(&IssueIntent::CycleSet {
                    project_id,
                    id: id.clone(),
                    name,
                    start: parse_edge(start)?,
                    end: parse_edge(end)?,
                    tombstone: remove.then_some(true),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: id }, true))
            }
            Request::IssueCycle { reff, cycle } => {
                let doc = self.resolve(&selectors, &reff)?;
                let cycle = match cycle.as_deref().map(str::trim) {
                    None | Some("") | Some("none") => None,
                    Some(sel) => {
                        let view: IssueView = self
                            .query(&IssueQuery::View {
                                doc: doc.clone(),
                                me: None,
                            })
                            .map_err(Self::effect_err)?;
                        let project = view.project_id.as_str().to_string();
                        Some(selectors.resolve_cycle(&project, sel).ok_or_else(|| {
                            Response::not_found(format!(
                                "no cycle matches {sel:?} in this issue's project"
                            ))
                        })?)
                    }
                };
                self.submit(&IssueIntent::IssueCycle {
                    doc: doc.clone(),
                    cycle,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::InitiativeList { page } => {
                let query = IssueQuery::Initiatives { page: page.clone() };
                let page: issues::contract::Page<issues::dto::InitiativeDto> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Initiatives { page }, false))
            }
            Request::InitiativeSet {
                initiative,
                name,
                description,
                owner,
                health,
                target,
                add_projects,
                remove_projects,
                remove,
            } => {
                let current = match &initiative {
                    Some(sel) => Some(selectors.resolve_initiative(sel).ok_or_else(|| {
                        Response::not_found(format!("no initiative matches {sel:?}"))
                    })?),
                    None => None,
                };
                let id = current
                    .as_ref()
                    .map(|(id, _)| id.clone())
                    .unwrap_or_else(|| issues::ids::mint_initiative_id(self.clock));
                let add_projects = add_projects
                    .iter()
                    .map(|selector| {
                        selectors.resolve_project(selector).ok_or_else(|| {
                            Response::not_found(format!("no project matches {selector:?}"))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let remove_projects = remove_projects
                    .iter()
                    .map(|selector| {
                        selectors.resolve_project(selector).ok_or_else(|| {
                            Response::not_found(format!("no project matches {selector:?}"))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let owner = owner.map(|o| {
                    let o = o.trim();
                    if o.eq_ignore_ascii_case("none") {
                        String::new()
                    } else {
                        o.to_string()
                    }
                });
                let target_date = match target.as_deref() {
                    None => None,
                    Some("none") | Some("") => Some(None),
                    Some(text) => Some(Some(parse_due(text).ok_or_else(bad_due)?)),
                };
                self.submit(&IssueIntent::InitiativeSet {
                    id: id.clone(),
                    name,
                    description,
                    owner,
                    health,
                    target_date,
                    add_projects,
                    remove_projects,
                    tombstone: remove.then_some(true),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: id }, true))
            }
            Request::TeamList { page } => {
                let query = IssueQuery::Teams { page: page.clone() };
                let page: issues::contract::Page<issues::dto::TeamDto> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Teams { page }, false))
            }
            Request::TeamSet {
                team,
                name,
                key,
                icon,
                lead,
                add_members,
                remove_members,
                remove,
            } => {
                let current =
                    match &team {
                        Some(sel) => Some(selectors.resolve_team(sel).ok_or_else(|| {
                            Response::not_found(format!("no team matches {sel:?}"))
                        })?),
                        None => None,
                    };
                let id = current
                    .as_ref()
                    .map(|(id, _)| id.clone())
                    .unwrap_or_else(|| issues::ids::mint_team_id(self.clock));
                let add_members = add_members
                    .into_iter()
                    .map(|actor| actor.trim().to_string())
                    .collect();
                let remove_members = remove_members
                    .into_iter()
                    .map(|actor| actor.trim().to_string())
                    .collect();
                let lead = lead.map(|l| {
                    let l = l.trim();
                    if l.eq_ignore_ascii_case("none") {
                        String::new()
                    } else {
                        l.to_string()
                    }
                });
                self.submit(&IssueIntent::TeamSet {
                    id: id.clone(),
                    name,
                    key,
                    icon,
                    lead,
                    add_members,
                    remove_members,
                    tombstone: remove.then_some(true),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: id }, true))
            }
            Request::TriageList { page } => {
                let query = IssueQuery::Triage { page: page.clone() };
                let page: issues::contract::Page<issues::records::TriageRecord> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::TriageItems { page }, false))
            }
            Request::TriageSubmit {
                title,
                body,
                source,
            } => {
                let id = issues::ids::mint_triage_id(self.clock);
                self.submit(&IssueIntent::TriageSubmit {
                    id: id.clone(),
                    title,
                    body: body.unwrap_or_default(),
                    source: source.unwrap_or_else(|| "cli".into()),
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: id }, true))
            }
            Request::TriageDecide {
                id,
                outcome,
                project,
                target,
                note,
            } => {
                let outcome = outcome.trim().to_ascii_lowercase();
                let (project_id, doc) = match outcome.as_str() {
                    "accepted" => {
                        let sel = project.as_deref().ok_or_else(|| {
                            Response::err("accepting needs a project: pass -p <project>")
                        })?;
                        let project_id = selectors.resolve_project(sel).ok_or_else(|| {
                            Response::not_found(format!("no project matches {sel:?}"))
                        })?;
                        let doc = DocId::mint(self.clock).as_str().to_string();
                        (Some(project_id), Some(doc))
                    }
                    "duplicate" => {
                        let sel = target.as_deref().ok_or_else(|| {
                            Response::err("duplicate needs the existing issue: pass its ref")
                        })?;
                        (None, Some(self.resolve(&selectors, sel)?))
                    }
                    _ => (None, None),
                };
                let effect = self
                    .submit(&IssueIntent::TriageDecide {
                        id: id.clone(),
                        outcome: outcome.clone(),
                        project: project_id,
                        doc,
                        note: note.unwrap_or_default(),
                        actor: facts.actor.clone(),
                        device: facts.device.clone(),
                        ts: facts.now,
                    })
                    .map_err(|e| match e {
                        SessionFailure::Rejected(Rejection::Conflict)
                        | SessionFailure::Conflict(SessionConflict::Body) => {
                            Response::err("that triage item was already decided")
                        }
                        other => Self::effect_err(other),
                    })?;
                let message = match (outcome.as_str(), &effect.doc) {
                    ("accepted", Some(doc)) => {
                        format!("accepted into {}", self.reff_for(&self.selectors(), doc))
                    }
                    ("duplicate", _) => "marked duplicate".into(),
                    _ => "declined".into(),
                };
                Ok((
                    Response::Ok {
                        message: Some(message),
                    },
                    true,
                ))
            }
            Request::Attach {
                reff,
                name,
                mime,
                content,
                size,
                comment,
            } => {
                let doc = self.resolve(&selectors, &reff)?;
                self.submit(&IssueIntent::Attach {
                    doc: doc.clone(),
                    id: issues::ids::mint_attachment_id(self.clock),
                    name,
                    mime: mime.unwrap_or_else(|| "application/octet-stream".into()),
                    content,
                    size,
                    comment,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(|e| match e {
                    // One refusal, one cause. The byte cap left with the
                    // inline write path — the Station's `max_content_len` is
                    // what bounds a file now, and it refuses at upload, long
                    // before an intent is built.
                    SessionFailure::Rejected(Rejection::LimitExceeded) => Response::err(format!(
                        "attachment refused: at most {} files per issue",
                        contract::MAX_ATTACHMENTS_PER_ISSUE,
                    )),
                    other => Self::effect_err(other),
                })?;
                Ok((self.ref_response(&doc), true))
            }
            Request::Detach { reff, id } => {
                let doc = self.resolve(&selectors, &reff)?;
                self.submit(&IssueIntent::Detach {
                    doc: doc.clone(),
                    id,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((self.ref_response(&doc), true))
            }
            Request::AttachmentGet { reff, id } => {
                let doc = self.resolve(&selectors, &reff)?;
                let record: serde_json::Value = self
                    .query(&IssueQuery::Attachment { doc, id })
                    .map_err(Self::effect_err)?;
                // Both eras, and neither faked. A record written after the
                // cutover names a content and carries no bytes; one written
                // before carries bytes and names nothing. The previous shape
                // defaulted a missing `data_b64` to `""`, which a caller then
                // decoded to zero bytes and wrote out as a 0-byte file
                // reporting success — the one outcome worse than an error.
                let content = record["content"].as_str().map(str::to_string);
                let data_b64 = record["data_b64"].as_str().map(str::to_string);
                if content.is_none() && data_b64.is_none() {
                    return Err(Response::err(
                        "this attachment record carries neither bytes nor a content id",
                    ));
                }
                Ok((
                    Response::Attachment {
                        name: record["name"].as_str().unwrap_or_default().to_string(),
                        mime: record["mime"].as_str().unwrap_or_default().to_string(),
                        content,
                        data_b64,
                        size: record["size"].as_u64().unwrap_or_default(),
                    },
                    false,
                ))
            }
            Request::ProjectUpdates { project, page } => {
                let id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                let query = IssueQuery::ProjectUpdates {
                    project: id,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::dto::ProjectUpdateDto> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Updates { page }, false))
            }
            Request::ProjectUpdatePost {
                project,
                body,
                health,
            } => {
                let id = selectors.resolve_project(&project).ok_or_else(|| {
                    Response::not_found(format!("no project matches {project:?}"))
                })?;
                self.submit(&IssueIntent::ProjectUpdatePost {
                    project_id: id,
                    id: issues::ids::mint_update_id(self.clock),
                    author: facts.actor.clone(),
                    body,
                    health: health.unwrap_or_default(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: project }, true))
            }
            Request::LabelNew { name, color } => {
                self.submit_change(
                    contract::ChangeOperation::LabelCreate {
                        name: name.clone(),
                        color: color.unwrap_or_else(|| "gray".into()),
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: name }, true))
            }
            Request::LabelList { page } => {
                let query = IssueQuery::Labels { page: page.clone() };
                let page: issues::contract::Page<LabelDto> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Labels { page }, false))
            }
            Request::LabelShow { label, publication } => {
                let result: contract::LabelProjection = self
                    .query_exact(&IssueQuery::Label { label }, publication)
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Label {
                        publication: result.publication,
                        label: result.label,
                    },
                    false,
                ))
            }
            Request::LabelEdit { label, name, color } => {
                self.submit_change(
                    contract::ChangeOperation::LabelEdit {
                        label: label.clone(),
                        name,
                        color,
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: label }, true))
            }
            Request::LabelDelete { label } => {
                self.submit_change(
                    contract::ChangeOperation::LabelDelete {
                        label: label.clone(),
                    },
                    facts.now,
                )
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: label }, true))
            }
            Request::SpaceRename { name } => {
                self.submit(&IssueIntent::SpaceRename {
                    name: name.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ref { reff: name }, true))
            }
            Request::SpaceDescribe { description } => {
                self.submit(&IssueIntent::SpaceDescribe {
                    description,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((Response::Ok { message: None }, true))
            }
            Request::RoleList { page } => {
                let query = IssueQuery::Roles { page: page.clone() };
                let roles: issues::contract::Page<issues::contract::RoleProjection> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Roles { page: roles }, false))
            }
            Request::RoleShow { role } => {
                let view: issues::contract::RoleProjection = self
                    .query(&IssueQuery::RoleShow { role })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Text {
                        text: serde_json::to_string_pretty(&view).unwrap_or_default(),
                    },
                    false,
                ))
            }
            Request::RoleCreate {
                name,
                description,
                project,
                capabilities,
            } => {
                // The adapter mints the id and resolves the project selector;
                // the World re-validates everything.
                let scope_project = match project {
                    None => None,
                    Some(sel) => Some(
                        selectors
                            .resolve_project(&sel)
                            .ok_or_else(|| Response::not_found("no such project"))?,
                    ),
                };
                let role_id = format!(
                    "role_{}",
                    issues::ids::ProjectId::mint(self.clock)
                        .as_str()
                        .trim_start_matches("prj_")
                );
                self.submit(&IssueIntent::RoleCreate {
                    role_id: role_id.clone(),
                    scope_project,
                    name,
                    description: description.unwrap_or_default(),
                    capabilities,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some(format!("created role {role_id}")),
                    },
                    true,
                ))
            }
            Request::RoleEdit {
                role,
                expect_revision,
                name,
                description,
                capabilities,
            } => {
                self.submit(&IssueIntent::RoleEdit {
                    role_id: role.clone(),
                    expected_revision: expect_revision,
                    name,
                    description,
                    capabilities,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some(format!("edited role {role} (a new revision is the head)")),
                    },
                    true,
                ))
            }
            Request::RoleDelete {
                role,
                expect_revision,
            } => {
                self.submit(&IssueIntent::RoleDelete {
                    role_id: role.clone(),
                    expected_revision: expect_revision,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some(format!(
                            "tombstoned role {role} — existing assignments keep their \
                             originally granted expansion until explicitly revoked"
                        )),
                    },
                    true,
                ))
            }
            Request::RoleResolve {
                role,
                expect_heads,
                body_json,
            } => {
                self.submit(&IssueIntent::RoleResolve {
                    role_id: role.clone(),
                    expected_heads: expect_heads,
                    body_json,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some(format!("resolved role {role} to one head")),
                    },
                    true,
                ))
            }
            Request::WorkflowShow { project } => {
                let project = selectors
                    .resolve_project(&project)
                    .ok_or_else(|| Response::not_found("no such project"))?;
                let view: issues::contract::WorkflowProjection = self
                    .query(&IssueQuery::Workflow { project })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Text {
                        text: serde_json::to_string_pretty(&view).unwrap_or_default(),
                    },
                    false,
                ))
            }
            Request::WorkflowValidate { body_json } => {
                // Pure local validation — nothing is committed.
                match serde_json::from_str::<issues::workflow::WorkflowBody>(&body_json) {
                    Ok(body) => match body.validate() {
                        Ok(()) => Ok((
                            Response::Ok {
                                message: Some("the workflow body is valid".into()),
                            },
                            false,
                        )),
                        Err(why) => Err(Response::err(format!("invalid workflow: {why}"))),
                    },
                    Err(e) => Err(Response::err(format!("workflow body does not decode: {e}"))),
                }
            }
            Request::WorkflowSet {
                project,
                expect_heads,
                body_json,
            } => {
                let project = selectors
                    .resolve_project(&project)
                    .ok_or_else(|| Response::not_found("no such project"))?;
                self.submit(&IssueIntent::WorkflowReplace {
                    project_id: project.clone(),
                    expected_heads: expect_heads,
                    body_json,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some("workflow replaced (a new revision is the head)".into()),
                    },
                    true,
                ))
            }
            Request::SpecList { project, page } => {
                let project = project
                    .map(|project| {
                        selectors
                            .resolve_project(&project)
                            .ok_or_else(|| Response::not_found("no such project"))
                    })
                    .transpose()?;
                let query = IssueQuery::Specs {
                    project,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::spec::SpecSummary> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Specs { page }, false))
            }
            Request::SpecShow { spec } => {
                let spec = self
                    .query(&IssueQuery::Spec { spec })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Spec {
                        spec: Box::new(spec),
                    },
                    false,
                ))
            }
            Request::SpecHistory { spec, page } => {
                let query = IssueQuery::SpecHistory {
                    spec,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::spec::Revision> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::SpecRevisions { page }, false))
            }
            Request::SpecReferences { project, page } => {
                let project = project
                    .map(|project| {
                        selectors
                            .resolve_project(&project)
                            .ok_or_else(|| Response::not_found("no such project"))
                    })
                    .transpose()?;
                let query = IssueQuery::SpecReferences {
                    project,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::spec::SpecReferenceFact> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::SpecReferences { page }, false))
            }
            Request::SpecObservations { project, page } => {
                let project = project
                    .map(|project| {
                        selectors
                            .resolve_project(&project)
                            .ok_or_else(|| Response::not_found("no such project"))
                    })
                    .transpose()?;
                let query = IssueQuery::SpecObservations {
                    project,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::records::SpecObservationRecord> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::SpecObservations { page }, false))
            }
            Request::BaselineHistory { baseline, page } => {
                let query = IssueQuery::BaselineHistory {
                    baseline,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::spec::BaselineRevision> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::BaselineRevisions { page }, false))
            }
            Request::SpecNew {
                project,
                kind,
                title,
                text,
                links,
            } => {
                let text = if text.starts_with(contract::DOCUMENT_PREFIX) {
                    text
                } else {
                    crate::document::plain_document(&text)
                };
                if !crate::document::compile_document(&text).valid {
                    return Err(Response::err("document could not be saved"));
                }
                let project = selectors
                    .resolve_project(&project)
                    .ok_or_else(|| Response::not_found("no such project"))?;
                let effect = self
                    .submit(&IssueIntent::ChangeSet {
                        operations: vec![contract::ChangeOperation::SpecCreate {
                            project: contract::ChangeProject::Existing { project },
                            kind,
                            title,
                            text,
                            links,
                        }],
                        ts: facts.now,
                    })
                    .map_err(Self::effect_err)?;
                let spec = effect
                    .results
                    .first()
                    .filter(|result| result.kind == "spec")
                    .map(|result| result.id.clone())
                    .ok_or_else(|| Response::err("change set omitted the created Spec"))?;
                let view = self
                    .query(&IssueQuery::Spec { spec })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Spec {
                        spec: Box::new(view),
                    },
                    true,
                ))
            }
            Request::SpecRevise {
                spec,
                expected,
                title,
                text,
                links,
                plan,
            } => {
                let text = text.map(|text| {
                    if text.starts_with(contract::DOCUMENT_PREFIX) {
                        text
                    } else {
                        crate::document::plain_document(&text)
                    }
                });
                if text
                    .as_deref()
                    .is_some_and(|text| !crate::document::compile_document(text).valid)
                {
                    return Err(Response::err("document could not be saved"));
                }
                self.submit(&IssueIntent::SpecRevise {
                    spec: spec.clone(),
                    expected,
                    title,
                    text,
                    links,
                    plan,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let view = self
                    .query(&IssueQuery::Spec { spec })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Spec {
                        spec: Box::new(view),
                    },
                    true,
                ))
            }
            Request::SpecDocumentUpgrade {
                spec,
                expected,
                text,
            } => {
                if !text.starts_with(contract::DOCUMENT_PREFIX)
                    || !crate::document::compile_document(&text).valid
                {
                    return Err(Response::err("document upgrade could not be completed"));
                }
                self.submit(&IssueIntent::SpecDocumentUpgrade {
                    spec: spec.clone(),
                    expected,
                    text,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let view = self
                    .query(&IssueQuery::Spec { spec })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Spec {
                        spec: Box::new(view),
                    },
                    true,
                ))
            }
            Request::SpecState {
                spec,
                expected,
                state,
            } => {
                self.submit(&IssueIntent::SpecState {
                    spec: spec.clone(),
                    expected,
                    state,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(|failure| {
                    if matches!(
                        state,
                        issues::spec::State::Issued | issues::spec::State::Withdrawn
                    ) {
                        Self::lifecycle_err(failure, "spec.issue", "Spec")
                    } else {
                        Self::effect_err(failure)
                    }
                })?;
                let view = self
                    .query(&IssueQuery::Spec { spec })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Spec {
                        spec: Box::new(view),
                    },
                    true,
                ))
            }
            Request::SpecResolve {
                spec,
                expected_heads,
                body_json,
            } => {
                let mut body: issues::spec::Body = serde_json::from_str(&body_json)
                    .map_err(|_| Response::err("invalid Spec resolution body"))?;
                if !body.text.starts_with(contract::DOCUMENT_PREFIX) {
                    body.text = crate::document::plain_document(&body.text);
                }
                if !crate::document::compile_document(&body.text).valid {
                    return Err(Response::err("document could not be saved"));
                }
                let body_json = serde_json::to_string(&body)
                    .map_err(|_| Response::err("invalid Spec resolution body"))?;
                self.submit(&IssueIntent::SpecResolve {
                    spec: spec.clone(),
                    expected_heads,
                    body_json,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let view = self
                    .query(&IssueQuery::Spec { spec })
                    .map_err(Self::effect_err)?;
                Ok((
                    Response::Spec {
                        spec: Box::new(view),
                    },
                    true,
                ))
            }
            Request::SpecObserve {
                spec,
                rel,
                target,
                note,
            } => {
                self.submit(&IssueIntent::SpecObserve {
                    observation: issues::ids::mint_observation_id(self.clock),
                    spec: spec.clone(),
                    rel,
                    target,
                    note,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let request = issues::contract::PageRequest::default();
                let query = IssueQuery::SpecObservations {
                    project: None,
                    page: request.clone(),
                };
                let page: issues::contract::Page<issues::records::SpecObservationRecord> = self
                    .query_page(&query, &request)
                    .map_err(Self::effect_err)?;
                Ok((Response::SpecObservations { page }, true))
            }
            Request::SpecRetract { spec, observation } => {
                self.submit(&IssueIntent::SpecRetract {
                    spec: spec.clone(),
                    observation,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let request = issues::contract::PageRequest::default();
                let query = IssueQuery::SpecObservations {
                    project: None,
                    page: request.clone(),
                };
                let page: issues::contract::Page<issues::records::SpecObservationRecord> = self
                    .query_page(&query, &request)
                    .map_err(Self::effect_err)?;
                Ok((Response::SpecObservations { page }, true))
            }
            Request::BaselineList { project, page } => {
                let project = project
                    .map(|project| {
                        selectors
                            .resolve_project(&project)
                            .ok_or_else(|| Response::not_found("no such project"))
                    })
                    .transpose()?;
                let query = IssueQuery::Baselines {
                    project,
                    page: page.clone(),
                };
                let page: issues::contract::Page<issues::spec::BaselineSummary> =
                    self.query_page(&query, &page).map_err(Self::effect_err)?;
                Ok((Response::Baselines { page }, false))
            }
            Request::BaselineShow { baseline } => {
                let baseline = self
                    .query(&IssueQuery::Baseline { baseline })
                    .map_err(Self::effect_err)?;
                Ok((Response::Baseline(Box::new(baseline)), false))
            }
            Request::BaselineNew {
                project,
                name,
                members,
            } => {
                let project = selectors
                    .resolve_project(&project)
                    .ok_or_else(|| Response::not_found("no such project"))?;
                self.require_issued_members(&members)?;
                let baseline = issues::ids::mint_baseline_id(self.clock);
                self.submit(&IssueIntent::BaselineCreate {
                    baseline: baseline.clone(),
                    project,
                    name,
                    members,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let view = self
                    .query(&IssueQuery::Baseline { baseline })
                    .map_err(Self::effect_err)?;
                Ok((Response::Baseline(Box::new(view)), true))
            }
            Request::BaselineRevise {
                baseline,
                expected,
                name,
                members,
            } => {
                if let Some(members) = &members {
                    self.require_issued_members(members)?;
                }
                self.submit(&IssueIntent::BaselineRevise {
                    baseline: baseline.clone(),
                    expected,
                    name,
                    members,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let view = self
                    .query(&IssueQuery::Baseline { baseline })
                    .map_err(Self::effect_err)?;
                Ok((Response::Baseline(Box::new(view)), true))
            }
            Request::BaselineState {
                baseline,
                expected,
                state,
            } => {
                self.submit(&IssueIntent::BaselineState {
                    baseline: baseline.clone(),
                    expected,
                    state,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(|failure| {
                    if matches!(
                        state,
                        issues::spec::State::Issued | issues::spec::State::Withdrawn
                    ) {
                        Self::lifecycle_err(failure, "baseline.issue", "Baseline")
                    } else {
                        Self::effect_err(failure)
                    }
                })?;
                let view = self
                    .query(&IssueQuery::Baseline { baseline })
                    .map_err(Self::effect_err)?;
                Ok((Response::Baseline(Box::new(view)), true))
            }
            Request::BaselineResolve {
                baseline,
                expected_heads,
                body_json,
            } => {
                self.submit(&IssueIntent::BaselineResolve {
                    baseline: baseline.clone(),
                    expected_heads,
                    body_json,
                    actor: facts.actor.clone(),
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                let view = self
                    .query(&IssueQuery::Baseline { baseline })
                    .map_err(Self::effect_err)?;
                Ok((Response::Baseline(Box::new(view)), true))
            }
            Request::IssueBaseline { reff, baseline } => {
                let doc = self.resolve(&selectors, &reff)?;
                self.submit(&IssueIntent::IssueBaseline {
                    doc,
                    baseline,
                    device: facts.device.clone(),
                    ts: facts.now,
                })
                .map_err(Self::effect_err)?;
                Ok((
                    Response::Ok {
                        message: Some(format!("updated baseline for {reff}")),
                    },
                    true,
                ))
            }
            Request::Packet { reff } => {
                let doc = self.resolve(&selectors, &reff)?;
                let packet = self
                    .query(&IssueQuery::Packet { doc })
                    .map_err(Self::effect_err)?;
                Ok((Response::Packet(Box::new(packet)), false))
            } // Ownership is fixed by the production classifier; the agreement
              // gate (control_classification) proves every Session-owned request
              // has an arm above, so a foreign request here is a caller bug,
              // never a servable state.
        }
    }

    fn ref_response(&self, doc: &str) -> Response {
        Response::Ref {
            reff: self.reff_for(&self.selectors(), doc),
        }
    }

    fn work(
        &self,
        selectors: &Selectors,
        reff: String,
        action: WorkAction,
        facts: &RouterFacts,
    ) -> Result<(Response, bool), Response> {
        let doc = self.resolve(selectors, &reff)?;
        let effect = self
            .submit(&IssueIntent::WorkState {
                doc: doc.clone(),
                action,
                actor: facts.actor.clone(),
                device: facts.device.clone(),
                ts: facts.now,
            })
            .map_err(Self::effect_err)?;
        let view: IssueView = self
            .query(&IssueQuery::View {
                doc,
                me: Some(facts.actor.clone()),
            })
            .map_err(Self::effect_err)?;
        Ok((Response::Issue(Box::new(view)), !effect.unchanged))
    }

    fn link(
        &self,
        reff: String,
        kind: String,
        target: String,
        add: bool,
        facts: &RouterFacts,
    ) -> Result<(Response, bool), Response> {
        self.submit_change(
            contract::ChangeOperation::IssueLink {
                issue: reff.clone(),
                kind,
                target,
                on: add,
            },
            facts.now,
        )
        .map_err(Self::effect_err)?;
        Ok((Response::Ref { reff }, true))
    }
}

fn apply_document_splices(
    source: &str,
    splices: &[crate::protocol::DocumentSplice],
) -> Option<String> {
    let mut characters = source.chars().collect::<Vec<_>>();
    for splice in splices {
        let start = usize::try_from(splice.index).ok()?;
        let delete = usize::try_from(splice.delete).ok()?;
        let end = start
            .checked_add(delete)
            .filter(|end| *end <= characters.len())?;
        characters.splice(start..end, splice.insert.chars());
    }
    Some(characters.into_iter().collect())
}

/// A shared clock for the router in production.
pub fn system_clock() -> SystemUlidSource {
    SystemUlidSource
}

fn bad_due() -> Response {
    Response::err("due must be unix seconds, YYYY-MM-DD, or `none`")
}

/// Parse a due-date argument: raw unix seconds, or `YYYY-MM-DD` as UTC
/// midnight. Timezone policy is deliberately the simplest honest one — a due
/// *date* names a day, and UTC midnight is the one reading every replica
/// derives identically; clients localize for display.
pub(crate) fn parse_due(text: &str) -> Option<u64> {
    let text = text.trim();
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return text.parse().ok();
    }
    let mut parts = text.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days-from-civil: civil date -> days since 1970-01-01.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let mp = ((m + 9) % 12) as u64;
    let doy = (153 * mp + 2) / 5 + (d as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    u64::try_from(days).ok().map(|d| d * 86_400)
}

#[cfg(test)]
mod tests {
    use super::parse_due;

    /// Every denial cause renders its own remedy, and non-standing failures
    /// never wear standing words — the collapsed "you lack write standing"
    /// message once sent a member whose grant hadn't synced to ask an admin
    /// for a grant they already held, and rendered ledger failures the same
    /// way (the class of dishonesty that cost a full debugging day).
    #[test]
    fn each_denial_cause_names_its_own_remedy() {
        use super::{DeniedCause, IssueRouter, Rejection, SessionFailure};
        let message = |failure: SessionFailure| -> String {
            match IssueRouter::effect_err(failure) {
                crate::protocol::IssuesResponse::Error { message, .. } => message,
                other => panic!("expected an error response, got {other:?}"),
            }
        };

        let not_member = message(SessionFailure::Rejected(Rejection::Denied(
            DeniedCause::NotAMember,
        )));
        assert!(not_member.contains("may not have synced"), "{not_member}");

        let unsatisfied = message(SessionFailure::Rejected(Rejection::Denied(
            DeniedCause::DemandUnsatisfied,
        )));
        assert!(unsatisfied.contains("scoped member"), "{unsatisfied}");

        let issuing = match IssueRouter::issuing_denied("Spec", "spec.issue") {
            crate::protocol::IssuesResponse::Error { message, .. } => message,
            other => panic!("expected an error response, got {other:?}"),
        };
        assert!(
            issuing.contains("spec.issue") && issuing.contains("space.admin"),
            "{issuing}"
        );

        let read = message(SessionFailure::Rejected(Rejection::Denied(
            DeniedCause::ReadRefused,
        )));
        assert!(
            !read.contains("write standing") && read.contains("read"),
            "a read refusal must not wear write words: {read}"
        );

        let ledger = message(SessionFailure::AuthorityUnavailable(
            "MissingHistory".into(),
        ));
        assert!(
            ledger.contains("not a permissions problem") && ledger.contains("MissingHistory"),
            "{ledger}"
        );
    }

    /// The typed causes keep the one stable public `error_kind`: heads key
    /// recovery UX off `denied`, whatever the message says.
    #[test]
    fn every_denial_cause_keeps_the_denied_error_kind() {
        use super::{DeniedCause, IssueRouter, Rejection, SessionFailure};
        for cause in [
            DeniedCause::NotAMember,
            DeniedCause::PrincipalMismatch,
            DeniedCause::DemandUnsatisfied,
            DeniedCause::ReadRefused,
        ] {
            match IssueRouter::effect_err(SessionFailure::Rejected(Rejection::Denied(cause))) {
                crate::protocol::IssuesResponse::Error { error_kind, .. } => {
                    assert_eq!(error_kind, crate::protocol::IssuesErrorKind::Denied);
                }
                other => panic!("expected an error response, got {other:?}"),
            }
        }
    }

    #[test]
    fn due_dates_parse_as_utc_midnight_and_unix_passthrough() {
        assert_eq!(parse_due("1970-01-01"), Some(0));
        // A known epoch day: 2026-07-22 = 20 656 days after the epoch.
        assert_eq!(parse_due("2026-07-22"), Some(20_656 * 86_400));
        assert_eq!(parse_due("1753142400"), Some(1_753_142_400));
        assert_eq!(parse_due("2026-13-01"), None, "month out of range");
        assert_eq!(parse_due("07-22"), None, "not a date");
        assert_eq!(parse_due(""), None);
    }
}
