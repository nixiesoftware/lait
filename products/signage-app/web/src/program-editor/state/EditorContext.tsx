/**
 * One editor session, shared by both shells.
 *
 * Composes program state, Space-wide kind config, and the panel that sits over
 * them. A shell reads this and draws; it holds no state of its own beyond
 * layout. That is what makes desktop and mobile separable — they disagree about
 * everything visual and about nothing else.
 */

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { useToast } from "@/ds";
import { saveMedia } from "@/utils/content/api";
import { mintBodyId } from "@/utils/lait/ids";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";
import type { KindPanel } from "../kinds/types";
import { useKindPresets, type KindPresets } from "./useKindPresets";
import { useProgramEditor, type Editor } from "./useProgramEditor";

export type PanelTarget =
  | { sort: "none" }
  /** A kind's presentation, opened from the clip that uses it. */
  | { sort: "kind"; panel: KindPanel; itemId: string | null }
  /** Nothing selected, or the program itself. */
  | { sort: "program" };

type Session = {
  editor: Editor;
  kinds: KindPresets;
  orbit: string | null;
  panel: PanelTarget;
  openKindPanel: (panel: KindPanel, itemId: string | null) => void;
  openProgramPanel: () => void;
  closePanel: () => void;
  /** Mint a library row for a kind, place it, and open its panel. */
  addKind: (panel: KindPanel) => Promise<void>;
  /** How many clips in this program use a kind — the blast radius, locally. */
  usageOf: (kind: string) => number;
};

const EditorSession = createContext<Session | null>(null);

export function useEditorSession(): Session {
  const session = useContext(EditorSession);
  if (!session) {
    throw new Error("useEditorSession must be used inside EditorProvider");
  }
  return session;
}

export function EditorProvider({
  initial,
  library,
  orbit,
  onSave,
  onRefreshLibrary,
  children,
}: {
  initial: SignageProgram;
  library: SignageMedia[];
  orbit: string | null;
  onSave: (program: SignageProgram) => Promise<void>;
  onRefreshLibrary: () => Promise<SignageMedia[]>;
  children: ReactNode;
}) {
  const toast = useToast();
  const kinds = useKindPresets();
  const editor = useProgramEditor({
    initial,
    library,
    resolve: kinds.resolve,
    onSave,
    onRefreshLibrary,
  });
  const [panel, setPanel] = useState<PanelTarget>({ sort: "none" });

  const { add, setRawLibrary, clips, selectAndSeek } = editor;

  const openKindPanel = useCallback(
    (kindPanel: KindPanel, itemId: string | null) => {
      const clip = itemId ? clips.find((entry) => entry.item.id === itemId) : null;
      if (clip) selectAndSeek(clip);
      setPanel({ sort: "kind", panel: kindPanel, itemId });
    },
    [clips, selectAndSeek],
  );

  const openProgramPanel = useCallback(() => setPanel({ sort: "program" }), []);

  const closePanel = useCallback(() => {
    setPanel({ sort: "none" });
    kinds.setDraft(null);
  }, [kinds]);

  /**
   * Adding a kind is one gesture: the row lands, the playhead moves to it, and
   * its panel opens. It used to be an `if` on the kind name — every kind
   * without that branch produced a clip nobody could configure.
   */
  const addKind = useCallback(
    async (kindPanel: KindPanel) => {
      try {
        const existing = await kinds.refresh();
        // Reuse the kind's first preset rather than minting one per clip:
        // presets are meant to be shared, and a clip that quietly owned its
        // own would rebuild the per-entry snapshot this port removed.
        const reuse = existing.find((entry) => entry.kind === kindPanel.kind);
        const preset =
          reuse ?? (await kinds.create(kindPanel.kind, kindPanel.label, kindPanel.defaults));
        const media: SignageMedia = {
          id: mintBodyId(),
          name: kindPanel.label,
          source: "kind",
          kind: kindPanel.kind,
          preset: preset.id,
          // The entry's own settings stay empty. What varies per venue is not
          // here at all — it lives on the screen and arrives at render time.
          settings: {},
          duration_ms: kindPanel.defaultDurationMs,
          width: null,
          height: null,
          catalog: null,
        };
        await saveMedia(media);
        setRawLibrary((current) => [media, ...current]);
        const itemId = add(media);
        setPanel({ sort: "kind", panel: kindPanel, itemId });
      } catch (err) {
        toast.show(
          `Could not add ${kindPanel.label}`,
          err instanceof Error ? err.message : String(err),
        );
      }
    },
    [add, kinds, setRawLibrary, toast],
  );

  const usageOf = useCallback(
    (kind: string) =>
      editor.program.items.filter((item) => {
        const media = editor.catalog.get(item.media);
        return media?.source === "kind" && media.kind === kind;
      }).length,
    [editor.catalog, editor.program.items],
  );

  const value = useMemo(
    () => ({
      editor,
      kinds,
      orbit,
      panel,
      openKindPanel,
      openProgramPanel,
      closePanel,
      addKind,
      usageOf,
    }),
    [
      editor,
      kinds,
      orbit,
      panel,
      openKindPanel,
      openProgramPanel,
      closePanel,
      addKind,
      usageOf,
    ],
  );

  return <EditorSession.Provider value={value}>{children}</EditorSession.Provider>;
}
