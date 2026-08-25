/**
 * The mobile surface: stage, timeline, action bar, sheet.
 *
 * Vertical and single-column by construction — no rail, no inspector, and no
 * panel that covers the stage. The sheet rises over the timeline and never over
 * the preview, because the whole reason to configure from here is watching the
 * card change.
 */

import { useState } from "react";
import { ArrowLeft, Pause, Play } from "lucide-react";
import { useToast } from "@/ds";
import type { SignageMedia } from "@/utils/lait/types";
import { ActionBar } from "./ActionBar";
import { KindSheet, ProgramSheet } from "./ConfigSheet";
import { Filmstrip } from "../Filmstrip";
import { LibrarySheet } from "../LibrarySheet";
import { formatClock } from "../model";
import { Stage } from "../Stage";
import { useEditorSession } from "../state/EditorContext";
import type { ClipActions } from "../ItemMenu";

export function MobileShell({
  container,
  actions,
  onBack,
}: {
  container: React.RefObject<HTMLElement | null>;
  actions: ClipActions;
  onBack: () => void;
}) {
  const { editor, orbit, panel, addKind } = useEditorSession();
  const { transport } = editor;
  const toast = useToast();
  const [addOpen, setAddOpen] = useState(false);
  const [durationOpen, setDurationOpen] = useState(false);
  const sheetOpen = panel.sort !== "none";

  return (
    <>
      <header className="pe-bar is-mobile">
        <button type="button" className="ds-icon" onClick={onBack} aria-label="Back">
          <ArrowLeft size={20} />
        </button>
        <input
          className="pe-title"
          value={editor.program.name}
          aria-label="Program name"
          onChange={(event) => editor.rename(event.target.value)}
        />
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          disabled={!editor.canSave || editor.saving}
          onClick={() => void editor.save()}
        >
          {editor.saving ? "Saving" : editor.dirty ? "Save" : "Saved"}
        </button>
      </header>

      <div className={`pe-mobile${sheetOpen ? " has-sheet" : ""}`}>
        <Stage
          clip={editor.stageClip}
          t={transport.t}
          playing={transport.playing}
          orbit={orbit}
          trim={editor.trim}
          container={container}
          actions={actions}
        />

        <div className="pe-transport is-mobile">
          <button
            type="button"
            className="pe-play"
            disabled={editor.durationMs <= 0}
            onClick={transport.toggle}
            aria-label={transport.playing ? "Pause" : "Play"}
          >
            {transport.playing ? (
              <Pause size={20} fill="currentColor" />
            ) : (
              <Play size={20} fill="currentColor" />
            )}
          </button>
          <span className="pe-rail-time">
            {formatClock(transport.t)}
            <span> / {formatClock(editor.durationMs)}</span>
          </span>
        </div>

        <Filmstrip
          program={editor.program}
          library={editor.catalog}
          media={editor.library}
          t={transport.t}
          selectedId={editor.selectedId}
          orbit={orbit}
          addOpen={addOpen}
          wide={false}
          container={container}
          actions={actions}
          onSelect={editor.setSelectedId}
          onSeek={(ms) => {
            transport.pause();
            transport.seek(ms);
          }}
          onMove={editor.move}
          onTrim={editor.setDuration}
          onTrimLive={(preview) => {
            if (preview) transport.pause();
            editor.setTrim(preview);
          }}
          onAddOpenChange={setAddOpen}
          onAddMedia={(media) => editor.add(media, editor.selectedId)}
          onUploaded={(uploaded) => {
            editor.setRawLibrary((current: SignageMedia[]) => [...uploaded, ...current]);
            void editor.refreshLibrary();
          }}
          onAddKind={(kindPanel) => void addKind(kindPanel)}
          onUploadError={(message) => toast.show("Upload refused", message)}
        />

        <ActionBar
          onAdd={() => setAddOpen(true)}
          onDuration={() => setDurationOpen(true)}
        />

        {panel.sort === "kind" ? (
          <KindSheet
            panel={panel.panel}
            presetId={
              editor.selected?.media?.source === "kind"
                ? (editor.selected.media.preset ?? null)
                : null
            }
          />
        ) : null}
        {panel.sort === "program" ? <ProgramSheet /> : null}
      </div>

      <LibrarySheet
        open={addOpen}
        onOpenChange={setAddOpen}
        library={editor.library}
        orbit={orbit}
        onAdd={(media) => editor.add(media, editor.selectedId)}
        onUploaded={(uploaded) => {
          editor.setRawLibrary((current: SignageMedia[]) => [...uploaded, ...current]);
          void editor.refreshLibrary();
        }}
        onAddKind={(kindPanel) => void addKind(kindPanel)}
        onUploadError={(message) => toast.show("Upload refused", message)}
        container={container}
      />

      {durationOpen && editor.selected ? (
        <DurationSheet
          seconds={Math.max(1, Math.round(editor.selected.durationMs / 1000))}
          onCommit={(next) => {
            if (editor.selected) editor.setDuration(editor.selected.item.id, next * 1000);
            setDurationOpen(false);
          }}
          onClose={() => setDurationOpen(false)}
        />
      ) : null}
    </>
  );
}

function DurationSheet({
  seconds,
  onCommit,
  onClose,
}: {
  seconds: number;
  onCommit: (seconds: number) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState(String(seconds));
  return (
    <section className="pe-sheet is-short" aria-label="Clip length">
      <header className="pe-sheet-head">
        <span className="pe-sheet-grip" aria-hidden />
        <strong>Length</strong>
      </header>
      <div className="pe-sheet-body">
        <label className="pe-field is-mobile">
          <span className="pe-field-label">Seconds</span>
          <input
            className="ds-input"
            type="number"
            inputMode="numeric"
            min={1}
            autoFocus
            value={draft}
            onChange={(event) => setDraft(event.target.value.replace(/[^\d]/g, "").slice(0, 4))}
          />
        </label>
      </div>
      <footer className="pe-sheet-foot">
        <button type="button" className="ds-btn ds-btn-quiet" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          onClick={() => {
            const parsed = Number.parseInt(draft, 10);
            if (Number.isFinite(parsed) && parsed > 0) onCommit(parsed);
            else onClose();
          }}
        >
          Set
        </button>
      </footer>
    </section>
  );
}
