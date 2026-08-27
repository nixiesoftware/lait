/**
 * The engine, from the browser.
 *
 * Medusa spoke to a Go REST API with a JWT from localStorage. This app is
 * served by the lait head itself, so everything is same-origin: the HttpOnly
 * cookie rides along and no token is ever visible to script. In dev the vite
 * proxy fakes that (see vite.config.ts) rather than the engine relaxing its
 * origin guard.
 *
 * One plane, one shape: `POST /api/spaces/{space}/worlds/{mount}/rpc` with
 * `{"cmd": "<verb>", ...}`; replies are `{"kind": "<variant>", ...}` and a
 * refusal carries the engine's own words. `{mount}` is whatever this head says
 * it serves us at — see `mount()`.
 */

/** A refusal from the engine, carrying its own words. */
export class LaitError extends Error {
  readonly status: number;
  /** `"denied" | "not_found" | "retry" | "error"`, `null` when absent. */
  readonly errorKind: string | null;
  constructor(message: string, status: number, errorKind: string | null = null) {
    super(message);
    this.name = 'LaitError';
    this.status = status;
    this.errorKind = errorKind;
  }
}

/**
 * A destructive verb sent without `confirm` — the engine answered with the
 * question it wants confirmed. Not a security boundary: it exists so a
 * destructive verb cannot fire by accident.
 */
export class ConfirmRequired extends Error {
  constructor(question: string) {
    super(question);
    this.name = 'ConfirmRequired';
  }
}

interface SpaceRow {
  id: string;
  name?: string;
}

interface SpacesReply {
  spaces: SpaceRow[];
  /** The mount this head serves this World at. Absent on a head that serves exactly what the World declares. */
  world?: string;
}

/**
 * The mount this World publishes.
 *
 * `MOUNT` in `products/signage-app/src/lib.rs`: it prefixes every tool an
 * agent has learned and is the `{world}` segment of every route a head builds.
 */
const DECLARED_MOUNT = 'signage';

let selectedSpace: string | null = null;
/**
 * The mount this head serves us at, as this head last stated it.
 *
 * Not a guess with a default: a product request sent to the wrong mount is
 * refused by name, and the refusal names a World the page is not. The World
 * publishes `signage` and that is what an installed release is served as. A
 * local World — a tree being worked on — is assigned a mount in its own
 * namespace so it cannot answer for the release it was copied from, and the
 * page is served from that tree with no way to know. So it asks, once, and
 * every call after the first spends the recorded answer.
 */
let served: string | null = null;

/**
 * The Orbit this head serves, resolved once. The URL carries the local Orbit
 * id — never the Space id.
 */
export async function space(): Promise<string> {
  if (selectedSpace) return selectedSpace;
  const r = await fetch('/api/spaces', { credentials: 'same-origin' });
  if (!r.ok) throw new LaitError('the head refused the spaces listing', r.status);
  const reply = (await r.json()) as SpacesReply;
  // The head restating a fact about itself; the page has no other way to learn it.
  served = reply.world ?? DECLARED_MOUNT;
  const first = reply.spaces[0];
  if (!first) throw new LaitError('this head serves no Space yet', 404, 'not_found');
  selectedSpace = first.id;
  return selectedSpace;
}

/** Which mount to address this World at. Resolved by the same request that resolves the Space. */
async function mount(): Promise<string> {
  if (served === null) await space();
  return served ?? DECLARED_MOUNT;
}

let knownActor: string | null = null;

/**
 * The acting identity, from the space plane. It signs every write anyway;
 * here it also names the `chooser` on a Slot choice — the deterministic
 * tie-breaker two replicas agree on.
 */
export async function actor(): Promise<string> {
  if (knownActor) return knownActor;
  const orbit = await space();
  const r = await fetch(`/api/spaces/${encodeURIComponent(orbit)}/rpc`, {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ cmd: 'whoami' }),
  });
  const body = (await r.json()) as { kind?: string; actor?: string };
  if (!r.ok || body.kind !== 'whoami' || typeof body.actor !== 'string') {
    throw new LaitError('the head could not say who is acting', r.status);
  }
  knownActor = body.actor;
  return knownActor;
}

/** Every signage verb goes through here; `cmd` is the verb. */
export async function rpc<T = unknown>(
  request: Record<string, unknown>,
  opts: { confirm?: boolean; signal?: AbortSignal } = {},
): Promise<T> {
  const orbit = await space();
  const world = await mount();
  const qs = opts.confirm ? '?confirm=true' : '';
  const r = await fetch(
    `/api/spaces/${encodeURIComponent(orbit)}/worlds/${encodeURIComponent(world)}/rpc${qs}`,
    {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(request),
    ...(opts.signal ? { signal: opts.signal } : {}),
    },
  );
  let body: Record<string, unknown> | null = null;
  try {
    body = (await r.json()) as Record<string, unknown>;
  } catch {
    body = null;
  }
  if (r.status === 409 && body?.kind === 'confirm_required') {
    throw new ConfirmRequired(String(body.question ?? 'Are you sure?'));
  }
  if (!r.ok || body?.kind === 'error') {
    throw new LaitError(
      String(body?.message ?? `HTTP ${r.status}`),
      r.status,
      typeof body?.error_kind === 'string' ? body.error_kind : null,
    );
  }
  if (!body) throw new LaitError('no reply', r.status);
  return body as T;
}
