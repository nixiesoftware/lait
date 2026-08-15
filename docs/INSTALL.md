# Installing lait

`lait` is a single self-contained binary. Native builds ship for **Linux** and
**macOS** (arm64 + x86_64) and **Windows** (x86_64 — arm64 Windows runs the
x86_64 build under the OS's built-in emulation). Pick whichever channel fits your
platform — they all land the same `lait` executable. Upgrading is node
maintenance rather than a command: the running daemon knows which build it is,
so `{"cmd":"host_update"}` on the host plane (see [`SERVE.md`](./SERVE.md))
pulls the latest release and atomically swaps the binary, and
`{"cmd":"host_restart"}` makes the swap take effect. Re-running the installer for
your channel works just as well.

> **Heads-up on the crypto:** lait's end-to-end encryption is research-grade and
> **not yet independently audited** — don't trust it with sensitive data yet. See
> [`THREAT-MODEL.md`](./THREAT-MODEL.md).

## Astrolabe desktop client

Astrolabe carries the Flutter interface, its Rust core, and the matching `lait`
sidecar as one release bundle. The tagged installer workflow currently ships:

| Platform | Release artifact | Installation shape |
|---|---|---|
| Windows x86_64 | `astrolabe-<version>-setup.exe` | per-user NSIS install |
| macOS arm64 | `astrolabe-<version>.dmg` | signed, notarized drag install |
| Linux x86_64 | `astrolabe-<version>-x86_64-unknown-linux-gnu.tar.gz` | relocatable directory |

The Linux archive is the first distribution vehicle, deliberately not a claim
that one distro package manager represents Linux. Extract it and run the
`astrolabe` executable inside; keep its `lib/`, `data/`, `libastrolabe.so`, and
`lait` siblings together. It is built and smoke-checked on Ubuntu 24.04, and
requires the target system's GTK 3 and AppIndicator runtime libraries. The
daemon-plus-browser path below remains supported on every Linux architecture,
including arm64 while the desktop client artifact is x86_64-only.

## Quick pick

| You have… | Use |
|---|---|
| macOS / Linux, want one command | the shell installer |
| Windows | the PowerShell installer, Scoop, or winget |
| Homebrew | `brew install nixiesoftware/tap/lait` |
| Rust toolchain, want to build | `cargo install --git …` |
| Running an always-on seed | Docker |

## Shell installer (macOS / Linux)

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/nixiesoftware/lait/releases/latest/download/lait-installer.sh | sh
```

Places `lait` in `~/.cargo/bin` (on `PATH` for most setups).

## PowerShell installer (Windows)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/nixiesoftware/lait/releases/latest/download/lait-installer.ps1 | iex"
```

## Homebrew (macOS / Linux)

```sh
brew install nixiesoftware/tap/lait
```

## Scoop (Windows)

```powershell
scoop bucket add lait https://github.com/nixiesoftware/scoop-bucket
scoop install lait
```

## winget (Windows)

```powershell
winget install NixieTechLLC.Lait
```

## cargo-binstall (prebuilt, no compile)

lait is not published to crates.io. Its workspace crates are internal
boundaries rather than a library surface, and crates.io has no private tier
and no delete — publishing would claim those names permanently and turn each
internal API into a public semver commitment. Build from the repository:

```sh
cargo install --locked --git https://github.com/nixiesoftware/lait lait
```

> The `lait` crate on crates.io stops at **0.4.8** and is not maintained.
> `cargo install lait` and `cargo binstall lait` would install that old
> version silently — use the `--git` form above.

Requires **Rust 1.91+** (the floor is driven by iroh 1.0.0-rc.1).

## From a git checkout

```sh
git clone https://github.com/nixiesoftware/lait
cd lait
cargo build --release   # → target/release/lait
```

## Docker — always-on seed node

A seed is a headless peer that stays reachable to bootstrap and backfill
encrypted space history for other nodes. It holds only ciphertext until an
admin admits it.

```sh
docker compose up -d --build          # from the repo root
```

The container runs `lait daemon`, which is headless on purpose — there is no
command surface inside it to type at. Binding it to a space is a one-time act
performed by a head pointed at the same node home, and the head **binds
loopback only**, so it cannot be reached from outside the container it runs in.
The practical shape is therefore: give the node a bind-mounted home instead of
an anonymous volume, and bootstrap it with a head on the host.

```yaml
# docker-compose.override.yml
services:
  seed:
    volumes:
      - ./seed-home:/data
```

```sh
docker compose stop seed                       # the store takes one writer
LAIT_HOME=./seed-home lait --open              # Welcome → Use an invite
docker compose start seed
```

Paste the `lait://join/…` link into Welcome. The store is then bound to the
space, and the long-running `seed` service serves it from there. Admitting the
seed is the inviter's side of the same flow, from their own app.

See [`docker-compose.yml`](../docker-compose.yml) for details. iroh handles NAT
traversal via relays, so no inbound port is required (publishing a UDP port just
speeds up direct dials).

## Verifying a download

Every release archive ships a `.sha256` sidecar — that is the one to use for a
binary download. (`sha256.sum` on the release page is the signed manifest for
`source.tar.gz`; it does not list the platform archives.) To check a manual
download:

```sh
sha256sum -c lait-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## Shell completions & man page

There are none, and there is nothing for them to complete. `lait` is a launcher
with three modes, not a command tree:

```sh
lait                       # the local app, and the daemon under it
lait daemon                # the identity-scoped host, headless
lait mcp                   # the stdio head an agent speaks
lait --version             # which build this is
```

## After installing

```sh
lait --open
```

That starts the daemon, serves the app on `127.0.0.1:7717`, and opens it. With
no space on this machine yet you land on **Welcome**: *Found a space* mints a
new one, *Use an invite* pastes a teammate's `lait://join/…` link and bootstraps
your store from it. Nothing is created implicitly, so the first act is always
deliberate.

Register the MCP server with an AI agent from the host plane — see
[`AGENT-EXPERIENCE.md`](./AGENT-EXPERIENCE.md) for the two-step version with a
sponsored identity:

```sh
# $PORT and $TOKEN come from the head's readiness line — `lait --json`
curl -sS --fail-with-body -X POST "http://127.0.0.1:$PORT/api/host/rpc" \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"cmd":"host_install_mcp","client":"claude","name":"lait-issues","dir":"'"$PWD"'","world":"issues"}'
```
