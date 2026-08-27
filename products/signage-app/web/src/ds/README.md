# The Signage language

Two systems, one above the other.

**The visual system is CommunityKit's.** `tokens.css` is
`communitykit/src/design-system/tokens.css`, byte for byte, so the two
products cannot drift on a value. Its four commitments hold here unchanged:
one quiet field, real material, one voice, one accent. Read that README
before this one; nothing below overrides it.

**The behavioural system is this product's.** There is no Save button. A
change shows it took on the field it changed. Live values wear a colour
nothing pressable wears. A broadcast states who it misses before it sends.
`signage.css` is the layer that gives those rules the vocabulary the site
never needed.

## Four meanings, four hues, one job each

| Hue | Token | Means | Appears on |
| --- | --- | --- | --- |
| blue | `--ds-accent` | the action | the one forward act on a surface; the sweep of a change taking; the selected clip; the playhead |
| green | `--ds-positive` | now | on air, a live value, a screen that is showing, the reach meter |
| red-orange | `--ds-alarm` | reach | an act that touches screens — never without its count beside it; a destructive act |
| ochre | `--ds-miss` | the miss | who a broadcast does not reach; a screen never heard from |

Nothing else in the interface is coloured. Everything else is ink at one of
three strengths. An app's mark (`.ds-app-mark`) is the one exception, and it
is the app's colour, not ours.

## Light, and only light

The shell and the editor set `color-scheme: light` and nothing reads
`prefers-color-scheme`. The one dark object on every page is a screen —
`--ds-panel-screen` — because a display panel is dark in every room. The
stage, every bezel, every thumbnail, and the kinds a program can carry
(Athan keeps its own night) sit on it.

## The sheets, in load order

| File | Owns |
| --- | --- |
| `tokens.css` | CommunityKit's tokens. Do not edit; replace from upstream. |
| `signage.css` | alarm, miss, control heights, the alarm material, and every old name the sheets still speak, resolved to what it means now |
| `base.css` | the ground: body, element defaults, focus, selection, reduced motion |
| `controls.css` | button, icon button, input, chip, tag, hint |
| `language.css` | commit, on-air, ago, reach, panel, unit, field labels |
| `overlay.css` | popover, menu, dialog, sheet, toast, tile, row, choose |
| `page.css` | shell, rail, dock, page head, toolbar, catalogue, table, badge, find, devices, bezel, composer |

`program-editor/program-editor.css` and `surfaces.css` are the editor's and
follow the same rules.

## Rules that are measured

A walk over every surface at 1440 and 390 asserts them; a change that fails
one is the change that is wrong.

- No `text-transform: uppercase`. A label is a sentence at weight 500.
- No `font-weight` above 500 in chrome.
- Every button, chip, input and badge is a pill.
- Every screen surface is `#171a1d`.
- Unmeasured is absent, never zero: "not tuned" and "never heard" are states
  with their own words, never a blank.
