# lait workbench

`lait-workbench` is a UI-neutral **supervisor library**. It owns lait daemon
lifecycle, device registration, authoritative observation and history, and it
holds no opinion about how any of that is drawn.

## Using it

Its consumer — the Astrolabe client — links this crate and calls it directly on
native Rust types. There is no serialization, no generated binding and no local
HTTP hop between the two.

Both ends of the lifetime are explicit calls rather than side effects of some
`main`:

```rust
let supervisor = Supervisor::start(Config::new(state_root, executable)).await?;
// … supervisor.add_device(…), start_device(…), snapshot(), subscribe() …
supervisor.shutdown().await;
```

`start` takes the first observation before it returns, so a caller's first read
is authoritative rather than empty — an empty first snapshot is
indistinguishable from a machine with no daemons on it. `shutdown` stops the
background sampling and then stops every *owned* daemon. Dropping every handle
without calling it stops the sampling and leaves owned daemons running, which is
the right reading of a consumer that crashed: those daemons come back as
`external` to whoever supervises next.

## The HTTP adapter

The `http` feature adds a loopback HTTP adapter over the same DTOs and the same
safety rules, and the `lait-workbench` binary that serves it. It is a
**diagnostics and testing surface**: it is not a user-facing executable, and
nothing that embeds the supervisor reaches it this way. The binary does not
build without the feature.

```sh
cargo run -p lait-workbench --features http
```

Build `lait` and `lait-workbench` into the same target directory. Configuration
is bootstrap plumbing supplied by the dev launcher:

- `LAIT_WORKBENCH_ROOT`: managed state root; defaults to
  `./target/lait-workbench`.
- `LAIT_BIN`: lait executable; defaults to the `lait` binary beside the
  workbench executable.
- `LAIT_WORKBENCH_PORT`: loopback port; defaults to `0` (ephemeral).

The first stdout line is machine-readable readiness data:

```json
{"url":"http://127.0.0.1:62616","token":"...","port":62616,"stateRoot":"..."}
```

Logs go to stderr. A launcher should retain the token in memory and give it to
the local frontend process, never persist it.

## Authentication

Every route requires both a loopback `Host`/`Origin` and either
`Authorization: Bearer <token>` or the run-scoped HttpOnly cookie. A browser can
exchange the bearer for that cookie with:

```http
POST /api/workbench/session
Authorization: Bearer <token>
```

The cookie is needed for native `EventSource`, which cannot set an Authorization
header. The gate reuses lait's DNS-rebinding protection.

## API v1

- `GET /api/workbench/snapshot` returns the authoritative snapshot.
- `GET /api/workbench/contract` returns the versioned route table and JSON
  Schemas generated from the Rust DTOs. Clients should generate or validate
  bindings from this document instead of maintaining a parallel model.
- `GET /api/workbench/events` is an SSE invalidation stream. Fetch a fresh
  snapshot after each event; events are not state patches.
- `GET /api/workbench/history/events?afterRevision=0&limit=200` returns the
  bounded event journal. `deviceId` optionally filters it.
- `GET /api/workbench/history/connections?afterRevision=0&limit=200` returns
  connection transitions (`connected`, `changed`, and `disconnected`). It can
  be filtered by `deviceId`, `spaceId`, and `peerId`.
- `GET /api/workbench/devices/{id}/logs?cursor=0&limit=200` returns structured
  daemon stderr entries. Omit `cursor` for a bounded tail; pass `nextCursor`
  back to continue. `reset: true` means the file was truncated since the
  supplied cursor.
- `POST /api/workbench/devices` accepts
  `{"id":"alice","label":"Alice","start":true}`. The backend chooses the
  home below `<stateRoot>/devices`; the browser cannot provide a filesystem
  path.
- `POST /api/workbench/devices/{id}/actions` accepts one of
  `{"action":"start"}`, `stop`, `restart`, or `force_stop`.

Snapshots use camelCase and carry `schemaVersion: 1`. Device lifecycle states
are `stopped`, `starting`, `running`, `stopping`, `external`, and `failed`.
Each device includes its PID, ownership bit, identity home, daemon log path,
start time, and last error.

`connections` contains passive observations from already-running Stations:
`sourceDeviceId`, `spaceId`, `peerId`, `peerNick`, `state`, `online`, `dialable`,
and `blockedBy`. Snapshot reads never place an inactive Station merely to make
the graph look populated.

The workbench samples passive observability once per second. Connection or log
changes also produce SSE invalidations, so the UI can fetch the relevant cursor
page. Event history retains the newest 1,024 entries and connection history the
newest 2,048 transitions for the current workbench run. History responses set
`droppedBefore` when a requested revision is older than retained state. Logs
stay in the daemon's existing file; the API reads bounded pages and strips
terminal color codes into timestamp, level, target, and message fields.

## Process safety

Daemons are spawned through `lait`'s own spawn path, which inherits exactly the
three stdio handles it names and, on Windows, allocates no console: a
console-subsystem child spawned from a GUI parent would otherwise flash a window
on screen, and one spawned from a terminal would join that terminal's process
group and take its Ctrl-C.

Graceful stop uses the daemon control protocol. Force-stop is only available
through the owned child handle returned by spawn; it never accepts an arbitrary
PID. A compatible daemon discovered in a managed home is reported as
`external`, and neither stop nor force-stop will terminate it. On graceful
workbench shutdown, all owned daemons are stopped while external daemons remain
untouched.

Device definitions are stored atomically in `<stateRoot>/devices.json`. The
state root is single-writer locked. After a workbench crash, registrations are
reloaded and surviving daemons are reported as `external`; the new supervisor
does not claim an owned handle it cannot prove it created.
