# Correspondence — the Spec set

Staging artifact. Content for `spec_new` / `baseline_new` / `issue_baseline`,
pending a route to the `issues_spec_*` surface on the workspace holding CORR.

Model per `docs/SPECS.md`. An Issue says what work is happening; a Spec says what
that work is meant to satisfy. The 13 CORR issues stay as work — this replaces
the durable truth I wrongly encoded as issue bodies.

Lifecycle for all: `draft → review → issued`. Nothing below is issued yet, and
the requirements that depend on **REC-D1** must not be issued before it lands.

---

## Goal

### G-1 · `goal` · Every actor holds durable correspondence identity

A Space is the cryptographic and replication boundary
(`docs/ARCHITECTURE.md`), and today essentially nothing crosses it: every Body
traces to a signed member, and the one artifact that leaves — an invite ticket —
travels out-of-band by copy-paste.

The intent is that every actor in a Space, human and sponsored agent alike,
holds a durable mailbox: addressable without a registry, private within a shared
Space, surviving device rotation, and outliving the actor itself.

Mail is the first contractor on that plane, not the plane.

Links: none. Goals establish intent.

---

## Requirements

### R-1 · `requirement` · A mailbox is confidential within its own Space

Membership of a Space must not confer readership of a member's correspondence.
Mailbox contents are sealed to the owning actor's devices, never to the Space
epoch key.

Rotation of an actor's device set must not require re-encrypting mail already
held.

`governs` → CORR-7 (Step 1 — mailbox primitive)

### R-2 · `requirement` · Correspondence outlives its actor

A sponsored agent dies with its sponsor via the remove-wins cascade; humans
leave through membership removal. Correspondence with third parties is a record
and cannot be tombstoned by either event.

Mailbox lifetime is therefore not actor lifetime. The terminal disposition is
whatever REC-D2 records.

`incorporates` → REC-D2 (when issued)
`governs` → CORR-7

### R-3 · `requirement` · Ingress carries provenance and never becomes authority

An inbound message is the first attacker-chosen, unauthenticated material to
become a Body. `From`, `Return-Path` and DKIM alignment are recorded as
provenance and are never trusted as identity.

Every surface above the plane must be able to render the difference between
"signed by a member" and "asserted by a stranger". Provenance is a field, not a
boolean, and distinguishes received from imported.

`governs` → CORR-8 (Step 2 — ingress quarantine), CORR-11 (Step 5 — import)

### R-4 · `requirement` · Egress never spends another actor's key

`serve::borrowed_key_refusal` already asks the right question — whose key is
about to be spent, not whether the act is permitted. Outbound mail is that
question with no recall.

No send path may exist before its gate. This is the primary property of the
plane for human actors, not a guardrail added for agents.

`governs` → CORR-9 (Step 3 — egress custody gate)

### R-5 · `requirement` · External addressing preserves per-space unlinkability

`crates/mechanics/src/actor.rs:24` — actors are per-space, and the same human in
two Spaces is two unlinkable actors. An email address is a global, permanently
correlatable name, and mail carries linkage outside where it cannot be retracted.

**Blocked on REC-D1.** Do not issue this requirement before that record exists;
its text is determined by the decision.

`incorporates` → REC-D1 (when issued)

### R-6 · `requirement` · Groups that cross the boundary use MLS; the kernel does not

Correspondence with parties outside the Space uses MLS (RFC 9420) for group key
agreement. The kernel's own epoch machinery is unchanged and MLS is never
introduced inside a Space.

The reason is a consistency-model mismatch, not a strength comparison. RFC 9750:
"the members of the group must agree on a single MLS Commit message that ends
each epoch and begins the next one" — MLS assumes a Delivery Service that
linearises commits, and its eventually-consistent mode requires clients to pause
sending or retain rollback state. `crates/mechanics/src/acl.rs:17` is the
opposite design: replicas at different sync points converge, replay is
deterministic under topo order and remove-wins, and concurrent operations
coexist. MLS resolves concurrency by electing one winner; lait resolves it by
converging. A sequencer inside a Space would undo the architecture.

Outside the boundary the objection disappears: MIMI rooms are hub-and-spoke, so
the ordering MLS wants is a property that route already has.

MLS also explicitly disclaims authorization, membership policy and recovery
(RFC 9750) — which is `acl.rs`, `authority.rs`, `custody.rs`, `recovery.rs`,
`ceremony.rs` and the FROST apparatus. Adopting MLS replaces the epoch-key
sealing in `ledger.rs` and nothing else.

`incorporates` → REC-D5 (when issued)
`governs` → CORR-10 (Step 4 — contractor seam)

---

## Designs

### D-1 · `design` · Correspondence is a substrate plane with a contractor seam

```
mechanics       legitimacy — identity, authority, custody (+ egress gate)
fabric          the shared world — Loro sealed
correspondence  Bodies crossing the Space boundary            [new]
  └─ contractor: the ONLY crate naming a mail protocol
comms           bytes between replicas — iroh sealed behind Transport
products/       issues · a mail client · both merely callers
```

The contractor seam copies `crates/comms` — "the only crate that names a
concrete network" — with the root manifest's payoff: swapping the adapter is a
manifest change, not a daemon rewrite. Deliverability becomes a deployment
choice.

Precedent for the layering: `crates/world-interface/src/destination.rs` keeps a
peer-authored-name property in the substrate because "the product is one caller
and the property is about the function."

`incorporates` → R-3, R-4
`governs` → CORR-5, CORR-10

### D-2 · `design` · The mailbox primitive — actor-keyed, DEK-slot sealed

Provisioning is free: `ActorId = act_ + blake3(Incept)` is self-certifying
(`actor.rs:23`), and is content-independent of any device key (`ids.rs:268`), so
a mailbox keyed by actor survives device rotation.

Sealing reuses the pattern at `custody.rs:17` — one DEK encrypts the payload
once, each slot wraps that DEK, "adding an unlock path never re-encrypts the
secret." Substitute device-of-the-actor for slot; fan out over
`actor::devices_of` via `crypto::seal_to`.

`incorporates` → R-1, R-2
`governs` → CORR-7

### D-3 · `design` · Body schemas and the content plane

`replica::body::MutationModel` already carries the split:

| Body | Mutation | Rationale |
|---|---|---|
| `message` | `Atomic` | Received mail never mutates. 64 MiB bound. |
| `thread` | `Collaborative` | Order, labels, triage state, assignment. |
| `mailbox` | `Collaborative` | Thread refs + per-device sync cursors. |

Attachments are not Bodies — they take the content plane's descriptor/residency
split, so a replica can name a gigabyte without holding it. Filename, MIME and
disposition are product metadata per `content.rs:9`.

No plaintext-hash identity: both commitments are over ciphertext, deliberately,
so the catalog is not an equality oracle over messages held in common.

`incorporates` → R-1
`governs` → CORR-7, CORR-12

### D-4 · `design` · What is adopted at the boundary and what is kept

One rule explains every case examined: an external standard buys interoperability
by surrendering exactly the property the kernel exists to hold — self-certifying
identity, sequencer-free convergence, or per-space unlinkability. So the standard
wins where we are already talking to strangers and loses where we are not.

| Overlap | Candidate | Verdict |
|---|---|---|
| Group key agreement | MLS 9420 | boundary only — R-6 |
| Actor / device identity | MIMI URIs | keep; alias outward |
| Room model | MIMI hub-and-spoke | keep inside, adopt outward |
| Message semantics | draft-ietf-mimi-content | adopt, minus its message-ID |
| Recipient sealing | HPKE 9180 | adopt — see `hpke-sealing.md` |
| Mail protocol | Stalwart / JMAP | adopt outright |
| Blob store | iroh-blobs, IPFS | keep the content plane |
| MIME parsing | libraries | adopt libs, keep the quarantine policy |
| Full-text search | Tantivy | adopt as a local projection |
| Filtering rules | Sieve | adopt if wanted; Stalwart ships ManageSieve |
| Authorization / membership | MLS, MIMI | keep — both disclaim it |
| Threshold recovery | — | keep; no analogue exists |
| Convergence | Matrix event DAG | keep; needs homeservers |
| Agent messaging | A2A | keep the mailbox; A2A is RPC-shaped |

**MIMI identity, specifically.** Its `u/` vs `d/` split is the ActorId/DeviceId
distinction, which is a useful independent confirmation of the shape. The
properties are weaker: identifiers are DNS-scoped and provider-minted, the draft
lists Authentication as a known gap and admits users cannot yet cryptographically
tie an identity to its provider, portability across providers is unaddressed, and
the baseline exposes identifiers to the hub. `actor.rs:23` is self-certifying and
unforgeable. Adopting MIMI identity would trade a cryptographic identity for an
administrative one and reintroduce DNS as a naming authority. The MIMI URI is
therefore an **external alias** for an actor, exactly as an email address is.

**mimi-content, specifically.** Take the semantics — `inReplyTo` threading,
`replaces` for edits and deletes, reactions as a disposition, multipart
relationships, inline vs external attachments. Do **not** take its message-ID:
it derives from a hash of content, sender URI and room URI, which reintroduces
the plaintext equality oracle `content.rs:16` refuses. The draft also treats
expiration as a client-side hint and does not distinguish durable from ephemeral
delivery classes, so the retention class in D-3 fills a real gap rather than
duplicating one.

`incorporates` → R-5, R-6
`references` → D-1

---

## Plan

### P-1 · `plan` · Staging

1. Mailbox + sealing under `MemCorrespondence`, no network — CORR-7
2. Ingress quarantine against a hostile MIME fixture corpus — CORR-8
3. Egress custody gate, before any send path exists — CORR-9
4. Contractor adapter; `MemCorrespondence` built first — CORR-10
5. Import, which doubles as the measuring rig for REC-D3/REC-D4 — CORR-11
6. Search projection — CORR-12
7. Client product, last, so product concerns stay out of the plane — CORR-13

Steps 1–3 need no mail protocol code, which is what makes the spike cheap to
discard.

`incorporates` → D-1, D-2, D-3
`references` → G-1

---

## Guide

### GU-1 · `guide` · Testing posture

Explicitly non-enforcing. `MemCorrespondence` mirrors `comms::mem::MemTransport`,
which lets the real daemon run with no network; mail tests should need no SMTP
server. Ingress works against a fixture corpus rather than live mail.

Per `docs/THREAT-MODEL.md:402`, every security property added here needs an
executable test at the enforcing boundary — that part is not guidance and
belongs to R-3 and R-4.

`references` → P-1

---

## Records — written when the decisions land

These are the four open decisions. They are `record` Specs ("preserve decisions
and as-built facts"), authored at the point the decision is taken, and the work
of taking them stays as CORR-1..4.

- **REC-D1** ← CORR-1 · mailbox addressing vs per-space unlinkability. Gates R-5.
- **REC-D2** ← CORR-2 · mailbox disposition when its actor is evicted. Gates R-2.
- **REC-D3** ← CORR-3 · storage multiplier from no plaintext-hash identity.
- **REC-D4** ← CORR-4 · search as a local derived projection.
- **REC-D5** ← CORR-14 · MLS at the correspondence boundary, kernel unchanged.
  Gates R-6. Interacts with D1: MLS credentials assume a stable client identity
  and an authentication service, which pushes against per-space unlinkable
  actors. Take D1 and D5 together.

Note that the HPKE adoption (`hpke-sealing.md`, CORR-15) is deliberately **not**
a record here. It is a kernel change with no dependency on any decision in this
project and should not wait on one.

---

## Baseline

### B-1 · `baseline` · Correspondence v0 — Step 1

Members (exact issued revisions, once issued):
R-1, R-2, D-1, D-2, D-3, and REC-D1 / REC-D2.

Bound to CORR-7 via `issue_baseline`, so its Packet answers "what governs this
work now?" without reimplementing the graph rules.

Deliberately excludes R-3 and R-4 — they govern steps 2 and 3 and belong to a
later baseline.
