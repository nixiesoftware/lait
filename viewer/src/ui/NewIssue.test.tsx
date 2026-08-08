import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LabelDto, MemberDto, ProjectDto, WorkflowState } from "../types";
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
    rpcMock.mockResolvedValue({ kind: "ref", reff: "WEB-1" });
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
    await act(async () => {
      root?.render(
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
        />,
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
      (call) => (call[1] as { cmd?: string })?.cmd === "issue_new",
    )?.[1] as Record<string, unknown> | undefined;
  }

  /**
   * The regression, stated as the thing that was actually broken: filing into
   * the project you are already in. Every other combination worked.
   */
  it("sends the project even when it is the one already open", async () => {
    const sent = await fileAnIssue("WEB");
    expect(sent).toBeTruthy();
    expect(sent).toMatchObject({ cmd: "issue_new", title: "A new issue", project: "WEB" });
  });

  /** And it is never merely present-but-empty: the engine reads `null` as "work
   *  it out yourself", which is the same failure spelled differently. */
  it("never leaves the project null or absent", async () => {
    const sent = await fileAnIssue("ENG");
    expect(Object.hasOwn(sent ?? {}, "project")).toBe(true);
    expect(sent?.project).toBe("ENG");
  });
});
