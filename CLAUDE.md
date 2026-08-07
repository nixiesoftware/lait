# Working on lait (agent notes)

lait is a local-first, peer-to-peer issue tracker: a Rust engine (`src/`, `crates/`)
and a React viewer (`viewer/`) that the binary hosts over the engine on loopback.

**There is no command surface.** `lait` is a launcher with exactly three modes:

```sh
lait daemon [--home <dir>]                        # the identity-scoped host
lait mcp                                          # the stdio head for an agent
lait [--json] [--port N] [--orbit SEL] [--open]   # the local app + the daemon under it
lait --version                                    # which build this is
```

Anything else exits 1. Everything a verb used to do is now a request one of those
three carries — see [`docs/SERVE.md`](docs/SERVE.md) for the three HTTP planes.

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
- `{ project }` / `{ issue }` — select a project (KEY) or issue (ref)
- `{ milestone }` — scope the issue surfaces to a `mls_` id; `""` is the
  No-milestone bucket and `null` clears the scope. Applied *after* `project`, so
  `{ project: "ENG", milestone: "mls_x" }` in one detail scopes ENG. Stays on the
  current view if it draws rows (list/board/calendar), else lands on Issues.
- `{ tab }` — Settings sub-page: `general | members | devices | labels | workflow | access`
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
loaded behind `import.meta.env.DEV`, absent from the embedded bundle):

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

## Rebuilding after a viewer change

`src/serve/shell.rs` embeds `src/serve/assets/` via `include_dir!` at **compile
time**, so a viewer edit is only visible through the running head after both steps:

```sh
(cd viewer && npm run build)   # regenerates src/serve/assets/*
cargo build                    # re-embeds the fresh bundle
```

`cd viewer && npm run dev` does both plus a live head, which is what you want
while iterating; it shells out to `lait --orbit <sel> --port <n> --json` and
reads that readiness line.

Kill running daemons first — a running `lait` binary holds the `.exe` lock and the
link step fails (`taskkill //F //IM lait.exe` on Windows).

## Verifying

- Viewer: `cd viewer && npm run check && npm run test` (`tsc -b --noEmit`, then
  `vitest run`).
- Engine, in the order CI runs them (kill daemons first; they hold the `.exe`
  lock during the test build):

  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features --locked
  cargo nextest run --workspace --all-features --profile pr --no-fail-fast
  ```

  `--workspace` is load-bearing: a bare `cargo test` covers only the root
  package and silently skips every product and crate. Tiering lives in
  `.config/nextest.toml`; see [`docs/TESTING.md`](docs/TESTING.md).
- End to end against the real binary: `bash ci/smoke-p0.sh`. It starts the head
  and drives all three HTTP planes — the closest thing to "run the product".

Pre-commit/pre-push hooks run `cargo fmt --all --check` — run `cargo fmt --all`
before committing Rust.
