import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { IssueView, LiveEntry, MemberDto, ProjectDto, WorkflowState } from "../types";
import { WorldViewStoreProvider } from "../core/worldViewReact";
import { liveKey, type LiveState } from "../live";
import { projectKeys, ProjectViewerStore, ProjectViewerStoreProvider } from "../projectStore";
import { IssueDetail } from "./IssueDetail";
import { TooltipProvider } from "./primitives";

const rpcMock = vi.hoisted(() => vi.fn());
const spaceRpcMock = vi.hoisted(() => vi.fn());
// Both transports: the store defaults to the real ones, so a partial mock of
// `../api` would leave it holding an undefined space transport.
vi.mock("../api", () => ({ rpc: rpcMock, spaceRpc: spaceRpcMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A socket that never opens.
 *
 * The live rail asks its question through the real socket, which would otherwise
 * dial `ws://localhost/api/session`, fail, and leave a reconnect timer running
 * past the end of the test. Held at `CONNECTING` for the whole run: the socket
 * refuses to send on a socket that is not `OPEN`, so nothing leaves and nothing
 * schedules.
 */
class UnopenedSocket {
  static readonly OPEN = 1;
  readyState = 0;
  binaryType = "arraybuffer";
  onopen: (() => void) | null = null;
  onmessage: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  send(): void {}
  close(): void {}
}

vi.stubGlobal("WebSocket", UnopenedSocket);

describe("IssueDetail loading", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
    rpcMock.mockReset();
  });

  it("renders the issue document without waiting for history or relations", async () => {
    const never = new Promise<never>(() => undefined);
    let resolveView!: (value: IssueView & { kind: "issue" }) => void;
    const view = new Promise<IssueView & { kind: "issue" }>((resolve) => {
      resolveView = resolve;
    });
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      if (request.cmd === "issue_view") return view;
      if (request.cmd === "milestone_list") {
        return Promise.resolve({ kind: "milestones", milestones: [] });
      }
      if (request.cmd === "packet") {
        return Promise.resolve({
          kind: "packet", issue: issue.doc_id, governing: [], guidance: [],
          proof: [], record: [], conflicts: [],
        });
      }
      if (request.cmd === "history" || request.cmd === "issue_graph") return never;
      throw new Error(`Unexpected request: ${request.cmd}`);
    });

    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    const store = new ProjectViewerStore(rpcMock);
    store.resources.set(projectKeys.row("local", issue.reff), {
      reff: issue.reff,
      doc_id: issue.doc_id,
      project_id: issue.project_id,
      key_alias: issue.key_alias,
      title: issue.title,
      status: issue.status,
      priority: issue.priority,
      assignee_summary: "",
      assignees: issue.assignees,
      tombstone: false,
      provisional: false,
    });

    act(() => {
      root?.render(
        <WorldViewStoreProvider store={store.resources}>
          <ProjectViewerStoreProvider store={store}>
            <StrictMode>
              <TooltipProvider>
                <IssueDetail
            spaceId="local"
            canonicalSpaceId="ws_test"
            reff={issue.reff}
            states={[state]}
            members={[]}
            labels={[]}
            projects={[project]}
            readOnly
            tombstone={false}
            openField={null}
            onOpenField={() => undefined}
            onError={vi.fn()}
            onDelete={() => undefined}
            onPredict={async () => true}
            onNavigate={() => undefined}
            onClose={() => undefined}
                />
              </TooltipProvider>
            </StrictMode>
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
    });

    expect(host.textContent).not.toContain("Loading issue");
    expect(host.querySelector<HTMLTextAreaElement>('[aria-label="Title"]')?.value)
      .toBe(issue.title);
    // The issue draws no bar of its own. It is a hop on the shell's trail and a
    // tenant of the shell's actions slot, so a second header here would be a
    // second inset, a second height and a second place the title could sit.
    expect(host.querySelector("header")).toBeNull();
    expect(host.querySelector('nav[aria-label="Breadcrumb"]')).toBeNull();
    await act(async () => {
      await Promise.resolve();
    });
    expect(rpcMock.mock.calls.filter(([, request]) => request.cmd === "issue_view")).toHaveLength(1);
    await act(async () => {
      resolveView({ kind: "issue", ...issue });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(host.textContent).not.toContain("Loading issue");
  });
});

describe("IssueDetail live rail", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
    rpcMock.mockReset();
  });

  function seededStore(): ProjectViewerStore {
    // Every fetch hangs. The rail draws off the seeded row and the seeded slot,
    // so a request that resolved would only land its state update after the
    // assertions, outside `act`.
    const never = new Promise<never>(() => undefined);
    rpcMock.mockImplementation((_space: string, request: { cmd: string }) => {
      const known = ["issue_view", "milestone_list", "history", "issue_graph", "packet"];
      if (known.includes(request.cmd)) return never;
      throw new Error(`Unexpected request: ${request.cmd}`);
    });

    const store = new ProjectViewerStore(rpcMock);
    store.resources.set(projectKeys.row("local", issue.reff), {
      reff: issue.reff,
      doc_id: issue.doc_id,
      project_id: issue.project_id,
      key_alias: issue.key_alias,
      title: issue.title,
      status: issue.status,
      priority: issue.priority,
      assignee_summary: "",
      assignees: issue.assignees,
      tombstone: false,
      provisional: false,
    });
    return store;
  }

  function mount(live: LiveState, members: MemberDto[]): HTMLDivElement {
    const store = seededStore();
    // Seeded, not pushed: the rail reads the slot the socket writes, and seeding
    // it is what lets this test pin the rendering without a socket.
    //
    // Under the **doc id**, which is what the rail asks by. The fixture's `reff`
    // is deliberately a different string, so a rail that asked by it would read
    // an absent slot and draw nothing.
    store.resources.set<LiveState>(liveKey("local", issue.doc_id), live);
    return render(store, members);
  }

  function render(store: ProjectViewerStore, members: MemberDto[]): HTMLDivElement {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    const mounted = host;
    act(() => {
      root?.render(
        <WorldViewStoreProvider store={store.resources}>
          <ProjectViewerStoreProvider store={store}>
            <StrictMode>
              <TooltipProvider>
                <IssueDetail
                  spaceId="local"
                  canonicalSpaceId="ws_test"
                  reff={issue.reff}
                  states={[state]}
                  members={members}
                  labels={[]}
                  projects={[project]}
                  readOnly
                  tombstone={false}
                  openField={null}
                  onOpenField={() => undefined}
                  onError={vi.fn()}
                  onDelete={() => undefined}
                  onPredict={async () => true}
                  onNavigate={() => undefined}
                  onClose={() => undefined}
                />
              </TooltipProvider>
            </StrictMode>
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
    });
    return mounted;
  }

  it("draws the people on the issue, and says which of them may have left", () => {
    // An uncertain row is rendered, not dropped. The daemon has stopped hearing
    // from that station inside the grace window; it has not said the person left,
    // and a facepile that quietly omits them reports an emptier room than there is.
    const mounted = mount(
      table([presence("act_a", 20, false), presence("act_b", 4_000, true)]),
      [member("act_a", "Ann"), member("act_b", "Bo")],
    );
    const here = mounted.querySelector('[title="Here now"]');
    expect(here?.textContent).toContain("Ann");
    expect(here?.textContent).toContain("Bo");
    expect(here?.textContent).toContain("may have left");
    // Both faces are drawn — the uncertain one in its own stack, so it can be
    // dimmed without dimming the people who are definitely there.
    expect(mounted.querySelector('[aria-label="Ann"]')).not.toBeNull();
    expect(mounted.querySelector('[aria-label="Bo"]')).not.toBeNull();
  });

  it("never draws a drifted caret as a position, or as an unresolved one", () => {
    const mounted = mount(
      table([
        caret("act_b", { caret: "at", position: 12 }, "description", 5),
        caret("act_a", { caret: "drifted" }, "description", 10),
        caret("act_c", { caret: "unresolved" }, "title", 20),
      ]),
      [member("act_a", "Ann"), member("act_b", "Bo"), member("act_c", "Cy")],
    );
    const rows = [...mounted.querySelectorAll('[title="Caret"]')].map((row) => row.textContent);
    expect(rows).toHaveLength(3);
    expect(rows[0]).toContain("12");
    // Drifted means the material the offset pointed into is gone. Any number here
    // would be the last one anybody saw, drawn as though it still meant something.
    expect(rows[1]).not.toMatch(/\d/);
    expect(rows[1]).not.toBe(rows[2]);
    expect(rows[2]).not.toMatch(/\d/);
  });

  it("asks by the doc id, because a project alias hashes to nobody's Body", () => {
    // The rail's question is turned into a Body id by hashing the string it was
    // given. `TEST-1` hashes to a Body no peer publishes under, so a rail that
    // asked by `reff` would be answered an empty table for ever — and its own
    // empty state renders nothing, so it would go on doing that in silence.
    const store = seededStore();
    store.resources.set<LiveState>(
      liveKey("local", issue.reff),
      table([presence("act_a", 20, false)]),
    );
    const mounted = render(store, [member("act_a", "Ann")]);
    expect(mounted.querySelector('[title="Here now"]')).toBeNull();

    act(() => {
      store.resources.set<LiveState>(
        liveKey("local", issue.doc_id),
        table([presence("act_a", 20, false)]),
      );
    });
    expect(mounted.querySelector('[title="Here now"]')?.textContent).toContain("Ann");
  });

  it("says nothing at all when the daemon cannot answer", () => {
    // Not "nobody is here". The rail has no idea who is here, and an empty
    // facepile drawn from that is a claim this node cannot make.
    const mounted = mount(
      { generation: null, partial: false, entries: [], unavailable: true },
      [member("act_a", "Ann")],
    );
    expect(mounted.querySelector('[title="Here now"]')).toBeNull();
    expect(mounted.textContent).not.toContain("Ann");
  });
});

function table(entries: LiveEntry[]): LiveState {
  return { generation: 4, partial: false, entries, unavailable: false };
}

function presence(actor: string, ageMs: number, uncertain: boolean): LiveEntry {
  return {
    actor,
    scope: { scope: "issue_view", world: "com.lait.issues", body: "aaaa" },
    kind: "presence",
    age_ms: ageMs,
    uncertain,
    caret: null,
    focus: null,
  };
}

function caret(
  actor: string,
  position: LiveEntry["caret"],
  field: string,
  ageMs: number,
): LiveEntry {
  return {
    actor,
    scope: { scope: "text_caret", world: "com.lait.issues", body: "aaaa", field },
    kind: "caret",
    age_ms: ageMs,
    uncertain: false,
    caret: position,
    focus: null,
  };
}

function member(key: string, alias: string): MemberDto {
  return { key, role: "contributor", me: false, alias };
}

const state: WorkflowState = {
  id: "done",
  name: "Done",
  category: "done",
  color: "green",
};

const project: ProjectDto = {
  id: "prj_test",
  name: "Test project",
  key: "TEST",
  color: "blue",
};

const issue: IssueView = {
  schema_version: 1,
  reff: "TEST-1",
  doc_id: "iss_01jz0000000000000000000000",
  space_id: "ws_test",
  project_id: project.id,
  project_key: project.key,
  key_alias: "TEST-1",
  title: "Primary content is ready",
  description: "",
  status: state.id,
  priority: "none",
  assignees: [],
  labels: [],
  label_names: [],
  comments: [],
  created_by: "actor_test",
  created_at: 1,
  provisional: false,
};
