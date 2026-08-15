# Handoff: start Astrolabe v1

You are picking up a planned, governed initiative. **The plan is not in this repo.** It lives
in the lait tracker as issued Specs and Issues. Read it there before writing code — this file
only tells you how to reach it and what will bite you.

---

## 1. Reach the plan

There is no CLI. Start a head and speak HTTP.

```sh
# Kill any running daemon first — a live lait.exe holds its own image and the link step fails.
taskkill //F //IM lait.exe

./target/release/lait.exe --json --port 0     # prints {url, token, port} before it accepts
```

Then POST JSON to, with `Authorization: Bearer <token>`:

| Route | Scope |
|---|---|
| `/api/host/rpc` | bootstrap: founding, entering, consent, orbit registry, MCP install |
| `/api/spaces/{orbit}/rpc` | Space authority: membership, devices, custody, diagnose, whoami |
| `/api/spaces/{orbit}/worlds/issues/rpc` | the tracker itself |

The orbit is `orb_61edfc37740cf3068605fde7dd0bfbf1323690f0c11b949827f84685f63e29dc`.
`GET /api/spaces` re-derives it if that ever changes.

### The wire `cmd` is the Rust variant name, not the MCP tool name

This wastes an hour if nobody tells you. `products/issues-app/src/protocol.rs` defines
`IssuesRequest` with `#[serde(tag = "cmd", rename_all = "snake_case")]`, so the wire name is the
**variant**. The MCP tool names are shorter aliases and **do not work over HTTP**.

| You want | Wire `cmd` | NOT |
|---|---|---|
| read an issue | `issue_view` | ~~`view`~~ |
| list rows | `list` | ~~`issue_list`~~ |
| link/unlink | `issue_link` / `issue_unlink` | ~~`link`~~ / ~~`unlink`~~ |

A wrong name returns `"invalid client operation"`. A wrong *argument* returns
`"invalid request"`. Those two errors mean different things; read them carefully.

### Start here

```json
{"cmd":"spec_list","project":"CLIENT"}        → the issued Astrolabe Plan
{"cmd":"spec_show","spec":"spc_01jvkgse8jmqe0o5v7fse4vb54"}
{"cmd":"geometry","project":"CLIENT"}          → the compiled morphology; the order to work in
{"cmd":"packet","reff":"CLIENT-2"}             → what governs any one issue
```

---

## 2. Governing truth

| | |
|---|---|
| Plan Spec | `spc_01jvkgse8jmqe0o5v7fse4vb54` — **issued `c21ec49a`**, *"Astrolabe v1: the local client for served Worlds"* |
| Baseline | **issued `30bef3ea`**, *"Astrolabe v1"* — bound to all 34 issues |
| Substrate Plan | `spc_01jvreu2lr5ubuupb6h8fnke9r` — **issued `7eca8ffa`**, Baseline `2379ea40`, 5 issues |

Revisions 1–4 of the Plan describe a Tauri shell and then a Flutter shell. **Both are dead.**
Only `c21ec49a` is current. If you find yourself reading about `flutter_rust_bridge`, you are in
a superseded revision — check `spec_show`'s `issued` field, not `heads` of an old history entry.

The Plan lands in each Packet's **`guidance`** bucket, not `governing`. That is correct: a `plan`
names work order; it does not state enforceable outcomes. There is no `requirement` or `design`
Spec yet. If you want enforcing governing truth for the licence closure, the accessibility path
or the signal-ordering guarantee, **write one** — that is a real gap, not an oversight to ignore.

---

## 3. What Astrolabe is

The local Windows application through which a person reaches the Worlds their device serves.
Reference shape is the **Steam client**: a library, a launcher, an identity, a social client —
that never draws the game.

- **It never renders a World.** `products/issues` ships its own head. `Open` is a handoff to the
  person's browser carrying a single-use, Orbit-scoped, expiring `LaunchTicket`.
- **It is entirely Rust.** No Flutter, no Dart, no FFI, no local HTTP boundary between the
  interface and the supervisor. The interface calls the supervisor library directly on native
  types. One App-owned Rust entity consumes the ordered `ClientSignal` stream — it is the only
  model of client state, and nothing mirrors it.
- **The UI substrate is *derived*, not adopted.** Lait owns a UI layer selectively derived from
  the MIT-licensed `warpui` / `warpui_core`, then stripped. It is not a general-purpose toolkit,
  it does not track Warp, and it carries **no permanent git dependency on the Warp monorepo**.

### The licence rules are gates, not guidance

Lait stays `MIT OR Apache-2.0`. Import only MIT-granted code. Preserve every notice. Record the
source commit and provenance of each imported file **at import time**. Depend on none of Warp's
AGPL crates (`ui_components`, `warpui_extras`, the application crates, `warp_errors`,
`markdown_parser`, `sum_tree`, `command`, `warp_util`). A dependency/licence audit runs in CI and
**fails the build**. See CLIENT-31.

---

## 4. Where to start

The geometry computes this; do not re-derive it by reading bodies. Eight issues are `ready`
(nothing blocks them). Ranked by what actually unlocks the most:

| | | |
|---|---|---|
| **CLIENT-2** | urgent | **Do this first.** Recover the workbench into a UI-neutral supervisor library. |
| **CLIENT-31** | urgent | Derive the UI substrate from WarpUI's MIT core, with the provenance record. |
| CLIENT-15 | urgent | Found, enter, consent from the client. |
| CLIENT-21 | high | Stage the binary; kill the `.exe`-lock tax. |
| CLIENT-17 | medium | Serve the overlay from the head. |
| CLIENT-19 | medium | World callback channel — milestoned **v-next**, outside the v1 gate. |

CLIENT-25 and CLIENT-26 are epics (containment roots), not work.

### CLIENT-2 has a trap in it

`tools/workbench` **does not exist on `main`.** It is one unmerged commit, `d42e11a` on branch
`feat/workbench`, well behind. Every issue that says "preserve the tested supervisor" is talking
about code no branch under development can see. Step one is recover and rebase it.

That commit also shipped a **React** workbench UI at `viewer/src/workbench/` and
`viewer/workbench.html`. The Rust-only design orphans it. Delete it in the same change.

Likewise `spike/bridge` and branch `spike/dart-rust-bridge` measured the Dart/Rust boundary.
That decision is reversed; close them out rather than leaving a third unmerged limb.

---

## 5. The gate, and what it actually depends on

`CLIENT-13` is the acceptance gate, blocked by the three surface epics (Reach, Custody,
Operations). `CLIENT-1` is the terminal.

**CLIENT-13 cannot pass on this plan alone.** Three of its release criteria depend on Substrate
work Astrolabe does not own:

| Blocker | Why the gate needs it |
|---|---|
| **SUB-2** | No World declares a name, icon or entry path; no read answers which Worlds an Orbit serves. The Library — the front page — has nothing to draw. |
| **SUB-1** | A Space's display name is owned by a World, so the row label is a stale cache. |
| **SUB-5** | No engine read answers footprint, object count or last verification. Storage has no authoritative source. |
| **SUB-4** | Address book model + daemon service; the client is only the A2 head. |

Those are real `blocks` edges in the catalog. **They do not appear in the Plan's drawing** —
`geometry::compile` filters to one project, so a cross-project edge is skipped. Read the
morphology as Astrolabe's internal order, never as its critical path. The Substrate issues are
independent of each other; they share a consumer, not an order.

---

## 6. Invariants you may not quietly relax

These are stated in the Spec and tested by the gate.

- **Listing is passive.** A Library that mounts every Station to draw itself is a defect.
  Placement is what `Open` causes, not what listing costs. Same rule for storage and contacts.
- **An observation error is not an empty result.** Failed sampling preserves the last good
  topology and marks it stale. Rendering a sampling failure as "no peers" is a defect the gate
  tests for directly.
- **Never synthesise a figure.** A number nobody measured is reported absent or stale. Same
  defect class as the above, wearing different clothes.
- **The overlay carries convenience, never authority.** It lives in a World's DOM, so a World can
  imitate it. Anything that grants, admits, approves or spends raises the native window.
- **Ownership is the safety boundary.** Only processes spawned in the current run are owned.
  Discovered daemons are external and can never be force-killed, including after a crash.
- **Removal and data deletion are separate.** Deletion needs all three of: stopped device,
  canonical containment under the managed root, explicit confirmation.
- **Signals stay ordered** across reconnect and across a supervised process restart. One stream,
  established before the first snapshot.

---

## 7. Environment traps that cost real time

**Use the release binary, and rebuild it after pulling.** Two independent failures stack here:

1. `target/debug/lait.exe` derives a *different device key* than `config/secret.key` produces
   (`c654…` vs `c3ab…`), so the debug build is **not an enrolled member** of the Space —
   `whoami` returns `member: false, can_write: false`. Reads work; every write is refused.
   Root cause not yet diagnosed. Worth chasing; it is probably a real bug.
2. A release binary older than the store's World implementation is refused too, with the
   misleading message `"invalid request"` on every write. `{"cmd":"diagnose"}` on the Space plane
   names it exactly: *"this build is v2 (…), the space runs v2 (…) — same version, different
   descriptor"*. `cargo build --release` fixes it.

Before concluding anything is a permissions or capability defect, run `diagnose` and check the
`implementation` gate. I lost time blaming a capability-scope mismatch that did not exist.

**Validate Typst without writing.** Issue bodies are Typst (`// lait-document:1` prefix; without
it your text is escaped as literal characters). `issue_new`/`issue_edit` do **not** compile-check.
`spec_revise` does, *before* it submits — so a deliberately stale `expected` is a free validator:

```json
{"cmd":"spec_revise","spec":"<any spec>","expected":"000…0","text":"<candidate>"}
```
`"document could not be saved"` → invalid Typst. `"that change conflicts…"` → valid, nothing written.

Available beyond standard Typst markup: `#lait-callout(tone)[…]`, `#lait-task(checked)[…]`,
`#lait-table(header: (…), rows: (…))`.

**Rebuilding the viewer** takes two steps — `cd viewer && npm run build` then `cargo build` —
because `src/serve/shell.rs` embeds `src/serve/assets/` via `include_dir!` at compile time.

**CLIENT-1 refuses new comments** (`Conflict`). Its thread predates the `tree:comments` cutover.
Put notes in its body. Other issues comment fine.

---

## 8. Verify

```sh
cd viewer && npm run check && npm run test     # 566 tests
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked
cargo nextest run --workspace --all-features --profile pr --no-fail-fast
bash ci/smoke-p0.sh                            # drives the real binary end to end
```

`--workspace` is load-bearing: a bare `cargo test` silently skips every product and crate.

---

## 9. Working the tracker

You are a member with write standing. File and move work as you go; the Plan reads current
morphology, so a status or relation change moves the open loci **without revising the document**.

- Structure: `issue_parent` for containment, `issue_link {kind:"blocks"}` for constraints,
  milestones and labels for facets. Do not encode order in prose — the geometry compiles it, and
  the transitive reduction will tell you which edges add no information.
- Found something the plan asserts that the code contradicts? File a `spec_observe` note rather
  than editing governing text. It carries your identity, never governs, and is retractable.
- Do **not** rewrite issued Spec revisions. Draft a successor.
