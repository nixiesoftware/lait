// The Library's lifecycle rules, pinned because both have regressed before.
//
// Presence is not liveness: an exited head stays listed so a person can see
// the thing they opened died, so counting rows painted a crashed World as
// Running. And heads are per World: opening Issues once put Signage into
// RUNNING, and STOP on either row stopped the one head both were reading.
// The Flutter client fixed both and recorded them; this holds the Tauri
// surface to the same facts.

import { describe, expect, it } from "vitest";

import { lifecycle, servingWorld } from "./app";
import { loadingClientView, type ClientView, type Head, type LibraryWorld } from "./client";

function world(mount: string): LibraryWorld {
  return {
    key: mount,
    worldMount: mount,
    displayName: mount,
    opensAt: "/",
    version: 1,
    tagline: null,
    accent: null,
    people: null,
    update: null,
  };
}

function head(overrides: Partial<Head>): Head {
  return {
    id: "head",
    kind: "browser",
    origin: "http://127.0.0.1:1/",
    owned: true,
    orbit: null,
    world: "issues",
    state: "running",
    stateDetail: null,
    ...overrides,
  };
}

function view(heads: Head[]): ClientView {
  return { ...loadingClientView, loading: false, heads };
}

describe("the Library reads heads per World, by their own state", () => {
  it("a running head lights only the World it serves", () => {
    const shared = view([head({ world: "issues" })]);
    expect(lifecycle(shared, world("issues"))).toBe("Running");
    expect(lifecycle(shared, world("signage"))).toBe("Ready");
    expect(servingWorld(shared, "signage")).toHaveLength(0);
  });

  it("an exited head is Stopped, not Running — presence is not liveness", () => {
    const crashed = view([head({ state: "exited" })]);
    expect(lifecycle(crashed, world("issues"))).toBe("Stopped");
  });

  it("unknown outranks exited: an unpollable head is not called Stopped", () => {
    const murky = view([head({ state: "exited" }), head({ id: "second", state: "unknown" })]);
    expect(lifecycle(murky, world("issues"))).toBe("Unknown");
  });

  it("a pre-pin head (world: null) matches no row", () => {
    const unpinned = view([head({ world: null })]);
    expect(lifecycle(unpinned, world("issues"))).toBe("Ready");
    expect(servingWorld(unpinned, "issues")).toHaveLength(0);
  });

  it("a World that declares no entry stays Unavailable whatever the heads say", () => {
    const shared = view([head({ world: "issues" })]);
    expect(lifecycle(shared, { ...world("issues"), opensAt: null })).toBe("Unavailable");
  });
});
