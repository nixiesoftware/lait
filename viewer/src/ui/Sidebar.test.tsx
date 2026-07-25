import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ProjectDto, SpaceRow } from "../types";
import { Sidebar } from "./Sidebar";
import { TooltipProvider } from "./primitives";

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

  it("keeps workspace destinations global and reaches a project's faces from the tree", () => {
    const onGo = vi.fn();
    const onMyIssues = vi.fn();
    const onOpenProjectView = vi.fn();
    const onSearch = vi.fn();
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    act(() => {
      root?.render(
        <TooltipProvider><Sidebar
          spaces={[space]}
          current={space.id}
          projects={[project]}
          currentProject={project.key}
          view="list"
          unread={3}
          favoriteProjects={[]}
          savedViews={[]}
          issueLayout="list"
          onPickSpace={vi.fn()}
          onSearch={onSearch}
          onOpenProjectView={onOpenProjectView}
          onGo={onGo}
          onMyIssues={onMyIssues}
          onApplySavedView={vi.fn()}
          onToggleFavorite={vi.fn()}
          onCreateProject={vi.fn()}
        /></TooltipProvider>,
      );
    });

    click("Projects");
    expect(onGo).toHaveBeenCalledWith("projects");
    click("Roadmap");
    expect(onGo).toHaveBeenCalledWith("timeline");
    click("My issues");
    expect(onMyIssues).toHaveBeenCalledOnce();
    click("Search issues");
    expect(onSearch).toHaveBeenCalledOnce();
    expect(host.textContent).toContain("3");

    // The project you are in opens itself, so its faces are reachable without
    // first landing on it and reading a tab strip.
    click("Activity");
    expect(onOpenProjectView).toHaveBeenCalledWith("WEB", "activity");
    click("Overview");
    expect(onOpenProjectView).toHaveBeenCalledWith("WEB", "overview");

    // Board and Calendar are layouts of Issues, not siblings of it — the tree
    // offers the destination once and the display popover picks the drawing.
    expect([...host.querySelectorAll("button")].map((b) => b.textContent))
      .not.toContain("Calendar");
    click("Issues");
    expect(onOpenProjectView).toHaveBeenCalledWith("WEB", "list");

    // The name still goes to the project itself — the chevron is what reveals —
    // and it names the face it lands on, because there is one navigation verb
    // and the destination is its argument, not its identity.
    const name = [...host.querySelectorAll("button")].find((b) => b.title?.startsWith("Web ·"));
    act(() => name?.click());
    expect(onOpenProjectView).toHaveBeenLastCalledWith("WEB", "list");

    // Exactly one row is "where you are": the face, not the face *and* its
    // project, which drew a two-row block and claimed two current pages.
    expect(host.querySelectorAll('[aria-current="page"]')).toHaveLength(1);
  });

  it("puts the space's verbs in a menu rather than listing every replica", () => {
    const onGo = vi.fn();
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    act(() => {
      root?.render(
        <TooltipProvider><Sidebar
          spaces={[space]}
          current={space.id}
          projects={[project]}
          currentProject={null}
          view="inbox"
          unread={0}
          favoriteProjects={[]}
          savedViews={[]}
          issueLayout="list"
          onPickSpace={vi.fn()}
          onSearch={vi.fn()}
          onOpenProjectView={vi.fn()}
          onGo={onGo}
          onMyIssues={vi.fn()}
          onApplySavedView={vi.fn()}
          onToggleFavorite={vi.fn()}
          onCreateProject={vi.fn()}
        /></TooltipProvider>,
      );
    });

    // Closed, it says nothing but the space it is on: the old `<details>` kept
    // the whole replica list — and "Workspace settings" below it — in the DOM.
    expect(host.querySelector("details")).toBeNull();
    expect(host.textContent).not.toContain("Workspace settings");

    const trigger = host.querySelector<HTMLElement>('[aria-label="Space menu"]');
    expect(trigger).toBeTruthy();
    expect(trigger?.getAttribute("aria-haspopup")).toBe("menu");
  });

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
