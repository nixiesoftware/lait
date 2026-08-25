# The HTTP head

`lait` is not a command surface. Bare `lait` starts the identity-scoped daemon
and serves a loopback HTTP head over it — the browser app, and the only general
interface the product has. This document describes that head: how to start it,
the three request planes, the credential and origin posture, and the one
refusal that is about custody rather than permission.

The head is an *adapter*, not a second engine. It re-binds the same
`control::Request`/`Response` pair the daemon has always spoken over a named
pipe or Unix socket onto a TCP port, because a browser cannot speak a named
pipe. Nothing behind it knows which transport a request arrived on.

## Starting it

```sh
lait                       # http://127.0.0.1:7717, prints a URL with a token
lait --open                # …and open it in the default browser
lait --port 0              # ephemeral port, when 7717 may be taken
lait --orbit acme          # pin the head to one local Orbit
lait --home <dir>          # serve one self-contained identity rather than the per-user one
lait --json                # one machine-readable readiness line, then serve
```

`--json` prints exactly one line and prints it **before** the listener starts
accepting, so a parent process can read one line and know the port is live:

```json
{"url":"http://127.0.0.1:7717/?token=…","token":"…","port":7717}
```

It exists so tooling does not have to scrape a token out of a sentence with a
regex — which would make prose written for a human into an API. `viewer/scripts/dev.mjs`
and `ci/smoke-p0.sh` are the two in-tree callers.

The port is fixed rather than ephemeral by default: the URL has to be
predictable and the `Origin` allowlist needs a stable name. A collision is
reported rather than silently worked around, because a server that lands on a
different port than it was asked for is a footgun for anything that bookmarked it.

## The three planes

Every plane is `POST`, takes a JSON object with a `cmd` discriminant, and
answers a JSON object. They are disjoint on purpose: a malformed product
request must not be able to fall through into root control decoding.

| Route | Scope | What it answers |
|---|---|---|
| `POST /api/host/rpc` | the daemon (one identity) | bootstrap: founding, entering, device consent, config, the Orbit registry, MCP install, update/restart, orientation |
| `POST /api/spaces/{id}/rpc` | one local Orbit | generic Space authority: membership, invites, devices, roles, access, custody, presence, diagnostics |
| `POST /api/spaces/{id}/worlds/{world}/rpc` | one World in one Orbit | the product itself — for `issues`, the whole tracker |

`{id}` is a **local Orbit id**, not a Space id: two local stores on one machine
may participate in the same Space, so the Space id would not choose between
them. `GET /api/spaces` is what turns a freshly founded store into the id every
Space route takes. `{world}` is the selected package's mount name — `issues`
for the independently installed tracker — the same string that prefixes its
MCP tool names.

Three more routes complete the surface:

| Route | Purpose |
|---|---|
| `GET /api/spaces` | the navigation catalog: every registered local Orbit, passively (listing never places a Station) |
| `GET /api/events` | the doorbell stream, as SSE. `subscribe` is refused on the RPC planes — it is a stream, not a one-shot |
| `GET /api/session` | the WebSocket carrying the transient (presence/caret), control (drained signals), and progress lanes |
| `POST`/`GET`/`HEAD` `/api/spaces/{id}/content[/{content}]` | Content-plane upload and download |

Anything else is the client: a real asset, or the SPA entry so the app can
resolve its own routes.

### Why the host plane exists

Founding a Space, entering one from an invite, and signing this machine's
device consent all happen at the one moment when there **is no space id to put
in a path**. Every other `/api` route is `/api/spaces/{id}/…` and is therefore
unanswerable exactly when those matter. So the route is daemon-scoped, and the
daemon is identity-scoped — the same scope the token on this server already
stands for.

What passes is narrowed by vocabulary, not by scope: `policy::is_host_plane`
is an exhaustive allowlist, so a `Request` variant added later must be
classified before it can reach the route. In particular `Request::Stop` is
daemon-scoped and is **not** on the host plane — a page able to send it could
shut down the server answering it. `HostRestart` is the safe form: it stops the
daemon *under* the head, and the head survives to stand a fresh one back up,
which is what makes a self-update take effect.

That reach is real and worth stating plainly. `HostSpaceFound`,
`HostSpaceEnter`, and `HostInstallMcp` create the directory they are given, so a
caller holding this token can create a directory anywhere this process can
write. Requests that name an *existing* store — the config layer, `HostOrbitRebuild` —
go through `orbits::bootstrap::admit` first, which refuses a path this daemon
does not serve. `HostInstallMcp` writes a portable `lait mcp` entry (`lait`
off PATH, optional `LAIT_AGENT` / `LAIT_WORLD`); it does not snapshot
`current_exe()` or `$LAIT_HOME`. `world` is the mount this session speaks.

### A worked example

`ci/smoke-p0.sh` is the executable version of this section. Abbreviated:

```sh
# found a Space (nothing is created implicitly)
POST /api/host/rpc   {"cmd":"host_space_found","home":"…/.lait","name":"Smoke","nick":"smoke"}
GET  /api/spaces                                    # → the local Orbit id

# the product
POST /api/spaces/{id}/worlds/issues/rpc  {"cmd":"project_new","name":"Engineering","key":"ENG"}
POST /api/spaces/{id}/worlds/issues/rpc  {"cmd":"issue_new","title":"fix login race","project":"ENG"}
POST /api/spaces/{id}/worlds/issues/rpc  {"cmd":"issue_start","reff":"ENG-1"}

# orientation, on both planes
POST /api/spaces/{id}/rpc                {"cmd":"whoami"}
POST /api/host/rpc                       {"cmd":"host_context"}
# whoami as an unsponsored LAIT_AGENT files Context.asks; Astrolabe notifies.
```

## Credentials and origin

The socket *was* the authentication. Binding the same façade to a TCP port
removes the OS permission check that made auth unnecessary, and adds a caller
that never existed before: the web pages the user visits. Two checks replace it,
and the order is deliberate.

**1 — the rebinding guard, first.** `Host` is mandatory and must be a loopback
authority for our port (`127.0.0.1:{port}`, `localhost:{port}`, `[::1]:{port}` —
an allowlist, because a missing entry fails visibly and a permissive match fails
silently and remotely). It runs before the credential check because it is what
survives a successful DNS-rebinding attack, at which point the browser *will*
hand over our cookie.

`Origin` is authoritative when present and must be us. Absent is allowed on
ordinary routes: browsers omit it on same-origin GETs, and `curl` never sends
one — but neither can a non-browser client be *tricked* into carrying our
cookie, which is the only attack this pair exists to stop. The `GET /api/session`
upgrade inverts that: a WebSocket handshake is exempt from CORS, so Origin is
the whole defence there and is required.

The head binds `127.0.0.1`, never `0.0.0.0`.

**2 — the token.** A 32-byte hex token, minted per run and never persisted,
compared in constant time. Three ways to present it, one meaning:

- `Authorization: Bearer <token>` — what a script or `curl` uses;
- the `lait_token_{port}` cookie — what the browser uses after the first load;
- `?token=…` in the query string — the opening navigation only.

**Query beats cookie**, and that precedence is load-bearing: the token is
per-run but the cookie outlives the run that set it, so after a restart the jar
holds a stale credential. Consulting it first would 401 the user out of the link
they just clicked, with nothing in the UI able to clear a cookie it cannot read.
`/` immediately trades the query token for the cookie and redirects, so it never
lingers in history or a `Referer`.

Which routes accept the query form is a property of the route, declared once in
the `ROUTES` table rather than re-decided per handler. Content routes refuse it:
a download URL is a thing people paste, put in a `src`, and leave in their
history, and a live credential in one of those is a credential in the URL bar,
in devtools, and in whatever logs a dev proxy keeps.

The `GET /api/session` upgrade refuses it too, for the same reason and one
more. A `ws://…?token=…` URL is exactly as pasteable as a download URL, and the
handshake already has a header channel — so it takes the cookie or a `Bearer`
header, and a client that builds the WebSocket URL with a query token gets a
`401` rather than a session. In a browser that means the cookie, since
`WebSocket` cannot set a header; that is the credential a same-origin handshake
already carries and the one that does not end up in history.

Every route is registered from that one `ROUTES` table and the gate is applied
as a layer over all of it. That is what makes "no path escapes the gate" a
property a test can check, rather than a habit each new handler has to remember.

## The custody fence

**A Station whose binding carries its own identity directory signs with that
seed.** An agent's Orbit is the case that exists today: `orbits::router` loads
the seed from the resolved identity directory, so a write routed into an
agent-held Orbit through this head goes out over **the agent's signature**.

Mechanics would approve it. It checks the *signer's* grants, the signer would be
the agent, and a sponsored agent legitimately holds `Standing::Write`. Nothing
behind this route asks the question a second time. So the head asks it once, at
the door:

```
{holder}'s space is read-only here — a write would be signed as {holder}.
Open the same space through your own node to write as yourself.
```

This is **custody, not standing**. It is not a judgement about what kind of
member owns the Station and it does not become redundant as anybody's grants
widen — the answer must stay no however wide they become. What it stops is a
person who is not the agent spending the agent's key. The agent writing through
its own node (`lait mcp`, which is that node) is unaffected, because then the
key being spent is its own.

It must never refuse a **read**: looking at a hosted identity's board signs
nothing, and that is the whole reason it is browsable here. `Catalog::signs_with_own_seed`
is the one spelling of the question and `borrowed_key_refusal` is the one
refusal, so the enum shape cannot become a proxy for it on four separate routes.

The same rule governs the host plane, where `orbits::bootstrap::admit` states it
for requests that name an existing store.

## Destructive verbs ask

The browser is the only surface left that *can* ask, and it has a modal, so a
destructive request comes back as `409 confirm_required` carrying the question,
and the UI puts it to the user. Re-sending with `?confirm=1` proceeds. The
question string is `host_client::destructive_question`'s, not a paraphrase, so
no two surfaces can disagree about what is dangerous.

This protects against an *accident*, not an attacker: anything that can POST a
delete can also POST `?confirm=1`. That is the whole of it, and it is worth
being honest about.

## The browser is not a peer

It holds no key, has no entry in the ACL, and is never invited. It is a lens on
a device's replica, and the device remains the only network identity. Listing
Spaces is passive — the catalog answers from durable bindings and never places a
Station to satisfy a probe. Selecting a Space is what attaches its daemon, and
therefore the first point at which anything is started.

## The bundle

The Issues viewer's build output is **committed** under
`products/issues-app/assets/web/`. The generic host embeds no product client;
bootstrap installers and the World publication workflow place these bytes in
the immutable Issues release beside its runner and declaration. Committed build
output can still go stale, so CI rebuilds it and fails if the result differs
from git. After a viewer change, locally:

```sh
(cd viewer && npm run build)   # regenerates products/issues-app/assets/web/*
cargo build -p lait-issues-runner -p lait
```

## Related

- [`PROTOCOL.md`](./PROTOCOL.md) — the control protocol these planes carry, and
  its compatibility rules.
- [`ARCHITECTURE.md`](./ARCHITECTURE.md) — where the head sits in the daemon /
  Orbit / Station graph.
- [`THREAT-MODEL.md`](./THREAT-MODEL.md) — what the loopback posture does and
  does not claim.
