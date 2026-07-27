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
LaitDaemon
  ├─ identity-scoped local control endpoint
  ├─ injected WorldPackages
  ├─ OrbitDirectory
  └─ ControlRouter
       ├─ IdentityTransportHubs (keyed by DeviceId)
       │    └─ concrete endpoint / protocol Router / gossip
       │         └─ SpaceTransportView (keyed by SpaceId)
       └─ OrbitOccupancy (keyed by local Orbit)
            ├─ vacant
            └─ StationPlacement
                 ├─ hosting
                 │    ├─ owned in-process
                 │    └─ attached compatibility process
                 └─ sole SpaceBridge for the Orbit
                      └─ Station occupying the Orbit
                           ├─ Mechanics
                           ├─ Replica
                           │    └─ Fabric
                           ├─ Comms -> SpaceTransportView
                           ├─ Neighbor registry and Contact
                           └─ WorldBridgeRegistry
                                └─ WorldBridge
                                     └─ docked Sessions

lait serve
  └─ HTTP/SSE adapter -> LaitDaemon

cwd CLI / pinned MCP
  └─ explicit Orbit/World route -> LaitDaemon
```

An Orbit is one durable local participation in a Space. It persists whether it
is vacant or occupied. Activation acquires that Orbit's exclusive operational
lease and places a Station into it; removing the Station drains its tasks and
releases the lease without turning the Station into an Orbit. The current
Runtime API transfers that lease through `Orbit::activate` and
`Station::go_dormant`; those consuming handles express ownership transfer, not
an ontological state conversion. A SpaceBridge is the sole product-side
entrance to the Station occupying one Orbit. There is never a second live
Station, Replica, or product store for the same local Orbit.

CLI, web, and MCP clients use one local control protocol. They do not open the
store or CRDT engine. An explicit control route addresses the process-level
daemon, a local Orbit plus its expected Space, or a World reached through that
Orbit. The CLI's cwd selects its default Orbit; MCP is pinned to its launch
Orbit; neither inherits catalog-wide visibility.

Trusted cwd and MCP adapters derive a pinned `ClientScope`; the web adapter
applies catalog identity policy. Each constructs an explicit route and opens the
identity-scoped LaitDaemon endpoint. The daemon resolves the Orbit, validates
its repeated Space expectation before activation, places or reuses its Station
host, and dispatches to one terminal owner: lifecycle, Mechanics, Station,
observation, or a WorldBridge. The receiving SpaceBridge independently
validates its Orbit, Space, World, and terminal owner. `OrbitDirectory`
discovers durable bindings; `StationPlacement` records where an active Station
is hosted; neither is a second lifecycle owner. The allowed Orbit set never
rides on the wire as a client-controlled claim.

Bridges are logical boundaries, not mandatory processes. The current deployment
is one identity-scoped LaitDaemon routing to zero or more Station placements and
their in-process SpaceBridges. A SpaceBridge or WorldBridge may move to a worker
process for stronger fault or plugin isolation without changing its route or
client contract. The ControlRouter hosts a vacant Orbit in-process and attaches,
without taking ownership, when a compatible historical per-home daemon already
holds that Orbit. Both placement modes retain the per-home socket as an internal
compatibility adapter, but CLI, MCP, and web requests enter through the one
LaitDaemon endpoint.

Catalog listing remains passive. The web adapter asks LaitDaemon for an
`if_running` status; LaitDaemon may inspect an already-live per-Orbit
compatibility adapter, but it never places a vacant Orbit for that probe.

Transport ownership follows device identity, not Orbit count. One
`IdentityTransportHub` owns the concrete endpoint, protocol Router, reachability
book, and gossip instance for a `DeviceId`. Each active Space gets a scoped
transport view with its own inbound queue and gossip topic. The hub reads a
bounded Contact Hello or presence probe only far enough to select its declared
Space, then replays the exact frame to Runtime; the SpaceBridge still performs
canonical decoding, signature, negotiated-peer, protocol, and Space
verification. Slow openers are bounded and dispatched concurrently.
An attached historical compatibility process retains its legacy endpoint until
that migration placement exits; the identity-hub invariant governs Stations
owned by LaitDaemon.

Two active Stations with the same `(DeviceId, SpaceId)` are rejected even if
they occupy distinct local Orbits: the remote address is the device key and the
opening protocol names only the Space, so no legitimate wire address can choose
between them. Distinct Orbits remain durable and independently selectable, and
the same Space can be active under distinct device identities.

Placement and shutdown are ordered:

```text
request
  -> trusted adapter authorizes ClientScope
  -> identity-scoped LaitDaemon endpoint
  -> OrbitDirectory resolves
  -> validate Orbit + expected Space before activation
  -> OrbitOccupancy single-flights by local Orbit
  -> healthy existing control channel: attach
     absent control channel: acquire daemon lock -> activate -> serve in-process

viewer shutdown
  -> stop HTTP and join its daemon event observer
  -> LaitDaemon and Stations remain active

explicit LaitDaemon shutdown
  -> stop accepting process-control work
  -> close and join daemon client connections
  -> gate ControlRouter against new placements
  -> close and join doorbell observers
  -> signal each owned SpaceBridge
  -> join control connections and Observation pumps
  -> drop WorldBridgeRegistry/Sessions
  -> Contact driver emits dormancy and unregisters its SpaceTransportView
  -> Station::go_dormant
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
fabric     canonical collaborative Body engine and Fabric journal integration
replica    Body transactions, protected material, Manifests, quotas,
           validation, and convergence
comms      transport, streams, discovery, gossip, and presence mechanisms
runtime    Orbit/Station lifecycle, Contacts, Worlds, Sessions, observations
lait       IssuesWorld and product/control adapters
```

Dependencies point inward through these boundaries. Product concepts such as
issues, projects, comments, roles, and workflows belong only to `lait`.
Mechanics does not interpret product roles. Fabric does not know authority,
transport, or product meaning. Comms moves bytes but cannot legitimize them.

Only Fabric names Loro. One collaborative Body maps to one Loro document, but
Loro is an implementation detail behind the generic `Fabric` contract. Replica
is the Body graph authority and is the only layer allowed to turn validated
transactions into Fabric changes.

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

## 4. Replica, Fabric, and durability

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
  -> Fabric applies collaborative changes
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

Fabric supplies generic collaborative primitives: registers, maps, stable-id
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

A WorldBridge is the application-side entrance to one registered World in one
active Space. It owns the reviewed implementation identity and the Sessions
docked for local identities. A WorldBridgeRegistry maps `WorldId` to distinct
bridge objects; a Session can never be reused across Worlds.

The application composition root supplies one compile-time `WorldPackages` set
to LaitDaemon. Each `WorldPackage` keeps a Runtime registration, semantic World
implementation, reviewed implementation identity, and optional local-control
adapter together. The same immutable package set is carried through
ControlRouter placement into every SpaceBridge; daemon routing validates the
addressed World against that injected set and never names IssuesWorld. A
control adapter owns product reference resolution, local id/time minting,
transient retry, and response rendering. This is a compile-time package seam,
not a promise of dynamic library loading or process isolation.

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

The package boundary is not yet the complete repository carve. The shared local
`Request`/`Response` schema, Issues-specific status/inbox/doorbell projections,
role-assignment planning, and tracker formation still live in the root
application. They must move behind a versioned generic World-call envelope and
product-owned lifecycle/projection hooks before the Issues package can become
an acyclic standalone crate or repository.

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

Fabric defines convergence mechanics; IssuesWorld defines issue semantics.

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
Fabric, Replica, Runtime, or Mechanics.

The merged IssuesWorld still stores status in a register and comments in the
Issue Body's event/list representation. Those converge, but they do not yet
implement transition-head conflicts or first-class reply/reaction/edit semantics.
This is a known IssuesWorld conformance gap, not a reason to change Fabric's
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
- Canonical encodings, domains, hashes, bounds, and tie-breaks are protocol.
- Product conflict semantics belong to the World that selects the primitive.
- Historical migration plans are not normative documentation.
