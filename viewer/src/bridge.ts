/**
 * The browser bridge — the socket that carries what the doorbell stream must not.
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
 * Frames are binary postcard, not JSON, because the progress lane is the
 * high-rate one and a number should cost bytes rather than a parse. Every frame
 * carries the protocol version — not just the handshake — so a tab left open
 * across a daemon restart is caught on the first frame rather than the first
 * misreading of one.
 */

/** Matches `BRIDGE_PROTOCOL_VERSION` in `src/serve/bridge.rs`. */
export const bridgeProtocolVersion = 1;

/** Matches `MAX_BRIDGE_FRAME_BYTES`. Checked before anything is allocated. */
export const maxBridgeFrameBytes = 64 * 1024;

/** Matches `Lane` in `src/serve/bridge.rs`. */
export const lane = { control: 0, progress: 1 } as const;
export type LaneId = (typeof lane)[keyof typeof lane];

/** What the engine says about one transfer. */
export interface TransferProgress {
  transfer: string;
  content: string;
  moved: number;
  total: number;
  done: boolean;
}

/** Connection state a person should be shown. Named apart from the doorbell's
 *  `Liveness` because the two can disagree, and a single word for both would
 *  hide exactly the case worth seeing. */
export type BridgeLiveness = "connecting" | "live" | "retrying" | "stale";

export type BridgeEvent =
  | { kind: "progress"; progress: TransferProgress }
  | { kind: "liveness"; liveness: BridgeLiveness };

/**
 * Decode one frame.
 *
 * The length is checked first and separately, because a decoder handed a
 * hostile length has already done the allocation by the time it fails. Returns
 * `null` for anything it will not accept — a lane it does not know, a version it
 * does not speak, a body that does not parse — because every one of those has
 * the same correct response here, which is to ignore the frame.
 */
export function decodeFrame(bytes: Uint8Array): BridgeEvent | null {
  if (bytes.byteLength > maxBridgeFrameBytes) return null;
  const cursor = { at: 0, bytes };
  const version = readVarint(cursor);
  if (version !== bridgeProtocolVersion) return null;
  const laneId = readVarint(cursor);
  const bodyLength = readVarint(cursor);
  if (bodyLength === null || bodyLength > maxBridgeFrameBytes) return null;
  if (cursor.at + bodyLength > bytes.byteLength) return null;
  const body = bytes.subarray(cursor.at, cursor.at + bodyLength);
  if (laneId !== lane.progress) return null;
  const progress = decodeProgress(body);
  return progress ? { kind: "progress", progress } : null;
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

/**
 * Open the bridge and keep it open.
 *
 * Returns a function that closes it. Symmetric by construction, because
 * `<StrictMode>` double-mounts effects in development and an unclosed socket
 * per mount would eat the browser's six-per-origin budget in a handful of
 * navigations.
 */
export function openBridge(onEvent: (event: BridgeEvent) => void): () => void {
  let socket: WebSocket | null = null;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let backoff = reconnectFloorMs;
  let closed = false;

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
  return () => {
    closed = true;
    if (retry !== null) clearTimeout(retry);
    socket?.close();
  };
}
