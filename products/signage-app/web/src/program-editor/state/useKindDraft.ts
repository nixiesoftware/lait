/**
 * A kind panel being edited.
 *
 * The draft is pushed to the session on every keystroke so the stage redraws
 * from it, and committed only when asked. Nothing here writes through to the
 * World until `commit()`, and nothing here rewrites the committed model — the
 * overlay sits beside it.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KindPanel, Settings } from "../kinds/types";
import { useEditorSession } from "./EditorContext";

/** Key order is not meaning, so it is not compared. */
function sameSettings(left: Settings, right: Settings): boolean {
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  return keys.every((key) => left[key] === right[key]);
}

export function useKindDraft(panel: KindPanel) {
  const { kinds } = useEditorSession();
  const config = kinds.configFor(panel.kind);
  const configId = config?.id ?? null;

  const [draft, setDraft] = useState<Settings>(() => panel.seed(config));
  const [name, setName] = useState(() => config?.name ?? panel.label);
  const [saving, setSaving] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const seeded = useRef(configId);

  // Reseed only when the underlying document changes identity — not on every
  // refresh, which would throw away what somebody is typing.
  useEffect(() => {
    if (seeded.current === configId) return;
    seeded.current = configId;
    setDraft(panel.seed(config));
    setName(config?.name ?? panel.label);
    setSubmitted(false);
    setFailure(null);
  }, [config, configId, panel]);

  const packed = useMemo(
    () => panel.pack(draft, config?.settings ?? {}),
    [panel, draft, config?.settings],
  );

  const { setDraft: setSessionDraft } = kinds;
  useEffect(() => {
    setSessionDraft({ kind: panel.kind, settings: packed });
  }, [setSessionDraft, panel.kind, packed]);

  const errors = useMemo(() => panel.validate(draft), [panel, draft]);
  const errorFor = useCallback(
    (key: string) => {
      if (!submitted) return null;
      return errors.find((entry) => entry.key === key)?.message ?? null;
    },
    [errors, submitted],
  );

  const patch = useCallback((next: Settings) => {
    setDraft((current) => ({ ...current, ...next }));
  }, []);

  const commit = useCallback(async (): Promise<boolean> => {
    setSubmitted(true);
    if (errors.length > 0) {
      setFailure(errors[0].message);
      return false;
    }
    setSaving(true);
    setFailure(null);
    try {
      await kinds.save(panel.kind, name.trim() || panel.label, packed);
      return true;
    } catch (err) {
      setFailure(err instanceof Error ? err.message : String(err));
      return false;
    } finally {
      setSaving(false);
    }
  }, [errors, kinds, name, packed, panel.kind, panel.label]);

  const remove = useCallback(async (): Promise<boolean> => {
    if (!config) return false;
    setSaving(true);
    try {
      await kinds.remove(config.id);
      return true;
    } catch (err) {
      setFailure(err instanceof Error ? err.message : String(err));
      return false;
    } finally {
      setSaving(false);
    }
  }, [config, kinds]);

  return {
    config,
    configured: config != null,
    draft,
    packed,
    name,
    setName,
    patch,
    errors,
    errorFor,
    failure,
    saving,
    commit,
    remove,
    dirty: config == null || !sameSettings(packed, config.settings),
  };
}
