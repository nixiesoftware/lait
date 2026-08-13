# Astrolabe shell and Library design QA

## Comparison target

- Source visual truth:
  - `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 025459.png` — Steam Library, 2557 × 1391 px.
  - `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 025419.png` — GOG Galaxy selected-item view, 1713 × 796 px.
- Rendered implementation: `artifacts/astrolabe-library-final.png`, 1024 × 711 px.
- CSS/logical viewport: 1024 × 711 at device pixel ratio 1 in the Flutter inspector capture.
- State: dark theme, Library selected, ISSUEWORLD ready, one local browser head.
- The sources are directional references rather than a screen to clone pixel-for-pixel. Comparison therefore evaluates the borrowed composition: dense Library spine, selected-item hierarchy, integrated chrome, and persistent bottom status.

## Findings

- No actionable P0, P1, or P2 differences remain.
- Fonts and typography: Instrument Sans and JetBrains Mono resolve through Covalence. Hierarchy is legible at the real desktop viewport, with compact chrome, a larger selected-World title, quiet fact labels, and a distinct path/address register.
- Spacing and layout rhythm: the 224 px Library rail, 48 px caption band, 32 px operational footer, 196 px hero, panel gutters, and compact row density preserve the useful Steam/GOG proportions without importing their store content.
- Colors and visual tokens: dark is the default and uses a cool neutral ramp rather than pure black. The separately rendered light variant remains readable and does not merely invert status colors. Focus, selection, border, and status treatments come from Covalence token axes.
- Image and asset fidelity: the reference products depend on game artwork, but Astrolabe has no authoritative image contract. The implementation correctly derives a restrained hero only from World-declared accent data and does not invent or fetch artwork. Icons use the Covalence pack.
- Copy and content: lifecycle copy distinguishes Ready, Starting, Running, Could not ask, and Unavailable. Running changes the action to View. The footer reports local identity, current action, heads, Spaces, and version without exposing a launch credential.

## Full-view comparison evidence

- `artifacts/astrolabe-library-final.png` shows the final dark Library against the Steam and GOG source captures in one comparison pass.
- `artifacts/astrolabe-theme-toggle-check.png` verifies the separately tuned light surface and the working in-chrome theme action.
- `artifacts/astrolabe-operations-dark.png` verifies that the four primary destinations resolve Operations into a compact Devices / Heads / Storage / Diagnostics contextual rail.

Focused crops were not needed: the full 1024 × 711 captures keep all critical chrome, labels, borders, action states, and footer content readable. The two variant captures isolate the two regions whose behavior cannot be judged from the Library frame alone.

## Interaction and runtime evidence

- Primary interactions tested through the Mosaic Flutter desktop lane: theme toggle, Library navigation, Operations navigation, and the Open lifecycle transition.
- Widget tests cover passive selection, declared routes, disabled in-flight actions, Ready → Starting, Running → View, Library search, footer deduplication, and launch-ticket sanitization.
- Flutter runtime log checked after the final navigation and theme passes. No framework exceptions or layout overflow reports were present.

## Comparison history

1. The first rendered pass, `artifacts/astrolabe-library-dark.png`, exposed a P1 security and information-density problem: the footer printed the full single-use launch-ticket URL.
2. The footer now reduces launch records to scheme, host, port, and path, and deduplicates identical notices. `artifacts/astrolabe-library-final.png` confirms the query credential is absent while the successful browser handoff remains visible.
3. No P0/P1/P2 findings remained in the final dark, light, and Operations captures.

## Follow-up polish

- P3: evaluate the Library rail at the 640 × 480 minimum with several long World names once a representative multi-World fixture is available.
- P3: tune World-provided hero accents against additional real declarations; ISSUEWORLD currently supplies no World accent in this Space-level row, so the correct rendering is neutral.

## Implementation checklist

- [x] Dark-first theme with a visible light option.
- [x] Refined primary and contextual navigation.
- [x] Persistent operational footer.
- [x] Searchable lifecycle-grouped Library rail.
- [x] Ready, Starting, Running/View, unreachable, and unavailable presentation.
- [x] Credential-safe, deduplicated activity summary.
- [x] Dark, light, and Operations visual verification.

final result: passed
