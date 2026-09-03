/**
 * The editor root: session, chrome that both surfaces share, and the choice
 * between them.
 *
 * It draws almost nothing. Desktop and mobile are separate components over one
 * state, rather than one component with a `wide` boolean threaded through it —
 * which is what made a desktop change require reasoning about the phone.
 */

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useAdminLayout } from "@/context/AdminLayoutContext";
import { useToast } from "@/ds";
import { space } from "@/utils/api/client";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";
import { DesktopShell, type SourcePage } from "./desktop/DesktopShell";
import { MobileShell } from "./mobile/MobileShell";
import { EditorProvider, useEditorSession } from "./state/EditorContext";
import type { ClipActions } from "./ItemMenu";
import { suppressCoarseContextMenu } from "./pointer";
import "./program-editor.css";
import "./surfaces.css";

type Props = {
  initial: SignageProgram;
  library: SignageMedia[];
  persisted: boolean;
  onDraft: (program: SignageProgram) => Promise<void>;
  onAir: (program: SignageProgram) => Promise<void>;
  onClose: () => void;
  onRefreshLibrary: () => Promise<SignageMedia[]>;
};

export function ProgramEditor({
  initial,
  library,
  onDraft,
  onAir,
  onClose,
  onRefreshLibrary,
}: Props) {
  const peRef = useRef<HTMLDivElement>(null);
  const [orbit, setOrbit] = useState<string | null>(null);
  const wide = useWide();
  const { setHideSidebar } = useAdminLayout();
  useVisualViewportBottom();

  useEffect(() => {
    setHideSidebar(true);
    return () => setHideSidebar(false);
  }, [setHideSidebar]);

  useEffect(() => {
    const root = peRef.current;
    if (!root) return;
    root.addEventListener("contextmenu", suppressCoarseContextMenu, true);
    return () => root.removeEventListener("contextmenu", suppressCoarseContextMenu, true);
  }, []);

  useEffect(() => {
    void space().then(setOrbit).catch(() => setOrbit(null));
  }, []);

  return createPortal(
    <div className={`pe${wide ? " is-wide" : " is-narrow"}`} ref={peRef}>
      <EditorProvider
        initial={initial}
        library={library}
        orbit={orbit}
        onDraft={onDraft}
        onAir={onAir}
        onRefreshLibrary={onRefreshLibrary}
      >
        <Session wide={wide} container={peRef} onClose={onClose} />
      </EditorProvider>
    </div>,
    document.body,
  );
}

/** Inside the provider, so the shells and the leave guard share one editor. */
function Session({
  wide,
  container,
  onClose,
}: {
  wide: boolean;
  container: React.RefObject<HTMLElement | null>;
  onClose: () => void;
}) {
  const { editor } = useEditorSession();
  const toast = useToast();
  const [source, setSource] = useState<SourcePage>(null);

  const actions: ClipActions = {
    clipboard: editor.clipboard,
    duplicate: editor.duplicate,
    copy: editor.copy,
    pasteAfter: editor.pasteAfter,
    remove: editor.remove,
    add: () => setSource("library"),
  };

  /**
   * Leaving flushes the draft rather than asks.
   *
   * "Save this program?" is a question that only exists because a product can
   * lose work. This one cannot: the draft already wrote itself, or is about
   * to, so leaving writes whatever is outstanding and goes. What is on air
   * is untouched — that takes the one act this screen offers for it.
   */
  const back = () => {
    if (editor.draftPending && editor.program.items.length > 0) void editor.saveDraft();
    onClose();
  };

  useUndoShortcuts(editor.undo, editor.redo);

  useEffect(() => {
    if (editor.error) toast.show("Could not save", editor.error);
  }, [editor.error, toast]);

  return (
    <>
      {wide ? (
        <DesktopShell
          container={container}
          source={source}
          onSource={setSource}
          actions={actions}
          onBack={back}
        />
      ) : (
        <MobileShell container={container} actions={actions} onBack={back} />
      )}

      {editor.program.items.length === 0 ? (
        <p className="ds-hint pe-hint">
          A program needs at least one item before it can be saved.
        </p>
      ) : null}

    </>
  );
}

function useUndoShortcuts(undo: () => void, redo: () => void) {
  const undoRef = useRef(undo);
  const redoRef = useRef(redo);
  undoRef.current = undo;
  redoRef.current = redo;
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
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
    () => typeof window !== "undefined" && window.matchMedia("(min-width: 900px)").matches,
  );
  useEffect(() => {
    const media = window.matchMedia("(min-width: 900px)");
    const onChange = () => setWide(media.matches);
    onChange();
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);
  return wide;
}
