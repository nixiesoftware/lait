/**
 * The engine link over a message port — the browser-native backend's seam.
 *
 * Where `httpLink` speaks head topology (URLs, cookies, EventSource), this
 * link speaks only structured clone: every request crosses as one message,
 * every answer comes back as the [`LinkReply`] union `link.ts` already made
 * clone-safe for exactly this boundary. The other side is the Worker that
 * composes the in-tab engine (`runtime::browser` behind the wasm module);
 * this file owns the frame vocabulary, and that host mirrors it — there is
 * no schema generation, because the protocol is nine small frames and a
 * generated binding would outweigh it.
 *
 * What this link deliberately does not do:
 *
 * - **Supervise the Worker.** A dead or wedged Worker leaves requests
 *   pending; only the composition root that spawned it can know it died and
 *   rebind. Inventing a timeout here would turn "the engine is thinking"
 *   into a refusal on slow machines.
 * - **Own liveness.** The doorbell's liveness frames come from the engine
 *   host, which knows whether its own event source is coherent; this side
 *   only relays. Until the first frame arrives the subscription reports
 *   "connecting", exactly what the user should be told.
 * - **Carry the session lane.** A `Socket` is a handle of functions and its
 *   errors are class instances — the one lane `link.ts` documents as owing a
 *   message-boundary adapter. Until that adapter exists, `session` refuses
 *   loudly rather than pretending presence works.
 *
 * `AbortSignal`s are translated best-effort: an abort frame crosses, the
 * local promise rejects, and a late reply is dropped. Callers already must
 * not depend on abort for correctness (see `link.ts`).
 */

import type { EngineLink, LinkReply, Liveness, RpcOpts } from "./link";
import type {
  HostRequest,
  SpaceDoorbell,
  SpaceRequest,
  WorldRequest,
} from "./types";
import type { Socket, SocketEvent } from "./socket";

/** Frames this side sends. The engine host mirrors this union. */
export type WorkerLinkRequest =
  | {
      lait: "rpc";
      id: number;
      verb: "spaces" | "host" | "space" | "world";
      space?: string;
      world?: string;
      request?: HostRequest | SpaceRequest | WorldRequest;
      confirm?: boolean;
    }
  | { lait: "abort"; id: number }
  | { lait: "events"; id: number }
  | { lait: "close"; id: number };

/** Frames the engine host sends back. */
export type WorkerLinkResponse =
  | { lait: "reply"; id: number; reply: LinkReply }
  | { lait: "ring"; id: number; ring: SpaceDoorbell | null }
  | { lait: "liveness"; id: number; liveness: Liveness };

/** The port half this link needs: a Worker or one end of a MessageChannel. */
export interface LinkPort {
  postMessage(message: unknown): void;
  addEventListener(
    type: "message",
    listener: (event: MessageEvent) => void,
  ): void;
  /** MessagePort queues until started; Worker has no such method. */
  start?(): void;
}

/**
 * Bind the engine behind `port` as an [`EngineLink`]. Composed once at a
 * composition root (`bindEngineLink(workerLink(worker))`) — the root that
 * spawned the Worker owns its lifecycle, this link owns only the frames.
 */
export function workerLink(port: LinkPort): EngineLink {
  let next = 1;
  const pending = new Map<number, (reply: LinkReply) => void>();
  const subscriptions = new Map<
    number,
    {
      onRing: (d: SpaceDoorbell | null) => void;
      onLiveness: (l: Liveness) => void;
    }
  >();

  port.addEventListener("message", (event: MessageEvent) => {
    const frame = event.data as WorkerLinkResponse | null;
    if (!frame || typeof frame !== "object" || !("lait" in frame)) return;
    switch (frame.lait) {
      case "reply": {
        const settle = pending.get(frame.id);
        // An unknown id is a late reply after abort, or another consumer's
        // frame on a shared port: not ours to act on either way.
        if (!settle) return;
        pending.delete(frame.id);
        settle(frame.reply);
        return;
      }
      case "ring": {
        subscriptions.get(frame.id)?.onRing(frame.ring);
        return;
      }
      case "liveness": {
        subscriptions.get(frame.id)?.onLiveness(frame.liveness);
        return;
      }
    }
  });
  port.start?.();

  function rpc(
    verb: "spaces" | "host" | "space" | "world",
    fields: {
      space?: string;
      world?: string;
      request?: HostRequest | SpaceRequest | WorldRequest;
    },
    opts: RpcOpts,
  ): Promise<LinkReply> {
    const id = next++;
    return new Promise<LinkReply>((resolve, reject) => {
      if (opts.signal?.aborted) {
        reject(new DOMException("request aborted", "AbortError"));
        return;
      }
      opts.signal?.addEventListener(
        "abort",
        () => {
          // Settle locally and tell the host; a reply that races the abort
          // finds no pending entry and is dropped.
          if (!pending.has(id)) return;
          pending.delete(id);
          port.postMessage({ lait: "abort", id } satisfies WorkerLinkRequest);
          reject(new DOMException("request aborted", "AbortError"));
        },
        { once: true },
      );
      pending.set(id, resolve);
      port.postMessage({
        lait: "rpc",
        id,
        verb,
        ...fields,
        ...(opts.confirm ? { confirm: true } : {}),
      } satisfies WorkerLinkRequest);
    });
  }

  return {
    spaces(signal?: AbortSignal): Promise<LinkReply> {
      return rpc("spaces", {}, signal ? { signal } : {});
    },

    hostRpc(request, opts) {
      return rpc("host", { request }, opts);
    },

    spaceRpc(space, request, opts) {
      return rpc("space", { space, request }, opts);
    },

    worldRpc(space, world, request, opts) {
      return rpc("world", { space, world, request }, opts);
    },

    events(onRing, onLiveness) {
      const id = next++;
      subscriptions.set(id, { onRing, onLiveness });
      onLiveness("connecting");
      port.postMessage({ lait: "events", id } satisfies WorkerLinkRequest);
      return () => {
        if (!subscriptions.delete(id)) return;
        port.postMessage({ lait: "close", id } satisfies WorkerLinkRequest);
      };
    },

    session(_onEvent: (event: SocketEvent) => void): Socket {
      // The documented exception in `link.ts`: the session lane owes a
      // message-boundary adapter (presence, progress, `mutate`'s rehydrated
      // error class). Refusing here is honest; pretending would be a socket
      // whose writes go nowhere.
      throw new Error(
        "the Worker engine link does not carry the session lane yet",
      );
    },
  };
}
