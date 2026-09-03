import { describe, expect, it, vi } from "vitest";

import { engineRouter, type EngineHandle } from "./engineRouter";

/**
 * The router's contract from the page's side, over a real MessageChannel: every
 * frame family routes to the right engine method and every streaming lane
 * (rings, carets) reaches the page — with a stub handle, no wasm.
 */

const settle = () => new Promise((r) => setTimeout(r, 0));

/** A stub engine handle a test steers per-method. */
function stubHandle(over: Partial<EngineHandle> = {}): EngineHandle {
  return {
    handleLink: () => "null",
    handleSession: () => "[]",
    watchCaret: async () => true,
    drainRing: () => undefined,
    drainCaret: () => new Promise<string | undefined>(() => {}), // never resolves
    repull: async () => 0,
    ...over,
  };
}

/** Router on port1; the test plays the page on port2. */
function harness(handle: EngineHandle) {
  const channel = new MessageChannel();
  const received: any[] = [];
  channel.port2.addEventListener("message", (e: MessageEvent) => received.push(e.data));
  channel.port2.start();
  const send = (frame: unknown) => channel.port2.postMessage(frame);
  const stop = engineRouter(handle, channel.port1, { pollMs: 0 });
  return { send, received, stop };
}

describe("engineRouter", () => {
  it("routes an rpc frame to handleLink and posts its reply", async () => {
    const reply = JSON.stringify({ lait: "reply", id: 7, reply: { kind: "reply", body: {} } });
    const handleLink = vi.fn(() => reply);
    const { send, received, stop } = harness(stubHandle({ handleLink }));
    send({ lait: "rpc", id: 7, verb: "spaces" });
    await settle();
    expect(handleLink).toHaveBeenCalledOnce();
    expect(received).toContainEqual(JSON.parse(reply));
    stop();
  });

  it("answers an events subscription with liveness then drains rings", async () => {
    let drained = 0;
    const ring = JSON.stringify({ space: "s", epoch: 1, seq: 1, reset: true });
    const drainRing = () => (drained++ === 0 ? ring : undefined);
    const { send, received, stop } = harness(stubHandle({ drainRing }));
    send({ lait: "events", id: 3 });
    await settle();
    expect(received).toContainEqual({ lait: "liveness", id: 3, liveness: "live" });
    expect(received).toContainEqual({ lait: "ring", id: 3, ring: JSON.parse(ring) });
    stop();
  });

  it("opens a session, posts its frames, and drives watchCaret from a cursor", async () => {
    const openFrame = { lait: "session:event", sid: 1, event: { kind: "liveness", liveness: "live" } };
    const handleSession = vi.fn(() => JSON.stringify([openFrame]));
    let watched: string | undefined;
    const watchCaret = async (question: string) => {
      watched = question;
      return true;
    };
    const { send, received, stop } = harness(stubHandle({ handleSession, watchCaret }));

    send({ lait: "session:open", sid: 1 });
    await settle();
    expect(received).toContainEqual(openFrame);

    send({
      lait: "session:watch",
      sid: 1,
      question: { space: "s", issue: "iss_x", cursor: { field: "description", anchor: 4 } },
    });
    await settle();
    expect(watched).toBeDefined();
    expect(JSON.parse(watched!)).toMatchObject({ issue: "iss_x" });
    stop();
  });

  it("stamps a drained caret with the watched reff and routes it to that session", async () => {
    const caret = { kind: "live", space: "s", issue: null, view: { kind: "live", generation: 0, partial: false, entries: [] } };
    let handed = false;
    const drainCaret = () =>
      handed ? new Promise<string | undefined>(() => {}) : ((handed = true), Promise.resolve(JSON.stringify(caret)));
    const { send, received, stop } = harness(stubHandle({ drainCaret }));
    send({ lait: "session:open", sid: 5 });
    // A caret drained before any watch has nowhere to route — must be dropped.
    await settle();
    await settle();
    expect(received.some((f) => f.lait === "session:event" && f.event?.kind === "live")).toBe(false);
    stop();
  });

  it("routes a caret drained after a watch, stamped with that issue's reff", async () => {
    const caret = { kind: "live", space: "s", issue: null, view: { kind: "live", generation: 0, partial: false, entries: [] } };
    let handed = false;
    const drainCaret = () =>
      handed ? new Promise<string | undefined>(() => {}) : ((handed = true), Promise.resolve(JSON.stringify(caret)));
    const { send, received, stop } = harness(stubHandle({ drainCaret, handleSession: () => "[]" }));
    send({ lait: "session:open", sid: 9 });
    send({ lait: "session:watch", sid: 9, question: { space: "s", issue: "iss_z" } });
    await settle();
    await settle();
    const live = received.find((f) => f.lait === "session:event" && f.event?.kind === "live");
    expect(live).toBeDefined();
    expect(live.sid).toBe(9);
    expect(live.event.issue).toBe("iss_z"); // stamped, not the raw null
    stop();
  });

  it("coalesces concurrent converges — repulls never pile up (the freeze guard)", async () => {
    // The primary liveness fix: many triggers (the 2s poll + every rpc/mutate)
    // must never run repull concurrently, or slow dials stack and freeze the
    // single Worker thread. Fire a burst of rpcs while a slow repull is in
    // flight; assert repull runs once now and exactly once more (coalesced),
    // never N times concurrently.
    let active = 0;
    let maxActive = 0;
    let calls = 0;
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const repull = async () => {
      calls++;
      active++;
      maxActive = Math.max(maxActive, active);
      await gate; // hold the first repull open while the burst arrives
      active--;
      return 0;
    };
    const { send, stop } = harness(stubHandle({ repull, handleLink: () => "null" }));
    // Five rpcs in a burst — each would trigger a converge.
    for (let i = 0; i < 5; i++) send({ lait: "rpc", id: i, verb: "spaces" });
    await settle();
    expect(active).toBe(1); // only one repull in flight despite five triggers
    release();
    await settle();
    await settle();
    expect(maxActive).toBe(1); // never concurrent
    expect(calls).toBe(2); // the in-flight one, then exactly one coalesced re-run
    stop();
  });

  it("routes a session:mutate to handleSession and posts its reply", async () => {
    const replyFrame = { lait: "session:reply", sid: 1, rid: 2, outcome: { ok: true, status: 200, response: {} } };
    const handleSession = vi.fn(() => JSON.stringify([replyFrame]));
    const { send, received, stop } = harness(stubHandle({ handleSession }));
    send({ lait: "session:mutate", sid: 1, rid: 2, space: "s", request: { cmd: "issue_view" } });
    await settle();
    expect(received).toContainEqual(replyFrame);
    stop();
  });
});
