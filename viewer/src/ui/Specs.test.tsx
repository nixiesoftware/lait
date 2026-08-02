import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { WorldViewStoreProvider } from "../core/worldViewReact";
import { ProjectViewerStore, ProjectViewerStoreProvider } from "../projectStore";
import type { SpecKind, SpecView } from "../types";
import { Specs } from "./Specs";
import { TooltipProvider } from "./primitives";

const rpcMock = vi.hoisted(() => vi.fn());
const spaceRpcMock = vi.hoisted(() => vi.fn());
vi.mock("../api", () => ({ rpc: rpcMock, spaceRpc: spaceRpcMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

function spec(id: string, kind: SpecKind, title: string, text = ""): SpecView {
  const body = {
    spec: id,
    project: "prj_1",
    kind,
    title,
    text,
    state: "draft" as const,
    links: [],
    author: "act_1",
    ts: 1_770_000_000,
  };
  return {
    spec: id,
    project: "prj_1",
    kind,
    title,
    state: "draft",
    revision: `rev_${id}`,
    heads: [`rev_${id}`],
    issued: [],
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

  async function render(props: Partial<React.ComponentProps<typeof Specs>> = {}, specs = SPECS) {
    rpcMock.mockImplementation((_space: string, request: { cmd: string; spec?: string }) => {
      if (request.cmd === "spec_list") return Promise.resolve({ kind: "specs", specs });
      if (request.cmd === "spec_show") {
        const found = specs.find((candidate) => candidate.spec === request.spec);
        return Promise.resolve({ kind: "spec", spec: found });
      }
      if (request.cmd === "spec_revise") {
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
