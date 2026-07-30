/**
 * Files, over the engine's content routes.
 *
 * Deliberately not part of `api.ts`. That module's `parse` unconditionally
 * reads a JSON body, which is the right shape for every RPC and the wrong one
 * for bytes — a download has no JSON to read and an upload has a body the
 * browser streams rather than serialises.
 *
 * Two rules hold everywhere below.
 *
 * **No ceiling is mirrored here.** The engine owns `max_content_len` and
 * refuses past it; a constant in this file would be a second number to keep in
 * step, and the one the user meets would be whichever is smaller. The refusal
 * carries the sentence, and `statusMessage` turns the status into one when it
 * does not.
 *
 * **A download is a navigation, not a fetch.** Reading megabytes into a blob to
 * hand it back to the browser is work the browser will do anyway, so
 * `downloadUrl` is a pure function and the caller navigates. That also means the
 * credential is the cookie the page already holds — nothing here puts a token in
 * a URL, and the engine refuses one on these routes regardless.
 */

/** What the engine says about one content, from a HEAD. */
export interface ContentResidency {
  /** Plaintext bytes. */
  size: number;
  chunkCount: number;
  residentChunks: number;
  pinned: boolean;
}

/** Whether every byte is here, or only some of them. */
export function isComplete(residency: ContentResidency): boolean {
  return residency.chunkCount > 0 && residency.residentChunks === residency.chunkCount;
}

/** What one upload became. */
export interface StoredContent {
  content: string;
  size: number;
}

export class ContentError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "ContentError";
    this.status = status;
  }
}

/**
 * The sentence a status deserves when the engine did not supply one.
 *
 * Each of these is a different thing for the person to do, which is why the
 * engine's refusals are typed rather than one message. The one worth reading
 * twice is 409: the file is real and its bytes are not here yet, which is a
 * thing that fixes itself once a transfer runs — quite unlike a 404.
 */
export function statusMessage(status: number): string {
  switch (status) {
    case 403:
      return "You do not have access to this file.";
    case 404:
      return "This file is not in this space.";
    case 409:
      return "This file has not finished arriving yet.";
    case 413:
      return "This file is larger than this space accepts.";
    case 416:
      return "That part of the file does not exist.";
    case 422:
      return "That is not a file this space can store.";
    case 503:
      return "This space is busy — try again in a moment.";
    default:
      return `Could not reach the file (HTTP ${status}).`;
  }
}

async function refusal(response: globalThis.Response): Promise<ContentError> {
  const body = (await response.json().catch(() => null)) as
    | { message?: string }
    | null;
  return new ContentError(
    body?.message ?? statusMessage(response.status),
    response.status,
  );
}

/** Geometry and residency, without moving a byte of the file itself. */
export async function residency(
  space: string,
  content: string,
  signal?: AbortSignal,
): Promise<ContentResidency> {
  const response = await fetch(contentPath(space, content), {
    method: "HEAD",
    credentials: "same-origin",
    ...(signal ? { signal } : {}),
  });
  if (!response.ok) throw await refusal(response);
  return readResidency(response.headers);
}

/**
 * Read a HEAD's answer.
 *
 * Split out and exported so it can be tested against a plain `Headers` — the
 * parsing is the part with edge cases, and it should not need a server to
 * exercise.
 */
export function readResidency(headers: Headers): ContentResidency {
  const number = (name: string): number => {
    const raw = headers.get(name);
    const value = raw === null ? Number.NaN : Number(raw);
    return Number.isFinite(value) && value >= 0 ? value : 0;
  };
  return {
    size: number("content-length"),
    chunkCount: number("x-lait-chunk-count"),
    residentChunks: number("x-lait-resident-chunks"),
    pinned: headers.get("x-lait-pinned") === "1",
  };
}

/**
 * Where to navigate to save this file.
 *
 * Pure, so the name-and-escaping rule is testable without a network. `name` is
 * advisory in both directions: the engine sanitises whatever arrives, and it is
 * sent only so the saved file is called something recognisable.
 */
export function downloadUrl(
  space: string,
  content: string,
  name?: string,
  offset = 0,
): string {
  const query = new URLSearchParams();
  if (offset > 0) query.set("offset", String(offset));
  if (name) query.set("name", name);
  const suffix = query.toString();
  return `${contentPath(space, content)}${suffix ? `?${suffix}` : ""}`;
}

/**
 * Send a file, and get back the id a Body may then name.
 *
 * The length is declared up front because the engine treats it as
 * authoritative: without it, a truncated upload and a complete one are the same
 * request, and the difference is a stored file that is permanently wrong.
 */
export async function upload(
  space: string,
  file: File | Blob,
  signal?: AbortSignal,
): Promise<StoredContent> {
  const response = await fetch(
    `/api/spaces/${encodeURIComponent(space)}/content?len=${file.size}`,
    {
      method: "POST",
      credentials: "same-origin",
      body: file,
      ...(signal ? { signal } : {}),
    },
  );
  if (!response.ok) throw await refusal(response);
  const body = (await response.json().catch(() => null)) as StoredContent | null;
  if (!body || typeof body.content !== "string") {
    throw new ContentError("the engine stored the file but did not name it", 502);
  }
  return body;
}

function contentPath(space: string, content: string): string {
  return `/api/spaces/${encodeURIComponent(space)}/content/${encodeURIComponent(content)}`;
}
