# Engineering research docket

This docket records research that constrains Lait's implementation. It is not a
literature survey. Each reference below must either change a design decision,
rule out a tempting shortcut, or create a measurable acceptance gate.

## Governing invariant: Constant-Time Feedback Continuity

"Constant-time" applies to the externally visible admission transition, not to
the whole operation. Before work proportional to a Body, project, corpus,
migration, or compaction range begins, Lait performs bounded validation and
reservation and emits either `Sending`/`Accepted` or a typed visible refusal.
The exact prior view remains readable and interactive, with any explicitly
tentative local overlay distinguished from durable truth. Size-proportional
work proceeds off publication and interaction locks. Its terminal event names
the same canonical operation and either the exact committed publication or a
typed failure. Human, agent, and incorporated peer work use these same phases.

The systems evidence refines that invariant rather than creating another
subsystem:

- Welsh, Culler, and Brewer, *SEDA: An Architecture for Well-Conditioned,
  Scalable Internet Services* (SOSP 2001), decomposes work into explicit queued
  stages whose admission and resources can be controlled independently:
  <https://doi.org/10.1145/502034.502057>
  Adopt bounded admission, reservation, and off-lock stages. Reject an
  unbounded queue or synchronous call chain that delays the first observable
  outcome until all downstream work completes.
- Terry et al., *Managing Update Conflicts in Bayou, a Weakly Connected
  Replicated Storage System* (SOSP 1995), explicitly distinguishes tentative
  writes visible to clients from writes whose order and conflict outcome have
  committed: <https://doi.org/10.1145/224056.224070>
  Adopt an honest visible distinction between optimism and durable commitment.
  Reject presenting `Accepted` as committed, silently reinterpreting an
  optimistic operation against a later publication, or losing its identity
  when remote ordering or conflict changes the final outcome.
- Rae et al., *Online, Asynchronous Schema Change in F1* (PVLDB 2013), keeps
  reads and writes available while a correctness-constrained schema transition
  advances asynchronously:
  <https://research.google/pubs/online-asynchronous-schema-change-in-f1/>
  Adopt resumable bounded migration stages while the exact old interpretation
  remains usable. Reject a migration that holds the foreground path, exposes a
  half-migrated target, or reports activation before semantic verification.
- Balmau et al., *SILK: Preventing Latency Spikes in Log-Structured Merge
  Key-Value Stores* (USENIX ATC 2019), shows that throughput-oriented
  compaction alone does not protect foreground tail latency and adds explicit
  scheduling, prioritization, and preemption:
  <https://www.usenix.org/conference/atc19/presentation/balmau>
  Adopt governor-reserved, low-priority, preemptible maintenance. Reject making
  a user or agent action wait for Body checkpointing, corpus consolidation, or
  cache compaction merely because a maintenance watermark was crossed.

Applied to the storage and query paths, a cold Body read first proves presence,
authenticates a bounded material coordinate, and reserves capacity; payload
fetch, decryption, verification, and projection then run off interaction locks.
A migration accepts consent and persists a resumable job before scanning its
first source page. Compaction installs only a semantically equivalent immutable
image after detached bounded work, while operations continue on the pinned old
image. None of these paths may substitute a stale publication to manufacture a
fast response: prompt feedback and exactness are simultaneous requirements.

Acceptance gates:

- acknowledgement/refusal work is bounded independently of Body bytes, project
  link count, historical generation size, and compaction debt;
- slow or deliberately stalled payload I/O, migration, extraction, and
  compaction leave the exact pinned view, cursor, text selection, and unrelated
  operations interactive;
- every `Accepted` operation produces correlated progress and exactly one
  terminal committed-publication or typed-failure observation across restart;
- overload returns a prompt typed refusal before allocation or queue growth,
  never an indefinitely delayed acknowledgement;
- equivalent human and agent operations traverse identical phases and differ
  only in access-path presentation, not durability, attribution, or live-event
  semantics.

## Immutable publication and non-blocking readers

- McKenney et al., *Read-Copy Update* (OLS 2001):
  <https://www.cs.bu.edu/~jappavoo/Resources/Papers/ols2001.pdf>
- Driscoll, Sarnak, Sleator, and Tarjan, *Making Data Structures Persistent*
  (JCSS 1989):
  <https://www.cs.cmu.edu/~sleator/papers/making-data-structures-persistent.pdf>
- Berenson et al., *A Critique of ANSI SQL Isolation Levels* (SIGMOD 1995):
  <https://web.stanford.edu/class/cs345d-01/rl/SQL-isolation-critique.pdf>
- Kung and Robinson, *On Optimistic Methods for Concurrency Control* (TODS
  1981):
  <https://www.eecs.harvard.edu/~htk/publication/1981-tods-kung-robinson.pdf>
- Coffman, Elphick, and Shoshani, *System Deadlocks* (Computing Surveys 1971):
  <https://uobdv.github.io/Design-Verification/Supplementary/System_Deadlocks-Four_necessary_and_sufficient_conditions_for_deadlock.pdf>
- Levandoski, Lomet, and Sengupta, *The Bw-Tree: A B-tree for New Hardware
  Platforms* (ICDE 2013):
  <https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/bw-tree-icde2013-final.pdf>
- Wang et al., *Building a Bw-Tree Takes More Than Just Buzz Words* (SIGMOD
  2018): <https://db.cs.cmu.edu/papers/2018/mod342-wangA.pdf>

The resulting rule is that a reader pins one immutable publication and never
waits for domain validation, extraction, persistence, or compaction. A writer
prepares against a pinned parent, performs the World callback as optimistic
computation, and validates the exact root and materialization only after entering
the serialized mutation lane. It then serializes the durable commit and installs
one new publication atomically. Replica mutation may briefly acquire publication
state, but publication state must never acquire Replica; removing circular wait
is a structural rule, not a convention. Old publications remain valid until
cursor and deferred-read leases release them. Delta chains and retained
generations need hard bounds and reclamation; "lock-free" is not itself a
performance argument.

Acceptance gates:

- a blocked World callback and a blocked extractor do not stall Find or live
  reads of the prior exact publication;
- local and remote commits retain one documented lock order and install in
  durable order;
- a stale optimistic callback fails typed at validation and is never silently
  reinterpreted or retried against another publication;
- cursors either resume the exact retained publication or fail typed, never
  fall forward;
- reclamation accounts for every old publication still pinned by a reader.

## Human and agent operation parity

- The ProseMirror guide makes transactions the common state-transition unit for
  typed input, commands, plugins, and collaborative changes, and applies them to
  immutable editor state:
  <https://prosemirror.net/docs/guide/#state.transactions>
  Adopt: humans and agents submit the same canonical operation primitives and
  produce the same attributable observation stream, even when their access
  paths differ. Reject: an agent-only mutation API whose effects bypass the
  live cursor, selection, highlighting, and feedback machinery used by the
  interactive client.
- Automerge Repo's `DocHandle` exposes one change/event surface for local and
  remotely incorporated document changes:
  <https://automerge.org/docs/reference/repositories/>
  Adopt: local UI work, agent work, and peer work converge through one signed
  transaction and event model. Reject: using an Automerge-style document as the
  physical Issue, project, or board shard; a large mutable document would make
  one scalar edit a coarse invalidation, checkpoint, and extraction unit.
- GitHub Projects' GraphQL surface combines cursor-paged item connections,
  explicit position ordering, mutation identifiers, and a dedicated item-
  position mutation:
  <https://docs.github.com/en/graphql/reference/objects#projectv2itemconnection>
  <https://docs.github.com/en/graphql/reference/input-objects#updateprojectv2itempositioninput>
  Adopt: deep board reads use stable bounded cursors, and every mutation carries
  a canonical client/operation identity that can be replayed and attributed.
  Reject: treating the board response or ordered item collection as one
  monolithic durable store; order, containment, and records remain separately
  addressable facts with compact indexed projections.
- PostgreSQL's index-only scan documentation explains that an index tuple alone
  cannot establish MVCC visibility; its visibility map allows the engine to
  decide whether a heap visit is required before returning the row:
  <https://www.postgresql.org/docs/current/indexes-index-only-scans.html>
  Adopt by analogy: authorization visibility is an index partition selected
  before traversal, metering, ranking, counting, or packing, so denied material
  cannot affect observable work or continuation. Reject: scan a shared posting,
  charge or rank every entry, and filter denied rows afterward.

Acceptance gates:

- equivalent human, agent, and remote operations produce the same durable
  transaction attribution and live observations;
- cursor pages contain no duplicate or skipped item across equal-position runs,
  and a replayed operation identity cannot apply twice;
- one board move or Issue scalar edit touches bounded record and index segments,
  not a project-sized collaborative document;
- corpora that differ only in denied rows produce identical answers, cursors,
  counts, metered usage, refusal, and bounded timing class.

## Packed corpus, overlays, and compaction

- O'Neil et al., *The Log-Structured Merge-Tree (LSM-Tree)* (Acta Informatica
  1996): <https://doi.org/10.1007/s002360050048>
- Balmau et al., *SILK: Preventing Latency Spikes in Log-Structured Merge
  Key-Value Stores* (USENIX ATC 2019):
  <https://www.usenix.org/conference/atc19/presentation/balmau>
- Yao et al., *MatrixKV: Reducing Write Stalls and Write Amplification in
  LSM-tree Based KV Stores* (USENIX ATC 2020):
  <https://www.usenix.org/conference/atc20/presentation/yao>
- Quinlan and Dorward, *Venti: A New Approach to Archival Storage* (FAST 2002):
  <https://www.usenix.org/conference/fast-02/venti-new-approach-archival-data-storage>
- Goodrich, Tamassia, and Schwerin, *Implementation of an Authenticated
  Dictionary with Skip Lists and Commutative Hashing* (DISCEX 2001):
  <https://cs.brown.edu/cgc/stms/papers/discex2001.pdf>
- Rhea et al., *On the Feasibility of Data-Level Path Redundancy* (USENIX 2008):
  <https://www.usenix.org/legacy/events/usenix08/tech/full_papers/rhea/rhea.pdf>
- Vigna, *Quasi-Succinct Indices* (WSDM 2013):
  <https://arxiv.org/abs/1206.4300>
- Boncz, Zukowski, and Nes, *MonetDB/X100: Hyper-Pipelining Query Execution*
  (CIDR 2005):
  <https://cs.brown.edu/courses/cs227/archives/2008/Papers/ColumnStores/MonetDB.pdf>

The corpus therefore uses immutable Body-range segments with compact local
dictionaries and primitive postings, plus a bounded changed-Body overlay. A
one-Body publication writes one new logical segment and one tiny manifest; it
does not rewrite a publication-sized image. Read amplification, tombstones, and
physical retained bytes are bounded independently of logical live rows.
Compaction is range-local, governor-reserved for old-plus-new peak memory, and
scheduled away from foreground work. It never changes semantic publication
coordinates, and old segment Arcs remain valid for already-issued cursors.
Segment names commit to canonical logical content rather than file position:
fixed-position chunking loses deduplication when preceding content shifts.
Monotone NodeIx postings are gap/bit packed, and NodeKey/Value objects are
materialized only for rows that survive traversal, gates, and pagination.

Acceptance gates:

- representative 100k and 1m Issues-v4 mixes fit the Station envelope with the
  Replica snapshot and corpus alive together;
- one changed Body writes material proportional to its segment, not the corpus;
- inserting a Body before an unchanged logical range preserves that range's
  segment identity and encrypted cache bytes;
- many hot-Body publications keep page, exact-count, and point-lookup work
  bounded before and during compaction;
- governor accounting uses physical live allocations, including stale segments
  and compaction peak, rather than only logical row counts;
- a corrupt, missing-key, truncated, or hostile cache image is quarantined and
  rebuilt without becoming replicated truth or World unavailability.

## Collaborative ordering and board ranks

- Dietz and Sleator, *Two Algorithms for Maintaining Order in a List* (1988):
  <https://www.cs.cmu.edu/~sleator/papers/maintaining-order.pdf>
- Dietz, Seiferas, and Zhang, *A Tight Lower Bound for Online Monotonic List
  Labeling* (SIAM J. Discrete Math. 2005):
  <https://doi.org/10.1137/S0895480100315808>
- Bender et al., *Two Simplified Algorithms for Maintaining Order in a List*
  (ESA 2002):
  <https://people.csail.mit.edu/edemaine/papers/DietzSleator_ESA2002/paper.pdf>
- Nédelec et al., *LSEQ: An Adaptive Structure for Sequences in Distributed
  Collaborative Editing* (DocEng 2013):
  <https://doi.org/10.1145/2494266.2494278>
- Weidner and Kleppmann, *The Art of the Fugue: Minimizing Interleaving in
  Collaborative Text Editing* (IEEE TPDS 2025):
  <https://arxiv.org/abs/2305.00583>

A flat fixed-width fractional rank cannot promise both bounded identifiers and
bounded relabeling under adversarial insertions. Board order must use explicit
indirection or variable path labels, with deamortized bounded maintenance that
is separate from the semantic move. Maintenance applies only to the exact
transition head it names; a concurrent move makes stale maintenance inert.
Normal updates advance a bounded maintenance phase. A move at a dense seam may
atomically relabel a bounded exact-head window with the move, but label density
alone may not reject the user's action; a genuine concurrent-head mismatch may
still return Conflict. Identifier growth, relabel work, and conflict heads are
visible and metered rather than hidden behind floats or an unbounded whole-lane
rewrite.

Acceptance gates:

- repeated insert-between at one location cannot make one user move rewrite a
  lane, silently lose order, or fail solely because the adjacent labels are
  dense;
- simultaneous moves converge or surface an explicit inert conflict;
- concurrent maintenance cannot combine a lane from one transition with a rank
  from another;
- rank size and maintenance work have enforced bounds and adversarial tests.

## Online World migration

- Rae et al., *Online, Asynchronous Schema Change in F1* (PVLDB 2013):
  <https://research.google/pubs/online-asynchronous-schema-change-in-f1/>
- Bhattacherjee et al., *BullFrog: Online Schema Evolution via Lazy Evaluation*
  (SIGMOD 2021):
  <https://www.cs.umd.edu/~mwh/papers/bhattacherjee21bullfrog.html>

Accepting a World update in the launcher is migration consent. The launcher
persists one crash-resumable update job, activates the exact migrator package,
runs deterministic idempotent batches, audits completion, prebuilds the target
publication, and only then activates the preferred package. Normal work remains
available through the migrator. A failure leaves the migrator active and never
reinterprets old facts with the preferred implementation. Migration markers are
evidence and resumption coordinates, not a second launcher or a user-facing
tracker workflow.

A representation migration does not counterfeit semantic continuity. The
prior transaction format commits to a protected whole-Body export, whereas the
current format commits to a Fabric causal closure. Re-encoding that declaration
would invalidate the original author's signature and authorization receipt.
The old store is therefore a bounded, validated, read-only migration source.
After update consent, the launcher-owned migration capability streams those
facts into a fresh generation through ordinary current transaction validation,
then verifies semantic Body and receipt evidence before the generation can be
activated. The half-built target is never hosted or synchronized, and the
normal current store carries no legacy-head variant.

Acceptance gates:

- interruption before and after every batch/verification/activation boundary
  resumes without duplicate Bodies or false completion;
- old and preferred interpretations are never selected ambiguously for one
  publication;
- failed verification cannot activate the preferred package;
- update refusal performs no migration, and update consent is attributable;
- a nonempty prior store cannot be converted by rewriting old signed
  descriptors, and no target generation is visible before every translated
  current transaction and semantic-equivalence check has committed.

## Visible conflicts and resumable workflows

- Preguiça, Baquero, and Shapiro, *Conflict-free Replicated Data Types
  (CRDTs)* (2018): <https://arxiv.org/abs/1805.06358>
- Zhuang et al., *ExoFlow: A Universal Workflow System for Exactly-Once DAGs*
  (OSDI 2023): <https://www.usenix.org/system/files/osdi23-zhuang.pdf>
- Zhang et al., *Fault-tolerant and Transactional Stateful Serverless
  Workflows* (Beldi, OSDI 2020): <https://arxiv.org/abs/2010.06706>

An Issue transition behaves like a multi-value register: causally superseded
heads disappear, but maximal concurrent heads remain visible. The board emits a
placement only for one self-authenticating head; it does not use LWW to erase a
concurrent human or agent decision. A ChangeSet or long Exec run separates its
durable deterministic plan and recovery coordinates from replaceable workers.
Operation and output identities derive from the plan plus ordinal, and every
batch commits its cursor and effects together before downstream work observes
them.

Acceptance gates:

- concurrent transition heads converge byte-for-byte, remain visible, and are
  inert in single-placement views until an explicit successor resolves them;
- a forged head/core/digest combination contributes no board fact;
- interruption after every workflow batch resumes without duplicate effects;
- two workers racing the same plan ordinal produce one idempotent durable
  result, and nondeterministic external output is never replayed as if it were
  deterministic.

## Exact dynamic graph analysis

- King and Sagert, *A Fully Dynamic Algorithm for Maintaining the Transitive
  Closure* (JCSS 2002): <https://doi.org/10.1006/jcss.2002.1883>
- Yu, *Cell-probe Lower Bounds for Dynamic Problems via a New Communication
  Model* (STOC 2016): <https://www.cs.princeton.edu/~hy2/files/dynintun.pdf>

Constant-time exact reachability requires quadratic materialization and costly
updates, while dynamic SCC results exhibit an update/query lower-bound tradeoff.
Lait therefore publishes exact adjacency and searchable facts synchronously but
builds global SCC, closure, reduction, layer, and slack Geometry as a separately
named artifact. An artifact exposes its exact source publication and readiness;
it never blocks fact publication or masquerades as current while stale.

Acceptance gates:

- one edge that merges a long chain into one SCC may consume the declared
  artifact budget without delaying the fact publication;
- a budget refusal happens before global work and is typed for the exact
  artifact key;
- Ready, Pending, Unavailable, and Expired never substitute another source
  publication;
- bounded pages meter all visited facts and transient projection memory.

## Admission and memory accounting

- Mehta and DeWitt, *Dynamic Memory Allocation for Multiple-Query Workloads*
  (VLDB 1993): <https://www.vldb.org/conf/1993/P354.PDF>
- Banga, Druschel, and Mogul, *Resource Containers: A New Facility for Resource
  Management in Server Systems* (OSDI 1999):
  <https://www.usenix.org/legacy/events/osdi99/full_papers/banga/banga.pdf>
- Gray and Cheriton, *Leases: An Efficient Fault-Tolerant Mechanism for
  Distributed File Cache Consistency* (SOSP 1989):
  <https://www.cs.cmu.edu/afs/cs.cmu.edu/academic/class/15712-s12/www/papers/gray89.pdf>
- Welsh, Culler, and Brewer, *SEDA: An Architecture for Well-Conditioned,
  Scalable Internet Services* (SOSP 2001):
  <https://doi.org/10.1145/502034.502057>
- Leis et al., *LeanStore: In-Memory Data Management Beyond Main Memory* (ICDE
  2018): <https://db.in.tum.de/~leis/papers/leanstore.pdf>
- Gray and Graefe, *The Five-Minute Rule Ten Years Later, and Other Computer
  Storage Rules of Thumb* (SIGMOD Record 1997):
  <https://arxiv.org/abs/cs/9809005>

Static per-operation quotas are insufficient when concurrent publications,
historical reconstruction, Geometry, cache decoding, and compaction share one
process. Lait reserves conservative source-shape estimates before allocation,
grows a reservation before additional work, converts transient bytes to exact
resident ownership only when the immutable artifact installs, and releases on
the last Arc/lease. Resource ownership follows the process and Station that pay
for the work, rather than whichever World package happened to request it. Cursor
and deferred pins are bounded server-owned reclamation leases, not correctness
authority. Analytical workers use bounded queues and typed admission failure; an
unbounded queue is merely unaccounted memory and delayed feedback. A cold
historical generation is priced from authenticated target-generation metadata,
never from the current snapshot.

The exact Body directory, bindings, causal coordinates, and Corpus indexes stay
resident, but immutable payload bytes do not automatically deserve permanent
DRAM residency. A ReadSnapshot may retain an authenticated material reference
and inflate an Atomic or immutable Body only when an admitted read, projection,
or edit names it. A bounded governor-owned hot cache keeps the working set fast.
The residency check must stay cheaper than a general synchronized lookup on the
hot path, and eviction may never discard the coordinates needed to reconstruct
the same exact Body. A verified warm Corpus cache can therefore reopen without
inflating every Body; a cold rebuild streams Bodies and releases each payload
after extraction.

That boundary begins in the Journal rather than in Replica. Recovery eagerly
authenticates transaction, metadata, and index roots, but an authenticated
deferred-object index commits each causal payload's digest, stored length,
and verification class without reading the payload itself; Replica's signed
material coordinate remains responsible for the semantic key epoch. The object
is fsynced before the index entry can commit and is hash/length verified on its
first admitted read. A missing or corrupt deferred payload is therefore a typed
Body failure, while a missing eager index is a fatal store failure. Background
collection traces every current, historical, and leased material closure from
the authenticated index; startup does not sweep the payload directory by
opening every object. Recovery authenticates the deferred Merkle root rather
than traversing all its leaves; lookup verifies the one requested path and a
detached scrub performs full structural validation. Before allocating payload
bytes, the Reader compares the file metadata with the authenticated stored
length and the governor reservation, then performs an exactly bounded read.
One publication leases that deferred root, not a rendered vector or map of all
objects beneath it. A new publication therefore acquires one shared root lease
and an individual Body read follows only that Body's signed causal closure;
advancing one Body may not enumerate or repin every other Body's artifacts.

Acceptance gates:

- concurrent Stations share the process envelope and fail before allocation;
- bounded worker queues preserve prompt Pending or Capacity feedback under
  overload, and one World/source cannot occupy every worker;
- warm startup with a verified Corpus cache performs no all-Body payload inflate,
  while cold and warm first-read/first-edit latency have explicit scale gates;
- Journal recovery performs zero deferred-payload reads, while crash points
  before and after object fsync/index commit cannot expose an uncommitted object
  or collect a committed current, historical, or leased one;
- deferred-index recovery work is independent of its entry count, and a hostile
  oversized object is refused before allocating beyond its authenticated bound;
- one changed Body acquires O(1) material-root lease state and does not clone or
  enumerate the publication's complete artifact set;
- Body payload fetch/decrypt never holds the Station publication or Replica
  mutation lock, and concurrent readers pin one exact inflated image;
- hot-cache eviction, corruption, missing keys, and opaque material fail typed
  without changing publication coordinates or falling forward;
- cache decode and historical reconstruction reserve before reading segment or
  causal payload bytes;
- local durability cannot commit when the exact candidate publication cannot be
  retained;
- remote truth still converges, but its exact read head becomes typed Building,
  Capacity, or Unavailable rather than serving stale coordinates.
