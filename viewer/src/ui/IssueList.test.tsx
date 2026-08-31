import { act } from "react";
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { RowGroup } from "../core/display";
import type { Row, WorkflowState } from "../types";
import type { IssueMutators } from "./fields";
import { IssueList } from "./IssueList";

const noopMutators: IssueMutators = {
  setStatus: () => undefined,
  setPriority: () => undefined,
  toggleAssignee: () => undefined,
  toggleLabel: () => undefined,
  swapLabel: () => undefined,
  setDue: () => undefined,
  setEstimate: () => undefined,
};

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
    // A native <input type="checkbox">, not Radix's <button role="checkbox">:
    // Astryx renders the real control, so the role is implicit and the element
    // participates in forms. The accessible name comes from a visually-hidden
    // <label for>, which is the mechanism worth asserting — not an aria-label
    // we happened to pass.
    const box = host!.querySelector<HTMLInputElement>('input[type="checkbox"]');
    expect(box).toBeTruthy();
    expect(host!.querySelector(`label[for="${box!.id}"]`)?.textContent).toBe("Select LIST-1");
    act(() => item.click());
    expect(onOpen).toHaveBeenLastCalledWith(current.reff);
    onOpen.mockClear();
    act(() => item.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    expect(onOpen).toHaveBeenCalledWith(current.reff);
  });

  it("keeps group and row leading columns on shared geometry", () => {
    render(row("LIST-2"), () => undefined);

    const header = host!.querySelector("section > header") as HTMLElement;
    const collection = host!.querySelector("[data-issue-collection]") as HTMLElement;
    const item = host!.querySelector("li[data-issue-ref]") as HTMLElement;
    expect(header.classList).toContain("mt-1");
    expect(header.classList).toContain("mx-2");
    expect(header.classList).toContain("px-4");
    expect(collection.classList).not.toContain("mx-2");
    expect(item.classList).toContain("mx-2");
    expect(item.classList).toContain("px-4");
    expect(header.classList).toContain("gap-2");
    expect(item.classList).toContain("gap-2");

    const headerSlots = [...header.querySelectorAll<HTMLElement>(":scope > span")].slice(0, 2);
    const rowSlots = [...item.querySelectorAll<HTMLElement>(":scope > span[data-row-control]")].slice(0, 2);
    expect(headerSlots).toHaveLength(2);
    expect(rowSlots).toHaveLength(2);
    for (const slot of [...headerSlots, ...rowSlots]) {
      expect(slot.classList).toContain("size-icon-md");
      expect(slot.classList).toContain("items-center");
      expect(slot.classList).toContain("justify-center");
    }
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
          mutators={noopMutators}
          readOnly={false}
          filtered={false}
      onClearFilter={() => {}}
        />
      );
    }
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root?.render(
        <Harness />
    ));

    const checks = [...host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];
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
          mutators={noopMutators}
          readOnly={false}
          filtered={false}
      onClearFilter={() => {}}
        />
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
          mutators={noopMutators}
          readOnly={false}
          filtered={false}
      onClearFilter={() => {}}
        />
    ));

    // Only what the row SHOWS. Astryx renders tooltip and popover content into
    // the DOM up front and reveals it through the native popover API, so a bare
    // `textContent` now includes the label legend hiding in the overflow chip's
    // tooltip — which is exactly the text this test is asserting is not on the
    // row. Strip the top-layer surfaces and the question goes back to being
    // "what can you read".
    const li = host.querySelector("li[data-issue-ref]")!;
    const visible = li.cloneNode(true) as HTMLElement;
    visible.querySelectorAll("[popover]").forEach((el) => el.remove());
    const rowText = visible.textContent ?? "";
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
          mutators={noopMutators}
          readOnly={false}
          filtered={false}
      onClearFilter={() => {}}
        />
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
