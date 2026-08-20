# lait

Track work at typing speed, onboard a teammate with one link, and let your
coding agents work the board like anyone else — no server, no signup, no
account. One binary, and the app is a browser tab it serves to you off
`127.0.0.1`.

```console
$ lait
lait — your spaces at:
  http://127.0.0.1:7717/?token=…
(loopback only; this link carries a one-time token for this run)
```

That is the whole invocation. `lait` is **not a command surface** — it is a
launcher with three modes (the app, the daemon, the agent head), and everything
a verb used to do is now a request the app makes for you, or one an agent makes
over MCP.

- **Instant** // your issues live on this machine and open in milliseconds —
  faster than a hosted tab, on a plane, in a basement
- **One-link teams** // send an invite over any chat; the joiner pastes it into
  their own Welcome screen — no accounts, no admin console, no seat licenses
- **Agent-native** // AI agents are first-class members: they claim, comment,
  and close issues through MCP with the same audit trail as a human
- **Nothing to learn** // no grammar, no flags, no `--help` to read; the app is
  the interface and the HTTP planes underneath it are the API
- **Private by default** // everything is end-to-end encrypted between members;
  there is no server to trust because there is no server
- **Works everywhere** // one self-contained binary for macOS, Linux, and
  Windows; offline-first, syncs whenever teammates are online together

Whether you're solo in a side project, working with a team, or wiring up agents
that need a shared board, the whole product is the one binary below.

> Curious how it works with no server? The short version: issues are CRDTs
> synced peer-to-peer, membership is a signed key graph, and every node keeps a
> durable local copy. The long version — architecture, data shapes, protocol,
> decision log — lives in [`docs/`](docs/README.md), starting with
> [`ARCHITECTURE.md`](docs/ARCHITECTURE.md); the HTTP surface is
> [`SERVE.md`](docs/SERVE.md).

## Install

`lait` is a single self-contained binary, built for **macOS, Linux, and Windows**
(arm64 + x86_64) and published as a GitHub Release on every tag. Pick a channel —
they all land the same `lait`. Full matrix + verification in
[`docs/INSTALL.md`](docs/INSTALL.md).

```bash
# macOS / Linux — shell installer (places lait in ~/.cargo/bin)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/nixiesoftware/lait/releases/latest/download/lait-installer.sh | sh

# Homebrew (macOS / Linux)
brew install nixiesoftware/tap/lait

# from source (Rust 1.91+)
cargo install --locked --git https://github.com/nixiesoftware/lait lait
```

```powershell
# Windows — PowerShell installer
powershell -ExecutionPolicy Bypass -c "irm https://github.com/nixiesoftware/lait/releases/latest/download/lait-installer.ps1 | iex"
# …or:  scoop install lait   ·   winget install NixieTechLLC.Lait
```

Upgrading is node maintenance rather than a command you type: the running daemon
knows which build it is, so `{"cmd":"host_update"}` on the host plane pulls the
latest release and swaps the binary in place, and `{"cmd":"host_restart"}` makes
the swap take effect. Re-running the installer above works just as well. For an
always-on **seed node**, see the [Docker setup](docker-compose.yml).

### Nightly / dev builds

Every merge to `main` publishes prebuilt binaries to a rolling **[`dev`
prerelease](https://github.com/nixiesoftware/lait/releases/tag/dev)** (Linux x64,
macOS arm64/x64, Windows x64) — bleeding edge, for dogfooding the latest `main`.
It's a GitHub *prerelease*, so it never shows as "Latest" and never touches the
package managers or crates.io.

```bash
# grab the current dev build for your platform
gh release download dev -R nixiesoftware/lait
```

A dev binary reports its commit so it's unmistakable from a tagged release:
`lait --version` → `lait <version>-dev+<sha> (<date>)`. That is the one question
that has to be answerable with nothing running, which is why it stayed a flag.

### Build from source

```bash
cargo build --release          # Rust 1.91+ (floor driven by iroh 1.0.0-rc.1)
```

Contributing? Enable the hooks once per clone
(`git config core.hooksPath .githooks`) so fmt issues never reach CI.

## The three modes

```console
lait                                            # the local app, and the daemon under it
  [--json] [--port N] [--orbit SEL] [--open] [--home <dir>]

lait daemon [--home <dir>]                      # the identity-scoped host, headless
lait mcp                                        # the stdio head an agent speaks
lait --version                                  # which build this is
```

Anything else exits `1` and says so. `--json` prints one machine-readable
readiness line — `{"url":…,"token":…,"port":…}` — *before* the listener accepts,
so a parent process can read one line and know the port is live. Exit codes:
`0` ok · `1` usage/error · `2` selector matched nothing · `3` daemon unreachable.

Astrolabe (`apps/astrolabe`) is the library client for the Worlds this device
serves. It lists, launches, and authors an agent's MCP binding. It never
draws a World — `Open` hands that to the browser. See
[`apps/astrolabe/README.md`](apps/astrolabe/README.md).

## Use it like this

### 1 · Solo: found a space and start filing

Run `lait`, open the link. With no space on this machine yet you land on
**Welcome**, which does exactly two things: *Found a space* (pick a directory
and a name — it mints the genesis and seeds a first project) or *Use an invite*.
Nothing is created implicitly, so the first act is always deliberate.

From there the app is the product: a focus view, issue lists, boards, calendar
and timeline, per-project overviews, a durable inbox, and an activity feed. The
inbox survives restarts (unlike a feed you scrolled past) and is
attribution-honest: comments carry their real author and state changes never
guess. Nothing needs the network; teammates come later (scenario 2) or never.

### 2 · Two of you: onboarding is one link

Invites are bearer links carrying everything a joiner needs — the space, the
trust root, and a single-use auto-admit pass.

- **You:** Settings → Members → *Invite*. Copy the `lait://join/…` link and send
  it over any private channel. The role it admits as (viewer, contributor,
  administrator), its TTL, and whether it is reusable are chosen there.
- **Them:** install `lait`, run it, and on Welcome pick *Use an invite*. Paste
  the link, pick a directory. Their store is created, bound to your Space, and
  the pass admits them automatically on first contact with a member — there is
  no queue and no approve step.

Everything is end-to-end encrypted; membership is a signed key graph, so
removing a member from Settings → Members rotates the key and revokes future
reads.

### 3 · The daily loop

Filing, claiming, commenting, and closing all happen where you are looking at
the board, and your teammate's activity finds you — the app holds a live
connection to the daemon (`GET /api/events` plus a `/api/session` socket), so a
change on their machine lands on yours without a refresh, complete with who has
which issue open and where their caret is.

> **Gone with the CLI:** lait no longer reads the issue off your git branch, and
> no longer cuts a branch for you when you start one. Those were properties of a
> shell that ran inside your checkout; the app does not run inside anything.

### 4 · Your coding agent is a teammate

Membership is a keypair and an issue is a perfect unit of agent work, so an MCP
agent files, claims, comments, and closes issues exactly like a human — same
operations, same audit trail. See [Use from an AI agent](#use-from-an-ai-agent-mcp)
below and [`docs/AGENT-EXPERIENCE.md`](docs/AGENT-EXPERIENCE.md).

### 5 · Many spaces, one machine

Local Orbits — each one durable participation in a Space — are registered in a
catalog, and the app is a picker over all of them: the space switcher lists
every Orbit this identity holds, and selecting one is what starts its Station.
`--orbit <selector>` pins a head to one of them instead, by name, id, or path.

Registry upkeep lives on the host plane rather than in a verb:
`host_orbit_forget` deregisters a row without touching its store,
`host_orbit_prune` drops rows whose store is gone, and `host_orbit_rebuild`
rebuilds one Orbit's local representation as an explicit generation.

### 6 · A team that's rarely online together

Sync is peer-to-peer; a team spread across timezones pins one always-on peer
(any box running the same binary) that backfills whoever comes online. Run
`lait daemon` on that box — headless, no HTTP head, never idle-shuts-down — and
point a head at the same `LAIT_HOME` once to enter the space from an invite and
to admit the node.

The seed holds ciphertext and the signed op-graph — it can neither read (E2EE)
nor forge (genesis-anchored signatures). See
[docker-compose.yml](docker-compose.yml).

## The API is the HTTP head

There is no `--json` on a verb any more, because there are no verbs. The head
`lait` serves is the scriptable surface, and it speaks the same versioned DTOs
the MCP tools return. Three planes, all `POST`, all JSON:

| Plane | Scope |
|---|---|
| `POST /api/host/rpc` | the daemon: founding, entering from an invite, device consent, local config, the Orbit registry, MCP install, update/restart, orientation |
| `POST /api/spaces/{id}/rpc` | one local Orbit: membership, invites, devices, roles, access, custody, presence, diagnostics |
| `POST /api/spaces/{id}/worlds/{world}/rpc` | one World in that Orbit — for `issues`, the whole tracker |

Plus `GET /api/spaces` (the catalog), `GET /api/events` (the doorbell stream, as
SSE — this is what replaced `watch`), and `GET /api/session` (presence, carets,
and drained signals over a WebSocket).

```bash
# Start a head in the background. It keeps running and serving, so read the
# readiness line out of a file rather than a pipe — the pipe never sees EOF.
lait --json --port 0 > ready.json &
until [ -s ready.json ]; do sleep 0.2; done
PORT=$(jq -r .port ready.json); TOKEN=$(jq -r .token ready.json)

post() { curl -sS --fail-with-body -X POST "http://127.0.0.1:$PORT$1" \
           -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' -d "$2"; }

# found a space, then find the local Orbit id every Space route takes
post /api/host/rpc '{"cmd":"host_space_found","home":"/tmp/demo/.lait","name":"demo"}'
ORBIT=$(curl -sS "http://127.0.0.1:$PORT/api/spaces" -H "Authorization: Bearer $TOKEN" | jq -r '.[0].id')

# file an issue and read the board
post "/api/spaces/$ORBIT/worlds/issues/rpc" '{"cmd":"issue_new","title":"fix login race","project":"ENG","priority":"high"}'
post "/api/spaces/$ORBIT/worlds/issues/rpc" '{"cmd":"board","project":"ENG"}'
```

`ci/smoke-p0.sh` is the executable version of that, run on every supported OS on
every push. The credential posture, the custody fence, and the full route table
are in [`docs/SERVE.md`](docs/SERVE.md).

## Use from an AI agent (MCP)

Register the MCP server by POSTing `host_install_mcp` to the host plane — it
merges a portable `lait` entry into that client's `mcpServers` (preserving any
others). The entry is `lait` off PATH with no captured home; Astrolabe's
Library authors the same shape, including the World pin (`post` is the helper
defined above):

```bash
post /api/host/rpc '{"cmd":"host_install_mcp","client":"claude","name":"lait-issues","dir":"'"$PWD"'","world":"issues"}'
```

`client` is `claude | cursor | windsurf | generic`; `scope` picks `user` or
`project`; `world` is the mount this session speaks (`issues` today);
`print` returns the would-be file contents instead of writing them.
Naming the client also names the **agent identity** its work signs as, which is
what makes the agent appear as itself in the roster rather than as an unnamed
key. The agent's first `whoami` asks the person on this machine to sponsor it
(Astrolabe notifies). Or sponsor it from Settings → Members in the app.

Or add it to `.mcp.json` by hand:

```json
{
  "mcpServers": {
    "lait-issues": {
      "command": "lait",
      "args": ["mcp"],
      "env": { "LAIT_AGENT": "claude", "LAIT_WORLD": "issues" }
    }
  }
}
```

`lait mcp` binds a space by walking up from the client's working directory
for a `.lait/`, and is pinned to that Orbit for the session. `$LAIT_WORLD`
pins the session to one World; unset, a build that hosts a single World takes
that pin. An unknown mount is a refusal, not an empty tool list. Nothing is
created implicitly, so a directory with no store is a refusal that names what
does exist — found or enter a space in the app first.

Tools exposed: the pinned World's designed surface (for Issues: `issues_new`,
`issues_edit`, `issues_move`, `issues_assign`, `issues_label`,
`issues_comment`, `issues_delete`, `issues_view`, `issues_list`,
`issues_board`, history, projects, labels, roles, access, workflows, Specs
and activity) plus shell-owned membership, diagnostics, identity, peer
discovery and sync. Each returns the **same versioned JSON DTO** the HTTP
planes emit; a build-gate parity test keeps the agent and browser surfaces in
lock-step. The World owns the tool list; `lait mcp` does not generate tools
from the wire protocol.

## Multi-node & end-to-end encryption

The default invite carries a **signed, single-use pass**, so a teammate is on
the board after one paste into Welcome — no separate approval round-trip. The
link carries a **bearer** admission capability: authority rides the channel you
send it over, bounded by expiry (default 7 days) and one use. Accepting the
invite *is* the approval; redemption completes automatically on the joiner's
first contact with a member.

What an invite admits as is chosen when you mint it in Settings → Members:

- **viewer** — read-only membership
- **contributor** — the default working membership
- **administrator** — full policy administration (the issuer must hold it)
- **reusable + a shorter TTL** — one link admits the whole team for a day

Space data is E2EE: issues sync as ciphertext, and a node that isn't in the
signed ACL (or has been removed) sees only ciphertext. Automatic redemption
never weakens this — the seal still happens key-side on an admin node holding
the space key, which also verifies the capability, its signed role expansion,
and the issuer's authority to delegate every capability it grants. Changes
propagate live P2P over iroh with no central server; any always-on node
advertised in a ticket acts as a portable seed that backfills cold clients.

Revoking is the mirror image: removing a member from Settings → Members rotates
the key, so they cannot read new content (lazy revocation).

## Running several nodes on one machine

Set a distinct `LAIT_HOME` per node and give each head its own port. One founds,
the other enters from the invite — there is no shared "room name": the gossip
topic derives from the space id carried in the ticket.

```bash
LAIT_HOME=/tmp/alice lait --port 7717 --json &
LAIT_HOME=/tmp/bob   lait --port 7718 --json &
```

Each head serves its own identity's spaces, with its own per-run token. Found in
alice's tab, mint an invite there, paste it into bob's Welcome.

## Licence

lait is **source-available, not open source**. Every first-party package in this
repository — the engine, the products, the client, the receivers, the viewer and
the tooling — is offered under the [PolyForm Noncommercial License
1.0.0](https://polyformproject.org/licenses/noncommercial/1.0.0), reproduced in
[`LICENSE`](LICENSE).

You may read, run, modify and redistribute it for any **noncommercial** purpose.
PolyForm names those purposes explicitly: personal study, research, experiment,
hobby and amateur projects, and use by charitable, educational, public-research,
public-safety, environmental and government organisations — regardless of how
that organisation is funded.

**Any commercial use requires a separate licence.** Ask: <omar@onnixi.com>.

Releases up to and including v0.9.0 were published under `MIT OR Apache-2.0`.
That grant is irrevocable for those versions and is not withdrawn here; this
licence governs subsequent versions.

The third-party dependency closure stays permissive and is unaffected — see
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and the allowlist in
[`deny.toml`](deny.toml).
