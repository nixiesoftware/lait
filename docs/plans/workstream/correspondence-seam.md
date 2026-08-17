# Correspondence — the crate layout and the contractor seam

Status: **blueprint**. No production code. Every claim below cites the file it is
derived from; every design choice states which existing seam it copies.

Scope: **(a)** the seam, **(b)** `MemCorrespondence`, **(c)** the egress custody
gate, **(d)** the ingress quarantine and `Provenance`, **(e)** threat-model
amendments, **(f)** crate-graph position and manifest edges.

Out of scope, owned elsewhere: the addressbook and naming layers; the sponsorship
relation; mailbox Body schemas and sealing; credential/passport work. Where this
document has to touch one of those it names the *shape of the hole* and stops —
see §2.4 (`ExternalAddress`), §2.6 (`Cursor`), §6.

Companion documents already settled: `docs/plans/correspondence-mailbox.md`
(the plane), `docs/plans/correspondence-specs.md` (G-1, R-1..R-6, D-1..D-4).
This is the D-1 slice made concrete.

---

## 0. The one-paragraph version

`crates/comms` is the model. It calls itself "the replaceable mechanism that
moves their bytes between peers" and "the only crate that names a concrete
network" (`crates/comms/src/lib.rs:20-25`); lait names `Transport`, `PeerId`,
`Topic` and framed `Stream`s, iroh is sealed behind them, and the root manifest
states the payoff — *"Swapping the adapter is a manifest change, not a daemon
rewrite"* (`Cargo.toml:110-113`). Correspondence takes exactly that shape one
boundary out: lait names `Correspondent`, `MailboxId`, `Franked`, `Quarantined`
and `Provenance`; a mail *protocol* is nameable in exactly one crate,
`crates/correspondence-post`; and the two gates are not functions a caller is
asked to remember but **types a caller cannot construct**, so a send path cannot
compile before its gate and a Body cannot be minted from unquarantined bytes.

---

## 1. Two crates, and why not one

`comms` puts its contractor *inside* itself: `mod iroh;` is private and the
implementation escapes only under a role name —

```rust
// crates/comms/src/lib.rs:63-68
pub use iroh::{IrohFactory as DefaultFactory, IrohTransport as DefaultTransport};
```

Correspondence splits instead:

| crate | names | must never name |
|---|---|---|
| `crates/correspondence` | the vocabulary, both gates, `mem`, `policy`, **format** libraries (MIME, HTML sanitiser, IDNA/unicode) | any mail protocol client, any DNS resolver, `iroh`, `loro` |
| `crates/correspondence-post` | **protocol** libraries (SMTP/IMAP/JMAP client, MX/DKIM DNS, mail TLS) | `loro`, `iroh`; and it cannot bypass either gate, because it cannot construct their types |

Three reasons the split is right here where it was not in `comms`:

1. **The gates must not sit in the same crate as the thing they gate.** A
   contractor that can name `Franked`'s private field can mint one. Putting the
   contractor in a sibling crate makes "the contractor cannot bypass the gate" a
   privacy fact rather than a review habit. `comms` has no analogue: there is no
   security predicate between `Transport` and `iroh`.
2. **A mail protocol client is a heavy, network-facing dependency tree** (TLS,
   DNS, SASL). The crate that defines the hostile-input boundary should not link
   it. `crates/relay` is the precedent already in the tree — hosting a relay is
   "a server role a client never plays" (`crates/comms/src/lib.rs:54-57`), so it
   was pushed out behind a feature and a separate crate.
3. **Deliverability becomes a deployment choice** (D-1). Stalwart, a hosted
   relay, or nothing at all is a root-manifest line.

**The seal line — protocol vs. format.** A crate may name a *format* library iff
it is inside the quarantine; a crate may name a *protocol* library iff it is the
contractor. MIME structure is format: its nesting and part bounds **are** the
security property, so the quarantine owns the parse. SMTP/IMAP/JMAP, MX lookup
and DKIM's DNS fetch are protocol. This is the same division `comms` already
draws — framing policy belongs to the seam ("Framing policy belongs to the
transport (the seam that owns the wire), so the constant lives here",
`crates/comms/src/lib.rs:127-132`) while the network belongs to the contractor.

DKIM specifically: the contractor performs the lookup and **reports a verdict**;
correspondence records the verdict as provenance and never elevates it to
identity (§4.3).

---

## 2. The vocabulary

New crate `crates/correspondence`, module layout mirroring `comms` one-for-one:

```
crates/correspondence/src/
  lib.rs        the seam: Correspondent, CorrespondentFactory, the shared nouns
  policy.rs     WHERE lait corresponds — mirrors comms::policy (lib.rs:28-33)
  mem.rs        MemCorrespondence — mirrors comms::mem  (§3)
  egress.rs     the custody gate; the only constructor of `Franked`  (§4)
  ingress.rs    the quarantine; the only constructor of `Admitted`   (§5)
  provenance.rs Provenance + Attribution                             (§5.4)
```

`lib.rs` opens with the same crash-safety block every crate root carries
(`crates/comms/src/lib.rs:1-18`) and `[lints] workspace = true`. Its module doc
states the role in the same register: *the replaceable mechanism that carries
material across the Space boundary, and the only plane on which unauthenticated
material becomes a Body.*

### 2.1 The trait — the narrow waist

`Transport` is dial / gossip / accept (`crates/comms/src/lib.rs:274-393`).
`Correspondent` is send / accept / resume, and nothing else:

```rust
#[async_trait]
pub trait Correspondent: Send + Sync {
    /// Whose correspondence this carries. `Transport::my_id` (lib.rs:283).
    fn identity(&self) -> MailboxId;

    /// Hand one franked message to the outside world.
    ///
    /// Takes `Franked` by value. There is no other parameter and no other
    /// constructor for that type, so this signature is what makes "the gate
    /// exists before any send path" a compile-time fact.
    async fn send(&self, franked: Franked) -> Result<Handoff>;

    /// The next arrival, already wrapped. `None` once the view has shut down,
    /// exactly as `Transport::accept` (lib.rs:337).
    ///
    /// Yields `Quarantined<Arrival>`, never `Arrival`: a contractor has no way
    /// to hand the plane material that has not been through §5.
    async fn accept(&self) -> Option<Quarantined<Arrival>>;

    /// Resume retrieval from a contractor-minted cursor (§2.6).
    async fn resume_from(&self, cursor: Cursor) -> Result<()>;

    /// Best-effort teardown; unblocks a parked `accept`, which returns `None`
    /// from then on. `Transport::shutdown` (lib.rs:392).
    async fn shutdown(&self);
}
```

Deliberately absent, and each absence is load-bearing:

- **No `Correspondent::receive_into_body`.** The contractor never mints a Body.
  It yields bytes; the plane decides. Same rule as
  `GossipEvent::Received`, whose `from` is documented as "the **delivering
  neighbor** … a routing hint, never an authenticated author"
  (`crates/comms/src/lib.rs:175-180`).
- **No address book.** `ExternalAddress` is opaque here (§2.4).
- **No mailbox list / folder / label verb.** Those are mailbox Body schema
  (D-3), not the wire.
- **No `verify` verb.** Verification is something the contractor *reports*, not
  something the plane asks it to perform on demand — otherwise a caller could
  re-ask until it liked the answer.

### 2.2 The factory — identity by construction

Copied verbatim in shape from `TransportFactory`
(`crates/comms/src/lib.rs:133-166`), including the rationale: a factory rather
than a ready-made value, "because the transport's identity must *be* the
Station's device identity. Handing the seed to the builder makes the two agree
by construction."

```rust
#[async_trait]
pub trait CorrespondentFactory: Send + Sync {
    async fn build(
        &self,
        identity_seed: &[u8; 32],
        policy: &policy::Correspondence,
    ) -> Result<Arc<dyn Correspondent>>;

    /// A view scoped to one (actor, Space).
    ///
    /// This is where decision D1 lands. `crates/mechanics/src/actor.rs:24`:
    /// actors are per-space, "the same human in two spaces is two unlinkable
    /// actors". If D1 resolves to (a) — one mailbox per (actor, space) — then
    /// unlinkability is a property of *this method having a `space`
    /// parameter*, and a factory that ignores it has deleted the invariant.
    /// The default forwards to `build`, exactly as `build_scoped` does at
    /// comms/src/lib.rs:150-162, so a simple contractor need not implement it —
    /// but the shipped one must, and a test asserts two scoped views of one
    /// seed produce non-equal `MailboxId`s.
    async fn build_scoped(
        &self,
        identity_seed: &[u8; 32],
        policy: &policy::Correspondence,
        actor: &ActorId,
        space: &SpaceId,
    ) -> Result<Arc<dyn Correspondent>>;

    async fn shutdown(&self) {}
}
```

### 2.3 `policy::Correspondence` — where lait corresponds

`comms::policy` owns *where lait operates* and is "the **single place** iroh's
relay/discovery vocabulary … is spoken" (`crates/comms/src/policy.rs:1-12`).
The mail analogue states the requirement; the contractor fulfils it:

```rust
pub enum Correspondence {
    /// No external correspondence at all. Egress refuses, ingress yields
    /// nothing. The default for a Space that has not opted in — mirrors
    /// `policy::Network::Isolated` (comms/src/policy.rs:56-63), and mirrors
    /// `LocalDestination::resolve(None)` returning `None`:
    /// "no destination is configured, and that is a refusal rather than a
    /// default" (world-interface/src/destination.rs:134-139).
    Isolated,
    /// A named submission/retrieval host lait supplies (self-hosted Stalwart,
    /// or a test harness). ~ `Network::Local`.
    Hosted(HostedMail),
    /// A managed provider. ~ `Network::Public`.
    Managed(ManagedMail),
}
```

Two rules carried over verbatim from the transport policy and worth restating,
because they are the ones that get relaxed under delivery pressure:

- **Configuration is guarded local deployment policy and is never accepted from
  an invite** — `docs/ARCHITECTURE.md:418-421` says this of relay/discovery. It
  is strictly more important for mail: a submission host accepted from a
  peer-authored artefact is an exfiltration route with a signature on it.
- **`Isolated` is a real mode, not a degraded one.** A Space that never
  corresponds must be expressible, and it must be the default.

### 2.4 Nouns

```rust
/// Whose correspondence. Actor-keyed, per D-2: `ActorId` is self-certifying
/// (mechanics/src/actor.rs:22-24) and content-independent of any device key
/// (mechanics/src/ids.rs:263-270), so a mailbox survives device rotation.
pub struct MailboxId { actor: ActorId, space: SpaceId }

/// An address outside the boundary.
///
/// **Opaque here on purpose.** Naming, resolution, and the address book are
/// another workstream's. Correspondence requires only three properties of it:
/// bounded, canonical (byte-equal round-trip), and *never* a path. Nothing in
/// this crate parses it, compares it case-insensitively, or displays it without
/// going through `Attribution` (§5.4).
pub struct ExternalAddress(Vec<u8>);

/// What the contractor hands back for a send. Provenance, never authority —
/// a queue id from an MTA is a receipt for a handoff, not a delivery proof and
/// certainly not a signature. Named `Handoff` rather than `Receipt` so it
/// cannot be confused with `replica::receipt`, which *is* an authority object.
pub struct Handoff { id: Vec<u8>, at: SystemTime }

/// One inbound message as the contractor received it: raw bytes plus the facts
/// the transport asserts about them. Never leaves the crate un-wrapped.
pub struct Arrival {
    raw: Vec<u8>,
    asserted: AssertedEnvelope,   // From / Return-Path / Message-ID / To
    checks: ExternalChecks,       // DKIM / SPF / DMARC verdicts (§5.3)
    at: SystemTime,
}
```

### 2.5 Bounds

`comms` owns `MAX_FRAME` because it owns the wire
(`crates/comms/src/lib.rs:127-132`), and `Stream::recv_bounded` exists so "an
oversized length prefix is rejected before allocating the body"
(`crates/comms/src/lib.rs:210-222`). Correspondence owns the same class of
constant, and the numbers must be derived rather than picked:

| constant | value | derivation |
|---|---|---|
| `MAX_MESSAGE_BYTES` | strictly below `replica::protected::MAX_BODY_BYTES` (64 MiB, `crates/replica/src/protected.rs:22-24`) minus envelope overhead (`MAX_PROTECTED_PLAINTEXT`, `:26-29`) | a message that parses but cannot become a Body is a failure discovered after the work |
| `MAX_MIME_DEPTH` | 8 | §5.2 |
| `MAX_MIME_PARTS` | 256 | §5.2 |
| `MAX_HEADER_BYTES` / `MAX_HEADER_COUNT` | 64 KiB / 512 | §5.2 |
| `MAX_RECIPIENTS` | small, explicit | §4.2 |

Attachments do **not** count against `MAX_MESSAGE_BYTES` — they take the content
plane's descriptor/residency split (`crates/replica/src/content.rs:47`,
`CHUNK_PLAINTEXT_LEN = 256 KiB`), per D-3.

### 2.6 `Cursor` — the per-device resumption trap, expressed as opacity

```rust
/// Contractor-minted retrieval state (IMAP `UIDVALIDITY`+`UID`, a JMAP state
/// string). Opaque to this crate, exactly as a length prefix is opaque above
/// `comms`.
pub struct Cursor(Vec<u8>);
```

The plane stores it; the schema for *where* is D-3's, and `correspondence-mailbox.md`
§3.5 records the trap: one `RegisterSet` holding it will be fought over by two
devices that are both correct about themselves, so it must be keyed per
`DeviceId`. Making `Cursor` opaque here is what stops this crate from having an
opinion that would then have to be reconciled with that schema.

---

## 3. `MemCorrespondence` — built first

Mirrors `crates/comms/src/mem.rs`, which is "what makes the *real daemon*
testable hermetically: build N `MemTransport`s off one `MemNet` switchboard …
they dial/gossip/accept through the same code paths as production — but the
'network' is a `HashMap` and some channels, so it is offline, instant, and
reproducible on every OS" (`mem.rs:4-10`). The equivalent claim here: **mail
tests need no SMTP server anywhere, and no DNS.**

This is step 1 of the staging in `correspondence-mailbox.md` §9 and everything
below depends on it.

### 3.1 The switchboard

```rust
/// The shared post office every in-memory correspondent is wired to.
/// Cloneable; all clones share one registry.  ~ `MemNet` (mem.rs:33-36).
#[derive(Clone, Default)]
pub struct MemPost(Arc<StdMutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// Inbound inbox per mailbox — actor-keyed.       ~ `Inner::peers` (mem.rs:41)
    boxes: HashMap<MailboxId, mpsc::UnboundedSender<Arrival>>,
    /// Inbound inbox per *stranger*. §3.3 — no comms analogue.
    strangers: HashMap<ExternalAddress, mpsc::UnboundedSender<Arrival>>,
    /// How the alias table maps an external address to a mailbox, so a test
    /// can address a member from outside without an address book.
    aliases: HashMap<ExternalAddress, MailboxId>,
    faults: Option<(Faults, Seeded)>,                 // ~ mem.rs:50
    partitions: BTreeSet<(Endpoint, Endpoint)>,       // ~ mem.rs:55, smallest-first
    delivered: Delivered,                             // ~ mem.rs:57
}
```

`Seeded` is `mem.rs:113-128`'s splitmix64, **written out again rather than taken
as a dependency**, for the reason recorded there: "a crate that seeds itself, or
a `HashMap` iteration order, silently reintroduces the nondeterminism a seed
exists to remove — and the failure mode is a bug report nobody can replay."
(A shared `Seeded` extracted to a third crate would be nicer and is *not*
proposed: the duplication is 15 lines and the alternative is a dependency edge
between two seams that must stay independent.)

`Delivered` counters (`mem.rs:133-139`) carry over verbatim in purpose: "a
'chaos' test that never dropped anything is a slow way of testing the happy
path."

### 3.2 Faults — where mail diverges from a network

```rust
pub struct Faults {
    /// Silently lost. The sender believes it sent.        ~ mem.rs:80
    pub drop_percent: u8,
    /// Delivered twice. THE mail reality, not a curiosity: an ingest that is
    /// not idempotent on Message-ID fails here rather than in production.
    pub duplicate_percent: u8,
    /// Accepted, then bounced back as a DSN. No comms analogue — a transport
    /// either delivers or does not, whereas an MTA can accept and *then*
    /// refuse, hours later, in a message that itself arrives through ingress
    /// and is itself attacker-influenced (a bounce quotes the original).
    pub bounce_percent: u8,
    /// Delivered out of order. SMTP guarantees no ordering; `MemPost` must be
    /// able to say so, because FIFO channels would otherwise teach every test
    /// above it an ordering that does not exist.
    pub reorder_percent: u8,
}

impl Faults {
    pub const PERFECT: Self = /* all zero */;   // ~ mem.rs:87-91: the default must
                                                // not silently rewrite existing tests
    pub const LOSSY: Self = /* ~ mem.rs:97-100 */;
}
```

`MemPost::partition(a, b)` cuts two endpoints off symmetrically, stored
smallest-first "so a partition is symmetric by construction rather than by
discipline" (`mem.rs:52-55`); `heal()` clears. For mail this models a domain
that cannot reach another — the deliverability failure that is otherwise
untestable without two real MTAs.

### 3.3 Strangers — the piece with no `comms` analogue

`MemNet` has only peers, because in `comms` everyone on the switchboard is a
device with a key. The correspondence switchboard must be able to produce the
adversary that §7 introduces:

```rust
impl MemPost {
    /// Attach a member's mailbox.               ~ `MemNet::peer` (mem.rs:257)
    pub fn mailbox(&self, id: MailboxId) -> MemCorrespondent;

    /// Attach a correspondent who is **not a member and never will be**.
    ///
    /// This is the whole reason the in-memory contractor exists rather than a
    /// test double per test: the ingress quarantine's adversary is somebody the
    /// Space never admitted, and there is no other way to obtain one hermetically.
    /// A `Stranger` can send arbitrary bytes with arbitrary asserted envelopes
    /// and arbitrary `ExternalChecks` — including a DKIM `pass` for a domain it
    /// does not own, because a test must be able to ask what happens when the
    /// contractor is wrong or lying (§5.3).
    pub fn stranger(&self, addr: ExternalAddress) -> Stranger;
}
```

`Stranger::post(raw, asserted, checks)` is the corpus injector. Together with a
fixture directory of hostile MIME (staging step 2) this is the whole ingress
test rig, with no network and no mail server.

### 3.4 Contract-fidelity notes

`mem.rs:12-18` carries a block declaring where the in-memory implementation
diverges and which implementation is the contract. `mem.rs` must carry the
equivalent, and it is longer because mail diverges more:

> Contract fidelity notes (the shipped contractor is the contract where they
> diverge): there is no DNS, so MX selection and DKIM key fetch do not happen —
> a `MemPost` `ExternalChecks` is whatever the test said it was, which is the
> point (§5.3). There is no size negotiation, no 8BITMIME, no greylisting and no
> retry schedule: a `drop` here is permanent where a real MTA would retry for
> days. Delivery is synchronous where submission is asynchronous, so a `Handoff`
> from `MemPost` means more than a real one does — nothing above may treat a
> `Handoff` as delivery, and the shipped contractor is what proves it.
> `send` succeeds whenever the destination is *registered* on the switchboard;
> deliverability is registration, so an unreachable destination must have been
> partitioned or never attached.

### 3.5 What step 1 proves with no protocol code

Steps 1–3 need no mail protocol at all (`correspondence-mailbox.md` §9), which
is what makes the spike cheap to discard. With `MemPost` alone:

- a mailbox is confidential inside a shared Space (R-1) — the sealing slice
  drives it, this provides the plane;
- device rotation costs no re-encryption (R-1) — add a device, deliver, read;
- the ingress quarantine holds against a hostile corpus (R-3), with the
  attacker being a `Stranger`;
- egress refuses before a send path exists (R-4), because `send` takes `Franked`
  and `frank` does not exist yet at step 1 — the trait compiles and nothing can
  call it. That is the ordering constraint made mechanical rather than
  remembered.

---

## 4. EGRESS — the custody gate

### 4.1 Where it lives, and the honest correction

The brief says the gate "belongs beside the existing predicate in mechanics".
The existing predicate is **not in mechanics today**:

- `Catalog::signs_with_own_seed` — `src/orbits/catalog.rs:141-143`
- `Catalog::path_signs_with_own_seed` — `src/orbits/catalog.rs:164-174`
- `serve::borrowed_key_refusal` — `src/serve/mod.rs:844-869`
- `orbits::bootstrap::admit` — `src/orbits/bootstrap.rs:277-286`
- `orbits::bootstrap::admit_formation_target` — `src/orbits/bootstrap.rs:321-332`

What is in mechanics is a *reference* to them, at
`crates/mechanics/src/acl.rs:118-121`:

> **The custody fences are not on that list.** `orbits::bootstrap::admit` and
> `serve::borrowed_key_refusal` ask whose *key* would make a signature, never
> what standing the holder has …

And they cannot move as they stand. `signs_with_own_seed` compares filesystem
directories (`same_path(&resolved.identity_dir, &self.identity)`), and mechanics
"lists **no scaffold** in its manifest … That absence *is* the boundary"
(`crates/mechanics/src/lib.rs:24-26`). A path comparison in the kernel would
break the property that makes the kernel worth having.

**Resolution — split the question from its resolution.** The *decision* is pure
over identity and belongs in mechanics; the *resolution* of which seed is which
stays where the filesystem is.

```rust
// NEW: crates/mechanics/src/spend.rs   (pub mod spend; in lib.rs beside `custody`)
//
//! Whose key is about to be spent.
//!
//! Not "is this act permitted" — that is `authorization`. This module answers
//! only whether the key that would make a signature is the key the caller is
//! entitled to spend, and it answers it over identity alone, which is why it
//! can live in the kernel at all. Resolving *which* key each of those is stays
//! with whoever holds the filesystem: `Catalog::signs_with_own_seed`
//! (src/orbits/catalog.rs:141) is that resolver today and remains it.

pub enum KeyCustody {
    /// The signature would be made with the caller's own key.
    Own,
    /// It would be made with a key this process merely holds.
    Borrowed { signer: DeviceId },
}

pub fn custody(signing: &DeviceId, entitled: &DeviceId) -> KeyCustody;
```

Then, in strict order:

1. `mechanics::spend` lands with its own tests.
2. `Catalog::signs_with_own_seed` keeps its signature and its doc comment
   (`src/orbits/catalog.rs:132-143` — that comment is the canonical statement of
   the rule and must not be duplicated) but its body becomes: resolve the two
   seeds, ask `mechanics::spend::custody`, return `matches!(.., Own)`. **No
   behaviour change**, provable by the existing tests at
   `src/orbits/catalog.rs:380-392` and `src/serve/mod.rs:1232-1258`.
3. `correspondence::egress` calls `mechanics::spend::custody` — never
   `signs_with_own_seed`, which it cannot see and must not.

That is what "beside the existing predicate in mechanics, not reimplemented per
product" resolves to once the code is actually looked at: **one decision, in the
kernel, with two resolvers and now three call sites.**

### 4.2 What the gate checks

```rust
// crates/correspondence/src/egress.rs

/// A message that has passed the custody gate. The ONLY thing
/// `Correspondent::send` accepts, and `frank` is its only constructor —
/// the field is private and there is no `Default`, no `From`, no
/// `#[derive(Deserialize)]`, and no `pub fn new`.
pub struct Franked(Outbound);

pub fn frank(
    outbound: Outbound,
    custody: mechanics::spend::KeyCustody,
    scope: &EgressScope,     // the (actor, space) whose view minted `outbound`
) -> Result<Franked, Refusal>;
```

Checks, in this order, all before anything is handed over:

1. **Custody.** `KeyCustody::Borrowed { signer }` ⇒ refuse. The refusal text
   follows `src/serve/mod.rs:860-865` in form, and the reason is stated in the
   same register: *outbound correspondence signed as somebody else has no
   recall.*

   **The head's read exemption does not apply here.** `borrowed_key_refusal`
   "must never refuse a read: reading a hosted identity's board *authors*
   nothing" (`src/serve/mod.rs:834-840`), and `src/serve/mod.rs:929` gates it
   behind `!policy::is_read(&req)`. Egress has **no read side** — every path
   through it leaves the boundary — so the gate is unconditional and there is no
   `is_read` equivalent to get wrong. State this explicitly in the doc comment,
   because the natural move when copying the head's gate is to copy its
   exemption too.

2. **Space binding.** `Franked` carries the `SpaceId` its `EgressScope` named;
   `Correspondent::send` refuses one whose scope does not match its own
   `MailboxId`. Precedent: a custody package "binds itself to its context —
   space, authority, ceremony, principal and leaf — so a restored share cannot
   be silently reopened against the wrong space"
   (`crates/mechanics/src/custody.rs:22-25`). Without this, a `Franked` minted
   under an unlinkable work actor could be posted through a side-project actor's
   contractor, which is D1's invariant destroyed by a plumbing mistake.

3. **Crossing is declared, never inferred.** Every `ExternalAddress` in the
   recipient set was named by the caller as external. A reply that acquires a
   recipient from an inbound message's headers must be re-franked with that
   recipient explicit — otherwise a `Reply-To:` a stranger chose becomes a
   destination nobody chose. This is the egress twin of "a peer-authored name is
   not a path" (`crates/world-interface/src/destination.rs:6-7`): a peer-supplied
   address is not a recipient.

4. **Bounds.** `MAX_RECIPIENTS`, `MAX_MESSAGE_BYTES`, attachment count and total
   descriptor size — checked before handoff, not by the contractor.

5. **Provenance does not launder.** An `Outbound` that quotes or forwards an
   `Admitted` body carries the quoted body's `Provenance` forward into the
   outbound record. A stranger's assertion must not become a member-attributed
   quote by the act of replying to it (§5.4).

### 4.3 The ordering constraint, mechanically

`Correspondent::send(&self, franked: Franked)` is written in step 1, before
`egress.rs` exists. Nothing in the workspace can construct a `Franked`, so
nothing can call `send`, so **no send path can exist before its gate** — and the
compiler enforces it rather than a reviewer. Step 3 adds `frank`, and that is
the moment sending becomes expressible.

Add one gate test in the `orbital_boundaries.rs` style
(`tests/it/orbital_boundaries.rs:135-167`, which has both a passing control and
an injected failing case): parse `crates/correspondence/src/egress.rs` and
assert `Franked` has exactly one constructor and it is `frank`.

---

## 5. INGRESS — the quarantine boundary

This is why the plane is substrate. `docs/THREAT-MODEL.md:26` lists replicated
bytes, display names, clocks, routes and network paths as untrusted — **but all
of it is member-authored**. An inbound message is the first attacker-chosen,
unauthenticated material to become a Body.

### 5.1 The shape

```rust
// crates/correspondence/src/ingress.rs

/// Material that has not been through the quarantine.
///
/// No `Deref`, no `AsRef`, no `into_inner`, no public field, no `Serialize`.
/// The only exit is `admit`. A newtype rather than a convention, for the same
/// reason `Franked` is: the product is one caller and the property is about the
/// function (world-interface/src/destination.rs:266-268).
pub struct Quarantined<T>(T);

/// Material that has been through it, carrying what was verifiable about it.
/// The ONLY value the plane will turn into a Body.
pub struct Admitted {
    body: SanitizedBody,
    attachments: Vec<SanitizedAttachment>,
    provenance: Provenance,          // NOT Option. §5.4.
}

pub fn admit(raw: Quarantined<Arrival>) -> Result<Admitted, Rejected>;
```

`Rejected` is a typed reason, and the reasons are **coarse to the outside**: the
contractor learns only that the message was refused. A detailed refusal handed
back to a sender is an oracle for the quarantine's bounds. This copies the
refusal-funnel rule at `docs/THREAT-MODEL.md:216-229` — "a refusal is never a
statement about what is held" — with the local variant: *a refusal is never a
statement about how the quarantine is configured.* Locally, the reason is
retained and observable to the mailbox owner, because a person needs to know why
their mail did not arrive.

### 5.2 MIME structure — refuse, do not repair

`destination.rs` draws the intake/save distinction explicitly: at intake a name
is **refused** because "the proposer there is a local actor holding write
authority who can simply pick another"; at save time it is **repaired** because
"refusing would let a peer make their own attachment unsaveable"
(`docs/THREAT-MODEL.md:258-264`). Ingress inverts the first half — the proposer
is remote and hostile — so:

- **Structure is refused, never repaired.** Depth > `MAX_MIME_DEPTH`, parts >
  `MAX_MIME_PARTS`, headers over budget, an unknown or ambiguous
  `Content-Transfer-Encoding`, a part whose declared length disagrees with its
  bytes, a message that does not decode: `Rejected`. Repairing structure means
  guessing what an attacker meant.
- **Bounds are checked before allocation**, on the same principle as
  `Stream::recv_bounded` — "an oversized length prefix is rejected before
  allocating the body" (`crates/comms/src/lib.rs:210-213`).
- **Content is repaired, never refused** — display names, filenames, HTML. A
  refusal there hands the sender the ability to make their own message
  unreadable, which is the exact failure `destination.rs:33-35` records.
- **Nesting is counted, not recursed.** The parser must be driven with an
  explicit depth budget rather than by recursion, so a 4000-deep
  `multipart/mixed` is a `Rejected`, not a stack overflow. `docs/THREAT-MODEL.md:82`
  already claims malformed input "rejects or surfaces as typed corruption rather
  than becoming a valid value or panicking the Station" — this is that claim on
  a new decoder, and it wants the same treatment `crates/runtime`'s Contact
  frames got: structural fuzzing, because "the parser is the outermost attack
  surface here" (`crates/runtime/Cargo.toml`, `proptest` dev-dependency note).

**HTML.** Allowlist-based sanitisation to a tag/attribute set the renderer
declares. Dropped unconditionally: `<script>`, `<style>`, `<svg>`, `<object>`,
`<embed>`, `<iframe>`, `<form>`, `<base>`, `<meta http-equiv=refresh>`, every
`on*` attribute, `javascript:` and `data:` URLs, and CSS entirely (an
`expression()`/`url()` in a style attribute is a fetch and a script vector).

**Remote images suppressed by default**, rewritten to a local placeholder, never
fetched at parse and never fetched at render. Two reasons, and the second is the
sharper one:

1. A remote image is a read receipt and an IP disclosure.
2. `docs/THREAT-MODEL.md:369-377` — *one origin serves the viewer, the API, and
   every attachment*, and that origin holds the session credential, which is why
   nothing on the content routes is ever rendered. A rendered message body is a
   *new* thing on that origin, and it is attacker-authored. The rule inherited
   from that section: message bodies get the content-route treatment
   (`nosniff`, `Content-Security-Policy: sandbox; default-src 'none'`,
   never the stored MIME type) unless and until a viewer surface is built that
   isolates them, and a "load remote images" affordance is a per-message,
   per-user action that never persists by default.

**Filenames.** Every attachment name goes through
`world_interface::destination::sanitize_display_name`
(`crates/world-interface/src/destination.rs:49`) — the same function, not a
second one. See §6 for the one dependency-direction problem this creates.

### 5.3 Unicode — and one gap in the existing sanitizer

- **NFC normalise** every display string before storage or comparison. Precedent:
  Coordinates v1 already rejects non-NFC UTF-8 in human-facing hints
  (`crates/runtime/Cargo.toml`, `unicode-normalization` note).
- **Confusable/homograph check** on display names and on the domain part.
  A mixed-script or confusable-skeleton collision with a known correspondent is
  not a rejection — it is a fact recorded in `Provenance` and rendered
  (§5.4). Rejecting would let anyone make a legitimate correspondent
  unreachable by registering a lookalike.
- **IDNA**: a U-label whose A-label differs from what a naive display would
  suggest is shown in its A-label (punycode) form.
- **Bidi and format characters must be stripped, and `destination.rs` does not
  do it.** `sanitize_display_name` filters `char::is_control`
  (`crates/world-interface/src/destination.rs:64`), which is general category
  `Cc` only. U+202E RIGHT-TO-LEFT OVERRIDE is category `Cf` — `is_control()` is
  **false** for it — so `invoice\u{202E}fdp.exe` survives that filter and renders
  as `invoiceexe.pdf`. A grep of `crates/` and `src/` finds no handling of bidi
  or format characters anywhere in the tree.

  Two consequences, and the second is not optional:
  1. The correspondence quarantine adds a format-character rule
     (strip `Cf`, and the U+2066..U+2069 isolates, before and after NFC).
  2. **This is a live gap in the existing attachment path**, not only a
     correspondence one — a member can attach a file with an RLO in its name
     today. It is small, it is in the shared sanitizer, and its test belongs in
     `destination.rs`'s own `mod tests` "beside the sanitizer rather than in the
     product, because the product is one caller and the property is about the
     function" (`destination.rs:266-268`). Raise it as its own issue; do not let
     it ride in on a correspondence branch.

**`From` / `Return-Path` / DKIM / SPF / DMARC are recorded, never trusted.** The
governing precedent is exact and already written down for a different plane:

> `from` is the **delivering neighbor** — the last hop in the gossip overlay, a
> routing hint, never an authenticated author … the transport does not
> authenticate payloads.
> — `crates/comms/src/lib.rs:175-180`

So `ExternalChecks` is a contractor *report*, `AssertedEnvelope` is a
*claim*, and neither ever produces an `ActorId`. There is no code path from an
inbound header to a member identity. The gate test for this is a grep-class
boundary check in the `orbital_boundaries.rs` style
(`tests/it/orbital_boundaries.rs:187-207`): `ingress.rs` and `provenance.rs` must
not name `ActorId` in any position that constructs one.

### 5.4 `Provenance` — a field, not a boolean

```rust
// crates/correspondence/src/provenance.rs

/// How this material arrived and what was verifiable about it.
///
/// Three variants because there are three answers. A boolean collapses
/// `Stranger` and `Imported`, and that collapse is exactly the bug: an import
/// *contains* strangers' assertions, and flattening them upgrades a stranger's
/// claim to the importer's word.
#[non_exhaustive]
pub enum Provenance {
    /// Signed by a member of this Space, and the signature was checked at the
    /// referenced frontier. The only variant that carries an `ActorId`, and it
    /// is obtained from `replica`'s receipt — never from a header.
    Member { actor: ActorId, receipt: ReceiptRef },

    /// Asserted by somebody this Space never admitted. Carries what the
    /// contractor claimed and what it checked; neither is identity.
    Stranger {
        asserted: AssertedEnvelope,
        checks: ExternalChecks,       // dkim/spf/dmarc verdicts, each tri-state
        confusable: ConfusableFinding, // §5.3
    },

    /// Brought in from an archive (mbox / Takeout). Distinct from `Stranger`
    /// because the *importer* is a member and the *original sender* is not:
    /// two different trust facts that a two-state field would merge.
    Imported {
        run: ImportRunId,
        source: ImportSource,
        original: Option<Box<Provenance>>,  // usually a `Stranger`
    },
}
```

**How every surface renders the difference.** Not by convention — by making the
name unobtainable without its attribution:

```rust
/// The only way to display a correspondent. Total, non-`Option`, no `Default`.
pub enum Attribution {
    Verified { display: String, actor: ActorId },
    Asserted { display: String, caveats: Vec<Caveat> },   // never empty-caveat
    Imported { display: String, run: ImportRunId, caveats: Vec<Caveat> },
}

impl Provenance { pub fn attribution(&self) -> Attribution; }
```

Rules that make this stick:

- `AssertedEnvelope` implements **no** `Display` and no `Serialize` into a DTO
  field of its own. The head, the viewer and MCP receive one `Attribution`
  struct, so a surface cannot render a name and forget the caveat — they are the
  same value. This is the structural form of
  `docs/THREAT-MODEL.md:246-248`: "An actor id inside unsigned application
  content is not cryptographic proof of authorship."
- `Caveat` is non-empty for every non-`Verified` variant by construction (the
  constructor takes a `Vec1`-shaped argument or the variant is unbuildable), so
  "asserted, no caveats" cannot be spelled.
- The viewer's rendering of `Asserted` must be visually distinct at a glance and
  must not be a tooltip. That is the design slice's call, but the *type* forces
  the question to be answered rather than defaulted.

---

## 6. The one dependency-direction problem, stated rather than hidden

Ingress must use `sanitize_display_name`
(`crates/world-interface/src/destination.rs:49`) — the same function, because
the whole precedent for this architecture is that the property belongs to the
function and not to a caller.

But `world-interface` depends on `replica` **and `runtime`**
(`crates/world-interface/Cargo.toml`), and correspondence must sit below runtime
(§7). So `correspondence → world-interface` drags `runtime` in and inverts the
layering.

`destination.rs` is 295 lines of pure `std` — no dependencies at all. It lives in
`world-interface` only because that is where its one caller lived. With a second
caller below it, the options are:

- **(A) Move the module to `crates/replica`, re-export from `world-interface`.**
  Recommended. `replica` is what both can see; it already owns the content plane
  (`crates/replica/src/content.rs`), whose doc records that filename/MIME/
  disposition are product metadata beside a `ContentRef` — so the *repair* of a
  peer-authored name sits beside the content it names. `replica`'s own deps
  (mechanics, fabric, journal) are all pure, so nothing new enters. Cost: one
  file move plus `pub use replica::destination;` in `world-interface`, both
  covered by the existing gates.
- **(B) `correspondence → world-interface`.** Rejected: correspondence would sit
  above runtime, which forbids runtime driving the plane (§7).
- **(C) Duplicate the sanitizer.** Rejected outright — it is the precise mistake
  `destination.rs:266-268` exists to prevent, and it already cost one
  regression.

Flagging (A) as the single cross-slice decision this blueprint needs from
whoever owns crate layout.

---

## 7. Where it sits in the crate graph

```text
journal          durability
mechanics        legitimacy — identity, authority, custody   (+ mechanics::spend, §4.1)
fabric           the shared world — loro sealed
replica          Body semantics, content plane, (+ destination, §6)
├── correspondence        material crossing the Space boundary        [NEW]
│     └── correspondence-post   the ONLY crate naming a mail protocol [NEW]
└── comms        bytes between replicas — iroh sealed
runtime          Orbit/Station lifecycle; drives comms, and later drives correspondence
world-interface  package mounts, MCP descriptors, web parsers
products/        issues · a mail client · both merely callers
lait (root)      composes comms + DefaultFactory, and correspondence + PostFactory
```

Three directions, each stated because getting one backwards is the failure mode:

1. **`correspondence` does not depend on `comms`, and `comms` does not depend on
   `correspondence`.** They are siblings: two different boundaries. Peer-to-peer
   *inside* the Space, and the world *outside* it. A dependency either way would
   mean one boundary can name the other's vocabulary, and the first thing that
   would leak across is a `PeerId` used as a correspondent.
2. **`correspondence` does not depend on `runtime`.** So `runtime` may later
   depend on `correspondence` and drive the plane from a Station the way
   `contact_driver.rs` drives the transport, with no cycle. If this is inverted,
   the plane can never be driven from a Station.
3. **`correspondence` does not depend on `fabric`.** The plane produces
   `Admitted` values; turning one into a Body is `replica`'s, per
   `docs/ARCHITECTURE.md:220-222` — "Replica is the Body graph authority and is
   the only layer allowed to turn validated transactions into Engine changes."

## 8. Manifest edges

### 8.1 Root `Cargo.toml`

```toml
[workspace]
members = [
    "crates/journal",
    "crates/mechanics",
    "crates/fabric",
    "crates/comms",
    "crates/correspondence",        # NEW
    "crates/correspondence-post",   # NEW
    "crates/relay",
    "crates/replica",
    "crates/runtime",
    "crates/world-interface",
    "products/issues",
    "products/issues-app",
]
```

and, in `[dependencies]`, the comment mirroring `comms`'s at `Cargo.toml:105-113`:

```toml
# Material crossing the Space boundary. The app composes the seam
# (`Correspondent`, `Franked`, `Quarantined`) and never names the contractor
# behind it: no SMTP/IMAP/JMAP client and no DNS resolver appears in THIS
# manifest. Swapping the mail contractor is a manifest change, not a plane rewrite.
correspondence = { path = "crates/correspondence" }
# The shipped mail contractor, named here only to hand its factory to the
# composition root — exactly as `comms::DefaultFactory` is constructed at
# src/orbits/router.rs:561 and never named again.
correspondence-post = { path = "crates/correspondence-post" }
```

### 8.2 `crates/correspondence/Cargo.toml`

```toml
[package]
name = "correspondence"
version = "0.7.8"
edition = "2021"
rust-version = "1.91"
description = "The lait correspondence seam: material crossing the Space boundary, with both gates. Names NO mail protocol."
license = "MIT OR Apache-2.0"
publish = false            # same reasoning as crates/comms/Cargo.toml:8-15

[lints]
workspace = true

[dependencies]
# lait's roots. Identity crosses this boundary self-certifying, and the custody
# DECISION lives here (mechanics::spend) even though its resolution does not.
mechanics = { path = "../mechanics" }
# Body/content vocabulary: ContentRef for attachments, and the shared
# peer-authored-name sanitizer once it moves (§6).
replica = { path = "../replica" }

anyhow = "1"
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
postcard = { version = "1", features = ["use-std"] }
tokio = { version = "1", features = ["sync", "rt", "macros", "time"] }
tracing = "0.1"

# FORMAT libraries — permitted here because the quarantine's bounds ARE the
# security property (§1). None of these opens a socket or resolves a name.
mail-parser = "…"             # MIME structure only
ammonia = "…"                 # HTML allowlist sanitisation
unicode-normalization = "0.1" # NFC; already in the tree via runtime
unicode-security = "…"        # confusable skeletons (§5.3)
idna = "…"                    # U-label / A-label

# ABSENT BY CONSTRUCTION, and the absence is the seal:
#   any SMTP/IMAP/JMAP client, any DNS resolver, `iroh`, `loro`.

[dev-dependencies]
tokio = { version = "1", features = ["full"] }
proptest = "1"   # the MIME decoder is the outermost attack surface (§5.2)
```

### 8.3 `crates/correspondence-post/Cargo.toml`

```toml
[package]
name = "correspondence-post"
description = "The lait mail contractor. The ONLY crate that names a mail protocol."
publish = false

[dependencies]
correspondence = { path = "../correspondence" }
mechanics = { path = "../mechanics" }
# The concrete mail protocol. This is the only manifest in the workspace that
# lists it, so SMTP/IMAP/JMAP is nameable here and nowhere else — the seal is
# the manifest (crates/comms/Cargo.toml:26-29).
mail-send = "…"        # or jmap-client
hickory-resolver = "…" # MX + DKIM key lookup
anyhow = "1"
async-trait = "0.1"
tokio = { version = "1", features = ["sync", "rt", "macros", "time", "net"] }
```

### 8.4 The seal, made executable

Extend `tests/it/orbital_boundaries.rs`, which already has
`only_fabric_names_loro_in_its_manifest` (`:135`) and
`only_comms_names_iroh_in_its_manifest` (`:155`), reusing its `manifest_lists_dep`
helper (`:22-36`) and keeping its discipline of a passing control *plus* an
injected failing case:

```rust
#[test]
fn only_the_post_crate_names_a_mail_protocol_in_its_manifest() {
    for protocol in MAIL_PROTOCOL_CRATES {              // mail-send, jmap-client, imap, …
        assert!(manifest_lists_dep("correspondence-post", protocol) || …);
        for crate_dir in ["mechanics", "fabric", "replica", "runtime",
                          "comms", "correspondence", "world-interface"] {
            assert!(!manifest_lists_dep(crate_dir, protocol),
                    "{crate_dir} must NOT name {protocol}");
        }
    }
    assert!(!manifest_at(&workspace_root(), protocol));  // and not the root
}

#[test]
fn correspondence_resolves_no_names_and_opens_no_sockets() {
    for resolver in ["hickory-resolver", "trust-dns-resolver", "reqwest", "ureq"] {
        assert!(!manifest_lists_dep("correspondence", resolver));
    }
}
```

Two `PRODUCT_SYMBOLS`-style source scans (`:177-207`) alongside:
`correspondence` must not name `iroh`, `loro`, or `PeerId`; `ingress.rs` and
`provenance.rs` must not construct an `ActorId`.

---

## 9. THREAT-MODEL amendments

`docs/THREAT-MODEL.md` scopes every adversary to someone who is or was a member.
Four edits, drafted below. Per `:402` — "Security claims require executable tests
at the enforcing boundary" — each new claim carries its test in §9.5, and none of
these paragraphs should land ahead of the test that makes it true.

### 9.1 Trust boundaries — insert after `:28`

> A Space boundary that material crosses is still a boundary, and correspondence
> is the first plane on which it is crossed in both directions. Everything a
> correspondent asserts is untrusted in a stronger sense than a peer's
> assertions are: a peer is at least a key the Space once admitted, while a
> correspondent may be anyone with the ability to send a message. Envelope
> senders, `From`, `Return-Path`, `Message-ID`, DKIM, SPF and DMARC results,
> MIME structure, display names, filenames and every byte of a message body are
> attacker-chosen and are recorded as provenance rather than believed. A
> verification verdict is a contractor's report about a domain, never a
> statement about a person, and no code path leads from an inbound header to an
> `ActorId`.
>
> The contractor that carries this traffic is outside the boundary and is not
> trusted to have checked anything. Bounds, structural validation and
> sanitisation are performed by the quarantine on this side, on the assumption
> that the contractor performed none of them and may itself be lying about what
> it did.

### 9.2 Adversaries considered — append to `:38`'s list

> - a correspondent the Space never admitted and never will, sending
>   attacker-chosen bytes that are intended to become a Body: hostile MIME
>   structure (deep nesting, part floods, disagreeing lengths, ambiguous
>   transfer encodings), active HTML, tracking and probing resources, forged or
>   confusable sender identities, and filenames constructed to become paths or
>   to render as something other than what they are;
> - a correspondent who is legitimate and whose *messages* are not — a forwarded
>   or quoted hostile message, a bounce notification quoting attacker-supplied
>   content, or a mailing list relaying a stranger;
> - a mail contractor that misreports authentication results, reorders,
>   duplicates or drops deliveries, or observes everything it carries;
> - a local caller attempting to spend another actor's key on an outbound
>   message, which is the existing borrowed-key adversary on a plane with no
>   recall.

### 9.3 Intended properties — append to `:54`'s list

> - Foreign material becomes a Body only through the quarantine. There is no
>   ordering of calls in which an inbound message reaches Body construction
>   without structural bounds, HTML sanitisation and name repair having been
>   applied, because the value that carries it out of the quarantine is the only
>   value the plane accepts and the quarantine is its only constructor.
> - Provenance is recorded for every message and is a field, not a flag. It
>   distinguishes material signed by a member, material asserted by a stranger,
>   and material imported from an archive; an import carries the original
>   assertion nested rather than flattened, so importing never upgrades a
>   stranger's claim to the importer's.
> - No surface can display a correspondent's name without its attribution: the
>   name and its caveats are one value, so a head, a viewer or an agent cannot
>   render the first and drop the second.
> - An inbound header never resolves to an actor. DKIM, SPF and DMARC results
>   are provenance about a domain; membership comes only from signed Mechanics
>   history.
> - Remote resources referenced by an inbound message are never fetched — not at
>   parse and not at render — without an explicit per-message action by the
>   mailbox owner.
> - Outbound correspondence is never signed or submitted with a key the caller
>   is not entitled to spend. The question is custody, not standing, and the
>   answer stays no however wide anybody's grants become. Unlike the local web
>   surface, this gate has no read exemption, because no path through egress
>   observes without leaving.
> - An outbound message is bound to the (actor, Space) whose view produced it
>   and cannot be submitted through another actor's correspondent, so per-space
>   unlinkability cannot be lost to a plumbing error.
> - A contractor cannot bypass either gate. It cannot construct the value the
>   send path accepts and it cannot construct the value the Body path accepts;
>   both types live in a crate it depends on and neither has a public
>   constructor.

### 9.4 New section — insert after "Peer-authored names on local paths" (`:250`)

> ## Foreign material at the correspondence boundary
>
> The preceding section is about a name authored by a *peer* — someone the Space
> admitted, whose bytes arrived through a validated transaction. Correspondence
> introduces material authored by someone the Space never admitted, arriving
> through a contractor that authenticates nothing. Every property in that section
> holds here and is not sufficient here.
>
> **Structure is refused; content is repaired.** The distinction is the same one
> intake and save-time draw, and for the same reason — who the proposer is. A
> message whose MIME structure exceeds the plane's bounds is refused outright,
> because repairing structure means guessing what an attacker meant. A display
> name, a filename or an HTML body is repaired and never refused, because
> refusing hands the sender the ability to make their own message unreadable.
> Bounds are checked before allocation, so a part flood or a fifty-deep multipart
> costs a decode attempt rather than memory.
>
> **A verification result is not an identity.** `From`, `Return-Path`, DKIM, SPF
> and DMARC are recorded as provenance. A DKIM pass says a domain signed
> something; it does not say who, it does not survive forwarding, and the party
> reporting it is the contractor, which is outside the boundary. There is no
> code path from any of these to an `ActorId`, and that absence is checked
> rather than asserted.
>
> **A name is never shown without what is known about it.** Confusable and
> mixed-script sender names are recorded, not rejected — rejecting would let
> anyone make a legitimate correspondent unreachable by registering a lookalike —
> and every surface receives the name and its caveats as one value. Bidirectional
> and other format characters are stripped, which the shared filename sanitizer
> does not do: it filters `char::is_control`, which is category `Cc`, and
> U+202E RIGHT-TO-LEFT OVERRIDE is category `Cf`.
>
> **A rendered message body is attacker-authored content on the credentialed
> origin.** The section on files on the local web surface records why nothing on
> the content routes is ever rendered: one origin serves the viewer, the API and
> every attachment, and that origin holds the session credential. A message body
> is the same exposure with a new author. It is sanitised to an allowlist, its
> remote resources are suppressed rather than fetched, and until a surface exists
> that isolates it, it is served under the same never-rendered rules the content
> routes carry.
>
> **What this does not claim.** Deliverability, spam classification, and
> reputation are the contractor's and are not security properties here. Metadata
> is not hidden — who corresponds with whom, when, and how much is visible to
> the contractor and to every hop, and encryption does not change that. And the
> quarantine bounds what becomes a Body; it does not make an attacker's content
> harmless to a person who reads it.

### 9.5 Executable tests — one per claim (`:402`)

| Claim | Test | Enforcing boundary |
|---|---|---|
| No unquarantined material becomes a Body | type-level: `Admitted` has one constructor; a gate test parses `ingress.rs` and asserts it | `crates/correspondence/src/ingress.rs` |
| No send path precedes its gate | type-level: `Franked` has one constructor; gate test on `egress.rs` | `crates/correspondence/src/egress.rs` |
| Borrowed key refused on egress | `MemPost` + a `Borrowed` custody: `frank` refuses, `send` is unreachable | `egress::frank` |
| Egress has no read exemption | a test asserting every `frank` path is gated, i.e. no `is_read`-shaped predicate exists in the crate | `egress.rs` |
| Space binding on `Franked` | mint under (actor A, space X), submit through (actor A, space Y) ⇒ refused | `Correspondent::send` |
| Hostile MIME refused, not repaired | fixture corpus (depth, part flood, length disagreement, ambiguous CTE) + `proptest` structural fuzz | `ingress::admit` |
| Active HTML removed | corpus of script/style/svg/`on*`/`javascript:`/`data:`/CSS `url()` | the sanitiser |
| Remote images not fetched | `MemPost` contractor asserts zero outbound resolution attempts during `admit` | `ingress::admit` |
| Header assertion never becomes identity | source scan: `ingress.rs`/`provenance.rs` construct no `ActorId` | boundary gate, `orbital_boundaries.rs` style |
| Provenance survives quote/forward | quote an `Admitted` `Stranger`, frank the reply, assert the nested provenance | `egress::frank` |
| Import does not upgrade a stranger | import a stranger's message; assert `Imported { original: Some(Stranger) }` | import path |
| Attribution is inseparable from the name | compile-fail / API test: `AssertedEnvelope` has no `Display` and no standalone DTO field | `provenance.rs` |
| Bidi/format characters stripped | corpus incl. U+202E; **also** a regression test in `destination.rs`'s own `mod tests` | the sanitiser (both) |
| Only one crate names a mail protocol | manifest gate with control + injected failure | `tests/it/orbital_boundaries.rs` |
| Duplicate delivery is idempotent | `Faults { duplicate_percent }` run, assert one Body | `ingress`/mailbox intake |
| Custody refactor changes nothing | existing `src/orbits/catalog.rs:380-392` and `src/serve/mod.rs:1232-1258` pass unchanged after `signs_with_own_seed` delegates | `mechanics::spend` |

### 9.6 Other documents that must move with it

- `docs/ARCHITECTURE.md:189-209` — the crate-boundaries block gains
  `correspondence` and `correspondence-post`.
- `docs/ARCHITECTURE.md:466-476` — §8 records that the custody question now has
  one decision (`mechanics::spend`) and three enforcement points; the existing
  sentence naming `serve::borrowed_key_refusal` and `orbits::bootstrap::admit`
  gains `correspondence::egress::frank`.
- `docs/SERVE.md:186-196` and `docs/AGENT-EXPERIENCE.md:53-60` — same, plus the
  note that egress carries no read exemption.

---

## 10. Build order

Each step is independently revertible, and 1–3 contain no mail protocol code at
all.

0. **Decide §6** (move `destination` to `replica`, or not). One file, blocking
   step 4 only.
1. **`crates/correspondence` skeleton.** `lib.rs` vocabulary, `policy.rs`,
   `mem.rs` with `MemPost`/`MemCorrespondent`/`Stranger`/`Faults`/`Delivered`.
   `Franked` and `Quarantined` exist and are unconstructible. `Correspondent::send`
   exists and is uncallable. Manifest gate lands here.
2. **`mechanics::spend`** + `Catalog::signs_with_own_seed` delegates to it. Zero
   behaviour change, proven by the existing tests.
3. **`egress.rs`.** `frank` appears; sending becomes expressible for the first
   time. Refusal tests before anything can actually reach a contractor.
4. **`ingress.rs` + `provenance.rs`** against the hostile fixture corpus, with a
   `Stranger` as the adversary. Structural fuzz here, not later.
5. **`crates/correspondence-post`.** The first line of protocol code in the
   project, written against a seam that already has its tests.

Steps 4 and 5 are where the threat-model text in §9 lands — §9.4's paragraphs
describe behaviour that exists at the end of step 4, and `:402` is the reason
they should not be written before it.
