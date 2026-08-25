/**
 * Space-wide kind configuration, and the draft that lets a panel show its
 * effect before it is committed.
 *
 * The draft is the whole reason typing in a panel feels instant: it costs no
 * round trip, and it never mutates the committed model. It lives *beside* the
 * exact view and is reconciled by the next `refresh()` — which is the shape
 * `docs/ARCHITECTURE.md` calls Constant-Time Feedback Continuity, and the
 * reason this is a draft rather than an optimistic write.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { deleteConfig, fetchConfigs, putConfig } from "@/utils/apps/api";
import type { SignageConfig, SignageMedia } from "@/utils/lait/types";
import { overlaySettings } from "../kinds/registry";
import type { Settings } from "../kinds/types";

export type KindDraft = { kind: string; settings: Settings } | null;

export type KindConfigs = {
  configs: SignageConfig[];
  draft: KindDraft;
  loading: boolean;
  error: string | null;
  configFor: (kind: string) => SignageConfig | null;
  /** What a kind's settings are *right now*, draft included. */
  settingsFor: (kind: string) => Settings | undefined;
  setDraft: (draft: KindDraft) => void;
  /** Resolve every kind row in a library against live config and draft. */
  resolve: (library: SignageMedia[]) => SignageMedia[];
  refresh: () => Promise<SignageConfig[]>;
  save: (kind: string, name: string, settings: Settings) => Promise<void>;
  remove: (id: string) => Promise<void>;
};

export function useKindConfigs(): KindConfigs {
  const [configs, setConfigs] = useState<SignageConfig[]>([]);
  const [draft, setDraft] = useState<KindDraft>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const latest = await fetchConfigs();
      setConfigs(latest);
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

  const configFor = useCallback(
    (kind: string) => configs.find((entry) => entry.kind === kind) ?? null,
    [configs],
  );

  const settingsFor = useCallback(
    (kind: string): Settings | undefined => {
      if (draft && draft.kind === kind) return draft.settings;
      return configFor(kind)?.settings;
    },
    [draft, configFor],
  );

  const resolve = useCallback(
    (library: SignageMedia[]): SignageMedia[] =>
      library.map((entry) => {
        if (entry.source !== "kind") return entry;
        const live = settingsFor(entry.kind);
        if (!live) return entry;
        return { ...entry, settings: overlaySettings(live, entry.settings) };
      }),
    [settingsFor],
  );

  const save = useCallback(
    async (kind: string, name: string, settings: Settings) => {
      await putConfig(kind, name, settings);
      await refresh();
      setDraft(null);
    },
    [refresh],
  );

  const remove = useCallback(
    async (id: string) => {
      await deleteConfig(id);
      await refresh();
      setDraft(null);
    },
    [refresh],
  );

  return useMemo(
    () => ({
      configs,
      draft,
      loading,
      error,
      configFor,
      settingsFor,
      setDraft,
      resolve,
      refresh,
      save,
      remove,
    }),
    [configs, draft, loading, error, configFor, settingsFor, resolve, refresh, save, remove],
  );
}
