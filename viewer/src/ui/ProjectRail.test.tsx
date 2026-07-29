import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MilestoneDto, ProjectDto } from "../types";
import { WorldViewStoreProvider } from "../core/worldViewReact";
import { ProjectViewerStore, ProjectViewerStoreProvider } from "../projectStore";
import { ProjectRail } from "./ProjectRail";
import { TooltipProvider } from "./primitives";

const rpcMock = vi.hoisted(() => vi.fn());
vi.mock("../api", () => ({ rpc: rpcMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

const project: ProjectDto = {
  id: "prj_test",
  name: "Platform",
  key: "PLAT",
  color: "blue",
};

/** One of each glyph state, so a render exercises all three derivations. */
const MILESTONES: MilestoneDto[] = [
  { id: "mls_0", name: "M0 — Environment", total: 2, done: 1 },
  { id: "mls_1", name: "M1 — Catalog", total: 3, done: 3 },
  { id: "mls_2", name: "M2 — Identity", total: 1, done: 0 },
];

describe("ProjectRail", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
    rpcMock.mockReset();
  });

  async function render(
    activeMilestone: string | null,
    onOpenMilestone: (m: string | null) => void = vi.fn(),
  ) {
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      if (request.cmd === "milestone_list") {
        return Promise.resolve({ kind: "milestones", milestones: MILESTONES });
      }
      throw new Error(`Unexpected request: ${request.cmd}`);
    });

    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    const store = new ProjectViewerStore(rpcMock);

    await act(async () => {
      root?.render(
        <WorldViewStoreProvider store={store.resources}>
          <ProjectViewerStoreProvider store={store}>
            <TooltipProvider>
              <ProjectRail
                spaceId="local"
                project={project}
                members={[]}
                counts={{ backlog: 2, active: 1, done: 4, total: 7 }}
                readOnly={false}
                activeMilestone={activeMilestone}
                onError={vi.fn()}
                onOpenMilestone={onOpenMilestone}
              />
            </TooltipProvider>
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  const row = (name: string) =>
    [...(host?.querySelectorAll("button") ?? [])].find((b) =>
      b.getAttribute("aria-label")?.includes(name),
    );

  it("draws each milestone's state from its counts, never from a stored field", async () => {
    await render(null);
    // 1/2, 3/3 and 0/1 — the three glyphs, plus the bucket's dotted diamond.
    const glyphs = [...host!.querySelectorAll("svg[role='img']")].map((g) =>
      g.getAttribute("aria-label"),
    );
    expect(glyphs).toEqual(["In progress", "Complete", "Not started", "No milestone"]);
    expect(host!.textContent).toContain("50% of 2");
    expect(host!.textContent).toContain("100% of 3");
  });

  /**
   * The state the shell exists for. A row that is already scoping the issue
   * list has said what it counts; what you need from it next is the way out.
   */
  it("swaps the active row's numbers for the way out", async () => {
    await render("mls_1");

    expect(host!.textContent).toContain("Clear filter");
    // Its own numbers step aside — the others keep theirs.
    expect(host!.textContent).not.toContain("100% of 3");
    expect(host!.textContent).toContain("50% of 2");

    // And the label says what the click will do, not where it will go.
    expect(row("Clear the M1 — Catalog filter")).toBeTruthy();
    expect(row("Show issues in M0 — Environment")).toBeTruthy();
  });

  it("dims the milestones that are not scoping, and only then", async () => {
    await render(null);
    const dimmed = () =>
      [...host!.querySelectorAll("li")].filter((li) => li.className.includes("opacity-45")).length;
    // Nothing scoped: nothing recedes.
    expect(dimmed()).toBe(0);

    await act(async () => root?.unmount());
    host?.remove();
    await render("mls_1");
    // The two siblings recede; the active row does not. They stay visible
    // rather than vanishing — which milestone is scoping only reads against
    // the ones that are not.
    expect(dimmed()).toBe(2);
    expect(host!.textContent).toContain("M0 — Environment");
    expect(host!.textContent).toContain("M2 — Identity");
  });

  it("scopes on click and clears when the scoped row is clicked again", async () => {
    const onOpen = vi.fn();
    await render(null, onOpen);
    act(() => row("Show issues in M2 — Identity")?.click());
    expect(onOpen).toHaveBeenLastCalledWith("mls_2");

    await act(async () => root?.unmount());
    host?.remove();
    await render("mls_2", onOpen);
    // Clicking the row you are inside is how you get back out, and it is the
    // same click that got you in.
    act(() => row("Clear the M2 — Identity filter")?.click());
    expect(onOpen).toHaveBeenLastCalledWith(null);
  });

  it("keeps the No-milestone bucket distinct from no filter at all", async () => {
    const onOpen = vi.fn();
    await render(null, onOpen);
    const bucket = () =>
      [...(host?.querySelectorAll("button") ?? [])].find((b) =>
        b.textContent?.startsWith("No milestone"),
      );
    // `""`, never `null`: the bucket is a real selection — the issues nobody
    // has scoped yet — and it is the one cut a per-milestone list cannot
    // otherwise reach.
    act(() => bucket()?.click());
    expect(onOpen).toHaveBeenLastCalledWith("");

    await act(async () => root?.unmount());
    host?.remove();
    await render("", onOpen);
    expect(bucket()?.textContent).toContain("Clear filter");
    act(() => bucket()?.click());
    expect(onOpen).toHaveBeenLastCalledWith(null);
  });

  it("offers a move only where it works", async () => {
    await render(null);
    // Radix opens on `pointerdown`, not `click` — a bare `.click()` here would
    // pass through the trigger and assert against an empty menu.
    const menuFor = async (name: string) => {
      const trigger = row(`Milestone actions for ${name}`);
      await act(async () => {
        trigger?.dispatchEvent(
          new MouseEvent("pointerdown", { bubbles: true, button: 0, detail: 1 }),
        );
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
      const items = [...document.querySelectorAll('[role="menuitem"]')].map((i) => i.textContent);
      await act(async () => {
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
      return items;
    };
    // The ends of the list have one direction each, the middle has both: a verb
    // offered where it cannot act is a verb you learn not to trust.
    expect(await menuFor("M0 — Environment")).toEqual(["Move down", "Remove milestone"]);
    expect(await menuFor("M1 — Catalog")).toEqual([
      "Move up",
      "Move down",
      "Remove milestone",
    ]);
    expect(await menuFor("M2 — Identity")).toEqual(["Move up", "Remove milestone"]);
  });
});
