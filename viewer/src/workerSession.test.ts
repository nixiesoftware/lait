import { describe, expect, it } from "vitest";

import { SocketMutationError } from "./socket";
import type { Response, WorldRequest } from "./types";
import {
  workerSession,
  type WorkerSessionRequest,
  type WorkerSessionResponse,
} from "./workerSession";

/**
 * The session adapter's contract from the page's side: it presents the same
 * `Socket` the page drives, every call crosses as one frame scoped by session
 * id, non-mutation events relay as data, a `mutate` settles on its reply, the
 * operation envelope is unwrapped exactly as `openSocket` unwraps it, and a
 * refusal comes back as a rehydrated `SocketMutationError` — the class the
 * boundary cannot clone.
 */

const rq = (cmd: string): WorldRequest => ({ cmd }) as unknown as WorldRequest;

/** Both ends of a real port: the adapter on one, the test's session host on the other. */
function harness() {
  const channel = new MessageChannel();
  const seen: WorkerSessionRequest[] = [];
  channel.port2.addEventListener("message", (event: MessageEvent) => {
    seen.push(event.data as WorkerSessionRequest);
  });
  channel.port2.start();
  const send = (frame: WorkerSessionResponse) => channel.port2.postMessage(frame);
  const events: unknown[] = [];
  const socket = workerSession(channel.port1, (event) => events.push(event));
  const sid = () => {
    const open = seen.find((f) => f.lait === "session:open");
    return (open as { sid: number }).sid;
  };
  return { socket, seen, send, events, sid };
}

const delivered = () => new Promise((r) => setTimeout(r, 0));

describe("opening and events", () => {
  it("opens a session and reports connecting before any host frame", async () => {
    const h = harness();
    // The synchronous liveness `openSocket` also emits.
    expect(h.events).toEqual([{ kind: "liveness", liveness: "connecting" }]);
    await delivered();
    expect(h.seen.some((f) => f.lait === "session:open")).toBe(true);
  });

  it("relays the host's non-mutation events as data", async () => {
    const h = harness();
    await delivered();
    const sid = h.sid();
    h.send({ lait: "session:event", sid, event: { kind: "liveness", liveness: "live" } });
    h.send({
      lait: "session:event",
      sid,
      event: {
        kind: "progress",
        progress: { transfer: "t1", content: "c1", moved: 40, total: 100, done: false },
      },
    });
    await delivered();
    expect(h.events).toContainEqual({ kind: "liveness", liveness: "live" });
    expect(h.events).toContainEqual({
      kind: "progress",
      progress: { transfer: "t1", content: "c1", moved: 40, total: 100, done: false },
    });
  });

  it("ignores frames for another session id on a shared port", async () => {
    const h = harness();
    await delivered();
    const before = h.events.length;
    h.send({ lait: "session:event", sid: 9999, event: { kind: "liveness", liveness: "live" } });
    await delivered();
    expect(h.events.length).toBe(before);
  });
});

describe("watch", () => {
  it("crosses a declaration as one frame", async () => {
    const h = harness();
    await delivered();
    h.socket.watch({ space: "orb_x", issue: "iss_1" });
    await delivered();
    const watch = h.seen.find((f) => f.lait === "session:watch");
    expect(watch).toMatchObject({ question: { space: "orb_x", issue: "iss_1" } });
  });
});

describe("mutate", () => {
  it("settles the promise on its reply and unwraps the operation envelope", async () => {
    const h = harness();
    await delivered();
    const sid = h.sid();
    const p = h.socket.mutate("orb_x", rq("issue_new"));
    await delivered();
    const req = h.seen.find((f) => f.lait === "session:mutate") as Extract<
      WorkerSessionRequest,
      { lait: "session:mutate" }
    >;
    expect(req).toMatchObject({ space: "orb_x" });
    // An operation-enveloped response, exactly as the wire carries it.
    const enveloped = {
      kind: "operation",
      response: { kind: "ok", value: 1 },
      receipt: { id: "r1" },
    } as unknown as Response;
    h.send({
      lait: "session:reply",
      sid,
      rid: req.rid,
      outcome: { ok: true, status: 200, response: enveloped },
    });
    // The caller sees the unwrapped inner response with the receipt merged in.
    expect(await p).toEqual({ kind: "ok", value: 1, receipt: { id: "r1" } });
  });

  it("passes a non-enveloped response straight through", async () => {
    const h = harness();
    await delivered();
    const sid = h.sid();
    const p = h.socket.mutate("orb_x", rq("whoami"));
    await delivered();
    const req = h.seen.find((f) => f.lait === "session:mutate") as { rid: number };
    const plain = { kind: "whoami" } as unknown as Response;
    h.send({ lait: "session:reply", sid, rid: req.rid, outcome: { ok: true, status: 200, response: plain } });
    expect(await p).toEqual(plain);
  });

  it("rehydrates SocketMutationError from a clone-safe refusal", async () => {
    const h = harness();
    await delivered();
    const sid = h.sid();
    const p = h.socket.mutate("orb_x", rq("issue_close"));
    await delivered();
    const req = h.seen.find((f) => f.lait === "session:mutate") as { rid: number };
    h.send({
      lait: "session:reply",
      sid,
      rid: req.rid,
      outcome: { ok: false, status: 403, error: { message: "read refused", errorKind: "denied" } },
    });
    await expect(p).rejects.toMatchObject({
      name: "SocketMutationError",
      message: "read refused",
      status: 403,
      errorKind: "denied",
    });
    await p.catch((e) => expect(e).toBeInstanceOf(SocketMutationError));
  });

  it("drops a reply for an unknown request id without throwing", async () => {
    const h = harness();
    await delivered();
    const sid = h.sid();
    // No pending mutate — a late reply after the caller moved on.
    h.send({ lait: "session:reply", sid, rid: 424242, outcome: { ok: true, status: 200, response: {} as Response } });
    await delivered();
    // Nothing threw; a real mutate still works afterward.
    const p = h.socket.mutate("orb_x", rq("issue_new"));
    await delivered();
    const req = h.seen.filter((f) => f.lait === "session:mutate").at(-1) as { rid: number };
    h.send({ lait: "session:reply", sid, rid: req.rid, outcome: { ok: true, status: 200, response: { kind: "ok" } as Response } });
    expect(await p).toEqual({ kind: "ok" });
  });
});

describe("close", () => {
  it("closes the session, rejects pending mutations, and sends a close frame", async () => {
    const h = harness();
    await delivered();
    const pending = h.socket.mutate("orb_x", rq("slow"));
    h.socket.close();
    await expect(pending).rejects.toThrow(/closed/);
    await delivered();
    expect(h.seen.some((f) => f.lait === "session:close")).toBe(true);
    // A mutate after close rejects locally, sending nothing new.
    const seenBefore = h.seen.length;
    await expect(h.socket.mutate("orb_x", rq("x"))).rejects.toThrow(/not ready/);
    await delivered();
    expect(h.seen.length).toBe(seenBefore);
  });

  it("stops relaying events after close", async () => {
    const h = harness();
    await delivered();
    const sid = h.sid();
    const before = h.events.length;
    h.socket.close();
    h.send({ lait: "session:event", sid, event: { kind: "liveness", liveness: "live" } });
    await delivered();
    expect(h.events.length).toBe(before);
  });
});
