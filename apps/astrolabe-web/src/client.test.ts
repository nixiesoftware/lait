import { afterEach, describe, expect, it, vi } from "vitest";

import {
  actionKey,
  createClientTransport,
  createFixtureTransport,
  currentOwnedWindowSurface,
  currentWorldSettingsSnapshot,
  decodeWorldSettingsSnapshot,
  encodeWorldSettingsSnapshot,
  fixtureClientView,
  keyFor,
  type AstrolabeClientBridge,
  type WorldSettingsSnapshot,
} from "./client";

describe("client transport", () => {
  afterEach(() => {
    vi.useRealTimers();
    delete window.__ASTROLABE_CLIENT__;
  });

  it("uses the core action-key vocabulary", () => {
    expect(keyFor({ type: "refresh" })).toBe(actionKey.refresh);
    expect(keyFor({ type: "open", world: "issues", entryPath: "/" })).toBe("open:issues");
    expect(keyFor({ type: "enterPresentation" })).toBe("present.enter");
    expect(keyFor({ type: "updateWorld", world: "issues" })).toBe("world.update:issues");
    expect(keyFor({ type: "installWorld", world: "issues" })).toBe("world.install:issues");
    // The channel is deliberately not in the key: one control per World, and a
    // key that varied with the value it sends would leave the old value's key
    // in flight while the new one dispatched.
    expect(keyFor({ type: "followWorldChannel", world: "issues", channel: "test" })).toBe("world.channel:issues");
    expect(keyFor({ type: "stopHead", id: "identity:default" })).toBe("head.stop:identity:default");
    expect(keyFor({ type: "bookMerge", from: "old", into: "new" })).toBe("book.merge:old:new");
    expect(keyFor({ type: "installMcp", client: "claude", scope: null, name: "lait", agent: null, noAgent: false, project: "/project", world: null, preview: true })).toBe("mcp.preview");
    expect(keyFor({ type: "displayAssignmentPut", device: "receiver", orbit: "space", world: "issues", surface: "board", inputJson: "{}", theme: "dark", staleAfterMs: 60_000, onStale: "keepWithNativeBanner", syncGroup: null, syncMode: "stayInSync", staticDelayMs: 0, expiresAtUnixMs: null })).toBe("display.assignment.put:receiver");
  });

  // These four are spelled in `Action::key` in tools/astrolabe/src/runtime.rs.
  // A key that disagrees does not fail anywhere — it just never matches
  // `inFlight`, so the control stays live through its own action and can be
  // pressed twice. That is a silent defect, so the spelling is pinned.
  it("spells the reach and invitation keys the way the core does", () => {
    expect(keyFor({ type: "shareReach" })).toBe("reach.share");
    expect(keyFor({ type: "addCorrespondent", announcement: "…" })).toBe("reach.add");
    expect(keyFor({ type: "openInvitation", message: "dep_7" })).toBe("invitation.open:dep_7");
    expect(keyFor({ type: "sendInvitation", to: "peer_ada", link: "lait://…" })).toBe(
      "invitation.send:peer_ada",
    );
  });

  // The same two-sided edit, for the two pairing acts. `Action::key` spells
  // these strings in tools/astrolabe/src/runtime.rs; renaming one side leaves
  // Add device and the two answers to an offer live through their own action.
  it("spells the device-pairing keys the way the core does", () => {
    expect(keyFor({ type: "devicePairEnter", code: "ABCD-EFGH" })).toBe("device.pair.enter");
    expect(keyFor({ type: "devicePairConfirm", pairing: "pai_7", accept: true })).toBe(
      "device.pair.confirm:pai_7",
    );
  });

  it("returns the in-flight snapshot before publishing the later completion", async () => {
    vi.useFakeTimers();
    const transport = createFixtureTransport(fixtureClientView);
    const observed: string[][] = [];
    const stop = transport.watch((view) => observed.push(view.inFlight));

    const immediate = await transport.dispatch({ type: "open", world: "issues", entryPath: "/" });
    expect(immediate.inFlight).toEqual(["open:issues"]);
    expect(observed).toEqual([["open:issues"]]);

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
    expect(currentOwnedWindowSurface(new URL("https://astrolabe.test/?surface=displays") as unknown as Location)).toBe("devices");
    expect(currentOwnedWindowSurface(new URL("https://astrolabe.test/?surface=devices") as unknown as Location)).toBe("devices");
    expect(currentOwnedWindowSurface(new URL("https://astrolabe.test/?surface=record") as unknown as Location)).toBeNull();
  });

  it("carries a World-settings snapshot through its window URL, whole", () => {
    const snapshot: WorldSettingsSnapshot = {
      key: "issues",
      name: "Issues — Ünïcode",
      worldMount: "issues",
      entryPath: "/",
      version: 3,
      activeOrigin: "http://127.0.0.1:52713/",
      dark: true,
    };
    const encoded = encodeWorldSettingsSnapshot(snapshot);
    // The payload must survive a URL query without further escaping.
    expect(encoded).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(decodeWorldSettingsSnapshot(encoded)).toEqual(snapshot);

    const url = new URL(`https://astrolabe.test/?surface=world-settings&snapshot=${encoded}`);
    expect(currentWorldSettingsSnapshot(url as unknown as Location)).toEqual(snapshot);
    // The settings window is not one of the two owned singleton surfaces.
    expect(currentOwnedWindowSurface(url as unknown as Location)).toBeNull();
    // A mangled payload reads as no snapshot, never a crash or a guess.
    expect(decodeWorldSettingsSnapshot("not-base64!@#")).toBeNull();
    expect(decodeWorldSettingsSnapshot(encodeWorldSettingsSnapshot({ ...snapshot, key: undefined as unknown as string }))).toBeNull();
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
