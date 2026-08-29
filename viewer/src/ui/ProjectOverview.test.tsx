import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MemberDto, MilestoneDto, ProjectDto, TeamDto } from "../types";
import { WorldViewStoreProvider } from "../core/worldViewReact";
import { ProjectViewerStore, ProjectViewerStoreProvider } from "../projectStore";
import { ProjectOverview } from "./ProjectOverview";

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

const publication = {
  publication: {
    manifest_root: Array(32).fill(1),
    implementation_digest: Array(32).fill(2),
    extractor_schema_digest: Array(32).fill(3),
  },
  materialization: 1,
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
        return Promise.resolve({ kind: "milestones", page: { publication, items: milestones } });
      }
      if (request.cmd === "project_updates") {
        return Promise.resolve({ kind: "updates", page: { publication, items: [] } });
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
                <ProjectOverview
                  spaceId="local"
                  project={project}
                  members={[]}
                  teams={[]}
                  readOnly
                  onError={vi.fn()}
                />
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
    expect(host.querySelector("svg.lucide-box")).not.toBeNull();

    milestones = [{ id: "mls_1", name: "Beta", total: 2, done: 1 }];
    await act(async () => {
      await store.handleDoorbell({
        space: "local", epoch: 1, seq: 1, reset: false,
        invalidations: [{ world: "com.lait.issues", dirty: [{ kind: "project", id: project.id, label: project.key, docs: ["doc_1"] }], planes: [] }],
        authority_advanced: false, activity_advanced: false, presence_advanced: false,
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(host.textContent).toContain("2 issues · 50%");
  });


  /**
   * The defect this page shipped with, and the reason the properties moved.
   *
   * Lead, team and the planned window lived in `ProjectRail`, whose open/shut
   * state is a PERSISTED preference. Shut it once and a project's Overview
   * could no longer show or set a single fact about the project — an empty one
   * drew a name, a placeholder and an update composer, and nothing else. The
   * page has to state the project's facts on its own, because it is the page
   * that is about the project; the rail is a filter and may be absent.
   *
   * Asserted through the controls' own labels rather than through a rail prop,
   * so it still holds if the rail is rebuilt or removed entirely.
   */
  it("states the project's properties without a rail", async () => {
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      if (request.cmd === "milestone_list") {
        return Promise.resolve({ kind: "milestones", page: { publication, items: [] } });
      }
      if (request.cmd === "project_updates") {
        return Promise.resolve({ kind: "updates", page: { publication, items: [] } });
      }
      throw new Error(`Unexpected request: ${request.cmd}`);
    });

    const members: MemberDto[] = [];
    const teams: TeamDto[] = [];
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    const store = new ProjectViewerStore(rpcMock);

    await act(async () => {
      root?.render(
        <WorldViewStoreProvider store={store.resources}>
          <ProjectViewerStoreProvider store={store}>
            <ProjectOverview
              spaceId="local"
              project={project}
              members={members}
              teams={teams}
              readOnly={false}
              onError={vi.fn()}
            />
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const text = host.textContent ?? "";
    for (const prompt of ["Set lead", "No teams yet", "Add start date", "Add target date"]) {
      expect(text, `the page must offer "${prompt}" with no rail beside it`).toContain(prompt);
    }
  });

  /**
   * One absence, said once.
   *
   * An empty feed used to draw a live textarea, an `UPDATES` caption, "No
   * updates yet." and a disabled `Post update` — four statements of the same
   * nothing, and the disabled button was the only filled thing on the page, so
   * it was the loudest. The composer is the shape a feed WITH posts in it
   * wants; an empty project wants one affordance that opens it.
   */
  it("offers one way in to an empty updates feed, not a standing composer", async () => {
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      if (request.cmd === "milestone_list") {
        return Promise.resolve({ kind: "milestones", page: { publication, items: [] } });
      }
      if (request.cmd === "project_updates") {
        return Promise.resolve({ kind: "updates", page: { publication, items: [] } });
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
            <ProjectOverview
              spaceId="local"
              project={project}
              members={[]}
              teams={[]}
              readOnly={false}
              onError={vi.fn()}
            />
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const composer = () => host?.querySelector('textarea[aria-label="New project update"]');
    expect(composer()).toBeNull();
    expect(host.textContent).not.toContain("No updates yet.");

    const invitation = [...host.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("Write the first project update"),
    );
    expect(invitation, "an empty feed must carry its own way in").toBeDefined();

    await act(async () => {
      invitation?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(composer()).not.toBeNull();
  });

  /** The milestone resource is keyed on the `prj_` id, so the request the page
   *  makes must carry the id — a KEY would register under an alias a rename
   *  moves, and the panel would silently stop refreshing. */
  it("asks for milestones by project id, not by the display key", async () => {
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      if (request.cmd === "milestone_list") {
        return Promise.resolve({ kind: "milestones", page: { publication, items: [] } });
      }
      if (request.cmd === "project_updates") {
        return Promise.resolve({ kind: "updates", page: { publication, items: [] } });
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
              <ProjectOverview
                spaceId="local"
                project={project}
                members={[]}
                teams={[]}
                readOnly
                onError={vi.fn()}
              />
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
