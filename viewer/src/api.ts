/**
 * The engine, from the browser.
 *
 * This file is the whole backend. The previous viewer had a `server/` directory —
 * a Vite middleware that spawned `lait --json` once per request and re-parsed its
 * stdout — because there was no other way in. Bare `lait` runs the HTTP head,
 * which exposes the control plane directly, so all of that collapses to `fetch`.
 *
 * Everything is same-origin: the page is served by the engine itself, so the
 * `HttpOnly` cookie rides along and no token is ever visible to script. In dev the
 * vite proxy fakes that (see vite.config.ts) rather than the engine relaxing its
 * origin guard.
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
  return body as SpacesReply;
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
  const response = await send<IssuesWireResponse>(
    `/api/spaces/${encodeURIComponent(space)}/worlds/issues/rpc`,
    request,
    opts,
  );
  if (response.kind !== "operation") return response as R;
  // Keep existing product result narrowing ergonomic while retaining the
  // durable operation receipt. MCP and other direct package consumers see the
  // same envelope on the wire; this is presentation, not a second protocol.
  return { ...response.response, receipt: response.receipt } as R;
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
