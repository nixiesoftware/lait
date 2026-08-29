/**
 * The fleet and everything that reaches it — one copy, for the whole app.
 *
 * A bezel drawing what a screen shows needs the same seven lists a footprint
 * needs to say whom a rule reaches, which are the same seven the sky stack
 * needs to rank claims. They are held once, here, outside React: a page that
 * mounts reads what is already known and draws on its first frame, and the
 * doorbell re-reads in the background. "Loading…" is a state a person sees
 * once per session, not once per click.
 *
 * Every mutation goes through `commit`: the change is applied to the held copy
 * *before* the write is sent, so the outcome is on screen in the same frame as
 * the gesture. The World's durable commit lands a moment later and the
 * doorbell re-reads; if the World refuses, the held copy is re-read from the
 * World and the refusal is thrown to whoever asked, so the surface that made
 * the change can say so on itself. There is no second model: what is shown
 * is the World's copy plus the changes it has not yet answered, and nothing
 * else.
 */

import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";
import { useRevision } from "@/ds";
import {
  deleteAudience,
  deleteBroadcast,
  deleteChannel,
  fetchAudiences,
  fetchBroadcasts,
  fetchChannels,
  fetchPresets,
  saveAudience,
  saveBroadcast,
  saveChannel,
} from "@/utils/apps/api";
import { deleteProgram, fetchPrograms, saveProgram } from "@/utils/broadcasts/api";
import { deleteMedia, fetchLibrary, saveMedia } from "@/utils/content/api";
import { deleteScreen, fetchScreens, saveScreen } from "@/utils/screens/api";
import {
  resolvePlayback,
  screensReached,
  type ResolutionInputs,
} from "@/utils/lait/resolve";
import type {
  Match,
  Playback,
  SignageAudience,
  SignageBroadcast,
  SignageChannel,
  SignageMedia,
  SignagePreset,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

export type Lists = {
  screens: SignageScreen[];
  channels: SignageChannel[];
  broadcasts: SignageBroadcast[];
  audiences: SignageAudience[];
  programs: SignageProgram[];
  media: SignageMedia[];
  presets: SignagePreset[];
};

type Held = {
  lists: Lists;
  /** The first read has landed. Before it, the lists are empty and `loading`. */
  loaded: boolean;
  loading: boolean;
  error: string | null;
};

const EMPTY: Lists = {
  screens: [],
  channels: [],
  broadcasts: [],
  audiences: [],
  programs: [],
  media: [],
  presets: [],
};

let held: Held = { lists: EMPTY, loaded: false, loading: false, error: null };
const listeners = new Set<() => void>();

function emit() {
  for (const listener of listeners) listener();
}

function set(next: Partial<Held>) {
  held = { ...held, ...next };
  emit();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function snapshot(): Held {
  return held;
}

// ── Reading ─────────────────────────────────────────────────────────────────

let reading: Promise<void> | null = null;
let readAgain = false;
/** Writes in flight. While any is, an arriving read must not overwrite what
 *  the person just did with what the World knew a moment before. */
let writing = 0;
let lastRevision = -1;

async function read(): Promise<void> {
  set({ loading: true, error: null });
  try {
    const [screens, channels, broadcasts, audiences, programs, media, presets] =
      await Promise.all([
        fetchScreens(),
        fetchChannels(),
        fetchBroadcasts(),
        fetchAudiences(),
        fetchPrograms(),
        fetchLibrary(),
        fetchPresets(),
      ]);
    if (writing > 0) {
      // Somebody's change is still on its way to the World; what we just read
      // predates it. Keep the held copy and read again once the write lands.
      readAgain = true;
      set({ loading: false });
      return;
    }
    set({
      lists: { screens, channels, broadcasts, audiences, programs, media, presets },
      loaded: true,
      loading: false,
      error: null,
    });
  } catch (err) {
    set({
      loading: false,
      error: err instanceof Error ? err.message : "Could not load the fleet",
    });
  }
}

/** Re-read everything, once, however many surfaces ask at the same moment. */
export function refresh(): Promise<void> {
  if (reading) {
    readAgain = true;
    return reading;
  }
  reading = read().finally(() => {
    reading = null;
    if (readAgain) {
      readAgain = false;
      void refresh();
    }
  });
  return reading;
}

// ── Writing ─────────────────────────────────────────────────────────────────

/**
 * Apply a change to the held copy now, then write it. The held copy is the
 * truth on screen until the World answers; a refusal re-reads and rethrows.
 */
async function commit<T>(apply: (lists: Lists) => Lists, write: () => Promise<T>): Promise<T> {
  set({ lists: apply(held.lists) });
  writing += 1;
  try {
    return await write();
  } catch (err) {
    void refresh();
    throw err;
  } finally {
    writing -= 1;
    if (writing === 0 && readAgain) {
      readAgain = false;
      void refresh();
    }
  }
}

function upsert<T extends { id: string }>(rows: T[], row: T): T[] {
  const at = rows.findIndex((entry) => entry.id === row.id);
  if (at < 0) return [...rows, row];
  const next = rows.slice();
  next[at] = row;
  return next;
}

function without<T extends { id: string }>(rows: T[], id: string): T[] {
  return rows.filter((entry) => entry.id !== id);
}

export function putScreen(screen: SignageScreen): Promise<unknown> {
  return commit(
    (lists) => ({ ...lists, screens: upsert(lists.screens, screen) }),
    () => saveScreen(screen),
  );
}

/** Removes and returns what was removed, so the caller can offer to undo. */
export function removeScreen(id: string): Promise<SignageScreen | null> {
  const was = held.lists.screens.find((entry) => entry.id === id) ?? null;
  return commit(
    (lists) => ({ ...lists, screens: without(lists.screens, id) }),
    async () => {
      await deleteScreen(id);
      return was;
    },
  );
}

/** Point a screen at a channel, or at nothing. One write, from the held copy. */
export function tune(screenId: string, channelId: string | null): Promise<unknown> {
  const screen = held.lists.screens.find((entry) => entry.id === screenId);
  if (!screen) return Promise.reject(new Error("that screen is not here"));
  if ((screen.tuned ?? null) === channelId) return Promise.resolve();
  return putScreen({ ...screen, tuned: channelId });
}

export function putChannel(channel: SignageChannel): Promise<unknown> {
  return commit(
    (lists) => ({ ...lists, channels: upsert(lists.channels, channel) }),
    () => saveChannel(channel),
  );
}

export function removeChannel(id: string): Promise<SignageChannel | null> {
  const was = held.lists.channels.find((entry) => entry.id === id) ?? null;
  return commit(
    (lists) => ({ ...lists, channels: without(lists.channels, id) }),
    async () => {
      await deleteChannel(id);
      return was;
    },
  );
}

export function putBroadcast(broadcast: SignageBroadcast): Promise<unknown> {
  return commit(
    (lists) => ({ ...lists, broadcasts: upsert(lists.broadcasts, broadcast) }),
    () => saveBroadcast(broadcast),
  );
}

export function removeBroadcast(id: string): Promise<SignageBroadcast | null> {
  const was = held.lists.broadcasts.find((entry) => entry.id === id) ?? null;
  return commit(
    (lists) => ({ ...lists, broadcasts: without(lists.broadcasts, id) }),
    async () => {
      await deleteBroadcast(id);
      return was;
    },
  );
}

export function putAudience(audience: SignageAudience): Promise<unknown> {
  return commit(
    (lists) => ({ ...lists, audiences: upsert(lists.audiences, audience) }),
    () => saveAudience(audience),
  );
}

export function removeAudience(id: string): Promise<SignageAudience | null> {
  const was = held.lists.audiences.find((entry) => entry.id === id) ?? null;
  return commit(
    (lists) => ({ ...lists, audiences: without(lists.audiences, id) }),
    async () => {
      await deleteAudience(id);
      return was;
    },
  );
}

export function putProgram(program: SignageProgram): Promise<unknown> {
  return commit(
    (lists) => ({ ...lists, programs: upsert(lists.programs, program) }),
    () => saveProgram(program),
  );
}

export function removeProgram(id: string): Promise<SignageProgram | null> {
  const was = held.lists.programs.find((entry) => entry.id === id) ?? null;
  return commit(
    (lists) => ({ ...lists, programs: without(lists.programs, id) }),
    async () => {
      await deleteProgram(id);
      return was;
    },
  );
}

export function putMedia(media: SignageMedia): Promise<unknown> {
  return commit(
    (lists) => ({ ...lists, media: upsert(lists.media, media) }),
    () => saveMedia(media),
  );
}

export function removeMedia(id: string): Promise<SignageMedia | null> {
  const was = held.lists.media.find((entry) => entry.id === id) ?? null;
  return commit(
    (lists) => ({ ...lists, media: without(lists.media, id) }),
    async () => {
      await deleteMedia(id);
      return was;
    },
  );
}

/** Something arrived by another road (an upload, the editor): hold it. */
export function adopt(patch: Partial<Lists>): void {
  set({ lists: { ...held.lists, ...patch } });
}

/** What the store holds right now, for code outside React. */
export function current(): Lists {
  return held.lists;
}

// ── Reading, from React ─────────────────────────────────────────────────────

export type Fleet = Lists & {
  /** True only before the first read lands. */
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
  /** What one screen shows at `now`, by the same ladder the World uses. */
  playbackFor: (screen: SignageScreen, now: number) => Playback;
  /** The ids a rule reaches, out of the whole fleet. */
  reachedBy: (rule: Match) => Set<string>;
  /** Screens tuned to a channel. */
  tunedTo: (channel: string) => SignageScreen[];
};

export function useFleet(): Fleet {
  const revision = useRevision();
  const state = useSyncExternalStore(subscribe, snapshot, snapshot);

  useEffect(() => {
    if (revision === lastRevision) return;
    lastRevision = revision;
    void refresh();
  }, [revision]);

  const { lists } = state;

  const playbackFor = useCallback(
    (screen: SignageScreen, now: number) => {
      const inputs: ResolutionInputs = { ...lists, screen };
      return resolvePlayback(inputs, now);
    },
    [lists],
  );

  const reachedBy = useCallback(
    (rule: Match) =>
      new Set(screensReached(rule, lists.screens, lists.audiences).map((screen) => screen.id)),
    [lists],
  );

  const tunedTo = useCallback(
    (channel: string) => lists.screens.filter((screen) => screen.tuned === channel),
    [lists],
  );

  return useMemo(
    () => ({
      ...lists,
      loading: !state.loaded,
      error: state.error,
      reload: refresh,
      playbackFor,
      reachedBy,
      tunedTo,
    }),
    [lists, state.loaded, state.error, playbackFor, reachedBy, tunedTo],
  );
}
