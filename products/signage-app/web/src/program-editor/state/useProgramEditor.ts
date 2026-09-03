/**
 * The whole editor, minus the drawing.
 *
 * Both shells consume this and nothing else. It exists because the editor used
 * to be one component that branched on a `wide` boolean threaded four levels
 * deep — so a desktop change could only be made by reasoning about the phone at
 * the same time. Desktop and mobile are different products over the same state;
 * this is the state.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";
import { panelFor } from "../kinds/registry";
import { haptic } from "../haptic";
import {
  addMedia,
  copyItem,
  duplicateItem,
  itemAtTime,
  layout,
  mediaById,
  moveItem,
  pasteItem,
  programDurationMs,
  removeItem,
  rename,
  sameProgram,
  setDuration,
  type ClipCopy,
  type LaidClip,
  type TrimPreview,
} from "../model";
import { useTransport } from "./useTransport";

const UNDO_DEPTH = 40;

export type EditorOptions = {
  /** The program as it is on air. */
  initial: SignageProgram;
  library: SignageMedia[];
  /** Applies live kind config over the raw library rows. */
  resolve: (library: SignageMedia[]) => SignageMedia[];
  /** Write the draft — what the editor holds, not what a screen shows. */
  onDraft: (program: SignageProgram) => Promise<void>;
  /** Put what the editor holds on air. The one write a screen sees. */
  onAir: (program: SignageProgram) => Promise<void>;
  onRefreshLibrary: () => Promise<SignageMedia[]>;
};

/** The program with its draft applied: what the editor opens on. */
export function withDraftApplied(program: SignageProgram): SignageProgram {
  const draft = program.draft;
  if (!draft) return { ...program, draft: undefined };
  return {
    ...program,
    name: draft.name,
    cycle: draft.cycle,
    items: draft.items,
    windows: draft.windows,
    draft: undefined,
  };
}

export type Editor = ReturnType<typeof useProgramEditor>;

export function useProgramEditor({
  initial,
  library: initialLibrary,
  resolve,
  onDraft,
  onAir,
  onRefreshLibrary,
}: EditorOptions) {
  // `program` is what the editor holds; `onAir` is what a screen shows;
  // `draftBaseline` is what was last written as a draft.
  const opened = withDraftApplied(initial);
  const [program, setProgram] = useState(opened);
  const [onAirProgram, setOnAirProgram] = useState<SignageProgram>({ ...initial, draft: undefined });
  const [draftBaseline, setDraftBaseline] = useState(opened);
  const [airing, setAiring] = useState(false);
  const [rawLibrary, setRawLibrary] = useState(initialLibrary);
  const [selectedId, setSelectedId] = useState<string | null>(
    initial.items[0]?.id ?? null,
  );
  const [clipboard, setClipboard] = useState<ClipCopy | null>(null);
  const [past, setPast] = useState<SignageProgram[]>([]);
  const [future, setFuture] = useState<SignageProgram[]>([]);
  const [trim, setTrim] = useState<TrimPreview | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pendingSeek = useRef<string | null>(null);
  const commitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inflight = useRef(0);

  const library = useMemo(() => resolve(rawLibrary), [resolve, rawLibrary]);
  const catalog = useMemo(() => mediaById(library), [library]);
  const durationMs = programDurationMs(program, catalog);
  const clips = useMemo(() => layout(program, catalog), [program, catalog]);
  const transport = useTransport(durationMs, program.cycle);
  const { pause, seek } = transport;

  const current = itemAtTime(clips, transport.t);
  const selected = clips.find((clip) => clip.item.id === selectedId) ?? current;
  const stageClip = (trim && clips.find((clip) => clip.item.id === trim.id)) || current;

  /** Differs from what is on air: there is something to put on air. */
  const dirty = !sameProgram(program, onAirProgram);
  /** Differs from the last written draft: there is something to autosave. */
  const draftPending = !sameProgram(program, draftBaseline);
  const canAir = dirty && program.items.length > 0 && !airing;

  useEffect(() => {
    setRawLibrary(initialLibrary);
  }, [initialLibrary]);

  useEffect(() => {
    if (selectedId && program.items.some((item) => item.id === selectedId)) return;
    setSelectedId(program.items[0]?.id ?? null);
  }, [program.items, selectedId]);

  // A clip added from a panel wants the playhead on it, but that clip has no
  // layout until the render after the state change.
  useEffect(() => {
    const id = pendingSeek.current;
    if (!id) return;
    const clip = clips.find((entry) => entry.item.id === id);
    if (!clip) return;
    pause();
    seek(clip.startMs);
    pendingSeek.current = null;
  }, [clips, pause, seek]);

  const apply = useCallback((next: SignageProgram) => {
    setProgram((currentProgram) => {
      if (sameProgram(currentProgram, next)) return currentProgram;
      setPast((stack) => [...stack, currentProgram].slice(-UNDO_DEPTH));
      setFuture([]);
      return next;
    });
  }, []);

  const undo = useCallback(() => {
    setPast((stack) => {
      if (stack.length === 0) return stack;
      const prev = stack[stack.length - 1];
      setProgram((currentProgram) => {
        setFuture((ahead) => [...ahead, currentProgram]);
        return prev;
      });
      return stack.slice(0, -1);
    });
  }, []);

  const redo = useCallback(() => {
    setFuture((ahead) => {
      if (ahead.length === 0) return ahead;
      const next = ahead[ahead.length - 1];
      setProgram((currentProgram) => {
        setPast((stack) => [...stack, currentProgram]);
        return next;
      });
      return ahead.slice(0, -1);
    });
  }, []);

  const add = useCallback(
    (media: SignageMedia, afterId: string | null = null): string | null => {
      pause();
      const known = new Set(program.items.map((item) => item.id));
      const next = addMedia(program, media, afterId);
      apply(next);
      haptic("select");
      const added = next.items.find((item) => !known.has(item.id));
      setSelectedId(added?.id ?? null);
      if (added) pendingSeek.current = added.id;
      return added?.id ?? null;
    },
    [apply, pause, program],
  );

  const pasteAfter = useCallback(
    (afterId: string | null) => {
      if (!clipboard) return;
      pause();
      const known = new Set(program.items.map((item) => item.id));
      const next = pasteItem(program, clipboard, afterId);
      apply(next);
      const added = next.items.find((item) => !known.has(item.id));
      setSelectedId(added?.id ?? null);
    },
    [apply, clipboard, pause, program],
  );

  /**
   * Write the program as it stands — as the draft. Nothing a screen shows
   * changes. Kept as a promise because leaving the editor flushes it.
   */
  const saveDraft = useCallback(async (): Promise<boolean> => {
    if (program.items.length === 0) return false;
    const ticket = ++inflight.current;
    setSaving(true);
    setError(null);
    try {
      await onDraft(program);
      if (ticket !== inflight.current) return true;
      setDraftBaseline(program);
      return true;
    } catch (err) {
      if (ticket !== inflight.current) return false;
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
      haptic("error");
      return false;
    } finally {
      if (ticket === inflight.current) setSaving(false);
    }
  }, [onDraft, program]);

  /**
   * Put what the editor holds on air: the one write a screen sees. The
   * draft is cleared with it, so what is on air and what is held agree.
   */
  const putOnAir = useCallback(async (): Promise<boolean> => {
    if (program.items.length === 0 || airing) return false;
    const ticket = ++inflight.current;
    setAiring(true);
    setError(null);
    try {
      await onAir(program);
      if (ticket !== inflight.current) return true;
      setOnAirProgram(program);
      setDraftBaseline(program);
      return true;
    } catch (err) {
      if (ticket !== inflight.current) return false;
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
      haptic("error");
      return false;
    } finally {
      if (ticket === inflight.current) setAiring(false);
    }
  }, [airing, onAir, program]);

  /**
   * An edit writes itself — as the draft — debounced only enough that
   * dragging a clip is one write rather than sixty. What is on air does not
   * move until somebody puts the draft there: an edit half made is not a
   * broadcast.
   *
   * A program with no items is the one state that cannot be written — the
   * contract refuses it — so it is left alone rather than retried forever.
   */
  useEffect(() => {
    if (!draftPending || program.items.length === 0) return;
    if (commitTimer.current) clearTimeout(commitTimer.current);
    commitTimer.current = setTimeout(() => void saveDraft(), 600);
    return () => {
      if (commitTimer.current) clearTimeout(commitTimer.current);
    };
  }, [draftPending, program, saveDraft]);

  const refreshLibrary = useCallback(async () => {
    const latest = await onRefreshLibrary();
    setRawLibrary(latest);
    return latest;
  }, [onRefreshLibrary]);

  const selectAndSeek = useCallback(
    (clip: LaidClip) => {
      setSelectedId(clip.item.id);
      pause();
      seek(clip.startMs);
    },
    [pause, seek],
  );

  /** The panel that configures the selected clip, when it has one. */
  const selectedPanel =
    selected?.media?.source === "kind" ? panelFor(selected.media.kind) : null;

  return {
    program,
    /** What is on air, as this editor last knew it. */
    baseline: onAirProgram,
    library,
    catalog,
    clips,
    durationMs,
    selected,
    selectedId,
    selectedPanel,
    stageClip,
    current,
    transport,
    trim,
    clipboard,
    dirty,
    canUndo: past.length > 0,
    canRedo: future.length > 0,
    saving,
    error,
    setError,
    setTrim,
    setSelectedId,
    setRawLibrary,
    setClipboard,
    apply,
    undo,
    redo,
    add,
    pasteAfter,
    saveDraft,
    putOnAir,
    draftPending,
    canAir,
    airing,
    refreshLibrary,
    selectAndSeek,
    rename: (name: string) => apply(rename(program, name)),
    setDuration: (id: string, ms: number) => apply(setDuration(program, id, ms)),
    move: (from: number, to: number) => apply(moveItem(program, from, to)),
    remove: (id: string) => {
      pause();
      haptic("delete");
      apply(removeItem(program, id));
    },
    duplicate: (id: string) => {
      pause();
      const next = duplicateItem(program, id);
      apply(next);
      haptic("select");
      const index = program.items.findIndex((item) => item.id === id);
      setSelectedId(next.items[index + 1]?.id ?? id);
    },
    copy: (id: string) => {
      const copied = copyItem(program, id);
      if (copied) setClipboard(copied);
    },
    discard: () => apply(onAirProgram),
  };
}
