import { act } from "react";
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { RowGroup } from "../core/display";
import type { Row, WorkflowState } from "../types";
import { IssueList } from "./IssueList";
import { TooltipProvider } from "./primitives";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;
Element.prototype.scrollIntoView = vi.fn();

describe("IssueList semantics", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("separates current issue semantics from bulk checkboxes and opens with click or Enter", () => {
    const onOpen = vi.fn();
    const current = row("LIST-1");
    render(current, onOpen);

    const item = host!.querySelector("li[aria-current=true]") as HTMLLIElement;
    expect(item.tabIndex).toBe(0);
    expect(host!.querySelector('[role="listbox"], [role="option"]')).toBeNull();
    expect(host!.querySelector('[role="checkbox"][aria-label="Select LIST-1"]')).toBeTruthy();
    act(() => item.click());
    expect(onOpen).toHaveBeenLastCalledWith(current.reff);
    onOpen.mockClear();
    act(() => item.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    expect(onOpen).toHaveBeenCalledWith(current.reff);
  });

  it("extends bulk selection across a shift-clicked range", () => {
    const toggled = vi.fn();
    const rows = [row("LIST-1"), row("LIST-2"), row("LIST-3")];
    const state: WorkflowState = { id: "backlog", name: "Backlog", category: "backlog", color: "gray" };
    function Harness() {
      const [checked, setChecked] = useState<ReadonlySet<string>>(new Set());
      return (
        <IssueList
          groups={[{ key: "backlog", kind: "status", label: "Backlog", rows, state }]}
          deleted={[]}
          deletedMode={false}
          states={[state]}
          members={[]}
          labels={[]}
          selection={rows[0]!.reff}
          checked={checked}
          optimistic={new Set()}
          onSelect={() => undefined}
          onToggleCheck={(reff) => {
            toggled(reff);
            setChecked((current) => {
              const next = new Set(current);
              if (!next.delete(reff)) next.add(reff);
              return next;
            });
          }}
          onOpen={() => undefined}
          onCreate={() => undefined}
          readOnly={false}
          filtered={false}
        />
      );
    }
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root?.render(
      <TooltipProvider>
        <Harness />
      </TooltipProvider>,
    ));

    const checks = [...host.querySelectorAll<HTMLButtonElement>('[role="checkbox"]')];
    act(() => checks[0]!.click());
    act(() => checks[2]!.dispatchEvent(new MouseEvent("click", { bubbles: true, shiftKey: true })));
    expect(toggled.mock.calls.map(([reff]) => reff)).toEqual(rows.map((item) => item.reff));
  });

  it("puts row actions on the right button, not a hover control", () => {
    const state: WorkflowState = { id: "backlog", name: "Backlog", category: "backlog", color: "gray" };
    const rows = [row("LIST-4")];
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root?.render(
      <TooltipProvider>
        <IssueList
          groups={[{ key: "backlog", kind: "status", label: "Backlog", rows, state }]}
          deleted={[]}
          deletedMode={false}
          states={[state]}
          members={[]}
          labels={[]}
          selection={null}
          checked={new Set()}
          optimistic={new Set()}
          onSelect={() => undefined}
          onToggleCheck={() => undefined}
          onOpen={() => undefined}
          onCreate={() => undefined}
          readOnly={false}
          filtered={false}
        />
      </TooltipProvider>,
    ));

    // The `⋯` is gone: it cost a permanent slot at the end of every line to
    // hold a control that only appeared once you hovered it.
    expect(host.querySelector('[aria-label^="Actions for"]')).toBeNull();

    // What replaces it has to stay reachable without a pointer. Radix opens a
    // context menu on the Menu key when its trigger has focus, so the trigger
    // must be the row itself — and the row must remain focusable.
    const item = host.querySelector("li[data-issue-ref]") as HTMLElement;
    expect(item).toBeTruthy();
    expect(item.getAttribute("tabindex")).not.toBeNull();
  });

  it("shows two labels on a row and drops the rest without a tally", () => {
    const state: WorkflowState = { id: "backlog", name: "Backlog", category: "backlog", color: "gray" };
    const labelled: Row = { ...row("LIST-9"), label_names: ["infra", "perf", "docs"] };
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root?.render(
      <TooltipProvider>
        <IssueList
          groups={[{ key: "backlog", kind: "status", label: "Backlog", rows: [labelled], state }]}
          deleted={[]}
          deletedMode={false}
          states={[state]}
          members={[]}
          labels={[{ id: "lbl_infra", name: "infra", color: "blue" }]}
          selection={null}
          checked={new Set()}
          optimistic={new Set()}
          onSelect={() => undefined}
          onToggleCheck={() => undefined}
          onOpen={() => undefined}
          onCreate={() => undefined}
          readOnly={false}
          filtered={false}
        />
      </TooltipProvider>,
    ));

    const rowText = host.querySelector("li[data-issue-ref]")?.textContent ?? "";
    expect(rowText).toContain("infra");
    expect(rowText).toContain("perf");
    // The third would start competing with the title for truncation budget, and
    // it is dropped silently — a trailing `+1` is a tally of things you cannot
    // see, sitting on the same edge as the date. The full set is in the detail.
    expect(rowText).not.toContain("docs");
    expect(rowText).not.toContain("+1");
    // A name with no matching label still renders: `label_names` is the
    // daemon's, and a catalog that has not arrived yet must not blank the row.
    expect(host.querySelector('[title="perf"]')).toBeTruthy();
  });

  function render(current: Row, onOpen: (reff: string) => void) {
    const state: WorkflowState = { id: "backlog", name: "Backlog", category: "backlog", color: "gray" };
    const groups: RowGroup[] = [{ key: "backlog", kind: "status", label: "Backlog", rows: [current], state }];
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root?.render(
      <TooltipProvider>
        <IssueList
          groups={groups}
          deleted={[]}
          deletedMode={false}
          states={[state]}
          members={[]}
          labels={[]}
          selection={current.reff}
          checked={new Set()}
          optimistic={new Set()}
          onSelect={() => undefined}
          onToggleCheck={() => undefined}
          onOpen={onOpen}
          onCreate={() => undefined}
          readOnly={false}
          filtered={false}
        />
      </TooltipProvider>,
    ));
  }
});

function row(key: string): Row {
  return {
    reff: `iss_${key.toLowerCase()}`,
    doc_id: `iss_${key.toLowerCase()}`,
    project_id: "prj_list",
    key_alias: key,
    title: "Tune list density",
    status: "backlog",
    priority: "high",
    assignee_summary: "",
    assignees: [],
    tombstone: false,
    provisional: false,
  };
}
