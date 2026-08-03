import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MilestoneDto, ProjectDto } from "../types";
import { WorldViewStoreProvider } from "../core/worldViewReact";
import { ProjectViewerStore, ProjectViewerStoreProvider } from "../projectStore";
import { ProjectOverview } from "./ProjectOverview";
import { TooltipProvider } from "./primitives";

const rpcMock = vi.hoisted(() => vi.fn());
const spaceRpcMock = vi.hoisted(() => vi.fn());
// Both transports: the store defaults to the real ones, so a partial mock of
// `../api` would leave it holding an undefined space transport.
vi.mock("../api", () => ({ rpc: rpcMock, spaceRpc: spaceRpcMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

const project: ProjectDto = {
  id: "prj_test",
  name: "Test project",
  key: "TEST",
  color: "blue",
};

describe("ProjectOverview", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
    rpcMock.mockReset();
  });

  /**
   * The bug this page shipped with: the milestone list held its own `useState`
   * and fetched once on mount, so its bars were a photograph. A milestone's
   * progress is counted from issue bodies, which means dragging a card to Done
   * — on a board, in another pane, by a teammate — changes a number here, and
   * this surface never heard about any of it.
   *
   * The ring below carries a dirty doc and no catalog plane at all: nothing
   * about the milestone RECORD changed, only the issues it counts.
   */
  it("re-reads milestone progress when an issue in the project moves", async () => {
    let milestones: MilestoneDto[] = [
      { id: "mls_1", name: "Beta", total: 2, done: 0 },
    ];
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      if (request.cmd === "milestone_list") {
        return Promise.resolve({ kind: "milestones", milestones });
      }
      if (request.cmd === "project_updates") {
        return Promise.resolve({ kind: "updates", updates: [] });
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
            <StrictMode>
              <TooltipProvider>
                <ProjectOverview
                  spaceId="local"
                  project={project}
                  members={[]}
                  readOnly
                  onError={vi.fn()}
                />
              </TooltipProvider>
            </StrictMode>
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // The name is an editable heading now, so it lives in a value rather than
    // in text: the rail's read-only row moved out to the project shell.
    const heading = () =>
      host?.querySelector<HTMLInputElement>('input[aria-label^="Milestone name"]')?.value;
    expect(heading()).toBe("Beta");
    expect(host.textContent).toContain("2 issues · 0%");

    milestones = [{ id: "mls_1", name: "Beta", total: 2, done: 1 }];
    await act(async () => {
      await store.handleDoorbell({
        space: "local", epoch: 1, seq: 1, reset: false,
        dirty: [{ kind: "project", id: project.id, label: project.key, docs: ["doc_1"] }],
        planes: [],
        authority_advanced: false, activity_advanced: false, presence_advanced: false,
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(host.textContent).toContain("2 issues · 50%");
  });

  /** The milestone resource is keyed on the `prj_` id, so the request the page
   *  makes must carry the id — a KEY would register under an alias a rename
   *  moves, and the panel would silently stop refreshing. */
  it("asks for milestones by project id, not by the display key", async () => {
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      if (request.cmd === "milestone_list") {
        return Promise.resolve({ kind: "milestones", milestones: [] });
      }
      if (request.cmd === "project_updates") {
        return Promise.resolve({ kind: "updates", updates: [] });
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
              <ProjectOverview
                spaceId="local"
                project={project}
                members={[]}
                readOnly
                onError={vi.fn()}
              />
            </TooltipProvider>
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const asked = rpcMock.mock.calls
      .filter(([, request]) => request.cmd === "milestone_list" || request.cmd === "project_updates")
      .map(([, request]) => request.project);
    expect(asked.length).toBeGreaterThan(0);
    expect(asked.every((p) => p === project.id)).toBe(true);
  });
});
