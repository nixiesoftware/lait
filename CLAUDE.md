# Working on lait (agent notes)

lait is a local-first, peer-to-peer issue tracker: a Rust engine (`src/`, `crates/`)
and a React viewer (`viewer/`) that the binary hosts over the engine on loopback.

**There is no command surface.** `lait` is a launcher with exactly three modes:

```sh
lait daemon [--home <dir>]                        # the identity-scoped host
lait mcp                                          # the stdio head for an agent
lait [--json] [--port N] [--orbit SEL] [--open] [--home <dir>]   # the local app + the daemon under it
lait --version                                    # which build this is
```

Anything else exits 1. Everything a verb used to do is now a request one of those
three carries — see [`docs/SERVE.md`](docs/SERVE.md) for the three HTTP planes.

## Plans live in lait, not in markdown

`docs/plans/` is **gitignored and deprecated**. A plan that lives in a file on one
machine is a plan that goes stale where nobody can see it happen — the
address-book docket's callsite table drifted a whole minor version, and the head
it named had been overtaken by a decision recorded in two other Plans. Neither
was visible until somebody went looking.

Author a plan as **Specs**, in the project that owns the work:

| Kind | What goes in it |
|---|---|
| `requirement` | what may never happen. Constraints, invariants, bounds, non-goals. |
| `design` | structure — the model, the placement, the seams. |
| `plan` | the order of work and its exit criteria. |
| `guide` | what an implementer has to find: callsites, traps, prior art. |
| `record` | measured facts. |

They are separated because **the order of work is the one part that is meant to
become obsolete**, and burying invariants inside it makes them obsolete too.

Link them onto the issues they govern (`links: [{rel: "governs", target: {kind:
"issue", issue: "iss_…"}}]`) and `packet` on any issue returns the lot, sorted
into buckets. The address book is the worked example: five Specs in the Substrate
project, reachable from **SUB-4** and **CLIENT-22**.

**Point a Guide at names, never at line numbers.** A name survives a refactor.

## Astrolabe — the client above all of it

Astrolabe is the local client through which a person reaches the Worlds their
device serves, over a **Rust core** (`tools/astrolabe`). Reference shape is the
Steam client — a library, a launcher, an identity — that **never draws a
World**. `Open` is a handoff to the person's browser; `products/issues` ships
its own head and stays the authority on its own presentation.

**Tauri is the only interface** (`apps/astrolabe-web`, TypeScript/React over
`src-tauri`). The Flutter interface (`apps/astrolabe`) and the egui one before
it are both out of the live build: egui was deleted, Flutter is **deprecated
and unwired** — see `apps/astrolabe/DEPRECATED.md`. Do not add to it, do not
keep it compiling, and do not treat its generated Dart as current.

Nothing in `tools/astrolabe` speaks to it any more: `flutter_rust_bridge`, the
checked-in `frb_generated.rs`, every `#[frb]` annotation, and
`api::watch(StreamSink)` are gone, and `ci/bridge-drift.sh` was deleted with
them. `api::subscribe` — a native callback — is the only way to watch the view
stream. **There is no generated binding and no codegen step any more.** Tauri's
host takes `api::ClientView` apart by **exhaustive destructuring**, so a field
added to the boundary stops compiling there until somebody decides what the
client does with it; the compiler is the whole check.

The client is gated by two jobs: `astrolabe-web (client)` typechecks and tests
the TypeScript, `astrolabe-web (host)` compiles the Rust behind a webview
toolchain, and both are in `orbital-complete`'s dependency list. `src-tauri` is
deliberately **outside the cargo workspace** so the main build needs no webview
libraries — which is why it needs a job of its own rather than none.

### Tauri is the installer, and the feed is the distribution

No canonical install or update ships through Cargo, a package registry, or a
git-forge release page. Installed machines follow a **signed channel pointer on the dist host**
(`src/update/feed.rs`, SUB-13) and download immutable artifacts from the same
GCS bucket. Cargo compiles Rust during a build; it is not an install or update
channel.

A `vX.Y.Z` tag runs `.github/workflows/release.yml`, which calls the
repository-owned native host and Tauri builders and assembles one short-lived
Actions artifact. That artifact is build transport only. Promotion seals and
publishes those bytes through our feed:

```sh
ci/publish-feed.sh --from-run <run-id> --version 0.9.1 --channel test \
  --seed ~/.lait-feed-signing.seed
# after verifying the test channel, publish the same run to stable
ci/publish-feed.sh --from-run <run-id> --version 0.9.1 --channel stable \
  --seed ~/.lait-feed-signing.seed
```

`--version ... --artifacts-dir ...` is the local recovery path. There is no
`--from-release`: GitHub Releases are not an artifact source or an authority.

**A publish is heard within the round trip, not on the period.** Both publish
scripts end by `POST`ing the pointer they just wrote to the notify relay
(`tools/feed-notify`, `lait-feed-notify` on Cloud Run as `foundation-notify`,
deployed from `nixiesoftware/foundation`; it will sit behind
`notify.foundation.pub` once the zone carries a record), and
every installed daemon holds one SSE subscription to it (`src/update/notify.rs`),
keyed by `update.notify` / `LAIT_FEED_NOTIFY`. A frame is opened with the same
pinned feed keys and only *wakes* the staging watcher — the watcher then walks
the bucket exactly as it does on its period, and a staged World release
relaunches the daemon generation on the spot instead of waiting for the next
start. The relay holds no keys of its own, verifies with the operator's
`--pubkey`s, and ratchets `published_at` forward per key; it is a doorbell, not
a second feed, and it primes its board from the bucket on start and every
five minutes, so a lost announce costs minutes and a restart costs nothing.
The 4.5 h check period is the floor for a machine that cannot hear it, not
the latency. Self-hosting is one binary and one config key.

The build emits what the feed serves, named the way it names things: the
`.dmg` a person installs, and `astrolabe-tree-<version>-<target>.tar.gz`, which
is what an already-running machine swaps in. **A release with an installer and
no tree can be installed and never updated from**, so the tree is not optional.

The bundle carries `lait` inside it (`bundle.externalBin`, staged from this
tree by `scripts/stage-sidecar.mjs --bundle`) as `Contents/MacOS/lait` beside
`Contents/MacOS/astrolabe`. Both names are load-bearing: `sidecar::beside`
looks for the first, `update::custody_of` looks for the second — which is why
`mainBinaryName` is `astrolabe` and not the product name. `build-astrolabe.sh`
runs the bundled sidecar and compares its version to the release, because "the
pair ships together by construction" is a claim and this is where claims get
checked.

The client matrix is deliberate: Windows x64 ships a per-user NSIS installer
whose stable launcher owns the swappable `current/` tree; Apple Silicon macOS
ships a Developer ID-signed, notarized, and stapled DMG; Linux x64 ships a
relocatable Tauri tarball. Each platform also emits the update tree an installed
client consumes. Intel macOS has a bare host archive but no Astrolabe client.

`.github/workflows/build-astrolabe.yml` is the live native Tauri builder. Its
Windows and Linux artifacts receive build-provenance attestations; macOS signs
every nested executable inside-out before sealing, notarizing, stapling, and
attesting the DMG. `apps/astrolabe/` is only a deprecated historical snapshot.

```sh
cd apps/astrolabe-web           # the canonical client
npm ci && npm run tauri dev     # stages the lait sidecar, then builds + runs
                                # the Rust core behind a webview. The sidecar
                                # is resolved beside the host binary, never
                                # from PATH — scripts/stage-sidecar.mjs puts
                                # it there; without it the identity daemon
                                # cannot start.
npm run check && npm test       # tsc, then the vitest suite
cargo test -p astrolabe         # the core: model, client, launch seam
```

The core is layered, and the boundary is `api/mod.rs` and nothing else:

- `client/` is the reach: the supervisor library it embeds (`tools/workbench`)
  and the host, Space and World planes it speaks. It draws nothing, which is why
  its rules are testable without a window.
- `model.rs` is the App-owned state — the *only* model of client state. The
  interface receives whole immutable `ClientView` projections and holds nothing
  but drafts. There is no optimistic local mutation, because that would be a
  second model disagreeing with the first exactly when an action was refused.
- `api/mod.rs` is the whole boundary: one `ClientView` out, one `ActionRequest`
  back. The one read outside that pair is `world_artwork` — a World's
  PNGs from its selected immutable release, asked for once per mount and
  cached on the interface side because the view is pushed whole to every
  surface on every pump.

  A field added here fails to compile in `src-tauri/src/main.rs` until somebody
  decides what the client does with it — that exhaustive destructure is
  deliberate and must not be relaxed with `..`. The `Generated/` drift check
  that `apps/astrolabe-ios/build-core.sh` says CI performs is still unwired.

### Dispatch returns the view; a surface never keeps an answer

A control reads
`view.inFlight.includes(actionKey.…)` and disables itself on the frame it was
clicked. The keys live in `actionKey`, and `Action::key` in
`tools/astrolabe/src/runtime.rs` spells the same strings — a key that disagrees
fails *nowhere*, it just never matches `inFlight`, so the control stays live
through its own action and can be pressed twice.

**They are not actually pinned against each other, whatever this file used to
say.** `client.test.ts` asserts TypeScript literals against TypeScript
literals, which catches a typo in the test and nothing in the core. Rename a
key on the Rust side and every control keyed on it goes quietly live. Ten keys
now ride on that; treat a key change as a two-sided edit until something checks
it.

### The Library is the catalog, not the install list

One row per reviewed first-party catalog entry, plus any independently
installed World not in that catalog. Listing starts no runner and fetches no
payload. Catalog membership and installation are separate facts: an
uninstalled row offers `Install`, reports `Installing` progress while its signed
channel artifact is fetched and verified, and offers `Open` only after an
immutable release is selected. Name, tagline, accent and entry path come from
the catalog until the installed release supersedes them. Which Spaces serve a
World, and whether any is up, are the destination's facts: the head's front
page carries the Space selector, and selecting there is what attaches a
daemon. Do not reintroduce Space rows or a placement badge here — a row whose
kind depends on whether a daemon is up is the "Unnamed Space" defect.

### Rules that are tested, not documented

Removal and data deletion are separate; deletion re-proves containment under the
managed root *at deletion time*, and is confirmed by typing the device's name. A
sampling failure degrades and preserves the last good topology — it never reads
as "no peers". Ownership is a boundary: force-stop lives on an owned handle and
there is no pid-based path to it. The overlay renders convenience and refuses
authority.

**Unmeasured is absent, never zero — and an absence says which kind it is.**
"This Space is not running" and "this Space could not be asked" are different
facts, and only one is worth acting on; folding them together is the
false-disconnection defect one layer down. Same for a diagnosis that could not be
taken, which is never "every gate passes".

**Listing is passive; choosing is the act.** The Library reads only what is
compiled in. `Open` starts the identity head; selecting a Space happens at the
destination.

### MCP ownership

The World designs the agent surface (tools, omissions, teaching text).
Astrolabe authors the editor binding (`LAIT_AGENT`, `LAIT_WORLD`) from the
selected Library World and never parents that process. `lait mcp` is the
stdio adapter: editor → `lait mcp` → daemon → WorldHost. Do not elevate
MCP onto Astrolabe, and do not generate tools from the wire protocol.

A project-scoped binding lands beside a `.lait` store, never inside one.
An unknown client name is refused.

### The client-to-process seam has been wrong twice

Both times: every component correct, the composition wrong, and a symptom that
named nothing. `start_head` passed `--home` to a launcher mode that did not
accept it, so the head exited before printing and the supervisor reported "head
exited before it announced an address". Then `Client::head` handed the supervisor
the *daemon's* directory beneath the identity, so the head came up, announced an
address, minted a valid single-use ticket — and served an identity nobody had
ever used.

`tools/astrolabe/tests/launch.rs` exists for that class and asserts the chain
rather than the parts. **Add to it before trusting a new seam between the client
and a real process.**

## Two generated files, and only one is Windows-hostile

`THIRD-PARTY-NOTICES.md` is generated from the lockfile by
`bash ci/third-party-notices.sh --update` and CI fails when it drifts. It is
platform-independent — `cargo metadata` resolves every target — so regenerate it
wherever you are.

## The coverage manifest can only be regenerated on Linux

`ci/coverage-manifest.txt` records every test id. Adding or renaming a test fails
the `coverage manifest` CI job until it is refreshed, and
`bash ci/coverage-manifest.sh --update` **refuses to run anywhere but Linux** by
design — Linux is where the check runs, and no other platform lists the same
ids. Windows drops the `cfg(unix)` tests and would record their absence as
coverage loss; macOS adds eight `cfg(target_os = "macos")` ones (the bundle
exchange, and the daemon's own bundle check) and would write a superset the
check rejects. Push, let the job fail, and take the file it uploads:

```sh
gh run download <run-id> -n coverage-manifest -D <dir>   # coverage-manifest.txt.actual
cp <dir>/coverage-manifest.txt.actual ci/coverage-manifest.txt
```

Budget one extra push per batch of test changes, and batch them.

## Driving the viewer in a headless browser

The viewer is a React SPA. **Do not navigate it with synthetic clicks.** An eval'd
`element.click()` fires React's `onClick` *inside* the eval; the resulting re-render
plus `history` update detaches the automation (CDP) execution context, so you get an
opaque `Uncaught` and the click usually has no effect. Dispatched events and
`history.replaceState` are unaffected — so navigate with the built-in event, which
the app handles on a deferred task:

```js
window.dispatchEvent(new CustomEvent("lait:nav", { detail: { view: "settings" } }))
```

`detail` fields:
- `{ view }` — `overview | list | board | calendar | timeline | projects | inbox | my-issues | activity | specs | settings`
  (the union lives in `viewer/src/core/registry.ts`)
- `{ project }` / `{ issue }` — select a project (KEY), or OPEN an issue (ref):
  a ref mounts the issue's detail (its read, editor, and live session); `null`
  clears the selection
- `{ milestone }` — scope the issue surfaces to a `mls_` id; `""` is the
  No-milestone bucket and `null` clears the scope. Applied *after* `project`, so
  `{ project: "ENG", milestone: "mls_x" }` in one detail scopes ENG. Stays on the
  current view if it draws rows (list/board/calendar), else lands on Issues.
- `{ tab }` — Settings sub-page: `preferences | profile | notifications | general | members | teams | devices | labels | workflow | access`
  (the list lives in `viewer/src/ui/settings/pages.ts`)
- `{ project: "<PROJECT_KEY>", view: "overview" }` — enter that project's overview page

Canonical project URLs are nested under
`/spaces/:space/projects/:project/{overview|issues|board|calendar|activity}`.
The old `?project=` and `?overview=` forms remain accepted as compatibility
inputs but are replaced with the canonical path after navigation.

To reach a sub-state, dispatch the view first, wait ~1s for it to mount, then
dispatch the sub-state. `wmux browser open <full-route-url>` also works (a full page
load avoids the in-eval history problem). The hook lives in `viewer/src/App.tsx`
(search `lait:nav`); Settings and Projects add their own listeners. It is inert in
normal use — nothing dispatches it.

### Ask the page, don't photograph it

A screenshot costs ~20k tokens and answers *"does this look right"*. Almost
nothing you need to know is that question — the last design walk found four
defect classes and every one of them was a number. `window.lait` carries the
tools that return those numbers, in dev builds only (`viewer/src/dev/inspect.ts`,
loaded behind `import.meta.env.DEV`, absent from the shipped Issues release):

```js
lait.where()                                  // url, viewport, theme, open dialog
lait.look(sel, { within, all })               // geometry + the styles that matter
lait.tree(sel = "#root", depth = 4)           // structure, no utility-class noise
lait.text(sel = "#root")                      // visible text, collapsed
lait.rungs()                                  // the ladder as it resolves at runtime
lait.go(detail)                               // dispatch `lait:nav` (see above)
```

`look` is the one you want. It deduplicates — eight identical tabs report once
as `8×` — drops matches that render nothing (a board answers `.astryx-button`
with 407 elements, 399 of them collapsed menus), and resolves a measurement to
its rung, so `36px` comes back as `bar-md`. It prints the properties the walk
found broken: explicit `width`/`max-width`, four-sided padding, `gap`, and
`justify-content`/`align-items` on every flex container even at their initial
values. Values nobody set are omitted; `{ all: true }` prints everything.

Budget a screenshot for the genuinely visual call — *is that shadow a halo, is
this cramped* — and measure for everything else.

Two traps, both learned the hard way:

- **`lait.go` does not wait, and you must not make it.** Awaiting frames inside
  the eval holds its execution context open across the re-render that `App.tsx`
  defers specifically to avoid — the same detachment as a synthetic click, from
  the other side. It cost a 45s CDP timeout on a navigation that had already
  succeeded. Dispatch in one call, ask `lait.where()` in the next.
- **A zero box does not mean hidden, and `checkVisibility()` does not either.**
  The app shell's root is `display: contents`; both tests prune it and take the
  whole tree with it. `rendered()` in `inspect.ts` asks `display` directly.

Start a head and get its URL/token: `lait --json` → one line, `{url, token, port}`,
printed *before* the listener accepts. `--port 0` takes an ephemeral port when
7717 may be busy. The token is the Bearer credential for every `/api` route.

The Welcome flow is what a browser sees when no Space exists yet: it founds one
(`host_space_found`) or enters one from an invite (`host_space_enter`) over
`POST /api/host/rpc`. Nothing is created implicitly, so a driver script that
wants a Space must ask for one.

## Working on a World, in the client that runs it

**There is no viewer dev server.** There was one — Vite on `:5178` proxying to
an engine on `:7717` — and it was removed. Two origins, a token carried by a
proxy instead of a cookie, a seam that existed only in development: what it
showed you was *a* viewer, not the one in the window. Do not reach for it, and
do not add it back.

A World is looked at in the client, as a **local World**: its own Library entry,
its own id and mount, sitting beside the release it was copied from rather than
replacing it.

```sh
(cd viewer && npm run build)          # → products/issues-app/assets/web
cargo stage-worlds
```

Then in Astrolabe: **+ Add local World** at the foot of the Library rail, and
pick `target/local-worlds/worlds/com.lait.issues/<version>`. The directory is
the whole ask — the name comes from the tree's own `world.json`.

After that, a page change is a rebuild and a reload, because a head reads a file
per request:

```sh
(cd viewer && npm run build)
cp -R products/issues-app/assets/web/. \
  target/local-worlds/worlds/com.lait.issues/<version>/
```

A **runner** change — `products/issues-app/src` or the crates under it — needs a
rebuild, a re-stage, and the World stopped and opened again from the Library.
Stopping and opening is the refresh; there is deliberately no reload command,
and nothing reaches into a World's page.

Kill running daemons before a test build — a running `lait` holds the `.exe`
lock (`taskkill //F //IM lait.exe` on Windows).

### What a local World is, and is not

An **unsealed World tree**: what a build produces before anything signs it, a
`world.json` beside the runner and pages it declares. Not a directory of pages —
that is refused when you add it.

- **It is a different World, and its runner is told so.** The host assigns it
  `local.<handle>` and mounts it at `local_<handle>`, so its MCP tools are
  `local_issues_list`, its routes are `/local_issues/`, and nothing that
  resolves by name can confuse it with the release. The name is not a label on
  top: it reaches the runner as `LAIT_WORLD_ID`, and the World serves under it —
  so its Bodies, capabilities and resources are all keyed by it, and it has
  **its own data**, an empty Issues rather than yours. A World that ignores the
  name is refused at admission, saying it cannot be run as a copy, because a
  World that kept its declared id would put its data where the release's lives.

  **Products must ask, never hardcode.** `PRODUCT_WORLD` is private in both
  first-party Worlds; `contract::world_id()` and `contract::product_world()` are
  the only ways to the answer, and `replica::body::served_world` is where it is
  resolved. A site that reaches past them pins a World to one identity per build
  — which is one set of data per device — so the constant is private and the
  compiler is the check.
- **Its Space activates it on open.** A Space records the Worlds it has
  activated and the capabilities its founder holds, written when the Space was
  formed. A World added afterwards has neither, and its capabilities carry its
  own id, so every request would be denied with nothing to explain why. Opening
  a Space now activates and seeds anything it has not seen; both calls are
  idempotent, and a refusal costs that one World rather than the open.
- **It admits no historical runner.** A migrator carries a store forward from an
  implementation a Space once activated. A World named here has neither.
- **It is never given a release digest.** `LAIT_WORLD_RELEASE` says `local`, so
  the World's own process can tell.
- **Consent is to bytes.** `world.json` and every runner it declares are
  digested when you add it, and re-checked on every read; a tree that changed
  says so in its settings window.
- **An agent session on one carries no privileged tools.** No `member_add`,
  `member_remove`, `key_rotate`, `invite_ticket`, `connect`, `join_room` or
  `world_upgrade` — see `mcp::ShellTool`, where the classification is
  exhaustive and a test holds it complete.
- **Its text is fenced.** Teaching text, tool descriptions, schema descriptions
  and tool results are wrapped in a per-run random delimiter. That is an
  attack-cost increase and a provenance label, **not** a mitigation — measured,
  delimiting of this shape fails against adaptive attacks. What bounds the
  damage is the tool split above.

`LAIT_WORLD_LINK=<world-id>=<dir>` still exists for CI and a one-off: it serves
a directory in place of a release for the life of the process holding it, and is
written down nowhere. A recorded override was tried and removed — a Library
row's claim is the only thing this client says about what you are running, and
it is worth what it cannot be made to say falsely.

## Verifying

- Viewer: `cd viewer && npm run check && npm run test` (`tsc -b --noEmit`, then
  `vitest run`).
- Engine, in the order CI runs them (kill daemons first; they hold the `.exe`
  lock during the test build):

  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features --locked
  cargo build --workspace --locked --all-targets --all-features
  fixture_channels="$(mktemp -d)"
  bash ci/prepare-independent-world-fixtures.sh \
    "$fixture_channels" "$PWD/target/debug" "$PWD/target/debug/lait-feed"
  export WORLD_FIXTURE_CHANNELS="$fixture_channels"
  export WORLD_FIXTURE_INSTALLER="$PWD/target/debug/world-channel-installer"
  cargo nextest run --workspace --all-features --profile pr --no-fail-fast
  bash ci/third-party-notices.sh --check
  ```

  `--workspace` is load-bearing: a bare `cargo test` covers only the root
  package and silently skips every product and crate. **The build and signed
  fixture-publish steps are load-bearing too**: nextest builds test binaries,
  never the workspace bins or the independent World channels. The real-process
  suites install those signed archives through the production boundary. There
  is no discovery beside `lait` and no direct record synthesis. A stale bin or
  absent fixture channel fails those tests in ways that name everything except
  the missing prerequisite — a pre-#136 receiver read as a
  broken media pipeline for most of a day. Tiering lives in
  `.config/nextest.toml`; see [`docs/TESTING.md`](docs/TESTING.md).
- End to end against the real binary: `bash ci/smoke-p0.sh`. It starts the head
  and drives all three HTTP planes — the closest thing to "run the product".
- Two nodes on one machine: `bash ci/bench-two-node.sh`. Two scratch identities
  under temp config roots, `LAIT_NETWORK=isolated` (no relay, no discovery —
  the ticket carries direct addresses), found → invite → enter → membership
  converges, and it stops the daemons it started. Safe beside a live daemon: it
  sets `LAIT_DISPLAY=off` and takes ephemeral ports.

**Kill the running Astrolabe client before running its suite.** A running
client holds the single-instance mutex, so
`a_second_acquire_is_told_somebody_else_holds_it` fails with "the first launch
was refused" — which reads like the guard being broken and is actually the
guard working. The display coordinator's fixed port is the same shape:
`astrolabe::launch` needs 7443, and a live daemon holds it.

Pre-commit/pre-push hooks run `cargo fmt --all --check` — run `cargo fmt --all`
before committing Rust.
