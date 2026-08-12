import { describe, expect, it } from "vitest";

import { DEFAULT_ROUTE, formatRoute, loadLastRoute, parseRoute, resolveLocalSpace, sameRoute, saveLastRoute } from "./route";
import { EMPTY_FILTER } from "./filter";
import type { SpaceRow } from "../types";

describe("viewer routes", () => {
  it("uses the neutral list route for non-viewer paths", () => {
    expect(parseRoute({ pathname: "/", search: "" })).toEqual(DEFAULT_ROUTE);
    expect(parseRoute({ pathname: "/assets/app.js", search: "" })).toEqual(DEFAULT_ROUTE);
  });

  it("round-trips canonical product identity without local machine state", () => {
    const route = {
      spaceId: "ws_alpha/beta",
      project: "LAIT WEB",
      view: "board" as const,
      issue: "iss_42/7",
    };
    const href = formatRoute(route);

    expect(href).toBe(
      "/spaces/ws_alpha%2Fbeta/projects/LAIT%20WEB/board?issue=iss_42%2F7",
    );
    expect(parseRoute(new URL(href, "http://lait.local"))).toEqual(route);
    expect(href).not.toMatch(/token|seed|path|daemon/i);
  });

  it("falls back to list for unknown views and ignores empty selections", () => {
    expect(
      parseRoute({ pathname: "/spaces/ws_1/not-a-view", search: "?project=&issue=%20" }),
    ).toEqual({ spaceId: "ws_1", project: null, view: "list", issue: null });
  });

  it("round-trips a legible applied filter", () => {
    const href = formatRoute({
      spaceId: "ws_1",
      project: "WEB",
      view: "list",
      issue: null,
      filter: { text: "login bug", mine: true, label: "customer", status: ["todo", "doing"], priority: [], assignees: [], milestone: "mls_1" },
    });
    expect(parseRoute(new URL(href, "http://lait.local")).filter).toEqual({
      text: "login bug",
      mine: true,
      label: "customer",
      status: ["todo", "doing"],
      priority: [],
      assignees: [],
      milestone: "mls_1",
    });
  });

  it("keeps the No-milestone bucket distinct from no milestone filter", () => {
    // `?milestone=` present-but-empty means "issues nobody has scoped yet" — a
    // selection you can only reach this way. Folding it back to `null` on the
    // round trip would silently widen the view to every issue in the project.
    const bucket = formatRoute({
      spaceId: "ws_1",
      project: "WEB",
      view: "board",
      issue: null,
      filter: { ...EMPTY_FILTER, milestone: "" },
    });
    expect(bucket).toContain("milestone=");
    expect(parseRoute(new URL(bucket, "http://lait.local")).filter?.milestone).toBe("");

    // ...and absent stays absent, rather than becoming the bucket.
    expect(
      parseRoute({ pathname: "/spaces/ws_1/projects/WEB/board", search: "" }).filter,
    ).toBeUndefined();
  });

  it("does not carry an issue selection onto surfaces that cannot display it", () => {
    expect(
      parseRoute({ pathname: "/spaces/ws_1/inbox", search: "?issue=iss_1" }),
    ).toEqual({ spaceId: "ws_1", project: null, view: "inbox", issue: null });
    expect(
      formatRoute({ spaceId: "ws_1", project: null, view: "inbox", issue: "iss_1" }),
    ).toBe("/spaces/ws_1/inbox");
  });

  it("does not carry project scope onto workspace destinations", () => {
    expect(
      parseRoute({ pathname: "/spaces/ws_1/settings", search: "?project=WEB&q=stale&mine=1" }),
    ).toEqual({ spaceId: "ws_1", project: null, view: "settings", issue: null });
    expect(
      formatRoute({
        spaceId: "ws_1",
        project: "WEB",
        view: "settings",
        issue: null,
        filter: {
          text: "stale",
          mine: true,
          label: null,
          status: [],
          priority: [],
          assignees: [],
          milestone: null,
        },
      }),
    ).toBe("/spaces/ws_1/settings");
    expect(
      formatRoute({
        spaceId: "ws_1",
        project: "WEB",
        view: "my-issues",
        issue: null,
      }),
    ).toBe("/spaces/ws_1/my-issues");
  });

  it("round-trips an open spec under its project's register", () => {
    const href = formatRoute({
      spaceId: "ws_1",
      project: "WEB",
      view: "specs",
      issue: null,
      spec: "spc_01JV0IUE",
    });
    expect(href).toBe("/spaces/ws_1/projects/WEB/specs?spec=spc_01JV0IUE");
    expect(parseRoute(new URL(href, "http://lait.local"))).toEqual({
      spaceId: "ws_1",
      project: "WEB",
      view: "specs",
      issue: null,
      spec: "spc_01JV0IUE",
    });
  });

  it("does not carry a spec selection onto surfaces that cannot display it", () => {
    // The key is absent rather than null when nothing is open, so a register
    // with no document open is the same route as one that never had a spec.
    expect(
      parseRoute({ pathname: "/spaces/ws_1/projects/WEB/issues", search: "?spec=spc_1" }),
    ).toEqual({ spaceId: "ws_1", project: "WEB", view: "list", issue: null });
    expect(
      formatRoute({ spaceId: "ws_1", project: "WEB", view: "list", issue: null, spec: "spc_1" }),
    ).toBe("/spaces/ws_1/projects/WEB/issues");
  });

  it("redirects legacy members routes into the settings shell", () => {
    expect(
      parseRoute({ pathname: "/spaces/ws_1/members", search: "?project=WEB" }),
    ).toEqual({ spaceId: "ws_1", project: null, view: "settings", issue: null });
  });

  it("round-trips the durable project portfolio destination", () => {
    const href = formatRoute({
      spaceId: "ws_1",
      project: null,
      view: "projects",
      issue: null,
    });
    expect(href).toBe("/spaces/ws_1/projects");
    expect(parseRoute(new URL(href, "http://lait.local"))).toMatchObject({
      spaceId: "ws_1",
      view: "projects",
      issue: null,
    });
  });

  it("uses canonical nested routes for project homes", () => {
    const overview = "/spaces/ws_1/projects/WEB/overview";
    expect(formatRoute({
      spaceId: "ws_1", project: "WEB", view: "overview", issue: null,
    })).toBe(overview);
    expect(parseRoute(new URL(overview, "http://lait.local"))).toEqual({
      spaceId: "ws_1", project: "WEB", view: "overview", issue: null,
    });
    expect(parseRoute({
      pathname: "/spaces/ws_1/projects/WEB/issues",
      search: "?issue=WEB-4",
    })).toMatchObject({
      project: "WEB", view: "list", issue: "WEB-4",
    });
  });

  it("upgrades legacy project query routes", () => {
    expect(parseRoute({
      pathname: "/spaces/ws_1/list",
      search: "?project=WEB&issue=WEB-4",
    })).toMatchObject({
      project: "WEB", view: "list", issue: "WEB-4",
    });
    expect(parseRoute({
      pathname: "/spaces/ws_1/projects",
      search: "?overview=WEB",
    })).toEqual({
      spaceId: "ws_1", project: "WEB", view: "overview", issue: null,
    });
  });

  it("accepts a legacy focus=1 link and drops the parameter", () => {
    // There is one way to read an issue now, so `focus=1` names nothing. An old
    // link must still open the issue it names — and then stop saying it.
    const legacy = parseRoute(new URL("/spaces/ws_1/projects/WEB/issues?issue=iss_1&focus=1", "http://lait.local"));
    expect(legacy.issue).toBe("iss_1");
    expect(formatRoute(legacy)).toBe("/spaces/ws_1/projects/WEB/issues?issue=iss_1");
  });

  it("compares every shareable route dimension", () => {
    const route = { spaceId: "ws_1", project: "WEB", view: "list" as const, issue: "iss_1" };
    expect(sameRoute(route, { ...route })).toBe(true);
    expect(sameRoute(route, { ...route, issue: "iss_2" })).toBe(false);
    expect(sameRoute(route, { ...route, project: "APP" })).toBe(false);
  });

  it("resolves canonical identity to a local target and prefers our actor", () => {
    const own = space("local-path-hash-own", "ws_shared", { kind: "own" });
    const agent = space("local-path-hash-agent", "ws_shared", { kind: "agent", name: "bot" });
    expect(resolveLocalSpace("ws_shared", [agent, own])).toBe(own);
    expect(resolveLocalSpace("ws_missing", [own])).toBeNull();
  });

  it("chooses the most recently opened replica deterministically", () => {
    const older = { ...space("own-a", "ws_shared", { kind: "own" }), last_opened: 10 };
    const newer = { ...space("own-b", "ws_shared", { kind: "own" }), last_opened: 20 };
    const agent = { ...space("agent", "ws_shared", { kind: "agent", name: "bot" }), last_opened: 30 };

    expect(resolveLocalSpace("ws_shared", [older, agent, newer])).toBe(newer);
    expect(resolveLocalSpace("ws_shared", [older, agent])).toBe(older);
  });

  it("addresses the composer as a segment under a project's issues", () => {
    const route = {
      spaceId: "ws_alpha",
      project: "EXEC",
      view: "list" as const,
      issue: null,
      composing: true,
    };
    const href = formatRoute(route);

    expect(href).toBe("/spaces/ws_alpha/projects/EXEC/issues/new");
    expect(parseRoute(new URL(href, "http://lait.local"))).toEqual(route);
  });

  it("drops the open issue while composing, because only one thing is in front", () => {
    expect(
      formatRoute({
        spaceId: "ws_alpha",
        project: "EXEC",
        view: "list",
        issue: "iss_7",
        composing: true,
      }),
    ).toBe("/spaces/ws_alpha/projects/EXEC/issues/new");
  });

  it("has no composer without a project to file into", () => {
    expect(
      formatRoute({ spaceId: "ws_alpha", project: null, view: "list", issue: null, composing: true }),
    ).toBe("/spaces/ws_alpha/list");
    expect(
      formatRoute({ spaceId: "ws_alpha", project: "EXEC", view: "board", issue: null, composing: true }),
    ).toBe("/spaces/ws_alpha/projects/EXEC/board");
  });

  it("ignores a trailing segment the grammar never writes", () => {
    expect(
      parseRoute({ pathname: "/spaces/ws_alpha/projects/EXEC/issues/edit", search: "" }).composing,
    ).toBeUndefined();
    expect(
      parseRoute({ pathname: "/spaces/ws_alpha/projects/EXEC/board/new", search: "" }).composing,
    ).toBeUndefined();
  });

  it("tells a composing route apart from the list it stands in front of", () => {
    const list = { spaceId: "ws_alpha", project: "EXEC", view: "list" as const, issue: null };
    expect(sameRoute(list, { ...list, composing: true })).toBe(false);
    expect(sameRoute(list, { ...list, composing: false })).toBe(true);
  });

  it("keeps a settings sub-page in the address, so it survives a reload", () => {
    const route = {
      spaceId: "ws_alpha",
      project: null,
      view: "settings" as const,
      issue: null,
      tab: "members",
    };
    const href = formatRoute(route);

    expect(href).toBe("/spaces/ws_alpha/settings?tab=members");
    expect(parseRoute(new URL(href, "http://lait.local"))).toEqual(route);
  });

  it("has no sub-page outside settings, and none for the default one", () => {
    expect(
      formatRoute({ spaceId: "ws_alpha", project: null, view: "inbox", issue: null, tab: "members" }),
    ).toBe("/spaces/ws_alpha/inbox");
    expect(
      parseRoute({ pathname: "/spaces/ws_alpha/settings", search: "?tab=general" }).tab,
    ).toBeUndefined();
    expect(
      parseRoute({ pathname: "/spaces/ws_alpha/settings", search: "?tab=nonsense" }).tab,
    ).toBeUndefined();
  });

  it("restores the last canonical workspace without storing a local handle", () => {
    localStorage.clear();
    const route = { spaceId: "ws_shared", project: "WEB", view: "board" as const, issue: "iss_1" };
    saveLastRoute(route);
    expect(loadLastRoute()).toEqual(route);
    expect(localStorage.getItem("lait.last-route")).not.toContain("local-path-hash");
  });
});

function space(id: string, canonical: string, identity: SpaceRow["identity"]): SpaceRow {
  return {
    id,
    space: canonical,
    name: canonical,
    path: `C:/${id}`,
    origin: "test",
    last_opened: 0,
    status: "up",
    identity,
    projects: [],
  };
}

const loc = (pathname: string) => ({ pathname, search: "" });

describe("team scope", () => {
  it("round-trips a team's issues", () => {
    const route = parseRoute(loc("/spaces/ws_1/teams/PLAT/issues"));
    expect(route).toEqual({
      spaceId: "ws_1",
      project: null,
      team: "PLAT",
      view: "list",
      issue: null,
    });
    expect(formatRoute(route)).toBe("/spaces/ws_1/teams/PLAT/issues");
  });

  it("defaults a bare team address to its issues", () => {
    expect(parseRoute(loc("/spaces/ws_1/teams/PLAT")).view).toBe("list");
  });

  it("carries the project list and the row views, and nothing that is about one project", () => {
    for (const [segment, view] of [
      ["board", "board"],
      ["projects", "projects"],
    ] as const) {
      const route = parseRoute(loc(`/spaces/ws_1/teams/PLAT/${segment}`));
      expect([route.view, route.team]).toEqual([view, "PLAT"]);
      expect(formatRoute(route)).toBe(`/spaces/ws_1/teams/PLAT/${segment}`);
    }
    // Overview, Activity and Specs are about a single project, so a team
    // address for one of them is not a route we wrote.
    expect(parseRoute(loc("/spaces/ws_1/teams/PLAT/activity")).view).toBe("list");
  });

  /**
   * The two structural scopes are mutually exclusive: you are looking at one
   * project or at a team's worth of them, and an address carrying both would
   * have to say which won. Project does, being the narrower claim.
   */
  it("lets a project win when a navigation leaves both set", () => {
    expect(
      formatRoute({ spaceId: "ws_1", project: "ENG", team: "PLAT", view: "list", issue: null }),
    ).toBe("/spaces/ws_1/projects/ENG/issues");
  });

  it("drops team scope on a destination that has no team form", () => {
    expect(
      formatRoute({ spaceId: "ws_1", project: null, team: "PLAT", view: "settings", issue: null }),
    ).toBe("/spaces/ws_1/settings");
  });

  it("is absent rather than null when unset, so routes compare equal", () => {
    const bare = parseRoute(loc("/spaces/ws_1/list"));
    expect("team" in bare).toBe(false);
    expect(sameRoute(bare, { ...bare, team: null })).toBe(true);
    expect(sameRoute(bare, { ...bare, team: "PLAT" })).toBe(false);
  });
});
