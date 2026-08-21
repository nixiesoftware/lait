import React, { useState, useEffect, useReducer } from "react";
import { motion } from "framer-motion";
import { BroadcastRow, rowDurationSeconds } from "@/components/broadcasts/types";
import type { SignageConfig, SignageMedia } from "@/utils/lait/types";
import ContentPreview from "./components/ContentPreview";
import PreviewBackdropBlur from "@/components/broadcasts/broadcast-editor/components/PreviewBackdropBlur";
import { Integration } from "@/components/integrations/types/integrations";
import { KINDS, fetchConfigs } from "@/utils/apps/api";
import { saveMedia } from "@/utils/content/api";
import { mintBodyId } from "@/utils/lait/ids";
import BroadcastHeader from "./components/BroadcastHeader";
import { Copy, Clipboard, Trash2, Plus, Layers, Clock } from "lucide-react";
import { useModal } from "@/hooks/useModal";
import { useContextMenu } from "@/hooks/useContextMenu";
import { ContextMenu, ContextMenuItem } from "@/components/ui/ContextMenu";
import { BroadcastClipboardProvider, useBroadcastClipboard } from "@/context/BroadcastClipboardContext";
import BroadcastTimeline from "@/components/broadcasts/broadcast-editor/components/BroadcastTimeline";
import { broadcastReducer } from "@/state/broadcastReducer";
import { loadDraftRows, saveDraftRows, clearDraftRows, loadSelection, saveSelection, clearSelection } from "@/utils/persistence";
import { useGlobalHotkeys, Hotkey } from "@/utils/keyboard/useGlobalHotkeys";
import Button from "@/components/ui/button/Button";
import { Modal } from "@/components/ui/modal";
import { useNavigate } from "@tanstack/react-router";


interface BroadcastEditorProps {
  broadcastId: string;
  broadcastName: string;
  initialRows: BroadcastRow[];
  allContent: SignageMedia[];
  onSave: (rows: BroadcastRow[], name?: string) => Promise<void>;
  onContentUploaded?: () => void;
}

// The Apps picker offers the kinds this application knows how to render
// forms for, filtered to those the Space has configured — an unconfigured
// kind is hidden and the Apps page is where to configure it.
function configuredIntegrations(configs: SignageConfig[]): Integration[] {
  return KINDS
    .filter((kind) => configs.some((config) => config.kind === kind.kind))
    .map((kind) => ({
      id: kind.kind,
      title: kind.label,
      path: `kind:${kind.kind}`,
    }));
}

function BroadcastEditorContent({
  broadcastId,
  broadcastName: initialName,
  initialRows,
  allContent,
  onSave,
  onContentUploaded
}: BroadcastEditorProps) {
  const { isOpen: isExitConfirmOpen, openModal: openExitConfirm, closeModal: closeExitConfirm } = useModal();

  const [rowsState, dispatch] = useReducer(broadcastReducer, { rows: [] });
  const rows = rowsState.rows;

  const [selectedItemId, setSelectedItemId] = useState<string | null>(() => loadSelection(broadcastId));

  const [isDirty, setIsDirty] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [showSaveToast, setShowSaveToast] = useState(false);
  const [previewContent, setPreviewContent] = useState<SignageMedia | null>(null);
  const [broadcastName, setBroadcastName] = useState(initialName);
  const [nameChanged, setNameChanged] = useState(false);
  // Baseline used for dirty checking in local-caching mode
  const [baselineRows, setBaselineRows] = useState<BroadcastRow[]>(initialRows);
  const [baselineName, setBaselineName] = useState(initialName);
  const [isIntentionalExit, setIsIntentionalExit] = useState(false);
  const videoRef = React.useRef<HTMLVideoElement>(null);
  const navigate = useNavigate();

  const [configs, setConfigs] = useState<SignageConfig[]>([]);
  useEffect(() => {
    let cancelled = false;
    fetchConfigs()
      .then((list) => {
        if (!cancelled) setConfigs(list);
      })
      .catch(() => {
        // A configs load failure shouldn't prevent editing existing rows.
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const integrations = configuredIntegrations(configs);

  // Initialize reducer rows from localStorage or props
  useEffect(() => {
    const storedDraft = loadDraftRows(broadcastId);

    let rowsToInit = initialRows;

    if (storedDraft) {
      try {
        const draft: BroadcastRow[] = storedDraft;

        if (draft.length === 0 && initialRows.length > 0) {
          clearDraftRows(broadcastId);
          rowsToInit = initialRows;
        } else if (draft.length > 0) {
          const serverItems = new Map(initialRows.map(row => [row.item.id, row]));
          const hasUnsavedChanges = draft.length !== initialRows.length ||
            draft.some((draftRow, index) => {
              const serverRow = serverItems.get(draftRow.item.id);
              return !serverRow ||
                draftRow.item.media !== serverRow.item.media ||
                draftRow.item.duration_ms !== serverRow.item.duration_ms ||
                initialRows[index]?.item.id !== draftRow.item.id;
            });

          if (hasUnsavedChanges) {
            rowsToInit = draft;
          } else {
            clearDraftRows(broadcastId);
            rowsToInit = initialRows;
          }
        }
      } catch {
        clearDraftRows(broadcastId);
        rowsToInit = initialRows;
      }
    }

    dispatch({ type: 'INIT', rows: rowsToInit });
    setBaselineRows(initialRows);
  }, []);

  // Track changes and persist to localStorage
  useEffect(() => {
    const toStable = (arr: BroadcastRow[]) =>
      arr.map(r => ({ id: r.item.id, m: r.item.media, d: r.item.duration_ms }));

    const hasRowChanges = JSON.stringify(toStable(rows)) !== JSON.stringify(toStable(baselineRows));
    const hasChanges = hasRowChanges || nameChanged;
    setIsDirty(hasChanges);

    if (hasRowChanges) {
      saveDraftRows(broadcastId, rowsState.rows);
    }
  }, [rows, baselineRows, nameChanged, rowsState.rows, broadcastId]);

  // Update preview when selection changes and persist selection
  useEffect(() => {
    if (selectedItemId !== null) {
      const selectedRow = rows.find(r => r.item.id === selectedItemId);
      setPreviewContent(selectedRow?.media || null);
      saveSelection(broadcastId, selectedItemId);
    } else {
      setPreviewContent(null);
      clearSelection(broadcastId);
    }
  }, [selectedItemId, rows, broadcastId]);

  // Auto-select first item if no selection and items exist
  useEffect(() => {
    if (selectedItemId === null && rows.length > 0) {
      setSelectedItemId(rows[0].item.id);
    } else if (rows.length === 0 && selectedItemId !== null) {
      setSelectedItemId(null);
    }
  }, [rows, selectedItemId, setSelectedItemId]);

  // Handle browser back button and tab close
  useEffect(() => {
    const handlePopState = () => {
      if (isDirty && !isIntentionalExit) {
        window.history.pushState(null, '', window.location.href);
        openExitConfirm();
      }
    };

    const handleBeforeUnload = (e: BeforeUnloadEvent) => {
      if (isDirty && !isIntentionalExit) {
        e.preventDefault();
        e.returnValue = '';
        return '';
      }
    };

    window.addEventListener('popstate', handlePopState);
    window.addEventListener('beforeunload', handleBeforeUnload);

    return () => {
      window.removeEventListener('popstate', handlePopState);
      window.removeEventListener('beforeunload', handleBeforeUnload);
    };
  }, [isDirty, openExitConfirm, isIntentionalExit]);

  const handleSave = async () => {
    setIsSaving(true);
    const currentSelection = selectedItemId;

    try {
      await onSave(rows, nameChanged ? broadcastName : undefined);

      clearDraftRows(broadcastId);
      if (currentSelection !== null) {
        saveSelection(broadcastId, currentSelection);
      }

      setBaselineRows(rows);
      if (nameChanged) {
        setBaselineName(broadcastName);
      }
      setIsDirty(false);
      setNameChanged(false);

      setShowSaveToast(true);
      setTimeout(() => setShowSaveToast(false), 2000);
    } catch (error) {
      console.error("Failed to save broadcast:", error);
      alert('Failed to save broadcast. Please try again.');
    } finally {
      setIsSaving(false);
    }
  };

  const handleExitConfirm = async () => {
    setIsIntentionalExit(true);
    await handleSave();
    const { goBack } = await import("@/utils/navigation/goBack");
    goBack(navigate, "/broadcast-list");
  };

  const handleExitCancel = () => {
    setIsIntentionalExit(true);

    clearDraftRows(broadcastId);
    clearSelection(broadcastId);

    setTimeout(async () => {
      const { goBack } = await import("@/utils/navigation/goBack");
      goBack(navigate, "/broadcast-list");
    }, 0);
  };

  const handleAddContent = (media: SignageMedia) => {
    // duration_ms null defers to the library entry's default (rule of the
    // wire, not a hold) — an explicit edit on the timeline overrides it.
    const newRow: BroadcastRow = {
      item: { id: mintBodyId(), media: media.id, duration_ms: null },
      media,
    };
    dispatch({ type: 'ADD_ITEM', row: newRow });
    setSelectedItemId(newRow.item.id);
  };

  const handleSelectIntegration = async (intg: Integration) => {
    // An integration item is a library entry naming the kind, with the
    // Space's configuration snapshotted into its settings, then a program
    // item naming that entry — no draft sentinel, no second save path.
    const config = configs.find((c) => c.kind === intg.id);
    const media: SignageMedia = {
      id: mintBodyId(),
      name: intg.title,
      source: { source: 'kind', kind: intg.id, settings: config?.settings ?? {} },
      duration_ms: 60_000,
      width: null,
      height: null,
      catalog: null,
    };
    try {
      await saveMedia(media);
    } catch (error) {
      console.error('Failed to create integration media:', error);
      alert('Failed to add the app. Please try again.');
      return;
    }
    onContentUploaded?.();

    const newRow: BroadcastRow = {
      item: { id: mintBodyId(), media: media.id, duration_ms: null },
      media,
    };

    const selectedIndex = selectedItemId !== null
      ? rows.findIndex(r => r.item.id === selectedItemId)
      : -1;
    const insertIndex = selectedIndex === -1 ? rows.length : selectedIndex + 1;

    const newRows = [...rows];
    newRows.splice(insertIndex, 0, newRow);
    dispatch({ type: 'REORDER', rows: newRows });
    setSelectedItemId(newRow.item.id);
  };


  // Context menu and clipboard
  const contextMenu = useContextMenu<BroadcastRow>();
  const emptyAreaContextMenu = useContextMenu<void>();
  const { clipboardItem, copyItem, hasClipboardItem } = useBroadcastClipboard();

  const handleRemoveItem = (itemId: string) => {
    const currentIndex = rows.findIndex(r => r.item.id === itemId);
    const newRows = rows.filter(r => r.item.id !== itemId);

    if (selectedItemId === itemId) {
      if (newRows.length > 0) {
        if (currentIndex > 0) setSelectedItemId(newRows[currentIndex - 1].item.id);
        else setSelectedItemId(newRows[0].item.id);
      } else setSelectedItemId(null);
    }

    dispatch({ type: 'REORDER', rows: newRows });
  };

  const handleDurationChange = (itemId: string, newDuration: number) => {
    dispatch({ type: 'UPDATE_DURATION', id: itemId, duration_ms: Math.round(newDuration * 1000) });
  };

  const handleReorder = (newRows: BroadcastRow[]) => {
    dispatch({ type: 'REORDER', rows: newRows });
  };

  // Context menu handlers
  const handleCopyItem = (item: BroadcastRow) => {
    copyItem(item);
  };

  const handlePasteItem = () => {
    if (!clipboardItem || !contextMenu.data) return;

    const targetIndex = rows.findIndex(r => r.item.id === contextMenu.data!.item.id);
    if (targetIndex === -1) return;

    const newItem: BroadcastRow = {
      ...clipboardItem,
      item: { ...clipboardItem.item, id: mintBodyId() },
    };

    const newRows = [...rows];
    newRows.splice(targetIndex + 1, 0, newItem);
    dispatch({ type: 'REORDER', rows: newRows });
    setSelectedItemId(newItem.item.id);
  };

  const handleDuplicateItem = (item: BroadcastRow) => {
    const targetIndex = rows.findIndex(r => r.item.id === item.item.id);
    if (targetIndex === -1) return;

    const duplicateItem: BroadcastRow = {
      ...item,
      item: { ...item.item, id: mintBodyId() },
    };

    const newRows = [...rows];
    newRows.splice(targetIndex + 1, 0, duplicateItem);
    dispatch({ type: 'REORDER', rows: newRows });
    setSelectedItemId(duplicateItem.item.id);
  };

  const handleDeleteItem = (item: BroadcastRow) => {
    handleRemoveItem(item.item.id);
  };


  const handleEditDuration = (item: BroadcastRow) => {
    if (selectedItemId !== item.item.id) {
      setSelectedItemId(item.item.id);
    }

    // Focus the duration input after a brief delay to allow UI to update
    setTimeout(() => {
      const durationButton = document.querySelector('.floating-controls-duration-button') as HTMLButtonElement;
      if (durationButton) {
        durationButton.click();
        setTimeout(() => {
          const durationInput = durationButton.parentElement?.querySelector('input[type="number"]') as HTMLInputElement;
          if (durationInput) {
            durationInput.focus();
            durationInput.select();
          }
        }, 50);
      }
    }, 100);
  };

  const handlePasteAtEnd = () => {
    if (!clipboardItem) return;

    const newItem: BroadcastRow = {
      ...clipboardItem,
      item: { ...clipboardItem.item, id: mintBodyId() },
    };
    dispatch({ type: 'ADD_ITEM', row: newItem });
    setSelectedItemId(newItem.item.id);
  };

  // Get context menu items
  const getContextMenuItems = (item: BroadcastRow): ContextMenuItem[] => [
    {
      label: "New page",
      icon: <Plus className="w-4 h-4" />,
      disabled: true
    },
    {
      label: "Edit Duration",
      icon: <Clock className="w-4 h-4" />,
      onClick: () => handleEditDuration(item)
    },
    { label: "", divider: true },
    {
      label: "Copy",
      icon: <Copy className="w-4 h-4" />,
      onClick: () => handleCopyItem(item)
    },
    {
      label: "Paste",
      icon: <Clipboard className="w-4 h-4" />,
      onClick: handlePasteItem,
      disabled: !hasClipboardItem
    },
    {
      label: "Duplicate",
      icon: <Copy className="w-4 h-4" />,
      onClick: () => handleDuplicateItem(item)
    },
    {
      label: "Delete",
      icon: <Trash2 className="w-4 h-4" />,
      onClick: () => handleDeleteItem(item)
    },
    { label: "", divider: true },
    {
      label: "Add transition",
      icon: <Layers className="w-4 h-4" />,
      disabled: true
    }
  ];

  const handleContextMenu = (e: React.MouseEvent, item: BroadcastRow) => {
    contextMenu.openContextMenu(e, item);
  };

  const handleEmptyAreaContextMenu = (e: React.MouseEvent) => {
    emptyAreaContextMenu.openContextMenu(e);
  };

  // Get empty area context menu items
  const getEmptyAreaContextMenuItems = (): ContextMenuItem[] => [
    {
      label: "New page",
      icon: <Plus className="w-4 h-4" />,
      disabled: true
    },
    {
      label: "Paste",
      icon: <Clipboard className="w-4 h-4" />,
      onClick: handlePasteAtEnd,
      disabled: !hasClipboardItem
    }
  ];

  const handleNameChange = (newName: string) => {
    setBroadcastName(newName);
    setNameChanged(newName !== baselineName);
  };

  const handleDiscard = () => {
    const currentSelection = selectedItemId;

    dispatch({ type: 'INIT', rows: initialRows });
    setBroadcastName(initialName);
    setNameChanged(false);
    setIsDirty(false);

    setIsIntentionalExit(true);
    setTimeout(() => setIsIntentionalExit(false), 100);

    clearDraftRows(broadcastId);
    const itemStillExists = initialRows.some(r => r.item.id === currentSelection);
    if (itemStillExists && currentSelection !== null) {
      saveSelection(broadcastId, currentSelection);
      setSelectedItemId(currentSelection);
    } else if (initialRows.length > 0) {
      setSelectedItemId(initialRows[0].item.id);
    } else {
      setSelectedItemId(null);
    }
  };


  // Global hotkeys for the editor (copy/paste/delete and selection nav)
  useGlobalHotkeys((e) => {
    if (Hotkey.isCopy(e)) {
      if (selectedItemId !== null) {
        const item = rows.find(r => r.item.id === selectedItemId);
        if (item) {
          copyItem(item);
          e.preventDefault();
        }
      }
    } else if (Hotkey.isPaste(e)) {
      // Paste after selected or at end if none selected
      if (clipboardItem) {
        if (selectedItemId !== null) {
          const targetIndex = rows.findIndex(r => r.item.id === selectedItemId);
          if (targetIndex >= 0) {
            const newItem: BroadcastRow = {
              ...clipboardItem,
              item: { ...clipboardItem.item, id: mintBodyId() },
            };
            const newRows = [...rows];
            newRows.splice(targetIndex + 1, 0, newItem);
            dispatch({ type: 'REORDER', rows: newRows });
            setSelectedItemId(newItem.item.id);
          }
        } else {
          handlePasteAtEnd();
        }
      }
      e.preventDefault();
    } else if (Hotkey.isDelete(e)) {
      if (selectedItemId !== null) {
        handleRemoveItem(selectedItemId);
        e.preventDefault();
      }
    } else if (Hotkey.isArrowLeft(e)) {
      if (rows.length > 0) {
        if (selectedItemId === null) {
          setSelectedItemId(rows[0].item.id);
        } else {
          const idx = rows.findIndex(r => r.item.id === selectedItemId);
          if (idx > 0) setSelectedItemId(rows[idx - 1].item.id);
        }
        e.preventDefault();
      }
    } else if (Hotkey.isArrowRight(e)) {
      if (rows.length > 0) {
        if (selectedItemId === null) {
          setSelectedItemId(rows[0].item.id);
        } else {
          const idx = rows.findIndex(r => r.item.id === selectedItemId);
          if (idx >= 0 && idx < rows.length - 1) setSelectedItemId(rows[idx + 1].item.id);
        }
        e.preventDefault();
      }
    }
  }, [rows, selectedItemId]);

  return (
      <div className="relative flex h-full overflow-hidden max-w-full">
      {/* Main Content */}
      <div className="flex-1 flex flex-col overflow-hidden border-gray-200 max-w-full min-md:mx-3 min-md:mb-3 z-10">
        {/* Header - Fixed height */}
        <div className="flex-shrink-0 max-sm:mx-3">
          <BroadcastHeader
            broadcastName={broadcastName}
            isDirty={isDirty}
            isSaving={isSaving}
            onSave={handleSave}
            onNameChange={handleNameChange}
            onShowContentLibrary={() => { /* no-op: panel is triggered from '+' */ }}
            onExitClick={openExitConfirm}
            onDiscard={handleDiscard}
          />
        </div>
        {/* Main Content Area - Fills remaining space */}
        <div className="flex-1 flex min-h-0 lg:pb-0 rounded-md overflow-hidden">
          {/* Left Section - Preview and Timeline - Full width on mobile */}
          <div className="flex-1 flex flex-row min-h-0 overflow-hidden">
            <div className="flex flex-col w-full overflow-hidden">
              {/* Main Preview Area - Takes up remaining vertical space and is the only scroll/zoom responsive area */}
              <div className="flex-1 overflow-clip overscroll-contain content-center max-sm:mx-3 relative">
                {/* Efficient blurred backdrop outside ContentPreview */}
                <PreviewBackdropBlur content={previewContent} />
                <ContentPreview
                  content={previewContent}
                  videoRef={videoRef}
                  onContextMenu={(e) => {
                    if (selectedItemId !== null) {
                      const selectedRow = rows.find(r => r.item.id === selectedItemId);
                      if (selectedRow) {
                        handleContextMenu(e, selectedRow);
                      }
                    }
                  }}
                />
              </div>

              {/* Bottom Timeline - Dynamic height, always visible */}
              <motion.div
                initial={{ y: 100, opacity: 0 }}
                animate={{ y: 0, opacity: 1, transition: { duration: .8, type: "spring", ease: "easeIn" } }}
                className="flex-shrink-0 w-full h-full max-h-[200px] sm:max-h-[170px]"
              >
                <BroadcastTimeline
                  rows={rows}
                  selectedItemId={selectedItemId}
                  onSelectItem={setSelectedItemId}
                  onReorder={handleReorder}
                  onRemoveItem={handleRemoveItem}
                  onDurationChange={handleDurationChange}
                  previewContent={previewContent}
                  videoRef={videoRef}
                  onContextMenu={handleContextMenu}
                  onEmptyAreaContextMenu={handleEmptyAreaContextMenu}
                  allContent={allContent}
                  integrations={integrations}
                  onSelectIntegration={handleSelectIntegration}
                  onContentUploaded={onContentUploaded}
                  onAddContentItem={handleAddContent}
                />
              </motion.div>
            </div>
          </div>

          {/* Desktop: Right Sidebar - Removed in favor of left sidebar */}
        </div>


      {/* Toast: Save successful */}
      {showSaveToast && (
        <div className="fixed top-4 right-4 z-[60] max-w-sm">
          <div className="rounded-xl border p-3 border-green-500 bg-green-50 dark:border-green-500/30 dark:bg-green-500/15 shadow">
            <div className="flex items-start gap-2">
              <svg className="w-5 h-5 text-green-600" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path fillRule="evenodd" clipRule="evenodd" d="M12 1.9A10.1 10.1 0 1 0 22.1 12 10.112 10.112 0 0 0 12 1.9Zm3.62 8.84a.85.85 0 0 0-1.27-1.13l-3.16 3.16-1.54-1.54a.85.85 0 1 0-1.2 1.2l2.13 2.13c.33.33.86.33 1.19 0l3.86-3.82Z"/></svg>
              <div>
                <h4 className="text-sm font-semibold text-gray-800 dark:text-white/90">Saved</h4>
                <p className="text-xs text-gray-600 dark:text-gray-300">Your broadcast changes have been saved.</p>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Exit Confirmation Modal */}
      <Modal isOpen={isExitConfirmOpen} hideOverlay={true} transformOrigin={"right top"} onClose={closeExitConfirm} showCloseButton={true}
               className="max-w-fit !h-fit left-3 top-3 !rounded-md
        border-1 border-gray-300 !border-b-0 !shadow-none z-50">
        <div className="relative w-full flex flex-col text-left overflow-auto px-3 py-3 gap-2">
            <h4 className="text-xl font-semibold text-gray-800 dark:text-white">
              Discard Changes?
            </h4>
          <div className="flex flex-1 gap-2">
            <Button size="sm" variant="outline" onClick={handleExitCancel} className="rounded-sm">
              Leave without Saving
            </Button>
            <Button size="sm" onClick={handleExitConfirm} className="rounded-sm">
              Save & Leave
            </Button>
          </div>
        </div>
      </Modal>

      {/* Context Menu for Timeline Items */}
      {contextMenu.data && (
        <ContextMenu
          isOpen={contextMenu.isOpen}
          position={contextMenu.position}
          onClose={contextMenu.closeContextMenu}
          header={
            <div>
              <div className="font-medium text-gray-900 dark:text-gray-100">
                {contextMenu.data.media.name}
              </div>
              <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                Duration: {rowDurationSeconds(contextMenu.data)}s
              </div>
            </div>
          }
          items={getContextMenuItems(contextMenu.data)}
        />
      )}

      {/* Context Menu for Empty Timeline Area */}
      <ContextMenu
        isOpen={emptyAreaContextMenu.isOpen}
        position={emptyAreaContextMenu.position}
        onClose={emptyAreaContextMenu.closeContextMenu}
        items={getEmptyAreaContextMenuItems()}
      />
        </div>
      </div>
  );
}

export default function BroadcastEditor(props: BroadcastEditorProps) {
  return (
    <BroadcastClipboardProvider>
      <BroadcastEditorContent {...props} />
    </BroadcastClipboardProvider>
  );
}
