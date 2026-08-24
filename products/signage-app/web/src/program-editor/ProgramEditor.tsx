import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AlertDialog } from "@base-ui/react/alert-dialog";
import {
  ArrowLeft,
  Clock,
  Copy,
  Pause,
  Play,
  Plus,
  Redo2,
  SkipBack,
  SkipForward,
  Trash2,
  Undo2,
} from "lucide-react";
import { useAdminLayout } from "@/context/AdminLayoutContext";
import { useToast } from "@/ds";
import { space } from "@/utils/api/client";
import { fetchConfigs, type KindDefinition } from "@/utils/apps/api";
import { saveMedia } from "@/utils/content/api";
import { mintBodyId } from "@/utils/lait/ids";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";
import { useClock } from "./clock";
import { Filmstrip } from "./Filmstrip";
import type { ClipActions } from "./ItemMenu";
import { LibrarySheet } from "./LibrarySheet";
import {
  addMedia,
  copyItem,
  duplicateItem,
  formatClock,
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
  type TrimPreview,
} from "./model";
import { haptic } from "./haptic";
import { suppressCoarseContextMenu } from "./pointer";
import { Stage } from "./Stage";
import "./program-editor.css";

type Props = {
  initial: SignageProgram;
  library: SignageMedia[];
  persisted: boolean;
  onSave: (program: SignageProgram) => Promise<void>;
  onClose: () => void;
  onRefreshLibrary: () => Promise<SignageMedia[]>;
};

export function ProgramEditor({
  initial,
  library: initialLibrary,
  persisted,
  onSave,
  onClose,
  onRefreshLibrary,
}: Props) {
  const peRef = useRef<HTMLDivElement>(null);
  const [program, setProgram] = useState(initial);
  const [baseline, setBaseline] = useState(initial);
  const [library, setLibrary] = useState(initialLibrary);
  const [selectedId, setSelectedId] = useState<string | null>(
    initial.items[0]?.id ?? null,
  );
  const [orbit, setOrbit] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [leaveOpen, setLeaveOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [clipboard, setClipboard] = useState<ClipCopy | null>(null);
  const [past, setPast] = useState<SignageProgram[]>([]);
  const [future, setFuture] = useState<SignageProgram[]>([]);
  const [trim, setTrim] = useState<TrimPreview | null>(null);
  const addAfterRef = useRef<string | null>(null);
  const wide = useWide();
  useVisualViewportBottom();
  const { setHideSidebar } = useAdminLayout();
  const toast = useToast();

  const catalog = useMemo(() => mediaById(library), [library]);
  const durationMs = programDurationMs(program, catalog);
  const clips = useMemo(() => layout(program, catalog), [program, catalog]);
  const { t, playing, seek, toggle, pause } = useClock(durationMs, program.cycle);
  const current = itemAtTime(clips, t);
  const selected =
    clips.find((clip) => clip.item.id === selectedId) ?? current;
  const stageClip =
    (trim && clips.find((clip) => clip.item.id === trim.id)) || current;
  const dirty = !sameProgram(program, baseline);
  const canSave = dirty && program.items.length > 0;
  const canUndo = past.length > 0;
  const canRedo = future.length > 0;

  const apply = (next: SignageProgram) => {
    setProgram((currentProgram) => {
      if (sameProgram(currentProgram, next)) return currentProgram;
      setPast((stack) => [...stack, currentProgram].slice(-40));
      setFuture([]);
      return next;
    });
  };

  useEffect(() => {
    setHideSidebar(true);
    return () => setHideSidebar(false);
  }, [setHideSidebar]);

  useEffect(() => {
    const root = peRef.current;
    if (!root) return;
    root.addEventListener("contextmenu", suppressCoarseContextMenu, true);
    return () => {
      root.removeEventListener("contextmenu", suppressCoarseContextMenu, true);
    };
  }, []);

  useEffect(() => {
    void space()
      .then(setOrbit)
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, []);

  useEffect(() => {
    setLibrary(initialLibrary);
  }, [initialLibrary]);

  useEffect(() => {
    if (selectedId && program.items.some((item) => item.id === selectedId)) return;
    setSelectedId(program.items[0]?.id ?? null);
  }, [program.items, selectedId]);

  const save = async (): Promise<boolean> => {
    if (!canSave) return false;
    setSaving(true);
    setError(null);
    try {
      await onSave(program);
      setBaseline(program);
      haptic("save");
      toast.show("Saved", "The World has this program.");
      return true;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
      haptic("error");
      toast.show("Could not save", message);
      return false;
    } finally {
      setSaving(false);
    }
  };

  const back = () => {
    if (dirty) setLeaveOpen(true);
    else onClose();
  };

  const add = (media: SignageMedia) => {
    pause();
    const known = new Set(program.items.map((item) => item.id));
    const next = addMedia(program, media, addAfterRef.current);
    apply(next);
    haptic("select");
    const added = next.items.find((item) => !known.has(item.id));
    setSelectedId(added?.id ?? null);
    setAddOpen(false);
  };

  const pasteAt = (afterId: string | null) => {
    if (!clipboard) return;
    pause();
    const known = new Set(program.items.map((item) => item.id));
    const next = pasteItem(program, clipboard, afterId);
    apply(next);
    const added = next.items.find((item) => !known.has(item.id));
    setSelectedId(added?.id ?? null);
  };

  const openAdd = (open: boolean, after: string | null = selectedId) => {
    if (open) {
      addAfterRef.current = after;
      void onRefreshLibrary().then(setLibrary);
    }
    setAddOpen(open);
  };

  const undo = () => {
    if (past.length === 0) return;
    const prev = past[past.length - 1];
    setFuture((ahead) => [...ahead, program]);
    setPast((stack) => stack.slice(0, -1));
    setProgram(prev);
  };

  const redo = () => {
    if (future.length === 0) return;
    const next = future[future.length - 1];
    setPast((stack) => [...stack, program]);
    setFuture((ahead) => ahead.slice(0, -1));
    setProgram(next);
  };

  const undoRef = useRef(undo);
  const redoRef = useRef(redo);
  undoRef.current = undo;
  redoRef.current = redo;

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const meta = event.metaKey || event.ctrlKey;
      if (!meta) return;
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
        return;
      }
      if (event.key === "z" && !event.shiftKey) {
        event.preventDefault();
        undoRef.current();
      } else if ((event.key === "z" && event.shiftKey) || event.key === "y") {
        event.preventDefault();
        redoRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const addKind = async (kind: KindDefinition) => {
    try {
      const configs = await fetchConfigs();
      const config = configs.find((entry) => entry.kind === kind.kind);
      const media: SignageMedia = {
        id: mintBodyId(),
        name: kind.label,
        source: "kind",
        kind: kind.kind,
        settings: config?.settings ?? {},
        duration_ms: 60_000,
        width: null,
        height: null,
        catalog: null,
      };
      await saveMedia(media);
      setLibrary((current) => [media, ...current]);
      add(media);
    } catch (err) {
      toast.show(
        "Could not add the app",
        err instanceof Error ? err.message : String(err),
      );
    }
  };

  const actions: ClipActions = {
    clipboard,
    duplicate: (id) => {
      pause();
      const next = duplicateItem(program, id);
      apply(next);
      haptic("select");
      const index = program.items.findIndex((item) => item.id === id);
      setSelectedId(next.items[index + 1]?.id ?? id);
    },
    copy: (id) => {
      const copied = copyItem(program, id);
      if (copied) setClipboard(copied);
    },
    pasteAfter: pasteAt,
    remove: (id) => {
      pause();
      haptic("delete");
      apply(removeItem(program, id));
    },
    add: () => openAdd(true, selectedId),
  };

  return createPortal(
    <div className={`pe${wide ? "" : " is-narrow"}`} ref={peRef}>
      <header className="pe-bar">
        <button type="button" className="ds-icon" onClick={back} aria-label="Back">
          <ArrowLeft size={20} />
        </button>
        <button
          type="button"
          className="ds-icon"
          disabled={!canUndo}
          onClick={undo}
          aria-label="Undo"
          title="Undo"
        >
          <Undo2 size={18} />
        </button>
        <button
          type="button"
          className="ds-icon"
          disabled={!canRedo}
          onClick={redo}
          aria-label="Redo"
          title="Redo"
        >
          <Redo2 size={18} />
        </button>
        <input
          className="pe-name"
          value={program.name}
          aria-label="Program name"
          onChange={(event) => setProgram(rename(program, event.target.value))}
        />
        {dirty ? (
          <button
            type="button"
            className="ds-btn ds-btn-ghost pe-discard"
            disabled={saving}
            onClick={() => apply(baseline)}
          >
            Discard
          </button>
        ) : null}
        {canSave ? (
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            disabled={saving}
            onClick={() => void save()}
          >
            {saving ? "Saving" : "Save"}
          </button>
        ) : (
          <span className="ds-hint">
            {saving ? "Saving" : persisted && !dirty ? "Saved" : ""}
          </span>
        )}
      </header>
      {program.items.length === 0 ? (
        <p className="ds-hint pe-hint">A program needs at least one item before it can be saved.</p>
      ) : null}
      {error ? <p className="ds-danger-text pe-error">{error}</p> : null}

      <Stage
        clip={stageClip}
        t={t}
        playing={playing}
        orbit={orbit}
        trim={trim}
        container={peRef}
        actions={actions}
      />

      <div className="pe-dock">
        <div className={`pe-rail${wide ? "" : " is-narrow"}`}>
          <div className="pe-rail-time">
            {formatClock(t)}
            <span> / {formatClock(durationMs)}</span>
          </div>
          <div className="pe-rail-play">
            <button
              type="button"
              className="pe-skip"
              disabled={durationMs <= 0}
              onClick={() => {
                pause();
                seek(0);
              }}
              title="Skip to start"
              aria-label="Skip to start"
            >
              <SkipBack size={18} fill="currentColor" />
            </button>
            <button
              type="button"
              className="pe-play"
              disabled={durationMs <= 0}
              onClick={toggle}
              title={playing ? "Pause" : "Play"}
              aria-label={playing ? "Pause" : "Play"}
            >
              {playing ? (
                <Pause size={26} fill="currentColor" />
              ) : (
                <Play size={26} fill="currentColor" />
              )}
            </button>
            <button
              type="button"
              className="pe-skip"
              disabled={durationMs <= 0}
              onClick={() => {
                pause();
                seek(durationMs);
              }}
              title="Skip to end"
              aria-label="Skip to end"
            >
              <SkipForward size={18} fill="currentColor" />
            </button>
          </div>
          <div className="pe-rail-clip">
            {selected ? (
              <>
                <span className="pe-rail-name">
                  {selected.media?.name ?? selected.item.media}
                </span>
                <DurationChip
                  ms={selected.durationMs}
                  onCommit={(ms) =>
                    apply(setDuration(program, selected.item.id, ms))
                  }
                />
                <button
                  type="button"
                  className="pe-tool"
                  title="Add after this clip"
                  aria-label="Add after this clip"
                  onClick={() => openAdd(true, selected.item.id)}
                >
                  <Plus size={18} />
                </button>
                <button
                  type="button"
                  className="pe-tool"
                  title="Duplicate"
                  aria-label="Duplicate"
                  onClick={() => actions.duplicate(selected.item.id)}
                >
                  <Copy size={18} />
                </button>
                <button
                  type="button"
                  className="pe-remove"
                  title="Remove from program"
                  aria-label="Remove from program"
                  onClick={() => actions.remove(selected.item.id)}
                >
                  <Trash2 size={18} />
                </button>
              </>
            ) : null}
          </div>
        </div>

        <Filmstrip
        program={program}
        library={catalog}
        media={library}
        t={t}
        selectedId={selectedId}
        orbit={orbit}
        addOpen={addOpen}
        wide={wide}
        container={peRef}
        actions={actions}
        onSelect={setSelectedId}
        onSeek={(ms) => {
          pause();
          seek(ms);
        }}
        onMove={(from, to) => apply(moveItem(program, from, to))}
        onTrim={(id, ms) => apply(setDuration(program, id, ms))}
        onTrimLive={(preview) => {
          if (preview) pause();
          setTrim(preview);
        }}
        onAddOpenChange={(open) => openAdd(open, null)}
        onAddMedia={add}
        onUploaded={(uploaded) => {
          setLibrary((current) => [...uploaded, ...current]);
          void onRefreshLibrary().then(setLibrary);
          if (uploaded.length > 0) {
            toast.show(
              "Uploaded",
              uploaded.length === 1
                ? uploaded[0].name
                : `${uploaded.length} items are in the library.`,
            );
          }
        }}
        onAddKind={(kind) => void addKind(kind)}
        onUploadError={(message) => toast.show("Upload refused", message)}
      />
      </div>

      {!wide ? (
        <LibrarySheet
          open={addOpen}
          onOpenChange={openAdd}
          library={library}
          orbit={orbit}
          onAdd={add}
          onUploaded={(uploaded) => {
            setLibrary((currentLibrary) => [...uploaded, ...currentLibrary]);
            void onRefreshLibrary().then(setLibrary);
          }}
          onAddKind={(kind) => void addKind(kind)}
          onUploadError={(message) => toast.show("Upload refused", message)}
          container={peRef}
        />
      ) : null}

      <AlertDialog.Root open={leaveOpen} onOpenChange={setLeaveOpen}>
        <AlertDialog.Portal container={peRef}>
          <AlertDialog.Backdrop className="ds-backdrop" />
          <AlertDialog.Popup className="ds-dialog ds-leave">
            <AlertDialog.Title>Save this program?</AlertDialog.Title>
            <AlertDialog.Description>
              The World does not have these edits yet.
            </AlertDialog.Description>
            <menu>
              <AlertDialog.Close className="ds-btn ds-btn-quiet">
                Keep editing
              </AlertDialog.Close>
              <button
                type="button"
                className="ds-btn ds-btn-ghost"
                onClick={() => {
                  setLeaveOpen(false);
                  onClose();
                }}
              >
                Don&apos;t save
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-solid"
                disabled={!canSave || saving}
                onClick={() => {
                  void save().then((ok) => {
                    if (ok) onClose();
                    else setLeaveOpen(false);
                  });
                }}
              >
                {saving ? "Saving" : "Save"}
              </button>
            </menu>
          </AlertDialog.Popup>
        </AlertDialog.Portal>
      </AlertDialog.Root>
    </div>,
    document.body,
  );
}

function DurationChip({
  ms,
  onCommit,
}: {
  ms: number;
  onCommit: (nextMs: number) => void;
}) {
  const seconds = Math.max(1, Math.round(ms / 1000));
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(String(seconds));

  useEffect(() => {
    if (!editing) setDraft(String(seconds));
  }, [seconds, editing]);

  const commit = () => {
    const parsed = Number.parseInt(draft, 10);
    if (Number.isFinite(parsed) && parsed > 0) onCommit(parsed * 1000);
    else setDraft(String(seconds));
    setEditing(false);
  };

  return (
    <div className="pe-duration">
      <Clock size={16} />
      {editing ? (
        <input
          type="text"
          inputMode="numeric"
          pattern="[0-9]*"
          enterKeyHint="done"
          value={draft}
          autoFocus
          aria-label="Duration in seconds"
          onChange={(event) =>
            setDraft(event.target.value.replace(/[^\d]/g, "").slice(0, 4))
          }
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter") commit();
            if (event.key === "Escape") {
              setDraft(String(seconds));
              setEditing(false);
            }
          }}
        />
      ) : (
        <button
          type="button"
          title="Duration"
          aria-label={`Duration ${seconds} seconds`}
          onClick={() => setEditing(true)}
        >
          {seconds}s
        </button>
      )}
    </div>
  );
}

function useVisualViewportBottom() {
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const sync = () => {
      const inset = Math.max(0, window.innerHeight - vv.height - vv.offsetTop);
      document.documentElement.style.setProperty("--vv-keyboard", `${inset}px`);
    };
    sync();
    vv.addEventListener("resize", sync);
    vv.addEventListener("scroll", sync);
    return () => {
      vv.removeEventListener("resize", sync);
      vv.removeEventListener("scroll", sync);
      document.documentElement.style.removeProperty("--vv-keyboard");
    };
  }, []);
}

function useWide(): boolean {
  const [wide, setWide] = useState(
    () => typeof window !== "undefined" && window.matchMedia("(min-width: 720px)").matches,
  );
  useEffect(() => {
    const media = window.matchMedia("(min-width: 720px)");
    const onChange = () => setWide(media.matches);
    onChange();
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);
  return wide;
}
