# The credential surface — passport, warrant, disclosure

Status: **blueprint**. No production code. Every claim below cites the tree at
the commit it was written against.

Local working note. Current product and protocol truth stays in the tracked
documentation set.

Scope: the *artifacts* — what an actor carries out of a Space and what a
counterparty can check. Out of scope by assignment: the addressbook/naming
layers, the sponsorship relation's ACL mechanics, mailbox Body schemas, and the
correspondence crate layout.

---

## 0. The one-paragraph version

lait mints authority evidence internally and calls it a receipt. That receipt is
the *shape* this design borrows and the artifact it must never export: it is
unsigned, derives its authenticity from history a stranger does not hold, and
every field it binds is an internal coordinate. So authority that travels needs
two new documents. A **passport** establishes standing — self-certifying
identity, who vouches, what the subject *can* do, until when — and carries no
purpose and no destination. A **warrant** authorizes one crossing — audience,
purpose, scope, budget, expiry — and may be *narrowed* by its holder but never
widened, which is what makes handing one to a sub-agent safe by construction
rather than by policy. Neither ever accumulates a record of where it has been.

---

## 1. What exists (a)

### 1.1 `AuthorizationReceipt` — internal, and must stay internal

Defined `crates/mechanics/src/demand.rs:489-520`, re-exported as
`mechanics::authorization::AuthorizationReceipt` at
`crates/mechanics/src/authorization.rs:7-12`. Sole real constructor:
`Authority::authorize`, `crates/mechanics/src/ledger.rs:1288-1343`.

Thirteen bound fields, grouped by what they are *for*:

| Group | Fields | What it binds |
|---|---|---|
| Principal | `space`, `world`, `actor`, `device` | who, resolved at the pinned frontier (`ledger.rs:1304-1316`) |
| Historical position | `authority_frontier`, `authority_checkpoint_commitment` | the exact replayed authority state (`ledger.rs:1332-1333`) |
| Policy | `policy_evidence_digest`, `demand_digest` | the demand and the *exact effective-grant witness set* that satisfied it (`demand.rs:472-479`) |
| Content position | `parent_manifest_root`, `implementation_id` | the Manifest root and the authority-approved World implementation |
| The one act | `intent_digest`, `effect_operations_digest`, `body_transaction_core_digest` | one specific transaction, and no other |
| Verdict | `decision: u8` | always `1`; "a denial is a typed result, never a receipt" (`demand.rs:518-519`, enforced on decode at `demand.rs:540-542`) |

**Presentable outside the Space today: no.** Four independent reasons, and each
one alone is disqualifying.

1. **It carries no signature of its own.** `demand.rs:481-487` states it plainly
   — "It is **not** an actor assertion or a separately signed token: authenticity
   comes from the signed authority history it is derived from plus the outer
   transaction signature that carries it." Strip the transaction and you hold
   bytes anyone can type.
2. **Verification requires the ledger.** `Authority::verify_receipt`
   (`ledger.rs:1350-1419`) re-materializes the checkpoint, re-resolves the
   device→actor binding at the frontier, re-evaluates the demand, and recomputes
   the witness digest. Every one of those needs the replayed signed history. An
   outsider has none of it, and `checkpoint_for` on a frontier you do not hold is
   `Failure::MissingHistory` (`ledger.rs:1440-1443`), not a soft answer.
3. **It authorizes a past act, not a future one.** `body_transaction_core_digest`
   binds one transaction core. There is no field a counterparty could read as
   "and therefore may do X here".
4. **Exporting it would leak the Space, not just the subject.** `authority_frontier`
   is a literal list of DAG head hashes and `authority_checkpoint_commitment`
   fingerprints the materialized state at that position. Two receipts shown to
   the same counterparty at different times reveal how much authority history
   accumulated in between — an activity oracle over *every member*, not only the
   presenter. This is the same class as the residency bitmap
   `docs/THREAT-MODEL.md:296` refuses ("A complete bitmap would let a peer
   reconstruct which parts of a file somebody had opened, which is a
   read-activity oracle over content the peer may legitimately hold").

**One live trap worth recording.** The default `AuthorityView::authorize_mutation`
(`crates/runtime/src/world.rs:184-226`) builds a *structurally valid* receipt
with `authority_checkpoint_commitment: [0u8; 32]` and
`policy_evidence_digest(&[])` — an empty witness, no policy evaluation. It is a
fixture path and is harmless today precisely because every receipt is
re-verified against real history by `verify_receipt`. The moment a receipt
becomes presentable to someone who cannot re-verify, that default is a forgery
oracle. Fifth reason not to export it.

### 1.2 `RequestReceipt` — an idempotency cache, not evidence

`crates/replica/src/receipt.rs:29-51`, keyed on `(Space, World, Device,
RequestId)` via `scope_key` (`receipt.rs:106-117`). Binds `payload_hash` to
discriminate identical replay from conflicting reuse, plus the application
`effect` bytes (≤ 1 MiB, `receipt.rs:27`), touched `bodies`, committed
`frontier`, and `transaction`.

**Presentable outside: no, and it is not even close.** It is unsigned, it is
keyed by *device* rather than actor, and it carries the application effect in
the clear. It answers "did I already do this" for one node's own store.

### 1.3 `AuthorityView` — the seam that has no outside

`crates/runtime/src/world.rs:136-243`. `resolve` / `admit_peer` /
`admit_contact_peer` (`:139-158`), `active_implementation` (`:170-176`),
`authorize_mutation` (`:184-226`), `evaluate_read` (`:235-242`). Every method
takes a `DeviceId`, a `station::Key`, or an `ActorId` and answers from replayed
Space history. There is no method whose input is an artifact a stranger
presented, and adding one is not this slice's job — see §3.3.

Note `admit_contact_peer` (`:156-158`): "Implementations may recognize a
narrower bootstrap standing (for example, possession of an unredeemed approach
coordinate) before ordinary membership exists." That is the existing hook shape
for "someone with a document but no history", and the warrant redemption path
in §3.3 is its sibling, not a replacement.

### 1.4 The precedent nobody named: `AdmissionCapability`

`crates/runtime/src/coordinates.rs:158-182`, carried inside `SignedCoordinates`
(`:200-208`). **This is the only artifact in the tree today that is presentable
outside the Space**, and it is already three-quarters of a warrant:

- **separately signed**, domain `lait/admission/1` (`coordinates.rs:37`), over a
  length-framed preimage (`coordinates.rs:265-272`) — the anti-ambiguity
  discipline used everywhere here;
- **space-bound** (`space: [u8; SPACE_ID_LEN]`) and **issuer-bound**;
- **time-windowed**: `issued_at` / `not_before` / `expires_at`;
- **use-capped**: `AdmissionUsePolicy::{SingleUse, Reusable{max_redemptions}}`
  bounded 2..=1024 (`coordinates.rs:132-156`);
- **revocable by nonce**: the `nonce` field is documented as "The capability id /
  revocation key" (`coordinates.rs:170-171`), and `AclAction::RevokeInvite {
  nonce }` (`crates/mechanics/src/acl.rs:196-198`) is the convergent kill switch;
- **carries generic scope**: `WorldAssignmentEvidence` (`demand.rs:557-616`) —
  the exact `(PolicyCapability, Resource)` assignments redemption installs, with
  a `validate` that refuses anything outside the declared World save one
  Mechanics-owned exception (`demand.rs:598-613`).

And it already states the doctrine this whole design rests on
(`coordinates.rs:19-22`):

> possession only authorizes a *request* — standing exists only after mechanics
> validates incorporated authority material at redemption.

Most importantly, look at **where the redemption count lives**: the ticket
carries the *cap*, and the count lives in the redeeming Space's replayed history
(`acl.rs:545` — "count for an admission capability's reuse cap"). The artifact
does not accumulate its own usage record. That is the no-travel-history rule,
already implemented once, and §6 makes it binding on warrants.

### 1.5 Primitives already in the tree

- `sigdag::sign_message` / `verify_message` (`crates/mechanics/src/sigdag.rs:164-195`)
  — detached ed25519 with **domain separation** and a topic binding. "lait's own
  primitive — no scaffold signing type involved."
- `ActorId = act_ + blake3(Incept)` (`crates/mechanics/src/actor.rs:22-27`,
  `ids.rs:268-272`), self-certifying, per-space, "content-independent of any
  device key, so devices rotate under a stable" id.
- `crypto::did_key_from_pubkey` — `did:key:z6Mk…` on every `MemberDto.did`
  (`src/dto.rs:30-34`) and `WhoamiDto.did` (`src/dto.rs:60-62`).
- `demand::{PolicyCapability, Resource}` (`demand.rs:126-211`) with a frozen
  canonical encoding, a byte-exact round-trip decoder (`demand.rs:336-351`), and
  hard bounds (`demand.rs:38-52`).
- `wallclock::now_secs()` (`crates/mechanics/src/wallclock.rs:65-67`) with a
  test-only freeze seam — the one time source for records.

**Nothing new needs inventing at the crypto layer.** What is missing is a
vocabulary and two envelopes.

---

## 2. The passport (b)

Durable, general, rarely reissued. It answers *who I am, who vouches for me,
what I am capable of, until when*. It answers nothing about where it is going.

### 2.1 Identity — already stronger than a real passport

A real passport needs a trusted issuer; a forged one is a forged issuer
signature. `ActorId` needs none: it *is* `blake3` of its own inception event, so
"any replica holding the `Incept` event validates the id by rehashing"
(`actor.rs:24-26`). The subject field of a passport is therefore not a claim at
all — it is a hash whose preimage the subject can produce on demand.

Device binding rides the existing consent machinery: every device in an actor's
set signed `lait/devbind/1` over a nonce-bearing context (`actor.rs:29-41`), and
rotation/recovery is self-authorized (`actor.rs:6-12`, `ActorOp::Recover` at
`actor.rs:166-171`). A passport signed by device D stays valid over the *actor*
after D rotates only if the verifier can re-resolve the actor's current device
set — which it cannot, from outside. So: **a passport is signed by one device
and reissued on rotation.** Durable in the sense of "months, not per-crossing",
not in the sense of "never".

### 2.2 Vouching — the sponsorship relation, projected

`AclAction::AddAgent { actor, grants }` (`acl.rs:159-172`); the sponsor is the
op's `by` actor; `AclState::sponsor_of` (`acl.rs:643`); the cascade that evicts
an agent whose sponsor left (`acl.rs:1854-1860`). The viewer already renders it
as information, not a gate (`viewer/src/ui/Members.tsx:136-142`).

A passport carries a `Vouch` — **not the ACL op**. Exporting the op would export
its DAG parents, its `actor_asof` frontier, and the sponsor's causal position,
which is §1.1's leak by another route. A `Vouch` is a fresh detached signature
by the sponsor's device under `lait/vouch/1` over
`(sponsor_actor, subject_actor, kind, issued_at, expires_at, nonce)` — and
nothing else. `kind` is a bounded name (`sponsor`, `colleague`, `employer`), not
a sentence.

### 2.3 Validity — and what the passport deliberately omits

Grants (`Standing`, `PolicyCapability` assignments) are evaluated by replaying
history at a causal position (`ARCHITECTURE.md` §3: "Authority evaluation is
historical"). Outside the Space there is no position to evaluate at. Therefore:

> **A passport carries no grants.** Authority is not portable; capability is.
> "What I may do" is the warrant's job (§3), and it is re-floored at redemption.

Validity in a passport is a plain wallclock window — `not_before` /
`expires_at`, seconds, from `wallclock::now_secs()`. A verifier's clock is a
verifier's claim; that asymmetry is accepted and is why the window is generous
and the warrant's is short.

### 2.4 The missing part: the capability descriptor

Grants say what an actor **may** do (authority, Space-relative, replayed).
Nothing anywhere says what an actor **can** do (capability, portable, asserted).
That gap is the whole reason a passport is not just an ActorId.

```rust
/// A capability name. Same grammar as a PolicyCapability, deliberately a
/// DISTINCT type so nothing can pass one where the other is meant.
pub struct Skill {
    pub domain: String,   // valid_name(): 1..=64 bytes of [a-z0-9._-]
    pub name: String,     // ditto
}
// e.g. Skill{"lang","rust"} · Skill{"vcs","git"} · Skill{"review","code"}
//      Skill{"build","cargo"} · Skill{"mcp","tool"}

pub enum Modality {
    /// The subject says so. Free, and worth exactly that.
    Declared,
    /// A named actor signed for it. Carries THAT actor and nothing else.
    Attested { by: ActorId, issued_at: u64, expires_at: u64,
               nonce: [u8; 16], signature: [u8; 64] },
    /// A digest of an in-Space artifact. Opaque outside by construction —
    /// the counterparty learns a hash, never what it hashes.
    Demonstrated { evidence_digest: [u8; 32] },
}

pub struct Claim { pub skill: Skill, pub modality: Modality }
```

Bounds, mirroring `demand.rs:38-52` rather than inventing new numbers: ≤ 64
claims per descriptor, ≤ 64 bytes per identifier, canonical postcard with sorted
+ deduplicated claims and decode/re-encode byte equality, digest via
`blake3::derive_key("lait.capability-descriptor.v1", …)`.

**The hard rule on attestations, and it is the sharpest edge in the slice:** an
`Attested` modality names the attestor, the skill, and a time window. It names
**no engagement, no space, no third party, and no place**. "Alice attested that
Bob can review Rust" is a capability claim; "Alice attested, during the Acme
engagement, that Bob can review Rust" is travel history wearing a capability
costume. See §6.

### 2.5 Capability attaches to the actor; availability attaches to the device

This mirrors the content plane exactly, and the citation is worth having because
it is the same reasoning one layer up. `ContentStatus::resident_chunks`
(`crates/runtime/src/content_host.rs:101-103`):

> How many chunks are here right now. Local, momentary, and never replicated —
> residency is not a property of the content.

So:

| | Capability | Availability |
|---|---|---|
| Attaches to | the **actor** | the **device** |
| Lifetime | durable, survives device rotation (`ids.rs:268-272`) | momentary |
| Travels | yes, in the passport | **never** |
| Replicated | it is a signed assertion | no |
| Answered by | reading the passport | asking, live, per-counterparty |

A phone cannot run a build. The actor's passport says `build.cargo`; the phone
answers `Elsewhere`; the laptop answers `Ready`. Putting availability in the
passport would make the passport a device inventory that goes stale on issue —
the same failure mode as baking purpose into it.

**Availability answers in three states and never a capacity number**, directly
copying `THREAT-MODEL.md:296` and `MAX_RESIDENCY_CHUNKS`
(`crates/runtime/src/transient.rs:327-329` — "A hint is a suggestion about who
to ask, so a complete bitmap is not what it is for"):

```rust
pub enum Availability { Ready, Elsewhere, Unavailable }
```

No queue depth, no load average, no machine name, no "busy until". Each of those
is an activity oracle over the operator's day. The seam is a
`SkillAvailabilityOracle` trait sited beside `plane::live::ResidencyOracle`
(`crates/runtime/src/plane/live.rs:281, 362-364`), answered on the transient
plane, never durable, never signed — a signed availability answer "would be a
durable artefact by another name" (`THREAT-MODEL.md:279-281`).

### 2.6 The envelope

```rust
pub const PASSPORT_DOMAIN: &[u8] = b"lait/passport/1";

pub struct Passport {
    pub version: u8,                 // exactly 1; unknown REJECTS, never negotiates
    pub subject: ActorId,            // act_… — self-certifying, verifiable by rehash
    pub subject_device: [u8; 32],    // the device that signed this passport
    pub issued_at: u64,
    pub not_before: u64,
    pub expires_at: u64,
    pub nonce: [u8; 16],             // capability id / revocation key (AdmissionCapability shape)
    pub capability_root: [u8; 32],   // commitment over the SALTED claim set (§4)
    pub vouch_root: [u8; 32],        // commitment over the SALTED vouch set (§4)
    pub signature_algorithm: u8,     // SIG_ALG_ED25519; anything else REJECTS
    pub signature: [u8; 64],
}
```

**Absent on purpose, each with its reason:**

| Absent | Why |
|---|---|
| `space` | origin is *legitimate but selectively disclosable* (§4.4), never stamped in |
| `purpose` / `destination` / `audience` | the warrant's job; in a passport it is stale on issue |
| `grants`, `Standing`, `authority_frontier`, `checkpoint_commitment` | Space-internal and unverifiable outside; §1.1 |
| any counterparty, engagement, or presentation record | §6 |
| a presentation counter or `last_used` | §6, and the `AdmissionCapability` precedent (`acl.rs:545`) puts the count in the *verifier's* history |
| device availability | §2.5 |

**The topic-slot decision.** `sigdag::sign_message(domain, space_id, seed, msg)`
binds a topic into the preimage so "a message signed for one topic fails
verification on another" (`sigdag.rs:144-149`). A passport must verify
*anywhere*, so its topic slot is the empty string. That is a real weakening and
it has a named consequence: **an intercepted passport replays**. It is
acceptable only because a passport authorizes nothing — it establishes standing,
and every act needs a warrant, which *is* audience-bound and holder-proved
(§3.2). Presentation ≠ standing, exactly as `coordinates.rs:19-22` already says.

### 2.7 The honest residual

Two counterparties who both hold the same passport and compare notes can link
their sessions: same `nonce`, same signature bytes. This is inherent to a
document that is durable and general. Three mitigations, in order of preference:

1. The frequent crossing is the warrant, which is per-audience and short-lived.
2. Per-audience reissue is *permitted and cheap* — one ed25519 signature. A
   subject who cares mints a fresh passport per counterparty.
3. BBS+ unlinkable multi-show would fix it properly. Deferred with a stated
   precondition — §4.3.

---

## 3. The warrant (c)

Per-crossing, narrow, short-lived, **attenuable**.

### 3.1 Shape: a signed chain, Biscuit-style, not macaroon-style

Macaroon attenuation is HMAC over a root secret the verifier shares. lait shares
no secret with an outside counterparty, so the HMAC construction is unavailable.
Biscuit's public-key attenuation is the right shape and it is also the shape
already in the tree: `sigdag::SignedNode` binds `parents` into the signature
specifically to close "the re-parent revocation bypass" (`sigdag.rs:12-14`). A
warrant chain is that discipline in a line rather than a DAG.

```rust
pub const WARRANT_DOMAIN: &[u8] = b"lait/warrant/1";
pub const WARRANT_LINK_DOMAIN: &[u8] = b"lait/warrant-attenuation/1";

pub struct WarrantRoot {
    pub version: u8,
    pub issuer: ActorId,
    pub issuer_device: [u8; 32],
    pub subject: ActorId,          // may equal issuer; the sub-agent case differs
    pub audience: AudienceRef,     // ActorId | did:key | opaque [u8;32]
    pub purpose: Resource,         // REUSED verbatim from demand.rs:126
    pub scope: Vec<(PolicyCapability, Resource)>,  // REUSED; ≤ 128, sorted, deduped
    pub budget: Budget,
    pub not_before: u64,
    pub expires_at: u64,           // protocol ceiling: expires_at - not_before ≤ MAX_WARRANT_TTL
    pub nonce: [u8; 16],           // revocation key + replay key; FRESH per warrant
    pub next_key: Option<[u8; 32]>, // the ONLY key permitted to attenuate further
    pub signature_algorithm: u8,
    pub signature: [u8; 64],
}

pub struct Attenuation {
    pub scope: Vec<(PolicyCapability, Resource)>, // must be ⊆ effective scope
    pub budget: Budget,                            // must be ≤ effective, kind-wise
    pub expires_at: u64,                           // must be ≤ effective
    pub purpose: Option<Resource>,                 // refinement only
    pub next_key: Option<[u8; 32]>,                // None = terminal
    pub author: [u8; 32],                          // MUST equal previous link's next_key
    pub signature_algorithm: u8,
    pub signature: [u8; 64],                       // over (prev_link_digest ‖ this block)
}

pub struct Warrant {
    pub root: WarrantRoot,
    pub links: Vec<Attenuation>,   // ≤ 8; ordered
    pub holder_proof: HolderProof, // fresh sig by the terminal key over
                                   // (warrant_digest ‖ audience_challenge)
}
```

`purpose` is a `Resource`, not a string. Reusing the type buys the bounds (≤ 8
segments, ≤ 64 bytes each, ≤ 512 total, no wildcard — `demand.rs:157-181`), a
frozen canonical encoding, and byte-exact matching against a counterparty's own
policy without inventing a second grammar. `Resource{world:"acme.review",
segments:["pr","4821"]}` is a purpose. A free-text field would become a tracking
payload the first time anyone wrote a sentence into it.

`Budget` — "what I may spend" — kept generic:

```rust
pub struct Budget { pub units: Vec<(BudgetKind, u64)> }  // ≤ 8 kinds, sorted, deduped
// BudgetKind is a bounded name in the same grammar: requests · bytes.egress
//                                                   tokens · wallclock.seconds
```

### 3.2 Holder proof

The chain proves *what* is authorized. `holder_proof` proves *who is presenting
it right now*: a fresh signature by the terminal key over `warrant_digest ‖
audience_challenge`, where the challenge is a nonce the counterparty supplied.
This is SD-JWT's Key Binding JWT done with `sigdag::sign_message`, and it is
what stops a captured warrant from being replayed by a third party.

### 3.3 Where attenuation is enforced

**Three places. Only the first two are cryptographic, and the third is the one
that actually makes the whole thing safe.**

**(1) In the decoder — `Warrant::decode_canonical`, not in a policy check.**

Every consumer reaches an *effective* warrant through exactly one function,
which folds the chain left to right:

```
effective = root;  for link in links { effective = narrow(effective, link)? }
```

`narrow` is total and monotone:

- **scope**: `link.scope ⊆ effective.scope` (byte-exact on canonical
  `(capability, resource)` pairs). Not a subset → `Invalid::Widened`.
- **expiry**: `effective.expires_at = min(effective.expires_at, link.expires_at)`;
  a link naming a *later* expiry → `Invalid::Widened`.
- **budget**: this one inverts and the naive reading is backwards. A link must
  name **every kind already present**, each with a value ≤ the effective value.
  Omitting a kind would leave it unbounded, which is widening. *Adding* a new
  kind is permitted — a new constraint only narrows.
- **purpose**: a refinement must extend the effective purpose's segments, never
  replace or shorten them.

**Reject, never clamp.** Silently clamping a widening attempt hides an attack and
makes the artifact's bytes disagree with its meaning. This matches
`AuthorizationDemand::decode_canonical` (`demand.rs:336-351`), which requires
round-trip byte equality and rejects non-canonical input rather than normalizing
it, and `ARCHITECTURE.md` §9: "Unknown signed, wire, or store versions fail
closed."

The decoder is the enforcement point *because it is the only funnel*. There is
no second path to an effective warrant, so there is no place for a caller to
forget the check — the same reason `serve::borrowed_key_refusal` is one function
called at two doors rather than a rule people remember.

**(2) In the key chain — why sub-agent delegation is safe by construction.**

`Attenuation.author` must equal the previous link's `next_key`; `next_key: None`
is terminal. Consequences:

- A holder can only ever produce a *further attenuation*, never a new root. The
  root's `issuer` signature cannot be re-created without the issuer's device key.
- A sub-agent's key is generated by the delegator and never leaves it upward, so
  the chain grows in exactly one direction.
- Recursive delegation is bounded by `links.len() ≤ 8` and by the fact that each
  hop can only shrink.

This is the *external* analogue of the fence `docs/AGENT-EXPERIENCE.md` states
internally — a sponsored agent holds content authority but never membership
authority (`acl.rs:130` `is_sponsorable_grant_set`, plus the blanket agent-author
ban in `judge_op`). A warrant chain must not be able to reintroduce it:

> **A warrant's scope may never contain `acl::policy_admin_capability()`
> (`acl.rs:269`) on `acl::policy_admin_resource()` (`acl.rs:274`), and a warrant
> carries no `Standing` at all.** Refused at mint, in the root's validator.

Note the deliberate inversion: `WorldAssignmentEvidence::validate`
(`demand.rs:598-613`) permits the policy-admin meta-capability as a
Mechanics-owned exception, because an administrator-level *admission* legitimately
installs it. A warrant has **no such exception**. Write that as a test, not a
comment.

**(3) At redemption — Mechanics is the floor, and this is what makes it safe.**

A warrant presented *to* a Space is verified structurally, then its effective
scope is intersected with what the presenter actually holds **here**, evaluated
by `Authority::authorize` (`ledger.rs:1295-1343`) at a pinned frontier as it is
for any other act. For an inbound stranger that intersection is empty and the
warrant buys exactly one thing: the right to *ask*.

This is `AdmissionCapability`'s rule verbatim (`coordinates.rs:19-22`) and it
means an issuer cannot widen beyond their own standing either — not by policy,
but because the artifact was never a source of authority in the first place.

**Summary: the holder cannot widen (1 + 2); the issuer cannot widen (3).
Attenuation is enforced in the decoder and re-floored by Mechanics; nothing in
between has to be trusted.**

### 3.4 Revocation

By `nonce`, exactly as invites revoke today: `AclAction::RevokeInvite { nonce }`
(`acl.rs:196-198`) and `Request::InviteRevoke` (`src/control.rs:338-343`) —
signed, convergent, and effective once synced. Revoking a root revokes every
attenuation of it, because every link's signature chains to the root digest. A
`WarrantRevoke` is the same op shape with a distinct discriminant so an invite
nonce and a warrant nonce cannot collide.

---

## 4. Selective disclosure (d)

### 4.1 The requirement

Origin and attestations must be disclosable **per-counterparty**. A passport
that broadcasts every attestation to everyone is a résumé handed to strangers;
one that broadcasts its origin is a Space membership disclosure the subject did
not consent to per-crossing.

### 4.2 The candidates, judged against this tree

**W3C Verifiable Credentials (JSON-LD) — reject.** JSON-LD needs `@context`
resolution (a network fetch, or a pinned context that silently ages) and RDF
Dataset Canonicalization to get stable bytes. This repo's entire signing
discipline is the opposite property: postcard with decode/re-encode byte
equality (`demand.rs:344-350`, `receipt.rs:83-96`, `coordinates.rs`), and
`ARCHITECTURE.md` §9 states "Canonical encodings, domains, hashes, bounds, and
tie-breaks are protocol." VC would add a canonicalization surface engineered out
everywhere else. It also assumes a *trusted issuer DID*, which is a strictly
weaker trust root than `ActorId = act_ + blake3(Incept)` — adopting it means
bolting a weaker root beside a stronger one.

**SD-JWT — adopt the mechanism, reject the encoding.** The mechanism is exactly
right: salted hash commitments per claim, holder discloses a subset, plus a Key
Binding JWT binding the presentation to an audience and a nonce. The encoding is
JSON + base64url + JOSE, which drags in `alg` negotiation — and this tree does
not negotiate: `signature_algorithm` is a tag that *rejects*
(`coordinates.rs:214-215` `UnsupportedSignatureAlgorithm`). It would also
introduce a second identity vocabulary (`iss`/`sub` strings) beside `ActorId`.

**BBS+ — reject for v1, name as the upgrade path.** It is the only candidate
that fixes §2.7's linkability, via unlinkable multi-show and true ZK selective
disclosure. Cost: BLS12-381 pairings, a second signature scheme beside ed25519,
and no threshold story matching lait's FROST/ceremony stack. This repo already
knows how to be honest about exactly this — `crates/mechanics/src/gaccess.rs:25-46`
enumerates what "passing the functional vectors does not establish". Preconditions
for revisiting: an independently reviewed implementation, a threshold story, and
a stated need beyond the residual in §2.7.

### 4.3 Recommendation: lait's own primitives, carrying SD-JWT's idea

Build on `sigdag::sign_message`/`verify_message` (`sigdag.rs:150-195`), postcard
canonical encoding with byte-exact round-trip, and `blake3::derive_key`
commitments (`demand.rs:456-461`, `472-479`).

```
commitment_i = blake3::derive_key("lait.passport-claim.v1",
                                  postcard(salt_i ‖ claim_i))
capability_root = blake3::derive_key("lait.passport-claim-set.v1",
                                     postcard(sorted(commitments)))
```

The passport signs only `capability_root`. **Disclosure** = handing over
`(salt_i, claim_i)` for the chosen subset; the verifier recomputes each
commitment and checks membership in the signed set. Salts are fresh 16-byte
values per claim per issuance, so an undisclosed claim leaks nothing — not its
content, not its skill domain. (If claim-length classes matter, pad to bounded
buckets; noted, not required for v1.) `vouch_root` works identically.

Audience binding is the warrant's `holder_proof` (§3.2) — the Key Binding JWT's
job, done with the primitive already here.

Justification against lait's conventions, point by point:

1. **One signature scheme.** ed25519 everywhere (`sigdag.rs:23`,
   `coordinates.rs:39`). Adding BLS breaks the single-verifier property.
2. **One canonical encoding.** postcard + round-trip equality is the frozen-bytes
   discipline every artifact here already uses.
3. **A stronger trust root already exists.** Self-certifying `ActorId` beats a
   VC issuer DID; do not add a weaker one beside it.
4. **Interop is already paid for where it is cheap.** `did:key:z6Mk…` on every
   `MemberDto`/`WhoamiDto` means an external DID-speaking verifier can already
   name a lait device key.
5. **Bounds and rejection are protocol.** SD-JWT/VC are permissive by design;
   `demand.rs` is exhaustively bounded and rejects. The native format inherits
   that for free.

**The one honest cost:** nothing off-the-shelf verifies a lait passport. Mitigate
with a dependency-free verifier spec in `docs/` (the exact preimage layouts), and
*optionally* a lossy SD-JWT-shaped **projection** for consumers that demand JSON
— explicitly a view, never the signed artifact, never a second signing format.

### 4.4 Origin — legitimate, and disclosed two ways

`Incept { space, nonce, devices, recovery_commit }` (`actor.rs:147-154`) binds
the space id, and the ActorId is its hash. So origin disclosure has a free
strong form and a needed cheap form:

- **Strong (offline, unforgeable):** disclose the `Incept` preimage. The verifier
  rehashes, gets the ActorId, and reads the space id out of the payload — no
  trust, no network. Cost: the preimage also carries the device set and recovery
  commitment. All-or-nothing.
- **Cheap (audience-scoped, recommended default):** an `OriginAttestation` — a
  detached signature under `lait/passport-origin/1` by a device of the subject
  over `(subject_actor, space_id, audience, nonce, expires_at)`. Audience-bound,
  so it is a *disclosure to one counterparty*, not a broadcast, and it reveals
  the space id without the device set.

Both are **omitted by default**. A passport with no origin disclosure is a
complete, valid passport — that is the point.

---

## 5. Surface (e)

### 5.1 HTTP: no new routes

Everything here is the Space-authority plane, `POST /api/spaces/{id}/rpc`
(`src/serve/mod.rs:327`, `docs/SERVE.md` plane table), with new `cmd`
discriminants. Constraints that must be honoured:

- Each new `Request` variant needs an arm in `control::classify`
  (`src/control.rs:1169`) — the match is exhaustive with no wildcard, so this
  fails compilation until it is explicit. Passport/warrant mint, attest, and
  revoke → `Mechanics`. `Availability` → `Station` (it is a live, momentary
  answer, the same owner as presence).
- `policy::is_host_plane` (`src/serve/policy.rs:177`) must **not** admit any of
  them. The host plane exists for the moment there is no space id
  (`docs/SERVE.md`); a credential is Space-scoped.
- **The custody fence applies unchanged, and nothing new is needed.** Minting a
  passport or a warrant *is a signature*, so `WarrantMint`, `WarrantAttenuate`,
  `WarrantRevoke`, `PassportIssue`, and `CapabilityAttest` are **writes** and
  must pass `borrowed_key_refusal` (`src/serve/mod.rs:670, 844-849, 930`) — a
  head serving the human's token must not mint a warrant into an agent-held
  Orbit, because it would go out over the agent's signature. `PassportShow`,
  `WarrantShow`, `Warrants`, and `Availability` are reads and are never refused.
  This falls out of `Catalog::signs_with_own_seed` (`src/orbits/catalog.rs:141`)
  with no new mechanism, exactly as `acl.rs:118-121` predicted it would.

### 5.2 MCP tools

`src/mcp.rs:32-33` records the gate: "`tests/mcp_parity.rs` asserts every one has
a tool below, so adding a `Request` without an MCP tool fails the
interface-parity build gate." So each variant below is mandatory, not optional.

| Tool | Kind | Purpose |
|---|---|---|
| `passport_show` | read | my passport, or another member's as disclosed here |
| `passport_issue` | write | mint/reissue; returns the artifact plus the disclosure set |
| `capability_declare` | write | set my `Declared` skills |
| `capability_attest` | write | sign an `Attested` claim for another actor |
| `availability` | read | this device's three-state answer per skill |
| `warrant_mint` | write | `{audience, purpose, scope[], budget, ttl}` |
| `warrant_attenuate` | write | narrow and hand on; returns a longer chain + a fresh terminal key |
| `warrant_show` | read | decode and print the **effective fold**, not the raw chain |
| `warrant_revoke` | write | by nonce; `InviteRevoke`-shaped |
| `warrants` | read | what this identity currently carries |

`warrant_show` printing the *fold* rather than the chain is deliberate: the chain
is what a holder controls, the fold is what is true, and a human reviewing an
agent's credential must be shown the second.

### 5.3 DTOs

Beside `WhoamiDto` in `src/dto.rs`:

```
PassportDto   { subject, did, issued_at, expires_at, nonce,
                claims: Vec<ClaimDto>, vouch: Vec<VouchDto>, origin: Option<String> }
ClaimDto      { skill, modality: "declared"|"attested"|"demonstrated",
                attestor: Option<String>, expires_at: Option<u64> }
WarrantDto    { nonce, issuer, subject, audience, links: u8, terminal: bool,
                effective: EffectiveDto }
EffectiveDto  { purpose, scope: Vec<ScopeDto>, budget: Vec<BudgetDto>, expires_at }
AvailabilityDto { skill, state: "ready"|"elsewhere"|"unavailable" }
```

`AvailabilityDto.state` is a three-value enum on the wire and never a count —
`THREAT-MODEL.md:296`.

**Leave `WhoamiDto` alone.** It answers "who am I *here*, and is my view whole"
(`src/dto.rs:47-51`); credentials are about elsewhere. Add `Request::Passport`
and `Request::Warrants` as siblings rather than growing the orientation blob.

### 5.4 Where a human reviews and revokes

Add a **`credentials`** tab to `Settings`: the `Tab` union at
`viewer/src/ui/Settings.tsx:33`, the `TABS` list at `:35`, the `isTab` guard at
`:40`, the `lait:nav { tab }` listener at `:107-111`, and the union documented in
`CLAUDE.md` and `viewer/src/core/registry.ts`.

Not the existing `access` tab: that is the internal policy plane (roles, grants),
and merging "what my agent may do inside" with "what my agent carries outside"
is precisely the passport/authority conflation this design exists to prevent.

The tab answers three questions and nothing else:

1. **What does each of my agents carry right now** — one row per live warrant,
   showing the *effective* fold, the audience, and the chain depth.
2. **How long until it expires** — sorted by expiry ascending, so the list is
   self-triaging.
3. **Revoke** — one button, per nonce.

Link into it from each sponsored row in `viewer/src/ui/Members.tsx:136-142`,
which already renders "sponsored · <sponsor>" and is where agents are created
(`agent_provision`, `Members.tsx:243`).

The passport gets a separate read-only panel beside the existing identity/`did:key`
display — reviewing standing and revoking a crossing are different acts and
should not share a button.

**One note against cargo-culting.** `viewer/src/ui/Governance.tsx:20-23` is
read-only on purpose because editing a workflow or role is a CAS ceremony
(`expect_heads`/`expect_revision`) whose conflict flow needs its own design pass.
Warrant revocation is *not* that: it is a single signed op keyed on a nonce with
no compare-and-swap, exactly like `InviteRevoke`. So a revoke button in the
viewer is correct here even though a role editor is not.

---

## 6. Explicitly out: no cross-space interaction history (f)

The rule, restated so it can be tested against: **the record of an interaction
belongs to the parties who were there, not to a document the subject carries
between them.** It is `actor.rs:26-27` ("cross-space linking is a local
address-book concern, never protocol state") one layer up, and it leaks
transitively — one disclosure reveals every prior counterparty.

Audit of this design, honestly:

| Component | Verdict |
|---|---|
| Passport | ✅ no counterparty, engagement, destination, presentation counter, or `last_used` |
| Warrant root | ✅ names exactly one audience — the party it is *for*, not a party it has been to |
| Attenuation chain | ✅ names delegation keys, which are ephemeral and generated per hop; it is a capability path, not an itinerary |
| Availability | ✅ three states, never a count, never a machine name, never durable |
| `AuthorizationReceipt` | ✅ **never leaves the Space** — §1.1 |
| `Attested` claim | ⚠️ **residual** — see below |
| Passport reuse across counterparties | ⚠️ **residual** — §2.7 |

**Residual 1 — an attestation names its attestor.** Unavoidable: an attestation
with an anonymous attestor is worthless. It is bounded in three ways: the
attestor is a party who *was there* and consented by signing; it is
non-transitive (disclosing skill A's attestor reveals nothing about skill B);
and it is per-claim selectively disclosable (§4.3). The rule that keeps it from
degrading: **an attestation must never name a third party, a space, a place, or
an engagement.**

**Residual 2 — passport linkability.** §2.7, mitigated by per-audience reissue,
fixed properly only by BBS+ (§4.2).

**The proposal to refuse, and it is the subtle one.** Someone will eventually
propose a `redemptions_so_far` counter inside a warrant, framed as anti-replay.
Refuse it. That field is a cross-counterparty interaction record living in the
subject's own pocket: whoever sees the warrant learns how many times it has been
spent elsewhere. The correct construction already exists —
`AdmissionUsePolicy` (`coordinates.rs:132-156`) puts the **cap** in the
artifact and the **count** in the redeeming Space's replayed history
(`acl.rs:545`). Warrants copy that exactly: the cap travels, the count does not.

---

## 7. Placement and build order

**Crate.** `crates/mechanics/src/credential/{passport.rs, warrant.rs,
descriptor.rs, disclosure.rs}`. Mechanics is "the sole source of truth for
actors and their valid devices; scoped capability assignments and delegation"
(`ARCHITECTURE.md` §3), and the types reuse `demand::{PolicyCapability,
Resource}`, `sigdag`, and `ids::ActorId` directly. The dependency direction is
already established: `crates/runtime/src/coordinates.rs:178` reaches *up* into
`mechanics::authorization::WorldAssignmentEvidence`. Whatever crate carries a
credential across the boundary carries bytes only — "Comms moves bytes but
cannot legitimize them" (`ARCHITECTURE.md` §2).

**Availability** does not go there. It is momentary and belongs beside
`plane::live::ResidencyOracle` in `crates/runtime`.

**Order**, each step landing with its own tests before the next:

1. `descriptor.rs` — `Skill`, `Modality`, `Claim`, bounds, canonical encoding,
   digest. Pure data; no signing. Golden-bytes test in the `receipt.rs:199-218`
   style.
2. `disclosure.rs` — salted commitments, set root, subset verification. Test that
   an undisclosed claim is unrecoverable and that a forged subset fails.
3. `passport.rs` — envelope, `PASSPORT_DOMAIN`, sign/verify, expiry, version and
   `signature_algorithm` rejection matrix (copy `coordinates.rs`'s matrix
   wholesale — it is the model).
4. `warrant.rs` — root, `Attenuation`, and **the fold**. The tests that matter:
   widening scope rejects; a later expiry rejects; a *dropped* budget kind
   rejects; a wrong `author` key rejects; `next_key: None` refuses a further
   link; `policy_admin_capability()` in scope refuses at mint.
5. Surface — `Request` variants, `classify` arms, `is_host_plane` exclusion,
   write-classification against `borrowed_key_refusal`, MCP tools (parity gate),
   DTOs, then the `credentials` tab.

## 8. Open questions to settle before step 3

1. **`MAX_WARRANT_TTL`.** Proposed 3600s. Long enough for a real task, short
   enough that revocation latency rarely matters. Needs one number, frozen.
2. **`AudienceRef` variants.** `ActorId` covers lait-to-lait. `did:key` covers a
   DID-speaking outsider. An opaque `[u8;32]` covers "a service that has a public
   key and nothing else". Three is probably right; confirm before freezing the
   postcard discriminants (they are positional and append-only).
3. **Sponsor counter-signature on the passport.** A `Vouch` is independently
   signed and independently disclosable (§2.2). Does the passport's
   `vouch_root` need the sponsor's signature *over the root* as well, or is
   per-vouch signing sufficient? Leaning sufficient — the sponsor is vouching for
   the subject, not for the subject's claim set.
4. **Whether a warrant may be minted by an agent for a sub-agent at all**, or
   only attenuated from one the human minted. Attenuation-only is strictly safer
   and costs one extra human step per delegation tree. Leaning attenuation-only
   for v1.
