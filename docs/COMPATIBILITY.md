# Compatibility matrix

Every versioned surface in LAIT, what gates it, and what a bump costs. The point
of collecting them in one table is that they are *not* one version: a store can
be rewritten while the wire holds still, and a wire generation can move without
touching a byte on disk.

There is no legacy fallback in any normal reader in this table. An unsupported
version is refused, not guessed or opened in place. A bounded prior reader may
exist only as input to an explicit generation build: it validates a committed
source, constructs the current representation elsewhere, proves logical
equivalence, and atomically activates the complete result.

## 1. How a version is enforced

Three mechanisms, and which one applies decides what a bump breaks.

**A leading version field.** Read first, checked before anything else in the
record is trusted. Applies to stores, manifests, descriptors, and canonical
records. A bump means an older reader refuses the file, and refusing is the
whole point.

**A hash domain.** The version is baked into a domain string
(`lait/manifest/2`), so a signature or content address computed under one
generation simply does not verify under another. There is no version check to
write — a mismatch is a verification failure. A bump therefore invalidates every
signature and every id derived under the old domain, which is why domains carry
the heaviest version numbers and move the least.

**An ALPN.** Negotiated during the QUIC handshake, before either peer speaks. A
bump means peers on different generations share no ALPN and never connect. There
is no half-speaking pair and no in-band fallback, so this is the most expensive
kind of bump and the one that needs feature bits to avoid.

### MCP protocol

The stdio server is `rmcp` 3.1 and advertises MCP `2026-07-28`. It still
answers the legacy `initialize` handshake for `2024-11-05`, `2025-06-18`, and
`2025-11-25`. `server/discover` is implemented. Successful tool results carry
`structuredContent` (the same versioned DTO as the HTTP head) plus a text
mirror. Product and argument failures are tool-execution errors
(`isError: true`) so the model sees the diagnostic; JSON-RPC errors stay
reserved for transport and unknown methods. `tools/list` for a 2026-07-28 peer
includes `ttlMs` and `cacheScope: private`.

This server does not implement Streamable HTTP, OAuth, resources, prompts,
sampling, roots, logging, or the Tasks extension. Geometry is compiled
Blueprint output for the viewer and is not an MCP tool.

`$LAIT_WORLD` pins the session to one World mount. Unset, a build that
hosts a single World (today: `issues`) takes that pin. Unset with more
than one hosted World is a refusal that names the mounts. An unknown
mount is a refusal, not an empty tool list. Existing bindings that omit
the variable stay valid while Issues is the sole package. A binding
that names a World writes `LAIT_WORLD` next to `LAIT_AGENT`; it still
does not pin a path or `LAIT_HOME`.

`WhoamiDto.sponsorship_asked`, `sponsorship_granted`, `wait_heads`, and
`HostReply::Context.asks` are additive optional fields (`serde default`,
empty asks omitted). An unsponsored named agent's `whoami` files a
host-plane ask; Astrolabe samples `HostContext` and notifies. The agent
Watches that wait (`Request::SponsorWatch` / MCP `wait`) with the same
head comparison Exec Watch uses. Approval (`AgentProvision`) moves the
heads; `granted` consumes the wake. This is not a World Signal and does
not drain `Request::Signals`. There is no Work `Start` for the wait —
opening it is `Whoami` as a named agent.

## 2. Durable formats

| Surface | Constant | Value | Gate |
|---|---|---|---|
| Store marker | `replica::marker::STORE_VERSION` | 1 | leading field |
| Orbit generation pointer | `runtime::generation` | 1 | magic + canonical body + checksum |
| Store manifest | `journal::STORE_FORMAT_VERSION` | 4 | leading field |
| Replica store meta | `replica::STORE_META_FORMAT_VERSION` | 6 (one-time migration from 2) | leading field |
| Manifest root | `replica::manifest::MANIFEST_FORMAT_VERSION` | 2 | leading field + `lait/manifest/2` |
| Content descriptor | `replica::content::CONTENT_FORMAT_VERSION` | 1 | leading field + `lait/content-id/1` |
| Causal artifacts | `fabric::causal::CAUSAL_FORMAT_VERSION` | 1 | leading field |
| Neighbour registry | `runtime::neighbors::REGISTRY_VERSION` | 2 | leading field |
| Custody package | `mechanics::custody::PACKAGE_VERSION` | 1 | leading field |
| Policy compiler | `mechanics::compile::COMPILER_VERSION` | 1 | leading field |
| Ledger semantics | `mechanics::ledger::LEDGER_SEMANTICS_VERSION` | 1 | leading field |
| World implementation descriptor | `runtime::implementation::DESCRIPTOR_VERSION_SECTIONED` | 2 | leading field |

### Find contract

`find::Grant` standalone bytes are implemented at version 1. They carry a
leading version field, have a 65,536-byte ceiling, and commit under the
`lait/find/grant/1\0` BLAKE3 domain. Schema, Field, Edge, Gate, and Feature
reference field order; the six operator bits; the two mode bits; and the ten
`Bound` fields are fixture-frozen. Canonical sets are sorted and duplicate-free;
unknown bits, absent or sentinel-unbounded ceilings, undeclared Schema
references, trailing bytes, and widening composition reject.

`find::Query` standalone bytes are implemented at version 4. They have a
262,144-byte ceiling and commit under the existing
`lait/find/query/1\0` BLAKE3 domain. The version is in the committed bytes. The
Schema and optional full `PublicationId`, mode, canonical topological Step
order, typed inputs, six operator tags, output, per-Step and whole-Query Bounds,
requested page size, and optional cursor are fixture-frozen. `Seek::Bodies` is
the live-refresh producer and is bounded at 4,096 exact Body keys. Before corpus access,
validation rejects
cycles and forward inputs, non-contiguous or duplicate Step ids, missing and
ill-typed inputs, unreachable Steps, unstable input or final output order,
undeclared references, non-finite or widening Bounds, exact-mode feature use,
unknown tags, trailing bytes, and non-canonical encodings. Query-to-Grant validation
additionally proves Schema, mode, operator, reference, and budget containment.
Every ranked producer has a total order whose final ascending tie-break is the
canonical Node identity; unordered Nodes or Paths cannot be the final output.

The optional implementation-descriptor Find section is implemented at section
tag `0x0003`. It carries sorted, canonical version-1 `find::Schema` entries.
That entry version is frozen as part of tag `0x0003`; changing the entry grammar
requires a new descriptor section tag rather than teaching this tag a second
meaning.
Each entry commits its Body sources, Field scalar semantics, Edges and Gates,
canonical Gate demands, analyzers and configuration, optional feature stamps,
operator/mode sets, and finite Bound. Changing any of those bytes moves only a
World that declares Find; declaring nothing omits the section and leaves its
pre-Find implementation bytes and id unchanged. Unknown section tags, duplicate
or cross-wired references, undeclared Body sources, and missing, extra, or
duplicate extractor-coordinate bindings reject during package composition.

`Session::find(Query)` is the single implemented evaluator seam. Runtime pins
the live Station epoch, Space, World, exact active implementation, extractor
schema digest, materialization, fresh principal and authority frontier, and
Station `find::Policy` through the same Session used by submission. A query may
omit `publication` to select the current image or name a retained full
`PublicationId`; a root alone has no encoding. Cursors additionally bind the
station-local materialization and expire rather than falling forward.

Package composition registers an executable extractor for every declared
source coordinate and rejects missing, extra, or duplicate bindings. One
immutable principal-neutral corpus is incrementally maintained per published
World image. The evaluator supports source, Body, id, field, term, feature,
predicate, walk, rank, merge, and pack flows over persistent forward and reverse
indexes. Ordered direct postings support bounded pages and opaque continuations;
their answers may include an exact gate-admitted `matched_total`. Operators
without an honest resumable state refuse a request that would require partial
results instead of presenting a truncated page as complete. Analyzed token
terms are indexed; analyzed phrase and prefix terms refuse until positional or
prefix structures with bounded amplification exist. Request-specific gates
partition postings before traversal, ranking, lookahead, counts, packing, and
metering, so denied population cannot influence either answers or resource
refusal. Body decoding is charged at corpus materialization rather than again
per query.

Control v14 and the root MCP `find` tool carry Runtime's exact Query and Answer
types through the authenticated Session. Product adapters and viewers use the
same evaluator; a private projection cache is not an alternative query source.
A change to standalone Grant or Query meaning requires a new leading version;
the digest domain changes only if the commitment meaning itself changes.

Issues exposes `issues_search` as a product presenter over that same Find
request. Its envelope contains the complete publication coordinate, product
field maps, an optional opaque base64url continuation, and an exact total only
when the selected direct posting can provide one without hidden-row influence.
Attaching the presenter changes response shaping only; the root `find` request
and its canonical answer remain product-neutral.

### Exec contracts and reservations

`exec::Spec` now has a generation-1 canonical standalone encoding with strict
validation for access demands, payloads, resume/effect/acceptance rules, bounded
Find Grants, Services, Links, and Run limits. `exec::Build` also has a
generation-1 canonical standalone publication envelope. The optional Exec
descriptor section is implemented at tag `0x0004`: it carries sorted canonical
Specs and composition refuses any embedded Find Grant that widens the active
World Find declaration. Its Spec grammar is frozen by the tag; adding fields or
changing meaning requires a new tag. An empty declaration is omitted, so Worlds
that do not adopt Exec retain their existing descriptor bytes and implementation
ids. E1 reserves the generation-1 Runtime Body schemas and implements the first
atomic lowering: `Start` becomes a predecessor-free `Started` event plus the
chunked canonical command in a protected Run Body.

| Surface | Current status | Compatibility rule |
|---|---|---|
| Spec and Link descriptor section | implemented at tag `0x0004` | optional sorted canonical Specs; an empty declaration is omitted and preserves the existing World implementation id; embedded Find Grants must be contained by active Find declarations; unknown or replacement section tags reject |
| Build publication envelope | standalone Build grammar implemented | `BuildId` commits to generation-1 executable material; publisher and device signature attest that identity but do not change it; unknown generations and algorithms reject |
| Offer news envelope | standalone Offer grammar implemented | `OfferId` commits to generation-1 news (Station, actor/device, World/implementation, exact Builds and Specs, resources, backend, enforcement, resident ContentRef hints, availability, epoch, expiry); publisher and device signature attest that identity but do not change it; an Offer reserves no Run and reveals no local path or catalog; `Offer::validate` does not consult a clock; `Offer::usable_at` applies expiry; a `Try` may omit an Offer or cite one as evidence; unknown generations and algorithms reject; this is not a reserved Body and not a ranking surface |
| Offer news table | Station-local, lossy | `Session::announce` retains signed Offers for this activation only, bounded by `MAX_OFFERS_PER_STATION`; they die with the Station and are not reserved Bodies. Announce evaluates each claimed Spec's offer demand. A first-use `Try` that cites an Offer must name live usable news for this Space/World/implementation; continue/resume may cite a historical OfferId from a prior Attempt — live news still authorizes the offer demand but does not require a new Ready; expired news still pays the offer demand. Station-only Tries omit Offer and remain legal while unused news is held |
| Readiness challenge | Station-local, nonce-bound | `Session::challenge` issues an expiring nonce against live Offer news; `Session::ready` accepts one signed answer. Readiness is not intent and does not reserve a Run. A first-use `Try` that cites an Offer also requires a live Ready for that Offer signed by this activation's device; two first-use Tries cannot share one Ready. The Ready is consumed only after that first-use `Leased` is durable. Answering and stalling cannot block a Station-only Try or imprison a Run after the leasing activation dies |
| Build publication | reserved Body, identity only | `Session::publish_build` writes the signed Build envelope into `lait.exec.build` at a Body id derived from `BuildId`. Republishing the exact envelope is idempotent; a different envelope under the same id is refused. This is not current/ramping choice and does not move an open Run's pinned Build |
| Application Exec package | composition and host retention implemented | the application-owned `WorldPackage` carries `exec::Package`; its Specs must exactly equal the reviewed descriptor, every Build must name that World, implementation, and Spec with compatible resume material, and every local handler must bind the exact Build artifact plus only declared Roles and Links; ambiguous or cross-wired packages reject before a Station host is created |
| Handler and Context seam | bounded in-process contract and completion staging implemented | a handler receives authenticated Run/Attempt coordinates, pinned input references, accepted resources, optional enforcement evidence (absence is advisory), Attempt limits, declared Links, and a cancellation watch through `exec::Context`; checkpoint references and child Starts can be staged only within the Attempt limits, child query grants remain unavailable until the Find delegation contract exists, and a validated `Completion` exposes typed material from which Runtime alone constructs canonical `Saved`/`Returned` events; unavailable capability facets grant no ambient Session, Replica, World, transport, or query access |
| Exact local selection | implemented over projected Run and Attempt coordinates | `exec::Package::select` requires the Attempt Build to equal the Build bound by `Started`, then resolves the exact Spec coordinate, Build identity, artifact, and optional Role handler; multiple Builds for one Spec may coexist, package order has no meaning, and missing historical material fails instead of falling forward to a newer Build |
| Trusted in-process backend | implemented with advisory enforcement | `exec::InProcess` reports `Enforcement::Advisory`, refuses an already-cancelled Context before entry, contains handler panics, and validates the candidate Outcome against the exact selected Spec; resource vectors remain scheduling/accounting evidence and this backend claims no process, container, or kernel isolation |
| Committed local dispatcher | implemented over immutable Replica generations | both discovery and invocation re-project the Run from the supplied committed snapshot; invocation requires the exact nonterminal Attempt, its committed `Began` event directly following the selected lease, and exact package selection before constructing Context or entering a backend; callers provide only Run and Attempt ids, so a pre-commit in-memory projection, a root-only Run, or a terminal Attempt cannot cross the execution seam |
| `exec::Cmd` and World command channel | semantic tags 0–7 implemented; World lowering supports Start, Try, Cancel, Accept, and Reject | canonical bytes have golden fixtures and every World effect producer must explicitly stage a command vector; Start pins a Spec and Build while Runtime derives ambient Run coordinates; Try pins a Run, Build, limits, and fence while Runtime derives a fresh Attempt id and writes Station/epoch from the ambient activation; Offer and enforcement are optional — a Station-only first Attempt omits both rather than minting a derived OfferId; an Offer, when present, must name this activation; Cancel records only `CancelAsked`; Accept/Reject validate one exact returned Attempt from the callback's pinned snapshot and append the product choice in the same transaction as ordinary World operations; Retry, Resume, and Drain reject until their missing scheduling/service coordinates exist rather than being ignored or guessed |
| Application Work capability | typed package and root composition implemented for inspect, watch, cancel, continue, and checkpoint resume | `ClientHost::call_work` carries Runtime-owned `WorkRequest` plus a serialized `WorkReply` or typed host refusal, while each application owns the vocabulary and controls that compose it; Issues exposes inspect, watch, cancel, continue, and resume, has no raw Start, and depends on neither the daemon protocol nor Astrolabe; inspect/watch are reads and every mutation enters the same `lower_exec` validator and protected commit path as `World::submit`; continue derives a newly fenced Attempt only from a completed Attempt's committed coordinates in the same Station activation, while resume also requires an exact committed checkpoint and `Resume::Checkpoint`; the current Issues verification Spec is `Resume::Restart`, so it refuses resume with direction to use continue; Started-only, cross-activation, and service-leased work remain typed scheduling refusals rather than guessed transitions; lifecycle projections contain ids, failure class, and returned output ContentRefs, but no product input/output payloads |
| Local perform outbox | implemented for one Station activation | `Session::perform` is an exclusive Station drain, not an RPC pump: the host loop waits on `exec_tick` plus a short interval and one activation runs the outbox at a time. It observes unresolved Runs, cites live Offer news and a Ready when both exist, otherwise leases Station-only, commits `Try`/`Began` before invoking, binds ingested output ContentRefs, then commits `Returned` or `Failed`. A `Began` this process did not claim — including one inherited from a prior epoch on this Station — and a prior-epoch `Leased` that never began are committed `Failed` with `FailureClass::Unknown` and are never re-invoked under that Attempt id. After that unknown failure, `Resume::Restart` and remaining Attempt budget, the same activation may `Try` a new Attempt; a later Return or handler/protocol failure is not another automatic retry |
| Issues verification adopter | semantic Start, local in-process handler, and acceptance transaction implemented; Build publication is identity only | `issue.verify/v1` is declared by IssuesWorld and exposed as `issues_verify`; the application derives the exact Runtime Run from its request coordinate, the World rechecks that binding, and the issue check plus protected `Started` event commit atomically; source is a committed ContentRef; the bundled verifier Build is signed and carried in the application package; callers may omit `build` to select that exact id, and the check then records `package_filled` so a surface cannot present a package default as a caller-named Build; the bundled handler binds the pinned source in-process — it does not compile the repository or isolate the host; the Station drain performs the Attempt locally after Start commits; `issues_accept_check` admits only an exactly-once matching Runtime Outcome and atomically attaches its report, records pass/fail, optionally enters an existing Done state, and stages `Accepted`; no generic `execute-plan` or Work `Start` surface is introduced |
| World Outcome and acceptance seam | namespace-bound fact facade and atomic product choice implemented | a hosted World callback may call `Context::outcome(RunId, AttemptId)` for Runtime-decoded facts from the same pinned snapshot as its ordinary reads; the World id is ambient rather than caller-selectable and raw protected Bodies or output bytes are never exposed; exactly one returned Outcome, matching Spec/Build and a valid lease-to-return causal chain, is required before Accept/Reject lowering; Returned remains distinct from Accepted and two terminal choices cannot be staged from one stale snapshot |
| Run, Build, and Service Bodies | schema ids, ownership boundary, and Start-to-Run lowering implemented | `lait.exec.run`, `lait.exec.build`, and `lait.exec.service`, each schema version 1 with encoding `lait.exec.body.v1`; package composition rejects a World declaration at any version, Runtime installs the schemas under every hosted World, and raw World snapshot reads hide them; a Run Body stores its `Started` event at `list:events` and its complete canonical command in ordered 64-KiB `map:command` chunks; the standalone Build envelope does not reserve its later Body schema |
| Run and Attempt events | generation-1 lifecycle event DAG implemented | wire tags 0–9 are `Started`, `Leased`, `Began`, `Saved`, `Returned`, `Failed`, `CancelAsked`, `Cancelled`, `Accepted`, and `Rejected`; `Started` is the one predecessor-free root and binds the derived Run id, exact Spec and Build, active World implementation, invoker/device/frontier, parent Manifest, input and query commitments, request/ordinal, resources and limits, and complete command digest/geometry; Attempt admission and returned Outcomes bind their exact coordinates and evidence; event ids use `lait.exec.run-event.v1`; predecessor lists are bounded, sorted, and duplicate-free; concurrent heads stay visible until an event explicitly joins them; adding or changing event meaning requires an explicit schema or envelope generation |
| Run, Attempt, and Outcome projections | implemented over protected events | `exec::Run::project` ignores collaborative-list order and rebuilds only Runtime-owned facts; it exposes separate sorted Attempts, Outcomes, cancellation facts, acceptance/rejection facts, and causal heads rather than a scalar LWW status; it refuses malformed DAGs, duplicate Attempt ids, unbound facts, repeated returns, coordinate contradictions, and Run/Attempt limit widening; output bytes remain opaque and only schema, digest, geometry, and ContentRefs enter the generic projection |
| Local unresolved-Run scan | implemented over an immutable committed Replica generation | `exec::scan_unresolved` selects only exact generation-1 protected Run bindings for one World, validates the complete event DAG and Body-derived Run identity, reconstructs every exact ordered command chunk, verifies the canonical `Start` digest and duplicated coordinates, and returns Run projections sorted by `RunId`; returned/failed Attempts and Attempt-scoped cancellation remain unresolved, while a Run-level cancellation or any acceptance fact removes the Run from dispatch consideration; the scan is read-only and remote incorporation never invokes it or a handler |

`RunId` is the first 128 bits of a domain-separated BLAKE3 commitment over the
canonical Space, World, device, 128-bit request id, and command ordinal. It is
therefore stable for an identical persistent-idempotency scope without using a
caller-supplied request id directly as a Body identity. Runtime validates the
ordinary World demand and every selected Spec's Start demand independently,
canonicalizes their deduplicated conjunction, and binds that one `All` demand
to the one transaction receipt. The World operations, protected Run operations,
bindings, content reachability, observation Bodies, and receipt commit in that
same Replica transaction; refusal leaves none of them visible.

Compatible Builds may change executable material without moving the World
implementation id only when their declared Spec meaning is unchanged. Changing
payload meaning, demands, limits, effects, acceptance, resume behavior, Find
Grants, or Links changes the descriptor identity. Build and content hashes are
always verified; neither is runtime attestation. A Build signature proves only
that its carried Device signed the canonical publication envelope. Mechanics
separately decides whether that Device represented the publisher and whether
publication satisfied the Spec's Build demand at the pinned authority frontier.

The marker and the store manifest version different things and move
independently: the marker identifies the *store layout* — what files exist and
where — while the manifest version identifies *what a commit records*. Replacing
the paged manifest with an authenticated index changed the second and not the
first.

The generation pointer versions neither semantic facts nor either component's
store format. It selects one immutable pair of Mechanics and Replica
materializations. Its source generation and equivalence evidence are part of
the canonical pointer body, and activation is serialized and compare-and-swap
checked. This is the compatibility boundary for a representation rewrite: old
bytes remain inactive rather than being destructively rewritten or taught to
every future reader. `HostOrbitRebuild` (`{"cmd":"host_orbit_rebuild"}` on the
host plane) is the application composition of the currently supported
prior-to-current recipe. The daemon releases its own placement for that Orbit
first — the rebuild requires the Orbit to be vacant, and running it from a
separate client was a store-lock race against whatever the daemon had open.

Journal format 4 authenticates eager control objects and deferred causal
payloads under separate roots, and Replica store meta 6 replaces rendered
object maps with authenticated Body, generation, receipt, and ownership roots.
A bounded prior reader recognizes the immediately preceding Journal/Replica
representation only as a read-only migration source. It never rewrites an old
whole-Body signed descriptor into a causal descriptor: those declarations have
different signed meanings. After launcher update consent, the composition-owned
migration job streams the prior committed facts into a fresh generation through
authorized current transactions, verifies semantic Body and receipt evidence,
and only then activates it. Unknown or older formats fail closed, and the normal
current reader carries no predecessor branch.

The descriptor is the only row whose version is chosen by the record's content
rather than by the build that wrote it. A descriptor emits 1 when it declares no
sections and 2 when it declares any, so the set of implementation ids this bump
moves is exactly the set of Worlds that declare a section. That is the whole
reason the section table exists: adding a section kind must not move the id of a
World that declares nothing of that kind, which two more fields in a fixed-order
tuple would have done to every id in the system.

**`com.lait.issues` is in that set, and its id moves with the v4 cutoff.** V4 is
a pre-v1 semantic migration, not a permanent predecessor-reading branch. It
retains each existing Issue Body key and collaborative operation history so
text anchors keep their meaning, then adds the stable identity and atomic board
placement roots in place. Every independently edited enrichment or project
entity moves to a deterministic record Body. Spec and Baseline revision DAGs
retain their semantic hashes and exact references while their immutable
revisions move to revision-sized Bodies. Plans remain Spec revisions and store
the full portable publication identity.

The migration is one launcher-authorized, crash-resumable protocol executed in
bounded deterministic transactions under an in-process capability bound to the
exact source, migrator, and target implementations. It is not a public tracker
intent and does not accept a caller-supplied actor. Its marker cannot claim completion
until every aggregate Catalog, enrichment, schedule, hierarchy, update, triage,
workflow, role, Spec, Baseline, and membership record has been materialized and
audited. Only then may the v4-only implementation activate. Internal Spaces use
the same one-time migration; no v3 compatibility promise survives the v4
activation. Historical semantic coordinates either resolve the exact retained
implementation/extractor package or fail typed — they are never reinterpreted
by the current package.

Durable activity is not truncated from product truth. An event, comment,
reaction, decision, or relationship is a bounded record Body and remains
recoverable after checkpoints. Paging and retention policies may bound a read
or a local analytical artifact, but they do not silently erase older committed
records.

Worlds that declare nothing keep the ids they had, which the same test asserts by
construction — a zero-section descriptor is byte-identical to what shipped before
sections existed.

The hash domain deliberately did not move with it. Ids stay derived under
`lait.world-implementation.v1` even at encoding version 2, because the domain is
what every shipped activation record was derived under and moving it invalidates
all of them at once — the cost §1 records against the "hash domain" mechanism.
An encoding version and a domain generation are separate numbers here precisely
so that the cheap change can be made without paying for the expensive one.

Within the table, an unknown section tag is refused rather than skipped. A
skipped section would make the implementation id a digest over bytes the build
did not interpret, which is the one thing a reviewed trust identity may not be.
Forward tolerance is not what the tag buys; not moving unrelated ids is.

Every count in the record is a `u16`, so 65535 schemas, and 65535 entries per
section, is a limit of the format rather than a policy anyone tunes
(`runtime::implementation::MAX_ENCODABLE_ENTRIES`). A longer list has no
encoding, and `encode` refuses it instead of writing a count that describes
fewer entries than follow it — which would derive an id over bytes `decode`
rejects, an encoding that does not decode.

A Manifest root's acceptance rule tightened without its version moving, which
is worth stating plainly because the two usually move together. A root whose
Body entries declare `ContentRef`s is now refused unless every one of them
resolves — from the root's own content index, or from a descriptor the receiver
already holds. The encoding did not change: `content_index_root` and
`content_count` have been in the record since `lait/manifest/2`, and a root that
declares no content is accepted exactly as before. What changed is that
advertising a declaration you cannot back is no longer a valid advertisement.

No peer in the field is affected. Nothing produces a content declaration yet —
the Issues attachment cutover is what will — so the set of roots this newly
refuses is empty today, and that is precisely why the rule lands now rather than
after there is state to break.

Index nodes carry no version of their own. They are content-addressed, immutable
journal objects reachable only from a root recorded in a versioned manifest, so
the manifest that names a node is what decides how it is read. Giving a node its
own version would let a node and its root disagree.

The content cache is the other unversioned thing on disk, and it is unversioned
for a different reason. `content-cache/` appeared beside the journal when the
content plane opened at activation, and it moved neither the marker nor the
manifest version — correctly, because the marker identifies the layout the
*record* is read from and the cache holds nothing the record names
(`DATA-CONTRACT.md` §13 says what does live there). Every entry is filed under
its own content address and re-checked on read, so an entry this build cannot
make sense of is dropped and refetched rather than interpreted. A format that can
always be discarded needs no version to refuse an old one, and giving it one
would imply a store could be made unreadable by its cache.

### The attachment migration window

Two record shapes coexist in the `attachments` map of an issue Body, and the
window has no end date because it needs none.

| Shape | Written by | Read by |
|---|---|---|
| `{…, data_b64}` | builds before the content cutover | every build, permanently |
| `{…, content}` | this build onward | this build onward |

`AttachmentMeta` carries no `deny_unknown_fields` and defaults every field but
`id` and `name`, which is why both shapes decode through one type and why the
new field could be added without a version moving. The *write* path is a clean
break — nothing emits `data_b64` any more, and the encoder that produced it is
deleted in both the engine and the viewer.

The read path is not a deprecation. It is the permanent cost of having once
written files into Bodies, and it is cheap: the old shape was bounded at 256 KiB
by 8 attachments, so the worst legacy Body is 2 MiB and needs no streaming
reader. `MAX_LEGACY_ATTACHMENT_BYTES` records that bound as what it now is — the
shape of what was already written, not a policy anyone tunes.

## 3. Signed and addressed domains

A domain is versioned prose baked into a hash. These are the ones that carry a
generation above 1, because those are the ones that have already moved:

| Domain | Covers |
|---|---|
| `lait/manifest/2` | the signed Manifest root, plus `…/body-key` and `…/content-key` index keys |
| `lait/body-transaction/3` | the signed Body transaction envelope and protected Fabric Material closure |
| `lait/coordinates/2` | Space coordinates |
| `lait/space/1/ceremony/2` | ceremony material and authority grants |

Everything else is at `/1`. A domain is never repurposed in place: changing what
a preimage means requires a new domain string, not a new interpretation of an
old one.

## 4. Wire generations

| ALPN | Plane | Status |
|---|---|---|
| `lait/contact/2` | Contact — authority, manifest nodes, Body payloads | implemented |
| `lait/neighbor-presence/1` | liveness probe | implemented |
| `lait/freight/1` | Freight — reliable exact-object request and response | implemented and **mounted** |
| `lait/session/1` | Live — transient collaboration and reliable signals | implemented and **mounted**, inbound and outbound; carries the control lane and the reliable-signal lane |
| `lait/exec/1` | Exec — direct Station work and bounded lifecycle flows | reserved; not registered, advertised, or mounted |

**What "implemented" means in this column.** It means a dial on that ALPN
reaches a handler that reads the opening, judges it, and answers — not that
every message the generation defines has a sender. `lait/freight/1` routes now:
an admitted peer's availability question and ranged chunk request are served
from committed descriptors and validated proof sidecars, behind one coarse
refusal. The fetching half is the same generation seen from the other end and
needs no wire change to arrive, which is why the status is recorded against the
generation rather than against a feature — the frames, bounds, and refusal
vocabulary a `1` peer must implement are already fixed by what serves them.

Advertising an ALPN is not the same as serving it, and until this branch both of
these were advertised and unserved: the endpoint registered them, a peer that
dialled completed a handshake, and the hub turned the opening away because no
driver owned the plane. `Orbit::activate` mounts both now.

This column tracks the service and not the registration. A successful handshake
was never evidence a plane was live, and for two releases it was evidence of the
opposite.

Each plane owns its own inbound queue. One queue for both would hand two drivers
strictly alternating connections, each refusing half of what it was given as a
foreign ALPN — so the split is what makes a second plane possible at all, rather
than a tidiness.

One detail reads like a contradiction and is not. The opening still carries
`protocol_version` and the refusal vocabulary still has `UnsupportedVersion`,
even though the ALPN already stops a mismatched pair from connecting at all. It
is belt and braces, and it costs a comparison. It is also the *only* refusal
that names its reason — every other one is deliberately coarse — so a peer
reading a bare refusal must not read it as a version problem.

Gossip rides iroh's own ALPN inside `crates/comms` and is transport plumbing, not
a LAIT protocol generation.

`lait/exec/1` is a reservation, not a compatibility promise that code can dial.
It becomes implemented only when a real opening is bounded and decoded, an
admitted peer is independently authorized, and a mounted driver answers it.
Advertising it earlier would repeat the false-positive service state described
above. Additive optional behavior uses negotiated feature bits; a peer that
would misinterpret changed command, flow, or refusal semantics requires a new
ALPN generation.

`PROTOCOL.md` — "Delivery planes" — has the full contract, including the frozen
bounds and which of them are LAIT policy rather than observations of the pinned
transport.

**Feature bits, not generations.** A capability that an older peer can safely
ignore is advertised as a bit in the opening, where an absent field decodes to
zero exactly as an older build would have sent it. The ALPN moves only for a
change an old peer would *misinterpret* — a removed or repurposed field, or
changed semantics for an existing one. Adding a lane, a hint, or an optional
answer is a bit.

**How a bit is negotiated, now that bits are negotiated.** Admission intersects
what the peer offered with what this build implements, and that intersection —
not the peer's offer — is what the accept carries. Three consequences, none of
them an error path:

- an absent or zero `features` is a peer that offers nothing, not a peer that is
  malformed. Zero is exactly what an older build sends, and being able to decode
  to it is what makes the field additive at all;
- a bit this build does not know is dropped from the intersection and never
  echoed, so a peer cannot discover what a later generation calls something by
  watching what we reflect back;
- a capability the peer did not offer is never used against it. A bit is
  permission to speak, not a hint, and the intersection is computed once at
  admission so no later decision re-guesses it.

Requested lanes are intersected the same way and for the same reason: a lane is
granted only when this build implements it *and* the peer asked for it. A lane
this build does not implement is dropped from the intersection, not treated as a
malformed opening — refusing the whole connection over one unknown lane byte
would make lanes an ALPN-level change, which is exactly what the additive rule
exists to avoid. An opening that asks for lanes and would be granted none is
refused rather than admitted lane-less, because a connection neither side can
use is a slot held open for nothing.

## 5. Local surfaces

| Surface | Constant | Value | Note |
|---|---|---|---|
| Local DTOs | `runtime::dto::DTO_PROTOCOL_VERSION` | 1 | loopback control plane and viewer |

| Local control channel | `control::CONTROL_PROTOCOL_VERSION` | 15 | daemon socket; `MIN_SUPPORTED_CONTROL_PROTOCOL` is also 15, so the mixed-version window is currently empty |

DTOs are a local contract between the engine and its own clients. They are
versioned because a stale viewer bundle is a real situation, not because they
cross a trust boundary.

v7 moved the minimum because a v6 process would desynchronise on the new content
envelope. v8 moved it again because the web viewer's Live delivery became a
standing subscription. v9 replaces the Issues-specific doorbell fields with
World-tagged invalidation groups; accepting a v8 endpoint would decode a moved
field as its empty default and silently miss refreshes. v10 frames World call
payloads the way v7 already framed content — a declared length, then the bytes —
and a v9 endpoint would read those bytes as a malformed second request, which on
a channel that now reuses connections desynchronises everything after it rather
than failing once. In each case, accepting the older endpoint would promise a
capability it cannot provide.

## 6. The pinned dependency

`iroh = 1.0.0-rc.1`. It is a release candidate, so behaviour that LAIT *observes*
rather than *chooses* is recorded separately under "Frozen bounds, and which
are ours" in `PROTOCOL.md`, and measured
by `crates/comms/tests/transport_capabilities.rs`. When the pin moves, those
observations are re-measured; the LAIT policy numbers beside them are unaffected.

Observed and chosen are not the only two answers a frozen number can have. §7
adds the third.

## 7. Upgrade posture — whose number is it?

A frozen number stays re-examinable only if the reason it holds that value sits
beside it. There are three reasons, and they age differently.

**lait policy.** Chosen by us and moved when we decide it should move:
`MAX_OPENING_BYTES`, `MAX_FLOW_READ_BYTES`, `MAX_LANES`, the refusal
vocabulary, the ALPN generations. A dependency bump does not touch these.

**Observed.** Measured from the pinned transport rather than chosen:
`max_datagram_size`, send-buffer space, reset semantics. §6 governs them — when
the pin moves they are re-measured, and `MAX_DATAGRAM_BYTES` is advisory rather
than authoritative because a measurement already came in under it.

**Host-derived.** Calibrated to the machine lait was built on — one developer
laptop, with a laptop's disk, memory, and uplink — rather than to anything the
protocol requires. This is the category that needed adding, because a laptop
default frozen into a shipped constant becomes, a year later, indistinguishable
from a ceiling a peer is entitled to rely on. Nothing on the wire reads any of
them: a peer meets one only as a refusal it already has to handle, which is
precisely why an operator may raise or lower it without speaking a different
protocol. `PROTOCOL.md` §12.2 lists the same ceilings from the peer's side.

| Constant | Value | Scope of the ceiling | Operator-configurable today |
|---|---|---|---|
| `runtime::budget::slots::MAX_SPACE_CONNECTIONS` | 64 inbound | **per driver**, so per plane | no — compiled in |
| `runtime::budget::slots::MAX_CONNECTIONS_PER_PEER_PLANE` | 2 | per driver, per peer | no — compiled in |
| `runtime::budget::slots::MAX_LIVE_SESSIONS` | 32 | the Live plane's own | no — compiled in |
| `runtime::budget::slots::MAX_ENDPOINT_CONNECTIONS` | 128 inbound | **nothing enforces it** | no — compiled in |
| `runtime::budget::slots::MAX_STAGED_BYTES` | 64 MiB per Space | per Space | no — compiled in |
| `runtime::lifecycle::ContentOptions::cache_quota_bytes` | 4 GiB | per activation | yes, per activation |

Two rows say something this table used to imply the opposite of.

`MAX_SPACE_CONNECTIONS` is enforced inside each driver's own ledger, so it is a
ceiling **per plane** rather than per Space. Two drivers run now, so a Space's
real inbound ceiling is twice the number in that row. That is deliberate —
Freight and Live have separate queues and separate threads precisely so a
saturated transfer cannot delay a cursor — but the number is not what a reader
would have assumed.

`MAX_ENDPOINT_CONNECTIONS` has no enforcement site at all. What actually bounds
inbound sessions device-wide is `MAX_PENDING_OPENERS` (64) and the per-Space
queue depth in the transport hub. The row stays because deleting it would be the
third time somebody rediscovered that the constant is decorative.

The cache quota is the shape the rest are headed for: a default on an options
struct the composition root fills in, not a constant. Until they get there, this
table is the whole defence — "128 inbound connections" is otherwise a sentence
somebody quotes back as a rule of the protocol, which is exactly what it is not.

`ContentOptions::max_content_len` (256 MiB) sits beside the quota and is *not*
host-derived. It is an operator lowering plan 13's maximum, so it may only ever
move down, and a peer that meets it has met this Station's policy rather than a
limit of the format.

Ceilings the S2–S5 blueprint names as host-derived but this build has not yet
minted — `MAX_ENDPOINT_MEMORY_INFLIGHT`, `SPACE_MEMORY_INFLIGHT`, and
`CONN_MEMORY_INFLIGHT` — join this table when they land.
