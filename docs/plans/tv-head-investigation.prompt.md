# Investigation brief: TV "viewer-head" architecture for lait

You are investigating an architecture decision for **lait**, a local-first,
peer-to-peer, end-to-end-encrypted issue tracker (Rust engine in `src/` +
`crates/` + `products/`, React viewer in `viewer/`). Read `CLAUDE.md` and
`docs/ARCHITECTURE.md` first.

## The goal

We want lait to be viewable and lightly operable on **televisions** — a wall
display for a team's work. TVs are the first intended consumers of a general
**"viewer-head"** model: an unprivileged, read-mostly client that renders a
lait Space on a screen it does not own, holding no journal, no identity, and no
sync responsibility.

**Roku is the beachhead platform**, chosen for fast time-to-device. tvOS,
Android TV, Tizen, and webOS are expected to follow, so the design must not be
Roku-shaped.

Your job is **not** to build the Roku app. It is to determine **which seam a
head binds to**, and to produce the architecture that answers that.

---

## Established findings — do not re-derive these

### Roku platform capabilities (researched; treat as settled)

- **BrightScript + SceneGraph only.** No native code path: the Independent
  Developer Kit (C++) is deprecated. No JVM, no WASM, **no webview/browser** —
  the React viewer cannot be embedded and the Rust engine cannot be
  cross-compiled.
- **Crypto is symmetric-only.** `roEVPCipher` (AES/3DES/DES/Blowfish),
  `roEVPDigest` (SHA), `roHMAC`, `roDeviceCrypto` (seals bytes under a
  per-app/device/model key). **No ed25519, X25519, RSA, or ECDH.** lait's
  identity is ed25519 and its E2EE uses X25519 sealed boxes, so a Roku device
  can never be a lait peer.
- **Networking is capable.** `roUrlTransfer` (HTTP/HTTPS, sync + async, custom
  headers, `EnablePeerVerification(false)` for self-signed);
  `roStreamSocket`/`roDatagramSocket` give Berkeley-style TCP **including
  `listen()`/`accept()`** and UDP multicast (so SSDP/mDNS is hand-rollable).
  No TLS on raw sockets, no native WebSocket, and **no streaming-response API**
  — async transfers deliver on completion, so SSE needs a raw socket or polling.
- **ECP** (RESTful HTTP on port **8060**) lets anything on the LAN launch the
  app, deep-link it, and inject literal keystrokes (`Lit_` prefix).
- **Storage is tiny**: `pkg:` read-only, `tmp:` wiped on exit, `cachefs:`
  evictable; the only durable store is a **32 KB registry** per app.
- **Foreground-only lifecycle.** No daemons, no launch-on-boot. Instant Resume
  suspends an app in RAM; it is not a running node.
- **Budgets**: 4 MB package for certification, < 15 s launch, < 3 s transitions,
  **< 250 ms remote-keypress response**; low-end devices ship 512 MB DRAM total.
- **Screensaver channels** are a real standalone app type that **forbids user
  input and video playback**.
- **Distribution**: private channels were removed in Feb 2022. Options are
  dev-mode sideload (instant, own devices), Beta Channels (capped and
  time-limited), or full Streaming Store certification. Certification criteria
  do not require video, but the pipeline is built around content apps — treat
  "fast approval for a utility app" as an **unvalidated assumption**.

### lait-side findings (verified in-repo; re-check but don't rediscover)

- **`lait serve` is loopback-only by design.** It binds `Ipv4Addr::LOCALHOST`
  (`src/serve/mod.rs:124`) and refuses any request whose `Host` is not a
  loopback authority (`src/serve/auth.rs:120`) as its DNS-rebinding guard. A LAN
  device cannot reach it today, and the loopback binding **is** the security
  model — widening it is not a flag flip.
- **The World-neutral seam is `crates/world-interface`**, whose header states
  that the runtime's World-call boundary "deliberately knows nothing about
  presentation." A package declares a CLI mount, MCP tools, `parse_web`, and
  `present_reply`.
- **The neutral presentation vocabulary is text.** `Presentation` is
  `{ stdout, stderr, exit_code, failure, failure_message }` and
  `PresentationOptions` is `{ json, color }`. `ClientOutput` states the split:
  "native clients render [presentation]; web clients return `value` directly."
  **There is no structured/graphical view vocabulary at the neutral seam.**
- **Issues is one World, not the view layer.** `products/issues/src/dto.rs`
  (`BoardView`, `BoardColumn`, `Row`, `IssueView`, `InboxEntry`, `GraphView`) is
  the *Issues World's* vocabulary in its pure semantic crate.
  `products/issues-app/` is the application package that owns the external
  protocol and client surfaces, with the terminal renderer in
  `presentation.rs` (~733 lines) and `projections.rs` alongside it.
- **The web route is generic; the payload is not.**
  `/api/spaces/{id}/worlds/{world}/rpc` resolves any mounted package via
  `registry.package_for_mount` (`src/serve/mod.rs:613`), but returns opaque
  World-specific JSON. The React viewer resolves this by **being an Issues
  client** — `viewer/src/api.ts:79` hardcodes `/worlds/issues/rpc`.
- **Access classification already exists** at the seam:
  `ClientAccess::{Query, Command}`, with the shell enforcing declared access and
  confirmation policy per invocation, and
  `WorldClientPackage::confirmation` existing so "the CLI prompt and the
  browser's modal cannot disagree about what is dangerous."

---

## The decision to resolve

**Which seam does a head bind to?** Three candidates:

1. **Bind to a World.** The TV app is an Issues client, as the React viewer is.
   Fastest; matches precedent. Cost is N Worlds × M platforms, permanently.
2. **Bind to `world-interface` via a new structured presentation mode.** Add a
   third mode beside terminal and raw JSON: a World-neutral view projection
   (columns/cards/fields/focusables) that each app package projects into, just as
   `presentation.rs` projects into ANSI today. Cost becomes M renderers + N
   projections. Extends an existing seam rather than inventing one.
3. **Bind to pixels.** The host renders frames; the TV displays them. Dissolves
   World-neutrality but inherits the input-latency problem.

A prior conclusion worth testing rather than assuming: **the answer may differ
by mode** — pixels for the input-free ambient/screensaver surface (where Roku
forbids input anyway, so constraint and design agree), structured data for the
interactive surface (where the 250 ms budget makes per-keypress round trips
untenable). And a suggested sequencing: **build option 1, but locate the code
where option 2 would live** (a TV projection in `issues-app/presentation.rs`,
shaped World-neutrally), so promotion later is a refactor with one caller.

Test these. Do not treat them as given.

---

## Investigate

**A. Does option 2 pay for itself?**
Draft the World-neutral view vocabulary concretely — as Rust types — and
hand-write what `issues-app` would emit for a board, an issue detail, and an
inbox. Then stress it: what does a World unlike Issues (a calendar, a document
store, a chat log) need that the vocabulary can't express? Where does it force a
World to lie about its shape? Is it a projection or a lowest-common-denominator
trap? Give a defensible verdict, not a survey.

**B. What does the head listener have to be?**
`serve`'s loopback guard cannot simply widen. Determine whether a head listener
belongs as a separate module, a separate binding on the same router, or a
distinct `lait head` surface — and what its threat model actually is with no
browser, no cookies, and no ambient authority in play. Read
`docs/THREAT-MODEL.md` and `src/serve/auth.rs` and argue from what's there.

**C. What is a head credential?**
It must be long-lived (a TV must not re-auth), device-scoped, revocable, and
World-scoped. Determine how it should ride the existing
`ClientAccess::{Query, Command}` classification rather than a parallel
mechanism, what `Command` operations (if any) a TV should be granted, and how
`WorldClientPackage::confirmation` gets honoured on a surface with no good place
for a modal. This is the security-critical part of the brief.

**D. Discovery and pairing.**
SSDP/mDNS from the head, with the ECP-injection trick available on Roku
specifically. What is the platform-neutral mechanism, and what degrades
gracefully?

**E. Ambient mode.**
Does a host-rendered idle frame compositing across *all* mounted Worlds avoid
the presentation seam entirely? If so, is it the cheaper first deliverable?

**F. Validate the Roku store assumption.**
The platform choice rests on fast approval for a non-video utility app. Find
evidence for or against — precedent apps, developer reports, category rules.
If it doesn't hold, say so plainly and note what changes (probably nothing:
sideload is instant regardless).

---

## Deliverable

A written architecture recommendation containing:

1. A **decision** on the binding seam, with the reasoning that forced it, and an
   explicit statement of what would have to be true for the runner-up to win.
2. The **Rust type sketch** for whatever seam you recommend (concrete, compiling
   or near-compiling signatures — not prose).
3. A **build order** with the World-neutral work isolated from Roku-specific
   work, so the first platform doesn't contaminate the seam.
4. **Named risks** — specifically anything where a TV client would silently
   diverge from CLI/web behaviour (confirmation policy is one; find the others).
5. Every claim about the repo anchored to `file:line`.

## Constraints

- **Read and analyse; do not implement.** No production code changes. Type
  sketches belong in the document.
- **Do not commit the output.** This repo keeps design/scoping docs as local
  scratchpads. Write to `docs/plans/`. Note that `docs/plans/` is currently
  **untracked but not gitignored** — verify with `git check-ignore -v`, and flag
  it rather than committing anything.
- Prefer reading the code over reading the docs where they disagree; the docs
  set is lean and the seams are documented in-source at length.
- If you touch Rust for a spike, `cargo fmt --all` before showing it, and kill
  running `lait` daemons first — they hold the `.exe` lock during a test build.

## Out of scope

Roku UI design, SceneGraph implementation, store submission mechanics, and any
non-TV head (mobile, embedded display). Note them if they change the seam
decision; otherwise leave them.
