# Threat model

This document states the security claims and explicit limits of the current
orbital implementation. It is not an audit report. The composed protocol and
novel ceremony code remain unaudited.

## Assets

- protected World Body contents and semantic history;
- integrity and completeness of the Replica transaction/Manifest graph;
- actor, device, membership, scoped capability, and delegation history;
- authority-approved World implementation identity;
- admission capabilities and role-assignment evidence;
- device private keys, Body keys, recovery secrets, and custody shares;
- attribution of signed authority and Body transactions;
- availability of sufficient replicas, routes, and recovery participants.

## Trust boundaries

The LAIT process, operating-system account, trusted native World code, and
readable local secret files are inside the device trust boundary. Local web and
MCP clients receive only the authority of the local control capability and
selected identity; they do not receive storage access.

Remote peers, gossip participants, Contacts, relays, discovery services,
replicated bytes, display names, clocks, routes, and network paths are untrusted.
Reachability or possession of ciphertext does not confer membership.

Signed Mechanics history is the Space authority anchor. Validity depends on
domain-separated signatures, Space binding, canonical encoding, causal history,
actor/device resolution, and authorization at the referenced historical
frontier. Current authority is never substituted for historical authority.

The active `WorldImplementationId` is a trust decision. It binds reviewed
in-process semantic code and its policy table; it is not remote attestation or a
sandbox guarantee.

## Adversaries considered

- an unauthenticated observer, relay, discovery peer, or gossip participant;
- a Contact peer sending malformed, reordered, duplicated, truncated,
  conflicting, oversized, or commitment-substituted frames;
- a peer lying about its holdings or withholding material;
- a peer presenting unauthorized transactions or an incomplete Manifest root;
- a former member retaining everything received before removal;
- a compromised current device acting with its legitimate keys and assignments;
- concurrent administrators, writers, or ceremony participants;
- a local unprivileged client attempting to cross Space, World, Body, Session,
  or identity boundaries;
- corruption or interruption at journal/object/manifest boundaries;
- a buggy or malicious registered World attempting cross-World operations,
  excessive resource use, insufficient demands, or mixed-root projections.

## Intended properties

- A peer lacking the required Body key cannot decrypt a protected Body merely by
  obtaining Replica or Contact material.
- Signed effects and transactions cannot be forged without the corresponding
  authorized signing key.
- Device authorship resolves to an actor only when the device was valid for that
  actor at the operation's referenced frontier.
- Removal and key rotation fence future content, subject to lazy-revocation
  limits.
- Authorization receipts bind principal, historical authority, parent Manifest,
  active World implementation, demand, intent, and complete operations.
- Remote adoption invokes no World callback and cannot replace historical state
  with the receiver's current state.
- Manifest adoption is all-or-nothing; incomplete or corrupted roots do not
  partially advance the visible Replica.
- Unsupported legitimate protected material can be retained and forwarded
  without becoming readable or executable.
- A false Contact holdings declaration can starve only the claimant; complete-root
  validation prevents it from making an incomplete root valid. Strict rejection
  of every noncanonical holdings ordering/duplicate is still an implementation gap.
- Ceremony traffic cannot enlarge ordinary authority frontiers. Ordinary World
  transactions load no FROST shares or transcript state.
- A World cannot write another World, undeclared schema, or Body outside its
  bounded callback view.
- A derived cache cannot serve Bodies from a Manifest other than the exact root
  that keyed the projection.
- Malformed or corrupt input rejects or surfaces as typed corruption rather than
  becoming a valid value or panicking the Station.
- Content chunks are individually authenticated against a ciphertext Merkle root
  bound to the descriptor, so a provider that cannot decrypt them also cannot
  substitute them, and a partial transfer cannot be steered onto other bytes.
- Two ingests of identical plaintext produce unrelated content ids, so holding a
  guessable file does not let a peer confirm that someone else holds it.
- Local residency is not authority: caching, pinning, and evicting a chunk
  changes no committed root and grants no read that the keys did not already
  grant.
- Accepting a delivery-plane opening is idempotent, so a replayed opening
  allocates no second session and consumes no budget twice.
- A refusal on a delivery plane distinguishes only "unsupported generation" from
  everything else, so an unadmitted peer cannot probe a Space by reading errors.

- Answering a Freight request creates no Neighbor, no presence, and no Beacon
  observation. A fetch is not a heartbeat. Neighbor liveness is written only on
  the Contact and neighbor-presence ALPNs; a plane driver reads authority to
  admit and the content host to answer, and neither of those is a liveness
  record. A Freight refusal, timeout, reset, or lie therefore never promotes,
  demotes, or evicts Neighbor state, so no peer can move another peer's standing
  by transferring — or by failing to.
- Freight authorization is decided before residency is consulted. The serve
  demand is answered first, and a request that fails it never reaches the
  committed descriptor or the cache, so a refusal cannot be timed to reveal what
  is held.

## Explicit non-goals and residual risks

- **No clawback.** Removal cannot erase plaintext, snapshots, keys, or exported
  custody material already copied.
- **Endpoint compromise.** Malware running as the user can read local plaintext
  and keys and act with the device's current authority.
- **Trusted World compromise.** An authority-approved native World can select an
  insufficient demand or leak data available to its callback. Implementation-id
  activation is governance, not sandboxing.
- **Recovery compromise.** Possession of sufficient actor or Space recovery
  material may permit takeover within that recovery authority.
- **Traffic analysis.** Encryption does not hide endpoint addresses, timing,
  transfer sizes, Space participation, or all metadata.
- **Availability.** Peers can disappear, withhold data, lie about holdings, or
  refuse ceremony participation. Cryptography cannot guarantee an online quorum.
- **Clock truth.** Expiry uses bounded clock assumptions; display timestamps,
  activity ordering, and presence remain advisory.
- **Denial of service.** Bounds limit amplification but do not eliminate CPU,
  storage, or bandwidth exhaustion by authorized or reachable peers.
- **Native-loop preemption.** Runtime contains World panics but cannot preempt an
  arbitrary infinite loop in trusted native World code.
- **0.5-RTT is not authenticated.** Data the accepter sends before the client's
  handshake completes, and the client's own initial flight, are replayable by an
  interceptor. LAIT treats them as untrusted: nothing with an effect is
  dispatched on them, and nothing sent on them is a commitment.
- **Residency is observable.** Which chunks a peer will serve is metadata. A
  private, bounded availability answer narrows it but does not hide it, and
  timing still distinguishes a resident chunk from a fetched one.
- **Freight residency is Space-wide.** The serve predicate authorizes any
  admitted member. This is deliberate and it is a compromise: no member holds
  `content.serve` on day one, so a grant-gated plane would refuse universally,
  which is not caution but the feature not working. Until that grant is seeded,
  any per-resource read restriction on attached content is advisory against a
  member — a member can pull the ciphertext of content attached to a project
  they hold no read grant on. What they gain is the ciphertext itself, plus
  confirmation of the content's size, its chunk count, and the fact that this
  Station holds those bytes now; the ciphertext is the durable half of that,
  because bytes copied today become readable to whoever obtains that epoch key
  in a future compromise. What they do not gain is plaintext — the Body key
  still gates reading, and a provider serves without holding it — nor any way to
  name content whose id they did not already learn from durable state, nor any
  standing they did not already have. The migration is a `content.serve` grant
  slotting into the same `ContentPolicy` closure: no change to `ContentHost`, to
  the wire, or to the refusal a peer sees.
- **An availability answer is shape-indistinguishable, not time-indistinguishable.**
  Content this Station never heard of and content it holds no chunk of return the
  same empty answer, but the first costs no filesystem probes and the second
  costs one per named index. A member who obtained a content id out of band can
  therefore confirm by timing that this Space has committed that descriptor. It
  requires an id the caller already legitimately holds — a 32-byte blake3
  preimage is not guessable — and closing it properly needs a constant-cost
  residency index rather than padding.

- **Formal verification.** The protocols are tested and fault-injected, not
  formally verified.

## Key compromise cases

### Device key

A compromised valid device can sign as itself and exercise the historically
effective assignments of its actor. Revoke the device and rotate relevant Body
keys. Previously received content remains exposed.

### Actor recovery material

Actor recovery can replace the valid device set. Keep recovery material offline.
Loss may make recovery impossible; compromise may permit actor takeover.

### Space recovery authority and custody

Space recovery can replace broader recovery authority. Threshold arrangements
reduce dependence on one device but add DKG, resharing, custody, and transition
risk. Public ceremony material is replicated; secret shares and nonces remain
encrypted local state and must never enter Engine or product Bodies.

## Admission and delegation

Coordinates links may carry bearer admission capability. Anyone possessing a
valid unexpired capability can attempt its allowed redemption, subject to its
candidate binding, use policy, revocation state, and issuer authority. Send it
over an appropriately private channel.

Automatic redemption does not make a candidate authoritative before Mechanics
commits membership and exact expanded assignments. Product role provenance is
opaque to Mechanics; generic assignments and issuer delegation are verified.

## Contact, gossip, and relay exposure

Beacon, presence, and gossip are discovery hints, not authority. Contact binds
signed Station identities to the authenticated transport peer and validates all
authority-bearing material independently.

Holdings declarations reveal Body keys and transaction commitments already held
by the initiator to the contacted peer. They are metadata and may assist traffic
analysis. They do not contain plaintext and are bounded, signed, canonical, and
used only to omit redundant transfer.

A ciphertext-only relay or opaque peer may learn sizes, timing, identifiers,
and graph relationships. LAIT does not claim metadata-private replication.

The delivery planes add two more exposures of the same kind. A Freight request
names one content id and one chunk index, so a provider learns exactly which
content a peer is assembling and how far along it is; and a realtime session's
datagram cadence tracks a person's activity closely enough to infer presence
even though every payload is sealed. Both are traffic analysis against an
already-admitted peer, and neither is mitigated by encryption.

An opening is bounded and canonical, and is refused on length before it is
decoded. Refusals are deliberately coarse — a peer that is not admitted, not
authorized for a lane, or over budget receives the same answer — because the
alternative is an oracle for what a Space holds and who may reach it.

The refusal funnel is a confidentiality device, and its ordering is what makes it
one. `PROTOCOL.md` §12.3 gives the sequence; the property it buys is that a
refusal is never a statement about what is held. Authorization is decided before
residency is consulted, so a peer that may not serve is refused whether or not
the bytes are here, and a peer that may serve receives the same empty
availability answer for content this Station has never heard of as for content it
holds no chunk of. Absence and ignorance are the same answer on purpose; the
alternative is an oracle for what a Space contains, answerable by guessing
content ids.

It is not an anti-traffic-analysis device. It hides which of several reasons
produced a refusal; it does not hide the request, and the request is the exposure
recorded above. An availability question carries a bounded index set and a
resumed chunk request carries the leaf hash it left off at, so a provider reads a
peer's progress from the requests alone, whether it serves them or refuses them
all.

## Product conflict integrity

CRDT convergence is not authorization and is not automatically a correct
product conflict policy. IssuesWorld must preserve causally meaningful
concurrent intent through transition/revision heads where required. Using an LWW
register is an explicit acceptance of a single deterministic winner.

Comments, replies, reactions, and workflow transitions must bind stable
identities and historical authorization. An actor id inside unsigned application
content is not cryptographic proof of authorship; signed Body transactions and
receipts provide the attribution boundary.

## Peer-authored names on local paths

Product data authored by a peer is not a path. An attachment's display name is
the clearest case: it converges from whoever attached the file, it is what a
person naturally saves the file as, and the app's save affordance invites
exactly that — so untreated it is an arbitrary-path write of peer-supplied
bytes, triggered by a local user doing something that reads like a read.

The name is treated at both ends, differently, because the proposer differs. At
intake the Issues engine **refuses** a name that is over-long or carries control
characters: the proposer there is a local actor holding write authority who can
simply pick another. At save time the name is **repaired**, never refused, into
a single relative file-name component — the proposer there is remote, and
refusing would let a peer make their own attachment unsaveable by naming it
badly. An explicit `--out` is untouched: that path the caller typed.

Repair is what the far end owes regardless of intake, because a Body reaching
this machine through convergence never passed local intake at all. Intake bounds
what this Space publishes; it is not a defense, and is not relied on as one.

## Transient state and reliable signals

The Live plane carries what people are doing and the signal lane carries
one-message events. Neither is durable, and the interesting exposures are not
about confidentiality of the payloads — a cursor position is not a secret — but
about what a peer can *learn* or *make happen* by sending them.

**A signal is authenticated by the connection and never signed.** It cannot be
forwarded and cannot be retained as evidence: there is nothing to show a third
party, and a Station repeating one is making a claim on its own authority. That
is intentional. A signed signal would be a durable artefact by another name, and
the whole contract here is that nothing is retained.

**A World signal is trusted only when this build's reviewed implementation is
active at the session's pinned frontier.** A World the Station does not host and
a World whose implementation nobody approved get the same refusal, and neither
reveals the other. Acting on a payload whose schema was never reviewed is the
failure this prevents.

**Delivery failure is not observable to the sender.** Zero local listeners, a
lagged local ring, and a full offer queue all leave the wire outcome identical.
Otherwise a peer could learn whether a viewer is open, or whether anyone is using
a Station at all, by sending an attention signal and watching what comes back.

**Residency hints answer in three states and never a chunk list.** A complete
bitmap would let a peer reconstruct which parts of a file somebody had opened,
which is a read-activity oracle over content the peer may legitimately hold. The
hint is keyed by the full content id: a prefix would let a peer probe for
holdings without knowing an id, which is weaker than Freight's exact
availability question. A peer that did not negotiate the capability may neither
receive hints nor publish them.

**A peer-supplied anchor path reaches the collaborative document's container
namespace.** It is bounded independently of the item, and it must equal the
field the peer already subscribed to — so a peer can only ask about what it said
it was watching. Without that binding, an anchor would let a peer name arbitrary
root containers on a receiver. This is the sharpest edge on the plane, because
it is the one place remote bytes reach a data structure rather than a decoder.

**A file offer starts nothing.** Receiving one writes no byte and resolves no
path, so a member cannot spend a Station's disk by sending a message. Automatic
acceptance requires the sender to resolve to one of the receiving identity's own
devices, an explicit local opt-in that defaults to off, and an explicitly
resolvable destination. The first gate is not widened by the second: a Station
that opted in still refuses a stranger.

**A caret resolve takes the same lock a commit takes.** `RwLock` is not
expressible here — the collaborative document underneath is not `Sync` — so
concurrent readers do not exist for anchors or for anything else. What bounds
the damage is the rate, not the access pattern: the per-connection datagram gate
and the session ceiling together bound resolutions to a few thousand a second
against a commit measured in milliseconds. That is a small duty cycle rather
than a safe one, and it is the number to re-derive if either ceiling moves.

**Presence is a gate on delivery, which makes it worth lying about.** A signal
goes to a peer that currently holds a session, so a peer that could fake presence
could pull other people's nudges toward itself. It cannot: presence for delivery
is the set of Stations this node holds a session with, established by the
transport's own authentication and the admission that followed it, and never a
claim carried in a message. A peer that says it is somebody else is refused before
it has a session at all.

**A nudge names durable material and never carries it.** Losing one costs
timeliness. That is deliberate — an alert that carried the fact would be a second
copy of it on a plane that keeps nothing, and would then have to be as trustworthy
as the record, which nothing here signs.

**A World's declared surface is enforced at delivery, not only reviewed at
registration.** A scope naming an undeclared schema is refused before it takes a
slot, and a signal past its World's declared ceiling is refused before its payload
reaches the World. Reviewing a declaration and then not enforcing it is the shape
of a reviewed identity that means nothing — the descriptor would move an
implementation id, every peer would see a different build, and a peer could still
send whatever it liked.

**Awareness is allowed to be incomplete; durable convergence is not.** Over the
session ceiling, or after a gate drop, the view reports itself partial. A
surface that can be incomplete and does not say so is worse than one that is
plainly unavailable.

## Local web surface

The head bare `lait` serves binds to loopback and uses a per-run bearer
capability with origin and rebinding defenses. Listing local Spaces must not
activate all Stations. Attaching to a Space preserves the selected local
identity. The browser is a local client, not an iroh peer or Space member. The
full posture is in [`SERVE.md`](./SERVE.md).

Because it is now the *only* general interface, two consequences are explicit.
The token this head mints stands for the identity a terminal would have acted
as, and the host plane's reach matches: a caller holding it can create a store
directory anywhere this process can write. And a write that would have to be
signed with a key this daemon merely hosts — an agent-held Orbit — is refused
before signing, on custody grounds rather than on standing. Mechanics would
approve that write, correctly, because it evaluates the signer's grants and the
signer would be the agent; the head is the only place the custody question is
asked, so the answer must stay no however wide anybody's grants become. Reads
are never refused: observing signs nothing.

### Files on the local web surface

One origin serves the viewer, the API, and every attachment. That origin holds
the session credential, so a stored attachment rendered inline would run there —
which is why nothing on the content routes is ever rendered: always
`application/octet-stream` whatever the stored MIME type says, always `nosniff`,
always `Content-Security-Policy: sandbox; default-src 'none'`, and always a
`Content-Disposition: attachment`. The stored MIME type is a peer's claim about
a peer's bytes and is not honoured.

The filename in that header passes two different escapes: the shared sanitiser
reduces a peer-authored name to one relative component, and the result is then
percent-encoded into the `filename*` form. A header is a line, and a name that
could end it early would inject the next one.

Content routes refuse a `?token=` credential. A download URL is pasted, put in a
`src`, and left in browser history — so a live token on one is a live token in
the URL bar, in devtools, in the download list, and in whatever the dev proxy
logs. The refusal is a property of the route, read from the same table the
router is built from, and an unregistered path defaults to refusing.

The WebSocket upgrade is the one place an absent `Origin` is not benign. A
handshake is exempt from CORS: the browser sends it cross-origin with no
preflight and attaches the cookie, so `check_upgrade_origin` requires an Origin
where the shared gate admits its absence.

Per-resource read restriction on attached content is still advisory, and this
surface does not change that. A member may read any content their Station
holds — the same Space-wide residency gap Freight carries, closed by the same
`content.serve` grant when it exists.

## Security maintenance

Security claims require executable tests at the enforcing boundary. Protocol
changes require malformed encodings, wrong-domain/Space/peer substitution,
historical-authority cases, concurrency permutations, restart/fault points,
resource bounds, and missing-key behavior.

Independent review should prioritize historical authority/checkpoints,
authorization receipt composition, Manifest/journal recovery, protected Body
encryption, admission/delegation, Contact state machines and holdings metadata,
World containment, FROST/DKG/resharing, custody, and recovery transitions.
