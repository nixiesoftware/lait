import { PROJECT_VIEW_LABEL, type ProjectView } from "../core/registry";
import { Button } from "./primitives";

/** The faces a project offers as tabs. Board and Calendar are absent on purpose:
 *  they are LAYOUTS of Issues, chosen beside grouping and ordering, so they live
 *  behind the display control rather than as destinations of their own. */
const TABS: readonly ProjectView[] = ["overview", "activity", "list"];

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
 * The pills are the toolbar's own — the same `Button` at the same rung the
 * status slices use, one rule apart. They sit in one band beside 28px icon
 * circles, and a tab drawn any other way reads as a different class of control
 * on the same bar rather than the same control choosing a different thing.
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
        // Board and Calendar are Issues wearing a different layout, so the
        // Issues tab stays lit under all three. Letting it go dark on a board
        // would say you had left the issues, when you are looking at them.
        const current = tab === "list" ? view !== "overview" && view !== "activity" : view === tab;
        return (
          <Button
            key={tab}
            size="md"
            variant={current ? "active" : "outline"}
            aria-current={current ? "page" : undefined}
            onClick={() => onPick(tab)}
          >
            {PROJECT_VIEW_LABEL[tab]}
          </Button>
        );
      })}
    </nav>
  );
}
