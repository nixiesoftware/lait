import { isIssueMode, isSpecMode, PROJECT_VIEW_LABEL, type ProjectView } from "../core/registry";
import { Button } from "@astryxdesign/core";
import { cn, toolbarControl } from "./primitives";

/** The faces a project offers as tabs. Board and Calendar are absent on purpose:
 *  they are LAYOUTS of Issues, chosen beside grouping and ordering, so they live
 *  behind the display control rather than as destinations of their own.
 *
 *  Specs earns a tab by the same rule that denies Board one: it is a different
 *  NOUN with its own query, not another drawing of the issues. It sits beside
 *  Issues because that is what it is beside — the work, and what the work is
 *  meant to satisfy.
 *
 *  Timeline briefly had one, on the argument that it answers a different
 *  question from Issues — what order the work happens in, rather than which
 *  work exists. True, and still not a tab: it answers that question about the
 *  *plan*, and Specs is already the tab for the plan. So it is a layout under
 *  Specs, and this strip is back to the four faces a project coordinates
 *  between. A fifth pill for a drawing of one of the other four is the thing
 *  the Board is denied a tab for. */
const TABS: readonly ProjectView[] = ["overview", "activity", "list", "specs"];

/**
 * The project's tab strip.
 *
 * This is a reversal, and the argument it reverses is worth keeping: a strip
 * under the header used to be a second switcher for a choice the sidebar's
 * project tree had already made, and the trail then had to stay silent about the
 * view to avoid saying "Issues" twice.
 *
 * What changed is the rail. A project now carries a console — properties,
 * milestones, progress — that persists across its faces, and clicking a
 * milestone narrows the issue list without leaving it. That makes the project a
 * place you are inside rather than a folder you picked from a tree, and a place
 * you are inside needs its own switcher: the sidebar can express "this project"
 * but not "this project, Issues, scoped to M1, console open". The strip does, so
 * the strip has it back and the trail stops at the project.
 *
 * The pills are the toolbar's own — the same `Button`, the same
 * `variant`/`elevation` pairing, and the same SIZE RUNG as everything else on
 * the bar. A tab drawn any other way reads as a different class of control on
 * the same bar rather than the same control choosing a different thing.
 *
 * That rung is `sm`, which is what the filter icon and the display controls at
 * the tail already are. The tabs were `md` — the one thing on the band a step
 * up from its neighbours, which is precisely the "different class of control"
 * this paragraph exists to forbid. Shrinking them was the rule catching up with
 * itself, not a new opinion about size.
 */
export function ProjectTabs({
  view,
  onPick,
}: {
  view: ProjectView;
  onPick: (view: ProjectView) => void;
}) {
  return (
    <nav aria-label="Project views" className="flex shrink-0 items-center gap-1">
      {TABS.map((tab) => {
        // A board is Issues wearing a different layout and a timeline is the
        // plan wearing one, so each tab stays lit under all of its own layouts.
        // Letting Issues go dark on a board would say you had left the issues
        // when you are looking at them, and the same is true of the timeline
        // and the plan it draws.
        const current =
          tab === "list" ? isIssueMode(view) : tab === "specs" ? isSpecMode(view) : view === tab;
        return (
          <Button
            key={tab}
            aria-current={current ? "page" : undefined}
            onClick={() => onPick(tab)}
            label={`${PROJECT_VIEW_LABEL[tab]}`}
            // Both states are CHIPS, and that part stays: a ghost resting
            // state was tried and removed because it read as text and the
            // strip lost its footing on the band. The edge is doing real work —
            // it is what says these four are controls rather than a row of
            // words.
            //
            // What did not work was leaving the *whole* difference to the fill.
            // Measured, the two states were `oklch(0.282)` against
            // `oklch(0.228)`: five hundredths of lightness, on chips whose text
            // was the same colour at the same weight. Four tabs that look
            // alike, with the current one identifiable only by staring.
            //
            // So the label carries it now, which is the channel that was going
            // spare. `bright` against `dim` is most of the neutral ramp —
            // unmistakable at a glance, in both themes — and the semibold is
            // what a tab strip in every other product uses to say which one you
            // are on. The fill difference stays underneath doing what it can;
            // it is no longer being asked to do all of it.
            variant={current ? "active" : "secondary"}
            elevation={current ? "none" : "low"}
            size="sm"
            // And a real border, which the strip never had. The comment above
            // used to credit "the edge" for saying these are controls — but
            // measured, `border-width` was `0px` on all four and the edge was a
            // *shadow*: an inset hairline on the current tab, a drop shadow on
            // the rest. A drop shadow is not an outline, and on a dark ground
            // it is very nearly nothing.
            className={cn(
              toolbarControl,
              // Taller and narrower. The vertical is a *rung* move rather than
              // a padding one: the height is pinned by `h-ctl-*` and the box is
              // border-box, so adding padding to a 24px chip only squeezes the
              // label — `md` (28px) is what four more pixels of vertical
              // actually means here.
              //
              // Which reverses a rule this file used to state: the tabs were
              // dropped to `sm` precisely so they matched the filter and
              // display controls at the tail of the same band. They are a step
              // above their neighbours again, deliberately — a tab strip is the
              // band's subject and the tail controls are its instruments, and
              // asking them to be the same size was treating a hierarchy as a
              // symmetry.
              "!h-ctl-md !px-2.5 !py-2.5",
              "!border",
              // Weight is deliberately NOT one of the channels.
              //
              // Semibold on the current tab was the obvious third cue and it
              // moved the strip: measured, the same label is 0.4–1.1px wider at
              // 600 than at 500, so selecting a tab widened it and nudged every
              // tab to its right. A control that changes size when you choose
              // it is a control you cannot click twice in the same place.
              //
              // Reserving the bold width would need a hidden copy of the label
              // inside a component that renders its own label, and colour is
              // already carrying this at full strength — `bright` against `dim`
              // is most of the neutral ramp, on top of a fill and a border that
              // both change too. Three channels is enough without the one that
              // reflows.
              current
                ? "!border-line-strong !text-bright"
                : "!border-line !text-dim hover:!text-fg hover:!border-line-strong",
            )}
          />
        );
      })}
    </nav>
  );
}
