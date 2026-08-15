# Handoff: finish Astrolabe v1

You are continuing an initiative that is **half landed and green**. The plan is not
in this repo — it lives in the lait tracker as issued Specs and Issues. This file
tells you how to reach it, what is already done, and what will cost you an hour if
nobody warns you.

`HANDOFF-ASTROLABE.md` (the first one) is **superseded**. It describes a donor-UI
plan that no longer exists and work that is now merged into the branch. Read this
file instead; keep that one only for its environment notes, which still hold.

---

## 0. Where things stand

**Branch `feat/astrolabe-supervisor`, PR #114, head `bbcd7d8`, all checks green.**
Nine commits, 59 files, ~13,900 lines. Continue on top of it — do not start a new
branch, and do not rebase what is already pushed.

```
1533 workspace tests · clippy clean · ci/smoke-p0.sh green · astrolabe.exe runs
```

11 of 34 CLIENT issues closed, plus SUB-2 and SUB-5. The plan's own geometry is the
authority on what is left; §4 below is a snapshot of it.

---

## 1. Reach the plan

There is no CLI. Start a head and speak HTTP.

```sh
taskkill //F //IM lait.exe          # a live lait.exe holds its image; the link step fails
cargo build --release               # ALWAYS rebuild after pulling — see §6
./target/release/lait.exe --json --port 0
```

One line comes back before the listener accepts: `{url, token, port}`. `Bearer
<token>` on every `/api` route.

| Route | Scope |
|---|---|
| `/api/host/rpc` | founding, entering, consent, orbit registry, MCP install |
| `/api/spaces/{orbit}/rpc` | membership, devices, custody, storage, diagnose, whoami |
| `/api/spaces/{orbit}/worlds/issues/rpc` | the tracker itself |

Orbit: `orb_61edfc37740cf3068605fde7dd0bfbf1323690f0c11b949827f84685f63e29dc`.

**The wire `cmd` is the Rust variant name, not the MCP tool name.**
`products/issues-app/src/protocol.rs` tags on the variant: `issue_view` not `view`,
`list` not `issue_list`, `issue_link` not `link`. A wrong name gives `"invalid client
operation"`; a wrong *argument* gives `"invalid request"`. Those mean different
things.

```json
{"cmd":"spec_show","spec":"spc_01jvkgse8jmqe0o5v7fse4vb54"}
{"cmd":"geometry","project":"CLIENT"}     ← the order to work in; do not re-derive it
{"cmd":"packet","reff":"CLIENT-7"}        ← what governs one issue
{"cmd":"issue_view","reff":"CLIENT-13"}   ← read its comments; the gate audit is there
```

---

## 2. Governing truth — **revision 6, not 5**

| | |
|---|---|
| Plan Spec | `spc_01jvkgse8jmqe0o5v7fse4vb54` — **issued `89b00354`**, *"Astrolabe v1"* |
| Baseline | `bas_01jvkgsekvk8rdoaapfs9loacm`, rev `30bef3ea`, bound to all 34 issues |
| Substrate Plan | `spc_01jvreu2lr5ubuupb6h8fnke9r` — issued `7eca8ffa`, 5 issues |

Revisions 1–4 describe a Tauri shell then a Flutter shell. **Revision 5 describes a
UI substrate derived from "WarpUI". All three are dead.** If you find yourself
reading about `flutter_rust_bridge` or `warpui`, check `spec_show`'s `issued` field.

### Why revision 5 was reversed — do not undo this

`warpui` is Zed's GPUI with the names changed, and four of the five facts revision 5
rested on are wrong:

- It is **Apache-2.0**. There is no MIT grant, so "import only code covered by
  WarpUI's MIT grant" names a licence that does not exist.
- The AGPL crates it forbade were **relicensed to GPL-3.0** in May 2026.
- Its platform crates are **published nowhere**, so "carry no permanent git
  dependency" and "use this donor" cannot both hold.
- It has **no headless rendering on Windows** — its own visual-test module says real
  rendering is macOS-only — which CLIENT-34 requires and CLIENT-13 lists.

The finding is a `clarifies` observation on the Spec (`spec_observations`). Revision
6 replaces the donor with **adopted `egui` + `eframe` + `accesskit`**, all
`MIT OR Apache-2.0`, so lait's own dual offer survives. The whole closure passes the
repo's existing permissive-only `deny.toml`.

**One constraint is framework-independent and still bites:** `accesskit_windows`
implements ten UIA control patterns and **does not implement Table, TableItem, Grid
or GridItem**. Any board- or table-shaped surface needs list or tree semantics, or an
upstream contribution. Discovering that after building a grid is the expensive order.

---

## 3. What exists now

```
tools/workbench/   the UI-neutral supervisor library (no windowing, no rendering)
tools/astrolabe/   the client: client/ (reach) · model.rs (state) · ui/ (surfaces)
packaging/windows/ the hand-authored NSIS installer
```

**No boundary inside Astrolabe** — no FFI, no local HTTP hop, no generated binding,
no serialization between what is observed and what is drawn. `runtime.rs` is the one
channel: supervision runs on its own Tokio thread and reaches the frame loop as
`Update`s drained at the top of each frame.

Rules that are **tested, not documented**. Do not quietly relax any of them:

- Removal and data deletion are separate; deletion re-proves containment under the
  managed root *at deletion time*, not at registration.
- A sampling failure **degrades and preserves the last good topology**. It never
  reads as "no peers". Staleness dates from the *first* failure, not the latest.
- **Unmeasured is absent, never zero.** A transfer with an unknown total draws no
  progress bar at all — empty and full are both claims about a proportion that does
  not exist.
- Ownership is a boundary: `force_kill_and_wait` lives on the owned child handle,
  `stop_head` takes an id and a handle, and there is **no pid-based path** to either.
  An unprovable identity is a refusal.
- The overlay renders convenience and **refuses authority**; anything that grants,
  admits, approves or spends raises the native window.
- Signals stay ordered across reconnect and across a supervised restart, on **one**
  stream. `Supervisor::start` returns the stream *with* the supervisor so
  "established before the first snapshot" is structural.
- A launch credential is single-use, Orbit-scoped and 30 seconds. Redemption
  consumes; replay 401s.

---

## 4. What is left, in the order the geometry gives

**Ready now** (nothing blocks them):

| | | |
|---|---|---|
| **CLIENT-5** | urgent | Wire the supervisor into the app host. Mostly done — `client/` + `runtime.rs` exist. Finish and close. |
| **CLIENT-15** | urgent | Found/enter/consent. The *calls* exist in `client/host.rs`; **no UI flow drives them**. This is the Welcome surface. |
| **CLIENT-20** | high | Heads. Browser heads start/stop/list. The **MCP binding half** (choosing orbit/identity/binary, `HostInstallMcp`) is not built. |
| **CLIENT-33** | high | Focus and UIA. Tabs announce through the Toggle pattern; a **real screen reader has never been driven**. |
| CLIENT-25/26 | urgent | Epics. Close when their children close. |
| CLIENT-19 | medium | v-next, outside the v1 gate. Leave it. |

**Then, in dependency order:** CLIENT-7 (shell + App model — the pivot; six issues
wait on it) → CLIENT-14, 16, 18, 22 → CLIENT-8, 9, 10 → CLIENT-27 → 28/29/30 →
**CLIENT-13** (the gate) → CLIENT-1.

**Substrate still open:** SUB-1 (a Space's name is owned by a World — the Library's
row label is a stale cache), SUB-3 (transfer control; the progress lane has no
producer), SUB-4 (address book model + daemon service, which CLIENT-22 needs).
SUB-2 and SUB-5 are **done**.

### The five things CLIENT-13 cannot pass without

Read the gate audit comment on **CLIENT-13** — every release criterion is marked
*proven* / *implemented* / *partly* / *not done* with the test name. The genuinely
missing ones:

1. **`Open` does not reach a World.** The Library lists rows and CLIENT-20 can start
   a head; nothing wires a row to a running one. The pieces exist —
   `POST /api/launch` mints, `Client::launch_url` composes, `start_head` spawns —
   the glue does not.
2. **No tray icon.** The exit *policy* is a tested pure function; the thing it
   minimises to is not built, so CLIENT-24 has no home.
3. **No address book.** SUB-4 first.
4. **No third-party notice generation.** One CI step revision 6 specifies and nobody
   wrote.
5. **No clean-machine install.** Which is what the gate *is*, and the one item
   nothing in CI substitutes for.

---

## 5. Traps that already cost time

**The coverage manifest cannot be regenerated on Windows.** `ci/coverage-manifest.sh
--update` refuses by design — `cfg(unix)` tests do not exist here and regenerating
records their absence as coverage loss. Adding or renaming *any* test fails the
`coverage manifest` job until refreshed. The loop:

```sh
git push
# let the job fail, then take the file it uploaded
gh run download <run-id> -n coverage-manifest -D <dir>   # → coverage-manifest.txt.actual
cp <dir>/coverage-manifest.txt.actual ci/coverage-manifest.txt
git commit && git push
```

CI is *meant* to auto-push this; it has never done so. **Budget one extra push per
batch of test changes, and batch them.**

**There is a fourth request-classification site, and it fails at runtime.** Adding a
`Request` variant fails compilation in three places by design — `control::classify`,
`serve::policy::is_read`, `serve::policy::is_host_plane`. But `handle` dispatches by
terminal owner into four `dispatch_*` functions, each with an `other =>` catch-all. A
variant classified `Observation` but arm-matched in `dispatch_mechanics` **compiles
clean** and answers *"request is not an observation operation"* against a live head.
Drive every new verb end to end.

**`egui_kittest` assertions, three ways to get them wrong:**
- A plain label's text is in the node's **`value`**, not its `label`.
- A selected tab is announced through **`toggled()`**, not `is_selected()`.
- A surface heading carries the same text as its tab — use `get_by_role_and_label`,
  or `get_by_label` panics on two matches.

**Never `git add -A` while a sub-agent is editing.** It happened twice here. Once it
swept a finished agent's work into an unrelated commit (messy history, nothing lost);
once it committed a half-finished refactor and had to be reset. **Stage explicit
paths.** Same for `cargo fmt --all`.

**`release dry-run (dist plan)` flakes.** It failed three times at `Install dist`
downloading cargo-dist from GitHub (`curl: (56)`). It is not your diff.
`gh run rerun <id> --failed`.

**Two `Set-Cookie` headers in an array replace rather than append.** Build a
`HeaderMap` and `append`.

**`clippy::arithmetic_side_effects` is denied** in `tools/workbench`,
`crates/world-interface` and others. `Instant::now() + d` is an error; use
`checked_add`. `expect`/`unwrap` outside tests likewise.

**Windows spawn needs `CREATE_NO_WINDOW`.** Without it a console-subsystem child gets
a console — freshly allocated when the parent has none (a black window on screen),
inherited when it has one (the daemon joins that console's process group and takes
its Ctrl-C). Set on both `daemon_spawn` and `heads`.

**`src/serve/mod.rs:open_browser` uses `cmd /C start`,** which flashes a console from
a parent with no console. It is now only reached by `lait --open`; if Astrolabe ever
calls it, prefer `ShellExecuteW`. Noted on CLIENT-14.

**Validate Typst without writing.** Issue bodies are Typst (`// lait-document:1`
prefix — without it your text is escaped literally). `issue_new`/`issue_edit` do not
compile-check; `spec_revise` does, *before* it submits, so a deliberately stale
`expected` is a free validator:

```json
{"cmd":"spec_revise","spec":"<any>","expected":"000…0","text":"<candidate>"}
```
`"document could not be saved"` → invalid. `"that change conflicts…"` → valid,
nothing written. Beyond standard Typst: `#lait-callout(tone)[…]`,
`#lait-task(checked)[…]`, `#lait-table(header: (…), rows: (…))`.

**CLIENT-1 refuses new comments** (`Conflict`) — its thread predates the
`tree:comments` cutover. Put notes in its body.

---

## 6. Environment

**Use the release binary and rebuild it after pulling.** `target/debug/lait.exe`
derives a *different device key* than `config/secret.key` (`c654…` vs `c3ab…`), so a
debug build is **not an enrolled member**: reads work, every write is refused. Root
cause still undiagnosed — worth chasing, probably a real bug. A stale release binary
is refused too, with the misleading `"invalid request"` on writes. Before blaming
permissions, run `{"cmd":"diagnose"}` on the Space plane and read the
`implementation` gate: *"this build is v2 (…), the space runs v2 (…) — same version,
different descriptor"* means rebuild.

Kill daemons before any build — a live `lait.exe` holds its image and the link step
fails.

---

## 7. Verify

```sh
cd viewer && npm run check && npm run test     # 566 tests
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked
cargo nextest run --workspace --all-features --profile pr --no-fail-fast   # 1533
bash ci/smoke-p0.sh                            # drives the real binary end to end
cargo test -p astrolabe                        # unit + headless interaction + packaging
```

`--workspace` is load-bearing: a bare `cargo test` silently skips every product and
crate.

**Prove the client by hand** — this is the fastest way to see the whole design work:

```sh
cargo build -p astrolabe && cp target/debug/lait.exe target/debug/
./target/debug/astrolabe.exe        # Library · Devices · Storage · Diagnostics
```

And the trust line, against a running head:

```
GET  /                    → no overlay        (no client, no overlay)
POST /api/launch          → a ticket
GET  /?ticket=…           → 303 + session cookie + client marker
GET  /?ticket=…  again    → 401                (replay fails closed)
GET  /  with the marker   → 1 overlay, 4 raise-links, 2 facts, 0 scripts
GET  /app.js  either way  → byte-identical     (assets are never composed)
```

---

## 8. Working the tracker

You are a member with write standing. File and move work as you go — the Plan reads
current morphology, so a status or relation change moves the open loci **without
revising the document**.

- Structure with `issue_parent` (containment) and `issue_link {kind:"blocks"}`
  (constraints). Do not encode order in prose; the geometry compiles it.
- **`geometry::compile` filters to one project**, so the SUB→CLIENT `blocks` edges are
  real in the catalog and **absent from the Plan's drawing**. Read the morphology as
  Astrolabe's internal order, never as its critical path.
- Found something the plan asserts that the code contradicts? File a `spec_observe`
  note rather than editing governing text. It carries your identity, never governs,
  and is retractable. That is how revision 5's donor error was handled, and the
  precedent is worth following.
- **Do not rewrite issued Spec revisions. Draft a successor** — `spec_revise`, then
  `spec_state` to `issued`.
