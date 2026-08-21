import React, { useState, useEffect, useCallback } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { BroadcastGridItem, BroadcastListItem, BroadcastDetailsModal } from "@/components/broadcasts";
import { AddBroadcastButton } from "@/components/broadcasts/AddBroadcastButton";
import { GridListView } from "@/components/ui/GridListView";
import { ContentSelectionBar } from "@/components/content/ContentSelectionBar";
import { ContentSortingBar, SortingState } from "@/components/content/ContentSortingBar";
import { ConfirmationModal } from "@/components/ui/ConfirmationModal";
import { ContextMenu, ContextMenuItem } from "@/components/ui/ContextMenu";
import { useModal } from "@/hooks/useModal";
import { useContextMenu } from "@/hooks/useContextMenu";
import { ExternalLink, Info, Copy, Trash2 } from "lucide-react";
import { useIsTabletUp } from "@/hooks/useResponsive";
import PageHeader from "@/components/common/PageHeader";
import { PageSearchBar } from "@/components/common/PageSearchBar";
import {
  fetchPrograms,
  fetchProgram,
  saveProgram,
  deleteProgram,
  fetchProgramScreens,
} from "@/utils/broadcasts/api";
import { fetchLibrary } from "@/utils/content/api";
import { fetchScreens } from "@/utils/screens/api";
import { mintBodyId } from "@/utils/lait/ids";
import type { SignageItem, SignageMedia } from "@/utils/lait/types";

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
  const [localSearch, setLocalSearch] = useState(searchQuery || "");
  const [broadcasts, setBroadcasts] = useState<BroadcastSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [viewMode, setViewMode] = useState<"grid" | "list">("list");

  // Sync local search when URL param changes externally
  useEffect(() => {
    setLocalSearch(searchQuery || "");
  }, [searchQuery]);
  const [selectedItems, setSelectedItems] = useState<Set<string>>(new Set());
  const { isTabletUp, width } = useIsTabletUp();
  const [isDesktop, setIsDesktop] = useState(false);
  const [mediaMap, setMediaMap] = useState<Map<string, SignageMedia>>(new Map());
  const [sortingState, setSortingState] = useState<SortingState>({
    nameSort: "asc",
    dimensionSort: null,
    typeFilter: null
  });

  const { isOpen: isDeleteConfirmOpen, openModal: openDeleteConfirm, closeModal: closeDeleteConfirm } = useModal();
  const { isOpen: isDetailOpen, openModal: openDetail, closeModal: closeDetail } = useModal();
  const [selectedBroadcast, setSelectedBroadcast] = useState<BroadcastSummary | null>(null);
  const [selectedBroadcastId, setSelectedBroadcastId] = useState<string | null>(null);
  const [detailViewItemId, setDetailViewItemId] = useState<string | null>(null);

  // Context menu
  const contextMenu = useContextMenu<BroadcastSummary>();

  // Track responsive state using shared hook (tablet breakpoint)
  useEffect(() => {
    setIsDesktop(isTabletUp);
  }, [isTabletUp, width]);

  const fetchBroadcasts = useCallback(async () => {
    setLoading(true);
    setError("");

    try {
      const [programs, library, screens] = await Promise.all([
        fetchPrograms(),
        fetchLibrary(),
        fetchScreens(),
      ]);

      setMediaMap(new Map(library.map(media => [media.id, media])));

      const screenNames = new Map(screens.map(screen => [screen.id, screen.name]));
      const summaries: BroadcastSummary[] = programs.map(program => ({
        id: program.id,
        name: program.name,
        items: program.items,
        contentCount: program.items.length,
        assignedScreens: [],
        assignedScreenCount: 0,
      }));
      setBroadcasts(summaries);

      // Which screens intend each program — the World's own index.
      const showing = await Promise.all(
        summaries.map(async (summary) => {
          const ids = await fetchProgramScreens(summary.id);
          const assigned: ScreenInfo[] = ids.map(id => ({ id, name: screenNames.get(id) ?? id }));
          return { id: summary.id, assigned };
        })
      );
      const assignedMap = new Map(showing.map(s => [s.id, s.assigned]));
      setBroadcasts(prev => prev.map(summary => ({
        ...summary,
        assignedScreens: assignedMap.get(summary.id) ?? [],
        assignedScreenCount: assignedMap.get(summary.id)?.length ?? 0,
      })));
    } catch (err) {
      setError((err as Error).message);
      setBroadcasts([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchBroadcasts();
  }, [fetchBroadcasts]);

  const handleCardClick = (broadcast: BroadcastSummary) => {
    // Prevent any event bubbling
    if (window.event) {
      window.event.stopPropagation();
    }

    // If item is already open in detail view, close it
    if (detailViewItemId === broadcast.id) {
      handleDetailClose();
    } else {
      // Otherwise, open it in detail view
      setSelectedBroadcast(broadcast);
      setDetailViewItemId(broadcast.id);
      openDetail();
    }
  };

  const handleDetailClose = () => {
    setDetailViewItemId(null);
    closeDetail();
  };

  const handleViewBroadcastItems = (broadcastId: string) => {
    navigate({ to: `/broadcast-list/broadcast/${broadcastId}` });
  };

  const handleOpenInNewTab = (broadcastId: string) => {
    window.open(`/broadcast-list/broadcast/${broadcastId}`, '_blank');
  };

  const handleMakeCopy = async (broadcast: BroadcastSummary) => {
    try {
      const loaded = await fetchProgram(broadcast.id);
      if (!loaded) return;
      const { program } = loaded;
      // The copy is a new document: fresh program, item, and window ids,
      // with each window's item references remapped to the new item ids.
      const itemIds = new Map(program.items.map(item => [item.id, mintBodyId()]));
      await saveProgram({
        ...program,
        id: mintBodyId(),
        name: `${program.name} (copy)`,
        items: program.items.map(item => ({ ...item, id: itemIds.get(item.id)! })),
        windows: program.windows.map(window => ({
          ...window,
          id: mintBodyId(),
          items: window.items.flatMap(id => {
            const mapped = itemIds.get(id);
            return mapped ? [mapped] : [];
          }),
        })),
      });
      fetchBroadcasts();
    } catch {
      setError("Failed to copy broadcast");
    }
  };

  const toggleItemSelection = (id: string) => {
    setSelectedItems(prev => {
      const newSet = new Set(prev);
      if (newSet.has(id)) {
        newSet.delete(id);
      } else {
        newSet.add(id);
      }
      return newSet;
    });
  };

  const deleteBroadcast = async (id: string) => {
    try {
      await deleteProgram(id);
      handleDetailClose();
      fetchBroadcasts();
    } catch (err) {
      setError((err as Error).message || "Failed to delete broadcast");
    }
  };

  const deleteSelectedBroadcasts = async () => {
    if (selectedItems.size === 0) return;

    try {
      await Promise.all(Array.from(selectedItems).map(id => deleteProgram(id)));

      // Clear selection and refresh
      setSelectedItems(new Set());
      fetchBroadcasts();
    } catch (err) {
      setError((err as Error).message || "Failed to delete selected broadcasts");
    }
  };

  const handleDeleteMultipleClick = () => {
    openDeleteConfirm();
  };

  const handleDeleteConfirm = async () => {
    if (selectedBroadcastId) {
      await deleteBroadcast(selectedBroadcastId);
    } else if (selectedItems.size > 0) {
      await deleteSelectedBroadcasts();
    }
    setSelectedBroadcastId(null);
    closeDeleteConfirm();
  };

  const updateBroadcastName = async (id: string, name: string) => {
    try {
      // The document is the unit of save: read it whole, rename, put whole.
      const loaded = await fetchProgram(id);
      if (!loaded) return;
      await saveProgram({ ...loaded.program, name });

      // Update selected broadcast if this is the item being viewed
      if (selectedBroadcast && selectedBroadcast.id === id) {
        setSelectedBroadcast({ ...selectedBroadcast, name });
      }

      fetchBroadcasts();
    } catch (err) {
      setError((err as Error).message || "Failed to update broadcast name");
    }
  };

  // Sorting and search filtering logic
  const sortAndFilterBroadcasts = (broadcasts: BroadcastSummary[]): BroadcastSummary[] => {
    let filtered = [...broadcasts];

    // Apply search query
    if (localSearch) {
      const q = localSearch.toLowerCase();
      filtered = filtered.filter(b => b.name.toLowerCase().includes(q));
    }

    if (sortingState.nameSort) {
      filtered.sort((a, b) => {
        const comparison = a.name.localeCompare(b.name);
        return sortingState.nameSort === "asc" ? comparison : -comparison;
      });
    }

    return filtered;
  };

  if (error && !loading) return <p className="text-red-500">{error}</p>;
  if (loading) return <p></p>;

  const sortedBroadcasts = sortAndFilterBroadcasts(broadcasts);

  const getContextMenuItems = (broadcast: BroadcastSummary): ContextMenuItem[] => [
    {
      label: "Open in new tab",
      icon: <ExternalLink className="w-4 h-4" />,
      onClick: () => handleOpenInNewTab(broadcast.id),
    },
    {
      label: "Details",
      icon: <Info className="w-4 h-4" />,
      onClick: () => handleCardClick(broadcast),
    },
    {
      label: "Make a copy",
      icon: <Copy className="w-4 h-4" />,
      onClick: () => handleMakeCopy(broadcast),
    },
    { divider: true, label: "" },
    {
      label: "Move to trash",
      icon: <Trash2 className="w-4 h-4" />,
      onClick: () => {
        setSelectedBroadcast(broadcast);
        setSelectedBroadcastId(broadcast.id);
        openDeleteConfirm();
      },
    },
  ];

  const emptyState = broadcasts.length === 0 && (
    <div className="flex flex-col items-center justify-center rounded-md flex-1 text-center border-2 border-dashed bg-gray-50 h-[300px] sm:h-[180px] w-full">
      <button className="text-md sm:text-sm border-1 border-gray-300 bg-white hover:bg-gray-100 py-1 px-2 rounded-md font-medium">Create a broadcast</button>
    </div>
  );

  return (
    <div className={"w-full relative justify-self-center pb-50"}>
      <PageHeader pageTitle={"Broadcasts"}>
        <AddBroadcastButton onAdd={fetchBroadcasts}/>
      </PageHeader>

      {/* All broadcasts */}
      <div className="flex items-center justify-between pt-6 pb-2">
        <p className="text-lg font-semibold">All Broadcasts</p>
      </div>

      <div className="flex flex-row gap-2 items-center justify-between rounded-lg pb-4 text-[10px]">
        {selectedItems.size > 0 ? (
          <ContentSelectionBar
            selectedCount={selectedItems.size}
            onDelete={handleDeleteMultipleClick}
            onClearSelection={() => setSelectedItems(new Set())}
          />
        ) : (
          <>
            <PageSearchBar
              value={localSearch}
              onChange={setLocalSearch}
              placeholder="Filter broadcasts..."
            />
            <div className="flex items-center gap-2">
              <ContentSortingBar
                sortingState={sortingState}
                onSortingChange={setSortingState}
                availableTypes={[]}
                hideTypeFilter={true}
                hideDimensionSort={true}
              />
            </div>
          </>
        )}
      </div>

      <div className="relative flex">
        {broadcasts.length === 0 ? (
          emptyState
        ) : (
          <GridListView
            items={sortedBroadcasts}
            viewMode={viewMode}
            onViewModeChange={setViewMode}
            isDesktop={isDesktop}
            gridClassName={`grid gap-3 grid-cols-2 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-4`}
            renderGridItem={(broadcast) => (
              <BroadcastGridItem
                key={broadcast.id}
                {...broadcast}
                items={broadcast.items}
                mediaMap={mediaMap}
                isSelected={selectedItems.has(broadcast.id)}
                onClick={() => handleViewBroadcastItems(broadcast.id)}
                onContextMenu={(e) => contextMenu.openContextMenu(e, broadcast)}
                onToggleSelect={toggleItemSelection}
                onUpdateName={updateBroadcastName}
              />
            )}
            renderListItem={(broadcast) => (
              <BroadcastListItem
                key={broadcast.id}
                {...broadcast}
                items={broadcast.items}
                mediaMap={mediaMap}
                isSelected={selectedItems.has(broadcast.id)}
                isHighlighted={detailViewItemId === broadcast.id}
                isDesktop={isDesktop}
                onClick={() => handleViewBroadcastItems(broadcast.id)}
                onContextMenu={(e) => contextMenu.openContextMenu(e, broadcast)}
                onToggleSelect={toggleItemSelection}
                onUpdateName={updateBroadcastName}
              />
            )}
          />
        )}
      </div>

      <ConfirmationModal
        isOpen={isDeleteConfirmOpen}
        onClose={closeDeleteConfirm}
        showCloseButton={false}
        onConfirm={handleDeleteConfirm}
        title={selectedBroadcastId
          ? `Delete "${selectedBroadcast?.name}" Broadcast?`
          : `Delete ${selectedItems.size} selected broadcasts?`}
        message={`This action cannot be undone.`}
        confirmText="Delete"
        variant="danger"
      />

      <BroadcastDetailsModal
        isOpen={isDetailOpen}
        broadcast={selectedBroadcast}
        onClose={handleDetailClose}
        onDelete={deleteBroadcast}
        onUpdate={updateBroadcastName}
        onViewItems={handleViewBroadcastItems}
      />

      {contextMenu.data && (
        <ContextMenu
          isOpen={contextMenu.isOpen}
          position={contextMenu.position}
          onClose={contextMenu.closeContextMenu}
          header={
            <div>
              <div className="font-medium text-gray-900 dark:text-gray-100">{contextMenu.data.name}</div>
              <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                {contextMenu.data.contentCount} {contextMenu.data.contentCount === 1 ? 'item' : 'items'}
              </div>
            </div>
          }
          items={getContextMenuItems(contextMenu.data)}
        />
      )}
    </div>
  );
};
