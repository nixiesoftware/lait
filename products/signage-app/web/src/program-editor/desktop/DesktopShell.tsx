/**
 * The desktop surface: sources on the left, the work in the middle, properties
 * on the right.
 *
 * Reads left to right as add → arrange → configure, which is the arrangement
 * Webflow, Framer and Canva all converge on, and the one VEED uses for exactly
 * this job. The rail is a destination, not a popover: choosing a source does
 * not cover the timeline, so you can see where a clip is about to land.
 */

import { useRef, useState } from "react";
import {
  ArrowLeft,
  Images,
  Pause,
  Play,
  Plus,
  Redo2,
  SkipBack,
  SkipForward,
  Undo2,
  Upload,
  X,
} from "lucide-react";
import type { SignageMedia } from "@/utils/lait/types";
import { uploadContentAll } from "@/utils/content/api";
import { CommitMark, useToast } from "@/ds";
import { Filmstrip } from "../Filmstrip";
import { Inspector } from "./Inspector";
import { KIND_PANELS } from "../kinds/registry";
import { LibraryPicker } from "../LibraryPicker";
import { formatClock } from "../model";
import { Stage } from "../Stage";
import { useEditorSession } from "../state/EditorContext";
import type { ClipActions } from "../ItemMenu";

export type SourcePage = "library" | "upload" | "apps" | null;

export function DesktopShell({
  container,
  source,
  onSource,
  actions,
  onBack,
}: {
  container: React.RefObject<HTMLElement | null>;
  source: SourcePage;
  onSource: (page: SourcePage) => void;
  actions: ClipActions;
  onBack: () => void;
}) {
  const { editor, orbit } = useEditorSession();
  const { transport } = editor;

  return (
    <>
      <header className="pe-bar">
        <button type="button" className="ds-icon" onClick={onBack} aria-label="Back">
          <ArrowLeft size={20} />
        </button>
        <input
          className="pe-title"
          value={editor.program.name}
          aria-label="Program name"
          onChange={(event) => editor.rename(event.target.value)}
        />
        <div className="pe-bar-tools">
          <button
            type="button"
            className="ds-icon"
            disabled={!editor.canUndo}
            onClick={editor.undo}
            aria-label="Undo"
          >
            <Undo2 size={18} />
          </button>
          <button
            type="button"
            className="ds-icon"
            disabled={!editor.canRedo}
            onClick={editor.redo}
            aria-label="Redo"
          >
            <Redo2 size={18} />
          </button>
        </div>
        {/* No Save. The timeline writes itself; this only says so. */}
        <CommitMark
          state={
            editor.saving
              ? "committing"
              : editor.error
                ? "refused"
                : editor.draftPending
                  ? "pending"
                  : "settled"
          }
          error={editor.error}
          onRetry={() => void editor.saveDraft()}
        />
        {/* The one act a screen sees. Everything else on this page is a draft. */}
        <button
          type="button"
          className="ds-btn pe-air"
          disabled={!editor.canAir}
          onClick={() => void editor.putOnAir()}
          title={editor.dirty ? "Put this version on every screen that plays it" : "This version is on air"}
        >
          {editor.airing ? "Putting on air…" : editor.dirty ? "Put on air" : "On air"}
        </button>
      </header>

      <div className={`pe-desk${source ? " has-source" : ""}`}>
        <SourceRail source={source} onSource={onSource} />
        {source ? <SourcePanel page={source} onClose={() => onSource(null)} /> : null}

        <main className="pe-centre">
          <Stage
            clip={editor.stageClip}
            t={transport.t}
            playing={transport.playing}
            orbit={orbit}
            trim={editor.trim}
            container={container}
            actions={actions}
          />

          <div className="pe-transport">
            <span className="pe-rail-time">
              {formatClock(transport.t)}
              <span> / {formatClock(editor.durationMs)}</span>
            </span>
            <div className="pe-rail-play">
              <button
                type="button"
                className="pe-skip"
                disabled={editor.durationMs <= 0}
                onClick={() => {
                  transport.pause();
                  transport.seek(0);
                }}
                aria-label="Skip to start"
              >
                <SkipBack size={18} fill="currentColor" />
              </button>
              <button
                type="button"
                className="pe-play"
                disabled={editor.durationMs <= 0}
                onClick={transport.toggle}
                aria-label={transport.playing ? "Pause" : "Play"}
              >
                {transport.playing ? (
                  <Pause size={24} fill="currentColor" />
                ) : (
                  <Play size={24} fill="currentColor" />
                )}
              </button>
              <button
                type="button"
                className="pe-skip"
                disabled={editor.durationMs <= 0}
                onClick={() => {
                  transport.pause();
                  transport.seek(editor.durationMs);
                }}
                aria-label="Skip to end"
              >
                <SkipForward size={18} fill="currentColor" />
              </button>
            </div>
            <span className="ds-hint">
              {editor.clips.length === 1 ? "1 clip" : `${editor.clips.length} clips`}
            </span>
          </div>

          <TimelineHost actions={actions} container={container} />
        </main>

        <Inspector />
      </div>
    </>
  );
}

function SourceRail({
  source,
  onSource,
}: {
  source: SourcePage;
  onSource: (page: SourcePage) => void;
}) {
  const items: { page: Exclude<SourcePage, null>; label: string; Icon: typeof Images }[] = [
    { page: "library", label: "Library", Icon: Images },
    { page: "upload", label: "Upload", Icon: Upload },
    { page: "apps", label: "Apps", Icon: Plus },
  ];
  return (
    <nav className="pe-source-rail" aria-label="Add to this program">
      {items.map(({ page, label, Icon }) => (
        <button
          type="button"
          key={page}
          className={`pe-source-tab${source === page ? " is-on" : ""}`}
          aria-pressed={source === page}
          onClick={() => onSource(source === page ? null : page)}
        >
          <Icon size={20} strokeWidth={1.75} />
          {label}
        </button>
      ))}
    </nav>
  );
}

function SourcePanel({ page, onClose }: { page: Exclude<SourcePage, null>; onClose: () => void }) {
  const { editor, orbit, addKind } = useEditorSession();
  const toast = useToast();
  const fileRef = useRef<HTMLInputElement>(null);

  const upload = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    try {
      const outcome = await uploadContentAll([...files]);
      editor.setRawLibrary((current: SignageMedia[]) => [...outcome.uploaded, ...current]);
      void editor.refreshLibrary();
      if (outcome.refused.length > 0) {
        toast.show("Upload refused", outcome.refused.map((row) => row.reason).join(" "));
      }
    } catch (err) {
      toast.show("Upload refused", err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <section className="pe-source-panel" aria-label={page}>
      <header>
        <strong>{page === "apps" ? "Apps" : page === "upload" ? "Upload" : "Library"}</strong>
        <button type="button" className="ds-icon" onClick={onClose} aria-label="Close">
          <X size={16} />
        </button>
      </header>

      {page === "library" ? (
        <LibraryPicker
          library={editor.library}
          orbit={orbit}
          variant="grid"
          onAdd={(media) => editor.add(media, editor.selectedId)}
          onUploaded={(uploaded) => {
            editor.setRawLibrary((current: SignageMedia[]) => [...uploaded, ...current]);
            void editor.refreshLibrary();
          }}
          onUploadError={(message) => toast.show("Upload refused", message)}
        />
      ) : null}

      {page === "upload" ? (
        <div className="pe-source-upload">
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            onClick={() => fileRef.current?.click()}
          >
            Choose files
          </button>
          <p className="ds-hint">Images and video. They land in the library and on the timeline.</p>
          <input
            ref={fileRef}
            type="file"
            accept="image/*,video/*"
            multiple
            hidden
            onChange={(event) => {
              void upload(event.target.files);
              event.target.value = "";
            }}
          />
        </div>
      ) : null}

      {page === "apps" ? (
        <div className="pe-source-apps">
          {KIND_PANELS.map((panel) => (
            <button
              type="button"
              key={panel.kind}
              className="ds-row"
              onClick={() => void addKind(panel)}
            >
              <span className={`ds-app-mark is-${panel.tone}`}>
                <panel.Icon size={18} strokeWidth={1.8} />
              </span>
              <span className="ds-row-copy">
                {panel.label}
                <span>{panel.description}</span>
              </span>
            </button>
          ))}
        </div>
      ) : null}
    </section>
  );
}

function TimelineHost({
  actions,
  container,
}: {
  actions: ClipActions;
  container: React.RefObject<HTMLElement | null>;
}) {
  const { editor, orbit, addKind } = useEditorSession();
  const toast = useToast();
  // The strip's own `+` expands in place. It is the gesture for "and then
  // this", so it appends rather than inserting after the selection — the rail
  // is where you go to browse, this is where you go to keep going.
  const [addOpen, setAddOpen] = useState(false);
  return (
    <Filmstrip
      program={editor.program}
      library={editor.catalog}
      media={editor.library}
      t={editor.transport.t}
      selectedId={editor.selectedId}
      orbit={orbit}
      addOpen={addOpen}
      wide
      container={container}
      actions={actions}
      onSelect={editor.setSelectedId}
      onSeek={(ms) => {
        editor.transport.pause();
        editor.transport.seek(ms);
      }}
      onMove={editor.move}
      onTrim={editor.setDuration}
      onTrimLive={(preview) => {
        if (preview) editor.transport.pause();
        editor.setTrim(preview);
      }}
      onAddOpenChange={(open) => {
        if (open) void editor.refreshLibrary();
        setAddOpen(open);
      }}
      onAddMedia={(media) => {
        editor.add(media, null);
        setAddOpen(false);
      }}
      onUploaded={(uploaded) => {
        editor.setRawLibrary((current: SignageMedia[]) => [...uploaded, ...current]);
        void editor.refreshLibrary();
      }}
      onAddKind={(panel) => {
        setAddOpen(false);
        void addKind(panel);
      }}
      onUploadError={(message) => toast.show("Upload refused", message)}
    />
  );
}
