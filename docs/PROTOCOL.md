# Protocol contract

This document defines LAIT's interoperability boundaries: signed bootstrap and
discovery packets, direct Contact, replicated authority and Body objects, and
the local daemon control channel. Rust type names are not wire specifications.

## 1. Identities and trust anchors

- `SpaceId` identifies one cryptographic and replication boundary.
- `ActorId` identifies a member within a Space.
- `DeviceId` identifies an actor device key.
- `Key` identifies a device's active network endpoint.
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

The Contact protocol field is version 2. The ALPN and individual
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
3. requested canonical Manifest index nodes, Body and content alike — one node
   family, addressed by content, so which index a node belongs to is decided by
   the signed root that reaches it and not by the frame that carried it;
4. requested protected Fabric artifacts and bounded completion commitments;
5. transcript completion, acknowledgment, or typed abort.

Each frame binds its Contact id. Records, sets, nodes, artifacts, payloads, and
the transcript use distinct domain-separated commitments. A signed transaction
descriptor commits the Body's Fabric `Material` and complete content-addressed
`ArtifactRef` closure. Artifact packs use a bounded framing table, reject a
hostile length before allocation, and may deliver causal deltas out of order;
duplicate bytes are idempotent, conflicting bytes or an incomplete dependency
closure reject. Publication additionally verifies the declared final causal
version. An abort discards staged transfer material.

The signed descriptor always commits the complete closure. A delivery pack may
omit an artifact only when the receiver declared a prior Body head and the
sender can prove the artifact is already in that head's closure. The receiver
reconstructs the strict pack from delivered artifacts plus its local
content-addressed store, then performs the same complete-closure validation.
An unknown or evicted prior-head proof falls back to sending the full closure;
cache state can cost bandwidth but cannot change meaning or acceptance.

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

Completeness covers the content catalog on the same terms as the Body set. A
root's content index is verified like its Body index — every entry canonical,
belonging to the root's Space, and sitting under the key its own hash derives —
and every `ContentRef` an adopted entry declares must resolve, either from the
advertised catalog or from a descriptor the receiver already holds. A root that
declares content it does not advertise is refused whole, because adopting it
would leave permanent local state naming bytes nobody here can ask anyone for.
The descriptors an advertisement carries are exactly those live Bodies reach.

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

Web and MCP clients enter one local protocol. A request carries an explicit
control route:

- `daemon` for the process-level catalog and daemon lifecycle;
- `space { orbit, space }` for Mechanics, Station, observations, and lifecycle
  through one local Orbit;
- `world { orbit, space, world }` for product mutations and queries.

`orbit` is a full, stable local identifier derived from the store binding;
`space` is repeated as an expectation. Distinct local Orbits in the same Space
therefore remain independently addressable, and a stale or confused binding
fails before activation. A trusted client adapter validates the complete route
against its `ClientScope`, the daemon::Daemon resolves it through its own
`orbits::Catalog`, and the receiving endpoint independently validates its address.
The allowed set is never accepted as a claim in the request.

A missing route is accepted only by the historical per-home adapter: its socket
identifies one Orbit and the uniquely claiming bundled World package is
selected. An absent or ambiguous package claim rejects. The identity-scoped
daemon::Daemon endpoint requires an explicit route. A version handshake precedes
requests. The production request classifier assigns every historical typed
request exactly one terminal owner; there is no wildcard product fallback.

The launcher calls its selector `--orbit` because it chooses the local durable
participation used to reach a Space. A head fixes the terminal target from the
route the request arrived on — `/api/host/rpc`, `/api/spaces/{orbit}/rpc`, or
`/api/spaces/{orbit}/worlds/{mount}/rpc` — before resolving the selected Orbit.
The route is therefore an output of addressing plus local navigation, not an
inference repeated by the daemon from product payload shape.

The optional `if_running: true` envelope field is reserved for passive,
explicitly Space-routed status, identity display, and configuration reload.
daemon::Daemon resolves and validates the complete Orbit address, then queries only
an already-live compatibility adapter; it does not place a vacant Orbit. Other
verbs and routes reject this mode. The field is omitted for ordinary dispatch,
preserving the existing envelope shape.

Protocol v10 sends product mutations and queries as a framed pair — a header
line, then exactly the bytes it declares:

```text
{ route: world { orbit, space, world },
  act_as?,
  call: { world, operation, version, len } }\n
<len bytes>
  -> { world, operation, version, status: "ok", len }\n
     <len bytes>
  -> { world, operation, version, status: "error", error }\n
```

This is the shape v7 gave content, for the same reason: JSON has no way to say
"bytes", so carrying them inside it costs a base64 pass and a third more wire in
each direction. A declared length costs neither. An error carries no length,
because the failure is the whole answer.

The route and call repeat the World identity and must agree. The payload is
bounded and opaque to the host, and is read in full before the call is
dispatched — so a header that arrives without its payload runs nothing. The
registered product handler decodes it and derives query/command access; the
client cannot grant itself read-only treatment with a wire flag. Replies repeat
the exact `(world, operation, version)` tuple so a product codec cannot accept a
reply for another contract.

An owned Station receives the call directly through its in-process StationHost.
An attached StationHost receives the identical opaque envelope through its
per-Orbit socket. Protocol v5 retired the typed product-request path;
protocol v6 removed product host projections from root `Request`/`Response`.
Older processes are outside the compatibility window and must restart before
attachment. The issue-tracker application emits
`com.lait.issues` / `issues.control` v1; daemon::Daemon does not infer or hardcode
either value.

Product client packages decode `WorldReply.payload`, own their MCP descriptors,
parse their explicit web route, and execute named local operations through
generic host facilities. The browser sends Space control to
`/api/spaces/{orbit}/rpc` and product input to
`/api/spaces/{orbit}/worlds/{mount}/rpc`; there is no decode fallback between
those namespaces. Root control treats World request and reply payloads as opaque
and has no product response enum.

Host capabilities may compose calls across the boundary. An Issues access
grant first queries an `AccessPlan`, then submits its exact generic assignments
to Space authority; inbox supplies a local watermark to an Issues query and
advances it only after success. Root control never receives role, project, or
product-response vocabulary.

Product World calls then reach the named World's registered handler, WorldHost,
and docked Session. A package-compiled Find invocation skips the semantic World
handler but reaches that same authenticated Session and its one evaluator; this
is an adapter distinction, not a second data path.
Membership, devices, custody, and ceremonies reach Mechanics. Neighbor and
Contact operations reach Station. Lifecycle operations reach Runtime/Orbit/
Station. Clients never open Replica or Engine directly.

JSON responses are strict versioned DTOs rather than serialized internal
objects. Unknown fields reject where the schema says strict; decoded lengths and
identifier grammars remain enforced after JSON decoding.

Control v14 adds product-neutral Runtime `Find`. A request names the World and
carries the canonical typed query DAG; the Session selects or validates its
exact publication, evaluates gates for the authenticated actor/device, and
returns an answer stamped with Manifest, implementation, extractor,
materialization, authority, actor, device, and query coordinates. Viewer, CLI,
MCP, and controllers use this same request path. No adapter may bypass the
evaluator with a private cache or reinterpret a root under current World code.

Control v15 adds daemon-owned native World update consent and status. Enqueue
durably records a stable operation before bundle download or per-Space
lifecycle work; a bounded restart-resumable worker reports progress and typed
Busy/Capacity refusal. Opening a Space or the resident channel watcher never
implies consent.

`Subscribe` carries Observation doorbells with Station epoch, sequence, reset
semantics, and invalidations grouped by stable World id. Inside each World,
item scopes carry `{kind,id,label,docs}` and structural planes carry
`{plane,scope?}`. The host never interprets those strings, and two Worlds that
choose the same word cannot invalidate each other's clients. Frames may be
coalesced. A frame may also carry authenticated operation/actor/device
attribution and bounded value-free Body/path/text-range changes. Those changes
carry canonical Fabric start/end anchors plus scalar offsets at the exact
candidate publication when Runtime can prove a local text splice. Concurrent
or unprovable remote edits degrade to `Dirty`. The ranges support immediate
cursor movement, highlighting, and a single `Seek::Bodies` refresh, but they
are not state deltas: values still come from the exact published corpus.
Overflow becomes an explicit dirty Body; reset, dirty, or expired coordinates
require a broader re-query. The World-scoped invalidation portion is the v9
shape; v8's Issues-specific doorbell fields are not accepted because decoding
them as empty would silently leave clients stale.

`LiveSubscribe` is the v8 standing projection for ephemeral presence, cursor,
typing, and text-preview rows. Its first line is a complete Live snapshot;
later lines are emitted when the table generation changes or at the exact local
deadline where an age-derived uncertainty/partial flag changes. It is a
latest-state stream, not an event log.

### 10.1 The content envelope

Protocol v7 adds a third envelope, the only one on this channel whose frames are
not entirely JSON:

```text
{ content: ContentCall, route, act_as?, body_len: N }

<exactly N raw bytes>
  -> { kind: "content_stream", len: M }
 <exactly M raw bytes>
  -> or one JSON line and no body
```

It exists because the alternative does not work at the sizes this plane is for.
A 256 MiB attachment as a JSON string is base64 on the wire, one allocation on
each side, and one token to the parser. The declared length is what lets both
ends stream, and it is authoritative in both directions: a body that ends early
is an error rather than a shorter content, because "however much arrived" and
"all of it" are otherwise the same thing, and the difference would be a
permanently wrong content that hashes perfectly well.

Order is fixed and each step is what makes the next one safe. The declared
length is checked against the Station's `max_content_len` **before** any body is
read, so a request that was always going to be refused cannot spend the disk
budget first. The header is read under a frame bound, so a sender that never
sends a newline is refused rather than buffered. The body is read through the
same reader that consumed the header — that reader has already pulled the first
bytes of the body while looking for the newline, and reading from anywhere else
silently drops them.

Refusals are typed — denied, unknown, not-resident, bounds, storage, invalid —
because a caller acts differently on each: a missing chunk is worth retrying
after a transfer and an unknown content never will be. Unknown says nothing
about whether the content exists elsewhere; a caller that could tell "not here"
from "never heard of it" would have an oracle for what a Space contains,
answerable by guessing ids.

An owned Station serves the call in process, so the body crosses from the socket
to the sealer without leaving that address space. An attached StationHost is
proxied byte for byte down its per-Orbit socket, never refused: `Attached` is a
reachable placement, and a surface that works only when the Station happens to
be in-process has a hidden precondition. Sealing runs on a blocking thread
because it takes the Replica's one writer, not because it touches a disk —
holding that writer on a runtime thread stalls the Contact driver too.

One connection is one request. A content transfer gets its own connection, which
is the grain this channel already had.

### 10.2 Transient state, and reliable signals

The Live plane carries two things that are not Bodies and never become them.

**Transient state** — cursors, presence, typing, residency hints — is what a
Station currently believes and will happily forget. It is never journaled, never
an Observation, and never survives a restart: a caret that outlived the tab
holding it is a ghost, and a presence that survived a crash is a lie about who
is here. Three rules follow, and they shape everything else:

- **Nothing has a goodbye.** A tab closing, a laptop sleeping and a network
  dropping all deliver exactly nothing, so every slot carries its own expiry.
  Retirement is an optimisation on top of that and never a prerequisite.
- **Epochs are compared, never ordered.** A `connection_epoch` is sixteen random
  bytes minted per reconnect, so two of them have no order. The only answerable
  question is whether an item's epoch is the one this session was admitted at.
- **The table is bounded and everything in it is evictable**, because nothing in
  it is correctness. A full table costs a stale cursor; an unbounded one is a
  Station a Space can make allocate without ever committing anything.

A payload that does not fit the path's datagram capacity is dropped and counted,
never truncated. Transient payloads have no retransmit, so half of one arrives as
corruption rather than as a gap.

An item's anchor path must equal the field its scope names. That is a bound
rather than a consistency check: the path becomes a container key inside the
collaborative document, so an anchor free to name any path is a peer choosing
which container a resolve touches.

**Reliable signals** are bounded one-message events — an invitation, a file
offer, an attention ping. Reliable means delivered or failed loudly; it does not
mean durable. The wire is:

```text
stream_kind | u16 selector | u32 length | canonical body
```

The selector precedes the length, and that ordering is load-bearing: a
declaration's `max_bytes` is a pre-allocation ceiling only if it is known before
the length is read. Behind the length, the schema is known only after a buffer
that size already exists, and the per-signal maximum is decoration. An unknown
selector is refused with no length consulted, the ceiling is floored against the
plane's own so a declaration table cannot raise the hard limit, and the decoded
body must be the signal its selector promised — otherwise a small declaration's
ceiling could be used to smuggle a larger-bounded shape past it.

A signal's display name is sanitised **on use** and never on decode. A
decode-time rewrite would make `encode(decode(x)) == x` false, and canonical
re-encode equality is what every shape on this plane rests on. A control
character is refused outright rather than repaired: it lands in a header, a
filename or a terminal, and none of those are places a peer chooses what
happens.

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
| `0x03` | native media Group | implemented, unidirectional |
| `0x04` | native media control/feedback | implemented, bidirectional |

The media lanes are an atomic pair and require the `NATIVE_LIVE_MEDIA` feature.
Admission grants neither unless the peer offered the feature and requested both
lanes. That prevents a half-capable session from receiving Groups it cannot
control, or control records for Groups it cannot receive. Unknown kinds reset
that stream and leave the connection alive.

### 12.1 Flows, framing, and 0.5-RTT replay

Both planes begin with one bounded canonical opening carrying the plane, the
generation, feature bits, the Space, both Station claims, a random session id,
a random session epoch, the authority frontier, and the requested lanes.

**Which flow carries what.** The opening arrives on the initiator's first
unidirectional flow and nothing else is written there. The answer — an accept or
a refusal — goes back on the responder's first unidirectional flow. Each is one
whole message with no length prefix: one message per flow needs no delimiter
because finishing the flow is the delimiter. Both are refused against the
opening ceiling before a buffer that size exists.

A router that demultiplexes by Space has to read the opening to do it, and
reading a flow consumes it, so the opening bytes travel *with* the connection to
the Space that owns it rather than being replayed. The owner decodes what the
router decoded. If the two disagree the connection is refused, because
everything downstream trusts that parse and the more permissive reading is never
the safe one.

**Freight flows carry no stream-kind byte.** The ALPN types the connection, and
the stream-kind table above is scoped to `lait/session/1`. A Freight opening
therefore names no lanes, and a well-behaved one carries an empty lane list.

An opening that names lanes anyway is **not** refused for that alone, and this
paragraph used to say it was. Refusing would be a wire-visible rule, and adding
one costs a frozen encoding a regeneration for a case that harms nobody: a lane
byte on a plane that serves no lanes is granted by nothing and reaches no
handler. Freight's quarantine is not a lane filter but an ordering rule — no
bidirectional flow is served before the accept has been written — and that rule
is what the plane actually relies on.

**One bidirectional flow per Freight request.** The flow is the correlator, so
there is no request id, no table keyed by one, and nothing of that shape to
bound. An abandoned request is a reset flow, and a peer cannot accumulate
outstanding state by asking without listening; concurrency is bounded by flows
rather than by identifiers.

**The framing on a Freight flow** is a 4-byte little-endian length prefix
followed by one canonical postcard frame. The prefix is what is checked: the
*declared* length is compared against the ceiling before a buffer that size is
reserved, so a peer cannot make a receiver allocate by claiming a large number
and then sending nothing. For a chunk answer the raw ciphertext follows the
header frame on the same flow, unframed, and the flow's finish ends it. Both
directions use the same framing so one reader serves both, but only the answer
strictly needs it — a request is the only thing on its flow, while a chunk
answer has a boundary inside it.

**Accepting an opening must be idempotent.** QUIC lets the accepting side write
before the client finishes its handshake, and the client's initial bytes can be
replayed by an attacker who intercepts handshake packets. A replayed opening
must not allocate a second session, consume a budget twice, or mint state the
first one already minted; `connection_id` and `connection_epoch` together are what
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

Native media adds narrower bounds at its own seam:

| Bound | Value | Meaning |
|---|---:|---|
| `MAX_TRACK_NAME_BYTES` | 128 B | identifier, never a path or URL |
| `MAX_CODEC_NAME_BYTES` | 64 B | WebCodecs codec string |
| `MAX_DECODER_CONFIG_BYTES` | 64 KiB | codec extradata |
| `MAX_MEDIA_HEADER_BYTES` | 4 KiB | one Group or Frame header |
| `MAX_MEDIA_FRAME_BYTES` | 16 MiB | one encoded access unit |
| `MAX_FRAMES_PER_GROUP` | 512 | frame-table ceiling |
| `MAX_MEDIA_GROUP_BYTES` | 32 MiB | materialized Group ceiling |
| `MAX_CATALOG_BYTES` | 256 KiB | one full canonical `catalog.json` update |
| `MAX_CATALOG_TRACKS` | 64 | advertised raw/CMAF/HLS variants |
| `MAX_GROUP_DURATION_MS` | 10,000 ms | hard keyframe-recovery cap |
| `MAX_LATENCY_MS` | 30,000 ms | hard negotiated delivery budget |
| `MAX_SUBSCRIPTIONS_PER_SESSION` | 128 per direction | connection-lifetime id/churn ceiling |
| `MAX_FETCHES_PER_SESSION` | 128 per direction | one-use Fetch id/response-handle ceiling |
| `MAX_ACTIVE_GROUPS_PER_SESSION` | 32 | incomplete Group/worker ceiling |

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

### 12.3 Native live media

The native wire adopts moq-lite's useful delivery semantics and owns its bytes.
A Track is a sequence of Groups; a Group is an ordered sequence of Frames and
occupies exactly one QUIC unidirectional stream. A live publisher assigns newer
Groups a higher advisory QUIC priority. An old or malformed Group is reset at
the stream rather than closing the Live session, so it cannot head-of-line a
new keyframe or consume flow control after it stops being useful.

The generation-1 vocabulary has fifteen explicit message selectors: thirteen
control records plus `GROUP` and `FRAME`. Control records are `SETUP`,
`SUBSCRIBE`, `SUBSCRIBE_UPDATE`, `SUBSCRIBE_OK`, `SUBSCRIBE_DROP`,
`SUBSCRIBE_END`, `FETCH`, `TRACK_INFO`, `REQUEST_KEYFRAME`, `CLOCK_PROBE`,
`CLOCK_REPLY`, `PLAYOUT_TARGET`, and `GO_AWAY`. Their selector byte is explicit;
Rust enum declaration order is not the wire. Each body is canonical postcard
and must reproduce its input exactly when re-encoded.

One `0x03` stream is:

```text
0x03
u32-le GroupHeader length | canonical GroupHeader
u32-le FrameHeader length | canonical FrameHeader | exact raw payload
...
FIN
```

Every declared length is checked before proportional allocation. `GroupHeader`
binds the subscription, Track, Track kind, sequence, coordinator-timeline
publication time, timescale, and maximum Group duration. `FrameHeader` is shaped
for a WebCodecs consumer: signed presentation timestamp, optional duration,
timescale, `Key`/`Delta`, and exact payload length. The first Frame is always
`Key`, timestamps never decrease, every Frame uses the Group timescale, and the
presentation span cannot cross the Group's declared duration or the protocol's
hard cap. Codec configuration is carried out of band by `TRACK_INFO`; peer media
payloads are raw encoded access units, never CMAF or another container.

One `0x04` flow carries one bounded control or feedback record. Track names are
identifiers, not routes: empty names, absolute paths, traversal, control
characters, and URL-like names are refused. `PLAYOUT_TARGET` carries a shared
time and media position as a correction target; it is never an imperative seek
command. `REQUEST_KEYFRAME` is the bounded recovery verb for a late join or a
dropped Group.

Both sides send exactly one `SETUP` before any other media control. Subscription
ids have one owner, are unique for the life of the connection, and move through
`Pending`, `Active`, then `Ended`; an ended id is never rebound. `UPDATE` is
accepted only from the subscriber that minted the id, while `OK`, `DROP`, and
`END` are accepted only from its publisher. A Group header must name an active
subscription and match that Track's preceding `TRACK_INFO` kind and timescale
before any frame payload is allocated. Latency and Group-duration limits are
the minimum of both `SETUP` offers. The
connection-scoped event handle is the stateful publishing seam: it serializes
control transitions, derives newer-Group QUIC priority, exposes current path
quality for conservative encoder adaptation, and refuses a Group the peer did
not subscribe to.

`FETCH` is the one control record that is a transaction rather than a
fire-and-forget event. The subscriber sends one request and a clean request FIN
on a bidirectional media-control stream; the publisher returns the requested
Group's length-framed Frames directly on that stream and FINs after the last
Frame. Track and Group sequence are implicit in the request, so no Group header
is repeated. A reset is the only refusal and an empty response is invalid.
Fetch ids have separate directional ownership, remain spent for the connection
lifetime, and are capped at 128 per direction. The request must resolve to
TrackInfo already carried by the current catalog (or the well-known catalog
Track itself), and the response has the same keyframe, timestamp, duration,
codec, and allocation bounds as a subscribed Group. Generic control sending
refuses `FETCH`; callers must use the transaction API so the response half
cannot be discarded. Fetch priority uses the moq-lite ordering: zero is most
important and is inverted only at the transport scheduling seam.

`catalog.json` is the well-known catalog Track. Each update is one complete,
canonical JSON document carried as the only key Frame in a new Group; lait does
not use catalog deltas, so a late join never depends on an expired predecessor.
The catalog carries WebCodecs codec strings and lowercase-hex decoder
description bytes, timescale, bitrate, dimensions and frame rate or audio
shape, render group, target latency, and a jitter hint. A generation-one
catalog that advertises video must include an H.264 (`avc1.` or `avc3.` plus
six hexadecimal profile characters) variant; one that advertises audio must
include AAC-LC (`mp4a.40.2`, `mp4a.40.02`, or `mp4a.67`). AV1 (`av01.*`) is
negotiated
optional. The connection installs only increasing catalog Group sequences; the
corresponding `TRACK_INFO` and every later media Group must match the latest
selected catalog entry exactly.

Encoder adaptation is connection-scoped and reads fresh QUIC path evidence; it
does not replace or configure QUIC congestion control. A video rendition starts
at its configured bitrate/frame-rate floor. The controller keeps 25% headroom
under `cwnd / RTT`, reduces immediately when capacity falls, queue delay grows
150 ms over the best RTT seen on that path, loss reaches 2%, or QUIC reports a
congestion event, and raises at most 25% once per three seconds. A path change,
counter reset, or unknown telemetry returns to the floor. When one encode feeds
several receivers, the source uses the minimum bitrate and frame rate across
their independent controllers. No padding, FEC, or duplicate media is
synthesized to probe an application-limited path.

CMAF and HLS v3 availability is named by a small opaque rendition id. Catalogs
cannot contain a URL or path. Astrolabe resolves that id inside the receiver's
assignment-bound HTTP session and is the only component that mints CMAF/HLS;
the peer Group payload remains raw encoded access units.

Groups use reliable streams, not datagrams, and use no application FEC. An
incomplete Group becomes reset-eligible only after a newer sequence exists.
Two clocks then apply: its coordinator-timeline age and its local monotonic age.
Whichever deadline arrives first resets the old QUIC stream. Active Group
registrations are connection-scoped and removed on completion, refusal, task
cancellation, or session end, so a skewed or stalled clock cannot buy old bytes
more flow-control time.

If the pin moves, this table is re-measured. Every row in the bounds table above
is a lait choice and moves only when we decide it should.

Not every number below is a lait choice. Some are *host-derived* — calibrated
against the machine lait was developed on rather than against anything the
protocol requires. No row in the bounds table above is one of those; those are
pre-allocation ceilings and all ours. The concurrency and disk ceilings below
are, and they are operator-configurable precisely so a laptop default is never
later read as a protocol limit. A peer may assume none of them.
`COMPATIBILITY.md` §7 is where the three sources are argued and where the
host-derived ceilings are listed with their configurability.

| Ceiling | Value | Source |
|---|---|---|
| inbound connections per identity endpoint | 128 | host-derived |
| inbound connections per Space | 64 | host-derived |
| inbound connections per peer per ALPN | 2 | lait policy |
| concurrent serve tasks per Space | 32 | lait policy |
| concurrent inbound transfers per Space | 8 | lait policy |
| chunks in flight per provider / per transfer | 4 / 8 | lait policy |
| staged bytes per Space | 64 MiB | host-derived |
| resident cache quota | 4 GiB default, operator-set | host-derived |
| largest single content | 256 MiB default, operator-set; may only lower the protocol maximum | host-derived |

Two connections per peer per plane because one reconnect may legitimately
overlap one connection that is still closing. Two ALPNs share one endpoint, so a
peer holding a Freight connection and a live connection at once is ordinary
rather than suspicious, and the per-Space ceiling is half the endpoint's so no
Space can starve a sibling. Staged bytes get their own ceiling because staging
is real disk that the cache quota does not count — an entry is not resident
until it installs, so without it a fleet of half-finished transfers fills a disk
while the cache reports itself comfortably inside its quota.

**The named deadlines.** Every one bounds how long a peer can hold something of
ours. They are layered rather than independent: a requester's budget covers the
provider's plus a margin, so a timeout names one side instead of a race.

| Deadline | Value | Source | What it bounds |
|---|---|---|---|
| opening read | 5 s | lait policy | a dialer that connects and then says nothing |
| accept write | 2 s | lait policy | writing the answer; longer means the peer is not reading |
| flush before drop | 5 s | iroh-derived | waiting for a written refusal to land |
| chunk resolve | 5 s | lait policy | a provider resolving a descriptor and answering |
| chunk header | 8 s | lait policy | the requester's side of that same exchange |
| chunk body idle | 10 s | lait policy | *progress*, not duration — reset only by a non-empty read |
| availability answer | 5 s | lait policy | a unary answer; a provider never scans to produce one |
| freight idle | 60 s | lait policy | a connection with no transfer in flight still holds slots |
| authority revalidation | 2 s | lait policy | how long a session can outlive a revocation |
| driver poll | 25 ms | lait policy | how long a driver may be parked without noticing cancellation |

The flush wait is iroh-derived because dropping a connection resets its streams:
without it a refusal that was correctly written reaches the peer as an ambiguous
transport error it will retry, which is the opposite of a refusal. If the pin's
close semantics change, that number is re-derived.

Four relations are asserted rather than assumed, because each one silently
breaks something if a value is lowered in isolation: the chunk-header deadline
exceeds the chunk-resolve deadline by a margin; the flush wait outlasts the
accept write; revalidation is far shorter than the idle reap; and the driver
poll is shorter than anything a driver does.

### 12.4 Admission, in order

Every opening is judged in one order, and the order is contract rather than an
implementation detail:

1. **Plane agreement.** The opening's plane must be the one the ALPN already
   fixed. An opening that disagrees is not confused, it is trying something.
2. **Generation.** The declared protocol version must be the one this ALPN
   speaks.
3. **Space.** The opening must name the Space this route serves.
4. **Initiator claim.** The claimed initiator Station must equal the identity
   the transport negotiated. This step is what turns a claim into a fact.
5. **Responder claim.** The claimed responder must be this Station. An opening
   addressed elsewhere is not ours to accept however well formed it is.
6. **Operator policy.** Whether this Station *will*, which is a different
   question from whether the peer *may*: an operator declining to serve bytes
   over a metered link is making no statement about anyone's membership.
7. **Mechanics.** Only now is the peer resolved to an actor at a locally
   resolvable frontier, and that frontier is pinned for the connection's life so
   every later question is answered against one view.

**Why that order.** Everything before step 7 is a comparison against a
fixed-size value the local side already holds. Nothing a remote peer writes can
make any of it more expensive, so a misaddressed or unadmitted opening costs
comparisons rather than an authority resolution — the one step that touches
shared state, and the one whose cost a flood would most like to multiply. An
implementation that resolved first and checked afterwards would reach the same
verdicts and still be exploitable.

**The claimed frontier is diagnostic.** Admission resolves at the *local*
frontier and never at the one the opening asserts, so a fabricated or dominating
claim buys nothing. It is also the only variable-length field of any size in the
opening, and is bounded at half the opening ceiling for that reason.

**Lanes are granted, not requested into existence.** The accept carries the
intersection of what the peer asked for with what this build implements. A lane
nobody asked for is never granted; a lane this build does not implement is
dropped from the intersection rather than refused, so a newer peer asking for
one lane we do not have still gets the lanes we do — that is what makes lanes
additive inside a generation. An opening whose requested lanes leave nothing
granted is refused, and a peer that later opens an ungranted flow is refused at
that flow rather than retroactively at the opening.

**Feature bits are intersected, never echoed.** The accept advertises the bits
the peer set that this build also implements. An unknown bit is ignored and not
reflected back, so setting bits at a peer is not a way to enumerate what it
supports.

**The refusal funnel.** One answer covers every question about standing: not
admitted, not authorized for a lane, over budget, and operator policy are
indistinguishable, because a peer that could tell them apart could map a Space
by being turned away from it in different ways. A second, structurally different
answer covers an opening that is not ours to judge at all — unparseable, bound
to another peer, or addressed to another Station or Space — and it says nothing
about the Space, only that the bytes were wrong. Below both, a connection the
router cannot place is closed with no answer at all, which is the coarsest
answer there is, and every close carries one code for every reason.

The single exception is an unsupported generation, which names the version this
build speaks because it is the one refusal a peer can act on. In practice the
ALPN gate makes it nearly unreachable — peers on different generations share no
ALPN and never connect — so it is best read as a reserved vocabulary for a
mismatch that arrives some other way; an opening whose declared version this
build cannot speak is still refused without elaboration.

A refusal is always written before the connection is closed, and the close waits
out the flush deadline. A close on its own arrives as a transport error the peer
will retry.

### 12.5 Freight

Requests are exact. There is no "list what you have" and no remote path: a peer
asks for one chunk of one content whose id it already holds, having learned it
from durable state. Availability answers are private, bounded, and say only
which chunks are servable — and a chunk counts only when its ciphertext *and* a
validated proof sidecar are both resident.

A provider may refuse without revealing whether authorization, policy, load,
absence, or incomplete proof material caused it.

**Exactness, stated as a rule.** An availability question names one content id
the peer already holds and the exact chunk indices it cares about. Both halves
matter: the id is not discoverable on this plane, and the indices are what bound
the work. A question about three chunks costs three existence checks whether the
content has four or four million, so a request cannot be turned into work by
being about something large. At most 4096 indices may be named.

An answer is bounded by what was asked. It is a subset of the named indices,
ascending and duplicate-free, and never mentions a chunk the question did not.

**Residency and ignorance are the same answer.** Content this Station has never
heard of answers exactly as content it holds no chunk of: the availability frame
with an empty list. A peer able to tell those apart would have an oracle for
what a Space contains, answerable by guessing ids, and the exactness rule above
would buy nothing. The guarantee is over the answer's *shape*; `THREAT-MODEL.md`
records what it does not cover.

**A chunk answer** is a header frame naming the content, the chunk index, the
canonical proof, and the whole chunk's ciphertext length, which agrees with the
proof's leaf. The proof is bounded at the same ceiling the resident cache
accepts, deliberately not a second one — a sidecar that arrived inside the wire
bound and then could not be stored would be a transfer that verifies and fails.
The requested range of raw ciphertext follows on the same flow and the flow's
finish ends it; what follows is never more than the request's own maximum or the
frozen chunk ceiling, whichever is smaller.

Those bytes are ciphertext. A provider does not need the Body key and must not
require it: verification is against the descriptor's ciphertext Merkle root,
which is exactly why the tree commits ciphertexts. A requester verifies the leaf
against its own descriptor before it appends a byte, and re-hashes the whole
chunk and re-verifies its proof before anything is installed where it could be
served on.

**The funnel covers answers, not only requests.** A provider that finds itself
about to write something past a ceiling writes the refusal instead — and writes
*only* the refusal. A bound checked only on receive turns a local mistake into a
remote protocol error attributed to the wrong side; refusing is a legal answer
and the only one that keeps every refusal identical, which it stops being the
moment a refusal is followed by a body. A peer that sends an answer frame where
a request belongs is refused the same way.

Standing on this plane is Space membership at the pinned frontier plus local
operator policy. There is no per-content read demand yet, so an admitted member
can pull the ciphertext of content attached to something it holds no read grant
on. That gap and its migration are recorded in `THREAT-MODEL.md` rather than
left implied here.

Resume is per immutable ciphertext chunk. A resumed request carries the leaf
hash the partial transfer already validated, and a provider whose leaf differs
is rejected before a byte is appended — so a resumed transfer cannot be steered
onto different content.

### 12.6 Live

Freight moves bytes somebody asked for. Live moves what people are *doing* —
where a cursor is, who is looking at an issue, who is typing, who holds part of
a file — and the difference that decides every rule below is that none of it is
worth retransmitting. A caret that arrives late is wrong, not delayed.

**Transient items ride datagrams and are never retransmitted.** A lost one is
repaired by the next one, or by a refresh, and never by a resend: the value it
carried was superseded before the loss was noticed. A payload that does not fit
the path's datagram capacity is dropped whole and counted. It is never
truncated and never fragmented — half a transient payload arrives as corruption
rather than as a gap, and there is no retransmit to correct it with.

A peer that negotiated no datagram support at all gets nothing rather than a
fragment, and the view says so through its partial flag.

**Every item names a scope, and a scope names a Body by World and id.** An
anchor that named only a Body would resolve against whichever World asked;
operation ids collide across the documents of one activation, so that is not a
lookup miss but a plausible and silently wrong answer.

**A scope admits only the payload kinds it is for.** A view scope carries
presence; a caret scope carries a caret or a selection; a typing scope carries
typing; a residency scope carries residency. This table is what makes the
per-connection slot ceiling arithmetic rather than an assertion — no scope
admits more than two kinds.

**An anchor's path must equal the scope's field.** The path becomes a container
key inside the collaborative document, so an anchor free to name any path is a
peer choosing which container a resolve touches on the receiver. Binding it to
the subscribed scope means a peer can only ask about what it already said it
was watching. The encoded anchor is bounded independently, before anything
resolves it.

**Session epochs are compared, never ordered.** An epoch is sixteen random bytes
minted per reconnect. Two of them have no order, so "is this stale" can only be
equality against the epoch the session was admitted at. Anything shaped like a
comparison would be reading noise as sequence.

**Nothing has a goodbye.** A tab closes, a laptop sleeps, a network drops, and
none of those delivers a message. Every slot carries its own expiry and
disappears without anyone saying so. Retirement is an optimisation on top of
that and never a prerequisite — and a retirement records a high-water mark that
outlives the slot, because a datagram already in flight when the retirement was
written would otherwise rebuild the slot for a full TTL.

**Subscriptions replace rather than accumulate.** A subscription is a snapshot
of what a client is looking at. An incremental protocol would let a client that
adds and removes views faster than its messages arrive end up subscribed to a
set neither side agrees on.

**A subscription is also a declaration, and it goes first.** On this plane the
two are one fact: having a document open is both "tell me about this" and "I am
here". A receiver drops a datagram for a scope the connection never subscribed
to — that bound is what stops a peer making a Station hold state on its behalf —
so presence published without a subscription is presence silently discarded.

**Nothing orders a reliable flow against an unreliable datagram**, and the
subscription rides the first while presence rides the second. A presence datagram
that overtakes its own subscription is therefore dropped, and the next word that
peer would hear is a full refresh interval away. A publisher re-sends the declared
set on a fast beat for a few seconds after it changes, which closes that window at
the cost of a handful of datagrams. It is not an acknowledgement scheme — there is
nothing to acknowledge with here — and a face appearing a second late is the worst
outcome rather than a lost one.

**Presence is re-sent before it expires, not only when it changes.** A slot dies
on the *receiver's* clock, so a publisher that spoke once and never again watches
everybody vanish after a minute and a half. The refresh interval sits comfortably
inside the presence TTL so that losing two refreshes in a row is survivable, which
a datagram path does without apologising.

**A departure is retired rather than left to expire.** Retirement is an
optimisation on top of expiry and never a prerequisite, but the difference a
person sees is a face vanishing when a tab closes rather than ninety seconds
later. The retirement carries the publisher's current sequence number, so a
presence datagram already in flight cannot rebuild the slot behind it.

**Awareness may be partial and has to say when it is.** Over the session
ceiling, or when a gate has dropped an item, the view reports itself
incomplete. Durable convergence is unaffected — that is Contact's job and it
does not ride this plane. A surface that can be incomplete and does not say so
is telling a confident lie, and the cap exists precisely so that it can be
reached.

**Residency hints are a Live capability, negotiated per session.** A hint says
who to ask first; it is not an inventory. It answers with one of three
states — absent, partial, complete — and never a chunk list, because a complete
bitmap would let a peer reconstruct which parts of a file somebody had opened.
It is keyed by the full content id: a prefix would let a peer probe "do you hold
anything under these bits" without knowing an id at all, which is weaker than
Freight's exact availability question. A peer that did not negotiate the
capability may neither receive hints nor publish them.

**Revocation reaches a live session two ways, and both are needed.** The
connection owner watches for authority to advance and closes a session whose
peer no longer has standing; the session itself re-asks on a bounded interval
and before adopting any subscription change. The first is edge-triggered and can
be missed; the second is what stops a revoked peer acquiring new scopes in the
window before the edge arrives. A revoked peer's slots disappear immediately
rather than at their TTL.

### 12.7 Reliable signals

A signal is a thing that happened — somebody offered you a file, invited you to
collaborate, asked for your attention. It is reliable in the sense that it is
delivered or it fails loudly, and it is durable in **no** other sense: nothing
journaled, nothing replayed after a restart, nothing that becomes activity.

That negative is the whole contract. It is enforced structurally rather than by
convention: the signal module may not name the Replica writer or the
Observation ring, a source gate fails the build if it does, and a behavioural
test asserts that driving signals moves neither the frontier, nor any byte under
the store directory, nor the Observation sequence.

**Framing: stream kind, then a `u16` selector, then a `u32` length, then a
canonical body.** The selector precedes the length, and that ordering is the
only reason a per-signal ceiling means anything. Behind the length, the schema
is known only after a buffer already exists, and every per-schema maximum
becomes decoration. Resolved first, an unknown selector is refused with nothing
allocated and the length never consulted. The declared length is refused against
the smaller of the schema's ceiling and the plane's, so a bad declaration table
can lower the limit and never raise it.

**One signal per flow.** The opener writes the stream kind; a response does not,
because the flow's kind was fixed when it was opened. Writing it again shifts
every field the reader is about to parse.

**Refusal is the pair, always.** Stopping the read half *and* resetting the
write half. A send-half reset alone does not stop an inbound writer: the peer
keeps writing into a flow nobody reads, and a refused peer can still drain a
full signal ceiling past a refusal that already happened. The refusal has to
cost the refused rather than the refuser.

**Every signal is declared, and an undeclared one is refused.** A declaration
fixes the selector, the ceiling, what the sender must satisfy, and whether an
answer is permitted. A signal nobody declared is one nothing knows how to bound.

**Every signal flow is bidirectional, whatever its response policy.** The
lane keeps one flow shape so a Ping can answer on the same flow; a one-way
signal simply ignores the receive half. Live's unidirectional queue is served
concurrently and belongs to native media Groups.

**The response policy lives on the declaration, not on the call**, and it governs
what it is actually about: whether an answer is read and a second deadline
spent. A caller cannot choose to wait for an answer to something nobody promised
to answer. Only the liveness ping expects one, and its answer carries the same
nonce — which is what makes it an answer to *that* ping rather than to any ping.
The acknowledgement is itself one-way, which is how a ping does not become a
loop. A sender that expects nothing stops its read half rather than dropping it,
so a peer cannot write into a flow the other side will never read.

**A World signal is refused when this build cannot interpret it.** A World this
Station does not host, and a World whose implementation is not active at the
session's pinned frontier, get the same answer: not registered. Both mean this
build cannot interpret that payload, and interpreting it anyway is how a schema
nobody reviewed gets acted on. That is a different answer from denial, and
neither reveals the other.

**Acceptance is not a delivery receipt.** It says the bytes left, framed and
bounded. It says nothing about whether a person saw them. Delivery failure is
deliberately not observable to the sender: zero local listeners and a lagged
local ring both leave the wire outcome unchanged, or a peer could learn whether
a viewer is open by pinging with an attention signal.

**Presence decides delivery, and nothing is held for the absent.** A signal
reaches a peer that currently holds a session; a peer with none is not queued for
and not retried. That is not a limitation being apologised for — the durable
record behind every signal is already committed and already converging, and it is
the absent peer's path. Holding signals for later would make this plane a
mailbox, which is the one thing a plane that keeps nothing must not become.

Choosing by presence rather than by a stored preference is the whole reason the
two planes are worth having together: presence is what is true now, and a
preference is what somebody configured once.

**A World says who and what; the host says whether they are reachable.** A World
that could see who is connected would be a World holding a delivery plane, and a
host that knew what a product's verbs mean would be a host holding product rules.
Neither happens: a World answers with an actor and a declared schema, and the host
turns that into a signal only for the peers it can actually reach.

**Nobody is told about their own action.** The acting identity is filtered out of
every fan-out. A person notified of everything they did stops reading
notifications, which costs the notifications that matter.

**An outbox refuses the newest rather than evicting the oldest.** Both are facts,
and evicting trades one for another of exactly equal standing — the rule a cursor
stream wants and a signal does not. The refusal reaches no sender: telling one
would be telling it about the receiver's queue.

**A file offer is a message, not a transfer.** Receiving one queues a name and a
content id. No fetch starts, no path is resolved, no byte is written. Whether
the receiver wants a gigabyte is a decision a person makes, and starting the
transfer on arrival would let any member spend a Station's disk by sending a
message. The queue is bounded and refuses the newest when full rather than
evicting the oldest — it is an inbox, and what is in it may be about to be acted
on. A full queue is indistinguishable to any sender from an empty one.

Automatic acceptance is gated three ways: the sender resolves to one of the
receiving identity's own devices, the Station has explicitly opted in, and a
destination is explicitly resolvable. The first is the strictest and is the
reason automatic acceptance is defensible at all — a file that lands on disk
without anyone clicking came from another machine belonging to the same person.
Failing any gate leaves the offer queued. Opting in never widens the first gate.

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
