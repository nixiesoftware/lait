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

`tools/astrolabe` is a second program, `astrolabe.exe`: the local client through
which a person reaches the Worlds their device serves. Reference shape is the
Steam client — a library, a launcher, an identity — that **never draws a World**.
`Open` is a handoff to the person's browser; `products/issues` ships its own head
and stays the authority on its own presentation.

```sh
cargo run -p astrolabe          # the client (needs lait.exe beside it when packaged)
cargo test -p astrolabe         # unit + interaction + packaging + presentation + launch
```

Three layers, and **no boundary between them** — no FFI, no local HTTP hop, no
generated binding, no serialization on the path from what is observed to what is
drawn:

- `client/` is the reach: the supervisor library it embeds (`tools/workbench`)
  and the host, Space and World planes it speaks. It draws nothing, which is why
  its rules are testable without a window.
- `model.rs` is the App-owned state — the *only* model of client state. It moves
  in exactly two ways, snapshot and invalidation. There is no optimistic local
  mutation, because that would be a second model disagreeing with the first
  exactly when an action was refused.
- `ui/` draws it and holds no logic.

`runtime.rs` is the one channel: supervision and sampling run on a Tokio runtime
on their own thread and reach the frame loop as `Update`s drained at the top of
each frame.

### Drawing returns actions; it never calls anything

```rust
pub fn draw(ui: &mut Ui, app: &App, chrome: &mut Chrome) -> Vec<Action>
```

A surface that called the client would do network work on the frame thread. One
that called it and *kept the answer* would be a second model of client state,
disagreeing with the first in exactly the case that matters — when the action was
refused. The shell dispatches what comes back and records it as in flight on the
same frame, so a control is disabled the moment it is clicked.

That is also what makes a click assertable: an interaction test presses a real
control and reads what the surface asked for, with no daemon, no browser and no
window. Seven surfaces — Library · Spaces · Members · Devices · Heads · Storage ·
Diagnostics — reachable by `Ctrl+1`–`Ctrl+7`, with `F5` to re-read.

### The UI substrate is adopted, not built

`egui` + `eframe` + `accesskit`, all `MIT OR Apache-2.0`, as ordinary pinned
dependencies. The Plan's revision 5 called for deriving one from "WarpUI"; that
is GPUI with the names changed, it is Apache-2.0 with **no MIT grant**, its
platform crates are unpublished, and it has **no headless rendering on Windows**
— which the release gate requires. Revision 6 (issued `89b00354`) reverses it;
the finding is a `clarifies` observation on the Plan Spec.

Interaction tests use `egui_kittest`, which renders offscreen and queries the
**AccessKit tree** — so they assert what a screen reader reads, not what pixels
looked like. Four gotchas found the hard way:

- A plain label's text lands in the node's `value`, not its `label`.
- A selected tab is announced through the Toggle pattern: assert `toggled()`,
  not `is_selected()`.
- A surface heading carries the same text as its tab, so disambiguate with
  `get_by_role_and_label`.
- **A text field's contents are a `value` on a node with no label**, so a scan
  over `query_all_by_label_contains("")` cannot see them. Assert on the draft the
  box is bound to.

Build the harness **bigger than the surface draws**
(`Harness::builder().with_size([1_200.0, 2_000.0])`). egui culls interaction
outside the clip rect, so a click on a control that fell off a too-small virtual
screen registers as nothing at all — which reads exactly like the control being
broken.

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

**Listing is passive; choosing is the act.** The Library and the Space list place
nothing. `Open` places, and so does selecting a Space to administer.

**Every accent clears 3∶1 against what is behind it** (`ui::theme::legible`).
egui's light theme answers `warn_fg_color` at 2.79∶1 — asking the visuals is not
the same as getting a readable answer.

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

## The coverage manifest cannot be regenerated on Windows

`ci/coverage-manifest.txt` records every test id. Adding or renaming a test fails
the `coverage manifest` CI job until it is refreshed, and
`bash ci/coverage-manifest.sh --update` **refuses to run on Windows** by design —
`cfg(unix)` tests do not exist here and regenerating would record their absence
as coverage loss. Push, let the job fail, and take the file it uploads:

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
  bash ci/third-party-notices.sh --check
  ```

  `--workspace` is load-bearing: a bare `cargo test` covers only the root
  package and silently skips every product and crate. Tiering lives in
  `.config/nextest.toml`; see [`docs/TESTING.md`](docs/TESTING.md).
- End to end against the real binary: `bash ci/smoke-p0.sh`. It starts the head
  and drives all three HTTP planes — the closest thing to "run the product".

Pre-commit/pre-push hooks run `cargo fmt --all --check` — run `cargo fmt --all`
before committing Rust.
