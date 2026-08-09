/** @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { WorldViewStoreProvider, useWorldResources } from "./worldViewReact";
import { WorldViewStore } from "./worldViewStore";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

describe("useWorldResources", () => {
  let root: Root | null = null;
  let host: HTMLDivElement | null = null;

  afterEach(() => {
    act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  function render(store: WorldViewStore, keys: string[], load: (key: string) => Promise<string>) {
    host ??= document.body.appendChild(document.createElement("div"));
    root ??= createRoot(host);
    act(() => {
      root!.render(
        <WorldViewStoreProvider store={store}>
          <Probe keys={keys} load={load} />
        </WorldViewStoreProvider>,
      );
    });
  }

  it("keeps every member active and redraws when any member changes", async () => {
    const store = new WorldViewStore();
    const load = vi.fn(async (key: string) => key.toUpperCase());
    render(store, ["a", "b"], load);
    await act(async () => undefined);

    expect(store.isActive("a")).toBe(true);
    expect(store.isActive("b")).toBe(true);
    expect(host?.textContent).toBe("A,B");

    act(() => store.set("b", "changed"));
    expect(host?.textContent).toBe("A,changed");
  });

  it("moves subscriptions when membership changes", async () => {
    const store = new WorldViewStore();
    const load = vi.fn(async (key: string) => key);
    render(store, ["a", "b"], load);
    await act(async () => undefined);

    render(store, ["b", "c"], load);
    await act(async () => undefined);
    expect(store.isActive("a")).toBe(false);
    expect(store.isActive("b")).toBe(true);
    expect(store.isActive("c")).toBe(true);
    expect(host?.textContent).toBe("b,c");
  });
});

function Probe({ keys, load }: { keys: string[]; load: (key: string) => Promise<string> }) {
  const resources = useWorldResources(keys, load);
  return <>{resources.map((resource) => resource.data ?? "…").join(",")}</>;
}
