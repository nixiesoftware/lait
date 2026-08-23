# Architecture

This document defines LAIT's current architectural boundaries, ownership model,
and trust relationships. It describes the orbital architecture merged in PR 64.
Historical node, Git-store, document-wrapper, ticket, and flat-grant designs are
not part of the current system.

## 1. LAIT in one view

LAIT is a local-first collaboration substrate with an issue tracker shipped as
its canonical first World. A Space is the cryptographic and replication
boundary. Each device keeps its own durable participation and can activate it
without a central server.

```text
daemon::Daemon
  ├─ identity-scoped local control endpoint
  ├─ injected WorldPackages
  ├─ orbits::Catalog
  └─ orbits::Router
       ├─ IdentityTransportHubs (keyed by DeviceId)
       │    └─ concrete endpoint / protocol Router / gossip
       │         └─ SpaceTransportView (keyed by SpaceId)
       └─ OrbitOccupancy (keyed by local Orbit)
            ├─ vacant
            └─ orbits::Placement
                 ├─ hosting
                 │    ├─ owned local task
                 │    └─ attached compatibility process
                 └─ sole StationHost for the Orbit
                      └─ Station occupying the Orbit
                           ├─ Mechanics
                           ├─ Replica
                           │    └─ Engine
                           ├─ Comms -> SpaceTransportView
                           ├─ Neighbor registry and Contact
                           └─ WorldRouter
                                └─ WorldHost
                                     ├─ docked Sessions
                                     └─ supervised selected World runner

lait (the local app)
  └─ HTTP/SSE head -> daemon::Daemon
       └─ WorldClientRegistry
            ├─ host and Mechanics surfaces
            └─ installed World client packages

lait mcp (pinned agent head)
  └─ WorldClientRegistry
       └─ explicit Orbit/World route -> daemon::Daemon

astrolabe (library / launcher / identity)
  └─ tools/astrolabe core -> host / Space / World planes
       └─ Tauri (apps/astrolabe-web) draws ClientView; never a World
```

An Orbit is one durable local participation in a Space. It persists whether it
is vacant or occupied. Activation acquires that Orbit's exclusive operational
lease and places a Station into it; removing the Station drains its tasks and
releases the lease without turning the Station into an Orbit. The current
Runtime API transfers that lease through `Orbit::activate` and
`Station::vacate`; those consuming handles express ownership transfer, not
an ontological state conversion. A StationHost is the sole product-side
entrance to the Station occupying one Orbit. There is never a second live
Station, Replica, or product store for the same local Orbit.

Web and MCP clients use one local control protocol. They do not open the store
or CRDT engine. An explicit control route addresses the process-level daemon, a
local Orbit plus its expected Space, or a World reached through that Orbit. The
web head is a picker over the whole registered catalog under one identity; MCP
is pinned to its launch Orbit and to one World (`$LAIT_WORLD`, or the sole
World this identity has selected) and inherits no catalog-wide visibility. The World
owns the agent surface (tools, omissions, instructions); `lait mcp` mounts
that surface and does not generate tools from the wire protocol. Astrolabe
authors the editor binding (`LAIT_AGENT`, `LAIT_WORLD`) and never parents
that process; it is not on the tool-call path.

**`lait` is a launcher, not a command surface.** It parses a mode, never a
grammar: `lait daemon` is the identity-scoped host, `lait mcp` is the stdio head
an agent speaks, `lait --version` answers the one question that must be
answerable with nothing running, and bare `lait [--json] [--port N]
[--orbit SEL] [--open] [--home <dir>]` starts the daemon and serves the HTTP head
over it. Anything else is refused. `--orbit` selects one durable local
participation by name, id, or path; without it the head is a picker over every
registered Orbit. `--home` selects which *identity* the head serves — spelled as
the daemon's because it selects the same thing — and is what lets Astrolabe run a
head per supervised device instead of one per machine.
Navigation that used to be typed — listing local participations, listing the
installed World packages, reading the selected identity and Space — is
orientation the head answers (`Request::HostContext`) rather than output a shell
prints. Every operation is a request one of the three modes carries; see
[`SERVE.md`](./SERVE.md) for the planes.

Requests arrive already classified. A head hands the registry a decoded value
whose terminal target is `Daemon`, `Space`, or `World { world }`; Orbit
resolution later completes that target into a wire `ControlRoute` without
reclassifying product intent. Host and Mechanics requests carry the typed
`Request`; installed product requests carry their package-owned opaque
`WorldCall` directly.

`WorldClientRegistry` composes one mount name, a collision-safe MCP prefix, a
web parser, an opaque reply decoder, and a local-operation executor per
installed World. The mount is a bare namespace key — it prefixes the package's
public MCP tool names and names its HTTP route segment. A parsed
`ClientInvocation` carries package-owned access and confirmation metadata for
the complete operation, including caller-local effects. The shell enforces that
metadata and supplies an object-safe `ClientHost` for World calls and generic
Space-authority facilities; it never matches a product host enum. The Issues
package therefore owns the `issues` mount — `POST /api/spaces/{id}/worlds/issues/rpc`
and the `issues_*` MCP tools — and their response codec. Adding a Files World
means registering another package with (for example) a `files` mount and
`files_*` tools; it does not add another branch to the head or the MCP router.
Duplicate Worlds, duplicate mounts, package-local tool names, and collisions
with shell names fail during composition.

The MCP adapter derives a pinned `ClientScope`; the web adapter applies catalog
identity policy. Web Space control and product calls use disjoint endpoints; a
product request names its World/package route before its payload is decoded.
Each adapter constructs an explicit route and opens the
identity-scoped daemon::Daemon endpoint. The daemon resolves the Orbit, validates
its repeated Space expectation before activation, places or reuses its Station
host, and dispatches to one terminal owner: lifecycle, Mechanics, Station,
observation, or a WorldHost. The receiving StationHost independently
validates its Orbit, Space, World, and terminal owner. `orbits::Catalog`
discovers durable bindings; `orbits::Placement` records where an active Station
is hosted; neither is a second lifecycle owner. The allowed Orbit set never
rides on the wire as a client-controlled claim.

Station placement remains a logical boundary: the current deployment is one
identity-scoped `daemon::Daemon` routing to zero or more Station placements and
their local StationHost tasks. World execution is a mandatory process boundary
on process-capable platforms. `orbits::Router` hosts a vacant Orbit locally and
attaches, without taking ownership, when a compatible historical per-home daemon
already holds that Orbit. Both placement modes retain the per-home socket for
Space control and Observation compatibility. An owned StationHost invokes the
selected supervised World runner; an attached placement forwards the same opaque
World call through its socket and that StationHost invokes its selected runner.
Web and MCP requests enter through the one `daemon::Daemon` endpoint.

Catalog listing remains passive. The web adapter asks daemon::Daemon for an
`if_running` status; daemon::Daemon may inspect an already-live per-Orbit
compatibility adapter, but it never places a vacant Orbit for that probe.

Transport ownership follows device identity, not Orbit count. One
`IdentityTransportHub` owns the concrete endpoint, protocol Router, reachability
book, and gossip instance for a `DeviceId`. Each active Space gets a scoped
transport view with its own inbound queue and gossip topic. The hub reads a
bounded Contact Hello or presence probe only far enough to select its declared
Space, then replays the exact frame to Runtime; the StationHost still performs
canonical decoding, signature, negotiated-peer, protocol, and Space
verification. Slow openers are bounded and dispatched concurrently.
The concrete endpoint is daemon-scoped by design: unregistering the last Space
route leaves the identity endpoint warm until explicit daemon::Daemon shutdown.
Dropping it on every last-Station transition would race concurrent placement
and turn Station churn into identity/transport churn.
An attached historical compatibility process retains its legacy endpoint until
that migration placement exits; the identity-hub invariant governs Stations
owned by daemon::Daemon.

Two active Stations with the same `(DeviceId, SpaceId)` are rejected even if
they occupy distinct local Orbits: the remote address is the device key and the
opening protocol names only the Space, so no legitimate wire address can choose
between them. Distinct Orbits remain durable and independently selectable, and
the same Space can be active under distinct device identities.

Placement and shutdown are ordered:

```text
request
  -> trusted adapter authorizes ClientScope
  -> identity-scoped daemon::Daemon endpoint
  -> orbits::Catalog resolves
  -> validate Orbit + expected Space before activation
  -> OrbitOccupancy single-flights by local Orbit
  -> healthy existing control channel: attach
     absent control channel: acquire daemon lock -> activate -> serve in-process

viewer shutdown
  -> stop HTTP and join its daemon event observer
  -> daemon::Daemon and Stations remain active

explicit daemon::Daemon shutdown
  -> stop accepting process-control work
  -> close and join daemon client connections
  -> gate orbits::Router against new placements
  -> close and join doorbell observers
  -> signal each owned StationHost
  -> join control connections and Observation pumps
  -> drop WorldRouter/Sessions
  -> Contact driver emits dormancy and unregisters its SpaceTransportView
  -> Station::vacate
  -> release the Orbit lock
  -> after all owned placements join, gracefully stop each identity Router,
     gossip instance, and concrete endpoint
```

Attached compatibility processes are not stopped by host shutdown. An owned
placement is never task-aborted as a successful shutdown: the joinable runner
must complete dormancy before the Orbit is considered vacant.

## 2. Crate boundaries

```text
mechanics  signed Space authority, actors/devices, scoped policy,
           admission, custody, recovery, and ceremony state
journal    semantics-free immutable-object/manifest durability and recovery
Engine     canonical collaborative Body engine and Engine journal integration
replica    Body transactions, protected material, Manifests, quotas,
           validation, and convergence
comms      transport, streams, discovery, gossip, and presence mechanisms
runtime    Orbit/Station lifecycle, Contacts, Worlds, Sessions, observations
runtime::world::call
           versioned opaque application calls and object-safe World handlers
world-interface
           package mount names, MCP descriptors, web parsers, and namespace
           validation
issues     IssuesWorld schemas, semantic model, product DTOs and identifiers
issues-app Issues application protocol plus its web and MCP client interfaces
lait       launcher, identity-scoped daemon, HTTP head, MCP head,
           host-capability adapters, viewer, and application composition
astrolabe  identity-scoped library client: reach, one model of client
           state, Tauri drawing. Never a World head; authors MCP bindings
```

Dependencies point inward through these boundaries. Product concepts such as
issues, projects, comments, roles, and workflows belong to the independently
packaged `products/issues` and `products/issues-app` crates. The outer `lait`
shell mounts those packages but does not declare their command grammar or MCP
schemas.
Mechanics does not interpret product roles. Engine does not know authority,
transport, or product meaning. Comms moves bytes but cannot legitimize them.

Only Engine names Loro. One collaborative Body has one independent Loro causal
history, but a live `LoroDoc` is a bounded mutation cache rather than the stored
shape of every Body. Cold Bodies are immutable Arc-backed exports plus compact
causal Versions; read generations share those exports and decode a projection
only for an explicitly visited Body. Atomic Bodies use the same Arc sharing.
Loro remains an implementation detail behind the generic `Engine` contract.
Replica is the Body graph authority and is the only layer allowed to turn
validated transactions into Engine changes.

## 3. Mechanics and authority

Mechanics is the sole source of truth for:

- actors and their valid devices;
- Space membership and admission;
- scoped capability assignments and delegation;
- historical authority frontiers and checkpoints;
- active World implementation identities;
- recovery configurations, custody, and explicit threshold ceremonies.

Worlds select a canonical `AuthorizationDemand` for an operation. Mechanics
evaluates it against signed history at the transaction's referenced authority
frontier. A product role is provenance used while expanding assignments; it is
never itself an effective grant.

Authority evaluation is historical. A transaction validly authored before a
later removal remains valid, while a currently authorized actor cannot validate
a transaction from a frontier where it lacked authority.

Ordinary Space authority and ceremony traffic share one crash-safe Mechanics
journal but remain distinct material classes:

- `SpaceAuthority` effects are terminal authority changes and may enter an
  `AuthorityFrontier`.
- `CeremonyMaterial` records sparse recovery, elevation, resharing, and custody
  progress under a separate bounded cursor.

Ceremony packets never enlarge ordinary authority frontiers. FROST is used only
to produce explicit recovery/elevation/reshare authority, never for ordinary
World transactions.

## 4. Replica, Engine, and durability

A Replica owns the protected Body graph for one Space. Its durable root is a
signed Manifest whose entries bind Body identities to their constituent signed
transactions and protected payloads. Concurrent Body heads are preserved; a
Manifest is an authenticated complete view, not a mutable cache index.

Local mutation follows one path:

```text
signed World action
  -> Session pins authority frontier + WorldPublicationId
  -> World returns Body operations + demand
  -> Runtime contains the operations
  -> Mechanics authorizes and produces a bound receipt
  -> Replica prepares causal artifacts + candidate Body image
  -> Runtime builds the candidate corpus and read publication
  -> Replica commits transaction and replacement Manifest
  -> Runtime atomically swaps snapshot + corpus + coordinates
  -> durable acknowledgment
  -> bounded value-free change Observation
```

Every path in that sequence obeys **Constant-Time Feedback Continuity**:

> Every intent produces bounded feedback before work proportional to the
> action begins. The client renders either a deterministic optimistic
> projection or an explicit pending operation, preserves it together with the
> existing exact view throughout processing, and atomically reconciles it with
> the matching terminal publication.

Human and agent access paths use the same signed operation identity,
attribution, phases, bounded progress, and terminal result. `Sending` is a
client-local one-frame transition that precedes network work. `Accepted`
requires a bounded durable operation receipt; `Committed` names the exact
`WorldPublicationId`. Long work runs off UI, reactor, Replica-mutation, and
publication-install locks. Refresh preserves loaded pages, cursor/selection,
scroll position, and optimism rather than blanking or snapping back. A refusal
removes or marks the optimistic projection visibly and carries its typed cause.
Action size may change terminal latency, never time-to-feedback, interactivity,
or visual continuity. This is a governing invariant of every touched path, not
a separate repair pass.

The Manifest rename is the authoritative Body-plane commit point. The journal
protocol reserves a monotonic sequence, stages immutable objects, records
material readiness, atomically replaces the Manifest last, and then performs
cleanup. Recovery exposes either the complete prior state or the complete new
state. It never heuristically repairs partial data.

An acknowledged mutation is durable before it is observed. If the filesystem
cannot determine whether the authoritative rename became durable, the operation
returns `OutcomeUnknown`; the Station must reopen and must not blindly retry.

Preparation is not an optimistic mutation of the live collaborative writer.
Every fallible product condition is checked first; candidate Body material and
its extracted corpus remain unpublished until the durable commit succeeds. An
unexpected Engine or extractor failure discards that candidate while readers
continue to pin the prior publication. There is no fallible derived-work gap
after a successful local commit.

Remote truth follows the same publication rules but cannot be gated by installed
World code: Contact convergence must remain possible for opaque relays. Replica
first validates and durably adopts the exact remote transaction graph. Runtime
then installs either the matching ready World publication or an explicit
`Building`/`Unavailable` read head when the exact implementation, extractor, or
key is absent. It never serves the prior publication under the new root or
implementation identity.

Engine supplies generic collaborative primitives: registers, maps, stable-id
lists, text, add-wins sets, counters, and atomic Bodies. Convergence of a
primitive is not a product conflict policy. A World that chooses a register
accepts its deterministic single-winner semantics. Causally significant product
state should use explicit predecessor/revision structures when concurrent intent
must remain visible.

## 5. Worlds and Sessions

A World is an independently versioned immutable release. `world.json` declares
its identity, presentation, host requirements, launch entries, and native
runners. The signed feed manifest authenticates every target artifact; staging
verifies size, digest, contained paths, declaration identity, compatibility,
artwork, and applicable runner files before atomically selecting a release.

The daemon launches applicable runners as owned child processes through
`world-runner`. Readiness is authenticated over a random-token loopback
channel; frames, callbacks, and request time are bounded. The process describes
its reviewed `WorldImplementationId`, which commits its descriptor, schemas,
policy table, and artifact identity. The host refuses a runner that answers for
another World, release, protocol, or preferred implementation. A crashed child
is restored from the same immutable release before the next call; a call that
may already have reached it is never replayed.

The outer runner protocol is version 1. Its length-delimited, token-bearing
control frames use Postcard and have a 64 MiB hard ceiling. The typed
`world-sdk` payload carried inside those frames is ABI 3 and uses the
JSON-compatible CBOR data model: SDK types normalize through
`serde_json::Value` before CBOR encoding, so tagged/flattened Runtime DTOs and
arbitrary JSON values retain one representation on both sides. Changing either
encoding or meaning requires a protocol/ABI bump; a release manifest declares
compatible host ranges for both.

A World receives only a bounded, Manifest-pinned view and immutable principal
facts. It cannot access storage, Loro, transport, custody secrets, or authority
mutation. It returns declared Body operations and a non-empty authorization
demand. Runtime validates World/schema containment before committing anything.

A WorldHost is the application-side entrance to one registered World in one
active Space. It owns the reviewed implementation identity and the Sessions
docked for local identities. A WorldRouter maps `WorldId` to distinct
host objects; a Session can never be reused across Worlds.

At daemon launch, `world::installed` passively enumerates each selected release,
launches its declared runners, and adapts the runner ABI into one
`WorldPackages`/`WorldClientRegistry` generation. The root package has no
production dependency on a product crate. The same immutable generation is
carried through `orbits::Router` placement into every StationHost; daemon
routing validates the addressed World against that injected set and never
names Issues or Signage.

Native iOS is the deliberate platform exception. Apple does not permit the app
to spawn helper executables or install new native code after signing, so
`astrolabe-ios` links only the reviewed first-party implementations included in
that signed application and adapts them to the same generic package/client
interfaces. This adapter is confined to the iOS crate; it is never a fallback
for a process-capable host and does not make independently downloaded native
World code executable on iOS.

The `runtime::world::call` namespace is the application-call boundary shared by a product
and its host. `WorldCall { world, operation, version, payload }` and its bound
`WorldReply` leave the payload opaque to daemon::Daemon and StationHost. The
runner-owned handler—not the client—decodes the call and classifies it as a query
or command before host policy runs. It owns product reference resolution, local
id/time minting, transient retry, and product response construction.
`world-sdk` carries the same product-neutral semantic, application, client,
display, and Exec ABI over the supervised process boundary. Host capabilities
are explicit callbacks; Worlds do not receive storage, transport, custody keys,
or ambient host access.

For an owned Station placement, orbits::Router invokes the in-process
StationHost directly; StationHost invokes the selected World process through
the ABI. The per-Orbit socket is not part of that World call stack.
If daemon::Daemon attaches to a standalone StationHost, the same opaque
`WorldClientRequest` crosses the socket and the receiving host invokes its
selected handler. Protocol v5 deliberately retired v4 application-call adapters and typed
product requests; protocol v6 removed the last product projection from root
control, so every placement now has the same product-neutral boundary.

Issues' semantic package lives at `products/issues` with no dependency on
the `lait` application crate, local control protocol, daemon, filesystem, or
process lifecycle. Its executable adapter lives at `products/issues-runner`;
product DTOs and identifiers remain under their owning packages. Moving those
packages to another repository changes the release producer, not Runtime or
host ownership.
The outer `world::lifecycle` adapter owns only generic Orbit/Station
materialization and invokes package lifecycle hooks with a docked Session.
`issues-app` supplies the reviewed implementation policy, founder grants,
initial-project policy, and crash-resumable signed `InitializeTracker` record.
`orbital` contains no Issues bootstrap implementation.

World updates cross a deliberate generation boundary. Consent and progress are
durable. The old generation downloads and verifies the new immutable release,
records `relaunching`, drains the daemon and all owned runners, then spawns a
fresh daemon from the selected release set. Only the new World implementation
performs bounded per-Space migration and activation. This prevents a staged
pointer from making old code assess new semantics, and preserves the invariant
that every Runtime Catalog is immutable for its process generation.

The sibling `products/issues-app` package owns the `issues.control` v1 codec,
query/command classification, `IssueRouter` execution adapter, product response
schema, host-capability vocabulary, role-to-authority planning, formation
policy, status/inbox/doorbell projections, the `issues` mount’s web parser, and
all 78 Issues MCP descriptors. It depends on the semantic package and generic
substrate/runtime/client interfaces, never back on `lait`.
Most client operations become `WorldCall`s at parse time. Inbox watermark I/O,
access assignment, attachment filesystem I/O, and implementation activation are
explicit named host-capability calls: their
interface and asynchronous orchestration remain product-owned while the shell
supplies generic World-call and Space-authority facilities that a semantic
World must not hold.

That ownership includes work controls. `issues_verify` is a semantic Issues
command: the application mints one persistent request coordinate, derives the
Run Runtime will assign to command zero, and submits the issue target plus
`Start` in one World effect. The World independently re-derives that Run id,
requires committed repository content and a valid project workflow, and writes
the check link only if Runtime can commit the protected `Started` event in the
same transaction. `issues_work` is the separate generic lifecycle facade for
inspect, watch, cancel, continue, and resume. Astrolabe may present either
surface, but neither contract is owned by Astrolabe or requires its harness.
Continue commits a fresh visible Attempt by deriving a bounded `Try` from a
completed Attempt's durable Offer (when one exists), enforcement, limit, and
fence evidence. Resume additionally requires the exact committed checkpoint
and a Spec whose resume contract is `Checkpoint`; the current Issues
verification Spec is `Restart`, so its supported next action is continue. A
Started-only Run waits for this Station's exclusive drain. The drain cites
live Offer news and a Ready when both exist; otherwise it commits a
Station-only first `Try` — no Offer, no derived OfferId, no enforcement
artifact. A prior-epoch `Leased` that never began is failed `Unknown` so
another Attempt can proceed. The application control seam never invents
scheduling coordinates from issue state. Signed Offer news is a standalone
envelope (`exec::Offer`) that a `Try` may cite as evidence; it is not a
reserved Body, not a reservation, and not a ranking of Stations.
`Session::announce` holds that news on the Station activation and evaluates
the claimed Specs' offer demand. First-use Tries that cite an Offer must
find live news and a nonce-bound Ready; the Ready is consumed only after
`Leased` is durable. Continue copies historical Offer coordinates: live
news still authorizes the offer demand but does not require a new Ready;
expired news still pays the offer demand. `Session::publish_build` writes a
signed Build envelope into the reserved Build Body. That is identity, not a
dispatch gate and not a ranking of Builds.
Inspect/watch expose failure class and returned output ContentRefs so an
attached actor can continue, cancel, or `issues_accept_check` from those
facts without reading output bytes.
Returned verification evidence becomes issue truth only through
`issues_accept_check`, which validates Runtime's typed Outcome and atomically
records the report, verdict, optional Done transition, history, and `Accepted`
event.

Role assignment is split at that boundary: Issues resolves `(role, project)`
into a package-owned `AccessPlan`; root control can only commit generic
`(World, capability, resource)` Mechanics assignments. Reviewed implementation
activation likewise carries an explicit World id. Neither root verb names an
Issues role, project, or singleton first-party product.

The client package also owns reply decoding and may compile a friendly
product-level read into Runtime's typed Find DAG. `ClientInvocation::Find` still
enters the host's authenticated Session; the package cannot supply principal,
authority, or bypass gates. The head and MCP adapter carry the decoded product
value out as JSON and apply typed failure semantics; they do not inspect it.
Product response variants, boards, issue detail, inbox wording, and JSON shape
do not appear in root control.

A Session binds a local identity to one World at an active Station. Queries and
mutations are authorized independently. It pins one immutable
`WorldPublication` containing the Replica snapshot, shared extracted corpus,
and complete Manifest/implementation/extractor/materialization coordinates.
Find applies request-specific gates to that principal-neutral corpus before any
traversal, scoring, count, or packing. Viewer, product adapter, control, Exec,
and MCP are access paths to this same Session primitive, not independent data
sources. Cache entries and analytical artifacts bind the complete publication;
they are disposable and cannot become replicated truth.

Remote adoption never invokes World code. Replica verifies transaction
structure, protected payload commitments, historical Mechanics receipts,
parent-Manifest availability, quotas, and the authority-approved implementation
identity. Nodes without a supported World or schema may retain and forward
legitimate protected material opaquely.

IssuesWorld (`com.lait.issues`) is the first-party reference World. On
process-capable hosts it has no private architectural path unavailable to
another conforming selected release. Its signed-iOS inclusion is the explicit
first-party platform exception described above.

Root control contains no issue command or response variants. The viewer and MCP
construct the Issues-owned application protocol and carry it in an opaque
`WorldCall`; attached StationHosts receive that same envelope. The web
viewer addresses `/worlds/issues/rpc`, so malformed product input cannot fall
through into root control decoding. Local host capabilities are decoded and
executed by the product package and may compose a World call with a filesystem,
caller-local state, or generic Space-authority facility. For example, inbox reads pass the caller-local watermark into an
Issues query and advance it only after a successful reply; the complete
operation is therefore a command when `clear` is true. Protocol v6 makes the
daemon boundary explicit on the wire.

## 6. Communication model

The communication layers have deliberately different semantics:

```text
Coordinates         signed bootstrap locator and optional admission capability
Beacon              signed, lossy news about reachability/change
Neighbor presence   authenticated directed liveness
Contact              bounded direct transfer transcript
Convergence          validation and durable incorporation
```

Gossip and presence improve discovery and convergence latency; they do not
confer membership or authority. Any peer may announce only what its signed
identity permits another node to verify independently.

Contact advertises a complete signed Manifest while transferring only material
the initiator does not declare as held. The declaration is signed and bounded;
a false declaration can starve only its claimant because adoption still
requires complete-root validation. Contact framing receipts are not convergence
receipts, and received bytes remain inert until Mechanics and Replica validate
them.

Coordinates may provide direct iroh routes for the initial Contact. Relay and
discovery configuration is guarded local deployment policy and is never
accepted from an invite. Accepting valid Coordinates is the user approval for
admission; redemption remains a verified Mechanics authority transition.

## 7. IssuesWorld conflict ownership

Engine defines convergence mechanics; IssuesWorld defines issue semantics.

- Scalar fields may deliberately use deterministic register semantics where a
  single winner is acceptable.
- Workflow status is causally significant. The canonical correction represents it by
  predecessor-bound transition records; concurrent heads are a typed conflict
  resolved by an authorized successor rather than silently delegated to LWW.
- Comments that support replies, reactions, edits, or moderation are first-class
  record Bodies. Replies bind an immutable parent comment id, reactions are
  LWW register Bodies keyed by the exact `(issue, comment, emoji, actor)` tuple,
  and editable text uses revision heads.
- Durable semantic events are immutable records used for history and inbox
  projection; engine oplogs are never a product history API.

These are product-schema choices. They do not add issue-specific types to
Engine, Replica, Runtime, or Mechanics. V4 applies the same record-addressed
rule to project topology, schedule, triage, updates, memberships, and immutable
Spec/Baseline revisions; the shared corpus is their directory and query surface,
not another source of truth.

## 8. Security posture

LAIT separates possession, convergence, and legitimacy:

- Comms proves reachability and transports bounded bytes.
- Protected Bodies provide confidentiality and content binding.
- Replica proves structural completeness and durable graph membership.
- Mechanics proves historical authority.
- A pinned World implementation chooses product meaning and sufficient demand.

Trusted native World code is not sandboxed or remotely attested. Cryptographic
authorization cannot prevent a reviewed-but-malicious World implementation from
selecting an insufficient demand; authority activation of the implementation id
is therefore a trust decision.

Body encryption, custody secrets, and device private keys are local secret
material. Lazy revocation cannot erase plaintext or keys already copied by a
removed device. Detailed claims and non-goals live in `THREAT-MODEL.md`.

The HTTP head adds one boundary that is not an authority question at all.
A Station whose catalog binding carries its own identity directory signs with
*that* seed — an agent's Orbit is the case that exists today — so a write routed
into it through a head serving somebody else's token would go out over the
agent's signature. Mechanics would authorize it, correctly: it evaluates the
*signer's* grants, and a sponsored agent legitimately holds write standing. The
head therefore refuses that write before it is signed, on custody grounds rather
than on standing. `Catalog::signs_with_own_seed` is the single spelling of the
question; `serve::borrowed_key_refusal` and `orbits::bootstrap::admit` are its
two enforcement points. Reads are never refused — observing a hosted identity's
board signs nothing. See `SERVE.md`.

## 9. Evolution rules

- Rust concepts use semantic names; versions live in encoded envelopes,
  domains, ALPNs, schema metadata, and store markers.
- Unknown signed, wire, or store versions fail closed.
- Backward compatibility is explicit policy, never an accidental fallback.
- Local representation changes build immutable Mechanics + Replica generations,
  verify logical equivalence, and activate both through one atomic Orbit pointer.
  They do not change World meaning or author Space authority.
- Canonical encodings, domains, hashes, bounds, and tie-breaks are protocol.
- Product conflict semantics belong to the World that selects the primitive.
- Historical migration plans are not normative documentation.
