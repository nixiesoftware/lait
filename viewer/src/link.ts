/**
 * The engine link — the one seam between this page and whatever answers it.
 *
 * The head is an adapter, not a second engine (SERVE.md): nothing behind it
 * knows which transport a request arrived on. This seam extends that exact
 * claim one layer up. Everything head-specific — URL construction, the
 * cookie posture, EventSource reconnection, the socket's postcard codec —
 * lives inside [`httpLink`], the production default; the modules above
 * (`api.ts`, `doorbell.ts`, `live.ts`) are policy and keep their exported
 * signatures. A browser-native deployment binds a different link at its own
 * composition root. There is deliberately no dev-mode selection here: a
 * backend that exists only in development is the removed Vite dev server
 * wearing a new hat.
 *
 * Two rules shape the contract:
 *
 * - **Refusals cross as data.** `LaitError` and `ConfirmRequired` are Error
 *   subclasses and do not survive a structured clone, and a later slice puts
 *   the engine in a Worker. So a link answers with [`LinkReply`] — a plain
 *   union — and `api.ts` rehydrates the classes on this side of the boundary.
 *   A link's refusal must carry `errorKind` honestly (`ui/AppState.tsx`
 *   classifies by it) and must never imitate the head's wrong-mount refusal
 *   (`"this head serves '…'"` + 404 + `not_found`): that string is native-head
 *   topology, and a link that echoed it would trigger mount-refresh replays in
 *   `api.ts` and silently reroute `LivePlane` editor writes.
 *
 * - **The planes stay three verbs.** Host, Space, and World requests are
 *   separate methods for the reason the head keeps them separate routes
 *   (`serve/policy.rs`): a malformed product request must not fall through
 *   into an unrelated control namespace, whatever answers it.
 *
 * `AbortSignal`s are best-effort: the HTTP link honors them, and a link over a
 * message boundary may translate or ignore them. Callers must not depend on
 * abort for correctness.
 *
 * Content stays off this seam on purpose. `content.ts`'s central export is a
 * URL the browser *navigates* to — not a call any injected function can
 * answer — and the upload path's authoritative `?len=` is a head contract.
 * An in-tab engine needs a different mechanism (a Blob, a Service Worker),
 * and papering that over with a verb here would only hide it.
 */

import type {
  HostRequest,
  SpaceDoorbell,
  SpaceRequest,
  WorldRequest,
} from "./types";
import { openSocket, type Socket, type SocketEvent } from "./socket";

/** What the user should be told about a stream's health. */
export type Liveness = "connecting" | "live" | "retrying";

/** An engine refusal, as data: the shape that crosses any boundary. */
export interface LinkRefusal {
  status: number;
  message: string;
  errorKind: string | null;
}

/** One answer from a link: the engine's reply body, a refusal, or a question. */
export type LinkReply =
  | { kind: "reply"; body: unknown }
  | { kind: "refusal"; refusal: LinkRefusal }
  | { kind: "confirm"; question: string };

export interface RpcOpts {
  confirm?: boolean;
  signal?: AbortSignal;
}

export interface EngineLink {
  /** The supervisor's Space list, plus the mount this backend serves. */
  spaces(signal?: AbortSignal): Promise<LinkReply>;
  /** The host plane — answers before any Space exists. */
  hostRpc(request: HostRequest, opts: RpcOpts): Promise<LinkReply>;
  /** Generic Space control, addressed to one Orbit. */
  spaceRpc(space: string, request: SpaceRequest, opts: RpcOpts): Promise<LinkReply>;
  /** One World request, through its explicit package route. */
  worldRpc(
    space: string,
    world: string,
    request: WorldRequest,
    opts: RpcOpts,
  ): Promise<LinkReply>;
  /**
   * The doorbell: dirty flags, never state. A ring of `null` means
   * "trust nothing, rebaseline" — a frame that could not be read, or frames
   * that were dropped. Returns the unsubscribe.
   */
  events(
    onRing: (d: SpaceDoorbell | null) => void,
    onLiveness: (l: Liveness) => void,
  ): () => void;
  /**
   * The session socket: presence, progress, and the control lane.
   *
   * The one lane the data-refusal rule does not yet cover: a `Socket` is a
   * handle of functions and its `mutate` rejects with `SocketMutationError`,
   * an Error subclass — neither survives a structured clone. A backend behind
   * a Worker owes a this-side adapter that carries the session over messages
   * and rehydrates that class, exactly as `api.ts` rehydrates `LaitError`.
   */
  session(onEvent: (event: SocketEvent) => void): Socket;
}

async function parseBody(r: Response): Promise<Record<string, unknown> | null> {
  return r.json().catch(() => null) as Promise<Record<string, unknown> | null>;
}

/** Fold one HTTP answer into the link vocabulary. */
function classify(r: Response, body: Record<string, unknown> | null): LinkReply {
  if (r.status === 409 && body?.kind === "confirm_required") {
    return { kind: "confirm", question: String(body.question ?? "Are you sure?") };
  }
  if (!r.ok || body?.kind === "error") {
    return {
      kind: "refusal",
      refusal: {
        status: r.status,
        message: String(body?.message ?? `HTTP ${r.status}`),
        errorKind: typeof body?.error_kind === "string" ? body.error_kind : null,
      },
    };
  }
  if (!body) {
    return {
      kind: "refusal",
      refusal: { status: r.status, message: "no reply", errorKind: null },
    };
  }
  return { kind: "reply", body };
}

async function post(
  endpoint: string,
  request: HostRequest | SpaceRequest | WorldRequest,
  opts: RpcOpts,
): Promise<LinkReply> {
  const qs = opts.confirm ? "?confirm=true" : "";
  const r = await fetch(`${endpoint}${qs}`, {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
    ...(opts.signal ? { signal: opts.signal } : {}),
  });
  return classify(r, await parseBody(r));
}

/**
 * The native head, over same-origin HTTP. Everything here is head topology:
 * root-relative paths because the engine serves this page, `same-origin`
 * credentials because the token is an HttpOnly cookie, `EventSource`'s native
 * reconnection, and `openSocket`'s framed WebSocket.
 */
export const httpLink: EngineLink = {
  async spaces(signal?: AbortSignal): Promise<LinkReply> {
    const r = await fetch("/api/spaces", {
      credentials: "same-origin",
      ...(signal ? { signal } : {}),
    });
    return classify(r, await parseBody(r));
  },

  hostRpc(request, opts) {
    return post("/api/host/rpc", request, opts);
  },

  spaceRpc(space, request, opts) {
    return post(`/api/spaces/${encodeURIComponent(space)}/rpc`, request, opts);
  },

  worldRpc(space, world, request, opts) {
    return post(
      `/api/spaces/${encodeURIComponent(space)}/worlds/${encodeURIComponent(world)}/rpc`,
      request,
      opts,
    );
  },

  events(onRing, onLiveness) {
    const es = new EventSource("/api/events", { withCredentials: true });
    es.onopen = () => onLiveness("live");
    // `EventSource` reconnects on its own, so there is no retry loop here —
    // only the liveness the user should see.
    es.onerror = () => onLiveness("retrying");
    es.addEventListener("doorbell", (ev) => {
      try {
        onRing(JSON.parse((ev as MessageEvent<string>).data) as SpaceDoorbell);
      } catch {
        // A frame we can't read is still news: rebaseline rather than ignore it.
        onRing(null);
      }
    });
    // Frames were dropped — the view may be stale in ways nobody can name.
    es.addEventListener("lagged", () => onRing(null));
    return () => es.close();
  },

  session: openSocket,
};

let bound: EngineLink = httpLink;
const onBind: Array<() => void> = [];

/** The link this page is composed over. */
export function engineLink(): EngineLink {
  return bound;
}

/**
 * Bind a different backend, once, at a composition root before render.
 *
 * Module-level caches above the seam (the mount cache in `api.ts`) are facts
 * about one backend and are dropped on rebind — that is what `onLinkBound`
 * registrations are for.
 */
export function bindEngineLink(link: EngineLink): void {
  bound = link;
  for (const reset of onBind) reset();
}

/** Register a cache reset to run whenever the link is rebound. */
export function onLinkBound(reset: () => void): void {
  onBind.push(reset);
}
