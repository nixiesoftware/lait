# Data contract

This document defines LAIT's durable and replicated invariants. It describes
behavior rather than duplicating Rust types or byte layouts. Exact encodings and
versions are fixed by source, golden fixtures, schemas, and `PROTOCOL.md`.

## 1. Coordinates of a committed view

Every authorized World operation is evaluated at two explicit request
coordinates:

```text
(authority frontier, Manifest root)
```

The authority frontier selects historical Mechanics state. The Manifest root
selects the complete authenticated Body view. Runtime pins both before invoking
a World and compares both again inside the Station writer before committing.
Either coordinate changing causes the local operation to commit nothing.

A query additionally pins the complete semantic identity of the World read
image:

```text
PublicationId {
    manifest_root,
    implementation_digest,
    extractor_schema_digest,
}
```

The root alone is insufficient: activating another reviewed implementation can
change the meaning or extraction of unchanged Bodies. `PublicationId` is the
portable coordinate stored in semantic records and historical queries. A live
Station extends it with a local monotonic `MaterializationId`, because arriving
keys or authority material can make an opaque Body readable without changing
the Manifest. Cursors and local cache leases bind that complete
`WorldPublicationId`; a materialization mismatch expires rather than falling
forward.

Derived output must never combine Bodies, implementation semantics, extractor
semantics, or policy from different coordinates. Authority remains a request
coordinate: the shared corpus is principal-neutral, and disclosure gates are
applied before traversal, ranking, counting, or packing.

## 2. Durable stores and journals

Each Space participation has one orbital store. Its marker identifies the store
format before any mutable file is trusted. Unknown, foreign, truncated, or
unsupported formats fail closed in every normal open path. Prior-format
interpretation exists only inside an explicit generation builder; it can read a
committed prior representation but can never make that representation appear
current in place.

Durability uses immutable content-addressed objects plus an atomically replaced
manifest:

1. reserve and persist a strictly advancing sequence;
2. write a `Prepared` journal record;
3. write and sync temporary objects;
4. record `MaterialReady`, place objects, and sync their directory;
5. replace and sync the authoritative manifest last;
6. acknowledge, record completion where required, and remove the journal.

Sequence gaps are allowed; reuse is not. Recovery exposes the complete old
manifest or the complete new manifest. Corruption, a lagging/missing counter, or
an object whose bytes do not match its reference is an integrity error, never a
cache miss or an invitation to reconstruct guesses.

Mechanics and Engine reuse the semantics-free journal mechanism but maintain
separate semantic manifests. A journal is not replicated product state.

An Orbit separates durable facts from their current local materialization.
Representation changes use this lifecycle:

```text
committed source facts -> Build -> immutable Generation -> Verify -> Activate
```

`Build` writes both the Mechanics and Replica components under an isolated
generation directory. Mechanics verifies the signed effect set and frontier;
Replica verifies the Body and receipt catalogs and reopens the result through
the current transaction, protected-material, index, receipt, and Manifest
validators. Their evidence digests commit to logical facts, not checkpoint or
index layout. `Activate` then compare-and-swaps one canonical
`active-generation` pointer. The switch therefore selects both components or
neither; a process can never observe new authority material with an old Replica
materialization.

No pointer means the original implicit generation. Once an explicit generation
is active, normal Mechanics and Replica opens resolve through the same pointer.
Identity secrets, marker, epoch, lock, neighbours, and content cache remain at
the stable Orbit root. A local representation activation authors no Space
authority effect and changes no World implementation identity. Conversely, a
semantic World change still requires its own reviewed implementation activation
and cannot be smuggled through a store rebuild.

Not everything under a store directory is journaled. An Orbit's directory also
holds `content-cache/`, a sibling of the journal rather than a part of it,
because the journal's promise — everything a root names is present and intact —
is the wrong promise for a content chunk, which is optional by design. A missing
object means the store is broken; a missing chunk means fetch it again. §13
states what lives there and what activation reclaims; `COMPATIBILITY.md` §2
states why it carries no format version.

A store manifest names **index roots**, not inventories. An index is a
persistent authenticated radix map from a 32-byte key to bounded bytes: one
nibble per level, leaves holding at most 256 entries, nodes content-addressed
and immutable, and therefore themselves ordinary journal objects under the same
object domain. Two indexes with the same contents have the same root regardless
of insertion order, and an update rewrites only the spine from the changed leaf
to the root — so commit cost is proportional to what changed, not to what is
stored.

Canonical shape is part of the contract, not an optimisation. A leaf that could
have been merged into its parent, or a branch that should have split, is an
integrity error on read: two encodings of the same map would otherwise produce
two roots, and the root is what a signature covers.

## 3. Mechanics authority data

Mechanics stores signed effects, graph/index deltas, authority checkpoints, and
batch receipts. Signed effects remain the semantic source of truth; checkpoints
are verified materializations that accelerate exact historical evaluation.

An `AuthorityFrontier` contains only heads that may change ordinary authority:
actor/device history, ACL and scoped policy effects, and terminal
`SpaceAuthority` effects. Ceremony proposals, rounds, custody attestations, and
abort/completion traffic use `CeremonyMaterial` and a separate bounded cursor.
They never become ordinary authority heads.

An authority batch is all-or-nothing. Its receipt binds the Space, prior
frontier, resulting frontier, and ordered batch digest. A prefix of a rejected
batch cannot survive restart.

Historical evaluation never substitutes current authority. A grant, revoke,
device state, World implementation activation, or delegation is interpreted at
the exact referenced frontier.

## 4. Body transactions and Manifests

Replica is the authority for the protected Body graph. A signed Body transaction
binds:

- Space, World, author actor/device, and request identity;
- its parent Manifest root and authority frontier;
- World implementation and schema bindings;
- ordered Body descriptors and protected payload commitments;
- intent, operation, effect, demand, and authorization-receipt digests.

The transaction id identifies the complete signed envelope. Reusing a request
identity with identical bytes returns the original result; reusing it with
different content is a conflict.

A signed Manifest commits a complete Body set through one canonical
authenticated index (§2). Entries are globally ordered by key. A Body may have multiple constituent transaction heads;
concurrent writes are retained rather than collapsed into a single transport
winner. Same-coordinate equivocation rejects.

Adopting a Manifest is atomic. Replica validates and stages every required
transaction, protected payload, authority dependency, schema binding, and quota
before one journal commit installs the complete replacement root. No accepted
prefix is externally visible when a later item fails.

Remote work may reference a verified historical or concurrent parent rather
than the receiver's current root. That exact parent's authenticated snapshot
must be reconstructable. Missing material returns a retryable
`ParentManifestUnavailable`; current state is never substituted.

A Manifest root is a fixed-size record. It carries the Space, the Replica
frontier, a body index root with its count, a content index root with its
count, the signer, and the authority frontier the signature is evaluated at. It
does not carry, and must never carry, a list of Bodies: an advertisement whose
size grows with the store is how a Space stops being able to announce itself.

A Body entry names its concurrent heads and the `ContentRef`s that Body
references. Each signed head binds Fabric causal `Material`: one checkpoint,
its bounded delta tail, history/version coordinates, and the content-addressed
`ArtifactRef` closure needed to reconstruct it. Ordinary collaborative edits
therefore transfer and persist update-sized causal artifacts rather than a
second whole-Body export. Out-of-order and duplicate artifacts are admissible;
the declared final version and complete dependency closure must verify before
publication.

Full checkpoints are the ordinary cold-start base. A shallow checkpoint is an
explicit compaction operation with a retention frontier, never a routine
checkpoint: peers that can still author concurrent pre-frontier changes need a
full checkpoint or retained history. Historical generation records name causal
Material and reconstruct the nearest visible Body; they do not store plaintext
or repeated `BodyExport` snapshots.

Checkpoint creation is prepared before a collaborative Body reaches the hard
tail bound. Crossing the soft watermark captures an immutable causal seed and
builds the full checkpoint away from the committing writer. A later edit may
install that checkpoint together with only the deltas it did not cover. The
256-delta/8-MiB target is a maintenance watermark, not an action-path snapshot
trigger. If the bounded worker cannot catch up, the tail may advance only to
the explicit 4096-delta/128-MiB emergency envelope; beyond it the write returns
typed checkpoint backpressure. No edit synchronously serializes a full Body
merely because it was the Nth edit.

Content references are Manifest data — they must survive a restart and reach
every participant — while the bytes they name are not.

An advertisement carries **both** indexes or it is refused. A `ContentRef` is a
name; asking for the bytes behind it needs the geometry, the epoch, and the
Merkle root, and those live only in the descriptor. A root that declares content
its content index does not carry names bytes the receiver could never ask
anyone for, so a receiver rejects it whole rather than adopting a declaration it
cannot resolve. Already holding the descriptor satisfies the rule as well as
receiving it does: convergence is incremental, and a comment on an issue must
not re-resolve that issue's attachment.

The content index advertises what live Bodies reach, and only that. A descriptor
awaiting its holder's own sweep is that holder's garbage; pushing it at a peer
would grow the peer's catalog from ours, and the peer's own sweep would then
have to undo it.

An observer keeps a bounded number of roots per signer and evicts the oldest
rather than refusing new ones; a peer that publishes quickly must not be able to
exhaust a watcher's memory, and must not be able to silence itself either.

## 5. Protected and opaque Bodies

Protected Body payloads are content-addressed and bound to their descriptor,
Space, schema, and encryption/key context. Plaintext never appears in a
Manifest or Contact framing metadata.

A legitimate Body whose World, schema, or key is unavailable remains opaque:

- retained byte-for-byte;
- counted against quotas;
- included in graph and Manifest completeness;
- unavailable to Engine and World callbacks;
- forwardable to another legitimate participant.

Opaque retention does not grant authority and cannot bypass historical receipt
validation. Becoming interpretable later requires validation through the normal
Replica path.

## 6. Content plane

Content is bulk immutable bytes referenced by a Body: attachments, images, and
files. It is a separate plane from Body payloads because it has a different
completeness contract. A Replica is **descriptor-complete** — it holds every
`ContentDescriptor` its Manifest references, and that is required for the root
to reconstruct. It is not byte-complete: chunks are fetched, cached, and
forgotten locally without changing a single committed root.

A descriptor is the whole identity of one ingest:

```text
format_version, space, content_nonce, plaintext_len,
chunk_plaintext_len, chunk_count, ciphertext_merkle_root, epoch
```

`content_nonce` is random per ingest, so two ingests of identical bytes are
different content. That is deliberate: convergent encryption would make
identical plaintext detectable across Spaces by anyone who can guess it.

The Merkle tree is built over **ciphertext** leaves. A peer serving a chunk to
someone who cannot decrypt it still proves the bytes are the right bytes, and a
provider needs no key to be useful. Chunk plaintext is a fixed 256 KiB except
the last; an odd node is promoted rather than duplicated, so no two distinct
chunk sequences share a root.

Each chunk is sealed independently under associated data binding the Space, the
content nonce, and the chunk index — and deliberately *not* the chunk count or
plaintext length, both of which the Merkle root already commits. Omitting them
is what lets a sender seal chunk `n` before it knows how many there will be.

A chunk is only servable when its ciphertext **and** a validated proof sidecar
are both resident locally. Residency, leases, and pins are local state (§13) and
never appear in a Manifest.

A fetched chunk and its validated proof sidecar are **cache state held under a
lease**, not required objects. They live in the resident cache, not the journal's
object store, and reaching them is a different call on purpose: a caller cannot
satisfy a required-object reference out of the cache, and a cache miss is
`NotResident` rather than an integrity error. Bytes and sidecar are one entry
published by one rename, so there is no window in which a proof exists and its
chunk does not — a half-existing entry is the one state this cache must not be
able to reach. Evicting an entry changes no root: the descriptor stays, the
Replica is still descriptor-complete, and what was lost is refetchable.

Between committing a descriptor and committing the Body that names it, nothing
declares the content — so by the reachability rule it is already collectable,
and the rule is not wrong: nothing on disk distinguishes an upload awaiting an
attach from an upload nobody ever attached. A **hold** distinguishes them. It is
in-memory and carries a deadline, and both properties are load-bearing: a hold
is a claim about an operation this process is running, so a restart correctly
ends it, and a deadline is what stops an upload nobody attaches from becoming
permanent disk.

A hold answers "may I delete this" and not "may I show this to a peer". Held
content is kept locally and never advertised — a peer receiving a descriptor no
Body names would adopt catalog it has no reason to keep, and its own sweep would
have to undo it.

Content reachability is derived, never counted. A stored count would be a second
source of truth that can disagree with the Bodies; what is authoritative is the
set of `ContentRef`s the committed Manifest names, plus explicitly declared
local intents. Sweeping removes only what neither reaches.

## 7. Engine representations

Engine exposes two Body representation classes:

- atomic Bodies contain Arc-shared canonical application bytes and use Replica's
  explicit concurrent-head policy;
- collaborative Bodies have one independent Loro history per Body behind the
  generic Engine interface. Cold state is an Arc-shared canonical export plus a
  compact causal Version; only a bounded least-recently-used set is inflated as
  live `LoroDoc`s for mutation.

An immutable read generation shares each unchanged Body payload with Engine and
with prior generations. It does not retain a second projected collaborative
view. Projection and anchor resolution decode only the explicitly visited Body;
recovery proves material once and hands that verified frozen image to the
long-lived Engine without a second import.

The collaborative algebra includes:

- deterministic single-winner registers and map entries;
- stable-identity ordered lists;
- Unicode-scalar text splices;
- observed-remove, add-wins sets;
- per-peer PN-counters;
- stable-identity movable trees, with per-node data entries;
- append-only logs, whose state is a bounded retained tail plus an exact count
  of everything ever appended.

One path has one established type. Reusing it as another type is a transaction
error and changes nothing. A multi-operation Engine batch is atomic.

Sequence placement — a list index, a tree node's position among its siblings —
is a statement about the writing replica's own view and stays one after merging.
A replica fifty elements behind places its insert fifty back, and no sequence
type makes that retroactively mean "the end". What converges is that every
replica then agrees where it went. A World that needs a chronology orders by a
field its own records carry; it does not read one out of the sequence.

Engine convergence is mechanical, not semantic. A World selecting a register
accepts that concurrent values collapse to one deterministic projection. If the
product must preserve concurrent intent, require explicit predecessors,
immutable records, or revision heads built from generic Bodies. Application code
must not infer a different hidden winner after reading the merged primitive.

## 8. World schemas and containment

Every operation identifies its target World, Body, schema, schema version, and
mutation model. Runtime rejects:

- undeclared or inactive schemas;
- writes outside the Session's World;
- cross-World or cross-Space Body references;
- operation/model mismatch;
- incompatible duplicate declarations;
- excessive paths, operations, or bytes;
- reads or writes outside the callback's bounded view.

A World effect contains one non-empty canonical authorization demand. Runtime
does not supply an implicit write grant. Query projections likewise carry an
explicit read demand that Mechanics evaluates before returning data.

The authority-approved `WorldImplementationId` pins the descriptor, policy
table, schemas, and artifact identity that selected the demand. Remote adoption
validates the bound identity without executing the World.

## 9. IssuesWorld data

IssuesWorld is the canonical first-party World, not a privileged lower layer.
`InitializeTracker` creates bounded singleton metadata and the migration marker;
there is no Space-wide product-state Body. Missing, wrong, or duplicate
semantic identity is corruption and is never synthesized during open.

An issue attachment is a `ContentRef` and a size, not bytes. The record names
content the content plane already holds, and the Body declares that reference —
which is what makes reachability, prefetch, and progress attribution work
without anything decoding product bytes. `size` means plaintext bytes in both
record shapes.

The pre-v1 migration resolves legacy inline attachment payloads into Content and
rewrites their exact references before v4 activation. A v4 record carries a
valid `ContentRef` or is refused; normal readers do not retain a second inline
payload grammar.

Product schema—not Engine—defines the meaning and conflict rule of each field.
The canonical conflict contract is:

- title and priority may use explicit deterministic scalar winner semantics;
- project movement must keep issue membership and board projection consistent;
- workflow status is represented by predecessor-bound transition records;
  concurrent live heads are a typed conflict until an authorized successor
  resolves them;
- descriptions use collaborative text where interleaving is acceptable;
- assignee, label, team, initiative, and other membership changes are stable
  relation records rather than an aggregate set owned by another entity;
- semantic history is immutable activity records, not the Loro oplog and not a
  truncated Issue log.

Each comment is a deterministic record Body. Its immutable parent comment is a
stable id, so concurrent replies survive without a shared tail or a positional
tree insertion. Siblings are ordered by `(created_at, comment id)` from record
fields. A reaction is one LWW register Body keyed by the exact
`(issue, comment, emoji, actor)` tuple; concurrent actors therefore never
overwrite one another and repeated intent is idempotent.

Sub-issue parentage and project links are stable relation Bodies. The corpus
maintains forward and reverse adjacency and Geometry detects SCCs explicitly;
no project-wide tree or map is rewritten to add one edge. A request that needs
acyclic semantics validates the exact predecessor/publication and records a
typed conflict when concurrent edges close a cycle.

An activity cursor names the stable record/order tuple, not a count of rows.
Paging is over the shared ordered corpus posting, so adding or removing another
record cannot silently shift a numeric resume position. A pull that returns
nothing retains the supplied continuation coordinate.

Comment revision and moderation can extend the comment record family without
making the Issue Body coarse:

```text
Comment
  id, issue, author, created_at, immutable parent_comment?
  revision heads
  actor-keyed reaction memberships
  tombstone/moderation revisions
```

Concurrent comment creation and replies all survive. Comment edits name their
predecessor revision; concurrent edits remain multiple heads until resolved.

These product rules must not introduce comment, issue, workflow, or project
types into Mechanics, Engine, Replica, Runtime, or Comms.

## 10. Scoped authorization data

Mechanics stores effective generic assignments over exact World resources and
capabilities. IssuesWorld stores product role and workflow definitions and
expands them before requesting an authority mutation.

A role-definition edit affects future expansion only. Existing assignments and
outstanding invitations retain their exact revision provenance and expansion.
Changing or deleting a Body projection cannot grant or revoke authority.

Every authorization receipt binds the principal, historical frontier, parent
Manifest, active World implementation, demand, policy witness, intent, complete
operations, and transaction core. Substitution of any bound coordinate or
digest rejects.

## 11. Contact and convergence

Contact is a bounded framing protocol. It transfers signed Mechanics material,
Manifest advertisements and index nodes, transactions, and protected Body
material. A
transfer acknowledgment proves only framing receipt.

An initiator may declare the Body-head commitments it already holds. Holdings
are a canonical strictly increasing unique sequence. Zero entries require the
defined empty digest. The declaration is signed and bounded. The accepter still
advertises the full Manifest and omits only declared heads; the receiver adopts
nothing unless local plus transferred material reconstructs the complete root.

Received bytes remain staged and inert until:

```text
Mechanics validates authority material
  -> Replica validates transactions, receipts, parents, payloads, and quotas
  -> one durable Manifest adoption
  -> one convergence result and Observation publication
```

A false holdings declaration can prevent its claimant from completing a root;
it cannot cause partial or corrupt adoption.

Equal Manifest roots prove equal *catalogs*, not equal readable material: a
declaration deliberately omits opaque heads, so two peers can agree on a root
and still owe each other bytes. Agreement on a root is therefore never a reason
to skip serving.

Where a catalog is large, peers may reconcile by descending the shared index
instead of transferring it: matching subtree hashes are skipped whole, and only
divergent spines are walked. The descent is bounded by depth and by the number
of nodes a single reconciliation may request, and it changes only how a
difference is *found* — adoption still runs the full validation path above.

## 12. Projections and caches

Projections are deterministic reads of one published World image under one
request's authority frontier. They are not replicated truth.

`WorldPublication` is the one atom readers pin: an immutable Replica snapshot,
its principal-neutral extracted corpus, and their exact `WorldPublicationId`.
The Station swaps that atom whole. It never publishes snapshot, corpus, or
coordinate pointers separately, and never serves an old corpus under a new
implementation or Manifest root. Retained publications are bounded; a cursor
whose publication or materialization has expired receives a typed expiration
and must resnapshot.

A continuation leases the exact retained publication it names. Leases have a
bounded lifetime and station-wide retained-memory budget, charged once per
unique publication rather than once per cursor. Admission is based on the
compact corpus shape, so a million-link publication may be retained while a
second equally large pin is refused if it would exceed the station budget.
Expiry and capacity refusal are typed; neither may fall forward to current
state or recompute a page against different coordinates.

The governing cost rule is:

> Pay bounded incremental work once per published generation; share it
> immutably; make every subsequent operation proportional to explicitly visited
> or returned material.

The corpus uses generation-local compact node identities, persistent indexes,
bidirectional edge postings, and late result materialization. Updating one Body
retracts that Body's prior extraction and inserts its replacement while sharing
unchanged roots. Find is the only query evaluator: viewer, CLI, controller,
Exec, and agent access paths submit the same typed operator DAG through a
Session. Disclosure gates run before traversal, ranking, counts, and packing, so
unauthorized material cannot influence even an aggregate or order.
An ordered interval is one `Seek::FieldRange` with explicit inclusive or
exclusive endpoints. Corpus seeks to the lower endpoint and stops at the upper
endpoint before evaluator work; expressing the upper bound as a later `Keep`
is not equivalent because it can visit unrelated postings after the interval
and violate both deep-lookup latency and bounded metering.

Activity, inbox, boards, graphs, aliases, and policy views remain
reconstructable from canonical Bodies and Mechanics history. An Observation
carries only authenticated attribution plus bounded, value-free Body/path/range
changes. Collaborative text ranges carry Fabric anchors together with scalar
offsets stamped to the exact candidate publication. The anchors preserve the
causal endpoints while the offsets let a viewer render immediately at that
publication; a range that cannot be proven degrades to `Dirty` instead of
guessing. Observations are actionable feedback, not another state source: a
client may use the range for cursor/highlight movement and a `Seek::Bodies` Find
to refresh all affected entities in one bounded query. Dirty, reset, overrun,
or expired coordinates require a fresh projection read.

Feedback has one operation-correlated phase contract across human and agent
access paths. `Sending` is painted locally within one frame before network or
action-sized work. `Accepted` exists only after a bounded durable operation
receipt, and `Committed` carries the exact terminal `WorldPublicationId`.
While work is pending, the client retains the prior exact projection, loaded
pages, selection, cursor, scroll position, and any deterministic optimistic
overlay; refresh may not blank, collapse, or fall back to a different
publication. Bounded progress is transient and may be coalesced, but it keeps
the same operation identity and runs off UI, reactor, Replica, and publication
locks. A refusal visibly reconciles the optimistic overlay with its typed
cause. Therefore action size may affect completion time but never bounded
time-to-feedback, continued interaction, or visual continuity.

Blueprint is the Issues World bundled in Lait, not another layer in the generic
engine. Lait owns publication, extraction, gates, Find, and causal storage.
Blueprint owns the meanings and physical sharding of Issue, relation, Plan,
project, team, label, milestone, status, and closure.

An Issue remains the durable core anchor. Its existing Body key and
collaborative history survive migration so range anchors retain their Body and
operation identity. V4 adds stable identity and alias roots; current board truth
is not one flat placement register. Every move authors an immutable,
predecessor-bound `IssueTransitionRecord` whose placement names a project,
workflow state, stable block, and local position. Issue metadata retains an
add-wins set of self-authenticating transition heads. Exactly one head emits a
board placement, zero means absent or migration-incomplete, and multiple
causally maximal heads are an explicit visible conflict that is inert on the
board until a successor resolves it. Enrichment does not accumulate inside the
Issue core: comments, reactions, durable activity, labels, assignments,
membership, and other independently edited relationships are record-addressed
Bodies.

Board order is two-level and exact-publication scoped. A lane carries
predecessor-bound topology heads; each stable `BoardBlock` carries an
authenticated block-order label; each sole Issue transition carries a local
label within that block. A leaf holds at most 128 Issues. Splitting a full leaf
relabels at most that leaf and publishes exact-transition-fenced Issue overlays
atomically with the topology successor; block maintenance is likewise fenced to
the exact block revision. A stale overlay is ignored if its transition,
project, state, or block no longer matches. Public traversal orders workflow
states by their declaration, then blocks by `(block_order, BlockId)`, then
members by `(local_position, IssueId)`, using one exact-`WorldPublicationId`
nested continuation. No flat `PROJECT_STATE_POSITION` projection is current
board truth.

The old Space-wide Catalog is not replaced by one merely smaller project blob,
nor by shards whose size depends on an unenforceable concurrent tail counter.
A durable entity or relationship that can be edited independently owns its own
deterministic Body: milestone, cycle, update, triage submission/decision/
resolution, hierarchy/link edge, comment, reaction, activity event, label,
workflow/role revision, Spec/Baseline revision, and initiative/team membership.
Compact project Bodies contain only genuinely bounded singleton metadata or
heads. The shared corpus and its schema postings are the directory and ordering
surface; aggregate maps are not a second authoritative catalog. A stable
relation is extracted as its own node, so each corpus node has one source Body
and reverse traversal does not require cross-Body row assembly.

V3 to v4 is one launcher-authorized, crash-resumable migration protocol, not one
unbounded transaction and not a public Issue intent. Accepting the exact World
update mints an in-process step capability bound to the source, migrator, and
target identities; the caller cannot supply its actor or invoke the protocol
through MCP. Its durable marker, canonical cursor, and audit log advance in
deterministic batches below Replica's operation and byte ceilings. Issue roots
are added in place. New record Bodies have deterministic keys, so a retry cannot
allocate a second home for the same fact. A completed migration activates
v4-only interpretation; pre-v1 Worlds use one-time migrations instead of
carrying compatibility branches indefinitely.

A Plan remains `Spec::Kind::Plan` and uses the immutable Spec revision DAG,
links, lifecycle, and baseline semantics. A Plan revision stores a bounded root
selection and the full portable `PublicationId` against which it was composed,
never a root alone. Empty roots select the project. Phases, issue membership,
progress, cycles, and a chosen global shape are derived rather than copied into
the revision.

Exact Issue and relation facts live in the corpus. Geometry is a separately
named immutable analytical artifact keyed by:

```text
WorldPublicationId
  + projection schema digest
  + canonical project/root selection fingerprint
```

It preserves dependency SCCs, layers, containment regions, closure, slack, and
residual loci without caching full result rows. Reads return `Ready`, `NotReady`,
`Unavailable`, or `Expired` for that exact key; they never silently substitute
the current graph. Budget estimation happens before global compilation.

This separation is required for exactness at scale. A single edge can turn a
long acyclic chain into one strongly connected component, changing geometry for
every node. No memory layout can make that semantic blast radius constant. Core
facts and local lookup still publish immediately; globally sensitive Geometry
may reuse prior structure or build as an explicit artifact whose source
publication is visible. Fact freshness and analytical readiness are therefore
both honest instead of one being hidden behind stale output.

Projection distinguishes valid, absent, unavailable, and corrupt data. It must
not turn an unavailable query into false zero counts or silently coerce malformed
stored values into valid DTOs.

## 12.1 Transient and signal state

Neither is durable, and neither is a partial delivery of something that will be.

Transient state (cursors, presence, typing, residency hints) is local to a
Station and expires on its own. It never enters the journal, never becomes an
Observation, and is gone after a restart — which is correct rather than a
limitation, because a cursor that survived the process holding it belongs to
nobody.

Reliable signals are delivered or they fail loudly, and they are durable in no
other sense. `crates/runtime/src/signal.rs` may not name the Replica writer or
the Observation ring, and a parser gate enforces that: privacy cannot, because
`Broadcaster::publish` is `pub(crate)` and the signal module sits inside that
crate, while StationCore's explicit metadata/control Replica seams are public
to the Runtime composition. One line is the whole distance between the design
and a violation — a `publish` from signal code would journal nothing and still
emit an Observation, which `StationHost::frame_for` turns into
`activity_advanced` for anything carrying scopes.

The parser gate is half of it. The other half runs: ten thousand delivered
signals leave the frontier, every byte under the store directory, and the
Observation sequence identical, and one ordinary commit in the same file moves
all three. The negative control is not decoration — a run that passes because
nothing was driven is indistinguishable from a run that passes because signals
are not durable, unless something shows the observables moving when they should.
The behavioural half also catches what the parser cannot: a handler reaching the
Replica *indirectly*, through a World submit several calls away, on a path the
parser reads as ordinary.

**A delivered signal is an event, not a state.** A listener subscribes and hears
what follows; it cannot re-read what it missed, and a restart delivers nothing
that was signalled before it. That is the same shape as a person who was out of
the room, and it is deliberate: a queue that survived would be a durable record
of who was contacted, which is precisely what a signal must not leave behind.

**A queued file offer is the one transient thing with no TTL.** It holds a
sender, a content id, a length and a display name — never bytes. A cursor
expires because a cursor that stopped moving is stale; an offer that has been
waiting an hour is exactly as valid as when it arrived, so only a decision or a
revocation removes one. It is still not durable: it lives in memory and a
restart forgets it, and the file it names is unaffected either way.

**What a Station publishes about itself is not in what it reads.** Two maps, not
one. A viewer whose own presence appeared in the table it reads would draw itself
beside everybody else, on every screen, for as long as it was looking.

**A World's declared scopes and signals are enforced, not merely reviewed.** A
scope naming a schema its World never declared is refused at subscription, before
it occupies a slot; a signal past its World's declared ceiling is refused before
its payload is acted on. Without that the descriptor's sections would move an
implementation id — every peer seeing a different reviewed build — and buy
nothing. A World that declares nothing keeps the id it had.

**A display name from a peer is stored exactly as sent.** It is sanitised where
it becomes a path and nowhere earlier. Rewriting on arrival would mean the name
shown to a person is not the name that was sent, and would break the
re-encode-equality rule every wire shape in the system rests on.

## 13. Local-only state

Device private keys, actor recovery material, custody shares, local petnames,
configuration, route/backoff state, space navigation, disposable projection
caches, resident content chunks and their proof sidecars, leases, pins, staging
areas, transfer progress, provider-selection statistics, and delivery-plane
session state are local state. They are not product Bodies and do not gain
authority by
being stored beside an Orbit.

Residency is held by lease, and a lease is scoped to the operation that took
it. Releasing an operation releases everything it held, including on the paths
where the operation failed — a reader that dies must not pin bytes forever.
Eviction under quota may remove anything unleased and unpinned; it may never
remove a descriptor, because that would break Manifest reconstruction.

Transfer progress is local state and nothing else. It is never a Body, an
Observation, a journal entry, a frontier change, a Doorbell, a Beacon, or an
activity item. It lives in `runtime::transfer::TransferRegistry`: one entry per
in-flight operation, its own watch channel that a reader re-reads a snapshot
from, and a 64-entry tail of finished transfers so a caller that asked a moment
ago can learn how it went. Both halves are bounded — the active set by the
ceiling on concurrent transfers, the completed tail by its fixed length. A
registry that grew one entry per completed transfer would be a memory leak with a
respectable name.

It is deliberately not on the Observation ring. That ring's contract is that an
entry corresponds to a durable commit; a progress frame corresponds to bytes
arriving on a socket. Putting the second in the stream of the first would give a
fact nobody agreed to a sequence number among facts everybody did, and would let
a chatty transfer push real commits out of a slow consumer's window. A watcher
that stalls on the progress channel falls behind on progress and on nothing else.
Progress is monotone, so coalescing loses nothing: a state change publishes
immediately, a moving byte count at most twice a second.

No part of a transfer survives a restart. The ladder — queued, connecting,
transferring, verifying, available, and the cancelled, failed, and evicted exits
— describes this machine's disk and this machine's network, and a peer's opinion
about it is not evidence. A transfer's lifetime is tied to a handle whose drop
fails it and releases what it held, so a fetch that panics or returns early
through a `?` cannot leave an operation lease pinning chunks nobody is fetching.

Provider-selection statistics are local and are not replicated truth. Which peers
answered `Have`, what they recently delivered, how often they refused or stalled,
and any operator preference among them are one Station's opinion formed from its
own measurements. They are never advertised, never committed, and never an input
another peer can influence except by actually serving well or badly. Two Stations
fetching the same content may choose different providers; neither is wrong, and
neither is evidence about the other.

The resident cache is an additive store-layout addition, not a journal change.
`content-cache/` sits beside the journal inside the Orbit's directory and holds
chunk entries, the tag directory recording leases and pins, and the staging area
for partly arrived chunks. It is a sibling rather than a part for the reason §2
gives: the journal fails closed on anything missing, and a chunk is allowed to be
missing. No marker, root, or Manifest names it, and deleting the whole directory
costs refetches and nothing else.

Activation opens that cache and immediately reclaims every operation lease and
every staging slot, because nothing was in flight before the Station existed —
any lease or partial on disk belongs to a run that is over. Content leases are
untouched: they belong to committed content and no restart makes them stale, so
installed chunks survive and an interrupted fetch resumes at chunk granularity
rather than from zero. One Station owns one cache over one directory; a second
cache over the same directory would sweep the first's staging. Dormancy leaves
the directory exactly as it is — the cache is durable local state, and the next
activation reclaims whatever the last one abandoned. Deorbit removes it with the
rest of the store, which is the only correct answer: the bytes were refetchable
copies of content this device no longer participates in.

A delivery-plane session id and epoch are local, per-connection, and randomly
minted. They are not identity, they confer nothing, and they exist so that a
replayed opening is recognisable as one.

Secrets are written with restrictive permissions and atomic replacement. They
must not appear in Debug output, logs, DTO examples, Engine, Manifests, or Contact
frames except for an explicitly authenticated encrypted custody package.

## 14. Evolution

- Store, wire, schema, and signed formats carry explicit versions and reject
  unknown incompatible input.
- Semantic Rust names do not carry protocol-version suffixes.
- Stored keys, tags, and signed action meanings are never repurposed in place.
- Canonical ordering, exact decoding, bounds, hashes, domains, and winner rules
  are compatibility surface.
- A new product conflict rule is a World-schema decision and requires
  convergence tests under arrival-order permutations and restart.
- Backward compatibility exists only when explicitly specified; there is no
  legacy architecture fallback.

## 15. Known limitations

- Lazy revocation cannot erase plaintext or keys previously copied by a removed
  participant.
- Trusted native World implementations are not sandboxed or remotely attested.
- Ceremony, recovery, custody, and the composed protocol remain security-review
  sensitive despite using established primitives.
- Full reference performance measurement is scheduled/manual; PRs use the smoke
  corpus and structural complexity gates.
