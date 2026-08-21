import React, { createContext, useContext, useMemo, useState, useCallback } from 'react';
import { usePlaybackControls } from '@/hooks/broadcast-editor';
import { BroadcastRow, rowDurationSeconds } from '@/components/broadcasts/types';

interface TimelineConfig {
  pixelsPerSecond: number;
  timelineHeight: number;
  scrollTailPx: number;
  itemHeight: number;
  showTimeMarkers: boolean;
  showDropZones: boolean;
  enableKeyboardShortcuts: boolean;
}

interface TimelineUIState {
  isScrubbing: boolean;
  isDragging: boolean;
  draggedItemId: string | null;
  dropIndex: number | null;
}

interface TimelineState {
  // Configuration
  config: TimelineConfig;

  // Timeline data
  rows: BroadcastRow[];
  totalDuration: number;
  startTimes: number[];

  // Selection state
  selectedItemId: string | null;

  // Playback state
  isPlaying: boolean;
  currentTime: number;

  // UI state
  ui: TimelineUIState;
}

interface TimelineActions {
  // Item management
  onSelectItem: (itemId: string | null) => void;
  onReorder: (rows: BroadcastRow[]) => void;
  onDurationChange: (itemId: string, duration: number) => void;
  onRemoveItem: (itemId: string) => void;
  onAddContent?: () => void;

  // Playback controls
  togglePlayback: () => void;
  seekToTime: (time: number) => void;

  // UI state setters
  setIsScrubbing: (isScrubbing: boolean) => void;
  setIsDragging: (isDragging: boolean) => void;
  setDraggedItemId: (itemId: string | null) => void;
  setDropIndex: (index: number | null) => void;

  // Context menu
  onContextMenu?: (e: React.MouseEvent, item: BroadcastRow) => void;
  onEmptyAreaContextMenu?: (e: React.MouseEvent) => void;
}

interface TimelineContextValue {
  state: TimelineState;
  actions: TimelineActions;
}

const TimelineContext = createContext<TimelineContextValue | undefined>(undefined);

export const useTimelineContext = () => {
  const context = useContext(TimelineContext);
  if (!context) {
    throw new Error('useTimelineContext must be used within TimelineProvider');
  }
  return context;
};

interface TimelineProviderProps {
  children: React.ReactNode;

  // Current rows - controlled from parent
  rows: BroadcastRow[];

  // Callbacks for operations
  onReorder: (rows: BroadcastRow[]) => void;
  onDurationChange: (itemId: string, duration: number) => void;
  onRemoveItem: (itemId: string) => void;

  // Optional props
  config?: Partial<TimelineConfig>;
  videoRef?: React.RefObject<HTMLVideoElement | null>;
  onSelectionChange?: (itemId: string | null) => void;
  onAddContent?: () => void;
  onContextMenu?: (e: React.MouseEvent, item: BroadcastRow) => void;
  onEmptyAreaContextMenu?: (e: React.MouseEvent) => void;
}

const DEFAULT_CONFIG: TimelineConfig = {
  pixelsPerSecond: 7,
  timelineHeight: 200,
  scrollTailPx: 120,
  itemHeight: 80,
  showTimeMarkers: true,
  showDropZones: true,
  enableKeyboardShortcuts: true
};

export function TimelineProvider({
  children,
  rows,
  onReorder,
  onDurationChange,
  onRemoveItem,
  config: userConfig,
  videoRef,
  onSelectionChange,
  onAddContent,
  onContextMenu,
  onEmptyAreaContextMenu
}: TimelineProviderProps) {
  // Don't manage rows internally - they come from parent
  // This makes the component fully controlled

  // UI state
  const [isScrubbing, setIsScrubbing] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const [draggedItemId, setDraggedItemId] = useState<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);

  // Merge user config with defaults
  const config = useMemo(() => ({
    ...DEFAULT_CONFIG,
    ...userConfig
  }), [userConfig]);

  // Calculate derived state
  const totalDuration = useMemo(() =>
    rows.reduce((sum, row) => sum + rowDurationSeconds(row), 0),
    [rows]
  );

  const startTimes = useMemo(() =>
    rows.reduce((acc, row, index) => {
      if (index === 0) {
        acc.push(0);
      } else {
        acc.push(acc[index - 1] + rowDurationSeconds(rows[index - 1]));
      }
      return acc;
    }, [] as number[]),
    [rows]
  );

  // Local selection state
  const [selectedItemId, setSelectedItemId] = useState<string | null>(null);

  // Wire onSelect to update local state and inform parent
  const handleSelectItem = useCallback((itemId: string | null) => {
    setSelectedItemId(itemId);
    onSelectionChange?.(itemId);
  }, [onSelectionChange]);

  // Create playback controls inside provider
  const {
    isPlaying,
    currentTime,
    togglePlayback: hookToggle,
    seekToTime: hookSeek
  } = usePlaybackControls({
    rows,
    startTimes,
    totalDuration,
    onSelectItem: (id: string) => handleSelectItem(id),
    videoRef
  });

  // Pass through to parent callbacks
  const handleReorder = onReorder;
  const handleDurationChange = onDurationChange;
  const handleRemoveItem = onRemoveItem;

  // Create UI state object separately to avoid recreating
  const uiState = useMemo(() => ({
    isScrubbing,
    isDragging,
    draggedItemId,
    dropIndex
  }), [isScrubbing, isDragging, draggedItemId, dropIndex]);

  // Create context value with stable references
  const value: TimelineContextValue = useMemo(() => ({
    state: {
      config,
      rows,
      totalDuration,
      startTimes,
      selectedItemId,
      isPlaying,
      currentTime,
      ui: uiState
    },
    actions: {
      onSelectItem: handleSelectItem,
      onReorder: handleReorder,
      onDurationChange: handleDurationChange,
      onRemoveItem: handleRemoveItem,
      onAddContent,
      togglePlayback: hookToggle,
      seekToTime: hookSeek,
      setIsScrubbing,
      setIsDragging,
      setDraggedItemId,
      setDropIndex,
      onContextMenu,
      onEmptyAreaContextMenu
    }
  }), [
    config,
    rows,
    totalDuration,
    startTimes,
    selectedItemId,
    isPlaying,
    currentTime,
    uiState,
    handleReorder,
    handleRemoveItem,
    handleDurationChange,
    handleSelectItem,
    onAddContent,
    hookToggle,
    hookSeek,
    onContextMenu,
    onEmptyAreaContextMenu
  ]);

  return (
    <TimelineContext.Provider value={value}>
      {children}
    </TimelineContext.Provider>
  );
}
