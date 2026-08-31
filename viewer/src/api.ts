/**
 * The engine, from the browser.
 *
 * This file is the whole backend. The previous viewer had a `server/` directory —
 * a Vite middleware that spawned `lait --json` once per request and re-parsed its
 * stdout — because there was no other way in. Bare `lait` runs the HTTP head,
 * which exposes the control plane directly, so all of that collapses to `fetch`.
 *
 * Everything is same-origin: the page is served by the engine itself, so the
 * `HttpOnly` cookie rides along and no token is ever visible to script. There is
 * no dev server and no proxy — this page only ever runs where the engine put it.
 */

import type {
  HostRequest,
  IssuesWireResponse,
  Response,
  SpaceRequest,
  SpacesReply,
  WorldRequest,
} from "./types";

/** A refusal from the engine, carrying its own words. */
export class LaitError extends Error {
  readonly status: number;
  /**
   * The engine's own classification of the failure (`error_kind`:
   * `"denied" | "not_found" | "retry" | "error"`), `null` when the reply
   * carried none (transport failures, host-plane errors).
   */
  readonly errorKind: string | null;
  constructor(message: string, status: number, errorKind: string | null = null) {
    super(message);
    this.name = "LaitError";
    this.status = status;
    this.errorKind = errorKind;
    if (errorKind) rememberKind(message, errorKind);
  }
}

/**
 * Message → engine `error_kind`, for classification sites that only hold the
 * message string. Most error paths reduce a `LaitError` to `e.message` before
 * it reaches the recovery UI, so the typed kind rides in this bounded side
 * table instead of a regex guessing it back out of the words. Safe because a
 * given message string only ever arrives tagged with one kind (the engine
 * writes both fields together).
 */
const KNOWN_KINDS = new Map<string, string>();
function rememberKind(message: string, kind: string): void {
  if (KNOWN_KINDS.size > 64) {
    const oldest = KNOWN_KINDS.keys().next().value;
    if (oldest !== undefined) KNOWN_KINDS.delete(oldest);
  }
  KNOWN_KINDS.set(message, kind);
}
export function errorKindOf(message: string): string | null {
  return KNOWN_KINDS.get(message) ?? null;
}

/**
 * A destructive verb wants its question asked first.
 *
 * The engine hands back `cli::destructive_question`'s own string — the same words
 * the CLI prompts with — so the modal and the terminal cannot disagree about what
 * is dangerous. Callers catch this, ask, and retry with `confirm`.
 */
export class ConfirmRequired extends Error {
  readonly question: string;
  constructor(question: string) {
    super(question);
    this.name = "ConfirmRequired";
    this.question = question;
  }
}

async function parse(r: globalThis.Response): Promise<unknown> {
  return r.json().catch(() => null);
}

/**
 * The mount this head serves us at, as this head last stated it.
 *
 * `null` until the first `spaces()` answers. It is not a guess with a default:
 * `mount()` waits for the head to say, because sending a product request to the
 * wrong mount is refused by name and the refusal names a World the page is not.
 */
let served: string | null = null;
/** The one in-flight lookup, so a cold start costs one request and not one per call. */
let asking: Promise<string> | null = null;

/**
 * Which mount to address this World at.
 *
 * The World publishes `issues` and that is what it is served as everywhere a
 * release is installed. A local World — a tree being worked on — is assigned a
 * mount in its own namespace so it cannot answer for the release it was copied
 * from, and the page is served from that tree with no way to know. So it asks,
 * once, and every product call after the first spends the recorded answer.
 *
 * The fallback is the published name rather than a refusal: a head that does not
 * carry the field is one that serves exactly what the World declares.
 */
async function mount(refresh = false): Promise<string> {
  if (refresh) served = null;
  if (served !== null) return served;
  asking ??= spaces()
    .then((reply) => reply.world ?? DECLARED_MOUNT)
    .finally(() => {
      asking = null;
    });
  return asking;
}

/**
 * The mount this World publishes.
 *
 * `MOUNT` in `products/issues-app/src/lib.rs`, which calls it published API for
 * the same reason: it prefixes every tool an agent has learned and is the
 * `{world}` segment of every route a head builds.
 */
const DECLARED_MOUNT = "issues";

/** The spaces picker. Supervisor-level: not a control-plane `Request`. */
export async function spaces(signal?: AbortSignal): Promise<SpacesReply> {
  const r = await fetch("/api/spaces", { credentials: "same-origin", ...(signal ? { signal } : {}) });
  const body = (await parse(r)) as SpacesReply | { kind: "error"; message: string } | null;
  if (!r.ok || (body && "kind" in body && body.kind === "error")) {
    throw new LaitError(
      body && "message" in body ? body.message : `HTTP ${r.status}`,
      r.status,
    );
  }
  if (!body) throw new LaitError("no reply", r.status);
  const reply = body as SpacesReply;
  // Recorded on every answer, not only the first: this is the head restating a
  // fact about itself, and the page has no other way to learn it.
  served = reply.world ?? DECLARED_MOUNT;
  return reply;
}

/**
 * Send one Issues World request through its explicit package route.
 *
 * The request/response types are the Issues package's application protocol,
 * not a REST translation. Generic membership, lifecycle, and authority calls
 * use `spaceRpc` instead, so malformed product input can never fall through
 * into an unrelated root command namespace.
 *
 * `confirm` is not a security boundary and is not pretending to be one: anything
 * that can send `issue_delete` can send `confirm`. It exists so a destructive verb
 * cannot fire by accident, which is exactly what the CLI's prompt buys.
 */
export async function rpc<R extends Response = Response>(
  space: string,
  request: WorldRequest,
  opts: { confirm?: boolean; signal?: AbortSignal } = {},
): Promise<R> {
  const sendTo = (world: string) => send<IssuesWireResponse>(
    `/api/spaces/${encodeURIComponent(space)}/worlds/${encodeURIComponent(world)}/rpc`,
    request,
    opts,
  );
  const addressed = await mount();
  let response: IssuesWireResponse;
  try {
    response = await sendTo(addressed);
  } catch (error) {
    if (!isWrongHead(error)) throw error;
    // A desktop window can outlive the head process behind it. If that head is
    // replaced by another mount, the module-level answer above is now stale.
    // The server refuses a wrong mount before parsing or invoking the request,
    // so refreshing the one head fact and replaying once is safe for writes as
    // well as reads. One retry also prevents a broken registry from looping.
    const current = await mount(true);
    if (current === addressed) throw error;
    response = await sendTo(current);
  }
  if (response.kind !== "operation") return response as R;
  // Keep existing product result narrowing ergonomic while retaining the
  // durable operation receipt. MCP and other direct package consumers see the
  // same envelope on the wire; this is presentation, not a second protocol.
  return { ...response.response, receipt: response.receipt } as R;
}

/** A refusal made at the head boundary, before a World sees the request. */
function isWrongHead(error: unknown): error is LaitError {
  return error instanceof LaitError
    && error.status === 404
    && error.errorKind === "not_found"
    && error.message.startsWith("this head serves '");
}

/**
 * Send one host-plane request — the plane that answers before any Space exists.
 *
 * Its own function rather than a `spaceRpc` with no space, because that is the
 * whole point of the route: founding a Space and entering one from an invite
 * have no space id to put in a path, so `/api/spaces/{id}/rpc` is unreachable at
 * exactly the moment they matter. Without a caller here, a machine with no store
 * opens a page that cannot bring one into existence.
 */
export async function hostRpc<R extends Response = Response>(
  request: HostRequest,
  opts: { signal?: AbortSignal } = {},
): Promise<R> {
  return send("/api/host/rpc", request, opts);
}

/** Send one generic Space-control request to the selected Orbit. */
export async function spaceRpc<R extends Response = Response>(
  space: string,
  request: SpaceRequest,
  opts: { confirm?: boolean; signal?: AbortSignal } = {},
): Promise<R> {
  return send(`/api/spaces/${encodeURIComponent(space)}/rpc`, request, opts);
}

async function send<R = Response>(
  endpoint: string,
  request: WorldRequest | SpaceRequest | HostRequest,
  opts: { confirm?: boolean; signal?: AbortSignal },
): Promise<R> {
  const qs = opts.confirm ? "?confirm=true" : "";
  const r = await fetch(`${endpoint}${qs}`, {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(request),
    ...(opts.signal ? { signal: opts.signal } : {}),
  });
  const body = (await parse(r)) as Record<string, unknown> | null;

  if (r.status === 409 && body?.kind === "confirm_required") {
    throw new ConfirmRequired(String(body.question ?? "Are you sure?"));
  }
  if (!r.ok || body?.kind === "error") {
    throw new LaitError(
      String(body?.message ?? `HTTP ${r.status}`),
      r.status,
      typeof body?.error_kind === "string" ? body.error_kind : null,
    );
  }
  if (!body) throw new LaitError("no reply", r.status);
  return body as R;
}
