import { mintBodyId } from "@/utils/lait/ids";
import type { SignageItem, SignageMedia, SignageProgram } from "@/utils/lait/types";

export const DEFAULT_ITEM_MS = 10_000;
export const MIN_ITEM_MS = 1_000;
export const MIN_CLIP_PX = 72;
export const PX_PER_MS = 0.028;
export const MIN_ZOOM = 0.4;
export const MAX_ZOOM = 3.5;
export const CLIP_GAP_PX = 10;
export const GRAVITY_PX = 18;
export const ADD_SLOT_PX = 80;
export const ADD_GAP_PX = 8;
export const HOLD_MS = 380;

export function itemDurationMs(
  item: SignageItem,
  media: SignageMedia | undefined,
): number {
  return item.duration_ms ?? media?.duration_ms ?? DEFAULT_ITEM_MS;
}

export function mediaById(library: SignageMedia[]): Map<string, SignageMedia> {
  return new Map(library.map((entry) => [entry.id, entry]));
}

export function programDurationMs(
  program: SignageProgram,
  library: Map<string, SignageMedia>,
): number {
  return program.items.reduce(
    (sum, item) => sum + itemDurationMs(item, library.get(item.media)),
    0,
  );
}

export type LaidClip = {
  item: SignageItem;
  media: SignageMedia | undefined;
  index: number;
  startMs: number;
  durationMs: number;
  x: number;
  w: number;
};

export function layout(
  program: SignageProgram,
  library: Map<string, SignageMedia>,
  pxPerMs: number = PX_PER_MS,
): LaidClip[] {
  const clips: LaidClip[] = [];
  let t = 0;
  let x = 0;
  program.items.forEach((item, index) => {
    const durationMs = itemDurationMs(item, library.get(item.media));
    const w = Math.max(MIN_CLIP_PX, durationMs * pxPerMs);
    clips.push({
      item,
      media: library.get(item.media),
      index,
      startMs: t,
      durationMs,
      x,
      w,
    });
    t += durationMs;
    x += w;
    if (index < program.items.length - 1) x += CLIP_GAP_PX;
  });
  return clips;
}

export function playheadX(clips: LaidClip[], t: number): number {
  if (clips.length === 0) return 0;
  const last = clips[clips.length - 1];
  if (t >= last.startMs + last.durationMs) return last.x + last.w;
  const clip = clips.find(
    (c) => t >= c.startMs && t < c.startMs + c.durationMs,
  );
  if (!clip) return 0;
  const frac = (t - clip.startMs) / clip.durationMs;
  return clip.x + frac * clip.w;
}

export function timeAtX(clips: LaidClip[], x: number): number {
  if (clips.length === 0) return 0;
  if (x <= 0) return 0;
  for (const clip of clips) {
    if (x < clip.x + clip.w) {
      const frac = Math.max(0, (x - clip.x) / clip.w);
      return clip.startMs + frac * clip.durationMs;
    }
  }
  const last = clips[clips.length - 1];
  return last.startMs + last.durationMs;
}

export function insertIndexAtX(clips: LaidClip[], x: number): number {
  for (let i = 0; i < clips.length; i++) {
    if (x < clips[i].x + clips[i].w / 2) return i;
  }
  return clips.length;
}

export function itemAtTime(
  clips: LaidClip[],
  t: number,
): LaidClip | null {
  if (clips.length === 0) return null;
  return (
    clips.find((c) => t >= c.startMs && t < c.startMs + c.durationMs) ??
    clips[clips.length - 1]
  );
}

export function rename(program: SignageProgram, name: string): SignageProgram {
  return { ...program, name };
}

export function addMedia(
  program: SignageProgram,
  media: SignageMedia,
  afterItemId: string | null,
): SignageProgram {
  const item: SignageItem = {
    id: mintBodyId(),
    media: media.id,
    duration_ms: null,
  };
  const items = [...program.items];
  const at =
    afterItemId === null
      ? items.length
      : items.findIndex((row) => row.id === afterItemId) + 1;
  const index = at <= 0 ? items.length : at;
  items.splice(index, 0, item);
  return { ...program, items };
}

export function removeItem(
  program: SignageProgram,
  itemId: string,
): SignageProgram {
  const items = program.items.filter((item) => item.id !== itemId);
  const keep = new Set(items.map((item) => item.id));
  return {
    ...program,
    items,
    windows: program.windows.map((window) => ({
      ...window,
      items: window.items.filter((id) => keep.has(id)),
    })),
  };
}

export function moveItem(
  program: SignageProgram,
  from: number,
  to: number,
): SignageProgram {
  if (from === to || from < 0 || from >= program.items.length) return program;
  const items = [...program.items];
  const [row] = items.splice(from, 1);
  const dest = to > from ? to - 1 : to;
  items.splice(Math.max(0, Math.min(items.length, dest)), 0, row);
  return { ...program, items };
}

export function duplicateItem(
  program: SignageProgram,
  itemId: string,
): SignageProgram {
  const index = program.items.findIndex((item) => item.id === itemId);
  if (index < 0) return program;
  const copy: SignageItem = {
    ...program.items[index],
    id: mintBodyId(),
  };
  const items = [...program.items];
  items.splice(index + 1, 0, copy);
  return { ...program, items };
}

export type ClipCopy = {
  media: string;
  duration_ms: number | null;
};

export function copyItem(
  program: SignageProgram,
  itemId: string,
): ClipCopy | null {
  const item = program.items.find((row) => row.id === itemId);
  if (!item) return null;
  return { media: item.media, duration_ms: item.duration_ms };
}

export function pasteItem(
  program: SignageProgram,
  copy: ClipCopy,
  afterItemId: string | null,
): SignageProgram {
  const item: SignageItem = {
    id: mintBodyId(),
    media: copy.media,
    duration_ms: copy.duration_ms,
  };
  const items = [...program.items];
  const at =
    afterItemId === null
      ? items.length
      : items.findIndex((row) => row.id === afterItemId) + 1;
  const index = at <= 0 ? items.length : at;
  items.splice(index, 0, item);
  return { ...program, items };
}

export function setDuration(
  program: SignageProgram,
  itemId: string,
  durationMs: number,
): SignageProgram {
  const ms = Math.max(MIN_ITEM_MS, Math.round(durationMs));
  return {
    ...program,
    items: program.items.map((item) =>
      item.id === itemId ? { ...item, duration_ms: ms } : item,
    ),
  };
}

export function sameProgram(a: SignageProgram, b: SignageProgram): boolean {
  return (
    a.name === b.name &&
    a.cycle === b.cycle &&
    a.items.length === b.items.length &&
    a.items.every(
      (item, i) =>
        item.id === b.items[i].id &&
        item.media === b.items[i].media &&
        item.duration_ms === b.items[i].duration_ms,
    )
  );
}

export function storedContentUrl(
  orbit: string,
  media: SignageMedia,
): string | null {
  if (media.source !== "stored") return null;
  return `/api/spaces/${encodeURIComponent(orbit)}/content/${encodeURIComponent(media.content)}`;
}

export function formatClock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/** Short duration on a clip: `8s`, then `1:30` past a minute. Seconds floor. */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  if (total < 60) return `${total}s`;
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

export function clampZoom(zoom: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom));
}

export function nativeDurationMs(media: SignageMedia | undefined): number | null {
  if (!media || media.source !== "stored") return null;
  if (!media.mime.startsWith("video/")) return null;
  return media.duration_ms;
}

export function widthForDuration(durationMs: number, pxPerMs: number): number {
  return Math.max(MIN_CLIP_PX, durationMs * pxPerMs);
}

export function durationForWidth(width: number, pxPerMs: number): number {
  return Math.max(MIN_ITEM_MS, Math.round(width / pxPerMs));
}

export type TrimEdge = "left" | "right";

export type TrimPreview = {
  id: string;
  edge: TrimEdge;
  durationMs: number;
};

export function rulerIntervalSec(pxPerMs: number): number {
  const pxPerSec = pxPerMs * 1000;
  if (pxPerSec >= 48) return 1;
  if (pxPerSec >= 18) return 5;
  if (pxPerSec >= 8) return 10;
  return 30;
}
