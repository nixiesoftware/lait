import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ProjectDto, SpaceRow } from "../types";
import { Sidebar } from "./Sidebar";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

describe("Sidebar navigation", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("keeps workspace destinations global and offers projects without their faces", () => {
    const onGo = vi.fn();
    const onMyIssues = vi.fn();
    const onOpenProjectView = vi.fn();
    const onSearch = vi.fn();
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    act(() => {
      root?.render(
        <Sidebar
          spaces={[space]}
          current={space.id}
          projects={[project]}
          currentProject={project.key}
          view="list"
          unread={3}
          favoriteProjects={[]}
          savedViews={[]}
          onPickSpace={vi.fn()}
          onSearch={onSearch}
          onOpenProjectView={onOpenProjectView}
          onGo={onGo}
          teams={[]}
          currentTeam={null}
          onGoTeam={vi.fn()}
          onMyIssues={onMyIssues}
          onApplySavedView={vi.fn()}
          onToggleFavorite={vi.fn()}
          onCreateProject={vi.fn()}
          onAddSpace={vi.fn()}
          onForgetSpace={vi.fn()}
          onPruneSpaces={vi.fn()}
        />      );
    });

    click("Projects");
    expect(onGo).toHaveBeenCalledWith("projects");
    click("My issues");
    expect(onMyIssues).toHaveBeenCalledOnce();
    click("Search issues");
    expect(onSearch).toHaveBeenCalledOnce();
    expect(host.textContent).toContain("3");

    // The tree offers PROJECTS, never their faces. It used to hang Overview,
    // Issues and Activity off each project, from before the project shell had a
    // tab strip you could see — now it has one, always, so the tree was drawing
    // the same three choices a pane away with two rows claiming to be current.
    // The nav answers "which project"; the strip answers "which face".
    const labels = [...host.querySelectorAll("button")].map((b) => b.textContent);
    // Roadmap is in this list now: the workspace chart was withdrawn with the
    // rest of the timeline views, so the nav must not still offer it.
    for (const face of ["Overview", "Activity", "Board", "Calendar", "Roadmap"]) {
      expect(labels, `the tree must not offer ${face} — the project strip does`)
        .not.toContain(face);
    }

    // The name opens the project, naming the face it lands on: there is one
    // navigation verb and the destination is its argument, not its identity.
    const name = [...host.querySelectorAll("button")].find((b) => b.title?.startsWith("Web ·"));
    expect(name?.querySelector("svg.lucide-box")).not.toBeNull();
    act(() => name?.click());
    expect(onOpenProjectView).toHaveBeenLastCalledWith("WEB", "list");

    // Exactly one row is "where you are" — the project, now that it has no
    // children to hand the highlight down to.
    expect(host.querySelectorAll('[aria-current="page"]')).toHaveLength(1);
  });

  it("scrolls the complete navigation body rather than only the project tail", () => {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    act(() => {
      root?.render(
        <Sidebar
          spaces={[space]}
          current={space.id}
          projects={[project]}
          currentProject={null}
          view="inbox"
          unread={0}
          favoriteProjects={[]}
          savedViews={[]}
          onPickSpace={vi.fn()}
          onSearch={vi.fn()}
          onOpenProjectView={vi.fn()}
          onGo={vi.fn()}
          teams={[]}
          currentTeam={null}
          onGoTeam={vi.fn()}
          onMyIssues={vi.fn()}
          onApplySavedView={vi.fn()}
          onToggleFavorite={vi.fn()}
          onCreateProject={vi.fn()}
          onAddSpace={vi.fn()}
          onForgetSpace={vi.fn()}
          onPruneSpaces={vi.fn()}
        />,
      );
    });

    const nav = host.querySelector<HTMLElement>('nav[aria-label="Workspace"]')!;
    const body = nav.children[1] as HTMLElement;
    expect(body.classList).toContain("min-h-0");
    expect(body.classList).toContain("flex-1");
    expect(body.classList).toContain("overflow-y-auto");
    expect(body.textContent).toContain("Inbox");
    expect(body.textContent).toContain("Workspace");
    expect(body.textContent).toContain("Projects");
    expect(body.lastElementChild?.classList).not.toContain("overflow-y-auto");
  });

  it("puts the space's verbs in a menu rather than listing every replica", () => {
    const onGo = vi.fn();
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    act(() => {
      root?.render(
        <Sidebar
          spaces={[space]}
          current={space.id}
          projects={[project]}
          currentProject={null}
          view="inbox"
          unread={0}
          favoriteProjects={[]}
          savedViews={[]}
          onPickSpace={vi.fn()}
          onSearch={vi.fn()}
          onOpenProjectView={vi.fn()}
          onGo={onGo}
          teams={[]}
          currentTeam={null}
          onGoTeam={vi.fn()}
          onMyIssues={vi.fn()}
          onApplySavedView={vi.fn()}
          onToggleFavorite={vi.fn()}
          onCreateProject={vi.fn()}
          onAddSpace={vi.fn()}
          onForgetSpace={vi.fn()}
          onPruneSpaces={vi.fn()}
        />      );
    });

    // Closed, it says nothing but the space it is on: the old `<details>` kept
    // the whole replica list — and "Workspace settings" below it — in the DOM.
    expect(host.querySelector("details")).toBeNull();
    // Astryx keeps the menu's items in the DOM and reveals them through the
    // native popover API, so "closed" is no longer "absent". What still has to
    // be true is that nothing leaks onto the sidebar itself.
    const chrome = host.cloneNode(true) as HTMLElement;
    chrome.querySelectorAll("[popover]").forEach((el) => el.remove());
    expect(chrome.textContent).not.toContain("Workspace settings");

    const trigger = host.querySelector<HTMLElement>('[aria-label="Space menu"]');
    expect(trigger).toBeTruthy();
    expect(trigger?.getAttribute("aria-haspopup")).toBe("menu");
  });

  /**
   * Adding a space lives here and nowhere else.
   *
   * It used to be reachable only from the empty state, which a selected space
   * replaces — so with one space open there was no way to found a second or, far
   * worse, to paste an invite to one. This is the regression test for that: the
   * menu is on screen whatever you have open, so the verb has to be on it.
   */
  it("offers adding a space, and prunes only when a row is missing", () => {
    const onAddSpace = vi.fn();
    const onForgetSpace = vi.fn();
    const onPruneSpaces = vi.fn();
    // A fresh mount per scenario. Reusing one root leaves the menu's own
    // open/closed state (and the modal pointer-events lock) behind from the
    // previous item click, and the next open silently toggles back shut.
    const render = (rows: SpaceRow[]) => {
      if (root) act(() => root?.unmount());
      host?.remove();
      host = document.createElement("div");
      document.body.append(host);
      root = createRoot(host);
      act(() => {
        root?.render(
          <Sidebar
            spaces={rows}
            current={space.id}
            projects={[project]}
            currentProject={null}
            view="inbox"
            unread={0}
            favoriteProjects={[]}
            savedViews={[]}
            onPickSpace={vi.fn()}
            onSearch={vi.fn()}
            onOpenProjectView={vi.fn()}
            onGo={vi.fn()}
            teams={[]}
            currentTeam={null}
            onGoTeam={vi.fn()}
            onMyIssues={vi.fn()}
            onApplySavedView={vi.fn()}
            onToggleFavorite={vi.fn()}
            onCreateProject={vi.fn()}
            onAddSpace={onAddSpace}
            onForgetSpace={onForgetSpace}
            onPruneSpaces={onPruneSpaces}
          />        );
      });
      openSpaceMenu();
    };

    render([space]);
    menuItem("Add space").click();
    expect(onAddSpace).toHaveBeenCalledOnce();

    // One entry, not two: founding and entering are the same errand, and the
    // surface behind it asks which with its own tab strip.
    render([space]);
    expect(menuLabels()).not.toContain("Found a space");
    expect(menuLabels()).not.toContain("Use an invite");

    // Forgetting names the row it acts on, so the caller never has to guess
    // which space a menu on a shared trigger meant.
    render([space]);
    menuItem("Forget this space").click();
    expect(onForgetSpace).toHaveBeenCalledWith(space.id);

    // No missing row, nothing to prune — an always-present "remove unavailable"
    // reads as a delete for the spaces that are fine.
    render([space]);
    // Anchored against an open menu, or the absence below proves only that
    // nothing rendered at all.
    expect(menuLabels()).toContain("Add space");
    expect(menuLabels()).not.toContain("Remove 1 unavailable space");

    render([space, { ...space, id: "gone-hash", space: "ws_gone", name: "Gone", status: "missing" }]);
    menuItem("Remove 1 unavailable space").click();
    expect(onPruneSpaces).toHaveBeenCalledOnce();
  });

  /**
   * Radix opens on `pointerdown`, not `click` — a bare `.click()` does nothing.
   * A `MouseEvent` typed `pointerdown` because jsdom ships no `PointerEvent`;
   * the trigger reads only `button` and `ctrlKey`, which this carries.
   */
  function openSpaceMenu() {
    const trigger = host!.querySelector<HTMLElement>('[aria-label="Space menu"]')!;
    act(() => {
      trigger.dispatchEvent(
        new MouseEvent("pointerdown", { bubbles: true, button: 0, ctrlKey: false }),
      );
    });
  }

  /** Menu items render in a portal, so they are on `document`, not in `host`. */
  function menuLabels(): string[] {
    return [...document.querySelectorAll('[role="menuitem"]')].map((item) =>
      (item.textContent ?? "").trim(),
    );
  }

  function menuItem(label: string): { click: () => void } {
    const item = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(
      (candidate) => (candidate.textContent ?? "").trim() === label,
    );
    expect(item, `menu item "${label}" — found ${JSON.stringify(menuLabels())}`).toBeTruthy();
    return { click: () => act(() => item?.click()) };
  }

  function click(label: string) {
    const button = [...host!.querySelectorAll("button")].find(
      (item) => item.textContent?.includes(label) || item.getAttribute("aria-label") === label,
    );
    expect(button).toBeTruthy();
    act(() => button?.click());
  }
});

const space: SpaceRow = {
  id: "local-hash",
  space: "ws_test",
  name: "Test space",
  path: "C:/test",
  origin: "test",
  last_opened: 0,
  status: "up",
  identity: { kind: "own" },
  projects: [],
};

const project: ProjectDto = {
  id: "prj_test",
  key: "WEB",
  name: "Web",
  color: "blue",
};
