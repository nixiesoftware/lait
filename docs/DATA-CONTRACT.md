# Data contract

This document defines LAIT's durable and replicated invariants. It describes
behavior rather than duplicating Rust types or byte layouts. Exact encodings and
versions are fixed by source, golden fixtures, schemas, and `PROTOCOL.md`.

## 1. Coordinates of a committed view

Every authorized World operation is evaluated at two explicit coordinates:

```text
(authority frontier, Manifest root)
```

The authority frontier selects historical Mechanics state. The Manifest root
selects the complete authenticated Body view. Runtime pins both before invoking
a World and compares both again inside the Station writer before committing.
Either coordinate changing causes the local operation to commit nothing.

A query is also pinned to both coordinates. Derived output must never combine
Bodies from one Manifest with policy or Bodies from another.

## 2. Durable stores and journals

Each Space participation has one orbital store. Its marker identifies the store
format before any mutable file is trusted. Unknown, foreign, truncated, or
unsupported formats fail closed; there is no legacy-store interpretation.

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

Mechanics and Fabric reuse the semantics-free journal mechanism but maintain
separate semantic manifests. A journal is not replicated product state.

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
references. Content references are Manifest data — they must survive a restart
and reach every participant — while the bytes they name are not.

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
- unavailable to Fabric and World callbacks;
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

## 7. Fabric representations

Fabric exposes two Body representation classes:

- atomic Bodies contain canonical application bytes and use Replica's explicit
  concurrent-head policy;
- collaborative Bodies use one Loro document per Body behind the generic Fabric
  interface.

The collaborative algebra includes:

- deterministic single-winner registers and map entries;
- stable-identity ordered lists;
- Unicode-scalar text splices;
- observed-remove, add-wins sets;
- per-peer PN-counters.

One path has one established type. Reusing it as another type is a transaction
error and changes nothing. A multi-operation Fabric batch is atomic.

Fabric convergence is mechanical, not semantic. A World selecting a register
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
Its Catalog has one deterministic Body identity per `(SpaceId, WorldId)` and is
created atomically by `InitializeTracker`. Missing, wrong, or duplicate semantic
Catalog state is corruption; it is never synthesized during open.

Issue content currently uses one Body per issue. Product schema—not Fabric—defines
the meaning of each field. The canonical conflict contract is:

- title and priority may use explicit deterministic scalar winner semantics;
- project movement must keep issue membership and board projection consistent;
- workflow status is represented by predecessor-bound transition records;
  concurrent live heads are a typed conflict until an authorized successor
  resolves them;
- descriptions use collaborative text where interleaving is acceptable;
- assignees and labels use membership sets;
- semantic history uses immutable events, not the Loro oplog.

The merged implementation still represents status as a register and comments as
Issue-Body list/events. That is sufficient for deterministic scalar convergence
and immutable flat comments, but it does not preserve concurrent transition
branches or support addressable replies/reactions/revisions. Before those
features are claimed, comments become first-class Comment Bodies:

```text
Comment
  id, issue, author, created_at, immutable parent_comment?
  revision heads
  actor-keyed reaction memberships
  tombstone/moderation revisions
```

Concurrent comment creation and replies all survive. Comment edits name their
predecessor revision; concurrent edits remain multiple heads until resolved.
Reaction membership is keyed by reaction and ActorId so a repeated reaction is
idempotent and concurrent actors do not overwrite each other.

These product rules must not introduce comment, issue, workflow, or project
types into Mechanics, Fabric, Replica, Runtime, or Comms.

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

Projections are deterministic views of one committed Manifest and authority
frontier. They are not replicated truth.

Every derived cache entry is keyed by the exact Manifest root whose Bodies it
contains. Per-Body reuse across roots additionally requires a reader-issued
version stamp that proves byte-equivalent constituent heads. A zero or unknown
root is not cacheable. A root mismatch rebuilds or advances before serving.

Activity, inbox, boards, graphs, aliases, and policy views must be reconstructable
from canonical Bodies and Mechanics history. Observation frames are doorbells;
after a reset or overrun, clients re-query the projection.

Projection distinguishes valid, absent, unavailable, and corrupt data. It must
not turn an unavailable query into false zero counts or silently coerce malformed
stored values into valid DTOs.

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
must not appear in Debug output, logs, DTO examples, Fabric, Manifests, or Contact
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
