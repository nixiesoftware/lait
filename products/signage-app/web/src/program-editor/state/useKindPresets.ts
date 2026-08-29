/**
 * Kind presentation, and the draft that lets a panel show its effect before it
 * is committed.
 *
 * The draft is why typing in a panel feels instant: it costs no round trip, and
 * it never mutates the committed model. It lives *beside* the exact view and is
 * reconciled by the next `refresh()` — the shape `docs/ARCHITECTURE.md` calls
 * Constant-Time Feedback Continuity, and the reason this is a draft rather than
 * an optimistic write.
 *
 * A kind may have any number of presets. Which one an entry draws is a
 * reference on the entry, so this resolves *by id* — the old lookup by kind is
 * what made a second venue in one Space impossible to express.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  createPreset,
  deletePreset,
  fetchPresets,
  savePreset,
} from "@/utils/apps/api";
import type { SignageMedia, SignagePreset } from "@/utils/lait/types";
import type { Settings } from "../kinds/types";

/** An unsaved edit to one preset, shown on the stage while it is typed. */
export type PresetDraft = { preset: string; settings: Settings } | null;

export type KindPresets = {
  presets: SignagePreset[];
  draft: PresetDraft;
  loading: boolean;
  error: string | null;
  byId: (id: string | null | undefined) => SignagePreset | null;
  forKind: (kind: string) => SignagePreset[];
  /** What a preset's settings are *right now*, draft included. */
  settingsOf: (id: string | null | undefined) => Settings | undefined;
  setDraft: (draft: PresetDraft) => void;
  /** Resolve every kind row in a library against its preset and any draft. */
  resolve: (library: SignageMedia[]) => SignageMedia[];
  refresh: () => Promise<SignagePreset[]>;
  save: (preset: SignagePreset) => Promise<void>;
  create: (kind: string, name: string, settings: Settings) => Promise<SignagePreset>;
  remove: (id: string) => Promise<void>;
};

export function useKindPresets(): KindPresets {
  const [presets, setPresets] = useState<SignagePreset[]>([]);
  const [draft, setDraft] = useState<PresetDraft>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const latest = await fetchPresets();
      setPresets(latest);
      setError(null);
      return latest;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return [];
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const byId = useCallback(
    (id: string | null | undefined) =>
      id ? (presets.find((entry) => entry.id === id) ?? null) : null,
    [presets],
  );

  const forKind = useCallback(
    (kind: string) => presets.filter((entry) => entry.kind === kind),
    [presets],
  );

  const settingsOf = useCallback(
    (id: string | null | undefined): Settings | undefined => {
      if (!id) return undefined;
      if (draft && draft.preset === id) return draft.settings;
      return byId(id)?.settings;
    },
    [draft, byId],
  );

  /**
   * The editor's view of a kind entry: preset under the entry's own settings.
   *
   * Deliberately *not* the screen's facts or place — those arrive at render
   * time from wherever the clip is playing, and this editor is placeless. A
   * preview that invented a location would be showing something no screen will.
   */
  const resolve = useCallback(
    (library: SignageMedia[]): SignageMedia[] =>
      library.map((entry) => {
        if (entry.source !== "kind") return entry;
        const base = settingsOf(entry.preset);
        if (!base) return entry;
        return { ...entry, settings: { ...base, ...entry.settings } };
      }),
    [settingsOf],
  );

  const save = useCallback(
    async (preset: SignagePreset) => {
      await savePreset(preset);
      await refresh();
      setDraft(null);
    },
    [refresh],
  );

  const create = useCallback(
    async (kind: string, name: string, settings: Settings) => {
      const preset = await createPreset(kind, name, settings);
      await refresh();
      return preset;
    },
    [refresh],
  );

  const remove = useCallback(
    async (id: string) => {
      await deletePreset(id);
      await refresh();
      setDraft(null);
    },
    [refresh],
  );

  return useMemo(
    () => ({
      presets,
      draft,
      loading,
      error,
      byId,
      forKind,
      settingsOf,
      setDraft,
      resolve,
      refresh,
      save,
      create,
      remove,
    }),
    [
      presets,
      draft,
      loading,
      error,
      byId,
      forKind,
      settingsOf,
      resolve,
      refresh,
      save,
      create,
      remove,
    ],
  );
}
