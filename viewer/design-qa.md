# Empty-state design QA

- Source visual truth: `artifacts/empty-state-qa/linear-projects-reference.jpg`
- Rendered implementation: `artifacts/empty-state-qa/local-issues-projects.png`
- Full comparison: `artifacts/empty-state-qa/projects-full-comparison.png`
- Focused comparison: `artifacts/empty-state-qa/projects-focused-comparison.png`
- State: Projects empty state, light theme, Local Issues World
- Source pixels: 768 × 521; compared as a 768 × 477 app crop
- Implementation pixels: 2880 × 1800; app content crop normalized to 768 × 441 and padded to 768 × 477 for the full comparison
- Browser viewport evidence: 2880 × 1800 macOS screen capture of Vivaldi rendering the Local Issues World at `127.0.0.1`; the browser chrome was excluded from the normalized comparison

**Findings**

- No actionable P0, P1, or P2 differences remain.
- Fonts and typography: both use a compact neutral sans-serif hierarchy. The implementation keeps the product's existing font and slightly stronger button label; title, body, and action still read in the same order as the source.
- Spacing and layout rhythm: the empty-state group is centered in the work area, internally left-aligned, and uses a compact art/title/body/action stack. The illustration is slightly larger than Linear's reference by intent, but no longer reads as a floating app icon.
- Colors and tokens: the implementation preserves the issue tracker's black primary action instead of copying Linear's violet control. Illustration color is now near-monochrome graphite with only faint dusty-lilac stipple.
- Image quality and asset fidelity: the generated 480 × 300 RGBA artwork has clean alpha, thin editorial outlines, restrained stipple, no glow, and no saturated fills. The compact interlocking cube cluster closely matches Linear's metaphor without copying its pixels.
- Copy and content: the implementation intentionally compresses Linear's teaching paragraph to one sentence, following the user's request to keep the states minimal.

**Comparison history**

1. Initial implementation: P1 art-direction mismatch. The Projects image used chunky black outlines and saturated violet 3D blocks; the text stack was centered and visually broad.
2. Fix: regenerated the complete asset family as thin, near-monochrome editorial line art; tightened the Projects cubes into one interlocking cluster; reduced illustration dimensions; left-aligned descriptive states; shortened copy; preserved centered terse states.
3. Post-fix evidence: the full and focused comparison images show matching composition, hierarchy, metaphor, and visual weight. No actionable P0/P1/P2 differences remain.

**Interaction and accessibility checks**

- Existing Create project behavior is unchanged.
- Decorative artwork remains `alt=""` and `aria-hidden="true"`.
- Light and dark asset families were inspected as contact sheets; dark mode reverses the near-monochrome linework while suppressing opaque white faces.

**Follow-up polish**

- P3: If the product later moves its primary action token toward violet, the empty-state CTA will become even closer to the reference without component-specific styling.

final result: passed
