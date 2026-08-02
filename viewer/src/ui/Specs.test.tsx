import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { WorldViewStoreProvider } from "../core/worldViewReact";
import { ProjectViewerStore, ProjectViewerStoreProvider } from "../projectStore";
import type { AssignmentDto, SpecKind, SpecRevision, SpecState, SpecView } from "../types";
import { Specs } from "./Specs";
import { TooltipProvider } from "./primitives";

const rpcMock = vi.hoisted(() => vi.fn());
const spaceRpcMock = vi.hoisted(() => vi.fn());
vi.mock("../api", () => ({ rpc: rpcMock, spaceRpc: spaceRpcMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

function spec(
  id: string,
  kind: SpecKind,
  title: string,
  text = "",
  lifecycle: { state?: SpecState; heads?: string[]; issued?: string[] } = {},
): SpecView {
  const state = lifecycle.state ?? "draft";
  const body = {
    spec: id,
    project: "prj_1",
    kind,
    title,
    text,
    state,
    links: [],
    author: "act_1",
    ts: 1_770_000_000,
  };
  return {
    spec: id,
    project: "prj_1",
    kind,
    title,
    state,
    revision: `rev_${id}`,
    heads: lifecycle.heads ?? [`rev_${id}`],
    issued: lifecycle.issued ?? [],
    body,
  };
}

/** Deliberately out of chain order in the reply, so grouping is what orders it. */
const SPECS = [
  spec("spc_c", "record", "What we shipped"),
  spec("spc_a", "requirement", "Login is race-free"),
  spec("spc_b", "goal", "Sign-in is trustworthy"),
  spec("spc_d", "requirement", "Sessions expire"),
];

describe("Specs", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
    rpcMock.mockReset();
  });

  async function render(
    props: Partial<React.ComponentProps<typeof Specs>> = {},
    specs = SPECS,
    grants: AssignmentDto[] = [],
    history: SpecRevision[] = [],
  ) {
    rpcMock.mockImplementation((_space: string, request: { cmd: string; spec?: string }) => {
      if (request.cmd === "spec_list") return Promise.resolve({ kind: "specs", specs });
      if (request.cmd === "access_list") return Promise.resolve({ kind: "assignments", rows: grants });
      if (request.cmd === "spec_history") {
        return Promise.resolve({ kind: "spec_revisions", revisions: history });
      }
      if (request.cmd === "spec_show") {
        const found = specs.find((candidate) => candidate.spec === request.spec);
        return Promise.resolve({ kind: "spec", spec: found });
      }
      if (
        request.cmd === "spec_revise" ||
        request.cmd === "spec_state" ||
        request.cmd === "spec_resolve"
      ) {
        return Promise.resolve({ kind: "spec", spec: specs[0] });
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
              <Specs
                spaceId="local"
                project="PLAT"
                projectName="Platform"
                readOnly={false}
                spec={null}
                baseline={null}
                members={[]}
                onOpenBaseline={vi.fn()}
                composing={null}
                onCompose={vi.fn()}
                onOpen={vi.fn()}
                onError={vi.fn()}
                {...props}
              />
            </TooltipProvider>
          </ProjectViewerStoreProvider>
        </WorldViewStoreProvider>,
      );
    });
    return host;
  }

  it("groups the register by kind, in the chain's order and not the reply's", async () => {
    const el = await render();
    const headings = [...el.querySelectorAll("h2")].map((h) => h.textContent);
    expect(headings).toEqual(["Goals", "Requirements", "Records"]);

    // Rows sit under their own kind, and an unused kind is absent entirely
    // rather than an empty group — there are ten kinds and three headings.
    const requirements = el.querySelector("ul[aria-label='Requirements']");
    expect([...requirements!.querySelectorAll("li")].map((li) => li.textContent)).toEqual([
      expect.stringContaining("Login is race-free"),
      expect.stringContaining("Sessions expire"),
    ]);
  });

  it("draws no lifecycle chrome for a document nothing has happened to", async () => {
    const el = await render();
    const text = el.textContent ?? "";
    // Stage 0's whole rule: a Spec draws what has happened to it. Every row here
    // is a fresh draft with one revision, so a state word or a revision
    // coordinate on screen would be the surface inventing a fact.
    expect(text).not.toMatch(/draft|issued|review|withdrawn/i);
    expect(text).not.toContain("rev_");
    expect(text).not.toContain("spc_");
  });

  it("says what a row's lifecycle is only once it has one", async () => {
    const el = await render({}, [
      spec("spc_plain", "requirement", "Nothing has happened to this one"),
      spec("spc_rev", "requirement", "Out for review", "", { state: "review" }),
      spec("spc_iss", "requirement", "Governing now", "", {
        state: "issued",
        issued: ["rev_spc_iss"],
      }),
      spec("spc_ahead", "requirement", "Being revised", "", {
        state: "draft",
        issued: ["rev_older"],
      }),
      spec("spc_conf", "requirement", "Two heads", "", { heads: ["rev_a", "rev_b"] }),
    ]);
    const row = (id: string) => el.querySelector(`[data-spec-id='${id}']`)?.textContent ?? "";

    // The plain draft is the one that says nothing — it is what every Spec is
    // to begin with, so a word here would appear on every row of a new project.
    expect(row("spc_plain")).not.toMatch(/draft|issued|review/i);
    expect(row("spc_rev")).toContain("In review");
    expect(row("spc_iss")).toContain("Issued");
    // Two facts, not one: the issued revision still governs while its successor
    // is written, so the row may not collapse to the head.
    expect(row("spc_ahead")).toContain("Issued · draft ahead");
    expect(row("spc_conf")).toContain("Concurrent heads");
  });

  it("opens a row as a document", async () => {
    const onOpen = vi.fn();
    const el = await render({ onOpen });
    const row = el.querySelector<HTMLElement>("[data-spec-id='spc_a']");
    await act(async () => row?.click());
    expect(onOpen).toHaveBeenCalledWith("spc_a");
  });

  it("offers creation from an empty register", async () => {
    const onCompose = vi.fn();
    const el = await render({ onCompose }, []);
    expect(el.textContent).toContain("No specs yet");
    const button = [...el.querySelectorAll("button")].find((b) => b.textContent?.includes("New spec"));
    await act(async () => button?.click());
    expect(onCompose).toHaveBeenCalledWith("any");
  });

  it("reads a document as kind, title and body", async () => {
    const el = await render(
      { spec: "spc_a", readOnly: true },
      [spec("spc_a", "requirement", "Login is race-free", "No two sessions.")],
    );
    expect(el.textContent).toContain("Requirement");
    expect(el.querySelector<HTMLTextAreaElement>("textarea[aria-label='Title']")?.value)
      .toBe("Login is race-free");
    expect(el.textContent).toContain("No two sessions.");
  });

  it("revises against the head it is showing", async () => {
    const el = await render({ spec: "spc_a" });
    const title = el.querySelector<HTMLTextAreaElement>("textarea[aria-label='Title']")!;
    await act(async () => {
      // React tracks the DOM value to decide whether `change` is real.
      const setter = Object.getOwnPropertyDescriptor(
        window.HTMLTextAreaElement.prototype,
        "value",
      )!.set!;
      setter.call(title, "Login is race-free everywhere");
      title.dispatchEvent(new Event("input", { bubbles: true }));
      // React delegates `onBlur` from `focusout`; a bare `blur` never reaches it.
      title.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    // The exact predecessor, not "latest": the engine refuses a write composed
    // on a head that has since moved rather than merging it.
    expect(rpcMock).toHaveBeenCalledWith("local", {
      cmd: "spec_revise",
      spec: "spc_a",
      expected: "rev_spc_a",
      title: "Login is race-free everywhere",
    });
  });

  /** Radix opens a dropdown on `pointerdown`, not on click. */
  async function openMenu(el: HTMLElement, label: string) {
    const trigger = el.querySelector<HTMLElement>(`[aria-label='${label}']`);
    expect(trigger).toBeTruthy();
    await act(async () => {
      trigger!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));
    });
    return [...document.querySelectorAll("[role='menuitem']")];
  }

  it("offers the transitions this head can take, and gates issuing on the grant", async () => {
    const el = await render({ spec: "spc_a" }, [spec("spc_a", "requirement", "Login is race-free")]);
    const items = await openMenu(el, "Lifecycle: Draft");
    expect(items.map((item) => item.textContent)).toEqual([
      "Send for review",
      // Issuing is deliberately not ordinary contribution, so an ordinary
      // contributor sees the verb and what it would take — not an absence.
      expect.stringContaining("Needs spec.issue"),
    ]);
    expect(items[0]?.getAttribute("data-disabled")).toBeNull();
    expect(items[1]?.getAttribute("data-disabled")).not.toBeNull();
  });

  it("unlocks issuing for an actor holding the project grant", async () => {
    const el = await render(
      { spec: "spc_a" },
      [spec("spc_a", "requirement", "Login is race-free")],
      [{ grant_id: "g", actor: "act_1", world: "com.lait.issues", capability: "spec.issue", resource: ["prj_1"] }],
    );
    const issue = (await openMenu(el, "Lifecycle: Draft"))[1];
    expect(issue?.textContent).toBe("Issue");
    expect(issue?.getAttribute("data-disabled")).toBeNull();
  });

  it("keeps the issued revision visible while a draft successor is open", async () => {
    const el = await render({ spec: "spc_a" }, [
      spec("spc_a", "requirement", "Login is race-free", "", {
        state: "draft",
        issued: ["rev_older"],
      }),
    ]);
    expect(el.textContent).toContain("is issued and governs");
    expect(el.textContent).toContain("This draft has not replaced it");
  });

  it("suppresses every transition while heads are concurrent", async () => {
    const el = await render({ spec: "spc_a" }, [
      spec("spc_a", "requirement", "Login is race-free", "", { heads: ["rev_a", "rev_b"] }),
    ]);
    expect(el.textContent).toContain("2 concurrent head revisions");
    expect(el.textContent).toContain("no revision wins by arriving later");
    // The state still reads; there is simply nothing to open, because the engine
    // refuses a transition whose expected head is one of several.
    expect(el.querySelector("[aria-label^='Lifecycle:']")).toBeNull();
  });

  it("shows no rail until there is a second revision to go back to", async () => {
    const view = spec("spc_a", "requirement", "Login is race-free");
    const only: SpecRevision[] = [
      { revision: "rev_spc_a", predecessors: [], body: view.body },
    ];
    const el = await render({ spec: "spc_a" }, [view], [], only);
    expect(el.textContent).not.toContain("revisions");
  });

  it("lists revisions newest first, marking which one governs", async () => {
    const view = spec("spc_a", "requirement", "Login is race-free", "", {
      state: "draft",
      issued: ["rev_1"],
    });
    const history: SpecRevision[] = [
      { revision: "rev_0", predecessors: [], body: { ...view.body, title: "First cut" } },
      { revision: "rev_1", predecessors: ["rev_0"], body: { ...view.body, state: "issued" } },
      { revision: "rev_spc_a", predecessors: ["rev_1"], body: view.body },
    ];
    const el = await render({ spec: "spc_a" }, [view], [], history);
    const rail = [...el.querySelectorAll("button")].find((b) => b.textContent?.includes("3 revisions"))!;
    expect(rail).toBeTruthy();
    await act(async () => rail.click());

    const entries = [...el.querySelectorAll("ol li")].map((li) => li.textContent ?? "");
    expect(entries).toHaveLength(3);
    // Newest at the top, and the governing revision is the middle one — the
    // rail is where you ask "which of these is real", and it is not the newest.
    expect(entries[0]).toContain("rev_spc_");
    expect(entries[1]).toContain("governs");
    expect(entries[2]).not.toContain("governs");
  });

  it("opens a historical revision as a record, not a draft", async () => {
    const view = spec("spc_a", "requirement", "Login is race-free", "Current wording.");
    const history: SpecRevision[] = [
      {
        revision: "rev_0",
        predecessors: [],
        body: { ...view.body, title: "First cut", text: "Older wording." },
      },
      { revision: "rev_spc_a", predecessors: ["rev_0"], body: view.body },
    ];
    const el = await render({ spec: "spc_a" }, [view], [], history);
    await act(async () => {
      [...el.querySelectorAll("button")].find((b) => b.textContent?.includes("2 revisions"))!.click();
    });
    await act(async () => {
      [...el.querySelectorAll<HTMLButtonElement>("ol li button")]
        .find((b) => b.textContent?.startsWith("rev_0"))!
        .click();
    });

    expect(el.textContent).toContain("This is a record");
    expect(el.textContent).toContain("Older wording.");
    // Editing the past would mean rewriting history or silently forking it.
    expect(el.querySelector<HTMLTextAreaElement>("textarea[aria-label='Title']")?.readOnly).toBe(true);
    expect(el.querySelector("[aria-label^='Lifecycle:']")).toBeNull();
  });

  it("resolves concurrent heads into a successor of every head", async () => {
    const view = spec("spc_a", "requirement", "Left wording", "", { heads: ["rev_l", "rev_r"] });
    const history: SpecRevision[] = [
      { revision: "rev_base", predecessors: [], body: { ...view.body, title: "Base" } },
      { revision: "rev_l", predecessors: ["rev_base"], body: { ...view.body, title: "Left wording" } },
      { revision: "rev_r", predecessors: ["rev_base"], body: { ...view.body, title: "Right wording" } },
    ];
    const el = await render({ spec: "spc_a" }, [view], [], history);

    await act(async () => {
      [...el.querySelectorAll("button")].find((b) => b.textContent === "Resolve…")!.click();
    });
    expect(el.textContent).toContain("Resolve 2 heads");

    // The commit is gated on having read them: the engine refuses a stale head
    // set, and a person who has not looked at both is not resolving anything.
    const commit = () =>
      [...el.querySelectorAll("button")].find((b) => b.textContent === "Create resolution draft")!;
    expect(commit().disabled).toBe(true);
    await act(async () => {
      el.querySelector<HTMLInputElement>("input[type='checkbox']")!.click();
    });
    await act(async () => commit().click());

    expect(rpcMock).toHaveBeenCalledWith("local", expect.objectContaining({
      cmd: "spec_resolve",
      spec: "spc_a",
      // Every head, so a third arriving mid-resolution makes this fail rather
      // than silently dropping a branch.
      expected_heads: ["rev_l", "rev_r"],
    }));
  });

  it("leaves an unchanged title alone", async () => {
    const el = await render({ spec: "spc_a" });
    const title = el.querySelector<HTMLTextAreaElement>("textarea[aria-label='Title']")!;
    await act(async () => {
      title.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    expect(rpcMock).not.toHaveBeenCalledWith(
      "local",
      expect.objectContaining({ cmd: "spec_revise" }),
    );
  });
});
