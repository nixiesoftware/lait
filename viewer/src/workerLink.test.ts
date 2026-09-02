import { describe, expect, it } from "vitest";

import type { LinkReply } from "./link";
import {
  workerLink,
  type WorkerLinkRequest,
  type WorkerLinkResponse,
} from "./workerLink";

/**
 * The worker link's contract from the page's side: every verb crosses as one
 * clone-safe frame and settles its own promise (ids, not order), refusals
 * arrive as the data `link.ts` made them, aborts settle locally and drop the
 * late reply, and the doorbell subscription relays exactly what the engine
 * host says — after honestly reporting "connecting" first.
 */

const reply = (body: unknown): LinkReply => ({ kind: "reply", body });

/** Both ends of a real port: the link on one, the test's engine host on the other. */
function harness(answer?: (frame: WorkerLinkRequest) => void) {
  const channel = new MessageChannel();
  const seen: WorkerLinkRequest[] = [];
  channel.port2.addEventListener("message", (event: MessageEvent) => {
    const frame = event.data as WorkerLinkRequest;
    seen.push(frame);
    answer?.(frame);
  });
  channel.port2.start();
  const send = (frame: WorkerLinkResponse) => channel.port2.postMessage(frame);
  return { link: workerLink(channel.port1), seen, send };
}

const delivered = () => new Promise((r) => setTimeout(r, 0));

describe("the rpc verbs", () => {
  it("carries every verb's coordinates and answers the right promise", async () => {
    const h = harness((frame) => {
      if (frame.lait !== "rpc") return;
      h.send({ lait: "reply", id: frame.id, reply: reply({ verb: frame.verb }) });
    });

    const [spaces, host, space, world] = await Promise.all([
      h.link.spaces(),
      h.link.hostRpc({ cmd: "host_context" } as never, {}),
      h.link.spaceRpc("orb_x", { cmd: "whoami" } as never, { confirm: true }),
      h.link.worldRpc("orb_x", "tab_issues", { cmd: "board" } as never, {}),
    ]);

    expect(spaces).toEqual(reply({ verb: "spaces" }));
    expect(host).toEqual(reply({ verb: "host" }));
    expect(space).toEqual(reply({ verb: "space" }));
    expect(world).toEqual(reply({ verb: "world" }));

    const rpcs = h.seen.filter((f) => f.lait === "rpc");
    expect(rpcs.map((f) => f.verb)).toEqual(["spaces", "host", "space", "world"]);
    expect(rpcs[2]).toMatchObject({ space: "orb_x", confirm: true });
    expect(rpcs[3]).toMatchObject({ space: "orb_x", world: "tab_issues" });
    // Distinct ids: the reply routes by id, never by arrival order.
    expect(new Set(rpcs.map((f) => f.id)).size).toBe(4);
  });

  it("settles promises by id even when replies arrive out of order", async () => {
    const held: WorkerLinkRequest[] = [];
    const h = harness((frame) => {
      if (frame.lait === "rpc") held.push(frame);
    });

    const first = h.link.hostRpc({ cmd: "a" } as never, {});
    const second = h.link.hostRpc({ cmd: "b" } as never, {});
    await delivered();
    // The second request answered first.
    const [fa, fb] = held;
    h.send({ lait: "reply", id: fb!.id, reply: reply("b") });
    h.send({ lait: "reply", id: fa!.id, reply: reply("a") });

    expect(await first).toEqual(reply("a"));
    expect(await second).toEqual(reply("b"));
  });

  it("passes a refusal through as the data it crossed as", async () => {
    const refusal: LinkReply = {
      kind: "refusal",
      refusal: { status: 403, message: "read refused", errorKind: "denied" },
    };
    const h = harness((frame) => {
      if (frame.lait === "rpc") {
        h.send({ lait: "reply", id: frame.id, reply: refusal });
      }
    });
    expect(await h.link.worldRpc("orb_x", "w", { cmd: "x" } as never, {})).toEqual(
      refusal,
    );
  });

  it("translates an abort: local AbortError, an abort frame, late reply dropped", async () => {
    const held: WorkerLinkRequest[] = [];
    const h = harness((frame) => {
      if (frame.lait === "rpc") held.push(frame);
    });

    const controller = new AbortController();
    const call = h.link.hostRpc({ cmd: "slow" } as never, {
      signal: controller.signal,
    });
    await delivered();
    controller.abort();
    await expect(call).rejects.toMatchObject({ name: "AbortError" });

    await delivered();
    const aborted = held[0]!;
    expect(h.seen.some((f) => f.lait === "abort" && f.id === aborted.id)).toBe(
      true,
    );
    // The late reply finds no pending entry; nothing throws, nothing settles.
    h.send({ lait: "reply", id: aborted.id, reply: reply("late") });
    await delivered();
  });

  it("rejects immediately on an already-aborted signal, sending nothing", async () => {
    const h = harness();
    const controller = new AbortController();
    controller.abort();
    await expect(
      h.link.hostRpc({ cmd: "never" } as never, { signal: controller.signal }),
    ).rejects.toMatchObject({ name: "AbortError" });
    await delivered();
    expect(h.seen).toEqual([]);
  });
});

describe("the doorbell subscription", () => {
  it("reports connecting, then relays exactly what the engine host says", async () => {
    const h = harness();
    const rings: unknown[] = [];
    const liveness: string[] = [];
    h.link.events(
      (d) => rings.push(d),
      (l) => liveness.push(l),
    );
    expect(liveness).toEqual(["connecting"]);
    await delivered();
    const sub = h.seen.find((f) => f.lait === "events");
    expect(sub).toBeDefined();

    h.send({ lait: "liveness", id: sub!.id, liveness: "live" });
    h.send({ lait: "ring", id: sub!.id, ring: { spaces: ["orb_x"] } as never });
    // Rebaseline crosses as the null it is, not as a dropped frame.
    h.send({ lait: "ring", id: sub!.id, ring: null });
    await delivered();

    expect(liveness).toEqual(["connecting", "live"]);
    expect(rings).toEqual([{ spaces: ["orb_x"] }, null]);
  });

  it("unsubscribe closes the lane and stops relaying", async () => {
    const h = harness();
    const rings: unknown[] = [];
    const stop = h.link.events(
      (d) => rings.push(d),
      () => {},
    );
    await delivered();
    const sub = h.seen.find((f) => f.lait === "events");
    stop();
    await delivered();
    expect(h.seen.some((f) => f.lait === "close" && f.id === sub!.id)).toBe(true);

    h.send({ lait: "ring", id: sub!.id, ring: null });
    await delivered();
    expect(rings).toEqual([]);
    // A second stop sends nothing more.
    stop();
    await delivered();
    expect(h.seen.filter((f) => f.lait === "close")).toHaveLength(1);
  });
});

describe("the boundary itself", () => {
  it("ignores frames that are not the protocol's", async () => {
    const h = harness((frame) => {
      if (frame.lait === "rpc") {
        h.send({ lait: "reply", id: frame.id, reply: reply("ok") });
      }
    });
    // Noise on a shared port: none of it is ours to act on.
    h.send("garbage" as never);
    h.send({ other: true } as never);
    h.send({ lait: "reply", id: 999, reply: reply("nobody asked") });
    expect(await h.link.spaces()).toEqual(reply("ok"));
  });

  it("refuses the session lane loudly until its adapter exists", () => {
    const h = harness();
    expect(() => h.link.session(() => {})).toThrowError(/session lane/);
  });
});
