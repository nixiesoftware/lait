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
| blue | `--ds-accent` | the action | the one forward act on a surface; the sweep of a change taking; the selected clip; the playhead; what is held |
| green | `--ds-positive` | now | on air, a live value, a screen that is showing, a file on glass, the reach meter |
| red-orange | `--ds-alarm` | reach | an act that touches screens — never without its count beside it; a destructive act |
| ochre | `--ds-miss` | the miss | who a broadcast does not reach; a screen never heard from |

Nothing else in the interface is coloured. Everything else is ink at one of
three strengths.

A state is never an edge bar. A thing that is on air is lit by its own
colour the way every other object here is lit — a hairline where its contact
line was, a soft cast on the field beneath it (`--ds-material-onair`) — and
the pill on it says the word. Inside a card, the winning rung wears the same
faint tint it always did, and its mark carries the light. An app's mark (`.ds-app-mark`) is the one exception, and it
is the app's colour, not ours.

## Light, and only light

The shell and the editor set `color-scheme: light` and nothing reads
`prefers-color-scheme`. The one dark object on every page is a screen —
`--ds-panel-screen` — because a display panel is dark in every room. The
stage, every bezel, every thumbnail, and the kinds a program can carry
(Athan keeps its own night) sit on it.

## Relations are drawn, not captioned

Five things — files, programs, channels, broadcasts, screens — depend on each
other, and the dependence is shown by five rules rather than told by a label.

1. **Containment is composition.** A thing draws what it is made of inside
   itself. A program's cover holds its files; a channel's day holds its
   programs; a screen's bezel holds what it is showing. Never a count beside
   a name where the things themselves could be drawn.
2. **Height is precedence.** Above wins. The sky stack on a screen's page is
   ranked as resolution ranks it; a broadcast's priority is its height on the
   day; and the rail descends from the glass to the source — Screens,
   Broadcasts, Channels, Programs, Files — so the navigation is the ladder.
3. **Time is horizontal, and there is one of it.** The filmstrip, a channel's
   dayparts, a broadcast's window and a screen's day are one `DayTrack` with
   one playhead, in the action colour, moving on the shared tick.
4. **Light is state.** Whatever is on glass right now carries the green
   wherever it appears — the file, the program, the channel, the screen.
   Everything not lit is dim by degree: an unused file is faint, an untuned
   screen is a light empty horizon. Dimness is information, and unmeasured is
   absent — nothing reads as unused or dark before it has been read.
5. **Holding one lights its relations.** One focus for the whole app
   (`focus.tsx`). Hold a channel and the screens tuned to it light; hold a
   program and the files it holds lift. Hover holds on a pointer that can
   hover; a tap toggles on one that cannot. The held thing itself is neither
   lit nor dimmed.

## The instruments

| Primitive | Draws | Placed on |
| --- | --- | --- |
| `Console` | the screen and its chin, one body: the card wrapping the glass and the key row beneath it, from whose chin a pressed key's panel blooms upward over the glass | the horizon's hero |
| `Bezel` | a screen at four sizes, showing what it shows: the frame on it now, the Athan card with live times, dark on purpose, or the empty horizon. On air is a rim of alarm; never heard from is a rim of ochre. | Screens rows · the horizon's hero · a channel's attached screens · a program's showing-on |
| `DayTrack` | one day: the ground, the dayparts, the bands laid over, the playhead | a channel card · a screen's today · a broadcast's transit · a program's carried-by |
| `Footprint` | the fleet as tiny bezels: reached lit, missed outlined ochre, or neutral when no rule is asked | the composer · a transit · the audiences that include a screen · Broadcasts when nothing is on air |
| `SkyStack` (`.ds-sky`) | the claims on one screen, ranked; the rung that answers is lit | the horizon |
| `Readout` (`.ds-readout`) | a fact the page shows you, opened into an `Inspector` to change | the horizon's facts |
| `Focus` | one held thing | everywhere |

`useFleet` (`utils/screens/fleet.ts`) loads the seven lists once per page, so
the instruments on it cannot disagree with each other.

## One gesture, one outcome

Every change is on screen the frame it is made. `utils/screens/fleet.ts` holds
the seven lists once, outside React; a mutation is applied to that copy first
and written to the World after, and a refusal re-reads and is thrown back to
the surface that made it. A page that mounts draws what is already held —
"Loading…" is seen once per session, not once per click.

Nothing asks "are you sure". A removal is a press, and for eight seconds the
bar at the foot of the page (`undo.tsx`) offers to put it back; undo is a fresh
put of the document the remover kept. Nothing asks for a name first, either: a
new screen or channel exists on the press under a name the page hands it,
with that name selected, so the first keystroke is the rename.

A value is changed by pressing what it is: the tuning is a chip that opens a
`ChoiceMenu` whose pick is the commit; what a channel carries is a row of
covers; a daypart's hours are the controls that set them; a label is a chip
whose × removes it and whose "+ label" becomes the field. The composer is two
questions drawn as objects — what, as the programs; whom, as the fleet — and
one act in the colour of reach, wearing its count.

## The sheets, in load order

| File | Owns |
| --- | --- |
| `tokens.css` | CommunityKit's tokens. Do not edit; replace from upstream. |
| `signage.css` | alarm, miss, control heights, the alarm material, and every old name the sheets still speak, resolved to what it means now |
| `base.css` | the ground: body, element defaults, focus, selection, reduced motion |
| `controls.css` | button, icon button, input, chip, tag, hint |
| `language.css` | commit, on-air, ago, reach, panel, unit, field labels |
| `overlay.css` | popover, menu, dialog, sheet, toast, tile, row, choose |
| `page.css` | shell, rail, dock, page head, toolbar, catalogue, table, badge, find, devices, composer |
| `instruments.css` | bezel, day, footprint, sky, readout, horizon, transit, constellation, star, held/dim |

`program-editor/program-editor.css` and `surfaces.css` are the editor's and
follow the same rules.

## Rules that are measured

A walk over every surface at 1440 and 390 asserts them; a change that fails
one is the change that is wrong.

- No `text-transform: uppercase`. A label is a sentence at weight 500.
- No `font-weight` above 500 in chrome.
- Every button, chip, input and badge is a pill.
- Every screen surface is `#171a1d`.
- Unmeasured is absent, never zero: "not tuned", "never heard" and "unused"
  are states with their own words, never a blank, and never claimed before
  they have been read.
