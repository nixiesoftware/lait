import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The mount a product request is addressed to.
 *
 * A head answers for exactly one World and refuses every other mount *by name*.
 * The page hardcoded `issues`, which is the mount the World publishes and the
 * mount every installed release is served at — and the wrong one for a local
 * World, which the host assigns a mount in its own namespace so a tree being
 * worked on cannot answer for the release it was copied from.
 *
 * The page could not tell, so every request it made was refused with the name
 * of a World it was not, and the surface drew "the local projection could not
 * be loaded" over a head that was working perfectly.
 */
describe("the mount a request is addressed to", () => {
  const calls: string[] = [];

  /** Answer `/api/spaces` with `world`, and record every other URL asked for. */
  const head = (world?: string) =>
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      calls.push(url);
      const body = url.startsWith("/api/spaces?") || url === "/api/spaces"
        ? { spaces: [], ...(world === undefined ? {} : { world }) }
        : { kind: "ok" };
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

  beforeEach(() => {
    calls.length = 0;
    vi.resetModules();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("is the one this head said it serves", async () => {
    vi.stubGlobal("fetch", head("local_issues"));
    const api = await import("./api");
    await api.rpc("orb_x", { cmd: "board" } as never);
    expect(calls).toContain("/api/spaces/orb_x/worlds/local_issues/rpc");
    expect(calls).not.toContain("/api/spaces/orb_x/worlds/issues/rpc");
  });

  it("is the World's published name when a head does not say", async () => {
    vi.stubGlobal("fetch", head(undefined));
    const api = await import("./api");
    await api.rpc("orb_x", { cmd: "board" } as never);
    expect(calls).toContain("/api/spaces/orb_x/worlds/issues/rpc");
  });

  /**
   * Learning the mount must not cost a request per call. It is one fact about
   * one head, and a board that fires twenty reads on mount would otherwise ask
   * twenty times.
   */
  it("is asked for once, however many requests follow", async () => {
    vi.stubGlobal("fetch", head("local_issues"));
    const api = await import("./api");
    await Promise.all([
      api.rpc("orb_x", { cmd: "board" } as never),
      api.rpc("orb_x", { cmd: "list" } as never),
      api.rpc("orb_x", { cmd: "inbox" } as never),
    ]);
    const asked = calls.filter((url) => url === "/api/spaces").length;
    expect(asked).toBe(1);
  });
});
