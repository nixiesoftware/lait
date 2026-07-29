# Protocol contract

This document defines LAIT's interoperability boundaries: signed bootstrap and
discovery packets, direct Contact, replicated authority and Body objects, and
the local daemon control channel. Rust type names are not wire specifications.

## 1. Identities and trust anchors

- `SpaceId` identifies one cryptographic and replication boundary.
- `ActorId` identifies a member within a Space.
- `DeviceId` identifies an actor device key.
- `StationId` identifies a device's active network endpoint.
- `WorldId` identifies a semantic World.
- `BodyId` is meaningful only with its World and Space.

Display names, petnames, filesystem paths, project keys, network routes, relay
addresses, and Loro peer ids confer no authority.

Space trust is established through signed genesis/Mechanics history. A node
validates authority at the frontier referenced by the received transaction; it
does not substitute current membership.

## 2. Versioning and canonicality

Compatibility is explicit at independent layers:

- store markers and manifests;
- signed packet protocol fields and domains;
- direct-protocol ALPNs;
- Contact frame grammar;
- external JSON DTO/schema versions;
- local daemon handshake versions.

Unknown incompatible versions fail closed. There is no fallback to legacy Space
tickets, legacy stores, old daemon routing, or historical document codecs.

Canonical encodings require exact decoding: no trailing bytes, duplicate map
keys, alternate enum spellings, unsorted set-like sequences, or non-minimal
representations. Identifiers, hashes, signatures, ordering, and bounds must be
reproducible by an independent implementation.

Semantic Rust types remain version-free. Numeric versions belong to encoded
envelopes, constants, domains, schemas, and fixtures.

## 3. Coordinates and admission

`SignedCoordinates` is the bootstrap object. Its payload binds the Space,
approach Station, canonical direct routes, and optionally an admission
capability. Routes are hints authenticated by the signer, not authority to
change the joiner's relay or discovery configuration.

The current Coordinates wire format is version 2. Direct IPv4 and IPv6 routes
are sorted and deduplicated canonically and reject unusable addresses, zero
ports, excess entries, and excess bytes.

Accepting valid Coordinates is the user approval boundary for joining. The
candidate durably records its signed acceptance proof before dialing. Contact
then transports the proof and authority material needed for Mechanics to redeem
the capability. Redemption verifies:

- Space, candidate actor/device, and capability binding;
- issuer authority at issuance;
- validity window and explicit revocation;
- single-use or bounded-reuse policy;
- the exact World assignment evidence and role provenance.

Membership and initial assignments commit atomically. There is no pending-member
approval queue or second approval command.

## 4. Beacon, presence, and gossip

Beacon is signed lossy news. Presence is authenticated directed liveness.
Gossip may disseminate reachability and change hints. None is durable authority
or proof that the sender may author a World mutation.

A freshness tracker accepts only a cryptographically verified Beacon. Replay,
wrong-Space, wrong-sender, expired-lease, malformed-route, and signature
substitution inputs do not update reachability state.

Presence uses distinct probe and acknowledgment messages. A successful exchange
proves the negotiated endpoint controls its key and is reachable at that moment;
Mechanics still determines actor/device standing.

Gossip is an optimization. Correctness cannot require every participant to join
one room, receive every announcement, or be online simultaneously. Direct
Contact plus persistent Neighbor retry state remains sufficient for eventual
exchange among reachable peers.

## 5. Contact handshake

Contact is a bounded direct transcript over the Contact ALPN. The signed Hello
binds at least:

- protocol version;
- Space and Contact id;
- initiator and responder Station identities;
- negotiated transport identity;
- nonce;
- canonical holdings count and digest.

The signed acknowledgment binds the exact Hello and a responder nonce. Both
sides bind the authenticated transport peer to the signed Station identity
before accepting transfer material.

A local process may share one concrete transport endpoint across several
Spaces. Its identity hub uses the bounded opening Hello's declared Space only
to select a Space-scoped inbound queue and replays the exact bytes unchanged.
The receiving Contact state machine still performs canonical decoding and all
signature, peer, protocol, responder, and Space checks; local demultiplexing is
not authority and changes no wire bytes. Presence probes follow the same rule.

The Contact protocol field is currently version 2. The ALPN and individual
domain strings have their own versioning and must not be inferred from that
field. A clean format break updates the affected bytes and fixtures atomically.

## 6. Holdings declarations and delta transfer

Before the acknowledgment, the initiator may stream its declared interpreted
Body heads as `(BodyKey, transaction commitment)` pairs. The declaration is:

- signed indirectly through the Hello's count and domain-separated digest;
- bounded in entries, bytes, chunks, and deadline;
- strictly increasing in canonical tuple order;
- duplicate-free;
- exactly decodable and exactly re-encodable;
- represented by count zero and the defined empty digest when empty.

Strict ordering/uniqueness and zero-count/empty-digest equivalence are canonical
requirements. The initial protocol-2 implementation signs and bounds holdings
but does not yet reject every alternate semantic encoding; that decoder gap must
be closed before independent interoperability is claimed.

Holdings frames are valid only in the pre-ack initiator-to-accepter window.
Wrong order, contact id, count, digest, bounds, or frame direction aborts the
transcript.

The accepter advertises its complete signed Manifest but may omit transaction
and Body material for declared heads. It stores no authoritative per-peer
holdings state. A false or stale declaration can starve only the claimant: the
receiver refuses the whole advertised root unless locally held plus transferred
material reconstructs it completely. A later truthful Contact can recover.

Opaque heads are not declared as interpreted holdings. This ensures material
that later becomes interpretable passes through validation again.

## 7. Contact transfer grammar

The transfer carries bounded frame families for:

1. ordered Mechanics authority records and their set commitment;
2. a signed Manifest-root offer;
3. requested canonical Manifest index nodes;
4. requested protected Body chunks and completion commitments;
5. transcript completion, acknowledgment, or typed abort.

Each frame binds its Contact id. Records, sets, nodes, chunks, payloads, and the
transcript use distinct domain-separated commitments. Chunk assembly rejects
conflicting duplicates, overlap, gaps, empty illegal chunks, overflow, and a
final commitment mismatch. An abort discards staged transfer material.

`TransferAck` proves framing receipt only. It is not evidence that Mechanics or
Replica accepted the data.

## 8. Convergence and incorporation

Received material remains inert until the receiver performs the complete local
validation chain:

```text
Contact transcript
  -> staged material
  -> Mechanics authority incorporation and receipt
  -> Replica transaction/receipt/parent/payload/quota validation
  -> one atomic Manifest adoption
  -> convergence outcome
```

Remote validation does not invoke World code. It verifies the authority receipt
at the historical frontier and the authority-approved World implementation id
bound into the transaction. Unsupported legitimate Bodies may be retained and
forwarded opaquely.

Manifest adoption is root-atomic. A transfer containing only a prefix of the
advertised root cannot partially advance the visible Replica. Idempotent replay
changes nothing; same identity with different bytes is equivocation.

## 9. Replicated object families

The compatibility surface includes canonical encodings and fixtures for:

- Coordinates and admission evidence;
- Beacon and presence packets;
- Mechanics effects, checkpoints, and batch receipts;
- ceremony material and terminal SpaceAuthority effects;
- World actions and authorization receipts;
- Body transactions and descriptors;
- protected Body payloads;
- Manifest roots and index nodes;
- store markers/manifests and journal objects;
- external DTO identifiers, projections, observations, and errors.

Every signed family has a distinct purpose/domain. Ceremony material cannot
decode as terminal authority; a framing receipt cannot decode as an
authorization receipt; an authority-batch receipt cannot substitute for World
authorization.

## 10. Local control channel

CLI, web, and MCP clients enter one local protocol. A request carries an
explicit bridge route:

- `daemon` for the process-level catalog and daemon lifecycle;
- `space { orbit, space }` for Mechanics, Station, observations, and lifecycle
  through one local Orbit;
- `world { orbit, space, world }` for product mutations and queries.

`orbit` is a full, stable local identifier derived from the store binding;
`space` is repeated as an expectation. Distinct local Orbits in the same Space
therefore remain independently addressable, and a stale or confused binding
fails before activation. A trusted client adapter validates the complete route
against its `ClientScope`, the LaitDaemon resolves it through its own
`OrbitDirectory`, and the receiving bridge independently validates its address.
The allowed set is never accepted as a claim in the request.

A missing route is accepted only by the historical per-home adapter: its socket
identifies one Orbit and the uniquely claiming bundled World package is
selected. An absent or ambiguous package claim rejects. The identity-scoped
LaitDaemon endpoint requires an explicit route. A version handshake precedes
requests. The production request classifier assigns every historical typed
request exactly one terminal owner; there is no wildcard product fallback.

The CLI calls its selector `--orbit` because it chooses the local durable
participation used to reach a Space. Its parser fixes the terminal target in a
`ClientAction` before resolving the selected Orbit. The route is therefore an
output of command parsing plus local navigation, not an inference repeated by
the daemon from product payload shape.

The optional `if_running: true` envelope field is reserved for passive,
explicitly Space-routed status, identity display, and configuration reload.
LaitDaemon resolves and validates the complete Orbit address, then queries only
an already-live compatibility adapter; it does not place a vacant Orbit. Other
verbs and routes reject this mode. The field is omitted for ordinary dispatch,
preserving the existing envelope shape.

Protocol v5 sends product mutations and queries as:

```text
WorldClientRequest {
  route: world { orbit, space, world },
  act_as?,
  call: WorldCall { world, operation, version, payload }
}
  -> WorldReply { world, operation, version, status, payload | error }
```

The route and call repeat the World identity and must agree. `payload` is
bounded, unpadded URL-safe base64 on the JSON wire and opaque to the host. The
registered product handler decodes it and derives query/command access; the
client cannot grant itself read-only treatment with a wire flag. Replies repeat
the exact `(world, operation, version)` tuple so a product codec cannot accept a
reply for another contract.

An owned Station receives the call directly through its in-process SpaceBridge.
An attached SpaceBridge receives the identical opaque envelope through its
per-Orbit socket. Protocol v5 retired the typed product-request path;
protocol v6 removed product host projections from root `Request`/`Response`.
Older processes are outside the compatibility window and must restart before
attachment. The issue-tracker application emits
`com.lait.issues` / `issues.control` v1; LaitDaemon does not infer or hardcode
either value.

Product client packages decode `WorldReply.payload`, own presentation for CLI
and MCP, parse their explicit web route, and execute named local operations
through generic host facilities. The browser sends Space control to
`/api/spaces/{orbit}/rpc` and product input to
`/api/spaces/{orbit}/worlds/{mount}/rpc`; there is no decode fallback between
those namespaces. Root control treats World request and reply payloads as opaque
and has no product response enum.

Host capabilities may compose calls across the boundary. An Issues access
grant first queries an `AccessPlan`, then submits its exact generic assignments
to Space authority; inbox supplies a local watermark to an Issues query and
advances it only after success. Root control never receives role, project, or
product-response vocabulary.

Product calls then reach the named World's registered handler, WorldBridge, and
docked Session.
Membership, devices, custody, and ceremonies reach Mechanics. Neighbor and
Contact operations reach Station. Lifecycle operations reach Runtime/Orbit/
Station. Clients never open Replica or Fabric directly.

JSON responses are strict versioned DTOs rather than serialized internal
objects. Unknown fields reject where the schema says strict; decoded lengths and
identifier grammars remain enforced after JSON decoding.

`Subscribe` carries Observation doorbells with Station epoch, sequence, reset
semantics, committed frontier, and dirty scopes. Frames may be coalesced. They
are not state deltas; clients re-query after notification or reset.

## 11. Failure and resource behavior

Every protocol has explicit limits for frames, records, nodes, chunks, payloads,
holdings, concurrency, and time. A peer cannot request unbounded buffering or
keep an untracked Station task alive indefinitely.

Dormancy rejects new work, terminates Sessions, cancels Contact/gossip, closes
Observations, drains tracked tasks within its deadline, persists required state,
and releases the store lock last.

An unreachable peer, interrupted join, or aborted Contact leaves a recoverable
Orbit and bounded retry state. It does not create membership, expose a partial
Manifest, or mutate guarded relay/discovery policy.

## 12. Delivery planes

Two versioned ALPNs run on the identity-scoped endpoint alongside Contact and
neighbour presence:

```text
lait/freight/1       reliable exact-object request and response
lait/session/1       long-lived realtime session
```

They are separate because they have different admission, timeouts, memory
profiles, shutdown semantics, and compatibility lifetimes. A file fetch must not
keep a realtime session alive; a cursor bug must not block artifact recovery.

**The ALPN is the version gate.** The transport negotiates it during the QUIC
handshake, so peers on different generations share no common ALPN and cannot
connect at all. There is no in-band version check and no half-speaking pair.
That makes a bump expensive, so the discipline is: bump only for a change an old
peer would *misinterpret* — a removed or repurposed field, or changed semantics
of an existing one — and carry every additive capability as a feature bit
inside the advertisement, where an absent field decodes to zero exactly as an
older build would send it.

Within `lait/session/1`, one connection carries typed stream kinds:

| Byte | Kind | Status |
|---|---|---|
| `0x01` | control | implemented |
| `0x02` | reliable signal | implemented |
| `0x03` | media frame | reserved, never reassigned |
| `0x04` | media feedback | reserved, never reassigned |

Reserved kinds are known and unimplemented, which is a different answer from
unknown: an unknown kind resets that stream, a reserved one means a peer is
speaking a protocol generation this build agreed to and has not built.

### 12.1 Opening, and 0.5-RTT replay

Both planes begin with one bounded canonical opening carrying the plane, the
generation, feature bits, the Space, both Station claims, a random session id,
a random session epoch, the authority frontier, and the requested lanes.

**Accepting an opening must be idempotent.** QUIC lets the accepting side write
before the client finishes its handshake, and the client's initial bytes can be
replayed by an attacker who intercepts handshake packets. A replayed opening
must not allocate a second session, consume a budget twice, or mint state the
first one already minted; `session_id` and `session_epoch` together are what
make a replay recognisable. No lane whose demand has an effect may dispatch on
0.5-RTT data — reads and availability answers are idempotent and safe, anything
that writes waits for a completed handshake.

A refusal is deliberately coarse. Distinguishing "not admitted" from "not
authorized for this lane" from "over budget" would tell an unadmitted peer more
about a Space than being turned away should reveal. Only an unsupported
generation is named, because it is the one refusal a peer can act on.

### 12.2 Frozen bounds, and which are ours

Every bound is a pre-allocation ceiling: checked against a declared length
before a buffer is reserved, never after bytes have arrived.

| Bound | Value | Source |
|---|---|---|
| `MAX_OPENING_BYTES` | 4 KiB | lait policy |
| `MAX_CONTROL_FRAME_BYTES` | 64 KiB | lait policy |
| `MAX_SIGNAL_BYTES` | 16 KiB | lait policy (docket ceiling) |
| `MAX_FLOW_READ_BYTES` | 256 KiB | lait policy |
| `MAX_CHUNK_FRAME_BYTES` | 320 KiB | derived from frozen content geometry |
| `MAX_DATAGRAM_BYTES` | 1200 B | **advisory** — see below |
| `MAX_LANES` | 8 | lait policy |
| `MAX_STREAM_WORKERS` | 32 | lait policy |

`comms::MAX_FRAME` is 64 MiB, the framing guard for whole protocol messages on
the existing framed `Stream`. Raw flows must **not** inherit it: a flow is read
incrementally, so its ceiling bounds one read rather than one message, and
64 MiB of pre-allocation per flow is how a handful of concurrent transfers
exhausts a receiver. `MAX_FLOW_READ_BYTES` is that separate ceiling.

**Observed, not chosen.** The transport is pinned at `iroh = 1.0.0-rc.1`, whose
QUIC implementation is `noq` with multipath and NAT traversal at the QUIC layer.
Measured over a direct local path by
`crates/comms/tests/transport_capabilities.rs`:

| Observation | Value |
|---|---|
| `max_datagram_size` | `Some(1382)`, then `Some(1162)` on a second run of the same test |
| `datagram_send_buffer_space` | 1 MiB |
| `open_uni` + write + finish | ~725 ns each over 64 |
| `SendStream::reset` after a partial write | `Ok(())`, and the receiver's read fails rather than ending clean |

The first row is the important one, and it is why `MAX_DATAGRAM_BYTES` is marked
advisory: two runs of the same test on the same machine returned different
capacities, and the second was **below** lait's own 1200 ceiling. The real limit
is the connection's current `max_datagram_size`, which is path-dependent and
moves with NAT traversal and relay fallback. A sender therefore checks capacity
at send time and coalesces or drops; it never truncates, and it never assumes
1200 is available.

Cheap stream opens are what make the "one short stream per unit of work" pattern
affordable rather than a design lait has to avoid. A reset surfacing as a read
error is what lets a receiver tell an abandoned transfer from a completed one —
without it, truncation would be silent.

If the pin moves, this table is re-measured. Every row in the bounds table above
is a lait choice and moves only when we decide it should.

### 12.3 Freight

Requests are exact. There is no "list what you have" and no remote path: a peer
asks for one chunk of one content whose id it already holds, having learned it
from durable state. Availability answers are private, bounded, and say only
which chunks are servable — and a chunk counts only when its ciphertext *and* a
validated proof sidecar are both resident.

A provider may refuse without revealing whether authorization, policy, load,
absence, or incomplete proof material caused it.

Resume is per immutable ciphertext chunk. A resumed request carries the leaf
hash the partial transfer already validated, and a provider whose leaf differs
is rejected before a byte is appended — so a resumed transfer cannot be steered
onto different content.

## 13. Conformance

An independent implementation must match:

- identifier grammar and canonical byte representation;
- signed preimages, domains, hashes, and signature verification;
- version and unknown-input rejection;
- bounds and abort classifications;
- Contact state-machine ordering and transcript commitments;
- historical authority evaluation;
- transaction and Manifest graph validation;
- protected/opaque Body behavior;
- deterministic collaborative convergence;
- local DTO schemas, errors, and Observation semantics.

Golden vectors must include positive encodings and negative substitution,
reordering, duplicate, truncation, trailing-byte, wrong-domain, wrong-Space,
wrong-peer, and over-limit cases. Round-tripping through one implementation is
not interoperability proof.
