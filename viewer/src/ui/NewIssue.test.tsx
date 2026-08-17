import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LabelDto, MemberDto, ProjectDto, WorkflowState } from "../types";
import { ProjectViewerStore, ProjectViewerStoreProvider } from "../projectStore";
import { NewIssue } from "./NewIssue";

const rpcMock = vi.hoisted(() => vi.fn());
const spaceRpcMock = vi.hoisted(() => vi.fn());
vi.mock("../api", () => ({ rpc: rpcMock, spaceRpc: spaceRpcMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

const STATES: WorkflowState[] = [
  { id: "backlog", name: "Backlog", category: "backlog", color: "gray" },
  { id: "doing", name: "In Progress", category: "active", color: "blue" },
];

const PROJECTS: ProjectDto[] = [
  { id: "prj_web", name: "Web", key: "WEB", color: "blue" },
  { id: "prj_engine", name: "Engine", key: "ENG", color: "green" },
];

const LABELS: LabelDto[] = [];
const MEMBERS: MemberDto[] = [];

/**
 * What the composer sends, and why the project is not optional.
 *
 * The composer used to omit `project` whenever it matched the one you had
 * open, on the theory that the daemon would fill in the obvious answer. It has
 * none: a null project is resolved through the CLI's chain — the git branch,
 * then `project.default`, then "is there exactly one?" — and a browser
 * satisfies none of those links. Reproduced against the running engine, the
 * payload without it answers:
 *
 *     no project chosen and no single default — pass -p <project>
 *
 * So the composer only worked when you picked a project you were NOT in, which
 * is the one case that took the branch, and failed on its own default every
 * time. The field is unconditional now, and this is the guard on that.
 */
describe("the composer names the project it files into", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  beforeEach(() => {
    rpcMock.mockReset();
    rpcMock.mockImplementation(async (_space: string, request: { cmd?: string }) => {
      if (request.cmd !== "change_set") throw new Error("unexpected request");
      return {
        kind: "change_set",
        results: [{ operation: 0, kind: "issue", id: "iss_created" }],
        receipt: {
          operation: "11".repeat(16),
          phase: "accepted",
          publication: {
            publication: {
              manifest_root: Array(32).fill(1),
              implementation_digest: Array(32).fill(2),
              extractor_schema_digest: Array(32).fill(3),
            },
            materialization: 1,
          },
        },
      };
    });
    localStorage.clear();
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  async function fileAnIssue(projectKey: string) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    const store = new ProjectViewerStore(rpcMock, undefined, undefined, () => "11".repeat(16));
    await act(async () => {
      root?.render(
        <ProjectViewerStoreProvider store={store}>
          <NewIssue
            spaceId="orb_test"
            canonicalSpaceId="ws_test"
            projectKey={projectKey}
            projects={PROJECTS}
            states={STATES}
            labels={LABELS}
            members={MEMBERS}
            onClose={vi.fn()}
            onError={vi.fn()}
            onCreated={vi.fn()}
          />
        </ProjectViewerStoreProvider>,
      );
    });

    // By accessible name, not by tag: the title is an `<input>` with no `type`
    // attribute, so `input[type="text"]` does not match it and a tag-list
    // selector silently lands on the description textarea instead — a test that
    // types into the wrong field and then proves nothing.
    const title = host.querySelector<HTMLInputElement>('input[aria-label="Issue title"]');
    expect(title, "the composer draws a title field").toBeTruthy();
    await act(async () => {
      // React tracks the last value it wrote on the node, so assigning through
      // the element skips the change and `onChange` never fires. The prototype
      // setter is what a controlled input actually listens to.
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(
        title,
        "A new issue",
      );
      title!.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const create = [...host.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Create issue",
    );
    expect(create, "the composer draws a Create button").toBeTruthy();
    await act(async () => {
      create?.click();
    });

    return rpcMock.mock.calls.find(
      (call) => (call[1] as { cmd?: string })?.cmd === "change_set",
    )?.[1] as Record<string, unknown> | undefined;
  }

  /**
   * The regression, stated as the thing that was actually broken: filing into
   * the project you are already in. Every other combination worked.
   */
  it("sends the project even when it is the one already open", async () => {
    const sent = await fileAnIssue("WEB");
    expect(sent).toBeTruthy();
    expect(sent).toMatchObject({
      cmd: "change_set",
      operations: [{
        op: "issue_create",
        title: "A new issue",
        project: { source: "existing", project: "WEB" },
      }],
    });
  });

  /** And it is never merely present-but-empty: the engine reads `null` as "work
   *  it out yourself", which is the same failure spelled differently. */
  it("never leaves the project null or absent", async () => {
    const sent = await fileAnIssue("ENG");
    const operations = sent?.operations as Array<Record<string, unknown>> | undefined;
    const project = operations?.[0]?.project as Record<string, unknown> | undefined;
    expect(project).toEqual({ source: "existing", project: "ENG" });
  });
});
