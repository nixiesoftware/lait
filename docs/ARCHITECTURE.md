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
                 │    ├─ owned in-process
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
                                     └─ docked Sessions

lait serve
  └─ HTTP/SSE adapter -> daemon::Daemon

cwd CLI / pinned MCP
  └─ WorldClientRegistry
       ├─ shell/Mechanics surfaces
       └─ installed World client packages
            └─ explicit Orbit/World route -> daemon::Daemon
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

CLI, web, and MCP clients use one local control protocol. They do not open the
store or CRDT engine. An explicit control route addresses the process-level
daemon, a local Orbit plus its expected Space, or a World reached through that
Orbit. The CLI's cwd selects its default Orbit; MCP is pinned to its launch
Orbit; neither inherits catalog-wide visibility.

The `lait` CLI is the navigation shell for this graph, not the command-line
identity of the bundled Issues World. Bare `lait` reports the selected identity,
Orbit, Space, and installed Worlds. `--orbit` selects one durable local
participation; `lait orbits` lists those local participations, while
`lait worlds` lists the semantic packages installed in the application
composition. Product commands live only below their World namespace, such as
`lait issues ...`.

Command parsing produces a `ClientAction` whose terminal target is already
`Daemon`, `Space`, or `World { world }`. Orbit resolution later completes that
target into a wire `ControlRoute`; it does not reclassify product intent. The
shell commands still carry the historical typed `Request`; installed product
commands emit their package-owned opaque `WorldCall` directly. The typed
product variants remain only for viewer, host-capability, and v3 daemon
compatibility adapters.

`WorldClientRegistry` composes one root CLI mount, collision-safe MCP prefix,
explicit web adapter, opaque reply decoder, and local-operation executor per
installed World. A parsed `ClientInvocation` carries package-owned access and
confirmation metadata for the complete operation, including caller-local
effects. The shell enforces that metadata and supplies an object-safe
`ClientHost` for World calls and generic Space-authority facilities; it never
matches a product host enum. The Issues package therefore owns
`lait issues ...`, the `issues_*` MCP tools, and their response codec. Adding a
Files World means registering another package with (for example) a `files`
mount and `files_*` tools; it does not add another branch to the root CLI or MCP
router. Duplicate Worlds, duplicate mounts, package-local tool names, and
collisions with shell names fail during composition.

Trusted cwd and MCP adapters derive a pinned `ClientScope`; the web adapter
applies catalog identity policy. Web Space control and product calls use
disjoint endpoints; a product request names its World/package route before its
payload is decoded. Each adapter constructs an explicit route and opens the
identity-scoped daemon::Daemon endpoint. The daemon resolves the Orbit, validates
its repeated Space expectation before activation, places or reuses its Station
host, and dispatches to one terminal owner: lifecycle, Mechanics, Station,
observation, or a WorldHost. The receiving StationHost independently
validates its Orbit, Space, World, and terminal owner. `orbits::Catalog`
discovers durable bindings; `orbits::Placement` records where an active Station
is hosted; neither is a second lifecycle owner. The allowed Orbit set never
rides on the wire as a client-controlled claim.

Service boundaries are logical boundaries, not mandatory processes. The current deployment
is one identity-scoped daemon::Daemon routing to zero or more Station placements and
their in-process StationHosts. A StationHost or WorldHost may move to a worker
process for stronger fault or plugin isolation without changing its route or
client contract. The orbits::Router hosts a vacant Orbit in-process and attaches,
without taking ownership, when a compatible historical per-home daemon already
holds that Orbit. Both placement modes retain the per-home socket for Space
control and Observation compatibility, but owned World calls dispatch directly
to the in-process World host. An attached placement forwards the same opaque World
call through that socket without translating its payload. CLI, MCP, and web
requests enter through the one daemon::Daemon endpoint.

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
           composable CLI mounts, MCP descriptors, and namespace validation
issues     IssuesWorld schemas, semantic model, product DTOs and identifiers
issues-app Issues application protocol plus CLI and MCP client interfaces
lait       orbital navigation shell, host-capability adapters, viewer, and
           application composition
```

Dependencies point inward through these boundaries. Product concepts such as
issues, projects, comments, roles, and workflows belong to the independently
packaged `products/issues` and `products/issues-app` crates. The outer `lait`
shell mounts those packages but does not declare their command grammar or MCP
schemas.
Mechanics does not interpret product roles. Engine does not know authority,
transport, or product meaning. Comms moves bytes but cannot legitimize them.

Only Engine names Loro. One collaborative Body maps to one Loro document, but
Loro is an implementation detail behind the generic `Engine` contract. Replica
is the Body graph authority and is the only layer allowed to turn validated
transactions into Engine changes.

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
  -> Session pins authority frontier + Manifest root
  -> World returns Body operations + demand
  -> Runtime contains the operations
  -> Mechanics authorizes and produces a bound receipt
  -> Replica commits transaction and replacement Manifest
  -> Engine applies collaborative changes
  -> durable acknowledgment
  -> Observation publication
```

The Manifest rename is the authoritative Body-plane commit point. The journal
protocol reserves a monotonic sequence, stages immutable objects, records
material readiness, atomically replaces the Manifest last, and then performs
cleanup. Recovery exposes either the complete prior state or the complete new
state. It never heuristically repairs partial data.

An acknowledged mutation is durable before it is observed. If the filesystem
cannot determine whether the authoritative rename became durable, the operation
returns `OutcomeUnknown`; the Station must reopen and must not blindly retry.

Engine supplies generic collaborative primitives: registers, maps, stable-id
lists, text, add-wins sets, counters, and atomic Bodies. Convergence of a
primitive is not a product conflict policy. A World that chooses a register
accepts its deterministic single-winner semantics. Causally significant product
state should use explicit predecessor/revision structures when concurrent intent
must remain visible.

## 5. Worlds and Sessions

A World is trusted in-process semantic code registered under an
authority-approved `WorldImplementationId`. The id commits its descriptor,
schemas, policy table, and artifact identity. Runtime verifies that exact
implementation is active before any World callback or projection.

A World receives only a bounded, Manifest-pinned view and immutable principal
facts. It cannot access storage, Loro, transport, custody secrets, or authority
mutation. It returns declared Body operations and a non-empty authorization
demand. Runtime validates World/schema containment before committing anything.

A WorldHost is the application-side entrance to one registered World in one
active Space. It owns the reviewed implementation identity and the Sessions
docked for local identities. A WorldRouter maps `WorldId` to distinct
host objects; a Session can never be reused across Worlds.

The application composition root supplies one compile-time `WorldPackages` set
to daemon::Daemon. Each `WorldPackage` keeps a Runtime registration, semantic World
implementation, reviewed implementation identity, and optional
`WorldCallHandler` together. The same immutable package set is carried through
orbits::Router placement into every StationHost; daemon routing validates the
addressed World against that injected set and never names IssuesWorld.

The `runtime::world::call` namespace is the application-call boundary shared by a product
and its host. `WorldCall { world, operation, version, payload }` and its bound
`WorldReply` leave the payload opaque to daemon::Daemon and StationHost. The
registered handler—not the client—decodes the call and classifies it as a query
or command before host policy runs. It owns product reference resolution, local
id/time minting, transient retry, and product response construction. This is a
compile-time package seam, not a promise of dynamic library loading or process
isolation.

For an owned Station placement, orbits::Router invokes the in-process
StationHost directly. The per-Orbit socket is not part of that World call stack.
If daemon::Daemon attaches to a standalone StationHost, the same opaque
`WorldClientRequest` crosses the socket and the receiving host invokes its
registered handler. Protocol v5 deliberately retired v4 application-call adapters and typed
product requests; protocol v6 removed the last product projection from root
control, so every placement now has the same product-neutral boundary.

IssuesWorld's semantic package lives at `products/issues` with no dependency on
the `lait` application crate, local control protocol, daemon, filesystem, or
process lifecycle. The root composes `lait::world`; product DTOs and identifiers
remain under their owning `issues` package. Moving that package to another
repository changes the dependency locator, not Runtime or host ownership.
The outer `world::lifecycle` adapter owns only generic Orbit/Station
materialization and invokes package lifecycle hooks with a docked Session.
`issues-app` supplies the reviewed implementation policy, founder grants,
initial-project policy, and crash-resumable signed `InitializeTracker` record.
`orbital` contains no Issues bootstrap implementation.

The sibling `products/issues-app` package owns the `issues.control` v1 codec,
query/command classification, `IssueRouter` execution adapter, product response
schema, host-capability vocabulary, role-to-authority planning, formation
policy, status/inbox/doorbell projections, `lait issues` command tree, and all
38 Issues MCP descriptors. It depends on the semantic package and generic
substrate/runtime/client interfaces, never back on `lait`.
Most client operations become `WorldCall`s at parse time. Inbox watermark I/O,
access assignment, git work-state behavior, attachment filesystem I/O, and
implementation activation are explicit named host-capability calls: their
interface and asynchronous orchestration remain product-owned while the shell
supplies generic World-call and Space-authority facilities that a semantic
World must not hold.

Role assignment is split at that boundary: Issues resolves `(role, project)`
into a package-owned `AccessPlan`; root control can only commit generic
`(World, capability, resource)` Mechanics assignments. Reviewed implementation
activation likewise carries an explicit World id. Neither root verb names an
Issues role, project, or singleton bundled product.

The client package also owns reply decoding and presentation. CLI and MCP pass
the decoded product value to its presenter; the shell only writes the returned
stdout/stderr and applies typed failure semantics. Product response variants,
tables, boards, issue detail, inbox wording, ANSI styling, and JSON shape do not
appear in root control or its renderer.

A Session binds a local identity to one World at an active Station. Queries and
mutations are authorized independently. Query results are computed from one
Manifest root and authority frontier; a derived cache must be keyed by that
complete root. Cache entries are disposable and cannot become replicated truth.

Remote adoption never invokes World code. Replica verifies transaction
structure, protected payload commitments, historical Mechanics receipts,
parent-Manifest availability, quotas, and the authority-approved implementation
identity. Nodes without a supported World or schema may retain and forward
legitimate protected material opaquely.

IssuesWorld (`com.lait.issues`) is the bundled reference World. It has no private
architectural path unavailable to another conforming World.

Root control contains no issue command or response variants. The viewer, CLI,
and MCP construct the Issues-owned application protocol and carry it in an
opaque `WorldCall`; attached StationHosts receive that same envelope. The web
viewer addresses `/worlds/issues/rpc`, so malformed product input cannot fall
through into root control decoding. Local host capabilities are decoded and
executed by the product package and may compose a World call with a
working-tree, filesystem, caller-local state, or generic Space-authority
facility. For example, inbox reads pass the caller-local watermark into an
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
  Bodies. Replies bind an immutable parent comment id, reactions are actor-keyed
  add-wins membership, and editable text uses revision heads.
- Durable semantic events are immutable records used for history and inbox
  projection; engine oplogs are never a product history API.

These are product-schema choices. They do not add issue-specific types to
Engine, Replica, Runtime, or Mechanics.

The merged IssuesWorld still stores status in a register and comments in the
Issue Body's event/list representation. Those converge, but they do not yet
implement transition-head conflicts or first-class reply/reaction/edit semantics.
This is a known IssuesWorld conformance gap, not a reason to change Engine's
baseline algebra.

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
