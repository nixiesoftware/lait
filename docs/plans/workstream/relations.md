# The sponsorship relation — walāʾ on the real surface

**Slice:** the *relation* between a sponsored agent and its sponsor. Not names, not
the addressbook, not the mailbox primitive, not the correspondence crate, not
credentials. Those are other agents' slices; this one is the edge itself.

**The design premise (settled, not relitigated here).** An `ActorId` is the
*referent* — self-certifying, `act_` + blake3(Incept), per-Space — and is not a
name. Relations between actors are never inferred from a name and never smuggled
into one; they are written with an explicit particle. lait's agent sponsorship
*is* walāʾ: clientage conferred by a patron, constitutive of the client's
standing, and — in the tradition — inalienable and inheritance-bearing in one
direction (patron inherits from client).

Verdict up front: **the relation already exists as a first-class signed fact.**
It is under-typed (one kind, no metadata), it is erased at exactly the moment it
becomes load-bearing (the cascade), and it is one raw op away from being
transferable. Everything below is against the real code.

---

## a) How sponsorship is modelled today

### The op

`AclAction::AddAgent { actor, grants }` — `crates/mechanics/src/acl.rs:169`.

The patron is **not a field**. It is `AclOp::by` (`acl.rs:483`), the actor the
signing device speaks for. So the relation is written by the *authorship* of the
op, and the op is signed under `ACL_DOMAIN` (`acl.rs:53`) with the device→actor
binding proven at the declared `actor_asof` frontier (`acl.rs:954-961`,
`acl.rs:1009`). This is a good shape: the particle cannot be forged separately
from the act of conferring, because it *is* the act.

`grants` defaults to `sponsored_agent_grants()` = `vec![Standing::Write]`
(`acl.rs:122-124`), the single policy site every caller reaches for.

### The authorization fence

`judge_op`, `acl.rs:1062-1188`. Two distinct invariants:

1. **The blanket agent-author ban** — `acl.rs:1082`:
   ```rust
   !agents_now.contains_key(by) && match &op.action { … }
   ```
   This one line is the *entire* reason a client holds no membership authority.
   Every ACL op — add, remove, set-grants, mint-epoch, revoke-invite, every
   policy grant/delegation/activation — is gated behind it. It is not a
   per-variant check; it is a prefix on the whole match.

2. **The AddAgent arm** — `acl.rs:1095-1101`:
   ```rust
   humans.contains(by)          // only a human member may confer
       && actor != by           // no self-clientage
       && !humans.contains(actor)   // the client is not already a principal
       && !agents_now.contains_key(actor)  // …nor already a client
       && is_sponsorable_grant_set(grants)
   ```
   `is_sponsorable_grant_set` (`acl.rs:130-132`) is `!grants.contains(&Standing::Admin)`.
   Tested at `acl.rs:2307-2333`: an injected `AddAgent` carrying `Admin` does not
   authorize, so no synced op can smuggle membership authority onto a client.

Note `humans` is seeded from `genesis.founding_actors` (`acl.rs:964-966`) and
grows only through `AddMember`/`SetGrants` (`acl.rs:1203`). A client is
deliberately never inserted into `humans` or `admins` (`acl.rs:1211-1217`) — the
comment there is explicit that pass-1 standing must not gain the agent.

### The materialized state

`AclState` — `acl.rs:513-557`. Two fields carry this:

```rust
members: BTreeMap<ActorId, BTreeSet<Standing>>,   // acl.rs:518
agents:  BTreeMap<ActorId, ActorId>,              // acl.rs:523 — client → patron
```

Every key in `agents` is also in `members` (`acl.rs:1571-1572`): a client holds a
*real* grant set through the same machinery, and the relation is orthogonal to
it. Accessors: `is_agent` (`acl.rs:639`), `sponsor_of` (`acl.rs:643`),
`is_human_member` (`acl.rs:648`), `agents()` (`acl.rs:680`). `standing()`
(`acl.rs:658`) returns `"agent"` as if it were a role — see the risk list.

Pass-1 continuation state is checkpointed as `ReplayCheckpoint::agents_now`
(`acl.rs:1331`), so the incremental replay path (`replay_continue`,
`acl.rs:1356`) carries the relation too.

### The cascade

Three separate places, in order:

- **In-pass (pass 1), `acl.rs:1218-1225`** — an authorized `RemoveMember` drops
  the subject from `humans`/`admins`/`agents_now` *and* retains-out every client
  of that subject, so an orphaned client cannot author later ops in the same
  replay.
- **Remove-wins (pass 2), `acl.rs:1653-1701`** — `AddAgent` counts as an add
  (`acl.rs:1673`, `acl.rs:1819`); a remove not causally succeeded by an add
  evicts, regardless of topo position.
- **The sponsor cascade proper, `acl.rs:1854-1866`** — runs **LAST**, after
  remove-wins *and* nonce-race eviction:
  ```rust
  let orphaned: Vec<ActorId> = agents.iter()
      .filter(|(_, sponsor)| !members.contains_key(*sponsor))
      .map(|(k, _)| k.clone()).collect();
  for k in orphaned { agents.remove(&k); members.remove(&k); }
  ```
  One pass suffices *because sponsors are never agents* (the `humans.contains(by)`
  fence on `AddAgent`). That is a load-bearing consequence of a) — a two-level
  clientage chain would silently break this loop. Guarded by the nonce-race test
  at `acl.rs:2479-2530`: a client sponsored by a race-loser must not survive
  orphaned.

Direct-sponsor retirement is also allowed without admin: `RemoveMember` authorizes
when `agents_now.get(actor) == Some(by)` (`acl.rs:1086-1088`) — a patron may
retire their own client. Correct and worth keeping: manumission is the patron's
act.

### Recorded invariants (all verified in code)

| Invariant | Where | Status |
|---|---|---|
| A client never holds `Admin` | `acl.rs:1100` + `acl.rs:130` | enforced at replay |
| A client authors no ACL op | `acl.rs:1082` | enforced at replay |
| A client dies with its patron | `acl.rs:1854-1866` | enforced at replay |
| A patron is never itself a client | `acl.rs:1096` (`humans.contains(by)`) | enforced at replay |
| No self-clientage | `acl.rs:1097` (`actor != by`) | enforced at replay |
| The head refuses to promote a client | `src/orbital/mechanics.rs:1414-1419` | enforced at the head only |
| Only a human member may confer | `src/orbital/mechanics.rs:1718-1720` | head; mirrored at replay |

---

## b) The explicit-particle problem — is the relation legible?

**Given only an `ActorId` and the signed ACL: yes to both questions, today.**

- **(i) "Is this actor a sponsored client at all?"** — `AclState::is_agent`
  (`acl.rs:639`). Derived from a signed `AddAgent`, not inferred from anything.
- **(ii) "Who is the patron?"** — `AclState::sponsor_of` (`acl.rs:643`), and
  independently recoverable from the audit trail: `AuditEntry { kind:
  "add_agent", by: <patron>, subject: <client> }` (`acl.rs:400-415`,
  `acl.rs:981-1001`), surfaced as `MemberLogEntry` (`src/dto.rs:104-119`,
  `src/orbital/mechanics.rs:1286-1306`).

So the particle **is** written. The Q 33:5 failure mode — a nisba that could mean
either descent or clientage — does not occur here, because a Space nisba is not
a relation claim in the first place: `Incept` establishes that the actor exists,
never that it has standing (`src/orbital/mechanics.rs:1780-1783`). A
provisioned-but-unsponsored agent resolves to its own actor and holds nothing
(`mechanics.rs:1171-1176`). Identity and relation are already separated in the
right place.

### Five real legibility gaps

1. **The relation has no kind.** `agents: BTreeMap<ActorId, ActorId>` is an
   untyped edge (`acl.rs:523`). The particle is spelled by the *field name* in
   Rust and by the *key name* on the wire (`sponsor`, `src/dto.rs:40`), not by a
   value. A second relation cannot be spelled at all (see §e).

2. **No metadata.** No conferral point, no terms. The op hash of the `AddAgent`
   is known during replay (`h` in `materialize_authorized`, `acl.rs:1551`) and
   thrown away. Compare `EpochAuth::mint_hash` (`acl.rs:440`), which keeps
   exactly this for exactly this reason ("does this causally descend X?").

3. **`role_label` erases it.** `acl.rs:135-143` maps grants → `admin | member |
   viewer`; a client reads as `member`. `mechanics::members()` (`mechanics.rs:1275`)
   uses it. Only the *separate* `sponsor` field disambiguates — which is the
   correct shape (role and relation are orthogonal axes), but it means **any
   consumer that renders `role` alone shows a client as an indistinguishable
   member**. That is the ambiguity the particle exists to prevent, reintroduced
   one layer up.

4. **`AclState::standing()` conflates the axes.** `acl.rs:658-670` returns
   `"agent"` *instead of* the grant label — a relation masquerading as a role.
   It is effectively dead (only `acl.rs:2052`/`2071` call it, and neither on an
   agent). Delete it or split it; do not let it grow a caller.

5. **The audit row drops the client's grants.** `entry.grants` is populated only
   for `AddMember`/`SetGrants` (`acl.rs:997-1001`, `acl.rs:1501-1506`), so an
   `add_agent` row shows `role: None` (`dto.rs:112-114`). The one log that proves
   the conferral does not say what was conferred.

### What to add

Promote the edge to a typed record. `AclState` derives `Serialize`
(`acl.rs:512`) and rides in `CheckpointObject.replay`, but
`checkpoint_commitment` covers only the *closure* — semantics, frontier,
effect set, actor events, space events (`crates/mechanics/src/ledger.rs:409-427`)
— **not** the materialized state. So a new field with `#[serde(default)]` does
not change any commitment and does not fork replicas. This is the single most
important compatibility fact for this slice.

```rust
/// A conferred relation between a client actor and its patron. Explicit,
/// signed, and never inferred: the tie is the `AddAgent` op's authorship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clientage {
    /// The patron — the op's `by`.
    pub patron: ActorId,
    /// Which relation. Append-only.
    pub kind: RelationKind,
    /// The conferring op's hash — its position in the causal graph, so
    /// "does X descend the conferral?" is answerable (cf. `EpochAuth::mint_hash`).
    pub conferred_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationKind {
    /// walāʾ — constitutive clientage. Dies with the patron; the patron
    /// takes the estate. The only kind today.
    Sponsorship,
    // Kafala — see §e. NOT added by this slice.
}
```

`AclState` change (`acl.rs:523`):
```rust
agents: BTreeMap<ActorId, ActorId>,          // keep: wire + checkpoint compat
#[serde(default)]
relations: BTreeMap<ActorId, Clientage>,     // add: the typed record
```
Populate both at `acl.rs:1563-1573` and in `apply_authorized`
(`acl.rs:1211-1217`). Keep `sponsor_of` delegating to `relations` with `agents`
as the decode fallback for an old checkpoint. `is_agent` becomes
`relations.contains_key(a) || agents.contains_key(a)` — and see the risk in §e
about what that predicate is load-bearing for.

**Do not** put the relation in the actor plane or the Incept. The referent is not
a name and must not acquire one; the relation belongs in the signed ACL where a
third party can verify who conferred it and when. Current placement is right.

---

## c) Inalienability

> walāʾ is like nasab — it is not sold and it is not gifted.

**Can the current code express a transfer?** In one op, no. In two, **yes** — and
the head permits it.

### What is correctly impossible

- No `TransferAgent`, `SetSponsor`, or `ReassignAgent` variant exists in
  `AclAction` (`acl.rs:147-256`). There is no op whose *meaning* is "the patron
  changes".
- `AddAgent` on an actor that is already a client is refused at replay
  (`!agents_now.contains_key(actor)`, `acl.rs:1099`) and at the head
  (`is_member` refusal, `mechanics.rs:1729-1731`). A second patron cannot
  overwrite the first while the first stands.
- `sponsor` is not a settable field anywhere: the patron is `AclOp::by`
  (`acl.rs:483`), signed. You cannot name a patron other than yourself.

### The two-op transfer (real, reachable from the product surface)

```
1. any admin        →  RemoveMember { actor: <client> }
2. any human member →  AddAgent    { actor: <client>, grants: … }   # a DIFFERENT patron
```

Step 1 clears the client from `humans`, `admins`, and `agents_now`
(`acl.rs:1218-1225`), which is exactly the state `AddAgent`'s arm requires
(`acl.rs:1098-1099`). Step 2 causally descends step 1, so remove-wins does not
evict (`acl.rs:1689-1700`), and the client is re-seated under the new patron. At
the head: `mechanics::member_remove` (`mechanics.rs:1349-1366`) then
`mechanics::agent_add` (`mechanics.rs:1712`) — whose `is_member` guard
(`mechanics.rs:1729`) is now false. **Both steps are ordinary product actions.**

Whether this is a violation depends on a design call this slice should make
explicitly:

- If the client's actor is treated as *the same client* (same key, same
  correspondence, same history), then this is a **sale of walāʾ** and must be
  forbidden.
- If removal is treated as **death** and re-sponsorship as a *new* client that
  happens to reuse a key, it is legitimate — but then the estate has already
  escheated (§d), and the new relation must not inherit the old one's material.

**Recommendation: forbid it, and make the ledger say so.** The tradition's reason
applies verbatim — the bond "cannot be removed by someone's action", and here
"someone" is *a third-party admin*, who is not even a party to the relation.

Fence: keep a grow-only tombstone of dissolved clientage and refuse re-conferral
to a *different* patron.

```rust
// AclState, new field
#[serde(default)]
former_relations: BTreeMap<ActorId, Clientage>,   // client → its last patron
```
Populated by the cascade (`acl.rs:1863-1866`) and by direct removal
(`acl.rs:1574-1577`). Then in `judge_op`'s `AddAgent` arm (`acl.rs:1095`), add:
```rust
&& former.get(actor).map_or(true, |prior| prior.patron == *by)
```
— re-sponsorship by *the same patron* stays legal (manumission then re-conferral
is the patron's own act, and is a no-op on the bond), re-sponsorship by anyone
else is unauthorized. Pass-1 needs `former` threaded alongside `agents_now`
through `judge_op`/`apply_authorized`/`ReplayCheckpoint`, mirroring `agents_now`
exactly (`acl.rs:1068`, `acl.rs:1197`, `acl.rs:1331`).

Grow-only and derived purely from the authorized op order, so it stays a pure
function of `(genesis, actor events, ops)` — the convergence property the module
header names (`acl.rs:16-21`).

### Where a future change would accidentally introduce a transfer

Four places. All should carry a comment naming this document.

1. **`SetGrants`/`AddMember` on a client actor severs the bond silently.**
   `acl.rs:1559-1562`:
   ```rust
   AclAction::AddMember { actor, grants } | AclAction::SetGrants { actor, grants } => {
       members.insert(actor.clone(), grants.iter().copied().collect());
       agents.remove(actor);            // ← emancipation, unnamed
   }
   ```
   plus `acl.rs:1203-1204` in pass 1 (`humans.insert`, `agents_now.remove`).
   `judge_op`'s `SetGrants` arm only checks `admins.contains(by)`
   (`acl.rs:1084`) — **any admin can convert somebody else's client into a full
   human member, in one op, and the ledger honors it.** The head declines to
   spell it (`member_set_role` refuses `is_agent`, `mechanics.rs:1414-1419`;
   `member_add` short-circuits on an existing member, `mechanics.rs:1331-1333`),
   so this is reachable only by a hand-built op — but the ACL is a *synced*
   plane and a peer can author whatever it likes. This is the single largest
   inalienability hole. Fix: refuse `SetGrants`/`AddMember` whose subject is in
   `agents_now`, in `judge_op`, next to the existing arm.

2. **`sponsored_agent_grants`' own doc-comment** (`acl.rs:99-121`) lists the three
   sites a re-granting feature would touch. It is a good map and it should
   additionally say: *widening grants must never be spelled as a re-`AddAgent`,
   because that is a re-conferral of the bond.* A future `SetAgentGrants` variant
   is the right shape — it changes the grant axis, never the relation axis.

3. **A second patron field.** If anyone ever adds `sponsor: ActorId` to
   `AddAgent` as a *field* (rather than reading `by`), transfer becomes
   expressible in one op by a third party. The patron must stay `AclOp::by`.

4. **Chained clientage.** The `humans.contains(by)` fence (`acl.rs:1096`) is what
   makes the one-pass cascade correct (`acl.rs:1857`). Relaxing it to let a
   client sponsor a sub-client both breaks the cascade's termination argument and
   creates a sub-let of walāʾ. Keep it, and say why at the fence.

---

## d) Escheat

> In walāʾ the patron inherits from the client.

### What happens today to an evicted client's authored content: **nothing.**

Content ops are authorized at **their own** historical frontier, never at current
state: `Authority::state_at` (`ledger.rs:1244`) resolves a checkpoint at the
referenced frontier and `signer_authorized_at` (`ledger.rs:1265`) asks
`StateView::signer_can_write` (`ledger.rs:662`) *there*. So an issue the client
filed while seated stays valid forever; eviction cannot retroactively invalidate
it, and `materialize_authorized` touches no content whatsoever. There is no
member-driven tombstoning anywhere — tombstones in this codebase are per-issue
deletes, not membership effects.

That is the right foundation. The problem is not the content; it is that **the
relation is destroyed at the exact moment it becomes load-bearing.** The cascade
(`acl.rs:1863-1866`) deletes the `agents` entry *and* the `members` entry. After
it runs:

- `sponsor_of(client)` → `None`. The bond is gone from materialized state.
- `mechanics::members()` (`mechanics.rs:1256-1283`) emits no row — it iterates
  `acl.members()`.
- The viewer's `sponsorName` lookup (`viewer/src/ui/Members.tsx:272-275`) finds
  nothing; issue attributions fall through to a bare `act_…`.
- The estate is **orphaned, not inherited**: `IssueDto.author` /
  `views.rs:68` / `dto.rs:546` / `created_by` (`dto.rs:645`) / `assignees`
  (`dto.rs:486`, `dto.rs:639`) all still name an actor nobody can explain.

The fact survives only in the audit log (`member_log`, `mechanics.rs:1286`), which
must be scanned to answer "whose client was this?". Recoverable, not legible.

### The mapping to get right

**Escheat is of the estate, never of the name.** In the tradition the mawlā keeps
his own name — walāʾ is not adoption, that is the whole point of Q 33:5 — and the
patron inherits his *property*. So:

- `author` / `created_by` must **never** be rewritten to the patron. Reattributing
  authorship is forging provenance, and it is precisely the adoptive filiation the
  model rejects. The client keeps its name on its work, permanently.
- What escheats is **standing over the material**: the open assignments, the
  ownership of anything the client held, and (later, another slice) the mailbox.

### What escheat requires — concretely

**1. The relation must outlive the cascade.** Reuse the `former_relations` map
from §c; the same field serves both. Populate it in the cascade at
`acl.rs:1863-1866` before the deletion:

```rust
for k in orphaned {
    if let Some(c) = relations.remove(&k) { former_relations.insert(k.clone(), c); }
    agents.remove(&k);
    members.remove(&k);
}
```
Add `former_patron_of(&ActorId) -> Option<&Clientage>` beside `sponsor_of`
(`acl.rs:643`).

**2. Replay names the obligation; it cannot discharge it.** Replay is pure
(`acl.rs:16-21`) and authors nothing. The codebase already has the exact
precedent: `RekeyFence` (`acl.rs:465-473`) — replay *names* a rekey obligation,
`rekey_fences()` (`acl.rs:619`) exposes it, an admin discharges it by rotating,
and replay retires it when an epoch causally descends the fence
(`acl.rs:1886-1892`). Mirror it:

```rust
/// An estate raised by the cascade: `client` was evicted with its patron,
/// so its durable material escheats to `patron`. Replay only names it;
/// the patron (or an admin) discharges it with ordinary content ops.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Estate {
    /// The evicted client.
    pub client: ActorId,
    /// The patron who takes the estate — the last conferring patron.
    pub patron: ActorId,
    /// The conferral's op hash, so the claim can prove the bond it rests on.
    pub conferred_at: String,
}
```
`AclState { #[serde(default)] estates: Vec<Estate> }`, sorted + deduped like
`rekey_fences` (`acl.rs:1851-1852`), with `estates()` beside `rekey_fences()`.
Discharge condition, evaluated the same way: retire an `Estate` once a content
frontier records the claim. **Design note:** unlike `RekeyFence`, the discharging
act lives in the *content* plane, which the ACL replay cannot see. Two options —
(a) leave `estates` grow-only and let the World's projection decide "claimed",
or (b) add an `AclAction::ClaimEstate { client }` so the discharge is itself a
signed ACL op and replay stays self-contained. **Prefer (b)**: it keeps the
obligation and its discharge on one plane, it is a new append-only variant (safe,
see §e), and it makes the claim a signed, third-party-verifiable act — which is
what an inheritance record has to be.

**3. The claim itself is ordinary content authorship by the patron.** Nothing
new in the content plane: the patron already holds `Write`, so reassignment of
the client's open issues is a normal `issue_assign`. What is new is a surface
that enumerates the estate (§f) and a projection that renders the provenance:
`authored by scout — client of Omar; estate held by Omar`.

**4. The patron must still be seated.** If the patron was removed (which is what
triggered the cascade in the first place — the common case!), the estate has no
taker. Two sub-cases, and the design must pick:
- Patron removed → escheat to **the Space** (the surviving admins), the classic
  "no heir" fallback. `Estate.patron` becomes `Option<ActorId>`, `None` = the
  Space.
- Patron's *own* removal is what evicted the client, so the estate should land
  wherever the patron's own material landed. This couples to the human-removal
  story, which today has no escheat at all. **Recommend: `patron: Option<ActorId>`,
  `None` handled by any admin.** Do not silently drop the estate.

### Cost summary

| Piece | Where | Size |
|---|---|---|
| `Clientage` + `RelationKind` | `acl.rs` new types | small |
| `relations` / `former_relations` / `estates` on `AclState` | `acl.rs:513-557` | small, `#[serde(default)]`, no commitment change (`ledger.rs:409-427`) |
| Thread `former` through pass 1 | `acl.rs:1062`, `:1192`, `:1319`, `:1434` | mechanical, mirrors `agents_now` exactly |
| Populate at the cascade | `acl.rs:1863-1866` | ~6 lines |
| `AclAction::ClaimEstate` + `judge_op` arm | `acl.rs:147`, `:1062` | one appended variant + one arm |
| Accessors + DTO + routes + viewer | §f | the bulk of the diff |
| Tests | `acl.rs` unit tests near `:2287`, `:2479` | 3-4 new cases |

The genuinely hard part is **none of the above** — it is the design call in (4)
about a heirless estate, and the §c call about whether removal is death.

---

## e) A second relation — kafāla (design only)

> Responsibility without inheritance and without the death-cascade: an agent
> operated by someone who should not inherit its correspondence, or sponsored by
> a role rather than a person.

**Can the current ACL express it? No — and the obvious way to add it is a trap.**

`AclAction` variants are **append-only positional postcard** (`acl.rs:145-146`,
restated at `acl.rs:61-62` for `Standing`). Adding a field to the existing
`AddAgent` struct variant changes its encoding: old builds hit
`decode_op_for_replay` (`acl.rs:917-926`), which warns and **excludes the op from
replay** — silently dropping authority, the exact failure that lost a Space's
implementation activations. So: **never touch `AddAgent`.** Append a new variant:

```rust
/// Confer kafāla — guaranteed standing without inheritance and without the
/// death-cascade. The guarantor is the op's `by` (a person) or a role.
AddClient {
    actor: ActorId,
    grants: Vec<Standing>,
    relation: RelationKind,
    guarantor: Guarantor,
},
```
Old builds see an undecodable op → an opaque DAG node with ancestry and no state
(`acl.rs:936-946`) → the kafāla client simply does not exist for them. That fails
*closed*, which is the correct direction, and it is the documented forward-compat
contract.

### What it costs

1. **A new arm in `judge_op`** (`acl.rs:1062`) — same fences as `AddAgent` plus
   validation of the guarantor.
2. **A generalized seat predicate.** The cascade (`acl.rs:1854-1866`) currently
   asks one question: *is the patron still a member?* Kafāla needs:
   ```rust
   Guarantor::Person(ActorId)                              // sponsorship: patron seated
   Guarantor::Role { capability, resource }                // kafala: ANY member holds it
   ```
   The role case is already computable inside `materialize_authorized` — it holds
   the full `PolicyPass` (`acl.rs:1549`, `:1599-1636`) and computes
   `policy_admins` from it (`acl.rs:1894-1905`). So "does any current member hold
   `(capability, resource)`?" is a local query over `policy.grants` minus
   `policy.revoked_grants`, filtered to `members`. No new input, no new
   non-determinism.
3. **No death-cascade, no estate.** A kafāla client is skipped by the orphan loop
   and never raises an `Estate`. Its material stays its own — which is the point.
4. **The seat must still terminate.** A kafāla client whose role has no holder is
   exactly the "survives orphaned" bug the cascade was written to prevent
   (`acl.rs:1856`, test at `acl.rs:2479-2530`). So kafāla does not remove the
   cascade — it *replaces the predicate*. The client is evicted when its guarantee
   lapses; it simply does not escheat when it does.

### The invariant risk this creates — read this before writing any code

`acl.rs:1082` is a single predicate:
```rust
!agents_now.contains_key(by) && match &op.action { … }
```
`agents_now` is **the only thing keeping any client out of the membership plane**,
and the same map is consulted again at `acl.rs:1087`, `:1079`, `:1147-1150`,
`:1158`. A new relation stored in a *new* map that is not folded into
`agents_now` (or into a single `is_client(by)` helper) **silently grants a kafāla
client full membership authority** — it would pass the ban, and `humans` would
have to be checked instead, which it is not for `AddMember`/`SetGrants`/
`RemoveMember`/`MintEpoch`/`RevokeInvite`.

**Mandatory shape:** every relation, of every kind, lands in *one* map, and
`judge_op` reads it through *one* helper. Introduce
`fn is_client(by: &ActorId) -> bool` before adding a second kind, not after.

**Verdict:** kafāla is cheap in wire format (one appended variant), moderate in
replay (one generalized predicate), and dangerous in exactly one line. Not in
scope for the first cut; the `RelationKind` enum should ship with only
`Sponsorship` so the shape is there when it is.

---

## f) Surface

### MCP tools (`src/mcp.rs`)

| Tool | Where | Change |
|---|---|---|
| `agent_add` | `mcp.rs:423-428`, listed `mcp.rs:91`, `:163` | Description already states the walāʾ terms verbatim ("content authority … never membership authority … dies with the sponsor"). Add an optional `relation` arg defaulting to `"sponsorship"` only when §e ships. |
| `whoami` | `mcp.rs:544` | Already returns `sponsor` (`dto.rs:82`, proven at `tests/it/agent_experience.rs:101-103`). Add `relation: { kind, patron, conferred_at }` beside it; keep `sponsor` as the compat alias. |
| `members` | `mcp.rs:435-438` | Rows gain the same `relation` object. |
| `member_log` | `mcp.rs:445` | Fix the `add_agent` row to carry its grants (`acl.rs:997-1001`, `:1501-1506`) — the conferral log must say what was conferred. |
| **new** `agent_estate` | — | List outstanding `Estate`s for the calling actor. Read-only. |
| **new** `agent_estate_claim` | — | Author `ClaimEstate`; refuses unless the caller is the named patron or an admin (heirless case). |

### HTTP (`docs/SERVE.md:49`, plane 2 — `POST /api/spaces/{id}/rpc`)

Existing: `Request::AgentAdd` (`src/control.rs:323`, dispatched `src/orbital/hosting.rs:1250`),
`Request::AgentProvision` (`control.rs:333`, `hosting.rs:1256`, handler `hosting.rs:1679-1717`).
`agent_provision` already mints the seed, self-incepts, sponsors, grants contributor
caps (`mechanics.rs:1778-1809`), and writes the local petname (`hosting.rs:1706`).

Add on the same plane, same shape:
- `{"cmd":"agent_estate"}` → `Vec<EstateDto>`
- `{"cmd":"agent_estate_claim","client":"act_…"}` → `Response::Ok`

Nothing belongs on `/api/host/rpc`: the relation is a Space fact, not a daemon
fact. Note `serve::borrowed_key_refusal` (`docs/AGENT-EXPERIENCE.md:36-60`,
`src/serve/mod.rs:823`) is **custody, not standing** and is untouched by any of
this — it asks whose key signs, never what relation the holder holds.

### DTOs (`src/dto.rs`, mirrored by hand in `viewer/src/types.ts`)

`MemberDto` (`dto.rs:19-45`) today:
```rust
pub role: String,               // dto.rs:27  — the grant axis
pub sponsor: Option<String>,    // dto.rs:40  — the relation axis, untyped
```
Add, keeping `sponsor` populated for wire compat (`viewer/src/types.ts:483`):
```rust
#[serde(default)]
pub relation: Option<RelationDto>,
// { kind: "sponsorship", patron: String, patron_did: Option<String>, conferred_at: String }
```
Same addition on `WhoamiDto` (`dto.rs:82`). New `EstateDto { client, client_did,
patron: Option<String>, conferred_at, open_issues: u32 }`.

Mirror every one in `viewer/src/types.ts` (`:476-483`, `:692`) — the file is a
hand-maintained mirror and the DTO comment at `dto.rs:12` says wire shapes must
stay byte-identical.

**Keep `role` and `relation` as two fields.** Never fold the relation into
`role_label` (`acl.rs:135`); never resurrect `AclState::standing()`
(`acl.rs:658`), which returns `"agent"` in the role slot. Grants and clientage
are orthogonal axes and the whole slice depends on that.

### Viewer — Settings → Members (`viewer/src/ui/Members.tsx`)

What exists: the roster is one row per member (`Members.tsx:116-149`); a client
draws a `Bot` chip reading `sponsored · <sponsorName>` with the tooltip
"Sponsored agent — standing dies with <name>" (`:136-144`); `sponsorName`
(`:272-275`) resolves the patron through the *local* petname store; the "Sponsor
an agent" action posts `agent_provision` (`:243`), open to any member because
sponsorship is not an admin power (`:218-221`); `add_agent` renders as "sponsored
agent" in the log (`:295`).

This is already close to right. Five changes so it reads as **clientage, not
descent**:

1. **Use a relational particle, not punctuation.** `sponsored · Omar` → **`client
   of Omar`** (or `sponsored by Omar`). The `·` is the particle today, and a
   middot is not a word. `mawlā Ibn ʿUmar` worked because the particle was a
   *lexeme*.
2. **Never let the patron's name enter the client's name slot.** The alias span
   (`:122-124`) and the relation chip (`:136-144`) are already separate elements —
   preserve that separation explicitly in a comment. `Omar's scout` is a
   possessive and reads as ownership/descent; `scout, client of Omar` does not.
3. **The chip links to the patron's row, and the link is one-directional.** The
   patron's row shows a **count** (`sponsors 3`), never a list of client names —
   a name list under a person reads as offspring. The relation is navigable
   upward (client → patron), summarized downward.
4. **One icon per kind.** `Bot` (`:141`) is the *agent-ness* icon, not the
   *relation* icon; the chip should carry a relation glyph so a second kind (§e)
   is distinguishable at a glance.
5. **`agentLogos` must never key off the relation.** `viewer/src/ui/agentLogos.ts:1-16`
   and `Avatar.tsx:41-69` draw a brand mark from the **local alias** only, and say
   so at length. That is exactly right: a brand mark is a name affordance. If it
   ever keys off `sponsor`/`relation`, the relation has become a name — the one
   thing this design forbids.

New surface: an **estate banner** when `estates` is non-empty, on the same page —
*"scout's work is unclaimed. scout was your client; its 12 open issues escheat to
you."* with a single Claim action posting `agent_estate_claim`. Model the banner
on how a `RekeyFence` is surfaced: an obligation named by replay, discharged by
one act.

Attribution surfaces (issue author, activity rows: `products/issues/src/dto.rs:546`,
`:698`, `:807`) render `authored by scout` today and will render a bare `act_…`
after eviction. With `former_relations` they can render `scout — formerly client
of Omar`, which is the honest string: the name stays the client's, the estate
does not.

---

## Open questions this slice must close before implementation

1. **Is `RemoveMember` on a client death or manumission?** §c's fence depends on
   the answer. Recommended: death — the bond is dissolved, the estate escheats,
   and re-conferral to a *different* patron is refused forever.
2. **Heirless estates.** Patron removal is the common cascade trigger, so most
   estates have no seated taker on day one. Recommended: `patron: Option<ActorId>`,
   `None` claimable by any admin.
3. **Does the estate include the client's scoped capability grants?** They are
   revocable policy assignments (`acl.rs:1599-1614`), not property; recommended
   answer is no — they lapse with membership (`effective_capability_grants`
   already filters on `members`, `acl.rs:715`) and nothing needs to change.
