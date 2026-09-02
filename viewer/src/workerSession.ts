/**
 * The session lane over a message port — the adapter `link.ts` said the
 * Worker backend owed.
 *
 * `httpLink.session` is `openSocket`: a `WebSocket` whose frames the page
 * decodes and whose `mutate` rejects with `SocketMutationError`. Neither a
 * live socket nor an `Error` subclass survives a structured clone, so a
 * backend behind a Worker cannot hand its socket across — it reconstructs one
 * on this side. This module is that reconstruction: it presents the exact
 * `Socket` the page already drives (`watch` / `mutate` / `close`), turns each
 * call into a frame, turns the host's frames back into `SocketEvent`s, and —
 * the whole reason the seam exists — **rehydrates `SocketMutationError` here**,
 * from the clone-safe `{message, status, errorKind}` the host sends in its
 * place, exactly as `api.ts` rehydrates `LaitError` on the RPC lane.
 *
 * It mirrors `openSocket`'s contract, not just its shape:
 *
 * - **The mutation lane never drops silently.** A `mutate` promise settles on
 *   its reply frame or on close; a reply for an unknown request id (a late
 *   one after close, or another session's on a shared port) is dropped, never
 *   thrown.
 * - **The unwrap lives here, once.** `openSocket` unwraps the operation
 *   envelope (`{...response, receipt}`) before resolving; so does this, so a
 *   caller cannot tell which backend answered.
 * - **Liveness is the host's to state.** The adapter reports `connecting`
 *   until the host's first liveness frame, then relays — a socket the page
 *   can see the health of, the same word `openSocket` uses.
 *
 * The frame vocabulary is owned here and mirrored by the Worker's session
 * host. A session id (`sid`) scopes every frame, so a closed session's late
 * frames are ignored and two sessions can share one port.
 */

import type { IssuesWireResponse, Response, WorldRequest } from "./types";
import {
  SocketMutationError,
  type Question,
  type Socket,
  type SocketEvent,
  type SocketLiveness,
} from "./socket";

/** The port half this adapter needs — a Worker or one end of a MessageChannel. */
export interface SessionPort {
  postMessage(message: unknown): void;
  addEventListener(
    type: "message",
    listener: (event: MessageEvent) => void,
  ): void;
  start?(): void;
}

/** Frames the page sends. The Worker's session host mirrors this union. */
export type WorkerSessionRequest =
  | { lait: "session:open"; sid: number }
  | { lait: "session:watch"; sid: number; question: Question }
  | {
      lait: "session:mutate";
      sid: number;
      rid: number;
      space: string;
      request: WorldRequest;
    }
  | { lait: "session:close"; sid: number };

/** A mutation outcome, in clone-safe data — no `Error` subclass crosses. */
export type WorkerSessionMutationOutcome =
  | { ok: true; status: number; response: Response }
  | {
      ok: false;
      status: number;
      error: { message: string; errorKind: string | null };
    };

/** Frames the session host sends back. */
export type WorkerSessionResponse =
  | { lait: "session:event"; sid: number; event: SocketEvent }
  | {
      lait: "session:reply";
      sid: number;
      rid: number;
      outcome: WorkerSessionMutationOutcome;
    };

let nextSid = 1;

/**
 * Reconstruct the session `Socket` over `port`, delivering non-mutation
 * events to `onEvent`. Each call opens one session; `close()` ends it.
 */
export function workerSession(
  port: SessionPort,
  onEvent: (event: SocketEvent) => void,
): Socket {
  const sid = nextSid++;
  let closed = false;
  let nextRequest = 1;
  const pending = new Map<
    number,
    { resolve: (response: Response) => void; reject: (error: Error) => void }
  >();

  const rejectPending = (message: string): void => {
    for (const request of pending.values()) request.reject(new Error(message));
    pending.clear();
  };

  port.addEventListener("message", (event: MessageEvent) => {
    if (closed) return;
    const frame = event.data as WorkerSessionResponse | null;
    if (!frame || typeof frame !== "object" || !("lait" in frame)) return;
    if (frame.sid !== sid) return;
    if (frame.lait === "session:event") {
      onEvent(frame.event);
      return;
    }
    if (frame.lait === "session:reply") {
      const request = pending.get(frame.rid);
      // An unknown id is a late reply after close, or a shared port's other
      // session — not ours to settle.
      if (!request) return;
      pending.delete(frame.rid);
      const outcome = frame.outcome;
      if (outcome.ok) {
        // The same operation-envelope unwrap `openSocket` does, so a caller
        // cannot tell which backend answered.
        const response = outcome.response as IssuesWireResponse;
        request.resolve(
          response.kind === "operation"
            ? { ...response.response, receipt: response.receipt }
            : response,
        );
      } else {
        request.reject(
          new SocketMutationError(
            outcome.error.message,
            outcome.status,
            outcome.error.errorKind,
          ),
        );
      }
    }
  });
  port.start?.();

  // Match `openSocket`, which emits "connecting" synchronously on connect.
  onEvent({ kind: "liveness", liveness: "connecting" satisfies SocketLiveness });
  port.postMessage({ lait: "session:open", sid } satisfies WorkerSessionRequest);

  return {
    watch(question: Question) {
      if (closed) return;
      port.postMessage({
        lait: "session:watch",
        sid,
        question,
      } satisfies WorkerSessionRequest);
    },

    mutate<R extends Response = Response>(
      space: string,
      request: WorldRequest,
    ): Promise<R> {
      if (closed) {
        return Promise.reject(new Error("the editor connection is not ready"));
      }
      const rid = nextRequest++;
      return new Promise<R>((resolve, reject) => {
        pending.set(rid, {
          resolve: (response) => resolve(response as R),
          reject,
        });
        port.postMessage({
          lait: "session:mutate",
          sid,
          rid,
          space,
          request,
        } satisfies WorkerSessionRequest);
      });
    },

    close() {
      if (closed) return;
      closed = true;
      rejectPending("the editor connection closed");
      port.postMessage({ lait: "session:close", sid } satisfies WorkerSessionRequest);
    },
  };
}
