import { afterEach, describe, expect, it, vi } from "vitest";

import {
  actionKey,
  createClientTransport,
  createFixtureTransport,
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
});
