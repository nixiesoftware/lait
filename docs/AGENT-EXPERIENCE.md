# Agent Experience — status and design

lait treats an agent as a **member whose identity is sponsored** — not a separate
actor class. There is no agent-specific vocabulary: grants, roles, removal,
attribution, and space selection are the *same* member machinery for humans and
agents. This document records what has shipped. Plans that govern further
work live as Specs in the tracker, not in `docs/plans/`.

## Sponsoring an agent

Two steps, and only the first is a decision.

**1 — sponsor the identity.** The usual path: the agent's first `whoami`
asks the person (Astrolabe notifies; Approve runs `agent_provision`).
Or, from the app: **Settings → Members -> Sponsor an
agent**, name it, done. That sends `{"cmd":"agent_provision","name":"<name>"}`
to `POST /api/spaces/{id}/rpc`. It mints the agent's seed under this store,
self-incepts it into the shared actor plane (the co-located analogue of a
joiner's inception arriving over Contact — no round trip, because the agent is
on this machine), sponsors it with the default content grant, and grants it the
contributor role's scoped capabilities. Idempotent: re-provisioning a known,
sponsored agent returns its actor.

**2 — point the agent's client at it.** Astrolabe's Library authors the
binding from the selected World (Preview / Write). Or `POST /api/host/rpc`
with `{"cmd":"host_install_mcp","client":"claude","name":"lait-issues","dir":"<project>","world":"issues"}`
merges a portable entry into that client's `mcpServers`, preserving any
others. The written shape is `{"command":"lait","args":["mcp"]}` — `lait`
off PATH, no captured home, no absolute binary. Naming the client also
names the agent identity — `claude`, `cursor`, `windsurf` — so the entry
carries `LAIT_AGENT=<name>` and the agent's work signs as itself.
`client: "generic"` derives no name, so pass `agent` explicitly;
`no_agent: true` declines one and leaves the work signed by the human.
`print: true` returns the would-be file contents instead of writing them.
`$LAIT_WORLD` pins the session to one World mount; omit it only while this
identity has a single selected World. An unknown mount is a refusal, not an empty
tool list. A project-scoped file lands beside a `.lait` store, never
inside one.

Ownership is split on purpose. The World designs the agent surface (tools,
omissions, teaching text). Astrolabe authors the binding and never parents
the process. `lait mcp` is the stdio adapter; traffic is editor →
`lait mcp` → daemon → WorldHost. `lait mcp` does not generate tools from
the wire protocol, and Astrolabe is not an MCP interchange.

The order does not matter. Installing the config first is fine; the agent's
first `whoami` files a host-plane sponsorship ask and returns `wait_heads`.
Astrolabe notifies the person; Approve is `AgentProvision`, which moves those
heads. The agent **Watches** (`wait` with the last heads) — Exec Watch's
comparison, not a `whoami` poll. `Unchanged` means still waiting; `granted`
is the wake, consumed once. A reconnect that missed the wake still sees it
on the next `wait` or `whoami`.

Do not treat onboarding as invite→connect: that is the peer-JOIN flow for a
new node, not for a sponsored agent attaching to a Space that already
exists. `whoami` is this agent's membership. `doctor` is this *device's*
onboarding gates — a pending membership there can be the machine, not the
agent. The Library is the catalog of Worlds, with installation reported as a
separate fact; Spaces belong to the head and are not titled here.

### The custody rule

A sponsored agent is **not** read-only, and nothing about it is second-class. It
holds `Standing::Write` (`crates/mechanics/src/acl.rs`), so through its own node
— `lait mcp`, which *is* that node — it files, comments, starts, closes, and
deletes issues exactly as any contributor does.

What the HTTP head refuses is narrower and is not about the agent at all. A
Station whose catalog binding carries its own identity directory signs with
*that* seed, so a write routed into an agent-held Orbit by a head serving
somebody else's token would go out **over the agent's signature**. Mechanics
would approve it — it evaluates the signer's grants, the signer would be the
agent, and the agent holds write standing — and nothing behind that route asks
again. So the head asks once, at the door, and refuses:

> `{agent}'s space is read-only here — a write would be signed as {agent}.
> Open the same space through your own node to write as yourself.`

This is **custody, not standing**. It does not become redundant as anybody's
grants widen; the answer must stay no however wide they become, because the
question is "whose key is about to be spent", not "is this act permitted". It
never refuses a *read*: observing a hosted identity's board signs nothing, and
that is the whole reason an agent's space is browsable from the app. The single
predicate is `Catalog::signs_with_own_seed`; the single refusal is
`serve::borrowed_key_refusal`. Never silently sign as somebody else.

## Shipped

### The linchpin — a sponsored member holds content authority

A sponsored member (an agent) is no longer grant-less/view-only. It holds the
**existing `Grant::Write`** through the same grant machinery any member uses —
minted at sponsor time (`AclAction::AddAgent { actor, grants }`, default
`Write`). The invariants are preserved by construction and by test
(`crates/mechanics/src/acl.rs`):

- **Dies with the sponsor.** The actor stays in the `agents` map; the sponsor
  cascade (remove-wins + nonce-race) evicts it when its sponsor leaves.
- **No membership authority.** `AddAgent` refuses `Grant::Admin` at replay
  (`is_sponsorable_grant_set`), and the blanket agent-author ban in `judge_op`
  stands — a sponsored member can file/close/comment but cannot add/remove
  members or rotate the key.
- **The E2EE recency fence is untouched.** A grant-less agent *already* held
  every sealed epoch key (read access via `seal_records_for_actor`); the linchpin
  adds *write* standing, not read access, so removal/rotation semantics are
  unchanged.

Because standing is grant-only (`can_write` is agent-blind), the content-authoring
gate (`signer_can_write` → `can_write`) authorizes a sponsored writer with no
special case.

### Identity surface — one surface

- **`did:key` for any member.** `crypto::did_key_from_pubkey` renders any device
  key as a spec-compliant `did:key:z6Mk…` (ed25519 multicodec + base58btc
  multibase) — a pure, offline, self-certifying, *synced-safe* handle. Exposed on
  every `MemberDto` and in `whoami`.
- **The roster renders sponsorship, does not gate on it.** `members()` is one row
  per member; a sponsored member reads as `member`/`viewer` (its grants) with a
  `sponsor` link — the viewer draws a "sponsored · <sponsor>" badge (`Bot` icon).
- **MCP onboarding says "attach, don't rebuild."** `get_info` tells an agent it
  has an identity and to call `whoami`/`sync`, not to treat onboarding as
  invite→connect (the peer-join flow for a *new node*).
- **Structured, actionable errors.** A denied write returns a typed
  `ErrorKind::Denied` with the next step ("ask your sponsor / an admin to grant
  write access"), as an MCP tool-execution error (`isError: true`) so the
  model sees the message. JSON-RPC errors stay reserved for transport and
  unknown methods.

### Observability — no more inference

- **`whoami`** — actor, `did:key`, device, space, role, capabilities, sponsor,
  name, and the loud partial-view signal, in one shot.
- **`sync`** — converges the keyring and reports completeness loudly, naming any
  missing epoch key instead of silently showing fewer issues (the 141-vs-154
  bug).
- **Hard partial-view guard.** A *delegated* identity (a sponsored agent) is
  refused authoring against a known-partial view — it could "close what's done"
  on issues it cannot see. A human acting for themselves gets the loud signal and
  judges; an agent is stopped by construction (`route_issue`).

### Operator friction removed

- **Build isolation.** Agent/test/worktree builds use a separate
  `CARGO_TARGET_DIR`, so they never lock a running node's `lait.exe`.
- **Clean-env test entrypoint.** A `#[ctor]` scrubs ambient `LAIT_HOME`/
  `LAIT_STORE`/`LAIT_CONFIG_ROOT` at unit-test *and* subprocess-spawning
  integration-test load, so a developer's shell `$LAIT_HOME` (pointing at their
  live node) can never poison a run — it previously collided a spawned test
  daemon with the live node's lock.
- **Named identities** are the selection mechanism; a named identity's secret
  lives under `config_root()` (the platform config dir), *outside* a
  working-directory sandbox — the answer to deterministic-seed reconstruction for
  the common "reset my working dir" case.

## Runtime — the multi-tenant daemon (Architecture B), shipped

**One store, one lock, one always-on daemon; the human and every sponsored agent
are signing clients of it.** This is the seamless bar, and it is live:

- **Multi-identity daemon.** The daemon holds the human's identity and docks a
  Session **per local agent identity**, all sharing the one Replica. Each Session
  signs and attributes as *that* identity. `Session::submit` requires
  `action.header.actor == docked principal`, so per-agent attribution comes from
  docking a Session as the agent — not from re-signing.
- **The `act_as` selector.** The control envelope (`control::ClientRequest`) is a
  `Request` plus an optional `act_as`, flattened and skip-when-`None`, so a
  request with no selector is byte-identical to the bare request — the wire stays
  backward-compatible. An MCP head picks the identity with `LAIT_AGENT=<name>`,
  which `host_install_mcp` writes into the client config for you; the daemon
  signs as that local agent.
- **One-step provisioning.** `agent_provision` (Astrolabe Approve, or
  Settings → Members -> Sponsor an
  agent) mints the agent's seed under the home, **self-incepts it into the shared
  store's actor plane** (no Contact round — the co-located analogue of a joiner's
  inception arriving over the wire), sponsors it with content authority, **and**
  grants it the contributor role's scoped capabilities (`space.contributor` +
  `space.issue.read`) so it can actually read the catalog and write. The ACL
  write grant is content authority; the scoped capabilities are the separate
  policy plane a functional contributor also needs — a sponsored member gets
  both.
- **Storage is O(1) by construction.** N agents on one machine share the one
  store — one `objects/` pool, one journal. There is no N-copy bloat to dedup,
  because there are not N stores. The separate-store shared-pool + cross-frontier
  GC path (Architecture A) is therefore unnecessary for the co-located case; B
  supersedes it. (For genuinely separate nodes — a laptop and a phone — dedup is
  a future storage optimization, not an attribution or lifecycle requirement.)

Proven end to end (`tests/it/agent_experience.rs`, and a recorded live run): the
human sponsors `scout` once; `whoami` as scout shows a distinct actor + `did:key`
+ write standing + read/contributor caps + the sponsor link; scout files,
comments on, and starts an issue; the activity log attributes scout's work to
scout's own signing device, distinct from the human's — all in one store, one
daemon, no restart.

### Deterministic seed across a *fully* reset sandbox

A named identity's seed is persisted under `config_root()`, outside a
working-directory sandbox. For a sandbox that wipes the config dir too,
deterministic reconstruction needs the seed derived from a stable name **plus a
machine/user secret that lives outside the sandbox** (an OS keyring or an
operator-provided env secret) — an operator policy, specified here, not a
built-in keyring integration.
