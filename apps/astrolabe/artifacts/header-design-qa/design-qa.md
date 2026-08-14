# Astrolabe header design QA

## Comparison target

- Two-tier hierarchy source: `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 194943.png`.
- Identity-block shape source: `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 202916.png`.
- Before-state reference: `C:\Users\Huginn\Pictures\Screenshots\Screenshot 2026-08-13 194936.png`.
- Native closed state: `implementation-no-in-app-icon.png`.
- Native open state: `implementation-no-in-app-icon-open.png`.
- Earlier focused side-by-side evidence: `comparison-identity-shape.png`.

## Normalization

- The identity reference is 349 × 763 px; its first 260 px were compared at 1:1 scale with the first 360 × 260 px of the native Astrolabe capture.
- The native Astrolabe window is 1040 × 720 logical pixels at the observed 1.0 capture density.
- State: dark theme, Library selected, local identity online, settings open for the focused comparison.
- Product-specific content is intentionally different; the comparison judges the identity block, two-tier hierarchy, anchored-panel silhouette, type hierarchy, spacing, and alignment.

## Fidelity review

- Header geometry: the primary client now has a 64 px identity tier followed by a distinct 44 px navigation tier. The mark, two-line identity, caret, navigation, address-book action, and Windows controls remain aligned without clipping.
- Identity treatment: the in-app mark was removed at the user's request. The title uses Astrolabe's brand color and the second line reports live local-identity state.
- Settings surface: the identity trigger opens a 336 px anchored menu with a text-only identity header, version, labelled settings section, refresh shortcut, and theme action. Its narrow stacked silhouette follows the supplied Friends & Chat reference while retaining proper menu semantics.
- Typography and color: the implementation keeps Astrolabe's existing Covalence type roles and color tokens. It matches the reference's bright identity name, quieter status, charcoal bands, and low-contrast separators without copying Steam's proprietary typeface.
- Accessibility and interaction: the trigger announces `Astrolabe settings` with expanded/collapsed semantics. Keyboard activation, menu-item roles, F5 refresh, theme switching, tab navigation, address-book access, window dragging, double-click maximize, and caption controls remain available.
- Asset quality: there are no raster assets in the in-app header or menu. The Windows launcher continues to use the existing `windows/runner/resources/app_icon.ico`, and all action glyphs use Covalence's configured icon pack.

## Findings and fixes

### Iteration 1

- [P2] The first implementation used a 32 px text-only wordmark, which matched Steam's main-client density but not the later supplied Friends & Chat identity shape.
- Fix: replaced it with the real mark, two-line identity and disclosure caret in a 64 px tier.

### Iteration 2

- [P2] The original 220 px two-row menu was visually too small to carry the reference's narrow-panel character.
- Fix: widened it to 336 px and added a structured identity header plus labelled settings section while preserving refresh and theme behavior.

### Final comparison

- `comparison-identity-shape.png` confirms the matching square identity mark, stacked cyan identity text, caret, dark two-tier structure, and narrow anchored panel.
- No actionable P0, P1, or P2 differences remain. Intentional deviations are Astrolabe-specific content and tokens, the user-directed text-only identity treatment, and the absence of Steam's friends-only actions.

### User-directed follow-up

- Removed the mark from both in-app identity placements and removed its Flutter asset registration.
- Preserved `Runner.rc` and the `.ico` launcher resource unchanged.
- Rebuilt and relaunched the complete native application; the final closed and open states contain no in-app icon.

## Verification

- `flutter analyze`: passed with no issues.
- `flutter test`: 39 tests passed.
- `flutter build windows --debug`: passed.
- Native closed and open states were captured and reviewed at 1040 × 720.
- Primary interactions tested: settings open/close, refresh dispatch, theme callback, primary navigation, F5 shortcut preservation, and native caption behavior.
- Console errors: not applicable to this native client; build, analyzer, and widget-test output contained no task-related errors.

## Follow-up polish

No P3 follow-up is required for the requested header change.

final result: passed
