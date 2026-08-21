import React, { useState, useEffect, useCallback, useMemo } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import {
  ScreenGridItem,
  ScreenDetailsModal,
  AssignBroadcastModal,
  AddScreenButton,
  NetworkNameModal,
  ScreenNameModal
} from "@/components/screens";
import { GridListView } from "@/components/ui/GridListView";
import { ConfirmationModal } from "@/components/ui/ConfirmationModal";
import { ContextMenu, ContextMenuItem } from "@/components/ui/ContextMenu";
import { useModal } from "@/hooks/useModal";
import { useContextMenu } from "@/hooks/useContextMenu";
import {
  ExternalLink,
  Info,
  Monitor,
  Trash2,
  Edit,
  Network as NetworkIcon,
  ArrowRightLeft,
  X
} from "lucide-react";
import { BroadcastTower } from "../../../../../public/images/icons/theme-icons";
import PageHeader from "@/components/common/PageHeader";
import { PageSearchBar } from "@/components/common/PageSearchBar";

import {
  fetchScreens,
  createScreen,
  saveScreen,
  deleteScreen,
  assignProgramToScreen,
  setScreenGroup
} from "@/utils/screens/api";
import {
  fetchGroups,
  createGroup,
  saveGroup,
  deleteGroup,
  assignProgramToGroup
} from "@/utils/networks/api";
import { fetchPrograms } from "@/utils/broadcasts/api";
import type { SignageGroup, SignageProgram, SignageScreen } from "@/utils/lait/types";
import { ContentSortingBar, SortingState } from "@/components/content";

export const ScreenList: React.FC = () => {
  const navigate = useNavigate();
  const { q: searchQuery } = useSearch({ strict: false }) as { q?: string };
  const [localSearch, setLocalSearch] = useState(searchQuery || "");
  const [screens, setScreens] = useState<SignageScreen[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [isDesktop, setIsDesktop] = useState(false);

  // Sync local search when URL param changes externally (e.g. from navbar)
  useEffect(() => {
    setLocalSearch(searchQuery || "");
  }, [searchQuery]);

  const { isOpen: isDeleteConfirmOpen, openModal: openDeleteConfirm, closeModal: closeDeleteConfirm } = useModal();
  const { isOpen: isDetailOpen, openModal: openDetail, closeModal: closeDetail } = useModal();
  const { isOpen: isAssignBroadcastOpen, openModal: openAssignBroadcast, closeModal: closeAssignBroadcast } = useModal();
  const { isOpen: isNetworkNameOpen, openModal: openNetworkName, closeModal: closeNetworkName } = useModal();
  const { isOpen: isScreenNameOpen, openModal: openScreenName, closeModal: closeScreenName } = useModal();
  const [selectedScreen, setSelectedScreen] = useState<SignageScreen | null>(null);
  const [detailViewItemId, setDetailViewItemId] = useState<string | null>(null);
  const [isCreatingNetwork, setIsCreatingNetwork] = useState(false);
  const [isCreatingScreen, setIsCreatingScreen] = useState(false);

  // Network (group) state
  const [networks, setNetworks] = useState<SignageGroup[]>([]);
  const [programs, setPrograms] = useState<SignageProgram[]>([]);
  const [activeNetworkFilter, setActiveNetworkFilter] = useState<string | "all" | "ungrouped">("all");
  const { isOpen: isMoveToNetworkOpen, openModal: openMoveToNetwork, closeModal: closeMoveToNetwork } = useModal();
  const { isOpen: isNetworkBroadcastOpen, openModal: openNetworkBroadcast, closeModal: closeNetworkBroadcast } = useModal();
  const { isOpen: isNetworkDeleteOpen, openModal: openNetworkDelete, closeModal: closeNetworkDelete } = useModal();
  const { isOpen: isNetworkRenameOpen, openModal: openNetworkRename, closeModal: closeNetworkRename } = useModal();
  const [selectedNetworkForAction, setSelectedNetworkForAction] = useState<SignageGroup | null>(null);
  const [moveToNetworkScreenId, setMoveToNetworkScreenId] = useState<string | null>(null);
  const networkContextMenu = useContextMenu<SignageGroup>();

  const [sortingState, setSortingState] = useState<SortingState>({
    nameSort: "asc",
    dimensionSort: null,
    typeFilter: null
  });

  // Context menu
  const contextMenu = useContextMenu<SignageScreen>();

  // Check if desktop on mount and window resize
  useEffect(() => {
    const checkDesktop = () => {
      const width = window.innerWidth;
      setIsDesktop(width >= 1024);
    };

    checkDesktop();
    window.addEventListener('resize', checkDesktop);
    return () => window.removeEventListener('resize', checkDesktop);
  }, []);

  const loadScreens = useCallback(async () => {
    setLoading(true);
    setError("");

    try {
      const [screensData, networksData, programsData] = await Promise.all([
        fetchScreens(),
        fetchGroups(),
        fetchPrograms(),
      ]);
      setScreens(screensData);
      setNetworks(networksData);
      setPrograms(programsData);
    } catch (err) {
      console.error("Error fetching screens:", err);
      setError((err as Error).message || "Failed to fetch screens");
      setScreens([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadScreens();
  }, [loadScreens]);

  const programNames = useMemo(
    () => new Map(programs.map((p) => [p.id, p.name])),
    [programs]
  );
  const networkNames = useMemo(
    () => new Map(networks.map((n) => [n.id, n.name])),
    [networks]
  );

  // The ladder's inputs, rendered honestly: an active override, else the
  // screen's own standing choice. Resolution stays with the engine.
  const activeOverride = (screen: SignageScreen) =>
    screen.intent.over && screen.intent.over.until_unix_ms > Date.now()
      ? screen.intent.over
      : null;

  const directProgramName = (screen: SignageScreen) => {
    const member = screen.intent.base?.member;
    return member ? programNames.get(member) ?? null : null;
  };

  const handleCardClick = (screen: SignageScreen) => {
    if (window.event) {
      window.event.stopPropagation();
    }

    if (detailViewItemId === screen.id) {
      handleDetailClose();
    } else {
      setSelectedScreen(screen);
      setDetailViewItemId(screen.id);
      openDetail();
    }
  };

  const handleDetailClose = () => {
    setDetailViewItemId(null);
    closeDetail();
  };

  const handleViewScreen = (screenId: string) => {
    navigate({ to: `/screen-list/${screenId}` });
  };

  const handleOpenInNewTab = (screenId: string) => {
    window.open(`/screen-list/${screenId}`, '_blank');
  };

  const handleAssignBroadcast = (screenId: string) => {
    const screen = screens.find(s => s.id === screenId);
    if (screen) {
      setSelectedScreen(screen);
      openAssignBroadcast();
    }
  };

  const handleDeleteScreen = async (id: string) => {
    try {
      await deleteScreen(id);
      handleDetailClose();
      await loadScreens();
    } catch (err) {
      setError((err as Error).message || "Failed to delete screen");
    }
  };

  const handleDeleteConfirm = async () => {
    if (selectedScreen) {
      await handleDeleteScreen(selectedScreen.id);
    }
    closeDeleteConfirm();
  };

  const updateScreenName = async (id: string, name: string) => {
    try {
      const screen = screens.find(s => s.id === id);
      if (!screen) return;
      await saveScreen({ ...screen, name });

      if (selectedScreen && selectedScreen.id === id) {
        setSelectedScreen({ ...selectedScreen, name });
      }

      loadScreens();
    } catch (err) {
      setError((err as Error).message || "Failed to update screen name");
    }
  };

  const handleAssignProgramToScreen = async (screenId: string, programId: string) => {
    await assignProgramToScreen(screenId, programId);
    await loadScreens();
  };

  const handleCreateNetwork = async (networkName: string) => {
    if (isCreatingNetwork) return;
    setIsCreatingNetwork(true);
    try {
      await createGroup(networkName);
      closeNetworkName();
      await loadScreens();
    } catch (err) {
      setError((err as Error).message || "Failed to create network");
    } finally {
      setIsCreatingNetwork(false);
    }
  };

  const handleRenameNetwork = async (newName: string) => {
    if (!selectedNetworkForAction) return;
    try {
      await saveGroup({ ...selectedNetworkForAction, name: newName });
      closeNetworkRename();
      setSelectedNetworkForAction(null);
      await loadScreens();
    } catch (err) {
      setError((err as Error).message || "Failed to rename network");
    }
  };

  const handleDeleteNetwork = async () => {
    if (!selectedNetworkForAction) return;
    try {
      await deleteGroup(selectedNetworkForAction.id);
      closeNetworkDelete();
      setSelectedNetworkForAction(null);
      if (activeNetworkFilter === selectedNetworkForAction.id) {
        setActiveNetworkFilter("all");
      }
      await loadScreens();
    } catch (err) {
      setError((err as Error).message || "Failed to delete network");
    }
  };

  const handleMoveToNetwork = async (networkId: string | null) => {
    if (moveToNetworkScreenId === null) return;
    try {
      await setScreenGroup(moveToNetworkScreenId, networkId);
      closeMoveToNetwork();
      setMoveToNetworkScreenId(null);
      await loadScreens();
    } catch (err) {
      setError((err as Error).message || "Failed to move screen");
    }
  };

  const handleAssignProgramToNetwork = async (targetId: string, programId: string) => {
    await assignProgramToGroup(targetId, programId);
    await loadScreens();
  };

  const handleCreateScreen = async (screenName: string) => {
    if (isCreatingScreen) return; // Prevent double-clicks

    setIsCreatingScreen(true);

    try {
      await createScreen(screenName);
      closeScreenName();
      await loadScreens();
    } catch (err) {
      console.error("Error creating screen:", err);
      setError((err as Error).message || "Failed to create screen");
      // Keep modal open on error so user can try again
    } finally {
      setIsCreatingScreen(false);
    }
  };

  // Filter screens by active network tab and search query
  const filteredScreens = screens
    .filter(s => {
      // Network filter
      if (activeNetworkFilter !== "all") {
        if (activeNetworkFilter === "ungrouped" && s.group) return false;
        if (activeNetworkFilter !== "ungrouped" && s.group !== activeNetworkFilter) return false;
      }
      // Search query filter
      if (localSearch) {
        const q = localSearch.toLowerCase();
        const nameMatch = s.name.toLowerCase().includes(q);
        const programMatch = directProgramName(s)?.toLowerCase().includes(q);
        if (!nameMatch && !programMatch) return false;
      }
      return true;
    })
    .sort((a, b) =>
      sortingState.nameSort === "desc"
        ? b.name.localeCompare(a.name)
        : a.name.localeCompare(b.name)
    );

  if (error && !loading) return <p className="text-red-500">{error}</p>;
  if (loading) return <p>Loading…</p>;

  const getNetworkContextMenuItems = (network: SignageGroup): ContextMenuItem[] => [
    {
      label: "Assign Broadcast",
      icon: <BroadcastTower className="w-4 h-4" />,
      onClick: () => {
        setSelectedNetworkForAction(network);
        openNetworkBroadcast();
      },
    },
    {
      label: "Rename",
      icon: <Edit className="w-4 h-4" />,
      onClick: () => {
        setSelectedNetworkForAction(network);
        openNetworkRename();
      },
    },
    { divider: true, label: "" },
    {
      label: "Delete",
      icon: <Trash2 className="w-4 h-4" />,
      onClick: () => {
        setSelectedNetworkForAction(network);
        openNetworkDelete();
      },
    },
  ];

  const getContextMenuItems = (screen: SignageScreen): ContextMenuItem[] => [
    {
      label: "View screen",
      icon: <Monitor className="w-4 h-4" />,
      onClick: () => handleViewScreen(screen.id),
    },
    {
      label: "Open in new tab",
      icon: <ExternalLink className="w-4 h-4" />,
      onClick: () => handleOpenInNewTab(screen.id),
    },
    {
      label: "Details",
      icon: <Info className="w-4 h-4" />,
      onClick: () => handleCardClick(screen),
    },
    { divider: true, label: "" },
    {
      label: "Assign a broadcast",
      icon: <BroadcastTower className="w-4 h-4" />,
      onClick: () => handleAssignBroadcast(screen.id),
    },
    {
      label: "Move to Network...",
      icon: <ArrowRightLeft className="w-4 h-4" />,
      onClick: () => {
        setMoveToNetworkScreenId(screen.id);
        openMoveToNetwork();
      },
    },
    { divider: true, label: "" },
    {
      label: "Delete",
      icon: <Trash2 className="w-4 h-4" />,
      onClick: () => {
        setSelectedScreen(screen);
        openDeleteConfirm();
      },
    },
  ];

  const emptyState = screens.length === 0 && (
    <div className="flex flex-col items-center justify-self-center justify-center flex-1 text-center">
      <p className="text-xl font-semibold text-gray-600 dark:text-gray-400 mb-4">
        Add your first screen
      </p>
      <AddScreenButton onOpenModal={openScreenName} className="h-10" />
    </div>
  );

  return (
    <div className={"w-full min-w-0 relative justify-self-center pb-50 overflow-x-hidden"}>
      <PageHeader pageTitle={"Screens"}>
        <div className="flex flex-row gap-2">
          <AddScreenButton onOpenModal={openScreenName} />
          <button onClick={openNetworkName}
            className="px-3 py-2 text-sm font-medium bg-white text-gray-700 shadow-theme-xs border border-gray-300 hover:bg-gray-50
              flex items-center gap-1.5 rounded-lg whitespace-nowrap transition">
            <NetworkIcon className="size-4" viewBox="0 0 28 28"/>
            New Network
          </button>
        </div>
      </PageHeader>

      {/* Network filter — dropdown on mobile, tabs on sm+ */}
      <div className="sm:hidden pt-4 pb-2">
        <select
          value={activeNetworkFilter}
          onChange={(e) => setActiveNetworkFilter(e.target.value)}
          className="w-full rounded-md border border-gray-200 bg-white py-2 px-3 text-sm text-gray-700 focus:border-brand-300 focus:outline-none focus:ring-1 focus:ring-brand-300 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-300"
        >
          <option value="all">All ({screens.length})</option>
          {networks.map(net => (
            <option key={net.id} value={net.id}>
              {net.name} ({screens.filter(s => s.group === net.id).length})
            </option>
          ))}
          <option value="ungrouped">Ungrouped ({screens.filter(s => !s.group).length})</option>
        </select>
      </div>
      <div className="hidden sm:flex w-full items-center gap-2 pt-4 pb-2 overflow-x-auto">
        <button
          onClick={() => setActiveNetworkFilter("all")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md whitespace-nowrap transition-colors
            ${activeNetworkFilter === "all"
              ? "bg-brand-500 text-white"
              : "bg-gray-100 dark:bg-gray-800 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-700"}`}
        >
          All ({screens.length})
        </button>
        {networks.map(net => (
          <button
            key={net.id}
            onClick={() => setActiveNetworkFilter(net.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              networkContextMenu.openContextMenu(e, net);
            }}
            className={`px-3 py-1.5 text-xs font-medium rounded-md whitespace-nowrap transition-colors
              ${activeNetworkFilter === net.id
                ? "bg-brand-500 text-white"
                : "bg-gray-100 dark:bg-gray-800 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-700"}`}
          >
            {net.name} ({screens.filter(s => s.group === net.id).length})
          </button>
        ))}
        <button
          onClick={() => setActiveNetworkFilter("ungrouped")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md whitespace-nowrap transition-colors
            ${activeNetworkFilter === "ungrouped"
              ? "bg-brand-500 text-white"
              : "bg-gray-100 dark:bg-gray-800 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-700"}`}
        >
          Ungrouped ({screens.filter(s => !s.group).length})
        </button>
      </div>

      <div className="flex w-full items-center justify-between py-2 border-gray-300">
        <p className="flex-1 text-2xl sm:text-xl text-left font-semibold mb-2 mt-2">
          {activeNetworkFilter === "all" ? "All Screens" :
           activeNetworkFilter === "ungrouped" ? "Ungrouped Screens" :
           networks.find(n => n.id === activeNetworkFilter)?.name ?? "Screens"}
        </p>
      </div>
      <div className="flex flex-wrap gap-2 items-center justify-between rounded-lg pb-4 text-[10px]">
        <div className="min-w-0 flex-1 sm:flex-none">
          <PageSearchBar
            value={localSearch}
            onChange={setLocalSearch}
            placeholder="Filter screens..."
          />
        </div>
        <ContentSortingBar
          sortingState={sortingState}
          onSortingChange={setSortingState}
          availableTypes={[]}
          hideTypeFilter={true}
          hideDimensionSort={true}
        />
      </div>
      <div
        className={`relative flex`}
      >
        {filteredScreens.length === 0 ? (
          emptyState
        ) : (
          <GridListView
            items={filteredScreens}
            viewMode={"grid"}
            isDesktop={isDesktop}
            gridClassName={`grid gap-3 grid-cols-1 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-4 flex-1`}
            renderGridItem={(screen) => {
              const override = activeOverride(screen);
              return (
                <ScreenGridItem
                  key={screen.id}
                  name={screen.name}
                  groupName={screen.group ? networkNames.get(screen.group) ?? null : null}
                  programName={directProgramName(screen)}
                  overrideProgramName={
                    override ? programNames.get(override.choice.member) ?? "Unknown program" : null
                  }
                  overrideUntilMs={override ? override.until_unix_ms : null}
                  onClick={() => handleViewScreen(screen.id)}
                  onContextMenu={(e) => contextMenu.openContextMenu(e, screen)}
                />
              );
            }}
          />
        )}
      </div>

      {/* Confirmation Modal for Delete */}
      <ConfirmationModal
        isOpen={isDeleteConfirmOpen}
        onClose={closeDeleteConfirm}
        showCloseButton={false}
        onConfirm={handleDeleteConfirm}
        title="Delete Screen?"
        message={`Are you sure you want to delete "${selectedScreen?.name}"? This action cannot be undone.`}
        confirmText="Delete"
        variant="danger"
      />

      {/* Screen Details Modal */}
      <ScreenDetailsModal
        isOpen={isDetailOpen}
        screen={selectedScreen}
        groupName={
          selectedScreen?.group ? networkNames.get(selectedScreen.group) ?? null : null
        }
        programName={selectedScreen ? directProgramName(selectedScreen) : null}
        onClose={handleDetailClose}
        onDelete={handleDeleteScreen}
        onUpdate={updateScreenName}
        onViewScreen={handleViewScreen}
        onAssignBroadcast={handleAssignBroadcast}
      />

      {/* Assign Broadcast Modal */}
      {selectedScreen && (
        <AssignBroadcastModal
          isOpen={isAssignBroadcastOpen}
          onClose={closeAssignBroadcast}
          screenId={selectedScreen.id}
          screenName={selectedScreen.name}
          currentBroadcastId={selectedScreen.intent.base?.member}
          onAssign={handleAssignProgramToScreen}
        />
      )}

      {/* Context Menu */}
      {contextMenu.data && (
        <ContextMenu
          isOpen={contextMenu.isOpen}
          position={contextMenu.position}
          onClose={contextMenu.closeContextMenu}
          header={
            <div>
              <div className="font-medium text-gray-900 dark:text-gray-100">{contextMenu.data.name}</div>
              <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                {contextMenu.data.group
                  ? networkNames.get(contextMenu.data.group) ?? "Unknown network"
                  : "No network"}
              </div>
            </div>
          }
          items={getContextMenuItems(contextMenu.data)}
        />
      )}

      {/* Network Name Modal */}
      <NetworkNameModal
        isOpen={isNetworkNameOpen}
        onClose={closeNetworkName}
        onConfirm={handleCreateNetwork}
        isCreating={isCreatingNetwork}
      />

      {/* Screen Name Modal */}
      <ScreenNameModal
        isOpen={isScreenNameOpen}
        onClose={closeScreenName}
        onConfirm={handleCreateScreen}
        isCreating={isCreatingScreen}
      />

      {/* Network Tab Context Menu */}
      {networkContextMenu.data && (
        <ContextMenu
          isOpen={networkContextMenu.isOpen}
          position={networkContextMenu.position}
          onClose={networkContextMenu.closeContextMenu}
          header={
            <div className="font-medium text-gray-900 dark:text-gray-100">
              {networkContextMenu.data.name}
            </div>
          }
          items={getNetworkContextMenuItems(networkContextMenu.data)}
        />
      )}

      {/* Move to Network Picker Modal */}
      {isMoveToNetworkOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30" onClick={closeMoveToNetwork}>
          <div className="bg-white dark:bg-gray-800 rounded-lg shadow-lg border border-gray-200 dark:border-gray-700 w-[calc(100vw-2rem)] sm:w-72 max-h-80 overflow-hidden" onClick={e => e.stopPropagation()}>
            <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-700 flex items-center justify-between">
              <span className="text-sm font-semibold text-gray-900 dark:text-white">Move to Network</span>
              <button onClick={closeMoveToNetwork} className="text-gray-400 hover:text-gray-600">
                <X className="w-4 h-4" />
              </button>
            </div>
            <div className="overflow-y-auto max-h-60 divide-y divide-gray-100 dark:divide-gray-700">
              {networks.map(net => {
                const currentScreen = screens.find(s => s.id === moveToNetworkScreenId);
                const isCurrentNetwork = currentScreen?.group === net.id;
                return (
                  <button
                    key={net.id}
                    disabled={isCurrentNetwork}
                    onClick={() => handleMoveToNetwork(net.id)}
                    className={`w-full px-4 py-2.5 text-left text-sm flex items-center gap-2 transition-colors
                      ${isCurrentNetwork
                        ? "text-gray-400 cursor-not-allowed bg-gray-50 dark:bg-gray-900"
                        : "text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"}`}
                  >
                    <NetworkIcon className="w-4 h-4" />
                    <span>{net.name}</span>
                    {isCurrentNetwork && <span className="ml-auto text-xs text-gray-400">Current</span>}
                  </button>
                );
              })}
              {networks.length > 0 && (
                <button
                  onClick={() => handleMoveToNetwork(null)}
                  className="w-full px-4 py-2.5 text-left text-sm flex items-center gap-2 text-red-600 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors"
                >
                  <X className="w-4 h-4" />
                  <span>Remove from Network</span>
                </button>
              )}
              {networks.length === 0 && (
                <div className="px-4 py-6 text-center text-sm text-gray-500">No networks created yet</div>
              )}
            </div>
          </div>
        </div>
      )}

      {/* Network Rename Modal */}
      <NetworkNameModal
        isOpen={isNetworkRenameOpen}
        onClose={() => { closeNetworkRename(); setSelectedNetworkForAction(null); }}
        onConfirm={handleRenameNetwork}
        isCreating={false}
      />

      {/* Network Delete Confirmation */}
      <ConfirmationModal
        isOpen={isNetworkDeleteOpen}
        onClose={() => { closeNetworkDelete(); setSelectedNetworkForAction(null); }}
        showCloseButton={false}
        onConfirm={handleDeleteNetwork}
        title="Delete Network?"
        message={`Are you sure you want to delete "${selectedNetworkForAction?.name}"? Screens in this network will become ungrouped.`}
        confirmText="Delete"
        variant="danger"
      />

      {/* Assign Broadcast to Network Modal */}
      {selectedNetworkForAction && (
        <AssignBroadcastModal
          isOpen={isNetworkBroadcastOpen}
          onClose={() => { closeNetworkBroadcast(); setSelectedNetworkForAction(null); }}
          screenId={selectedNetworkForAction.id}
          screenName={selectedNetworkForAction.name}
          currentBroadcastId={selectedNetworkForAction.intent.base?.member}
          onAssign={handleAssignProgramToNetwork}
        />
      )}
    </div>
  );
};
