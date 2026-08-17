# Correspondence — the mailbox primitive

Status: **scoping**. Nothing here is built. Decisions in §4 gate everything else.

Local working note. Current product and protocol truth stays in the tracked
documentation set.

---

## 1. The claim

Mail is not a product package sibling to `products/issues`. The substrate-shaped
thing underneath it is **material crossing the Space boundary**, and the noun at
its centre is a **mailbox owned by an actor**.

`docs/ARCHITECTURE.md` opens with "A Space is the cryptographic and replication
boundary." Today essentially nothing crosses it: every Body traces to a signed
member, and the one artifact that does leave — an invite ticket — is delivered
out-of-band by copy-paste. Correspondence is the first principled crossing in
both directions.

The test that this belongs below products: the same plane serves issue
notifications, invite delivery, agent-to-agent messages, and a mail client. Build
`crates/mail` and none of that generalises.

Precedent already in the tree — `crates/world-interface/src/destination.rs`,
which sanitises peer-authored attachment names and explicitly does *not* live in
the product:

> Kept beside the sanitizer rather than in the product, because the product is
> one caller and the property is about the function.

## 2. Layering

```
mechanics      legitimacy — identity, authority, custody   (+ the egress gate)
fabric         the shared world — Loro sealed
correspondence Bodies crossing the Space boundary                    [new]
               egress: custody + addressing
               ingress: quarantine + provenance
               mailbox: actor-keyed, actor-sealed, durable
  └─ contractor: SMTP/IMAP/JMAP adapter — the ONLY crate naming a mail protocol
comms          how bytes move between replicas — iroh sealed behind Transport
products/      issues · a mail client · both merely callers
```

The contractor seam copies `crates/comms` deliberately. That crate is "the
replaceable mechanism that moves their bytes between peers" and "the only crate
that names a concrete network"; the root manifest states the payoff — *"Swapping
the adapter is a manifest change, not a daemon rewrite."* Correspondence takes
the same shape, with two consequences worth having:

- Self-hosted Stalwart vs. a hosted relay becomes a deployment choice rather than
  an architecture commitment. Deliverability stops being a design problem.
- A `MemCorrespondence` mirrors `comms::mem::MemTransport`, which today lets "the
  *real daemon* run with no network at all." Deterministic mail tests with no
  SMTP server anywhere. This is the only way the plane stays testable at the
  repo's current bar.

## 3. The mailbox primitive

### 3.1 Actor-keyed

`crates/mechanics/src/actor.rs:23` — `ActorId = act_ + blake3(Incept event)`,
self-certifying, "no registry mints it and none can forge it." Provisioning is
therefore free: an actor exists ⟹ its mailbox is addressable. No server-side
inbox id, no bearer token.

`crates/mechanics/src/ids.rs:268` — an `ActorId` is "content-independent of any
device key, so devices rotate under a stable" id. A mailbox keyed by actor
survives device rotation by construction. That is the durability property.

**For every actor, not only sponsored agents.** Humans are the primary case; the
agent case is the one that falls out for free. See §4 D3 and §8 for what the
human case costs.

### 3.2 Sealing — the DEK-slot pattern

A Space epoch key would make every mailbox readable by every member. Wrong for
mail. The fix already exists in a different application —
`crates/mechanics/src/custody.rs:17`:

> One random data-encryption key encrypts the payload once. Each slot wraps that
> DEK a different way, so adding an unlock path never re-encrypts the secret and
> never requires having all paths present at once.

Substitute *device of the actor* for *slot*:

- mailbox holds a DEK; mail is encrypted under it once
- DEK is wrapped per device via `crypto::seal_to` (`crypto.rs:208`, which targets
  `DeviceId`), fanned out over `actor::devices_of(actor)` (`actor.rs:389`)
- adding a device writes one more wrap and re-encrypts nothing

This is what makes a mailbox private *inside* a shared Space.

### 3.3 Schemas

`replica::body::MutationModel` (`crates/replica/src/body.rs:65`) already carries
the split, so this is a declaration rather than a convention:

| Body | Mutation | Why |
|---|---|---|
| `message` | `Atomic` | Received mail never mutates. One canonical value, no collaborative overhead. `MAX_BODY_BYTES` is 64 MiB (`protected.rs:24`) — a fat HTML message fits with room. |
| `thread` | `Collaborative` | `ListInsert` (stable element identity) for order; `SetAdd`/`SetRemove` for labels; `MapSet` for triage state; `RegisterSet` for assignment/snooze. Two devices triaging offline converge. |
| `mailbox` | `Collaborative` | Thread refs, plus per-device sync cursors — see the trap in §3.5. |

Attachments are **not** Bodies. They go to the content plane
(`crates/replica/src/content.rs:1`), whose descriptor/residency split is exactly
right: every replica carries identity, size, epoch, chunk geometry and a Merkle
root; the chunks are local policy. "A World can name a gigabyte without every
peer downloading a gigabyte." A phone syncs the descriptor for a 40 MB deck and
fetches chunks only on open. Chunks are 256 KiB, sized to fit Contact's 1 MiB
frame.

The same module states where filename/MIME/disposition belong — "product metadata
[that] live in a World Body; two names may reference one `ContentRef`" — which is
an email attachment described exactly.

### 3.4 No plaintext-hash identity

Both commitments are over ciphertext, deliberately:

- `body.rs:39` — `ContentCommitment` commits to ciphertext "so it is not an
  equality oracle over decrypted content."
- `content.rs:16` — "there is no plaintext-hash identity and so no equality
  oracle: two ingests of identical bytes produce different `ContentId`s."

This matters more for mail than for issues. Plaintext-hash identity would let
anyone holding the catalog learn *which messages two members have in common* — a
shared newsletter would produce identical digests. Do not content-address
messages by a hash of the message. The cost is stated in §4 D3.

### 3.5 Sync-cursor trap

One `RegisterSet` holding an IMAP `UIDVALIDITY` / JMAP state string will be
fought over by two devices syncing the same account, where both writers are
correct about themselves. Key it per device:
`MapSet { path: "cursors", key: <DeviceId>, .. }`. Convergence gives the union;
each device reads its own.

## 4. Open decisions — settle before code

### D1. Addressing vs. per-space unlinkability — **load-bearing**

`actor.rs:24`:

> the `Incept` payload binds the space id + a nonce, so actors are **per-space** —
> the same human in two spaces is two unlinkable actors (cross-space linking is a
> local address-book concern, never protocol state).

An email address is a global, public, permanently correlatable name. Binding one
to a per-space actor relinks what the protocol separates — and mail carries the
linkage *outside*, where it cannot be retracted: `Message-ID`, `References`
chains, DKIM `d=`, envelope sender.

- **(a) One mailbox per (actor, space).** Work and side-project correspondence
  identities are cryptographically unlinkable. Compartmentalisation as a feature.
- **(b) One human-level mailbox across Spaces.** Deletes the unlinkability
  invariant.

Recommendation: **(a)**, recorded as a decision in the threat model rather than
discovered as fallout. Note the friction is worst in the human case — a person
has one social identity in a way an agent does not, and (a) means multiple
addresses.

### D2. Mailbox lifetime ≠ actor lifetime

A sponsored agent "dies with the sponsor" via the remove-wins cascade
(`docs/AGENT-EXPERIENCE.md`). Humans leave a Space through membership removal.
Correspondence with third parties is a record — contractual, financial, sometimes
discoverable. **An inbox cannot be tombstoned because its actor was evicted.**

Options: mail escheats to the sponsor / an admin on eviction, or a mailbox has an
archived terminal state the cascade cannot touch. Decide before provisioning
ships — retrofitting retention onto a remove-wins cascade is miserable.

### D3. Storage multiplier

No plaintext-hash identity (§3.4) means **no dedup**. The same message ingested
into three mailboxes is stored three times; a mailing list across a team is
stored per member. Noise for issues, a real multiplier for human mail at decade
scale. Size it before committing. If it is unacceptable the answer is *not* to
weaken §3.4 — it is convergent encryption scoped to a single mailbox, which
leaks only to its own owner.

### D4. Search projection scope

`crates/replica/src/index.rs` is a canonical authenticated radix trie — exact-key
lookup and set commitments over 32-byte hashed keys the crate never sees in
plaintext. It cannot answer `from:alice subject:invoice after:2025-01`.

Proposal: full-text search is a **local derived projection** (Tantivy) over
decrypted Bodies — rebuildable, never synced, never signed. A replicated search
index over E2EE content is a research problem; a local one is a Tuesday. Confirm
that per-device rebuild cost is acceptable at human-mailbox scale.

## 5. Ingress — the quarantine boundary

The reason this must be substrate. `docs/THREAT-MODEL.md:26` already lists
replicated bytes, display names, clocks, routes and network paths as untrusted —
but all of it is **member-authored**. An inbound message is the first
attacker-chosen, unauthenticated material to become a Body.

Foreign material must be quarantined before it is a Body: MIME nesting depth and
part count bounds, HTML sanitisation, remote-image suppression by default,
`From` / `Return-Path` / DKIM alignment recorded as *provenance* rather than
trusted as identity, unicode homograph normalisation on display names, and
`destination.rs`-class filename handling for every attachment.

`destination.rs` is one instance of this class that already cost a regression
(its own `attachment_regression` test records it). No product should be trusted
to remember the other twelve.

Provenance is a first-class field, not a boolean: a Body must carry *how* it
arrived and *what was verifiable about it*, and every surface above must be able
to render the difference between "signed by a member" and "asserted by a
stranger."

## 6. Egress — the custody gate

`serve::borrowed_key_refusal` already asks the right question — "whose key is
about to be spent", not "is this act permitted". Outbound mail is that question
with no recall: an agent sending as a human is unrecoverable in a way a
mis-signed issue comment is not.

The gate belongs beside the existing predicate in mechanics, not reimplemented
per product. For humans this is the *primary* property of the plane, not a
guardrail on agents.

## 7. Import

A human mailbox does not start empty. Ingest from `.mbox` / Google Takeout is a
first-class requirement, not a migration afterthought:

- idempotent on re-run, keyed on `Message-ID`
- attachments split to the content plane on the way in
- resumable — a 15-year archive will not import in one pass
- provenance marked as *imported*, distinct from *received* (§5)

## 8. Threat-model amendments required

`docs/THREAT-MODEL.md` currently scopes every adversary in "Adversaries
considered" (`:38`) to someone who is or was a member. Ingress introduces an
adversary who was never admitted and never will be. Required before code:

- Trust boundaries (`:18`) — the Space boundary as a crossing point
- Adversaries (`:38`) — the never-admitted correspondent
- Intended properties (`:54`) — mailbox confidentiality within a shared Space;
  the D1 addressing decision
- A section for ingress quarantine, alongside "Peer-authored names on local
  paths" (`:250`)

`:402` — "Security claims require executable tests at the enforcing boundary."
Every property added here needs one.

## 9. Staging

1. **Mailbox + sealing, no network.** DEK-slot per device, actor-keyed, the three
   schemas. `MemCorrespondence` only. Proves confidentiality-within-a-Space and
   device rotation without any protocol work.
2. **Ingress quarantine against fixtures.** Hostile MIME corpus, no live mail.
3. **Egress custody gate.** Refusal proven at the boundary before anything can
   actually send.
4. **Contractor adapter.** SMTP/JMAP behind the seam; Stalwart underneath.
5. **Import**, then **search projection**.
6. **Client product** last — it is convenience, and building it early will drag
   product concerns into the plane.

Steps 1–3 need no mail protocol code at all, which is what makes the spike cheap
to throw away if the shape is wrong.

## 10. What this is not

- Not a mail server. Deliverability lives with the contractor.
- Not a replicated search index.
- Not agent-specific. Every actor has a mailbox; agents are the case that costs
  nothing extra.
- Not a `products/mail` package. The client on top may well be one.
