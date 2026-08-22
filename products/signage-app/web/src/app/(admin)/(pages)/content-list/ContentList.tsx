import React, {useState, useEffect, useRef} from "react";
import { useSearch } from "@tanstack/react-router";
import { ContentItemProps, ContentDetailsModal, ContentGridItem, ContentListItem, ContentSelectionBar, ContentSortingBar, SortingState, SourceCategory, sourceCategory } from "@/components/content";
import {Grid, List, Info, Trash2, Upload} from "lucide-react";
import { useModal } from "@/hooks/useModal";
import { useContextMenu } from "@/hooks/useContextMenu";
import { ContextMenu, ContextMenuItem } from "@/components/ui/ContextMenu";
import { ConfirmationModal } from "@/components/ui/ConfirmationModal";
import PageHeader from "@/components/common/PageHeader";
import { PageSearchBar } from "@/components/common/PageSearchBar";
import {BiPlus} from "@react-icons/all-files/bi/BiPlus";
import {useCreateBroadcast} from "@/utils/navigation/hooks";
import { deleteMedia, fetchLibrary, fetchMediaUsedBy, saveMedia, uploadContent } from "@/utils/content/api";
import { fetchPrograms } from "@/utils/broadcasts/api";

const CATEGORY_ORDER: SourceCategory[] = ["image", "video", "card", "kind", "live", "stored"];

export const ContentListPage: React.FC = () => {
  const { q: searchQuery } = useSearch({ strict: false }) as { q?: string };
  const [localSearch, setLocalSearch] = useState(searchQuery || "");
  const [items, setItems] = useState<ContentItemProps[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  // Sync local search when URL param changes externally
  useEffect(() => {
    setLocalSearch(searchQuery || "");
  }, [searchQuery]);
  const [viewMode, setViewMode] = useState<"grid" | "list">("grid");
  const [uploading, setUploading] = useState<ContentItemProps[]>([]);
  const [selectedContent, setSelectedContent] = useState<ContentItemProps | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [selectedItems, setSelectedItems] = useState<Set<string>>(new Set());
  const [detailViewItemId, setDetailViewItemId] = useState<string | null>(null);
  const [sortingState, setSortingState] = useState<SortingState>({
    nameSort: null,
    dimensionSort: null,
    typeFilter: null
  });
  const { isOpen: isDetailOpen, openModal: openDetail, closeModal: closeDetail } = useModal();
  const { isOpen: isDeleteConfirmOpen, openModal: openDeleteConfirm, closeModal: closeDeleteConfirm } = useModal();
  const [deleteItemId, setDeleteItemId] = useState<string | null>(null);
  const [deleteMultiple, setDeleteMultiple] = useState(false);
  // Program names still playing the delete targets — fetched before the
  // deletion is offered, so the confirmation can say what it would orphan.
  const [deleteUsedBy, setDeleteUsedBy] = useState<string[]>([]);

  const { handleCreate: handleCreateBroadcast, isCreating: isCreatingBroadcast } = useCreateBroadcast(() => {
    fetchContent();
  });

  const contextMenu = useContextMenu<ContentItemProps>();

  const fetchContent = () => {
    setLoading(true);
    fetchLibrary()
      .then(setItems)
      .catch((err) => {
        setError((err as Error).message || "Failed to load the library");
      })
      .finally(() => {
        setLoading(false);
      });
  }

  useEffect(() => {
    fetchContent();
  }, []);

  // Clear error message after 5 seconds
  useEffect(() => {
    if (error) {
      const timer = setTimeout(() => {
        setError("");
      }, 5000);
      return () => clearTimeout(timer);
    }
  }, [error]);

  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || []);
    if (fileInputRef.current) {
      fileInputRef.current.value = '';
    }
    if (files.length === 0) return;

    const stamp = Date.now();
    const placeholders: ContentItemProps[] = files.map((file, i) => {
      const tempId = `upload-${stamp}-${i}`;
      return {
        id: tempId,
        tempId,
        name: file.name.replace(/\.[^/.]+$/, ""),
        source: "stored",
        content: "",
        size: file.size,
        mime: file.type,
        duration_ms: null,
        width: null,
        height: null,
        isUploading: true,
      };
    });
    setUploading(placeholders);

    try {
      await uploadContent(files);
    } catch (err) {
      setError((err as Error).message || "Upload failed");
    } finally {
      setUploading([]);
      fetchContent();
    }
  };

  const handleUploadClick = () => {
    fileInputRef.current?.click();
  };

  const handleContentClick = (content: ContentItemProps) => {
    // Prevent any event bubbling
    if (window.event) {
      window.event.stopPropagation();
    }

    // If item is already open in detail view, close it
    if (detailViewItemId === content.id) {
      handleDetailClose();
    } else {
      // Otherwise, open it in detail view
      setSelectedContent(content);
      setDetailViewItemId(content.id);
      openDetail();
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

  const handleDetailClose = () => {
    setDetailViewItemId(null);
    closeDetail();
  };

  // The used-by answer exists before the deletion is offered; failing to
  // get it means the deletion is not offered — an unasked question is
  // never "nothing uses this".
  const offerDelete = async (ids: string[], multiple: boolean) => {
    try {
      const usedBy = await Promise.all(ids.map((id) => fetchMediaUsedBy(id)));
      const programIds = [...new Set(usedBy.flat())];
      let names: string[] = [];
      if (programIds.length > 0) {
        const programs = await fetchPrograms();
        names = programIds.map((pid) => programs.find((p) => p.id === pid)?.name ?? pid);
      }
      setDeleteUsedBy(names);
    } catch (err) {
      setError((err as Error).message || "Could not check which broadcasts use this media");
      return;
    }
    setDeleteItemId(multiple ? null : ids[0]);
    setDeleteMultiple(multiple);
    openDeleteConfirm();
  };

  const handleDeleteClick = () => {
    if (detailViewItemId !== null) {
      offerDelete([detailViewItemId], false);
    }
  };

  const handleDeleteMultipleClick = () => {
    offerDelete(Array.from(selectedItems), true);
  };

  const handleDeleteConfirm = async () => {
    try {
      if (deleteMultiple) {
        for (const id of selectedItems) {
          await deleteMedia(id);
        }
        setSelectedItems(new Set());
      } else if (deleteItemId !== null) {
        await deleteMedia(deleteItemId);
        if (detailViewItemId === deleteItemId) {
          handleDetailClose();
        }
      }
      fetchContent();
    } catch (err) {
      setError((err as Error).message || "Failed to delete media");
    }
    closeDeleteConfirm();
    setDeleteItemId(null);
    setDeleteMultiple(false);
    setDeleteUsedBy([]);
  };

  const updateContentName = async (id: string, name: string) => {
    const item = items.find((i) => i.id === id);
    if (!item) return;

    try {
      const { isUploading: _isUploading, tempId: _tempId, ...media } = item;
      await saveMedia({ ...media, name });
      if (selectedContent && selectedContent.id === id) {
        setSelectedContent({ ...selectedContent, name });
      }
      fetchContent();
    } catch (err) {
      setError((err as Error).message || "Failed to rename media");
    }
  };

  // Sorting and filtering logic
  const sortAndFilterContent = (content: ContentItemProps[]): ContentItemProps[] => {
    let filtered = [...content];

    // Apply search query
    if (localSearch) {
      const q = localSearch.toLowerCase();
      filtered = filtered.filter(item => item.name?.toLowerCase().includes(q));
    }

    // Type filtering is client-side on the source tag; stored entries
    // split image/video by mime.
    if (sortingState.typeFilter) {
      filtered = filtered.filter(item => sourceCategory(item) === sortingState.typeFilter);
    }

    // Apply sorting
    if (sortingState.nameSort) {
      filtered.sort((a, b) => {
        const comparison = a.name.localeCompare(b.name);
        return sortingState.nameSort === "asc" ? comparison : -comparison;
      });
    } else if (sortingState.dimensionSort) {
      filtered.sort((a, b) => {
        const aSize = (a.width || 0) * (a.height || 0);
        const bSize = (b.width || 0) * (b.height || 0);
        const comparison = aSize - bSize;
        return sortingState.dimensionSort === "asc" ? comparison : -comparison;
      });
    }

    return filtered;
  };

  if (error && !loading) return <p className="text-red-500">{error}</p>;
  if (loading) return <p>Loading...</p>;

  // Apply sorting and filtering
  const sortedItems = sortAndFilterContent(items);

  // Combine uploading items with sorted items
  const displayItems = [...uploading, ...sortedItems];

  const availableTypes = CATEGORY_ORDER.filter((c) =>
    items.some((item) => sourceCategory(item) === c),
  );

  const getContextMenuItems = (content: ContentItemProps): ContextMenuItem[] => [
    {
      label: "Details",
      icon: <Info className="w-4 h-4" />,
      onClick: () => handleContentClick(content),
    },
    { divider: true, label: "" },
    {
      label: "Move to trash",
      icon: <Trash2 className="w-4 h-4" />,
      onClick: () => {
        setSelectedContent(content);
        offerDelete([content.id], false);
      },
    },
  ];

  const deleteName = items.find((i) => i.id === deleteItemId)?.name;
  const usedByNote = deleteUsedBy.length > 0
    ? ` Still playing in: ${deleteUsedBy.join(", ")}.`
    : "";

  return(
    <div className={"w-full min-w-0 relative justify-self-center pb-50 overflow-x-hidden"}>
      <PageHeader pageTitle={"Media"}>
        <div className="flex flex-row gap-2">
          <button className="px-3 py-2 text-sm font-medium bg-brand-500 text-white shadow-theme-xs hover:bg-brand-600 disabled:bg-brand-300
                        flex items-center gap-1.5 rounded-lg whitespace-nowrap transition" onClick={handleUploadClick}>
            <Upload className="size-4"/>
            Upload
          </button>
          <button className="px-3 py-2 text-sm font-medium bg-white text-gray-700 shadow-theme-xs border border-gray-300 hover:bg-gray-50 disabled:bg-gray-300
                        flex items-center gap-1.5 rounded-lg whitespace-nowrap transition"
                  onClick={() => handleCreateBroadcast()}
                  disabled={isCreatingBroadcast}>
            <BiPlus className="size-4"/>
            {isCreatingBroadcast ? "Creating..." : "New broadcast"}
          </button>
        </div>
      </PageHeader>

      <div className="flex items-center justify-between">
        <p className="flex-1 text-2xl sm:text-xl font-semibold pt-8 pb-4">All Media</p>
      </div>

      {/* Hidden file input */}
      <input
        ref={fileInputRef}
        type="file"
        multiple
        accept="image/*,video/*"
        onChange={handleFileSelect}
        style={{ display: 'none' }}
      />

      <div
        className={`relative h-screen max-h-[calc(100vh-200px)]`}
      >

      {error && <p className="mb-4 text-sm text-red-500">{error}</p>}

      <div className={`flex flex-wrap justify-between items-center mb-3 rounded-lg`}>
        {selectedItems.size > 0 ? (
          <ContentSelectionBar
            selectedCount={selectedItems.size}
            onDelete={handleDeleteMultipleClick}
            onClearSelection={() => setSelectedItems(new Set())}
          />
        ) : (
          <div className="flex flex-wrap items-center gap-2 pr-1 rounded-sm p-0.5 w-full sm:w-auto justify-between">
            <div className="min-w-0 flex-1 sm:flex-none">
              <PageSearchBar
                value={localSearch}
                onChange={setLocalSearch}
                placeholder="Filter media..."
              />
            </div>
            <ContentSortingBar
              sortingState={sortingState}
              onSortingChange={setSortingState}
              availableTypes={availableTypes}
            />
          </div>
        )}
        <div className="flex justify-end px-2 gap-2">
            {/* View mode toggle */}
            <div className="bg-gray-100 dark:bg-gray-800 rounded-sm p-1 justify-self-end hidden sm:flex">
              <button
                onClick={() => setViewMode("grid")}
                className={`p-2 rounded ${viewMode === "grid" ? "bg-white dark:bg-gray-700 shadow-sm" : ""}`}
                title="Grid view"
              >
                <Grid className="w-4 h-4 dark:text-gray-200" />
              </button>
              <button
                onClick={() => setViewMode("list")}
                className={`p-2 rounded ${viewMode === "list" ? "bg-white dark:bg-gray-700 shadow-sm" : ""}`}
                title="List view"
              >
                <List className="w-4 h-4 dark:text-gray-200" />
              </button>
            </div>
        </div>
      </div>

      {/* Content grid/list */}
      {viewMode === "grid" ? (
        <div className={`grid gap-4 grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6`}>
          {displayItems.map((content) => (
            <ContentGridItem
              key={content.tempId || content.id}
              content={content}
              onClick={handleContentClick}
              onContextMenu={(e) => contextMenu.openContextMenu(e, content)}
              isSelected={selectedItems.has(content.id)}
              onToggleSelect={toggleItemSelection}
              onUpdateName={updateContentName}
            />
          ))}
        </div>
      ) : (
        <div className="rounded-sm mt-4 overflow-x-auto">
          <table className="w-full table-fixed">
            <thead className="border-b-gray-200 border-b-1">
              <tr>
                <th className="py-1 text-center text-xs max-md:text-[10px] font-medium text-gray-700 dark:text-gray-300 uppercase w-13">
                  {/* Checkbox column */}
                </th>
                <th className="pr-2 py-1 text-left text-xs max-md:text-[10px] font-medium text-gray-900 dark:text-gray-300 uppercase w-3/6">
                  Name
                </th>
                <th className="pr-2 py-1 text-left text-xs max-md:text-[10px] font-medium text-gray-900 dark:text-gray-300 uppercase w-1/6">
                  Type
                </th>
                <th className="pr-2 py-1 text-left text-xs max-md:text-[10px] font-medium text-gray-900 dark:text-gray-300 uppercase w-2/6">
                  Dimensions
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-200 dark:divide-gray-700  ">
              {displayItems.map((content) => (
                <ContentListItem
                  key={content.tempId || content.id}
                  content={content}
                  onClick={handleContentClick}
                  onContextMenu={(e) => contextMenu.openContextMenu(e, content)}
                  isHighlighted={detailViewItemId === content.id}
                  isSelected={selectedItems.has(content.id)}
                  isDesktop={true}
                  onToggleSelect={toggleItemSelection}
                  onUpdateName={updateContentName}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
      {/* Empty state */}
      {items.length === 0 && !loading && (
        <div className="flex gap-2 flex-col items-center justify-center rounded-md flex-1 text-center border-2 border-dashed bg-gray-50 h-[300px] sm:h-[180px] w-full">
          <p className="text-md sm:text-sm font-medium">
            Drop anything here or click upload
          </p>
          <button onClick={handleUploadClick} className="text-md sm:text-sm border-1 border-gray-300 bg-white hover:bg-gray-100 py-1 px-2 rounded-md font-medium">
            Upload
          </button>
        </div>
      )}
      </div>

      <ContentDetailsModal
        isOpen={isDetailOpen}
        content={selectedContent}
        onClose={handleDetailClose}
        onDelete={handleDeleteClick}
        onUpdate={updateContentName}
      />

      <ConfirmationModal
        isOpen={isDeleteConfirmOpen}
        onClose={closeDeleteConfirm}
        showCloseButton={false}
        onConfirm={handleDeleteConfirm}
        title="Delete Media?"
        message={
          deleteMultiple
            ? `Are you sure you want to delete ${selectedItems.size} selected items?${usedByNote} This action cannot be undone.`
            : `Are you sure you want to delete "${deleteName || selectedContent?.name || 'this media'}"?${usedByNote} This action cannot be undone.`
        }
        confirmText="Delete"
        variant="danger"
      />

      {contextMenu.data && (
        <ContextMenu
          isOpen={contextMenu.isOpen}
          position={contextMenu.position}
          onClose={contextMenu.closeContextMenu}
          header={
            <div>
              <div className="font-medium text-gray-900 dark:text-gray-100">{contextMenu.data.name}</div>
              {contextMenu.data.width && contextMenu.data.height && (
                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                  {contextMenu.data.width}×{contextMenu.data.height}
                </div>
              )}
            </div>
          }
          items={getContextMenuItems(contextMenu.data)}
        />
      )}
    </div>
  );
};
