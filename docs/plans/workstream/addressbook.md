# Correspondence — the addressbook and the naming layers

Status: **scoping**. Nothing here is built. Local working note; current product
and protocol truth stays in the tracked documentation set.

Slice of the settled Correspondence design covering **the four naming layers**
and **the addressbook surface**. Siblings: `correspondence-mailbox.md` (the
mailbox primitive, Body schemas, D1 addressing decision), the sponsorship /
relations edge, the crate layout and contractor trait, credentials/passports.
Those are out of scope here and are cited, never relitigated.

The discipline this note enforces everywhere:

> **Handles route. Referents identify. Names are conferred.**

---

## 0. What is actually in the tree today

Findings first, because two of them change the design.

1. **Petnames live in `<home>/aliases.json`** — a flat
   `BTreeMap<String, String>`, written by `write_alias`
   (`src/orbital/hosting.rs:3756`) and read by `read_aliases`
   (`src/orbital/hosting.rs:3748`). `<home>` is the **Orbit store dir**
   (`.lait/`, `src/config.rs:56` `STORE_DIR`), not the identity dir. Petnames
   are therefore **per-Orbit**, not per-identity: one human with two Orbits
   keeps two disjoint name stores, and the same colleague must be named twice.
2. **That one flat map already mixes two id namespaces.**
   `Mechanics::members()` overlay looks entries up by **ActorId**
   (`src/orbital/hosting.rs:2015`), `agent_provision` writes an **ActorId** key
   (`src/orbital/hosting.rs:1706`), and `who()` looks the *same file* up by
   **DeviceId** (`src/orbital/hosting.rs:1725`, `:1775` —
   `n.station.as_device().to_string()`). There is no discriminator on the key.
   Nothing collides today only because `act_…` and 64-hex never prefix each
   other. It is a namespace waiting to be widened by a third subject, which is
   exactly what a handle is.
3. **The member overlay does prefix matching**
   (`src/orbital/hosting.rs:2020`, `m.key.starts_with(k.as_str())`). A stored
   key that is a prefix of two actor ids names both of them. `set_alias`
   resolves to a canonical id before writing (`src/orbital/hosting.rs:2074`)
   specifically to avoid this, so the read-side fallback exists only for
   entries written before that fix. It is a compatibility shim, not a feature.
4. **The classifier already agrees petnames are node-local.**
   `control::classify` puts `Request::MemberAlias` under
   `RequestOwner::Lifecycle` — "daemon process + node-local config" —
   alongside `SeedAdd`/`SeedList`/`ConfigReload`
   (`src/control.rs:1227`), *not* under `Mechanics` with `Members`/`MemberLog`.
   The new nouns inherit that owner.
5. **`crates/comms/src/policy.rs:173` is unrelated.** Its "one in-process
   address book" is iroh's `MemoryLookup` — an `EndpointId → SocketAddr/relay`
   reachability cache handed to `Endpoint::builder().address_lookup(…)`. It
   maps a device key to a *network path*. It holds no name, no actor, no
   external contact point, and it is discarded on restart. Different thing,
   same word. **Do not extend it and do not name the new store `AddressBook`
   in `comms`.** (`src/orbital/hosting.rs`'s `seeds.json` is the closer
   precedent for a durable node-local registry: `SeedRecord` at `:3663`,
   load/save at `:3683`/`:3708`, DTO at `src/dto.rs:124`.)
6. **No `handle` noun exists anywhere.** `grep` over `src/ crates/ products/`
   for email/phone/handle returns only `JoinHandle`-class hits and one prose
   use — `dto.rs:60` calls a `did:key` "the self-certifying, offline interop
   handle", and `control.rs:1435` calls an issue ref "the resolved canonical
   handle". Both are loose. The word is free; take it and define it once.
7. **The viewer gates petname editing behind `isAdmin`**
   (`viewer/src/ui/Members.tsx:150`). A petname is local, private, and unsigned;
   there is no authority question in it. This is a defect the new surface must
   not inherit — see §7.

---

## 1. The four naming layers

| Layer | Type today | Stored | Synced? | Signed? | Cardinality |
|---|---|---|---|---|---|
| **DeviceId** | `mechanics::ids::DeviceId` (64-hex ed25519 pubkey) | actor plane events; local seed in `<identity_dir>/secret.key` | yes (in signed actor events) | **yes** — it is the signing key | many per actor |
| **ActorId** | `mechanics::ids::ActorId` (`act_` + blake3(Incept)) | actor plane (`lait/actor/1`) | yes | **yes**, self-certifying | one per (human, Space) |
| **petname** | bare `String` in a `BTreeMap<String,String>` | `<orbit home>/aliases.json` | **no** | no | one per subject *per Orbit* |
| **handle** | *does not exist* | — | — | — | many per actor |

### 1.1 `DeviceId` — what signs

`crates/mechanics/src/ids.rs:8` states it: "Read a `DeviceId` as *which device*,
an `ActorId` as *who*." It is the ed25519 public key and the same bytes as the
iroh `EndpointId`, which is why `who()` can key presence rows by it. Consent
bindings (`crates/mechanics/src/actor.rs:132` `DeviceBinding`) are what make a
device a device *of* an actor, and the binding carries the device's own
signature, so a device is never claimed without consenting.

For the addressbook a `DeviceId` is a **subject you may name** (that is what
`who()` does today) but never a routing target for correspondence: a mailbox is
actor-keyed, and devices come and go under a stable actor.

### 1.2 `ActorId` — the referent

`crates/mechanics/src/actor.rs:22-27`. Self-certifying, per-Space, unlinkable
across Spaces. `Directory::actor_of_device` (`:399`) deliberately returns `None`
for an ambiguously bound device — "ambiguous binding forfeits attribution" —
which is the right shape for the addressbook too: when the mapping is
ambiguous, say nothing rather than guess.

It is essentially never spoken. `MemberDto.key` carries it and every viewer
surface draws `m.did ?? m.key` in a `<code>` block *underneath* the name
(`viewer/src/ui/Members.tsx:146`). That is the correct treatment and the new
surface should copy it exactly.

`did:key` (`dto.rs:30`, minted at `src/orbital/mechanics.rs:1266`) is a fifth
thing worth naming precisely so it does not get mistaken for a handle: it is a
**deterministic re-encoding of a DeviceId**, synced-safe, offline-verifiable,
and it identifies. It does not route. It belongs with the referents.

### 1.3 petname — the conferred name

`crates/mechanics/src/acl.rs:37` is the position:

> Names never enter this plane. The only synced identity facts are keys,
> actors, and signed ops; petnames live in each node's local alias store.

The MCP description (`src/mcp.rs:449`) says the same in the tool text: "local to
this device, never synced or part of the signed ACL". It is a `String` with no
newtype, no validation beyond `trim()`, and no subject kind.

**What must change and what must not.** The property — local, unsigned,
plural, revisable — must not change. The *representation* must: a flat
`String → String` map cannot carry a third subject kind, cannot carry more than
one name for a subject, and cannot say who told you the name. The addressbook
in §3 is that representation, and `aliases.json` migrates into it (§8).

### 1.4 handle — the missing layer

Everything below.

---

## 2. The handle noun

> An external point of contact that routes inbound material to an actor's
> mailbox and stamps outbound material leaving it.

### 2.1 Type

Two nouns, because direction changes the storage answer and merging them
produces a table where "is this mine" is a flag people forget to check.

```rust
// crates/correspondence/src/handle.rs

mechanics::prefixed_id!(
    /// A bound handle — an external address that routes into a mailbox here.
    HandleId, "hdl_"
);

/// The external network a handle lives on. Add-only (postcard positional),
/// exactly like `acl::Standing`.
pub enum Network { Mail, Sms, Mimi, Matrix }

/// A handle we hold. It ROUTES; it never IDENTIFIES.
pub struct Handle {
    pub id: HandleId,
    pub network: Network,
    /// Canonicalised per network by the contractor — never by the product,
    /// never by the viewer. One spelling, decided once.
    pub address: String,
    /// The actor whose mailbox this routes into. Always an actor here: a
    /// handle we hold is by definition ours.
    pub actor: ActorId,
    pub scope: HandleScope,
    pub state: HandleState,      // Active | Retired
    pub minted_ms: u64,
}

/// A handle *they* hold — a correspondent's address, learned from traffic or
/// typed in. Never authoritative about who they are.
pub struct Correspondence {
    pub network: Network,
    pub address: String,
    /// The local referent we have decided these addresses belong to. Local,
    /// revisable, and an assertion by this node only.
    pub subject: CorrespondentId,
    /// How we came to believe it — never a boolean. Mirrors the ingress
    /// provenance requirement in `correspondence-mailbox.md` §5.
    pub provenance: Provenance,
}

mechanics::prefixed_id!(
    /// A purely local referent for somebody with no `ActorId` — the
    /// addressbook's own "who". Minted here, never synced, never signed.
    CorrespondentId, "cor_"
);
```

`prefixed_id!` is already the repo's id grammar
(`crates/mechanics/src/ids.rs:105`), so `hdl_`/`cor_` sort and render like
every other id.

**Do not call the counterparty noun `Contact`.** `Contact` is taken: it is the
bounded direct transfer transcript in the comms model
(`docs/ARCHITECTURE.md:36`, `:404`, `:413`). Colliding on it would make
"a Contact with Alice" ambiguous between a sync session and a person.

### 2.2 Where it lives — the three-way split

The obvious question is "signed Space state or local?", and the honest answer
is that neither is right for all of it. Three categories, and the middle one
is new:

**(a) Bound handles are actor-private replicated state.**
Not signed Space authority, not node-local. A handle must survive device loss
and must be visible to *every device of its actor* — the ingress contractor may
run on any of them — but must be invisible to other members, because a
per-correspondent handle table is a map of who you talk to.

The mechanism already exists: seal the handle table to the actor's mailbox
DEK-slot set, exactly as the mailbox itself is sealed
(`crates/mechanics/src/custody.rs:62-80`, `KeySlot::RecoveryKey { recipient,
wrapped_dek }` — one DEK, many independent unwrap paths, adding a device never
re-encrypts). One Body, one DEK, one slot per device of the actor. It
converges across the actor's devices and decrypts for nobody else.

This is the category that keeps `acl.rs:37` true: nothing enters the ACL plane,
and Mechanics never learns an address.

**(b) One optional published anchor is Space-visible.**
A colleague needs *some* way to reach you. Exactly one handle per (actor,
network) may be marked `published`, which copies its address into a
Space-readable (epoch-sealed, unsigned, non-authority) Body that the members
view renders. Publication is an explicit act with its own verb — never a side
effect of minting — because it is the moment a routing fact becomes a
correlatable fact for everyone in the Space.

**(c) Correspondent handles are node-local, in the addressbook (§3).**
Never synced by default. They are assertions this node makes about strangers,
and the sync of an unverified assertion about a third party is how one member's
mistaken attribution becomes everyone's.

### 2.3 Cardinality

Many per actor, unbounded, and the count is not user-visible in the
per-correspondent mode — the point of the mode is that you stop thinking about
it. Bounded in practice by one row per (network, correspondent).

### 2.4 Granularity — the trade

The brief lists four axes. The first collapses immediately: **"per external
network" is not a scope**, because `network` is already a field on the type, so
every handle is per-network by construction. A mail address and a phone number
are separate rows in every design. Three real options remain.

```rust
pub enum HandleScope {
    /// One handle for this actor in this Space, on this network.
    Space,
    /// One handle per counterparty.
    Correspondent(CorrespondentId),
    /// One handle per thread.
    Conversation(ConversationId),
}
```

**(1) Per (actor, Space).** One address, e.g. `omar.eng@…`.

- *For*: works with a plain single-address IMAP/SMTP account — no catch-all
  domain, no relay, no minting API. Reply-to is stable, so humans do not get
  confused. Trivially explainable. It is the floor `correspondence-mailbox.md`
  D1(a) already settles: compartmentalisation *between Spaces*.
- *Against*: every counterparty who ever writes to you holds the same string.
  Any two of them can confirm they are talking to the same person; a leak or a
  breach at one correlates all of them; a spammer who gets it cannot be cut off
  without burning the address for everybody. Within a Space you get zero
  compartmentalisation, which is where most correspondence actually is.

**(2) Per counterparty.** A distinct address minted the first time you
correspond with someone.

- *For*: this is the shipping pattern — Apple Hide My Email, Firefox Relay,
  SimpleX pairwise queues. It is the exact unit at which the correlation harm
  occurs, so it is the exact unit at which to defend. Burning one address cuts
  off one sender and costs nothing else. Two counterparties cannot confirm they
  reach the same person. It maps one-to-one onto `CorrespondentId`, which the
  addressbook needs anyway, so it adds no new referent.
- *Against*: it requires a contractor that can *mint* addresses — a catch-all
  domain, or a relay with an API. Plain IMAP against one mailbox cannot do it.
  Deliverability and DMARC alignment get harder the more addresses you send
  from. And a human who has to *tell* somebody an address out loud has nowhere
  to get one from, because the counterparty does not exist yet.

**(3) Per conversation.** A distinct address per thread.

- *For*: strictly the finest compartmentalisation.
- *Against*: thread identity in mail is not a thing you can rely on.
  `References`/`In-Reply-To` chains break on every mail client that has ever
  shipped, so the address space grows without bound while the address→thread
  map degrades. You would be minting a permanent identifier keyed on a
  heuristic. SMS and voice have no thread noun at all. Reject.

### 2.5 Recommendation

**Per counterparty, with the (actor, Space) handle retained as the anchor.**

Concretely:

- Every actor gets one `HandleScope::Space` handle per network at mailbox
  provisioning. It is the one you can say out loud, the one that goes on a
  business card, and the only one eligible to be *published* (§2.2b). Inbound
  to it is accepted and is the path by which a *new* correspondent reaches you.
- The first time material is exchanged with a correspondent, a
  `HandleScope::Correspondent` handle is minted **lazily** and becomes the
  stamp on all subsequent outbound to them and the preferred inbound route.
  Established handles are never rotated — churn is how this pattern fails
  socially.
- Minting is a **contractor capability, not a promise**. Declare it on the
  contractor trait so a deployment that cannot mint degrades honestly instead
  of silently falling back:

  ```rust
  pub enum Minting {
      /// One address, fixed. Only `HandleScope::Space` is available.
      Fixed,
      /// A catch-all domain: any local-part routes here. Mint freely.
      CatchAll { domain: String },
      /// A relay with a minting API (Hide-My-Email-shaped).
      Api,
  }
  fn minting(&self) -> Minting;
  ```

  Under `Fixed`, `handle_mint` with a correspondent scope refuses with a
  message naming the deployment limitation, and everything still works at
  granularity (1). The UI must render which mode is in force — a user who
  believes they have pairwise addresses and does not is worse off than one who
  knows they have one.

This is the same shape as the D1 decision one level down: D1 compartmentalises
*between Spaces* by binding a mailbox to (actor, Space); this compartmentalises
*within* a Space by binding a handle to (actor, Space, correspondent).

---

## 3. The addressbook

> The **local, unsynced** store binding conferred names to known handles and
> known referents.

One store, per **identity**, not per Orbit. This is a deliberate departure from
`aliases.json` (finding §0.1): a person you know is a person you know in every
Orbit under this identity, and the current per-Orbit split makes you name every
colleague once per store. It lives beside the other identity-scoped local state
— `config::identity_dir()` (`src/config.rs:366`), the same directory
`secret.key` sits in — as `addressbook.json`, and is loaded/saved with the
`load_seeds`/`save_seeds` discipline (`src/orbital/hosting.rs:3683`): an absent
file is `Ok(empty)`, an unreadable file is `Err`, and a write refuses on `Err`
so a transient parse failure never becomes a permanent deletion.

### 3.1 Shape

The fix for the flat map (findings §0.2, §0.3) is a **typed subject**:

```rust
/// What a name is conferred on. The discriminator `aliases.json` never had.
pub enum Subject {
    Actor(ActorId),
    Device(DeviceId),
    Correspondent(CorrespondentId),
}

/// One card. Names are plural because names are conferred and people confer
/// more than one; `display` is which of them this node draws.
pub struct Card {
    pub subject: Subject,
    pub names: Vec<String>,
    pub display: usize,
    /// External addresses attributed to this subject. For an `Actor` subject
    /// these are the handles *they* published; for a `Correspondent` they are
    /// what we learned from traffic.
    pub handles: Vec<Correspondence>,
    /// Other referents believed to be the same subject — including the
    /// cross-Space link `actor.rs:24` explicitly assigns to this store.
    pub also: Vec<Subject>,
    pub note: String,
}
```

`also` is where cross-Space linking lives. `crates/mechanics/src/actor.rs:24`
says the same human in two Spaces is two unlinkable actors and that
"cross-space linking is a local address-book concern, never protocol state".
This field is the concrete discharge of that sentence, and it is the reason the
store must be identity-scoped rather than Orbit-scoped — a per-Orbit store
cannot hold a link between two Orbits' actors.

**Relations are not names.** The settled model says sponsorship (clientage) is
an explicitly named relation, never smuggled into a name. So a `Card` carries no
`sponsor` field: sponsorship is signed ACL state
(`crates/mechanics/src/acl.rs:26-34`, rendered from `MemberDto.sponsor`,
`src/dto.rs:40`) and the addressbook must not shadow it with a local guess. The
relations slice owns that edge; this store stays a naming store.

### 3.2 Not `comms`

Restating finding §0.5 because the name will attract it: the address book at
`crates/comms/src/policy.rs:173` is iroh's `MemoryLookup`, an
`EndpointId → route` cache for dialing. It is transient, holds no names, and
lives on the wrong side of the contractor seam. The correspondence addressbook
shares nothing with it but a noun.

---

## 4. Surface

### 4.1 Control requests

Appended to `control::Request` (`src/control.rs`), variants add-only per
`docs/ARCHITECTURE.md:481`. All six classify as `RequestOwner::Lifecycle` —
node-local config, the arm `MemberAlias` is already in (`src/control.rs:1227`).

```rust
/// The local addressbook: every card, its conferred names, and the handles
/// and referents bound to it. Local to this identity, never synced.
BookList,
/// Confer (or clear, with an empty name) a local name on any subject —
/// an actor id, a device id, a `cor_` referent, or a handle that resolves
/// to one. The general form of `MemberAlias`.
BookName { subject: String, name: String },
/// Bind a known handle or a second referent to a card.
BookBind { subject: String, other: String },
/// Forget a card entirely.
BookForget { subject: String },

/// Our bound handles — what routes into a mailbox here.
HandleList { actor: Option<String> },
/// Mint one. `scope` is `space` or a `cor_` referent; refuses under a
/// `Minting::Fixed` contractor with the limitation named.
HandleMint { network: String, scope: String },
/// Stop accepting inbound and never stamp outbound with it again.
HandleRetire { handle: String },
/// Copy a handle's address into Space-visible state so colleagues can
/// reach you. Its own verb because it is its own decision (§2.2b).
HandlePublish { handle: String, published: bool },
```

Three exhaustive matches must be extended or the build fails, which is the
intended guard rail:

- `control::classify` (`src/control.rs:1169`) — all eight to `Lifecycle`.
- `serve::policy::is_read` (`src/serve/policy.rs:19`) — `BookList` and
  `HandleList` are reads; the other six are not. Note `MemberAlias` sits on the
  *not*-a-read side (`src/serve/policy.rs:50` region) even though it signs
  nothing; keep the new writes on the same side for consistency.
- `serve::policy::is_space_plane` (`src/serve/policy.rs:200` region) — all
  eight, beside `MemberAlias`.

Also update `control::representative_requests()` (`src/control.rs:1261`), whose
list drives the classification test and regenerates
`docs/plans/generated/request-routing.tsv` (`tests/it/control_classification.rs:50`).

### 4.2 Responses and DTOs

New `Response` variants (`src/control.rs:1417`), following `Seeds`/`Members`:

```rust
/// The local addressbook (reply to `BookList`).
Book { cards: Vec<CardDto> },
/// Bound handles (reply to `HandleList`).
Handles { handles: Vec<HandleDto> },
```

DTOs in `src/dto.rs` beside `MemberDto`/`SeedDto` — that module is explicitly
"Space-substrate projections … the DTOs the navigation shell returns for
identity, membership, authority, and pinned remotes" (`src/dto.rs:1`), which is
where naming belongs.

```rust
pub struct CardDto {
    /// `actor` | `device` | `correspondent` — the discriminator the flat
    /// alias map never had.
    pub kind: String,
    /// The subject's canonical id.
    pub subject: String,
    /// The name this node draws. Empty when unnamed — the viewer already
    /// renders that state ("unnamed", `Members.tsx:123`).
    pub name: String,
    /// Every conferred name, `name` first. Names are plural.
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub handles: Vec<HandleDto>,
    /// Other referents believed to be this subject, including cross-Space.
    #[serde(default)]
    pub also: Vec<String>,
    /// Whether this subject is a member of the Space on screen. Rendered,
    /// never a gate.
    pub member: bool,
}

pub struct HandleDto {
    /// `hdl_…` for one of ours; absent for a correspondent's.
    #[serde(default)]
    pub id: Option<String>,
    /// `mail` | `sms` | `mimi` | `matrix`.
    pub network: String,
    pub address: String,
    /// `space` | `cor_…` — the granularity this handle was minted at.
    pub scope: String,
    /// `active` | `retired`.
    pub state: String,
    /// Ours (routes into a mailbox here) vs. theirs.
    pub ours: bool,
    /// Space-visible so colleagues can reach you (§2.2b). Always false for
    /// a correspondent's handle.
    pub published: bool,
    /// How we came to hold it: `minted` | `published` | `observed` | `typed`
    /// | `imported`. Provenance is a field, never a boolean.
    pub provenance: String,
}
```

`MemberDto.alias` (`src/dto.rs:44`) stays exactly as it is — the wire shape is
mirrored by hand in `viewer/src/types.ts` and changing it breaks the members
view for no gain. It becomes a *projection* of the addressbook's `Actor` card
rather than a direct `aliases.json` read.

### 4.3 MCP tools

Extend the family, do not rename it. Existing shell tools are bare snake_case
noun_verb — `member_add`, `member_remove`, `member_log`, `member_alias`,
`seed_add`/`seed_list`/`seed_remove`, `device_add`/`device_list`/`device_revoke`
(`src/mcp.rs:161-178`). New tools, added to that `const` list and to the
`#[tool]` impl block:

| Tool | Args | Notes |
|---|---|---|
| `book_list` | — | the addressbook |
| `book_name` | `{ subject, name }` | general form of `member_alias`; empty clears |
| `book_bind` | `{ subject, other }` | bind a handle or a second referent |
| `book_forget` | `{ subject }` | |
| `handle_list` | `{ actor? }` | ours |
| `handle_mint` | `{ network, scope }` | refuses under `Minting::Fixed` |
| `handle_retire` | `{ handle }` | |
| `handle_publish` | `{ handle, published }` | |

`member_alias` **stays**, unchanged, delegating to `BookName` with an actor
subject. It is in the public tool list, in `viewer/src/types.ts:1004`, and in
`CHANGELOG.md:1378`; removing it buys nothing. Its description
(`src/mcp.rs:449`) needs one correction: it says "local to this device", and
after §3 the store is local to this *identity*, spanning that identity's
Orbits.

Tool descriptions must carry the discipline, because an agent reads them as the
spec: `handle_*` says *routes, never identifies*; `book_name` says *conferred,
local, never authoritative*; neither mentions the other's job.

### 4.4 HTTP

All eight ride the **Space plane**, `POST /api/spaces/{id}/rpc`
(`docs/SERVE.md:49`), beside `member_alias`. No new route, no new plane. The
`{id}` is a local Orbit id, and although the addressbook store is
identity-scoped, the *reads* are Space-flavoured (a card says whether its
subject is a member of the Space on screen), so the Orbit route is the honest
one.

Not the host plane: `policy::is_host_plane` is an allowlist narrowed by
vocabulary (`docs/SERVE.md:80`) and nothing here is formation.

Not the world plane: naming is substrate, not product. Adding `book_*` to the
`issues` mount would put a naming vocabulary inside a package that must be
removable (`docs/ARCHITECTURE.md:335`).

**The custody fence applies unchanged.** `handle_mint`, `handle_publish` and
outbound stamping spend an actor's addressing authority; a head serving
somebody else's token must refuse them exactly as it refuses a write
(`docs/SERVE.md:168`, `serve::borrowed_key_refusal`,
`Catalog::signs_with_own_seed`). `book_*` is node-local naming and signs
nothing — but it writes into *this identity's* store, so it follows
`MemberAlias`'s existing classification rather than inventing a third answer.

---

## 5. The viewer

**Settings, as a new tab. Not a new `View`.**

`viewer/src/core/registry.ts:31` holds the `View` union — `overview | list |
board | calendar | timeline | projects | inbox | my-issues | activity | specs |
settings`. Every member of it except `settings` is a way of looking at *work*:
issues drawn four ways, plus the projects/inbox/activity/specs surfaces over
them. An addressbook is not a view of work, and adding it to that union would
put a naming surface in the sidebar's work tree, in `PROJECT_VIEWS`
adjacency, and in the route grammar's project nesting
(`/spaces/:space/projects/:project/…`), none of which it belongs in.

`Settings` already owns the identity surfaces and its tab union is
`"general" | "teams" | "members" | "devices" | "labels" | "workflow" |
"access"` (`viewer/src/ui/Settings.tsx:33`, `TABS` at `:35`, rendered at
`:113`/`:163`). Add **`"contacts"`** between `members` and `devices` — the
three identity tabs then read outward: who is *in* the Space (members), who we
*know* (contacts), what *we* are (devices).

Why a sibling of Members rather than an expansion of it: Members is a
projection of the signed ACL and must stay one — every row there is
cryptographically backed. The addressbook is the opposite: every row is a local
assertion. Putting a `cor_` stranger in the members table would make the one
surface whose rows are all verifiable into one where some are and some are not,
which is precisely the distinction §5 of `correspondence-mailbox.md` demands be
renderable.

The tab needs three regions:

1. **Cards** — name, subject id in a `<code>` line beneath (copying
   `Members.tsx:146`'s `m.did ?? m.key` treatment), handles as chips, a
   `member` badge where the subject is also in the ACL.
2. **My handles** — the bound-handle table, with the minting mode stated in
   prose at the top ("this deployment issues one address" vs. "a new address
   per person"), because a user who thinks they have pairwise addresses and
   does not is worse off than one who knows.
3. **Provenance** rendered per handle, never as an icon alone.

Driver hook: `lait:nav { tab: "contacts" }` works for free — `Settings.tsx:107`
already listens and validates through `isTab`. Add `contacts` to the `{ tab }`
enumeration in `CLAUDE.md`. (While there: `CLAUDE.md`'s list is already stale —
it omits `teams`, which shipped at `Settings.tsx:33`.)

`viewer/src/types.ts` needs the eight new `cmd` members on the `Request` union
(beside `member_alias` at `:1004`), on the `SpaceRequest` extraction (`:1068`),
and the two DTO mirrors. That file is hand-maintained against `src/dto.rs`
(`src/dto.rs:11`).

---

## 6. Conflicts with existing invariants

### 6.1 `acl.rs:37` — "petnames live in each node's local alias store"

**Not violated, and the design is shaped by it.** No name of any kind enters the
ACL plane. Bound handles are actor-private replicated state (§2.2a) sealed to
the actor's own DEK slots; Mechanics never sees an address. The published
anchor (§2.2b) is Space-visible epoch-sealed content, *not* signed authority and
not an ACL op — the same category as any other Body.

**The tension is real at one point** and should be recorded rather than
discovered: §2.2b makes a *routing* fact visible to every member. That is not a
name and confers no authority, but it is the first identity-adjacent fact
outside the local store. Two guards keep it honest: publication is an explicit
verb (never a side effect of minting), and only the `Space`-scoped anchor is
eligible, so the per-correspondent table — the map of who you talk to — is
never publishable at all.

### 6.2 `actor.rs:24` — "cross-space linking is a local address-book concern"

**Not violated; discharged.** `Card.also` (§3.1) is the store that sentence
names, and making it identity-scoped rather than Orbit-scoped is what lets it
hold a link between two Spaces' actors at all. The current `aliases.json`
location makes that sentence unimplementable, which is a quiet argument that
the file is in the wrong place.

**A per-(actor, Space) handle would violate it** if it were shared across
Spaces — one address reaching two Spaces' actors relinks what inception
deliberately separates. `correspondence-mailbox.md` D1(a) already settles this
in our favour: one mailbox per (actor, Space), so a handle is per-Space by
construction. Recorded here because the friction is real: a human then has
several addresses, and the temptation to "just use one" is exactly the
temptation D1 refuses.

### 6.3 The flat alias namespace (finding §0.2)

Adding handles to the current `BTreeMap<String,String>` **would** break it:
`who()` keys by `DeviceId` and `members()` by `ActorId` over the same file, and
an email address is a third unprefixed string that can collide with neither
safely nor legibly. The typed `Subject` in §3.1 is the prerequisite, not an
enhancement.

### 6.4 Migration is a compatibility question

`aliases.json` is written by shipped builds. The addressbook read path must
absorb it: on first load, import every entry, inferring `Subject::Actor` from
an `act_` prefix and `Subject::Device` from 64-hex, and dropping anything else
with a `tracing::warn!` (the `load_seeds` precedent, `src/orbital/hosting.rs:3696`).
The bare-prefix entries the read-side fallback exists for
(`src/orbital/hosting.rs:2020`) resolve against the roster at import time; ones
that resolve ambiguously are dropped, loudly. Keep writing `aliases.json` for
one release so a downgrade does not lose names.

### 6.5 Not a conflict, but adjacent

`MemberDto.alias` staying wire-identical means the members view keeps working
untouched while the store underneath changes. That is the seam to hold.

---

## 7. Defects in the existing petname layer

Small, real, and worth fixing with this work rather than around it.

1. **Petname editing is gated behind `isAdmin`** (`viewer/src/ui/Members.tsx:150`
   — the `{isAdmin && !readOnly && (` block wraps both the rename button and the
   remove button). A petname is private to this node and confers nothing; a
   viewer-standing member has exactly as much right to name their colleagues as
   an admin. The engine agrees — `set_alias` (`src/orbital/hosting.rs:2068`)
   checks no standing at all. Split the block: rename is ungated (still
   `!readOnly`, since it writes node state through a possibly-borrowed head),
   remove stays admin-only.
2. **Petnames do not span Orbits** (finding §0.1). Fixed by §3's identity-scoped
   store.
3. **`aliases.json` writes are not atomic** (`src/orbital/hosting.rs:3770`,
   a bare `fs::write`) and a failed write mid-way truncates the name store.
   `seeds.json` has the same shape (`:3708`). Write-temp-then-rename when the
   store moves.
4. **The MCP description says "local to this device"** (`src/mcp.rs:449`);
   after §3 it is local to this identity. One-line correction, but agents read
   these as spec.

---

## 8. Staging

1. **Typed subjects, no handles.** Land `Subject`, `Card`, the identity-scoped
   `addressbook.json`, the `aliases.json` import, `BookList`/`BookName`/
   `BookForget`, and `MemberDto.alias` as a projection of it. `member_alias`
   delegates. Fixes §7.1–§7.4. **No correspondence dependency at all** — this
   stage is shippable on its own and is worth shipping on its own.
2. **The handle type, no network.** `Handle`, `HandleScope`, `Network`,
   `handle_list`/`handle_mint`/`handle_retire` against a `Minting::Fixed` mock
   contractor. Proves the scope algebra and the refusal path with no SMTP
   anywhere — mirroring the `MemCorrespondence` discipline in
   `correspondence-mailbox.md` §2.
3. **Actor-private sealing.** Move the bound-handle table into the DEK-slot
   Body. Requires the mailbox slice; blocked on it.
4. **Correspondents and provenance.** `CorrespondentId`, `Correspondence`,
   `book_bind`, provenance rendering. Requires ingress.
5. **Publication.** `handle_publish` and the Space-visible anchor, with the
   §6.1 note landed in `THREAT-MODEL.md` first — `correspondence-mailbox.md` §8
   already lists the amendments this rides on.
6. **Per-correspondent minting.** `Minting::CatchAll` / `Minting::Api`, lazy
   mint on first exchange, the viewer's minting-mode prose.

Stage 1 is the whole of the naming cleanup and depends on nothing else in the
Correspondence programme. Everything after it is gated on a sibling slice.
