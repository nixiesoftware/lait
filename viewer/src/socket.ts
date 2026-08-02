/**
 * The browser socket — the socket that carries what the doorbell stream must not.
 *
 * `useDoorbell` stays exactly as it is. Its `EventSource` carries dirty flags
 * over one broadcast ring, and a `lagged` there costs every tab a full
 * rebaseline. That is the correct price for "something you can see has changed"
 * and the wrong price for "this upload is at 40%", which is superseded half a
 * second later and matters to one tab.
 *
 * So progress rides its own socket, and nothing in this module touches the
 * doorbell.
 *
 * Three lanes arrive here and they are not interchangeable. **Progress** and
 * **transient** may drop: the next number and the next facepile supersede the
 * ones that were lost, so falling behind costs staleness rather than backlog.
 * **Control** may not: a delivered signal — an invitation, a file offer, a
 * request for attention — has no successor, so the engine drops the socket
 * rather than the fact, and a `retrying` liveness after a quiet close is how
 * that shows up here.
 *
 * The frame envelope is binary postcard, so every frame carries the protocol
 * version — not just the handshake — and a tab left open across a daemon restart
 * is caught on the first frame rather than the first misreading of one. The
 * bodies are not uniform: progress is postcard because it is the high-rate lane
 * and a number should cost bytes rather than a parse, while transient and
 * control bodies are JSON, because they carry `control.rs` `Response` values
 * that `types.ts` already mirrors. Decoding those by hand here would be a second
 * decoder for shapes that already have one, and the two would drift.
 */

import type { Response } from "./types";

/** Matches `SOCKET_PROTOCOL_VERSION` in `src/serve/socket.rs`. */
export const socketProtocolVersion = 1;

/** Matches `MAX_SOCKET_FRAME_BYTES`. Checked before anything is allocated. */
export const maxSocketFrameBytes = 64 * 1024;

/** Matches `Lane` in `src/serve/socket.rs`. Appended to, never reordered: the
 *  engine encodes a lane by its declaration index, so renumbering one would make
 *  every frame in flight decode as a different lane. */
export const lane = { control: 0, progress: 1, transient: 2 } as const;
export type LaneId = (typeof lane)[keyof typeof lane];

/** What the engine says about one transfer. */
export interface TransferProgress {
  transfer: string;
  content: string;
  moved: number;
  total: number;
  done: boolean;
}

/** The `live` reply, exactly as an RPC would return it. */
export type LiveReply = Extract<Response, { kind: "live" }>;

/** The `signals` drain, exactly as an RPC would return it. */
export type SignalsReply = Extract<Response, { kind: "signals" }>;

/** Connection state a person should be shown. Named apart from the doorbell's
 *  `Liveness` because the two can disagree, and a single word for both would
 *  hide exactly the case worth seeing. */
export type SocketLiveness = "connecting" | "live" | "retrying" | "stale";

export type SocketEvent =
  | { kind: "progress"; progress: TransferProgress }
  /** The answer to the question this socket declared. `issue` is `null` for the
   *  whole table, and it is carried because the engine narrows the rows to an
   *  issue while counting generations for the whole table — a tab has to know
   *  the answer is the one it asked for. */
  | { kind: "live"; space: string; issue: string | null; view: LiveReply }
  /** Signals the engine drained on this tab's behalf. It drains once for the
   *  whole server, so nothing else may drain the same space: two drainers take
   *  half the set each and neither sees the whole. A consumer that ignores one
   *  of these has destroyed it — the daemon's queue is already empty. */
  | { kind: "signals"; space: string; drained: SignalsReply }
  | { kind: "liveness"; liveness: SocketLiveness };

/**
 * What a socket asks to be kept up to date on. `null` stops the watch.
 *
 * `issue` is an `iss_` doc id and not a project alias — the engine derives the
 * Body id from the string as given, and an alias hashes to a Body nothing
 * publishes under. Omitting it is not "the whole table": it names the space
 * this tab is in, which is what the engine drains signals for, and asks no live
 * question at all.
 */
export interface BrowserCursor {
  field: string;
  /** Unicode-scalar offsets in the stored Markdown, never UTF-16 DOM offsets. */
  anchor: number;
  focus?: number;
}

export type Question = {
  space: string;
  issue?: string;
  cursor?: BrowserCursor;
  typing?: boolean;
} | null;

/**
 * Decode one frame.
 *
 * The length is checked first and separately, because a decoder handed a
 * hostile length has already done the allocation by the time it fails. Returns
 * `null` for anything it will not accept — a lane it does not know, a version it
 * does not speak, a body that does not parse, a body on the wrong lane — because
 * every one of those has the same correct response here, which is to ignore the
 * frame.
 */
export function decodeFrame(bytes: Uint8Array): SocketEvent | null {
  if (bytes.byteLength > maxSocketFrameBytes) return null;
  const cursor = { at: 0, bytes };
  const version = readVarint(cursor);
  if (version !== socketProtocolVersion) return null;
  const laneId = readVarint(cursor);
  const bodyLength = readVarint(cursor);
  if (bodyLength === null || bodyLength > maxSocketFrameBytes) return null;
  if (cursor.at + bodyLength > bytes.byteLength) return null;
  const body = bytes.subarray(cursor.at, cursor.at + bodyLength);
  if (laneId === lane.progress) {
    const progress = decodeProgress(body);
    return progress ? { kind: "progress", progress } : null;
  }
  if (laneId === lane.transient || laneId === lane.control) {
    return decodeSpaceFrame(laneId, body);
  }
  return null;
}

/**
 * A JSON body: one `kind`-tagged control-plane value, addressed to a space.
 *
 * The lane decides which kinds are admissible rather than merely labelling them.
 * A `signals` body arriving on the lane that may drop would be a signal the
 * engine had promised to deliver and then queued somewhere it can be lost, and
 * accepting it here would hide that.
 */
function decodeSpaceFrame(laneId: number, body: Uint8Array): SocketEvent | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(body));
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const framed = parsed as { space?: unknown; issue?: unknown; kind?: unknown };
  if (typeof framed.space !== "string") return null;
  if (laneId === lane.transient && framed.kind === "live") {
    return {
      kind: "live",
      space: framed.space,
      issue: typeof framed.issue === "string" ? framed.issue : null,
      view: parsed as LiveReply,
    };
  }
  if (laneId === lane.control && framed.kind === "signals") {
    return { kind: "signals", space: framed.space, drained: parsed as SignalsReply };
  }
  return null;
}

/** postcard's `varint(u32)`: seven bits per byte, low group first, high bit
 *  continues. */
function readVarint(cursor: { at: number; bytes: Uint8Array }): number | null {
  let value = 0;
  let shift = 0;
  while (cursor.at < cursor.bytes.length) {
    // `noUncheckedIndexedAccess` is on, and the bound above is what makes this
    // safe — spelled out rather than asserted away.
    const byte = cursor.bytes[cursor.at++] ?? 0;
    value |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) return value >>> 0;
    shift += 7;
    if (shift > 28) return null;
  }
  return null;
}

function writeVarint(value: number): number[] {
  const out: number[] = [];
  let rest = value >>> 0;
  while (rest >= 0x80) {
    out.push((rest & 0x7f) | 0x80);
    rest >>>= 7;
  }
  out.push(rest);
  return out;
}

/** The one frame this side sends: the same envelope, so the engine's version
 *  and lane checks see exactly what they see from any other sender. */
export function encodeFrame(laneId: LaneId, body: Uint8Array): Uint8Array {
  const head = [
    ...writeVarint(socketProtocolVersion),
    ...writeVarint(laneId),
    ...writeVarint(body.byteLength),
  ];
  const framed = new Uint8Array(head.length + body.byteLength);
  framed.set(head, 0);
  framed.set(body, head.length);
  return framed;
}

function decodeProgress(body: Uint8Array): TransferProgress | null {
  const cursor = { at: 0, bytes: body };
  const transfer = readString(cursor);
  const content = readString(cursor);
  const moved = readVarint(cursor);
  const total = readVarint(cursor);
  if (cursor.at >= body.length) return null;
  const done = body[cursor.at] === 1;
  if (transfer === null || content === null || moved === null || total === null) {
    return null;
  }
  return { transfer, content, moved, total, done };
}

function readString(cursor: { at: number; bytes: Uint8Array }): string | null {
  const length = readVarint(cursor);
  if (length === null || cursor.at + length > cursor.bytes.length) return null;
  const slice = cursor.bytes.subarray(cursor.at, cursor.at + length);
  cursor.at += length;
  return new TextDecoder().decode(slice);
}

/** Reconnect backoff. Its own, not the doorbell's — `EventSource` reconnects
 *  by itself and a `WebSocket` does not. */
const reconnectFloorMs = 500;
const reconnectCeilingMs = 30_000;

/** An open socket. */
export interface Socket {
  /**
   * Declare what this socket wants the live view of.
   *
   * A declaration, not a subscription: it replaces whatever was declared last,
   * and it is re-sent after every reconnect, because the engine holds it per
   * socket and a reconnected socket is a new one. The engine asks each declared
   * question once per tick for the whole server, so two tabs on one issue cost
   * one read and a question nobody holds is never asked.
   */
  watch(question: Question): void;
  close(): void;
}

/**
 * Open the socket and keep it open.
 *
 * Returns a handle that closes it. Symmetric by construction, because
 * `<StrictMode>` double-mounts effects in development and an unclosed socket
 * per mount would eat the browser's six-per-origin budget in a handful of
 * navigations.
 */
export function openSocket(onEvent: (event: SocketEvent) => void): Socket {
  let socket: WebSocket | null = null;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let backoff = reconnectFloorMs;
  let closed = false;
  let declared: Question = null;

  const declare = (): void => {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    const body = new TextEncoder().encode(JSON.stringify(declared ?? { space: null }));
    socket.send(encodeFrame(lane.transient, body));
  };

  const connect = (): void => {
    if (closed) return;
    onEvent({ kind: "liveness", liveness: "connecting" });
    const url = new URL("/api/session", window.location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    // No token in the URL. The cookie rides a same-origin handshake, and the
    // engine refuses a query credential on this route anyway.
    const next = new WebSocket(url);
    next.binaryType = "arraybuffer";
    socket = next;

    next.onopen = () => {
      backoff = reconnectFloorMs;
      onEvent({ kind: "liveness", liveness: "live" });
      // The engine holds the declaration per socket, so a reconnected socket
      // starts with none. Re-sending is what keeps a dropped connection from
      // silently ending the presence a surface is still drawing.
      declare();
    };
    next.onmessage = (message: MessageEvent<ArrayBuffer | string>) => {
      if (typeof message.data === "string") return;
      const event = decodeFrame(new Uint8Array(message.data));
      if (event) onEvent(event);
    };
    next.onclose = (event: CloseEvent) => {
      if (closed) return;
      // 1002 is the engine saying this tab is from another build. Retrying
      // cannot fix that, and retrying forever would hide it.
      if (event.code === 1002) {
        onEvent({ kind: "liveness", liveness: "stale" });
        return;
      }
      onEvent({ kind: "liveness", liveness: "retrying" });
      retry = setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, reconnectCeilingMs);
    };
    next.onerror = () => {
      // `onclose` always follows, and it owns the retry — doing it here too
      // would double the reconnection rate for every failure.
    };
  };

  connect();
  return {
    watch(question: Question) {
      declared = question;
      declare();
    },
    close() {
      closed = true;
      if (retry !== null) clearTimeout(retry);
      socket?.close();
    },
  };
}
