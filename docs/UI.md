# Product surfaces

lait has three product surfaces: CLI, local web, and MCP. They are clients of
the same daemon and use the same command and projection contract. No surface
opens Replica or Engine independently; product work reaches a World through a
docked Session.

## 1. Product model

A space is a local replica of a shared issue tracker. Run `lait init` to found one
or `lait join` to create a replica from an invite. Other commands require an
existing space and never create one as a side effect.

Within a space:

- issues have stable `iss_` identifiers and friendly project aliases;
- projects, labels, workflow states, and board order are shared;
- assignments and authors refer to stable actors rather than devices;
- petnames are local and never replace an actor id in authority decisions;
- reads use Manifest-pinned local projections; Contact and convergence happen
  through the active Station.

## 2. CLI

The CLI favors flat verbs for daily issue work and nouns for registries and
administration. Run `lait --help` or `lait <command> --help` for the exact current
grammar; generated help is the command reference.

Common flows:

```text
lait init
lait issues new "Fix the import path"
lait issues ls
lait issues show <ref>
lait issues edit <ref>
lait issues start <ref>
lait issues done <ref>
lait issues comment <ref> "Reproduced on Windows"
lait issues board
```

`<ref>` resolution happens in the daemon. Full ids, unique prefixes, friendly
aliases, and supported contextual forms resolve through one grammar. Ambiguity
returns candidates; clients do not guess.

`--json` returns versioned response DTOs suitable for scripts. Error behavior is
classified by type, not by matching message text. Human output may improve
without changing the JSON contract.

Destructive or security-sensitive operations can require explicit confirmation.
Non-interactive clients must use the documented confirmation mechanism rather
than relying on a prompt.

## 3. Web

`lait serve` starts a loopback-only web application that can list locally known
spaces through the local daemon control layer. It is a local client, not an iroh
peer and not a space member.

The server uses a per-run bearer capability and origin/rebinding checks. A
browser may list navigation metadata without activating every Space. Attaching
to a row starts or reuses only the Station placed in that local Orbit, under the
correct local identity. Rows use local Orbit ids because two local stores may
participate in the same Space. The web process owns no Station: it sends routed
requests and receives catalog doorbells through the same identity-scoped
daemon::Daemon endpoint as CLI and MCP.

The web application provides issue lists, boards, detail, inbox, activity,
members, filters, and command actions. Server-side semantics such as reference
resolution, authorization, project selection, and filtering remain in the
daemon; the browser does not reimplement them.

Actor/device management is not yet fully represented in the web members view.
Use the CLI for device enrollment, revocation, and recovery until parity lands.

## 4. MCP

`lait mcp` exposes the daemon command surface as MCP tools for agents. MCP uses
the same request and response types as other clients and remains pinned to the
Orbit selected at launch even though the daemon knows the wider local catalog.
A parity test guards the intentional tool mapping.

Agents do not receive a privileged storage API. They resolve references, submit
commands, encounter the same authorization checks, and receive the same
versioned projections as human clients.

## 5. Live updates

Clients subscribe to dirty notifications. A notification identifies projections
that may have changed; it does not contain authoritative state. Clients re-read
the relevant board, issue, inbox, activity, members, or status projection.

Subscriptions begin with a reset. Clients also rebaseline when the daemon epoch
changes or when their sequence cursor falls behind the retained ring. Dirty
notifications may be coalesced without losing correctness.

Optimistic UI is permitted only as a temporary overlay. The next authoritative
projection always wins. A scalar may have a declared deterministic winner;
causally meaningful concurrent transitions or revisions surface a typed conflict
rather than being silently described as "the CRDT merge."

## 6. Identity, membership, and access

A member is an actor with one or more device keys. Admission carries an exact
expanded assignment set. The shipped roles are viewer, contributor, and
administrator, while effective authority consists of scoped World capabilities
stored and evaluated by Mechanics. Role names are provenance and UX; they are
not flat runtime grants.

Different projects in one Space may use different behavioral RBAC. They remain
inside one membership and encryption boundary; projects requiring distinct
read confidentiality belong in distinct Spaces.

The member surface shows actors. Device commands manage the keys behind the
current actor:

- `device invite` creates an enrollment token;
- `device accept` runs on the new device without a daemon and produces consent;
- `device add` binds that consent and seals held content keys;
- `device revoke` removes a device and rotates when possible;
- `device ls` lists the current actor's devices;
- `recover` resets the actor to the current device using the offline actor
  recovery key.

Space recovery and custody are separate from actor device recovery. Their
commands operate on the space recovery authority and require explicit
expected targets before a device contributes sensitive material.

## 7. Joining

Signed Coordinates carry the Space bootstrap anchor, approach Station and
bounded direct-route hints, plus an optional admission capability. Joining
creates and records a recoverable Orbit before activation and first Contact.

An authorized reusable or single-use invite may admit the joining actor
automatically: accepting the invite is the approval, and redemption completes
on the joiner's first contact with a member — there is no approval queue.
Unadmitted nodes may perform the bounded bootstrap Contact needed for redemption
but cannot dock an Issues Session or read protected Bodies.

`lait doctor` reports onboarding gates in order: Space, Station, admission,
peer reachability, convergence, and key/custody health where applicable. It distinguishes
waiting from failure instead of presenting an empty board as success.

## 8. Presence and names

Presence is online, away, or offline and is advisory. It reflects device
reachability and recent local interaction, not actor authority.

That is one presence and there are now two, and the word covers both. The
durable one above is a **device**: reachable or not, recorded in the Neighbor
registry, and outliving anybody's attention. The second is a **viewer
indicator** — who has this issue open, where their caret is, who is typing —
keyed by **actor**, held only for as long as the session that published it, and
never journaled. Neither implies the other. A reachable device is not looking at
anything, and a person reading an issue through a station this node cannot
otherwise reach still belongs on the facepile. Both remain advisory, and neither
is an input to an authorization decision.

The viewer indicator carries an uncertainty the durable one does not, and a
client has to draw it rather than resolve it. Past the caret grace window the
daemon still reports the entry and marks it `uncertain`: it is shown, marked,
not dropped — a collaborator who has gone quiet for a minute has not left the
room, and a facepile that quietly omits them reports an emptier room than there
is. A caret position is one of three answers that do not collapse into each
other: a position, `drifted` — the material the offset was attached to is gone —
and `unresolved`, which is the absence of an answer rather than one. A client
that draws `drifted` as its last known offset points confidently at the wrong
character, and one that draws `drifted` and `unresolved` the same way reports a
live caret as lost.

Network nicknames are self-asserted display data. Local petnames are preferred
for familiar rendering, but security-sensitive selection and confirmation show
stable identifiers. A name alone never selects a recovery target or grants
membership.

`who` and `live` report different things and are not interchangeable. `who`
reports the durable Neighbor registry: which peers exist and whether they are
reachable, keyed by **device**. `live` reports the transient table the Live
plane holds — who is looking at an issue, where a caret is, who is typing —
keyed by **actor**, and nothing in it is journaled, replayed, or survives the
session that published it.

Rows from `live` carry an actor id, resolved by the daemon through the Station's
authority view. A row whose Station resolves to no actor is omitted rather than
carried under its device id: a device id and an actor id are different strings
for the same person, and a client colouring an avatar by hashing whichever it
was handed would draw one human twice, in two colours, on the surface whose
whole job is telling people apart.

Every `live` reply carries a generation and a `partial` flag. A caller sends the
generation it holds and is answered `live_unchanged` while it stands, so a poll
that finds nothing new costs one comparison. `partial` means the node is not
hearing from everyone it could be — over its session cap, or dropping scopes at
a gate — and a client must show it. Awareness is allowed to be incomplete;
drawing three of five people with no indication is a confident lie.

The generation is not the whole story. A row's `uncertain` is derived per read
from its age, and nothing moves the counter when one crosses the grace window,
so the cheap answer is given only once every row is already uncertain and
nothing more can flip. Otherwise a caller would go on drawing a caret as current
until the slot expired half a minute later — the same confident lie, told about
one person instead of the room.

An unscoped `live` returns Body ids from every hosted World. The derivation from
an issue's doc id to its Body id runs one way, so those ids name nothing a
browser can display — which is why `live` takes an `issue` and narrows
daemon-side.

That narrowing is by **Body**, not by scope. Somebody reading an issue, a caret
in its description and a typing flag in its title are three different scopes over
one Body, and a caller that named the issue asked about all three. The `issue` is
a doc id and never a project alias: the Body id is a hash of the string as given,
so `ENG-12` names a Body nothing publishes under and is answered an empty table
rather than an error.

`signals` drains. A signal is an event and not a state anyone can re-read, so
every one is answered exactly once and two callers on one space split the set
between them. The daemon's queue is bounded, and a full one drops its **oldest**
and says how many in the reply: a caret superseded by the next caret has lost
nothing, while an invitation dropped to make room for a ping is gone. What has
not been seen yet is what somebody is about to act on.

A browser does not read either of them. `lait serve` reads them on the browser's
behalf and pushes the answers down the `/api/session` WebSocket, which carries
three lanes:

- **transient** — the `live` reply for one declared question. Lossy on purpose:
  the next view supersedes the one that was lost, so a slow tab falls behind in
  staleness rather than in backlog.
- **control** — the drained signals. Lossless on purpose: nothing supersedes a
  signal, so a tab that stops reading is disconnected rather than served a
  stream with a hole in it.
- **progress** — a local transfer's byte count, which predates both.

A socket declares one thing — a space, and optionally an issue — on the
transient lane, and the server acts on every declaration once per tick for the
whole server. Two tabs on one issue cost one read; a question nobody holds is
never asked, which is what keeps an open browser from placing a Station for
every Orbit on the machine. Because the drain empties the queue, the server is
the only thing that may drain a space a browser is watching, and it skips an
agent's space entirely: that space is observable through the browser and not
operable, and taking an agent's signals out from under it because somebody left
a tab open is the same write the RPC surface refuses at the door.

The two lanes run off different halves of that declaration. `live` is polled per
*question*, so a declaration naming no issue costs no read — it says which space
the tab is in and nothing more. The drain runs per *space*, for a tab on a board
as much as for one on an issue: the daemon's queue is bounded and overwrites its
oldest, so gating a lane that may not drop on a lane that may would destroy
invitations for want of a facepile nobody asked for. A client declares its space
for as long as it is in it, and stops when it leaves.

A drain delivered to a client is a drain the daemon no longer holds. A client
that decodes one and does not keep it has destroyed it — for its own later reads,
for `lait signals` at a terminal, and for every other reader there is. Holding it
is not optional, and neither is bounding what is held: a client that has lost the
oldest of them says how many, exactly as the daemon does.

## 9. Comments, workflow conflicts, and partial state

Comments are addressed by stable identities. When replies, reactions, editing,
or moderation are available, clients treat comments as first-class records:
replies retain their parent id, reactions retain actor membership, and
concurrent edits retain revision heads. A list position is never a comment id.

The canonical workflow model uses predecessor-bound transitions. When the
product exposes concurrent transition heads, clients must present a conflict
requiring authorized resolution rather than inventing a winner from timestamps
or arrival order. The current scalar-status projection does not yet expose that
conflict and is a known product-schema limitation.

Clients must also distinguish:

- a valid value;
- a legitimately unavailable value, such as a provisional catalog row;
- a corrupt stored record.

Healthy JSON shapes remain stable. Where a projection supports corruption
sidecars, malformed records appear there with their locus and reason rather than
vanishing or appearing as typed values with sentinel fields.

## 10. Compatibility

The client performs a daemon handshake before normal commands. A missing,
incompatible, or unintelligible daemon is reported distinctly from an absent
daemon. Clients do not spawn over a process they cannot safely identify.

The exact control-channel compatibility rules are in
[`PROTOCOL.md`](./PROTOCOL.md). Exact command spelling comes from generated CLI
help, avoiding a second handwritten command table that can drift.
