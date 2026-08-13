# Astrolabe shell and Library design QA

## Comparison target

- Source visual truth:
  - `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 025459.png` — Steam Library, 2557 × 1391 px.
  - `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 025419.png` — GOG Galaxy selected-item view, 1713 × 796 px.
  - `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 095620.png` — prior Astrolabe Library with the requested edge-to-edge correction, 1020 × 704 px.
  - `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 101439.png` — Steam's stable launch/status/configuration band.
  - `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 110548.png` — Astrolabe's separate settings window before owned chrome.
- Rendered implementations:
  - `artifacts/astrolabe-library-running.png`, 1040 × 720 px.
  - `artifacts/astrolabe-world-settings.png`, 560 × 680 px.
- CSS/logical viewport: 1040 × 720 at device pixel ratio 1 in the Flutter inspector capture.
- State: dark theme, Library selected, ISSUEWORLD running through an existing local identity daemon, separate World settings window open.
- The Steam and GOG sources remain directional references. The 09:56 Astrolabe capture plus the user's follow-up are the exact scoped target: preserve the established shell while removing the outer Library gutter, making both the hero and primary Open/lifecycle band full-bleed in the detail pane, and moving the inset to the information content only.

## Findings

- No actionable P0, P1, or P2 differences remain.
- Fonts and typography: Instrument Sans and JetBrains Mono resolve through Covalence. Hierarchy is legible at the real desktop viewport, with compact chrome, a larger selected-World title, quiet fact labels, and a distinct path/address register.
- Spacing and layout rhythm: the 224 px Library rail now begins at the client's left edge. The 196 px hero begins at x=224/y=0 and fills the complete detail width. The primary Open/lifecycle band follows at the same full width with a square lower rule; World Details and Serving Now alone begin on a separate 20 px inset below it. The 48 px caption band, 32 px operational footer, and compact row density retain the established proportions.
- Colors and visual tokens: dark is the default and uses a cool neutral ramp rather than pure black. The separately rendered light variant remains readable and does not merely invert status colors. Focus, selection, border, and status treatments come from Covalence token axes.
- Image and asset fidelity: the reference products depend on game artwork, but Astrolabe has no authoritative image contract. The implementation correctly derives a restrained hero only from World-declared accent data and does not invent or fetch artwork. Icons use the Covalence pack.
- Copy and content: lifecycle copy distinguishes Ready, Launching, Running, Could not ask, and Unavailable. Running remains a factual state and gains a separate Go to action; it never becomes Cancel or Stop. The stable band reports last opened, version, and sync before the far-right settings control. The footer reports local identity, current action, heads, Spaces, and version without exposing a launch credential.
- Window chrome: all Astrolabe windows now receive native hidden-titlebar options and the visible frame from one shared window contract. The main window uses the preserve-to-tray close policy; disposable secondary windows use close. The settings comparison confirms that the white native titlebar, Flutter icon, and theme discontinuity are gone while native-sized caption targets remain at the top-right edge.

## Full-view comparison evidence

- `artifacts/astrolabe-library-final.png` was opened in the same comparison input as the 09:56 Astrolabe source capture. The comparison shows the requested delta directly: the outer Library gutter and inter-pane gap are gone, the rail divider meets the header and footer, the hero and Open/lifecycle band are rectangular and full-bleed, and only the lower information panels remain inset.
- `artifacts/astrolabe-theme-toggle-check.png` verifies the separately tuned light surface and the working in-chrome theme action.
- `artifacts/astrolabe-operations-dark.png` verifies that the four primary destinations resolve Operations into a compact Devices / Heads / Storage / Diagnostics contextual rail.
- `artifacts/astrolabe-library-running.png` verifies the persistent Running state, separate Go to action, stable runtime facts, far-right settings control, and successful attachment to an Orbit left running by the existing local identity daemon.
- `artifacts/astrolabe-world-settings.png` was opened in the same comparison input as the 11:05 source capture. The settings content and geometry are preserved, while its top edge now uses the exact Astrolabe wordmark, border, surface, and painted window controls used by the primary client.

The source and implementation were compared at native DPR 1; their 4 × 7 px capture-size difference is limited to the outer window bounds and does not affect the measured client layout. Focused crops were not needed because the full captures keep the rail edge, hero bounds, lower-content inset, labels, borders, action state, and footer readable. The two variant captures isolate the two regions whose behavior cannot be judged from the Library frame alone.

## Interaction and runtime evidence

- Primary interactions tested through the Mosaic Flutter desktop lane: theme toggle, Library navigation, Operations navigation, and the Open lifecycle transition.
- Widget tests cover passive selection, declared routes, disabled in-flight actions, Ready → Launching, Running + Go to, shared settings chrome, Library search, footer deduplication, and launch-ticket sanitization.
- A daemon integration test proves a passive Worlds probe cannot place a vacant Orbit and does report it after an explicit placement. The client also falls back to the compatibility-window Status probe, so it can recognize an Orbit already held by a running local identity daemon without restarting or replacing it.
- Flutter runtime log checked after the final navigation and theme passes. No framework exceptions or layout overflow reports were present.

## Comparison history

1. The first rendered pass, `artifacts/astrolabe-library-dark.png`, exposed a P1 security and information-density problem: the footer printed the full single-use launch-ticket URL.
2. The footer now reduces launch records to scheme, host, port, and path, and deduplicates identical notices. `artifacts/astrolabe-library-final.png` confirms the query credential is absent while the successful browser handoff remains visible.
3. No P0/P1/P2 findings remained in the final dark, light, and Operations captures.
4. The 09:56 annotation capture exposed a P2 composition issue: a shared page gutter inset the entire Library, and a second gap kept the hero from owning the detail pane.
5. Library now bypasses the document-style page margin, the rail retains only its internal control padding, the hero and primary Open/lifecycle band are full-bleed and square at the pane boundary, and the information stack alone owns the 20 px inset. The revised `artifacts/astrolabe-library-final.png` was compared with the annotation capture and has no remaining P0/P1/P2 findings.
6. The 11:05 settings capture exposed a P1 ownership break: a light native titlebar sat over an otherwise dark Astrolabe window. Native options and visible chrome are now one reusable contract consumed by both primary and secondary windows; the implementation comparison has no remaining P0/P1/P2 shell findings.
7. Runtime QA exposed a lifecycle reporting defect: Launch placed ISSUEWORLD, but an older daemon rejected the passive Worlds probe and the Library stayed Ready. The new compatibility fallback reports Running from passive Status without waking vacant Orbits; the final capture verifies Running and Go to against the daemon that was already active.

## Follow-up polish

- P3: evaluate the Library rail at the 640 × 480 minimum with several long World names once a representative multi-World fixture is available.
- P3: tune World-provided hero accents against additional real declarations; ISSUEWORLD currently supplies no World accent in this Space-level row, so the correct rendering is neutral.

## Implementation checklist

- [x] Dark-first theme with a visible light option.
- [x] Refined primary and contextual navigation.
- [x] Persistent operational footer.
- [x] Searchable lifecycle-grouped Library rail.
- [x] Edge-to-edge Library frame with full-bleed selected-World hero and Open/lifecycle band.
- [x] Ready, Launching, Running/Go to, unreachable, and unavailable presentation.
- [x] Persistent Running state with a separate Go to action and no stop/cancel implication.
- [x] World settings as an independently movable desktop window.
- [x] Shared Astrolabe-owned chrome and explicit close behavior for every window.
- [x] Passive attachment to a running local identity daemon across the supported compatibility window.
- [x] Credential-safe, deduplicated activity summary.
- [x] Dark, light, and Operations visual verification.

final result: passed
