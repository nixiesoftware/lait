/**
 * A kind's presentation, being edited.
 *
 * Two things happen on every keystroke and they are deliberately different
 * speeds. The **draft** goes to the session immediately, so the card on the
 * stage redraws under the cursor — that is the instant half, and it costs no
 * round trip. The **write** follows on a debounce, because a preset is shared
 * and committing sixty times while somebody types a city name is sixty
 * replications for one decision.
 *
 * There is no Save button. A clip always points at a preset by the time this
 * panel can be opened — `addKind` reuses one or creates one — so every edit
 * here is an update to something that already exists, which is what makes
 * commit-on-change safe rather than a way to litter a Space with half-named
 * presets.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CommitState } from "@/ds";
import type { KindPanel, Settings } from "../kinds/types";
import { useEditorSession } from "./EditorContext";

/** Key order is not meaning, so it is not compared. */
function sameSettings(left: Settings, right: Settings): boolean {
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  return keys.every((key) => left[key] === right[key]);
}

const DEBOUNCE_MS = 500;

export function useKindDraft(panel: KindPanel, presetId: string | null) {
  const { kinds } = useEditorSession();
  const preset = kinds.byId(presetId);
  const presetKey = preset?.id ?? null;

  const [draft, setDraft] = useState<Settings>(() => panel.seed(preset));
  const [name, setName] = useState(() => preset?.name ?? panel.label);
  const [state, setState] = useState<CommitState>("settled");
  const [failure, setFailure] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState(false);
  const seeded = useRef(presetKey);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inflight = useRef(0);

  // Reseed only when the underlying document changes identity — not on every
  // refresh, which would throw away what somebody is typing.
  useEffect(() => {
    if (seeded.current === presetKey) return;
    seeded.current = presetKey;
    setDraft(panel.seed(preset));
    setName(preset?.name ?? panel.label);
    setState("settled");
    setFailure(null);
    setSubmitted(false);
  }, [preset, presetKey, panel]);

  const packed = useMemo(
    () => panel.pack(draft, preset?.settings ?? {}),
    [panel, draft, preset?.settings],
  );

  // The instant half: the stage redraws from this, every keystroke.
  const { setDraft: setSessionDraft } = kinds;
  useEffect(() => {
    if (!presetKey) return;
    setSessionDraft({ preset: presetKey, settings: packed });
  }, [setSessionDraft, presetKey, packed]);

  const errors = useMemo(() => panel.validate(draft), [panel, draft]);
  const errorFor = useCallback(
    (key: string) => {
      // Before a write has been attempted, a half-typed coordinate is not a
      // mistake — it is somebody mid-thought.
      if (!submitted) return null;
      return errors.find((entry) => entry.key === key)?.message ?? null;
    },
    [errors, submitted],
  );

  const write = useCallback(
    async (settings: Settings, label: string) => {
      if (!preset) return;
      const ticket = ++inflight.current;
      setSubmitted(true);
      if (errors.length > 0) {
        setState("refused");
        setFailure(errors[0]?.message ?? "refused");
        return;
      }
      setState("committing");
      setFailure(null);
      try {
        await kinds.save({ ...preset, name: label.trim() || panel.label, settings });
        if (ticket !== inflight.current) return;
        setState("settled");
      } catch (err) {
        if (ticket !== inflight.current) return;
        setState("refused");
        setFailure(err instanceof Error ? err.message : String(err));
      }
    },
    [errors, kinds, panel.label, preset],
  );

  const schedule = useCallback(
    (settings: Settings, label: string) => {
      setState("pending");
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => void write(settings, label), DEBOUNCE_MS);
    },
    [write],
  );

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const patch = useCallback(
    (next: Settings) => {
      setDraft((current) => {
        const merged = { ...current, ...next };
        schedule(panel.pack(merged, preset?.settings ?? {}), name);
        return merged;
      });
    },
    [name, panel, preset?.settings, schedule],
  );

  const rename = useCallback(
    (next: string) => {
      setName(next);
      schedule(packed, next);
    },
    [packed, schedule],
  );

  const remove = useCallback(async (): Promise<boolean> => {
    if (!preset) return false;
    setState("committing");
    try {
      await kinds.remove(preset.id);
      setState("settled");
      return true;
    } catch (err) {
      setState("refused");
      setFailure(err instanceof Error ? err.message : String(err));
      return false;
    }
  }, [kinds, preset]);

  return {
    preset,
    configured: preset != null,
    draft,
    packed,
    name,
    rename,
    patch,
    errors,
    errorFor,
    failure,
    state,
    /** Try the same value again after a refusal. */
    retry: () => void write(packed, name),
    remove,
    dirty: preset == null || !sameSettings(packed, preset.settings),
  };
}
