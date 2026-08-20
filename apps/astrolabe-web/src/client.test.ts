import { afterEach, describe, expect, it, vi } from "vitest";

import {
  actionKey,
  createClientTransport,
  createFixtureTransport,
  currentOwnedWindowSurface,
  fixtureClientView,
  keyFor,
  type AstrolabeClientBridge,
} from "./client";

describe("client transport", () => {
  afterEach(() => {
    vi.useRealTimers();
    delete window.__ASTROLABE_CLIENT__;
  });

  it("uses the core action-key vocabulary", () => {
    expect(keyFor({ type: "refresh" })).toBe(actionKey.refresh);
    expect(keyFor({ type: "open", entryPath: "/issues" })).toBe("open:/issues");
    expect(keyFor({ type: "updateWorld", world: "issues" })).toBe("world.update:issues");
    expect(keyFor({ type: "stopHead", id: "identity:default" })).toBe("head.stop:identity:default");
    expect(keyFor({ type: "bookMerge", from: "old", into: "new" })).toBe("book.merge:old:new");
    expect(keyFor({ type: "installMcp", client: "claude", scope: null, name: "lait", agent: null, noAgent: false, project: "/project", world: null, preview: true })).toBe("mcp.preview");
    expect(keyFor({ type: "displayAssignmentPut", device: "receiver", orbit: "space", world: "issues", surface: "board", inputJson: "{}", theme: "dark", staleAfterMs: 60_000, onStale: "keepWithNativeBanner", syncGroup: null, syncMode: "stayInSync", staticDelayMs: 0, expiresAtUnixMs: null })).toBe("display.assignment.put:receiver");
  });

  it("returns the in-flight snapshot before publishing the later completion", async () => {
    vi.useFakeTimers();
    const transport = createFixtureTransport(fixtureClientView);
    const observed: string[][] = [];
    const stop = transport.watch((view) => observed.push(view.inFlight));

    const immediate = await transport.dispatch({ type: "open", entryPath: "/" });
    expect(immediate.inFlight).toEqual(["open:/"]);
    expect(observed).toEqual([["open:/"]]);

    await vi.advanceTimersByTimeAsync(500);
    const complete = await transport.current();
    expect(complete.inFlight).toEqual([]);
    expect(complete.heads).toMatchObject([{ id: "identity:fixture", orbit: null, owned: true }]);
    stop();
  });

  it("prefers the desktop host bridge over a development fixture", async () => {
    const bridge: AstrolabeClientBridge = {
      current: async () => fixtureClientView,
      watch: () => () => undefined,
      dispatch: async () => fixtureClientView,
    };
    window.__ASTROLABE_CLIENT__ = bridge;

    const transport = createClientTransport();
    expect(transport.mode).toBe("host");
    expect(await transport.current()).toBe(fixtureClientView);
  });

  it("only routes Flutter-owned top-level surfaces into secondary windows", () => {
    expect(currentOwnedWindowSurface(new URL("https://astrolabe.test/?surface=book") as unknown as Location)).toBe("book");
    expect(currentOwnedWindowSurface(new URL("https://astrolabe.test/?surface=displays") as unknown as Location)).toBe("displays");
    expect(currentOwnedWindowSurface(new URL("https://astrolabe.test/?surface=record") as unknown as Location)).toBeNull();
  });

  it("settles fixture-only lifecycle actions", async () => {
    vi.useFakeTimers();
    const transport = createFixtureTransport(fixtureClientView);
    const starting = await transport.dispatch({ type: "bookImport", path: "/cards.json" });
    expect(starting.inFlight).toEqual(["book.import"]);
    await vi.advanceTimersByTimeAsync(500);
    expect((await transport.current()).inFlight).toEqual([]);
  });
});
