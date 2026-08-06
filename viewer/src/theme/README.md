# The design system

lait's UI is Astryx (`@astryxdesign/core`), themed from lait's own palette.
**Zero Radix packages. One component system.**

It started as a spike with one question — *does lait's palette survive Astryx,
or does adopting Astryx mean adopting Meta's colour?* — and the answer was yes,
intact and in OKLCH, so it became the rewrite. Everything below is what that
established, including the parts that did not go well.

## What is still ours, and why

Astryx is the system. Five things extend it rather than being replaced by it,
each because Astryx would otherwise have to own something it cannot know:

| ours | why it stays |
|---|---|
| `Kbd` | our keybinding registry formats chords; Astryx's `Kbd` wants its own grammar |
| `LabelChip` / `LabelChips` / `LabelDots` | label colour comes from the space's catalog, per label |
| `Avatar` | colour and fallback derive from a device key |
| `ui/icons.tsx` | priority bars and the status ring are data encoded as shape |
| `controlTrigger` / `interactiveRow` / `navigationItem` | the recipes for rows, cells and triggers that are not Buttons |

Everything else — buttons, fields, dialogs, popovers, menus, tooltips, badges,
checkboxes, switches — is Astryx, extended through the theme where lait's
vocabulary is larger than theirs (`urgent`/`high`/`medium`/`low`, `danger`,
lait's neutral-inverse `primary`, the pill radius, the nine categorical hues on
lait's curve).

## What was built

- `tool/palette.mjs` — the seeds and curves, extracted from
  `generate-tokens.mjs` so two generators can read one source of truth.
  `generate-tokens.mjs` now imports it and emits a **byte-identical**
  `tokens.generated.css` (`npm run tokens:check` passes, 412 tests pass).
- `tool/generate-astryx-theme.mjs` — the same palette projected onto Astryx's
  token vocabulary. `npm run tokens:astryx`.
- `src/theme/laitTheme.ts` → `astryx theme build` → `lait.css` / `lait.js` /
  `lait.d.ts` / `lait.variants.d.ts`.
- `src/theme/proof.tsx` + `proof.html` — a throwaway harness. Delete with the
  branch.

## What the spike proved

**OKLCH passes through untouched.** `defineTheme`'s `TokenValue` is
`string | [light, dark]` — arbitrary CSS, no colour parsing. The compiled
`lait.css` contains 46 `oklch()` values and 22 `color-mix(in oklch, …)`
derivations. The only hex in the file is our own `--color-accent-fg`, which was
always a literal. The browser keeps doing per-display gamut mapping.

**`light-dark()` is the native shape.** `[light, dark]` tuples compile straight
to `light-dark(…)`, wrapped in `@scope ([data-astryx-theme="lait"])` inside
`@layer astryx-theme`. Our palette needed no restructuring at all.

**Our vocabulary extends theirs.** `defineTheme.tokens` is typed against a
closed set of 79 colour tokens, and priority is not in it. That looked like a
wall. It is not: `StatusDotVariantMap` and `BadgeVariantMap` are declared as
open interfaces, and `astryx theme build` detects `variant:*` keys in
`components` that are not in the base type and **writes the module
augmentations itself** — see `lait.variants.d.ts`. So `urgent / high / medium /
low` became first-class variants drawn from our ramp, and
`<StatusDot variant="urgent" />` type-checks.

**Astryx's nine categorical hues are now ours.** They are declared at Astryx's
hue angles but take lightness and chroma from our accent curve, so a label chip
cannot out-shout a priority glyph two columns away. This was the single
cheapest win in the spike.

## What it cost

Measured by building `proof.html` alone (12 components + React 19):

| | raw | gzip |
|---|---|---|
| CSS (`astryx.css` + reset + theme) | 150 kB | **27 kB** |
| JS (incl. React) | 418 kB | **127 kB** |

Current committed `app.js` baseline is 860 kB / 258 kB gzip. Astryx's CSS is
shipped whole on the pre-built path; the source path tree-shakes to roughly a
third but needs the StyleX build plugin.

## Density: resolved, as a cascade layer

Density is **not** a theme. `tool/geometry.mjs` owns the axis; the generator
emits both densities into `src/theme/geometry.generated.css` under
`@layer astryx-density`, and the toggle stays exactly what it already was — one
`[data-density]` attribute on `<html>`, set by `applyDensity` in `App.tsx`.

Three reasons it is a layer and not a second theme:

1. **Size.** Measured: `defineTheme({extends: laitTheme})` overriding *two*
   tokens still compiles to 93 token overrides / 9.4 KB, because `extends`
   merges and re-emits everything. The layer is 30 declarations, ~1 KB.
2. **Switch cost.** An attribute write is one style recalc. Swapping
   `<Theme theme>` re-renders the provider and re-registers the theme *before*
   doing that same recalc. Measured flip, including a forced synchronous
   layout: **4.1 ms**, zero React renders.
3. **Orthogonality — the real reason.** N themes × M densities is N×M artefacts
   if density is a theme, and N+M if it is a layer. One theme and two densities
   today; the argument only strengthens with a second theme.

It works because Astryx components read tokens through `var()` — 426
`--spacing-*` references, 50 `--radius-*`, 25 `--font-size-*`.

Verified: `--spacing-3` 12 → 13.5px, `--font-size-base` 13 → 14px,
`--text-body-leading` 1.5385 → 1.5, row height 56 → 61px — while
`--spacing-icon-sm` stays 14px and the status dot stays 8px. **The pinned axes
do not move**, which is the invariant `designSystem.test.ts` exists to protect.

## Use the CLI, not guesswork

`@astryxdesign/cli` is the authoritative surface and it is built for agents:
`--dense` for token-efficient docs, `--json` for typed output, `--detail`
for depth. The commands that earned their keep here:

```sh
astryx doctor                    # validated the hand-rolled install
astryx docs migration --dense    # the Tailwind-coexistence layer order + bridge
astryx docs icons                # the 29 semantic names
astryx component Button --dense  # the real API — elevation, isIconOnly, clickAction
astryx theme build <file>        # compiles the theme + generates variant augmentations
```

Reading `astryx docs migration` changed two decisions: the cascade order in
`styles.css` is now theirs, and `@astryxdesign/core/tailwind-theme.css` is
imported so a not-yet-migrated Tailwind surface writes `bg-surface` and gets
the *same* colour a migrated Astryx component gets. Without that bridge the two
halves of the app read from two palettes for the duration of the migration.

`astryx init` is deliberately NOT run: it writes agent docs into `CLAUDE.md`,
and that file is ours. `--agent-docs-path` can retarget it if we want them.

## Buttons: ten variants became four plus one

`primitives.tsx` grew ten button variants. The first read of Astryx's four was
that we needed six generated ones. Reading `astryx component
Button|IconButton|Link|SegmentedControl` showed most of ours existed because we
lacked the right *component*:

| ours | becomes |
|---|---|
| `ghost`, `destructive` | native, one for one |
| `outline` | `variant="secondary" elevation="low"` — `elevation` is `shadow-control` |
| `toolbar` | `variant="secondary" size="sm"` |
| `primary` | native, with a theme override — see below |
| `danger` | the one genuinely new variant |
| `active` | `<SegmentedControl>` — the variant was emulating one |
| `inline` | `<Link>` |
| `pill`, `size="icon"` | `<IconButton>` |

`primary` keeps our meaning by theme override rather than by a new name: ours is
a neutral inverse, not an accent fill, because "a neutral inverse commit keeps
blue available for focus and state instead of making every save look like a
Jira call-to-action". That reasoning still holds, so the look is overridden and
the name is not.

`danger` is new vocabulary — the quiet inline "X" that only reddens on hover, as
distinct from `destructive`, the filled confirm. One asks, the other confirms;
having both is the point.

## The call-site sweep

139 usages across 30 files, done with a codemod (`scratchpad/codemod.py`,
three passes) and 14 hand edits. `tsc` was the gate throughout: 0 errors,
412/412 tests.

- `Button` 83, `IconButton` 56, `Link` 1.
- `disabled`/`loading` -> `isDisabled`/`isLoading`; `title` -> `tooltip`
  (Astryx's `BaseProps` omits `title`); children -> `label`, with the glyph
  moving from children into the `icon` slot.
- lait's default button size was `sm` and Astryx's is `md`, so every migrated
  site states `size="sm"`. Omitting it would have grown every button a rung.
- `primitives.tsx` lost `Button`, `IconButton`, `InlineAction` and the
  ten-variant `button` recipe — about 130 lines, replaced by a note.

Two tests changed, both because Astryx's behaviour is better and ours asserted
the old shape:

1. `[role="status"]` is no longer unique — Astryx buttons publish their own
   live regions, so the bar has three. The progress line got a
   `data-bulk-progress` hook rather than relying on document order between two
   design systems.
2. A disabled button with a tooltip gets `aria-disabled`, not `disabled`, so it
   stays focusable and can still explain why it is unavailable. The test now
   asserts that.

Cost of the whole migration, measured against `HEAD`:

| | before | after |
|---|---|---|
| `app.js` | 860 kB / 258 kB gzip | 1,028 kB / **306 kB gzip** |
| `index.css` | 62 kB / 12 kB gzip | 215 kB / **39 kB gzip** |

Tailwind is still in the bundle and still winning the cascade for the surfaces
that have not moved. Both numbers come down when it goes.

## The rest of the primitives

Fields, dialogs and popovers followed the buttons. `tsc` clean, 412/412.

**Fields.** `Input` + a sibling `FieldLabel` collapsed into one `TextInput`:
Astryx's inputs are field-level and own their label, description, status and
required/optional markers. `NewProject`'s key field lost a hand-rolled
`aria-describedby` span — the guidance is `description` and the error is
`status`, both wired by the component. `onChange` hands you the value, not the
event, so `(e) => setX(e.target.value)` became `setX`.

**Dialogs.** `dialogs.tsx` dropped both Radix dialog packages.
`AlertDialog` takes `title` / `description` / `actionLabel` / `onAction` as
*required* props, so the a11y wiring is not something a call site can forget.
The prompt is `Dialog purpose="form"` — backdrop clicks stop dismissing once
you have typed, which is exactly the semantics a `window.prompt` replacement
needs.

**Popovers.** Nine of them, from Radix's compound shape to Astryx's
content-as-a-prop. One caught a real problem: **Astryx renders popover content
into the DOM whether or not it is open**, revealing it through the native
popover API rather than by mounting. Radix portalled it only when open. On a
200-row issue list, where every row carries five `Combobox` pickers, that is a
thousand cmdk instances nobody opened. `Picker` and `DatePicker` gate their
content on the open flag; the other seven are small enough not to care. The
same eager mount was what broke four unrelated test files with `ResizeObserver
is not defined` — gating fixed those too.

**Deleted:** `PopoverContent`, `Checkbox`, `Switch` from `primitives.tsx`, and
the `@radix-ui/react-popover`, `-checkbox`, `-switch` and `-alert-dialog`
dependencies. Still Radix: `dialog` (the create flows), `dropdown-menu`,
`context-menu`, `tooltip`.

## Three regressions the mapping caused, and where they were fixed

All three came from the same mistake: treating a lait decision as a call-site
detail when it was a system-wide one. All three were fixed in the theme, not at
call sites — which is the test of whether the theme is carrying its weight.

1. **Boxes instead of pills.** Astryx's button radius is `--radius-element`
   (8px). Ours is a pill at every size, and `primitives.tsx` said why: "a
   button carries no border of its own in the common variants, so its shape is
   whatever the fill describes — and a row of buttons has to agree: a ghost
   Cancel beside a primary Save cannot be a pill next to a box." Fixed as
   `button.base` / `iconbutton.base` `borderRadius: var(--radius-full)`, which
   also restores the 28px circular icon button.

2. **The selected state vanished.** `active` was first collapsed onto
   `secondary` with the difference carried in `elevation` alone — which is not
   a difference anyone can see. The current project tab rendered identically to
   the other three. It is now a real generated variant: the neutral ramp's
   `active` rung, the 0.5px edge ring, and deliberately no lift, because "a
   pressed control is set INTO the bar, and a shadow claiming it had risen off
   is the one thing that would make the pair read as two kinds of button."
   That rung is also the one with no home in Astryx's token vocabulary — its
   `--color-overlay-pressed` is an alpha composited over content, ours is an
   opaque surface — so the value is stated in the component override.

3. **A blue-tinted secondary.** Astryx fills `secondary` with
   `rgba(5, 54, 89, 0.1)`; ours is the page background with a whisper of lift.
   In a toolbar of grey chips the tint reads as a faintly coloured one.
   Overridden to `--color-background-body` with `--color-tint-hover` on hover.

4. **A double inset on every menu.** `.astryx-popover` ships `padding: 12px` —
   a reasonable default for a panel of prose. Ours are menus: `PopoverContent`
   had none, and every call site states its own (`p-2` on the pickers, `w-72
   p-2` on saved views) so a list runs edge to edge and a row's hover fill
   reaches the panel wall. Stacked, the two made a **20px** inset. Overridden
   to `padding: 0` in the theme, which puts it back to the 8px the call site
   asked for — one rule instead of ten `!important`s.

## Not everything pressable is a Button

The codemod's worst assumption. `<button>` in lait's source meant several
different things, and only one of them is what Astryx calls a `Button`:

- **an action with a label** — a Button. `Save`, `Delete`, `New issue`.
- **a menu row** — full-width, flat, left-aligned. That is `navigationItem()`.
- **a grid cell** — a calendar day. That is a 28px square.
- **a graphic** — the timeline's coloured bar. It is pressable and it is not a
  control.

Astryx's `Button` brings `padding: 8px 12px` at `size="sm"`, a min-height, a
focus offset and (after our theme) a pill radius. In a 26.6px calendar cell that
is **24px of horizontal padding around a one-digit label** — measured, and it
clipped every date in the month grid. The same padding made relation rows and
the timeline's project row taller and pill-shaped where they should be flat.

Reverted to real elements with lait's own recipes: `navigationItem()` for menu
rows, a bare `<button>` for the day cells and the timeline bar.

The tell, if it happens again: a `className` on a `Button` containing
`w-full`, `justify-start`, `flex-1`, `size-ctl-*` or `p-0`. That is a call site
saying "this is not shaped like a button". Where the class already neutralises
padding (`p-0`, `px-1`) Tailwind's `utilities` layer wins and the damage is
hidden — which is why only the calendar visibly broke.

## Radix, package by package

**`react-tooltip` — gone.** Astryx components carry their own `tooltip` prop
and need no provider, so `TooltipProvider` was vestigial: removed from `App.tsx`
and eight test files. `LabelDots` was the one real Radix tooltip and moved to
Astryx's `<Tooltip content>`.

**`react-dialog` — gone.** Six roots. Five were centred modals and became
`<Dialog isOpen onOpenChange width purpose="form">`; `Dialog.Portal`,
`Dialog.Overlay` and the fixed-position className all go, because the component
owns them. `Dialog.Close asChild` becomes an ordinary `onClick`.

The sixth is the mobile nav **drawer**, which is not a centred modal.
`DialogPosition` is `{start?, end?, top?, bottom?}`, so it is expressible:
`position={{ start: 0, top: 0, bottom: 0 }}` pins it to the inline start and
both block edges.

**`react-dropdown-menu` — gone.** Eight menus, a rewrite rather than a swap:
Astryx's menus are data-driven and lait's were compound with submenus.

The working pattern is **compound mode** — omit `items` entirely and pass
`DropdownMenuItem` children. The data form (`DropdownMenuItemData`) is
`{label, onClick?, isDisabled?, icon?, items?}` with no `endContent`, so
anything with a trailing column (a revision hash, a count, a missing-capability
note) has to be compound. The documented `children` render-prop is *not* an
escape hatch — it does not typecheck alongside `items`; the two modes are
exclusive. Triggers move into `button={{...}}` with `isIconOnly` and
`hasChevron={false}` for the `⋯` ones. Submenus are `DropdownMenuSubMenu`,
separators are `Divider`, and a destructive item wears its tone on the `label`
(a ReactNode) because Astryx's item has no destructive variant.

Two things it cost, both worth knowing:

- `label` on a Button is the visible text **and** the accessible name. Specs'
  lifecycle pill showed "Draft" but was named "Lifecycle: Draft" — that needs
  an explicit `aria-label` now, and it was a test that caught it.
- Sidebar's space menu had a composite trigger (mark, title, status dot).
  `icon` takes the mark and `label` the title, but `endContent` is typed to an
  Icon or a Badge, so the status dot moved *out* of the trigger to sit beside
  it. Arguably better — a status is not something you click.

**`react-context-menu` — gone too, and this was the interesting one.**

Astryx's `ContextMenu` renders `<div onContextMenu>{children}<span/></div>`. It
wraps its trigger and has no `asChild`. Our trigger is the issue row's `<li>`,
and a wrapper between the `<ul>` and its items breaks both the list semantics
screen readers navigate by AND the row's own flex layout, which the list owns.

`display: contents` answers both at once. The element generates no box, so the
row is laid out by the `<ul>` exactly as before, and it is **not exposed to the
accessibility tree**, so the row's a11y parent is the list again. One CSS rule,
scoped to `[data-issue-collection] > div` rather than written as a global `div`
escape hatch — it is a statement about these lists, not a licence.

Verified by reading the accessibility tree, not by inspecting the DOM:

```
list
 listitem          <- direct child; the wrapper is not in the tree
  "ENG-142" ...
 menu "Context menu"
```

One imperfection worth naming: the menu surface is a sibling of the rows inside
the list, so a `list` contains something that is not a `listitem`. That is
Astryx's structure and it is the lesser problem — the parent relationship the
rows depend on is intact.

The four submenus, two separators and nine items moved to compound mode:
`DropdownMenuSubMenu`, `Divider`, `DropdownMenuItem` with `endContent` for the
trailing check marks.

## Deliberately NOT migrated

Three primitives stay lait's, and the reason is the same each time — Astryx
would have to own something it has no way to know:

- **`Kbd`.** Ours renders a chord through `formatBinding(k, {glyphs: true})`
  from lait's keybinding registry. Astryx's `Kbd` takes a `keys` string in its
  own grammar and formats it itself. Two formatters, one output; ours is the
  one holding the bindings.
- **`LabelChip` / `LabelDots`.** Label colour comes from the space's catalog,
  per label. Astryx has nine categorical hues, which is a different idea.
- **`Avatar`.** Ours derives its colour and fallback from a device key.
- **`ui/icons.tsx`.** Priority bars and the status ring are data encoded as
  shape. No lucide or Astryx equivalent, by design.

## The className sweep, and why Tailwind stays

Measured before deciding: **3,795 class occurrences, 465 distinct**. Of those,
**74 across 40 sites sit on an Astryx component** — 2%. The rest are on lait's
own markup, and they are two things: layout (`flex`, `gap-2`, `min-w-0`,
`truncate`) and lait's own tokens (`size-icon-sm`, `text-mute`, `border-line`,
`rounded-surface`).

So "drop Tailwind" was the wrong goal and this file said so too early. Those
tokens ARE the design system — the pinned icon axis, the control ladder, the
mark sizes, the OKLCH ramp, a thousand lines of deliberate derivation in
`styles.css`. Removing Tailwind means re-expressing all of it in StyleX and
rewriting 3,700 call sites for no gain. Astryx's own migration guide asks only
to "remove legacy Tailwind classes from each completed surface, **keeping only
token-backed layout utilities**", and `tailwind-theme.css` exists precisely so
the two read one palette. They do.

What the sweep actually found, in the 74:

- Three `text-danger` on ghost Buttons — that IS the `danger` variant, which the
  theme already defines. The class was doing half its job.
- `rounded-full` on a Button — the theme gives every button a pill now.
- `size-ctl-md` on two IconButtons — an Astryx IconButton at `size="sm"`
  already measures 28px. Restating a default is a thing to chase when the
  ladder moves.
- `hover:bg-danger/10 hover:text-danger` on the bulk-bar delete — that pair is
  the `danger` variant spelled out.
- One `className=""`.

Everything else is legitimate: `ml-auto` and friends position a control in its
parent's flow, which is the call site's business, not the design system's.

Also removed: the `ui-overlay` and `ui-drawer` **exit animations**. They fired
on `[data-state="closed"]`, which only Radix ever wrote, and Radix no longer
drives a dialog or a drawer. `.ui-surface`'s exit stays — the issue row's
context menu still writes it.

## The open-menu highlight, and a lesson about measuring

A `Combobox` or `DatePicker` trigger darkens while its menu is open. That rode
on `data-[state=open]` — Radix's attribute, which Astryx does not write — so it
had been dead since the popover migration.

`controlTrigger` now takes an `open` variant, and both components already hold
that state in React, so the styling comes from us rather than from guessing at
whose attribute wins. The resting fill moved into a compound variant too, so a
trigger only ever carries one background class.

**The lesson is in how long that took.** `getComputedStyle` on the trigger
returned the resting colour no matter what — through a class, through an
`!important` class, through an inline style, and finally through a hard-coded
literal. A literal red also read back as the page background, which is not a
cascade phenomenon at all: the element is anchored to a top-layer popover, and
Chrome served a stale computed style for it.

Everything downstream of that was wasted: layer-order archaeology, a
`tailwind-merge` theory, an `!important` that made things worse. A single
zoomed screenshot of the trigger — closed, then open — settled it in one step
and showed the very first fix had worked.

**Verify a visual change by looking at it.** `getComputedStyle` is a fine tool
for tokens and geometry; it is not trustworthy for an element in or anchored to
the top layer.

## What is unresolved

1. **`astryx theme build` silently drops tokens (0.3.0).** Given 106 tokens it
   emitted 92 and reported "92 token overrides" while naming none of the 26 it
   discarded: all 15 `--spacing-*` and all 11 `--text-*-leading`. Those names
   are valid `TokenName`s — they come from `spacingDefaults` and
   `typeScaleDefaults` — so TypeScript accepts what the builder throws away.
   The emit allowlist is narrower than the type that gates it. Worth reporting
   upstream. Geometry lives in a layer partly because of this and partly
   because it belonged there anyway.
2. **`<Theme>` owns `data-theme` on `<html>`.** It syncs the attribute itself,
   so setting it externally is overwritten on mount. Scheme is driven through
   `<Theme mode>`. Same attribute we already use — so the mechanism is
   compatible, but ownership moves and `useTheme()` becomes the toggle.
3. **Status badges outweigh categorical ones.** Astryx's `success/warning/error`
   badge variants compose `--color-*` and `--color-*-muted` differently from the
   categorical variants, so they render solid while categorical render tinted.
   In light mode the `warning` badge is a brown fill that fails contrast.
   Fixable in `components`, not yet done.
4. **`--color-overlay-hover` / `-pressed` stayed Astryx's.** Those are alpha
   tints composited over content; our `hover`/`active` are opaque surface rungs.
   Putting an opaque colour in an overlay slot paints over content. `active`
   currently has no route into the theme.
5. **Astryx is prop-driven, not children-driven.** `Button` and `Badge` require
   `label`; `StatusDot` requires `label` for accessibility at the *type* level.
   This is the constrained-API property working as intended, and it means a
   migration is a rewrite of call sites, not a find-and-replace of imports.

## Versions

`@astryxdesign/core@0.3.0`, `@astryxdesign/cli@0.3.0`, `@stylexjs/stylex@0.19.0`,
React 19. Astryx is Beta with no semver policy and ~16 npm publishes a day.
