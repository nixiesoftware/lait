import { useState, useEffect } from "react";
import { useParams } from "@tanstack/react-router";
import BroadcastEditor from "@/components/broadcasts/broadcast-editor/BroadcastEditor";
import { BroadcastRow } from "@/components/broadcasts/types";
import { useAdminLayout } from "@/context/AdminLayoutContext";
import { fetchProgram, saveProgram } from "@/utils/broadcasts/api";
import { fetchLibrary } from "@/utils/content/api";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";

export default function BroadcastPage() {
  const params = useParams({ strict: false });
  const broadcastId = String(params.id ?? "");
  const { setHideSidebar } = useAdminLayout();
  const [program, setProgram] = useState<SignageProgram | null>(null);
  const [allContent, setAllContent] = useState<SignageMedia[]>([]);
  const [originalRows, setOriginalRows] = useState<BroadcastRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false); // background fetch flag for route changes
  const [error, setError] = useState("");

  const refreshContent = async () => {
    try {
      setAllContent(await fetchLibrary());
    } catch (e) {
      console.error('Failed to refresh library:', e);
    }
  };

  const loadData = async (opts?: { initial?: boolean }) => {
    const initial = opts?.initial ?? false;
    if (initial) setLoading(true); else setRefreshing(true);
    setError("");
    try {
      // One fetch returns the program and the library entries its items
      // name, in item order — the join happens here, not per item.
      const [loaded, catalog] = await Promise.all([
        fetchProgram(broadcastId),
        fetchLibrary(),
      ]);

      if (!loaded) {
        setError("Broadcast not found");
        return;
      }

      setProgram(loaded.program);
      setAllContent(catalog);

      const mediaById = new Map(loaded.media.map(m => [m.id, m]));
      const rows = loaded.program.items.flatMap((item): BroadcastRow[] => {
        const media = mediaById.get(item.media);
        if (!media) {
          console.warn(`Library entry missing for item media: ${item.media}`);
          return [];
        }
        return [{ item, media }];
      });

      setOriginalRows(rows);
    } catch (e: unknown) {
      console.error(e);
      setError(e instanceof Error ? e.message : 'An error occurred');
    } finally {
      if (initial) setLoading(false); else setRefreshing(false);
    }
  };

  useEffect(() => {
    // First load shows skeleton, subsequent broadcastId changes do background refresh
    loadData({ initial: true });
  }, []);

  // Background refresh on broadcastId change after initial mount
  useEffect(() => {
    if (originalRows.length > 0) {
      loadData({ initial: false });
    } else {
      loadData({ initial: true });
    }
  }, [broadcastId]);

  // Hide sidebar while the editor owns the viewport
  useEffect(() => {
    setHideSidebar(true);
    return () => {
      setHideSidebar(false);
    };
  }, [setHideSidebar]);

  // The editor saves the document: the ordered items[] rides one put.
  const handleSave = async (rowsToSave: BroadcastRow[], newName?: string) => {
    if (!program) return;

    const items = rowsToSave.map(r => r.item);
    const itemIds = new Set(items.map(it => it.id));
    const next: SignageProgram = {
      ...program,
      name: newName ?? program.name,
      items,
      // A program window chooses among the program's own items; drop
      // references to items this save removes.
      windows: program.windows.map(w => ({
        ...w,
        items: w.items.filter(id => itemIds.has(id)),
      })),
    };

    await saveProgram(next);
    setProgram(next);
    setOriginalRows(rowsToSave);
  };

  if (loading) return (
    <div className="fixed inset-0 top-0 bottom-0 z-40 overflow-hidden">
      <div className="absolute top-0 left-0 right-0 h-1 bg-gradient-to-r from-brand-400 to-brand-600 animate-pulse"></div>
      <div className="h-full w-full flex items-center justify-center">
        <div className="text-sm text-gray-500 dark:text-gray-300"></div>
      </div>
    </div>
  );
  if (error) return (
    <div className="text-red-500 dark:text-white/90">{error}</div>
  );

  return (
    <div className="fixed inset-0 top-0 bottom-0 z-40 overflow-hidden">
      {/* Background refresh indicator (thin progress bar) */}
      {refreshing && (
        <div className="absolute top-0 left-0 right-0">
          <div className="h-0.5 w-full overflow-hidden bg-transparent">
            <div className="h-full w-full animate-[progress_1.2s_ease-in-out_infinite] bg-gradient-to-r from-brand-400 via-brand-600 to-brand-400" style={{ backgroundSize: '200% 100%' }}></div>
          </div>
        </div>
      )}
      <BroadcastEditor
        broadcastId={broadcastId}
        broadcastName={program?.name ?? ""}
        initialRows={originalRows}
        allContent={allContent}
        onContentUploaded={refreshContent}
        onSave={handleSave}
      />
    </div>
  );
}
