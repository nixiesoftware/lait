import { beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineLink, LinkReply } from "./link";
import type { SocketEvent } from "./socket";
import { SocketMutationError, type Socket } from "./socket";

/**
 * The engine link is the one seam between the page and whatever answers it.
 * These tests hold its contract from the page's side: a bound backend answers
 * everything, refusals cross as data and come back as the error classes, and
 * a backend's refusal must never be mistaken for the native head's
 * wrong-mount topology — the two failure modes that matter are a replayed
 * request against a backend that answered honestly, and editor writes
 * silently rerouted to a fetch with no server behind it.
 */

const reply = (body: unknown): LinkReply => ({ kind: "reply", body });

/** A backend that answers the control plane and refuses the product plane. */
function fakeLink(overrides: Partial<EngineLink> = {}): EngineLink & {
  calls: string[];
} {
  const calls: string[] = [];
  return {
    calls,
    spaces: async () => {
      calls.push("spaces");
      return reply({ spaces: [], world: "tab_issues" });
    },
    hostRpc: async () => {
      calls.push("host");
      return reply({ kind: "ok" });
    },
    spaceRpc: async () => {
      calls.push("space");
      return reply({ kind: "ok" });
    },
    worldRpc: async () => {
      calls.push("world");
      return reply({ kind: "ok" });
    },
    events: () => () => {},
    session: () => {
      throw new Error("no session in this test");
    },
    ...overrides,
  };
}

beforeEach(() => {
  vi.resetModules();
});

describe("binding a backend", () => {
  it("routes every plane through the bound link", async () => {
    const link = fakeLink();
    const { bindEngineLink } = await import("./link");
    const api = await import("./api");
    bindEngineLink(link);

    await api.spaces();
    await api.hostRpc({ cmd: "host_context" } as never);
    await api.spaceRpc("orb_x", { cmd: "whoami" } as never);
    await api.rpc("orb_x", { cmd: "board" } as never);

    // No second "spaces": the explicit call above already taught the mount.
    expect(link.calls).toEqual(["spaces", "host", "space", "world"]);
  });

  it("addresses the mount the bound backend says it serves", async () => {
    const worlds: string[] = [];
    const link = fakeLink({
      worldRpc: async (_space, world) => {
        worlds.push(world);
        return reply({ kind: "ok" });
      },
    });
    const { bindEngineLink } = await import("./link");
    const api = await import("./api");
    bindEngineLink(link);

    await api.rpc("orb_x", { cmd: "board" } as never);
    expect(worlds).toEqual(["tab_issues"]);
  });

  it("drops the mount cache on rebind — the mount is a fact about one backend", async () => {
    const { bindEngineLink } = await import("./link");
    const api = await import("./api");

    const first = fakeLink();
    bindEngineLink(first);
    await api.rpc("orb_x", { cmd: "board" } as never);

    const worlds: string[] = [];
    const second = fakeLink({
      spaces: async () => reply({ spaces: [], world: "other_issues" }),
      worldRpc: async (_space, world) => {
        worlds.push(world);
        return reply({ kind: "ok" });
      },
    });
    bindEngineLink(second);
    await api.rpc("orb_x", { cmd: "board" } as never);
    expect(worlds).toEqual(["other_issues"]);
  });
});

describe("refusals crossing the seam as data", () => {
  it("come back as LaitError with the backend's own error kind", async () => {
    const link = fakeLink({
      worldRpc: async () => ({
        kind: "refusal",
        refusal: {
          status: 501,
          message: "this tab holds no runner for 'issues'",
          errorKind: "unavailable",
        },
      }),
    });
    const { bindEngineLink } = await import("./link");
    const api = await import("./api");
    bindEngineLink(link);

    const thrown = await api.rpc("orb_x", { cmd: "board" } as never).catch((e) => e);
    expect(thrown).toBeInstanceOf(api.LaitError);
    expect((thrown as InstanceType<typeof api.LaitError>).errorKind).toBe("unavailable");
    // The classification side table learned the kind, so surfaces that only
    // hold the message string still classify it.
    expect(api.errorKindOf(thrown.message)).toBe("unavailable");
  });

  it("come back as ConfirmRequired when the backend asks a question", async () => {
    const link = fakeLink({
      spaceRpc: async () => ({ kind: "confirm", question: "Delete this device's data?" }),
    });
    const { bindEngineLink } = await import("./link");
    const api = await import("./api");
    bindEngineLink(link);

    const thrown = await api
      .spaceRpc("orb_x", { cmd: "member_remove" } as never)
      .catch((e) => e);
    expect(thrown).toBeInstanceOf(api.ConfirmRequired);
    expect((thrown as InstanceType<typeof api.ConfirmRequired>).question).toBe(
      "Delete this device's data?",
    );
  });

  /**
   * The wrong-mount retry is native-head topology: a desktop window outliving
   * its head process. A backend's honest refusal must not trigger it — the
   * replay would re-ask a backend that already answered, and a broken backend
   * would be asked twice for every request.
   */
  it("do not trigger the wrong-mount replay", async () => {
    let asked = 0;
    const link = fakeLink({
      worldRpc: async () => {
        asked += 1;
        return {
          kind: "refusal",
          refusal: { status: 404, message: "no such issue", errorKind: "not_found" },
        };
      },
    });
    const { bindEngineLink } = await import("./link");
    const api = await import("./api");
    bindEngineLink(link);

    await api.rpc("orb_x", { cmd: "board" } as never).catch(() => {});
    expect(asked).toBe(1);
    expect(link.calls.filter((c) => c === "spaces")).toHaveLength(1);
  });
});

describe("a backend session that refuses a mutation", () => {
  /**
   * `LivePlane` reroutes editor writes to HTTP after the head's wrong-mount
   * refusal — a compatibility path for one topology. Any other refusal must
   * propagate and leave the socket in charge, or a backend with no server
   * behind it would have every keystroke re-fail over a fetch.
   */
  it("does not silently reroute editor writes", async () => {
    const { LivePlane } = await import("./live");
    const { WorldViewStore } = await import("./core/worldViewStore");

    const mutations: string[] = [];
    const socket: Socket = {
      watch: () => {},
      mutate: async (_space, request) => {
        mutations.push((request as { cmd: string }).cmd);
        throw new SocketMutationError("this tab holds no runner", 501, "unavailable");
      },
      close: () => {},
    };
    const fallback = vi.fn();
    const plane = new LivePlane(
      new WorldViewStore(),
      (onEvent: (e: SocketEvent) => void) => {
        onEvent({ kind: "liveness", liveness: "live" });
        return socket;
      },
      () => 0,
      fallback as never,
    );

    await expect(
      plane.mutate("orb_x", { cmd: "issue_edit" } as never),
    ).rejects.toBeInstanceOf(SocketMutationError);
    // A second write still goes to the socket: the refusal did not flip the
    // plane onto HTTP.
    await plane.mutate("orb_x", { cmd: "issue_edit" } as never).catch(() => {});
    expect(mutations).toEqual(["issue_edit", "issue_edit"]);
    expect(fallback).not.toHaveBeenCalled();
  });
});
