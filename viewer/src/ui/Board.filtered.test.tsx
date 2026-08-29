import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DEFAULT_DISPLAY } from "../core/display";
import type { BoardView, Row } from "../types";
import { Board } from "./Board";
import type { IssueMutators } from "./fields";

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

// The board must never render as a silent grid of empty columns when a filter
// (classically a leftover `mine` from "My issues") is what emptied it — that was
// the "boards empty despite containing issues" bug.
describe("Board filtered-empty state", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  const board: BoardView = {
    schema_version: 1,
    project: { id: "prj_x", name: "X", key: "X", color: "blue", description: "", lead: "", start_date: null, target_date: null, archived: false },
    columns: [
      { state: { id: "backlog", name: "Backlog", category: "backlog", color: "gray" }, rows: [] },
      { state: { id: "done", name: "Done", category: "done", color: "green" }, rows: [] },
    ],
    total: 0,
    complete: true,
  };

  function mount(props: Partial<Parameters<typeof Board>[0]>) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() =>
      root?.render(
          <Board
            board={board}
            display={DEFAULT_DISPLAY}
            members={[]}
            labels={[]}
            selection={null}
            optimistic={new Set()}
            onSelect={() => undefined}
            onCreate={() => undefined}
            onDrop={() => undefined}
            onReassign={() => undefined}
            mutators={noopMutators}
            onLoadChildren={() => Promise.resolve([])}
            readOnly={false}
            filtered={false}
            onClearFilter={() => undefined}
            {...props}
          />
      ),
    );
  }

  it("offers a Clear filter action when a filter has hidden every issue", () => {
    const onClearFilter = vi.fn();
    mount({ filtered: true, onClearFilter });

    const state = host!.querySelector('[data-application-state="filtered-empty"]');
    expect(state).toBeTruthy();
    const clear = [...host!.querySelectorAll("button")].find((b) => /clear filter/i.test(b.textContent ?? ""));
    expect(clear).toBeTruthy();
    act(() => clear!.click());
    expect(onClearFilter).toHaveBeenCalledOnce();
  });

  it("shows the columns (not the filtered state) when no filter is active", () => {
    mount({ filtered: false });
    expect(host!.querySelector('[data-application-state="filtered-empty"]')).toBeNull();
    expect(host!.querySelector('[aria-label="Issue board"]')).toBeTruthy();
  });

  it("keeps selection and horizontal position while pending rows refresh", () => {
    const row: Row = {
      reff: "ONE-1",
      doc_id: "iss_1",
      project_id: "prj_x",
      key_alias: "ONE-1",
      title: "Keep me selected",
      status: "backlog",
      priority: "none",
      assignee_summary: "",
      assignees: [],
      tombstone: false,
      provisional: false,
    };
    const loaded: BoardView = {
      ...board,
      columns: [{ ...board.columns[0]!, rows: [row] }, board.columns[1]!],
    };
    mount({ board: loaded, selection: row.reff, optimistic: new Set([row.doc_id]) });
    const canvas = host!.querySelector<HTMLElement>('[aria-label="Issue board"]')!;
    act(() => {
      canvas.scrollLeft = 347;
      canvas.dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    const refreshed: BoardView = {
      ...loaded,
      columns: [
        { ...loaded.columns[0]!, rows: [] },
        { ...loaded.columns[1]!, rows: [{ ...row, status: "done" }] },
      ],
    };
    act(() => {
      root?.render(
        <Board
          board={refreshed}
          display={DEFAULT_DISPLAY}
          members={[]}
          labels={[]}
          selection={row.reff}
          optimistic={new Set([row.doc_id])}
          onSelect={() => undefined}
          onCreate={() => undefined}
          onDrop={() => undefined}
          onReassign={() => undefined}
          mutators={noopMutators}
          onLoadChildren={() => Promise.resolve([])}
          readOnly={false}
          filtered={false}
          onClearFilter={() => undefined}
        />,
      );
    });
    expect(canvas.scrollLeft).toBe(347);
    expect(host!.querySelector('[aria-current="true"]')?.textContent).toContain("Keep me selected");
  });
});
