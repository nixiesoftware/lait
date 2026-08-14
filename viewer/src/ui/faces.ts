import { useEffect, useReducer } from "react";

import { hostRpc } from "../api";

/**
 * Authored faces, resolved through the address book — the identity API.
 *
 * The book is the one namer, and the one face-holder: a Card carries a
 * picture (stamped canonically for known coding agents at provision, authored
 * for everyone else), and every application asks the book instead of keeping
 * a name-matched table of its own. This module is that ask, batched: surfaces
 * call `useFace(memberKey)` and the cache resolves whatever is missing with
 * one `book_resolve` per microtask.
 *
 * Scope is one space at a time (the actor spelling needs the `ws_` id and the
 * resolve needs the orbit); `App` sets it on selection. A key the book does
 * not answer stays `null` — the key-derived fallback, not an error — and a
 * failed resolve marks the keys asked so absence cannot become a fetch loop.
 */
const faces = new Map<string, string | null>();
const listeners = new Set<() => void>();
let orbit: string | null = null;
let ws: string | null = null;
const queue = new Set<string>();
let flushing = false;

/** Point the cache at the selected space. Clears on any change of scope. */
export function setFaceScope(nextOrbit: string | null, nextWs: string | null) {
  if (orbit === nextOrbit && ws === nextWs) return;
  orbit = nextOrbit;
  ws = nextWs;
  faces.clear();
  queue.clear();
  notify();
}

function notify() {
  for (const cb of [...listeners]) cb();
}

function enqueue(key: string) {
  if (queue.has(key)) return;
  queue.add(key);
  if (!flushing) {
    flushing = true;
    queueMicrotask(() => void flush());
  }
}

async function flush(): Promise<void> {
  const scopeOrbit = orbit;
  const scopeWs = ws;
  const keys = [...queue];
  queue.clear();
  if (!scopeOrbit || !scopeWs || keys.length === 0) {
    flushing = false;
    return;
  }
  // Asked-and-unanswered is recorded up front so a refused or failed resolve
  // cannot loop the same keys forever.
  for (const key of keys) faces.set(key, null);
  const handles = keys.map((key) =>
    key.startsWith("act_") ? `actor:${scopeWs}:${key}` : key,
  );
  try {
    const r = await hostRpc({ cmd: "book_resolve", orbit: scopeOrbit, handles });
    if (r.kind === "book_resolution" && orbit === scopeOrbit) {
      for (const hit of r.hits) {
        if (!hit.picture) continue;
        const key = hit.handle.startsWith("actor:")
          ? hit.handle.split(":")[2]
          : hit.handle;
        if (key) faces.set(key, hit.picture);
      }
    }
  } catch {
    // Absence, never an error: the rows keep their key-derived fallback.
  }
  notify();
  if (queue.size > 0) {
    void flush();
  } else {
    flushing = false;
  }
}

/**
 * The authored face for a member key, or `null` while unknown or absent.
 * Subscribes the calling component; resolution is lazy and batched.
 */
export function useFace(key: string | undefined): string | null {
  const [, force] = useReducer((c: number) => c + 1, 0);
  useEffect(() => {
    listeners.add(force);
    return () => {
      listeners.delete(force);
    };
  }, []);
  if (!key || !orbit || !ws) return null;
  if (faces.has(key)) return faces.get(key) ?? null;
  enqueue(key);
  return null;
}

/** The stored `<mime>;base64,<data>` form as a drawable URL. */
export function faceUrl(stored: string): string {
  return `data:${stored}`;
}
