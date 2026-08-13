# Handoff: Astrolabe

`HANDOFF-ASTROLABE.md` and `HANDOFF-ASTROLABE-2.md` are **superseded**. Keep them
only for environment notes, which still hold.

**You are picking up a page-by-page interface pass.** The visual language landed;
the pages have not been rebuilt on it yet. Start at the Library — it is the front
page and it carries a real defect.

After the interface, the **address book** is the client's core deliverable and
the largest thing unbuilt. It is fully specified in the tracker (§6).

---

## 0. Where things stand

**Branch `feat/astrolabe-supervisor`, PR #114.** Continue on top of it — do not
start a new branch, and do not rebase what is pushed.

```
1613 workspace tests (Linux) · clippy clean · every CI job green
astrolabe: 82 unit · 38 interaction · 8 packaging · 4 presentation · 1 launch · 1 render
```

24 of 34 CLIENT issues closed, plus SUB-2 and SUB-5.

The client runs. Build and launch it with:

```sh
taskkill //F //IM astrolabe.exe        # it holds the single-instance mutex
cargo build --release -p astrolabe
./target/release/lait.exe daemon &     # Astrolabe attaches; it never starts this
./target/release/astrolabe.exe
```

---

## 1. What to do first

### 1.1 The Library cannot open anything, and the tooltip blames the wrong thing

On a freshly started daemon **every row is unopenable**. `client/library.rs`:
when an Orbit is vacant, `WorldsActive` answers nothing, `activated.is_empty()`,
and the row is built with `entry_path: None`. `ui/library.rs` then does
`let openable = entry_path.is_some()` and the disabled hover says *"This World
declares no entry path yet (SUB-2)"* — but `products/issues-app` **does** declare
one (`with_display("Issues", Some("📋"), Some("/"))`) and SUB-2 shipped.

The comment two lines above the check already says what should happen:

> `// A *vacant* Orbit is still openable: opening is precisely what places one.`

**The fix is a distinction the code does not make.** A *World* row opens at the
entry path that World declared — never guess that. A *Space* row opens at the
**head's root for that Orbit**, which is not a guess about any World: it is the
Orbit's own front door, what `lait --orbit <sel>` serves, and the head shows
whatever is actually activated once placement happens. Listing stays passive;
the click is what places.

Fix it as part of the Library page or on its own — the last question put to the
repo owner was exactly that and went unanswered, so pick one and say which.

### 1.2 Then the pages, in this order

**Library · Members · Devices · Heads · Spaces · Diagnostics · Storage.**

Each page needs the same four things, and none of them needs new product surface:

1. **Rows on the ladder.** Today a row is three or four things jammed on one line
   at whatever width they happen to take. A row is `control::lg()` or
   `control::xl()` tall with aligned columns.
2. **Prose on `theme::prose`, not `theme::secondary`.** The explanatory sentences
   under each heading are still on the label floor at `.small()` and are the
   least readable thing in the client. `prose` exists and is used in exactly one
   place so far.
3. **Cut the essays.** Look at the Heads surface: three lines of grey explaining
   what an MCP binding pins. Warp and Linear do not explain themselves on the
   page. One line, the rest on hover.
4. **Headings on a rhythm.** `gap::section()` between sections, not a bare
   `ui.separator()` doing all the work.

---

## 2. The visual language

`tools/astrolabe/src/ui/geometry.rs`. Ported from the tracker's own system in
`viewer/src/styles.css` rather than invented — two design systems in one product
is one more than anybody can hold.

**The question is never "how many pixels". It is which rung.**

| axis | rungs | scales? |
|---|---|---|
| `control::` | 20 · 24 · **28** · 32 · 40 | yes |
| `bar::` | 32 · 36 · **44** | yes, but named by what they hold |
| `gap::` | tight 4 · stack 6 · row 8 · section 20 | — |
| `text::` | 10 · 11 · 12 · **13** · 15 | pinned |
| `radius::` | **by role**: mark · row · control · surface | — |
| `nav::` | header only: padding 1.4× body, gap 4 | — |

`geometry::apply` is the **only** function that turns a rung into an egui field,
and `ui::install(ctx)` is the only caller. Retuning is one edit. A raw number at
a call site is a decision nobody can find again.

Two rules worth keeping:

- **Roundness is chosen by what a thing *is*.** The rungs are private; you cannot
  reach for an 8. A nav pill takes `radius::row()` because the tracker's own
  system documents that role as "the sidebar's nav items, the settings tabs".
- **Colour is asked of the visuals and then held to a floor.** `theme::secondary`
  clears 3∶1 (a short label), `theme::prose` clears 4.5∶1 (a sentence).
  `theme::raised` and `theme::hairline` derive the header's step and edge from
  the page, so a light scheme sinks where a dark one lifts.

### The header is done

`ui/header.rs`, proportioned against the Steam redesign the Spec names as this
client's reference shape. Composition and ratios were taken; pixels were not —
that design runs at a 16px body and this client runs at 13.

Two things from the reference are **deliberately absent**, and both should stay
absent until the reason changes:

- **No search field.** Nothing to search yet. A box that looks like a control and
  does nothing is worse than the space it fills.
- **No account chip.** Nothing honest to put in one. `HostContext` answers an
  identity *home*, which is a path; a person's name lives in a Space's `whoami`,
  which is per-Space and unread until one is chosen. The last segment of a config
  directory is not an identity. If you want that cluster, it needs a real global
  "who am I", which is a small engine ask.

---

## 3. How to see what you are doing

### Renders — use these

```sh
cargo test -p astrolabe --test surfaces      # → target/surfaces/*.png
```

Every surface, both themes, plus the empty machine and the tallest state,
rendered offscreen through wgpu. **This is the loop**: change, render, look, fix.
It asserts two things no semantic test can — a surface that rendered one flat
colour laid nothing out, and a page whose background came from one theme while
its text came from the other.

One `#[test]` on purpose: `nextest` gives each test its own process, but plain
`cargo test` uses threads, and two wgpu devices coming up together there is an
access violation, not a failure.

Once the language stops moving, swap `render` for kittest's `snapshot` and this
becomes the visual regression gate.

### The live window — and a warning

`scratchpad/capture-screen.ps1` raises the window and grabs the screen. There is
also a `capture.ps1` that tries `PrintWindow(PW_RENDERFULLCONTENT)` first because
it does not steal focus.

**`PrintWindow` lied.** On this wgpu surface it returned *part* of a frame — the
header painted, the page black — and the picture looked plausible enough to
report as fine. It was not. When the answer matters, use the screen grab; you can
tell it is honest because other windows bleed in at the edges.

### The harness is not the shell

The render harness paints its own central panel. A client that paints a
background and one that does not are **identical** from inside it. That is how
the client shipped for weeks rendering dark text on near-black on any
light-themed Windows: `eframe::App::ui` hands over a `Ui` with no background —
its own docs say so at `epi.rs:168` — and nothing wrapped it. Now fixed in
`ui::draw` plus `clear_color`.

The lesson generalises: **the one defect class the harness cannot see is whatever
the shell was supposed to provide.** Launch the real thing before believing a
render.

---

## 4. The design reference

Figma, `Steam Redesign (Community)`, file key `1aRdxESsqfgwQfRU2R6Wmh`.

A personal access token is at `scratchpad/.figma-token` (the repo owner said they
would rotate it within days — if it 403s, ask for a new one rather than working
around it).

```sh
T=$(cat .../.figma-token); KEY=1aRdxESsqfgwQfRU2R6Wmh
# render any node
curl -s -H "X-Figma-Token: $T" "https://api.figma.com/v1/images/$KEY?ids=426:4075&format=png&scale=3"
# and its real numbers — padding, itemSpacing, cornerRadius, fontSize, fills
curl -s -H "X-Figma-Token: $T" "https://api.figma.com/v1/files/$KEY/nodes?ids=426:4075"
```

`scratchpad/read_header.py` walks a node tree and prints one line per layer with
its box, layout, padding, gap, radius, fill and type. **Use the node JSON, not a
screenshot** — it gives numbers you can port instead of pixels you have to guess
at. The header component is `426:4075`; the page frames are children of canvas
`18:946`.

What the reference's header actually measures, already extracted:

```
bar 57 · nav item 45 · padding 29h/13v on a 16px body · gap 7 · corner 3
active #4B619B · inactive = the bar's own colour
search well #0E141B @20%, radius 3 · icon box #76808C @10%
```

Do not open Figma in a browser to look at it — the canvas times out CDP
screenshots. The API is the way in.

---

## 5. Rules that are tested, not documented

Do not quietly relax any of these.

- Removal and data deletion are separate; deletion re-proves containment *at
  deletion time* and is confirmed by typing the device's name.
- A sampling failure degrades and preserves the last good topology. Never "no
  peers".
- **Unmeasured is absent, never zero — and an absence says which kind it is.**
  "Not running" and "could not be asked" are different facts.
- Ownership is a boundary: `force_kill_and_wait` lives on the owned handle and
  there is no pid-based path to it.
- The overlay renders convenience and refuses authority.
- A launch credential is single-use, Orbit-scoped and 30 seconds.
- **Listing is passive; choosing is the act.** The Library and the Space list
  place nothing. `Open` places, and so does selecting a Space to administer.
- **Drawing returns `Vec<Action>` and never calls the client.** A surface that
  called it would do network work on the frame thread; one that kept the answer
  would be a second model of client state. It is also what makes a click
  assertable — press a real control, read what was asked for, no daemon.

---

## 6. After the interface: the address book

The client's core deliverable, and fully specified **in the tracker** — plans are
not markdown here any more.

```json
{"cmd":"packet","reff":"SUB-4"}       ← the engine half: A0 and A1
{"cmd":"packet","reff":"CLIENT-22"}   ← the head: A2
```

Five issued Specs in the Substrate project: a `requirement` (invariants), two
`design` (the model and its Fabric mapping; placement, the daemon service and
passive resolution), a `plan` (phases A0–A5) and a `guide` (every callsite by
name, verified against `v0.7.11`).

Three things to know before starting: Fabric carries every operation A0's mapping
needs (verified — the central bet holds); you will be the **first real caller** of
`export_checkpoint`, which has none outside a contract test; and A0's first pull
request is a **persistence proof, not a feature**.

### Everything else still open

| | Waiting on |
|---|---|
| **CLIENT-24** item-level notification | **SUB-6**. `Request::Signals` is a drain and the head already drains it. Its comment has the shape: the broadcast fan-out already exists, so this is one `subscribe()` per consumer, not a cursor protocol. |
| **CLIENT-33** screen reader | A Windows session with Narrator or NVDA. Everything testable is tested. |
| **CLIENT-16** viewer panes | A scope decision — removing them strands macOS and Linux while v1 is Windows-only. Commented on the issue. |
| **CLIENT-13** the gate | A clean Windows machine, which is what the gate *is*. |
| **SUB-1**, **SUB-3** | Engine initiatives in their own right. |

---

## 7. Traps

**Kill `astrolabe.exe` before running its suite**, or the single-instance test
fails with "the first launch was refused" — which reads like the guard being
broken and is the guard working. Kill `lait.exe` before any build; a live one
holds its image and the link step fails.

**Both `Open` defects had the same shape**: every component correct, the
composition wrong, and a symptom that named nothing. `start_head` passed `--home`
to a launcher mode that did not accept it; then `Client::head` handed the
supervisor the *daemon's* directory beneath the identity, so the head came up,
minted a valid ticket, and served an identity nobody had ever used.
`tests/launch.rs` exists for that class and asserts the chain rather than the
parts. **Add to it before trusting a new seam between the client and a real
process.**

**There is a fourth request-classification site, and it fails at run time.**
Adding a `Request` variant fails compilation in three places by design. But
`handle` dispatches into four `dispatch_*` functions each with an `other =>`
catch-all, so a variant classified one way and arm-matched in another compiles
clean and answers *"request is not an observation operation"* against a live
head. Drive every new verb end to end.

**The coverage manifest cannot be regenerated on Windows.** Push, let the job
fail, take the artifact:

```sh
gh run download <run-id> -n coverage-manifest -D <dir>
cp <dir>/coverage-manifest.txt.actual ci/coverage-manifest.txt
```

Budget one extra push per batch of test changes, and batch them.
`THIRD-PARTY-NOTICES.md` is the same shape *except* it regenerates anywhere:
`bash ci/third-party-notices.sh --update`.

**`egui_kittest`, four ways to get it wrong:** a plain label's text is in the
node's `value`, not its `label`; a selected tab announces through `toggled()`,
not `is_selected()`; a surface heading carries the same text as its tab, so use
`get_by_role_and_label`; and **a text field's contents are a `value` on a node
with no label**, so a scan over `query_all_by_label_contains("")` cannot see them
— assert on the draft the box is bound to.

**Build the harness bigger than the surface** — egui culls interaction outside
the clip rect, so a click on a control that fell off a too-small virtual screen
registers as nothing, which reads exactly like the control being broken.

**A frame that sets only a minimum height takes the whole window**, and its
contents then centre themselves in it. Allocate an explicit strip.

**Use the release binary and rebuild it after pulling.** `target/debug/lait.exe`
derives a different device key than `config/secret.key`, so a debug build is not
an enrolled member: reads work, every write is refused. Root cause undiagnosed,
probably a real bug. Before blaming permissions, run `{"cmd":"diagnose"}` on the
Space plane.

**Never `git add -A` while a sub-agent is editing.** Stage explicit paths.

---

## 8. Verify

```sh
cd viewer && npm run check && npm run test
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked
cargo nextest run --workspace --all-features --profile pr --no-fail-fast
bash ci/smoke-p0.sh
bash ci/third-party-notices.sh --check
cargo test -p astrolabe
```

## 9. Working the tracker

You are a member with write standing. There is no CLI — start a head and speak
HTTP:

```sh
./target/release/lait.exe --json --port 0     # {url, token, port} before it accepts
```

Orbit `orb_61edfc37740cf3068605fde7dd0bfbf1323690f0c11b949827f84685f63e29dc`;
`Bearer <token>` on every `/api` route; `/api/host/rpc`,
`/api/spaces/{orbit}/rpc`, `/api/spaces/{orbit}/worlds/issues/rpc`.

The wire `cmd` is the **Rust variant name**, not the MCP tool name — `issue_view`
not `view`, `list` not `issue_list`. An issue's body is in `description`, not
`body`.

- File and move work as you go; a Plan reads current morphology.
- `geometry::compile` filters to one project, so SUB→CLIENT edges are real in the
  catalog and absent from a Plan's drawing.
- **Do not write a new plan as a markdown file.** `docs/plans/` is gitignored and
  deprecated. Author it as Specs — `requirement` for what may never happen,
  `design` for structure, `plan` for the order of work, `guide` for what an
  implementer must find — and link them onto the issues they govern. The address
  book is the worked example.
- Found something a Spec asserts that the code contradicts? `spec_observe`. When
  it is an *issue's* claim, comment on the issue. Both were used here.
