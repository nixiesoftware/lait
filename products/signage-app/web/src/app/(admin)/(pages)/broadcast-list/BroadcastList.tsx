import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { Clapperboard, Copy, Plus, Trash2 } from "lucide-react";
import {
  Confirm,
  Cover,
  Empty,
  Inspector,
  Page,
  PageHeader,
  PageSearch,
  PageStatus,
  PlaylistRow,
  PlaylistTile,
  SelectionBar,
  ViewToggle,
  haptic,
  useOrbit,
  useToast,
  useWide,
  type MenuItem,
} from "@/ds";
import { Thumb } from "@/program-editor/Thumb";
import { formatDuration, itemDurationMs } from "@/program-editor/model";
import {
  deleteProgram,
  fetchProgram,
  fetchPrograms,
  fetchProgramScreens,
  saveProgram,
} from "@/utils/broadcasts/api";
import { fetchLibrary } from "@/utils/content/api";
import { fetchScreens } from "@/utils/screens/api";
import { mintBodyId } from "@/utils/lait/ids";
import type { SignageItem, SignageMedia } from "@/utils/lait/types";
import { useCreateBroadcast } from "@/utils/navigation/hooks";

export interface ScreenInfo {
  id: string;
  name: string;
}

export interface BroadcastSummary {
  id: string;
  name: string;
  items: SignageItem[];
  contentCount: number;
  assignedScreens: ScreenInfo[];
  assignedScreenCount: number;
}

export const BroadcastListPage: React.FC = () => {
  const navigate = useNavigate();
  const { q: searchQuery } = useSearch({ strict: false }) as { q?: string };
  const toast = useToast();
  const orbit = useOrbit();
  const wide = useWide();

  const [query, setQuery] = useState(searchQuery || "");
  const [programs, setPrograms] = useState<BroadcastSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [viewMode, setViewMode] = useState<"grid" | "list">("list");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [mediaMap, setMediaMap] = useState<Map<string, SignageMedia>>(new Map());
  const [inspect, setInspect] = useState<BroadcastSummary | null>(null);
  const [rename, setRename] = useState("");
  const [deleteIds, setDeleteIds] = useState<string[] | null>(null);

  useEffect(() => {
    setQuery(searchQuery || "");
  }, [searchQuery]);

  const { handleCreate, isCreating } = useCreateBroadcast(() => {
    void load();
  });

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const [list, library, screens] = await Promise.all([
        fetchPrograms(),
        fetchLibrary(),
        fetchScreens(),
      ]);
      setMediaMap(new Map(library.map((media) => [media.id, media])));
      const screenNames = new Map(screens.map((screen) => [screen.id, screen.name]));
      const summaries: BroadcastSummary[] = list.map((program) => ({
        id: program.id,
        name: program.name,
        items: program.items,
        contentCount: program.items.length,
        assignedScreens: [],
        assignedScreenCount: 0,
      }));
      setPrograms(summaries);

      const showing = await Promise.all(
        summaries.map(async (summary) => {
          const ids = await fetchProgramScreens(summary.id);
          const assigned: ScreenInfo[] = ids.map((id) => ({
            id,
            name: screenNames.get(id) ?? id,
          }));
          return { id: summary.id, assigned };
        }),
      );
      const assignedMap = new Map(showing.map((row) => [row.id, row.assigned]));
      setPrograms((prev) =>
        prev.map((summary) => ({
          ...summary,
          assignedScreens: assignedMap.get(summary.id) ?? [],
          assignedScreenCount: assignedMap.get(summary.id)?.length ?? 0,
        })),
      );
    } catch (err) {
      setError((err as Error).message);
      setPrograms([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const openEditor = (id: string) => {
    navigate({ to: `/broadcast-list/broadcast/${id}` });
  };

  const makeCopy = async (row: BroadcastSummary) => {
    try {
      const loaded = await fetchProgram(row.id);
      if (!loaded) return;
      const { program } = loaded;
      const itemIds = new Map(program.items.map((item) => [item.id, mintBodyId()]));
      await saveProgram({
        ...program,
        id: mintBodyId(),
        name: `${program.name} (copy)`,
        items: program.items.map((item) => ({ ...item, id: itemIds.get(item.id)! })),
        windows: program.windows.map((window) => ({
          ...window,
          id: mintBodyId(),
          items: window.items.flatMap((id) => {
            const mapped = itemIds.get(id);
            return mapped ? [mapped] : [];
          }),
        })),
      });
      haptic("save");
      await load();
    } catch {
      toast.show("Failed to copy program");
      haptic("error");
    }
  };

  const updateName = async (id: string, name: string) => {
    try {
      const loaded = await fetchProgram(id);
      if (!loaded) return;
      await saveProgram({ ...loaded.program, name: name.trim() });
      if (inspect?.id === id) setInspect({ ...inspect, name: name.trim() });
      haptic("save");
      await load();
    } catch (err) {
      toast.show("Failed to rename program", (err as Error).message);
      haptic("error");
    }
  };

  const confirmDelete = async () => {
    if (!deleteIds) return;
    try {
      await Promise.all(deleteIds.map((id) => deleteProgram(id)));
      setSelected(new Set());
      if (inspect && deleteIds.includes(inspect.id)) setInspect(null);
      haptic("delete");
      await load();
    } catch (err) {
      toast.show("Failed to delete program", (err as Error).message);
      haptic("error");
    } finally {
      setDeleteIds(null);
    }
  };

  const toggle = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return programs
      .filter((row) => !q || row.name.toLowerCase().includes(q))
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [programs, query]);

  const menuFor = (row: BroadcastSummary): MenuItem[] => [
    { label: "Open", onPick: () => openEditor(row.id) },
    {
      label: "Open in new tab",
      onPick: () => window.open(`/broadcast-list/broadcast/${row.id}`, "_blank"),
    },
    {
      label: "Details",
      onPick: () => {
        setInspect(row);
        setRename(row.name);
      },
    },
    { label: "Make a copy", onPick: () => void makeCopy(row) },
    {
      label: "Delete",
      danger: true,
      onPick: () => setDeleteIds([row.id]),
    },
  ];

  const coverCells = (row: BroadcastSummary) =>
    row.items.slice(0, 4).map((item) => (
      <Thumb key={item.id} media={mediaMap.get(item.media)} orbit={orbit} />
    ));

  const meta = (row: BroadcastSummary) => {
    const ms = row.items.reduce(
      (sum, item) => sum + itemDurationMs(item, mediaMap.get(item.media)),
      0,
    );
    const clips = `${row.contentCount} ${row.contentCount === 1 ? "clip" : "clips"}`;
    const screens =
      row.assignedScreenCount > 0
        ? `${row.assignedScreenCount} ${row.assignedScreenCount === 1 ? "screen" : "screens"}`
        : "not showing";
    return `${clips} · ${formatDuration(ms)} · ${screens}`;
  };

  const showList = !wide || viewMode === "list";

  return (
    <Page>
      <PageHeader title="Programs" icon={<Clapperboard size={20} />}>
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          disabled={isCreating}
          onClick={() => void handleCreate()}
        >
          <Plus size={16} />
          {isCreating ? "Creating…" : "New program"}
        </button>
      </PageHeader>

      <PageStatus loading={loading && programs.length === 0} error={error} />

      {selected.size > 0 ? (
        <SelectionBar count={selected.size} onClear={() => setSelected(new Set())}>
          <button
            type="button"
            className="ds-btn ds-btn-danger"
            onClick={() => setDeleteIds(Array.from(selected))}
          >
            <Trash2 size={16} />
            Delete
          </button>
        </SelectionBar>
      ) : (
        <div className="ds-toolbar">
          <PageSearch
            value={query}
            onChange={setQuery}
            placeholder="Filter programs…"
          />
          <ViewToggle value={viewMode} onChange={setViewMode} />
        </div>
      )}

      {programs.length === 0 && !loading ? (
        <Empty title="Create a program to start a playlist.">
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            disabled={isCreating}
            onClick={() => void handleCreate()}
          >
            New program
          </button>
        </Empty>
      ) : showList ? (
        <div className="ds-pl-list">
          {rows.map((row) => (
            <PlaylistRow
              key={row.id}
              name={row.name}
              meta={meta(row)}
              selected={selected.has(row.id)}
              onSelect={() => toggle(row.id)}
              onOpen={() => openEditor(row.id)}
              menu={menuFor(row)}
              more={menuFor(row)}
            >
              <Cover>{coverCells(row)}</Cover>
            </PlaylistRow>
          ))}
        </div>
      ) : (
        <div className="ds-pl-grid">
          {rows.map((row) => (
            <PlaylistTile
              key={row.id}
              name={row.name}
              meta={meta(row)}
              selected={selected.has(row.id)}
              onSelect={() => toggle(row.id)}
              onOpen={() => openEditor(row.id)}
              menu={menuFor(row)}
              more={menuFor(row)}
            >
              <Cover>{coverCells(row)}</Cover>
            </PlaylistTile>
          ))}
        </div>
      )}

      <Inspector
        open={inspect != null}
        onOpenChange={(open) => {
          if (!open) setInspect(null);
        }}
        title={inspect?.name ?? "Program"}
        actions={
          inspect && (
            <>
              <button
                type="button"
                className="ds-btn ds-btn-solid"
                onClick={() => openEditor(inspect.id)}
              >
                Open
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-ghost"
                onClick={() => void makeCopy(inspect)}
              >
                <Copy size={16} />
                Copy
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-danger"
                onClick={() => setDeleteIds([inspect.id])}
              >
                Delete
              </button>
            </>
          )
        }
      >
        {inspect && (
          <>
            <div className="ds-pl-tile" style={{ marginBottom: 12 }}>
              <Cover>{coverCells(inspect)}</Cover>
            </div>
            <label className="ds-field">
              <span>Name</span>
              <input
                className="ds-input"
                value={rename}
                onChange={(event) => setRename(event.target.value)}
                onBlur={() => {
                  if (rename.trim() && rename.trim() !== inspect.name) {
                    void updateName(inspect.id, rename);
                  }
                }}
              />
            </label>
            <p className="ds-hint">{meta(inspect)}</p>
            {inspect.assignedScreens.length > 0 && (
              <p className="ds-hint">
                Showing on {inspect.assignedScreens.map((s) => s.name).join(", ")}
              </p>
            )}
          </>
        )}
      </Inspector>

      <Confirm
        open={deleteIds != null}
        onOpenChange={(open) => {
          if (!open) setDeleteIds(null);
        }}
        title={
          deleteIds && deleteIds.length > 1
            ? `Delete ${deleteIds.length} programs?`
            : `Delete “${programs.find((p) => p.id === deleteIds?.[0])?.name ?? inspect?.name ?? "this program"}”?`
        }
        description="This cannot be undone."
        confirmLabel="Delete"
        danger
        onConfirm={confirmDelete}
      />
    </Page>
  );
};
