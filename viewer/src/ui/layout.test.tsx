import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  Breadcrumbs,
  HeaderActions,
  HeaderActionsOutlet,
  HeaderSlotProvider,
  IssueCrumb,
  ProjectCrumb,
  SurfaceHeader,
  WorkspaceCrumb,
} from "./layout";
import { Combobox } from "./Picker";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

describe("Breadcrumbs", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("climbs from ancestors only, and drops a chevron with the crumb it belongs to", () => {
    const openWorkspace = vi.fn();
    const openProject = vi.fn();
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    act(() => {
      root?.render(
        <Breadcrumbs
          items={[
            {
              key: "workspace",
              label: "Nova",
              optional: true,
              content: <WorkspaceCrumb name="Nova" />,
              onNavigate: openWorkspace,
            },
            {
              key: "project",
              optional: true,
              content: <ProjectCrumb name="Engine" color="#f00" />,
              onNavigate: openProject,
            },
            { key: "issue", content: <IssueCrumb id="ENG-12" title="Ship it" /> },
          ]}
        />,
      );
    });

    const crumbs = [...host.querySelectorAll("li")];
    expect(crumbs).toHaveLength(3);

    // Every ancestor climbs; the leaf is where you already are.
    const links = [...host.querySelectorAll("button")];
    expect(links).toHaveLength(2);
    act(() => links[0]!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    act(() => links[1]!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    expect(openWorkspace).toHaveBeenCalledOnce();
    expect(openProject).toHaveBeenCalledOnce();

    // Exactly one "you are here", and it is the last crumb.
    const current = host.querySelectorAll('[aria-current="page"]');
    expect(current).toHaveLength(1);
    expect(current[0]?.textContent).toBe("ENG-12Ship it");

    // The separator lives inside the crumb *before* it, so an ancestor that
    // collapses on a narrow surface takes its chevron with it and the trail
    // never opens with a stray ›.
    expect(crumbs.map((li) => li.querySelectorAll("svg.lucide-chevron-right").length))
      .toEqual([1, 1, 0]);
    // Only ancestors may collapse.
    expect(crumbs.filter((li) => li.className.includes("hidden"))).toHaveLength(2);
  });

  it("resolves every group-hover variant against a group that actually wraps it", () => {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    act(() => {
      root?.render(
        <Breadcrumbs
          items={[
            {
              key: "project",
              control: true,
              content: (
                <Combobox
                  variant="crumb"
                  label="Project"
                  swatchShape="square"
                  value={{ id: "ENG", label: "Engine", swatch: "#f00" }}
                  options={[{ id: "ENG", label: "Engine", swatch: "#f00" }]}
                  onPick={() => undefined}
                />
              ),
            },
          ]}
        />,
      );
    });

    // A `group-hover/name:` utility whose `group/name` is nowhere above it is
    // silent: the class compiles, matches nothing, and the affordance never
    // arrives. The breadcrumb's project switcher shipped that way — it asked for
    // `group/prop`, which only exists on an issue property row.
    const orphans: string[] = [];
    for (const el of host.querySelectorAll<HTMLElement>("*")) {
      const wanted = [...el.classList]
        .map((c) => /^group-(?:hover|focus-within)\/([\w-]+):/.exec(c)?.[1])
        .filter((name): name is string => Boolean(name));
      for (const name of new Set(wanted)) {
        let carrier: HTMLElement | null = el.parentElement;
        while (carrier && carrier !== host && !carrier.classList.contains(`group/${name}`)) {
          carrier = carrier.parentElement;
        }
        if (!carrier || carrier === host) orphans.push(`${el.tagName}: group/${name}`);
      }
    }
    expect(orphans).toEqual([]);
  });

  it("lets a view fill the shell's actions slot without remounting the header", () => {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);

    const shell = (view: "list" | "issue") => (
      <HeaderSlotProvider>
        <SurfaceHeader
          leading={<button type="button">toggle</button>}
          trail={<Breadcrumbs items={[{ key: "p", content: <ProjectCrumb name="Engine" /> }]} />}
          actions={view === "issue" ? <HeaderActionsOutlet /> : <button type="button">New</button>}
        />
        {view === "issue" && (
          <HeaderActions>
            <button type="button">Close issue</button>
          </HeaderActions>
        )}
      </HeaderSlotProvider>
    );

    act(() => root?.render(shell("list")));
    const header = host.querySelector("header");
    expect(header?.textContent).toContain("New");

    // Switching views must not swap the bar out from under the user — same node,
    // different tenants. A remount is what made the old issue header arrive with
    // its own inset and no sidebar toggle.
    act(() => root?.render(shell("issue")));
    expect(host.querySelector("header")).toBe(header);
    expect(host.querySelectorAll("header")).toHaveLength(1);
    expect(header?.textContent).toContain("Close issue");
    expect(header?.textContent).not.toContain("New");
  });
});
