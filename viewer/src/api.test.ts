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

  it("refreshes a stale mount and safely retries a request once", async () => {
    let lookups = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      calls.push(url);
      if (url === "/api/spaces") {
        lookups += 1;
        return Response.json({ spaces: [], world: lookups === 1 ? "issues" : "local_issues" });
      }
      if (url.endsWith("/worlds/issues/rpc")) {
        return Response.json({
          kind: "error",
          error_kind: "not_found",
          message: "this head serves 'local_issues' and not 'issues'; open that World's own head",
        }, { status: 404 });
      }
      return Response.json({ kind: "ok" });
    }));

    const api = await import("./api");
    await api.rpc("orb_x", { cmd: "change_set" } as never);

    expect(calls).toEqual([
      "/api/spaces",
      "/api/spaces/orb_x/worlds/issues/rpc",
      "/api/spaces",
      "/api/spaces/orb_x/worlds/local_issues/rpc",
    ]);
  });

  it("does not replay an ordinary not-found response", async () => {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      calls.push(url);
      if (url === "/api/spaces") return Response.json({ spaces: [], world: "local_issues" });
      return Response.json({
        kind: "error",
        error_kind: "not_found",
        message: "issue does not exist",
      }, { status: 404 });
    }));

    const api = await import("./api");
    await expect(api.rpc("orb_x", { cmd: "issue_detail" } as never))
      .rejects.toThrow("issue does not exist");
    expect(calls).toEqual([
      "/api/spaces",
      "/api/spaces/orb_x/worlds/local_issues/rpc",
    ]);
  });
});
