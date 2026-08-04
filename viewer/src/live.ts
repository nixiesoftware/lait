/**
 * The Live plane, as a browser sees it.
 *
 * Two facts drive everything here.
 *
 * **None of it is durable.** A caret, a "somebody is looking at this", a
 * "somebody is typing" — the daemon holds them in a table it will happily
 * forget, and nothing replays them after a restart. So this module never
 * caches across a page load, never persists, and treats an empty answer as the
 * truth rather than as a failure to load.
 *
 * **The daemon may not know the request.** A tab left open across a
 * `host_update` + `host_restart` can be talking to a build whose control plane
 * predates `live`, which answers `bad request: unknown variant`. That is not an error worth showing anybody:
 * the correct response is to stop asking and let every surface draw without
 * presence. `unavailable` is that state, and it is held rather than thrown.
 *
 * **This module does not read anything.** It folds. The engine subscribes to
 * each live view once for the whole server and pushes changes down the socket's
 * transient lane, so a hundred tabs on one issue cost one stream; a second
 * reader here would be a second subscription asking the same question with its
 * own generation, and for `signals` it would be worse than duplication — that
 * request drains, so two callers split the set and neither sees the whole.
 *
 * Which is why what arrives on the control lane is *held* here rather than
 * merely decoded. The drain that produced it emptied the daemon's queue, so a
 * signal this module lets fall through is not late, it is gone — from the CLI
 * and the RPC surface as much as from the browser.
 *
 * The slots live in the app's `WorldViewStore` rather than in React state. Every
 * other live projection in this viewer is read through `useSyncExternalStore`
 * there, and a second mechanism for the same job would mean two answers to "what
 * is on screen right now" that can disagree. Nothing here touches the doorbell:
 * a facepile changing is not a reason to re-read a board.
 */

import { useCallback, useEffect, useMemo } from "react";

import {
  openSocket,
  type BrowserCursor,
  type BrowserTextPreview,
  type Socket,
  type SocketEvent,
  type Question,
} from "./socket";
import { useWorldResource, useWorldViewStore } from "./core/worldViewReact";
import type { ResourceKey, WorldViewStore } from "./core/worldViewStore";
import type {
  CaretPosition,
  LiveEntry,
  LiveScope,
  Response,
  SignalEntry,
  TextPreview,
  WorldRequest,
} from "./types";

/** What a surface draws from. */
export interface LiveState {
  /**
   * The generation the last answer carried, or `null` before the first one.
   *
   * `null` rather than `0`: the daemon's counter starts at zero, so sending
   * zero on a first read would claim to hold a table that has never been seen.
   */
  generation: number | null;
  /**
   * This node is not hearing from everyone it could be.
   *
   * Surfaced, never swallowed. Awareness is allowed to be incomplete; drawing
   * three of five people with no indication is a confident lie.
   */
  partial: boolean;
  entries: LiveEntry[];
  /**
   * The daemon cannot answer this request. Every entry is stale by definition,
   * so it is cleared — a facepile frozen on whoever happened to be there when
   * the daemon went away is worse than an empty one.
   */
  unavailable: boolean;
}

export const emptyLive: LiveState = {
  generation: null,
  partial: false,
  entries: [],
  unavailable: false,
};

/**
 * Fold one reply into held state.
 *
 * `live_unchanged` keeps the entries and takes the generation, which is the
 * whole point of the two-variant reply: the daemon said nothing moved, so
 * anything that redraws from this state must see the same array it saw last
 * time.
 */
export function applyLive(state: LiveState, reply: Response): LiveState {
  if (reply.kind === "live") {
    return {
      generation: reply.generation,
      partial: reply.partial,
      entries: reply.entries,
      unavailable: false,
    };
  }
  if (reply.kind === "live_unchanged") {
    // The same `entries` reference, not a copy. A consumer memoising on it is
    // relying on exactly the property the daemon just asserted.
    return { ...state, generation: reply.generation, unavailable: false };
  }
  return state;
}

/** The state after a failed read. */
export function liveUnavailable(): LiveState {
  return { ...emptyLive, unavailable: true };
}

/** One person, on this issue, right now. */
export interface WatcherRow {
  actor: string;
  /**
   * Past the caret grace window. The person is still drawn — a quiet
   * collaborator who stops moving has not left the room — but drawn as a guess,
   * because that is what the daemon says it is.
   */
  uncertain: boolean;
}

/**
 * Who is looking at this, freshest first, one row per person.
 *
 * De-duplicated by actor because one person on two devices is one person on a
 * facepile, and because the daemon's table is keyed by station: a laptop and a
 * phone open on the same issue are two entries and one human. The freshest
 * device decides the uncertainty, so a laptop that went quiet does not make
 * somebody typing on their phone look like a ghost.
 */
export function watching(entries: LiveEntry[]): WatcherRow[] {
  const freshest = new Map<string, { age: number; uncertain: boolean }>();
  for (const entry of entries) {
    if (entry.kind !== "presence") continue;
    const held = freshest.get(entry.actor);
    if (held === undefined || entry.age_ms < held.age) {
      freshest.set(entry.actor, { age: entry.age_ms, uncertain: entry.uncertain });
    }
  }
  return [...freshest.entries()]
    .sort((a, b) => a[1].age - b[1].age || a[0].localeCompare(b[0]))
    .map(([actor, seen]) => ({ actor, uncertain: seen.uncertain }));
}

/** The same rows, actors only. */
export function watchers(entries: LiveEntry[]): string[] {
  return watching(entries).map((row) => row.actor);
}

/** Who is typing, by actor. A coarse fact with no intermediate values. */
export function typists(entries: LiveEntry[]): string[] {
  const seen = new Set<string>();
  for (const entry of entries) {
    if (entry.kind === "typing") seen.add(entry.actor);
  }
  return [...seen].sort();
}

/** Where one person's caret is, in one field. */
export interface CaretRow {
  actor: string;
  /** The engine's name for the field, not a caption. Renaming it here would put
   *  a second vocabulary between the daemon's scope and what anybody reads. */
  field: string;
  position: CaretPosition;
  /** A selection's far end; null for a collapsed caret. */
  focus: CaretPosition | null;
  uncertain: boolean;
}

/**
 * Every caret worth drawing, freshest first, one row per person per field.
 *
 * Kind is not the filter — a carried position is. A `selection` is a caret with
 * a range, and its anchor is as real a position as a bare caret's; dropping it
 * would hide the person doing the most conspicuous thing on the page.
 */
export function carets(entries: LiveEntry[]): CaretRow[] {
  const freshest = new Map<string, CaretRow & { age: number }>();
  for (const entry of entries) {
    const position = entry.caret;
    if (position === null) continue;
    const field = fieldOf(entry.scope);
    if (field === null) continue;
    // A separator no actor id and no field name can contain, written as an
    // escape: a raw NUL in the source makes git call this file binary and stop
    // diffing and merging it.
    const at = `${entry.actor}\u0000${field}`;
    const held = freshest.get(at);
    if (held !== undefined && held.age <= entry.age_ms) continue;
    freshest.set(at, {
      actor: entry.actor,
      field,
      position,
      focus: entry.focus,
      uncertain: entry.uncertain,
      age: entry.age_ms,
    });
  }
  return [...freshest.values()]
    .sort((a, b) => a.age - b.age || a.actor.localeCompare(b.actor) || a.field.localeCompare(b.field))
    .map(({ actor, field, position, focus, uncertain }) => ({
      actor,
      field,
      position,
      focus,
      uncertain,
    }));
}

export interface PreviewRow {
  actor: string;
  field: string;
  preview: TextPreview;
  uncertain: boolean;
}

/** The newest cumulative preview per actor and field. Intermediate datagrams
 * are intentionally irrelevant: every row starts from a durable revision. */
export function previews(entries: LiveEntry[]): PreviewRow[] {
  const freshest = new Map<string, PreviewRow & { age: number }>();
  for (const entry of entries) {
    if (entry.kind !== "preview" || !entry.preview) continue;
    const field = fieldOf(entry.scope);
    if (field === null) continue;
    const key = `${entry.actor}\u0000${field}`;
    const held = freshest.get(key);
    if (held && held.age <= entry.age_ms) continue;
    freshest.set(key, {
      actor: entry.actor,
      field,
      preview: entry.preview,
      uncertain: entry.uncertain,
      age: entry.age_ms,
    });
  }
  return [...freshest.values()]
    .sort((a, b) => a.age - b.age || a.actor.localeCompare(b.actor))
    .map(({ actor, field, preview, uncertain }) => ({ actor, field, preview, uncertain }));
}

/** The editor scopes name a field; the rest are about a document, not a place
 * in one. */
function fieldOf(scope: LiveScope): string | null {
  if (
    scope.scope === "text_caret"
    || scope.scope === "text_preview"
    || scope.scope === "typing"
  ) return scope.field;
  return null;
}

/**
 * One caret position, in words.
 *
 * Here rather than in a component because these three strings are the only
 * place the three caret states become readable, and a second surface spelling
 * them its own way is how `drifted` starts reading like `unresolved`. They are
 * different facts: `drifted` says the material this position was attached to is
 * gone, `unresolved` says nobody worked out where it is. Neither is a position,
 * and neither may render as one — a stale offset drawn as a number is a caret
 * pointing confidently at the wrong character.
 */
export function caretPhrase(position: CaretPosition): string {
  if (position.caret === "at") return `position ${position.position}`;
  if (position.caret === "drifted") return "position lost";
  return "position unknown";
}

/** What has arrived on the control lane and not been acted on. */
export interface SignalDrain {
  signals: SignalEntry[];
  /**
   * How many were lost before anybody could act on them, oldest first — the
   * daemon's queue for want of room, and this list for the same reason.
   *
   * Worth showing. A dropped caret is superseded by the next one; a dropped
   * invitation is an invitation nobody will ever see again.
   */
  dropped: number;
}

export const noSignals: SignalDrain = { signals: [], dropped: 0 };

/**
 * How many drained signals one space holds before the oldest go.
 *
 * A bound is not optional: the daemon's drain is destructive, so nothing takes
 * these back out of this tab, and a list that only grew would be a leak on the
 * one lane that may not drop. The oldest go rather than the newest, which is
 * the daemon's own rule for the same queue.
 */
export const maxHeldSignals = 64;

/**
 * Add one delivery to what is held.
 *
 * Appended rather than replacing, because a signal is a fact acted on once and
 * a delivery that overwrote the previous one would lose whatever had not been
 * acted on yet. The counter accumulates for the same reason: a loss does not
 * stop being true when the next delivery lands.
 */
export function applySignals(held: SignalDrain, reply: Response): SignalDrain {
  if (reply.kind !== "signals") return held;
  const signals = [...held.signals, ...reply.signals];
  const overflow = Math.max(0, signals.length - maxHeldSignals);
  return {
    signals: signals.slice(overflow),
    dropped: held.dropped + reply.dropped + overflow,
  };
}

/** The key space live tables occupy in the `WorldViewStore`. Its own root rather
 *  than a leaf under `space:`, so one `evict` bounds every space at once. */
export const liveKeyPrefix = "live:";

export function liveKey(space: string, issue: string | null): ResourceKey {
  return `${liveKeyPrefix}${encodeURIComponent(space)}/${encodeURIComponent(issue ?? "_")}`;
}

/**
 * The key space drained signals occupy, one slot per space.
 *
 * Its own root, and deliberately not under `live:`: that prefix is swept and
 * evicted, and a signal evicted to make room for a facepile is the loss this
 * lane exists to prevent. One slot per space, bounded by `maxHeldSignals`.
 */
export const signalsKeyPrefix = "signals:";

export function signalsKey(space: string): ResourceKey {
  return `${signalsKeyPrefix}${encodeURIComponent(space)}`;
}

/** How many tables are kept for questions nobody is asking any more. Small: each
 *  one is a facepile that stopped being answered the moment its surface went
 *  away, so the cache buys a frame of continuity on back-navigation and nothing
 *  else. */
export const maxLiveSlots = 8;

/** How long an unasked table stays drawable before it is blanked. */
export const liveSilenceMs = 15_000;

/** How often the sweep runs. */
export const liveSweepMs = 5_000;
/** Matches the daemon's caret coalescing window. */
export const cursorPublishMs = 80;

export interface EditorAwareness {
  cursor: BrowserCursor | null;
  typing: boolean;
  preview?: BrowserTextPreview | null;
  /** The editor position belongs to text that has not reached the local CRDT
   * replica yet. Keep the last published anchor instead of minting this scalar
   * offset against the wrong document revision. */
  defer?: boolean;
}

/** Whether an absolute editor offset can be turned into a CRDT anchor now.
 *
 * A blur is always publishable because it retires a position. Every other
 * state must describe the exact text the local replica has acknowledged, with
 * no writes still queued between the two. */
export function awarenessReadyFor(
  snapshot: string,
  settled: string,
  pending: number,
  retiring: boolean,
): boolean {
  return retiring || (pending === 0 && snapshot === settled);
}

/**
 * The transient tables, held where every other projection is held.
 *
 * The store owns the data and publishes it; this owns the index — which keys
 * exist and when each last heard an answer. Two reasons the stamp is not in the
 * payload: the store's read bumps its own LRU clock, so a sweep that read every
 * slot to check its age would flatten the ordering `evict` sorts on, and a
 * timestamp is not something a surface should be able to redraw from.
 */
export class LiveSlots {
  private heard = new Map<ResourceKey, number>();
  /**
   * The one question a surface is currently asking.
   *
   * Exempt from expiry. The engine sends a frame only when the generation
   * moves, so silence on a watched question means the table did not change —
   * that is what the generation is for — and blanking a stable facepile every
   * fifteen seconds would turn the cheapest possible answer into a flicker.
   */
  private asked: ResourceKey | null = null;

  constructor(
    private readonly store: WorldViewStore,
    private readonly clock: () => number = () => Date.now(),
  ) {}

  /** Record what this tab wants answered. `null` is asking nothing. */
  ask(question: Question): void {
    this.asked = question ? liveKey(question.space, question.issue ?? null) : null;
  }

  /** Fold one pushed answer into its slot. */
  admit(space: string, issue: string | null, reply: Response): void {
    const key = liveKey(space, issue);
    // The transient lane is one broadcast for the whole server, so every socket
    // sees the answer to every other socket's question. A frame this tab did
    // not ask for is somebody else's answer, and taking it would put a slot in
    // the store for every issue anybody anywhere opens — bounded only by a
    // sweep that runs while a surface is mounted.
    if (key !== this.asked) return;
    const previous = this.store.read<LiveState>(key).data ?? emptyLive;
    const view = applyLive(previous, reply);
    if (view === previous) return;
    this.heard.set(key, this.clock());
    this.store.set(key, view);
  }

  /**
   * Fold one drain into what this tab holds for that space.
   *
   * Not gated on a declaration, unlike a live view. The server drains on this
   * tab's behalf and the drain is destructive: a signal turned away here is
   * gone from the daemon's queue too, so there is nowhere for it to arrive
   * later and nobody else to take it.
   */
  deliver(space: string, reply: Response): void {
    const key = signalsKey(space);
    const held = this.store.read<SignalDrain>(key).data ?? noSignals;
    const next = applySignals(held, reply);
    if (next === held) return;
    this.store.set(key, next);
  }

  /** Forget what has been acted on. */
  forget(space: string): void {
    this.store.set(signalsKey(space), noSignals);
  }

  /**
   * The socket stopped answering.
   *
   * Every table goes to `unavailable` rather than staying as it was, and leaves
   * the index so the sweep cannot later overwrite that with a plain empty table
   * — "the daemon is not talking to me" and "nobody is here" look identical on
   * screen and are not the same thing to anyone deciding whether to speak.
   */
  silence(): void {
    for (const key of this.heard.keys()) this.store.set(key, liveUnavailable());
    this.heard.clear();
  }

  /**
   * Expire what stopped being answered, then bound what is left.
   *
   * Blanked rather than deleted: a surface that remounts on an expired key reads
   * an empty table and fills it on the next frame, where a deleted key would
   * make it read `cold` and have nothing to say about the difference.
   */
  sweep(): void {
    const now = this.clock();
    const preserve = new Set<ResourceKey>();
    if (this.asked !== null) preserve.add(this.asked);
    for (const [key, at] of [...this.heard]) {
      if (preserve.has(key)) continue;
      if (now - at < liveSilenceMs) continue;
      this.heard.delete(key);
      this.store.set(key, emptyLive);
    }
    this.store.evict(liveKeyPrefix, maxLiveSlots, preserve);
  }
}

/** How a plane gets its socket. Injected so the slot store can be exercised
 *  without one. */
export type SocketOpener = (onEvent: (event: SocketEvent) => void) => Socket;

/**
 * The socket, and the slots it feeds.
 *
 * One socket per tab, opened when something first names a space or asks a
 * question and kept for the tab's life: it also carries the control lane, which
 * the engine may not drop, so closing it because a facepile went off screen
 * would hand back the guarantee that lane exists for. The declaration is what
 * changes — the engine holds one per socket and the last one wins, which is
 * exactly right for a browser drawing one issue at a time.
 *
 * The space and the question are held apart because they fail apart. The
 * question is what the engine subscribes to a live view for; the space is what it
 * drains signals for, and a tab that closed a detail pane has stopped asking
 * who is on an issue without leaving the room it asked from.
 */
export class LivePlane {
  readonly slots: LiveSlots;
  private socket: Socket | null = null;
  /**
   * The space this tab is in.
   *
   * Held apart from the question and never cleared, only replaced. It is what
   * the engine drains signals for, and the drain is the reason this socket
   * stays open — so a detail pane closing must not take it with it. The viewer
   * draws one space at a time, so the last one named is the one it is in.
   */
  private attached: string | null = null;
  private question: Question = null;
  private awareness: EditorAwareness = { cursor: null, typing: false, preview: null };
  private awarenessTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    store: WorldViewStore,
    private readonly open: SocketOpener = openSocket,
    clock: () => number = () => Date.now(),
  ) {
    this.slots = new LiveSlots(store, clock);
  }

  /** Say which space this tab is looking at. */
  attach(space: string): void {
    if (this.attached === space) return;
    this.attached = space;
    this.declare();
  }

  /** Ask for one issue's live table. `null` stops asking without leaving. */
  ask(question: Question): void {
    const changed = this.question?.space !== question?.space || this.question?.issue !== question?.issue;
    this.question = question;
    if (changed) {
      this.awareness = { cursor: null, typing: false, preview: null };
      if (this.awarenessTimer !== null) clearTimeout(this.awarenessTimer);
      this.awarenessTimer = null;
    }
    this.declare();
  }

  /** Publish editor-local state on the same standing declaration. Cursor motion
   * is coalesced; blur/retirement is immediate so a departed caret does not
   * linger for a network round. */
  aware(issue: string, awareness: EditorAwareness): void {
    if (!this.question || this.question.issue !== issue) return;
    const nextPreview = awareness.preview ?? null;
    const previewChanged = !samePreview(this.awareness.preview ?? null, nextPreview);
    if (awareness.defer) {
      // A transaction-driven selection update can beat the splice RPC that
      // makes its scalar offset meaningful. Cancel any unsent offset, but keep
      // the last declaration: its CRDT anchor remains valid while text moves.
      if (this.awarenessTimer !== null) clearTimeout(this.awarenessTimer);
      this.awarenessTimer = null;
      if (previewChanged) {
        this.awareness = { ...this.awareness, preview: nextPreview };
        this.declare();
      }
      return;
    }
    this.awareness = {
      cursor: awareness.cursor,
      typing: awareness.typing,
      preview: nextPreview,
    };
    // Preview traffic is already cumulative/coalesced by revision. Send it to
    // the standing socket now; another 80 ms cursor window would be pure lag.
    if (previewChanged) {
      if (this.awarenessTimer !== null) clearTimeout(this.awarenessTimer);
      this.awarenessTimer = null;
      this.declare();
      return;
    }
    if (awareness.cursor === null && !awareness.typing) {
      if (this.awarenessTimer !== null) clearTimeout(this.awarenessTimer);
      this.awarenessTimer = null;
      this.declare();
      return;
    }
    // A throttle with a latest-value slot, not a debounce. Continuous typing
    // must still emit motion every window; restarting the timer for every input
    // would keep a fast typist invisible until they stopped.
    if (this.awarenessTimer !== null) return;
    this.awarenessTimer = setTimeout(() => {
      this.awarenessTimer = null;
      this.declare();
    }, cursorPublishMs);
  }

  /** Use the ordered control lane for editor durability as well as presence. */
  mutate<R extends Response = Response>(space: string, request: WorldRequest): Promise<R> {
    this.attach(space);
    this.declare();
    if (this.socket === null) return Promise.reject(new Error("the editor connection is unavailable"));
    return this.socket.mutate<R>(space, request);
  }

  private declare(): void {
    const declared = this.question
      ? {
          ...this.question,
          ...(this.awareness.cursor ? { cursor: this.awareness.cursor } : {}),
          ...(this.awareness.typing ? { typing: true } : {}),
          ...(this.awareness.preview ? { preview: this.awareness.preview } : {}),
        }
      : (this.attached === null ? null : { space: this.attached });
    this.slots.ask(this.question);
    // A tab that has named nothing at all does not open a socket to say so.
    if (this.socket === null && declared === null) return;
    this.socket ??= this.open((event) => this.receive(event));
    this.socket.watch(declared);
  }

  private receive(event: SocketEvent): void {
    if (event.kind === "live") {
      this.slots.admit(event.space, event.issue, event.view);
      return;
    }
    // Held, not merely decoded. The engine drained these out of the daemon's
    // queue on this tab's behalf and kept no copy, so a branch that fell
    // through here would be the whole Control lane delivering into nothing.
    if (event.kind === "signals") {
      this.slots.deliver(event.space, event.drained);
      return;
    }
    if (event.kind !== "liveness") return;
    // `connecting` is not a loss — it is the first thing every socket says,
    // including the one that is about to work.
    if (event.liveness === "retrying" || event.liveness === "stale") this.slots.silence();
  }
}

function samePreview(a: BrowserTextPreview | null, b: BrowserTextPreview | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return a.field === b.field
    && a.base === b.base
    && a.result === b.result
    && a.index === b.index
    && a.delete === b.delete
    && a.insert === b.insert
    && a.anchor === b.anchor
    && a.focus === b.focus;
}

const planes = new WeakMap<WorldViewStore, LivePlane>();

function planeFor(store: WorldViewStore): LivePlane {
  let plane = planes.get(store);
  if (!plane) {
    plane = new LivePlane(store);
    planes.set(store, plane);
  }
  return plane;
}

/**
 * The live table for one issue, kept current.
 *
 * `issue` is an `iss_` **doc id**, never a project alias. It narrows
 * daemon-side, and the Body id it narrows by is derived from the string as
 * given: `ENG-12` hashes to a Body nothing publishes under, which is an empty
 * table for ever rather than an error anybody sees. The narrowing itself is
 * unavoidable — an unscoped `live` returns Body ids from every hosted World,
 * and the derivation runs one way, so those rows name nothing a browser can
 * display.
 *
 * `null` asks nothing and still attaches the space, which is what keeps the
 * Control lane running for a tab with no issue open.
 */
export function useLiveTable(space: string, issue: string | null): LiveState {
  const store = useWorldViewStore();
  const plane = useMemo(() => planeFor(store), [store]);
  const slot = useWorldResource<LiveState>(liveKey(space, issue));

  useEffect(() => {
    plane.attach(space);
    plane.ask(issue === null ? null : { space, issue });
    // The question goes; the space does not. A closed detail pane is a person
    // still in the room, and the signals delivered to that room are not this
    // pane's to hang up on.
    return () => plane.ask(null);
  }, [plane, space, issue]);

  // Deps are `[plane]`, which is stable for the life of the store. Keying a
  // sweep on anything the traffic moves — a table, a generation, an entry count
  // — tears the interval down and rebuilds it on every frame that arrives, so
  // under steady presence it never reaches its own delay and never fires. The
  // prediction sweep in `App.tsx` carries the same warning for the same reason.
  useEffect(() => {
    const timer = window.setInterval(() => plane.slots.sweep(), liveSweepMs);
    return () => window.clearInterval(timer);
  }, [plane]);

  return slot.data ?? emptyLive;
}

/** The write half of the issue's Live declaration. It deliberately shares the
 * plane used by `useLiveTable`: reading and publishing two independent room
 * declarations would let the last socket frame erase the first. */
export function useLiveAwareness(issue: string): (awareness: EditorAwareness) => void {
  const store = useWorldViewStore();
  const plane = useMemo(() => planeFor(store), [store]);
  useEffect(
    () => () => plane.aware(issue, { cursor: null, typing: false, preview: null }),
    [plane, issue],
  );
  return useCallback((awareness) => plane.aware(issue, awareness), [plane, issue]);
}

/** Ordered editor RPCs over the same native socket carrying Live updates. */
export function useLiveMutation(): <R extends Response = Response>(
  space: string,
  request: WorldRequest,
) => Promise<R> {
  const store = useWorldViewStore();
  const plane = useMemo(() => planeFor(store), [store]);
  return useCallback(
    <R extends Response = Response>(space: string, request: WorldRequest) =>
      plane.mutate<R>(space, request),
    [plane],
  );
}

/** What one space has delivered, and how to say it has been dealt with. */
export interface SignalInbox {
  drain: SignalDrain;
  /** Drop what is held. A signal is acted on once, and a list that kept it
   *  would offer the same invitation again on the next render. */
  acknowledge: () => void;
}

/**
 * The signals delivered for one space.
 *
 * Reading this is also what declares the space, so a surface that draws no
 * facepile still keeps the Control lane running: the server drains a space only
 * while some tab has named it, the daemon's queue is bounded and overwrites its
 * oldest, and an invitation lost because nobody had an issue open is exactly
 * the outcome that lane exists to rule out.
 */
export function useSignalInbox(space: string): SignalInbox {
  const store = useWorldViewStore();
  const plane = useMemo(() => planeFor(store), [store]);
  const slot = useWorldResource<SignalDrain>(signalsKey(space));

  useEffect(() => plane.attach(space), [plane, space]);
  const acknowledge = useCallback(() => plane.slots.forget(space), [plane, space]);

  return { drain: slot.data ?? noSignals, acknowledge };
}
