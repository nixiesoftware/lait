# Product surfaces

lait has two product surfaces: the local web app and MCP. They are clients of
the same daemon and use the same request and projection contract. No surface
opens Replica or Engine independently; product work reaches a World through a
docked Session.

There is no third product surface. Astrolabe is the library client above
them — it lists Worlds, opens a browser, and authors an MCP binding. It
never draws a World. `lait` is a launcher — `lait daemon`, `lait mcp`, and
bare `lait` (the app, and the daemon under it) — not a command surface, and
nothing about the product is reachable by typing a verb at a shell.

## 1. Product model

A space is a local replica of a shared issue tracker. The app's Welcome screen
founds one (`host_space_found`) or creates a replica from an invite
(`host_space_enter`), both on the host plane. Nothing else creates a space as a
side effect; every other request requires one to exist.

Within a space:

- issues have stable `iss_` identifiers and friendly project aliases;
- projects, labels, workflow states, and board order are shared;
- assignments and authors refer to stable actors rather than devices;
- petnames are local and never replace an actor id in authority decisions;
- reads use Manifest-pinned local projections; Contact and convergence happen
  through the active Station.

## 2. The request contract

Every surface sends the same JSON requests and reads the same versioned
response DTOs, so this section describes both of them at once. The daily issue
vocabulary is `issue_new`, `list`, `issue_view`, `issue_edit`, `issue_start`,
`issue_done`, `issue_stop`, `comment`, `board` — over HTTP on the World plane
(`POST /api/spaces/{id}/worlds/issues/rpc`), or as the `issues_*` tools over
MCP. The mount name (`issues`) is what makes those two spellings one namespace.

`reff` resolution happens in the daemon. Full ids, unique prefixes, and friendly
aliases resolve through one grammar. Ambiguity returns candidates; clients do
not guess.

Error behaviour is classified by type, not by matching message text. The JSON
contract is what surfaces agree on; wording may improve without breaking it.

Destructive or security-sensitive operations require explicit confirmation. The
head answers them `409 confirm_required` carrying the question, and the caller
re-sends with `?confirm=1`; the browser draws that as a modal. The question
string comes from one place, so no two surfaces can disagree about what is
dangerous. This guards against an accident, not an attacker — anything that can
send the request can also send the confirmation.

## 3. Web

Bare `lait` starts a loopback-only web application that lists locally known
spaces through the local daemon control layer. It is a local client, not an iroh
peer and not a space member. It is also the *default* mode: running the binary
with no arguments is how a person opens the product.

The server uses a per-run bearer capability and origin/rebinding checks. A
browser may list navigation metadata without activating every Space. Attaching
to a row starts or reuses only the Station placed in that local Orbit, under the
correct local identity. Rows use local Orbit ids because two local stores may
participate in the same Space. The web process owns no Station: it sends routed
requests and receives catalog doorbells through the same identity-scoped
`daemon::Daemon` endpoint the MCP head uses.

The web application provides issue lists, boards, detail, inbox, activity,
members, devices and recovery, labels, workflow, access, filters, and actions.
Server-side semantics such as reference resolution, authorization, project
selection, and filtering remain in the daemon; the browser does not reimplement
them.

Requests are split across three planes by scope, and the split is structural
rather than stylistic: bootstrap has no space id to name, so it cannot ride a
space-scoped route. `POST /api/host/rpc` carries formation, device consent,
local config, the Orbit registry, MCP install, update/restart, and orientation;
`POST /api/spaces/{id}/rpc` carries generic Space authority;
`POST /api/spaces/{id}/worlds/{world}/rpc` carries the product. Full route
table, credential posture, and the custody fence: [`SERVE.md`](./SERVE.md).

A head that would have to sign with a key it merely hosts refuses the write
rather than performing it. That refusal is about custody, not about anyone's
standing, and it never applies to a read.

## 4. MCP

`lait mcp` exposes the daemon request surface as MCP tools for agents. MCP uses
the same request and response types as the web head and remains pinned to the
Orbit selected at launch and to one World (`$LAIT_WORLD`, or the sole World
this build hosts). The World designs that tool list; the adapter does not
generate it from the wire protocol. Astrolabe writes the portable binding
(`lait` off PATH, `LAIT_AGENT`, `LAIT_WORLD`) from the selected Library row
and never parents that process. A parity test guards the shell half; World
coverage lives on the package.

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

The member surface shows actors. Settings → **Devices & recovery** manages the
keys behind the current actor, and the enrolment round-trip spans two machines:

- `device_invite` (Space plane) creates an enrolment token;
- `host_device_consent` (**host** plane) runs on the *new* machine and signs its
  consent — it is on the host plane precisely because that machine has no
  membership anywhere yet, so there is no space id it could name;
- `device_add` (Space plane) binds that consent and seals held content keys;
- `device_revoke` removes a device and rotates when possible;
- `device_list` lists the current actor's devices.

Space recovery and custody are separate from actor device recovery.
`space_custody_export` and `space_custody_import` operate on the space recovery
authority and require explicit expected targets before a device contributes
sensitive material. The Devices & recovery panel reads its warnings — share
missing, unreadable, backup unverified — straight off the reported recovery
status rather than inferring them.

## 7. Joining

Signed Coordinates carry the Space bootstrap anchor, approach Station and
bounded direct-route hints, plus an optional admission capability. Joining
creates and records a recoverable Orbit before activation and first Contact.

An authorized reusable or single-use invite may admit the joining actor
automatically: accepting the invite is the approval, and redemption completes
on the joiner's first contact with a member — there is no approval queue.
Unadmitted nodes may perform the bounded bootstrap Contact needed for redemption
but cannot dock an Issues Session or read protected Bodies.

`diagnose` reports onboarding gates in order: Space, Station, admission, peer
reachability, convergence, and key/custody health where applicable. It
distinguishes waiting from failure instead of presenting an empty board as
success. The app runs it while a join is settling, which is why entering from an
invite says what it is waiting for rather than showing an empty board.

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

In the web document editor, a resolved caret is drawn at its anchored position
in the body itself, with the actor's name and stable colour. A non-collapsed
selection tints its exact range. The activity rail remains a summary rather
than the only place cursor state is visible. The browser publishes cursor
motion and typing on the transient lane, coalesced to 80 ms; clearing focus is
immediate so a departed caret is retired promptly. Text edits use a separate
350 ms quiet window and the durable issue-edit path, with blur flushing the
last batch.

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

A browser does not read either of them. The head reads them on the browser's
behalf and pushes the answers down the `/api/session` WebSocket, which carries
three lanes:

- **transient** — the `live` reply for one declared question. Lossy on purpose:
  the next view supersedes the one that was lost, so a slow tab falls behind in
  staleness rather than in backlog.
- **control** — the drained signals. Lossless on purpose: nothing supersedes a
  signal, so a tab that stops reading is disconnected rather than served a
  stream with a hole in it.
- **progress** — a local transfer's byte count, which predates both.

A socket declares a space, optionally an issue, and optionally the active
document cursor/selection and typing flag on the transient lane. The server
coalesces all browser declarations for a Space into the Station's replace-all
Live publication and acts on changes on an 80 ms beat; housekeeping and signal
draining remain on a one-second beat. Two tabs on one issue cost one read; a
question nobody holds is never asked, which is what keeps an open browser from
placing a Station for every Orbit on the machine. Because the drain empties the
queue, the server is the only thing that may drain a space a browser is
watching, and it skips an agent's space entirely: that space is observable
through the browser and not operable, and taking an agent's signals out from
under it because somebody left a tab open is the same write the RPC surface
refuses at the door.

The two lanes run off different halves of that declaration. `live` is polled per
*question*, so a declaration naming no issue costs no read — it says which space
the tab is in and nothing more. The drain runs per *space*, for a tab on a board
as much as for one on an issue: the daemon's queue is bounded and overwrites its
oldest, so gating a lane that may not drop on a lane that may would destroy
invitations for want of a facepile nobody asked for. A client declares its space
for as long as it is in it, and stops when it leaves.

A drain delivered to a client is a drain the daemon no longer holds. A client
that decodes one and does not keep it has destroyed it — for its own later reads,
for every other tab, and for every other reader there is. Holding it is not
optional, and neither is bounding what is held: a client that has lost the
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
[`PROTOCOL.md`](./PROTOCOL.md). Exact request spelling comes from the typed
`control::Request` enum and each package's MCP descriptors — the two things the
wire is generated from — rather than from a second handwritten table that can
drift.
