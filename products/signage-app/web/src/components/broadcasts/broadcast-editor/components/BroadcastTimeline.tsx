import React, { useRef, useCallback } from 'react';
import { useTimelineContext } from '@/context/TimelineContext';
import { BroadcastRow, mediaKind, rowDurationSeconds } from '@/components/broadcasts/types';
import type { SignageMedia } from '@/utils/lait/types';
import { Integration } from '@/components/integrations/types/integrations';
import { TimelineProvider } from '@/context/TimelineContext';
import { isVideoContent } from '@/utils/uploads/contentTypeUtils';
import {
  useDragAndDropWithContext,
  useAutoScroll,
  useTimelineScrubbing
} from '@/hooks/broadcast-editor';
import {
  ScrubBar,
  TimelineControls,
  TimelineItems
} from './index';

interface BroadcastTimelineProps {
  rows: BroadcastRow[];
  selectedItemId: string | null;
  onSelectItem: (itemId: string | null) => void;
  onReorder: (rows: BroadcastRow[]) => void;
  onDurationChange: (itemId: string, duration: number) => void;
  onRemoveItem: (itemId: string) => void;
  onContextMenu?: (e: React.MouseEvent, item: BroadcastRow) => void;
  onEmptyAreaContextMenu?: (e: React.MouseEvent) => void;
  videoRef?: React.RefObject<HTMLVideoElement | null>;
  previewContent?: unknown; // Kept for compatibility but not used
  allContent?: SignageMedia[];
  integrations?: Integration[];
  onSelectIntegration?: (integration: Integration) => void;
  onContentUploaded?: () => void;
  onAddContentItem?: (media: SignageMedia) => void;
  onShowIntegrations?: () => void;
}


function BroadcastTimelineInner({
  onContextMenu,
  onEmptyAreaContextMenu,
  videoRef,
  allContent,
  integrations,
  onSelectIntegration,
  onContentUploaded,
  onAddContentItem
}: Omit<BroadcastTimelineProps, 'rows' | 'selectedItemId' | 'onSelectItem' | 'onReorder' | 'onDurationChange' | 'onRemoveItem'>) {
  // Refs
  const timelineScrollRef = useRef<HTMLDivElement>(null);

  // Get everything from context now
  const { state, actions } = useTimelineContext();
  const { rows, totalDuration, startTimes, selectedItemId, isPlaying, currentTime, config } = state;
  const { togglePlayback, seekToTime, onDurationChange } = actions;

  // Use config from context
  const pixelsPerSecond = config.pixelsPerSecond;
  const timelineHeight = config.timelineHeight;
  const scrollTailPx = config.scrollTailPx;

  // Memoize scrub start handler
  const handleScrubStart = useCallback(() => {
    if (isPlaying) {
      togglePlayback();
    }
  }, [isPlaying, togglePlayback]);

  // During scrubbing, also ensure video preview plays on video items
  const onScrubTimeChange = useCallback((time: number) => {
    // Update timeline state and selection
    seekToTime(time);

    // During scrubbing, only seek the video time; do NOT auto-play.
    // play() calls can race with selection changes and cause "media was removed" errors.
    if (videoRef?.current) {
      const clampedTime = Math.max(0, Math.min(totalDuration, time));
      const idx = startTimes.findIndex((start, i) => {
        const end = start + (rows[i] ? rowDurationSeconds(rows[i]) : 0);
        return clampedTime >= start && clampedTime < end;
      });
      if (idx >= 0 && rows[idx] && isVideoContent(mediaKind(rows[idx].media))) {
        const itemStart = startTimes[idx];
        const tInto = clampedTime - itemStart;
        // Only adjust currentTime to keep preview in sync
        try {
          if (Math.abs((videoRef.current.currentTime || 0) - tInto) > 0.05) {
            videoRef.current.currentTime = tInto;
          }
        } catch {
          // ignore transient errors while the element/source is being swapped
        }
      }
    }
  }, [seekToTime, videoRef, totalDuration, startTimes, rows]);

  // Scrubbing controls
  const handleScrubEnd = useCallback(() => {
    // If not actively playing, ensure the video preview is paused when scrubbing stops
    if (!state.isPlaying && videoRef?.current) {
      try {
        videoRef.current.pause();
      } catch {}
    }
  }, [state.isPlaying, videoRef]);

  const {
    isScrubbing,
    handleScrubStart: beginScrub
  } = useTimelineScrubbing({
    totalDuration,
    pixelsPerSecond,
    scrollContainerRef: timelineScrollRef,
    onTimeChange: onScrubTimeChange,
    onScrubStart: handleScrubStart,
    onScrubEnd: handleScrubEnd
  });

  // Drag and drop - now uses context
  const {
    dragState,
    handleDragStart,
    handleDragEnd,
    handleDragOver,
    handleDrop
  } = useDragAndDropWithContext(timelineScrollRef);

  // Auto-scroll during drag
  const {
    stopAutoScroll,
    checkAutoScroll
  } = useAutoScroll({
    scrollContainerRef: timelineScrollRef,
    isDragging: dragState.isDragging
  });

  // Get selected row for controls

  return (
    <div className="flex flex-col h-full">
      {/* Controls Bar */}
      <div className="flex relative items-center justify-center gap-4 px-1 py-2">
        <TimelineControls
          isPlaying={isPlaying}
          currentTime={currentTime}
          totalDuration={totalDuration}
          onPlayToggle={togglePlayback}
          onSeek={seekToTime}
        />
      </div>

      {/* Timeline Container */}
      <div
        className="relative w-screen px-6 bg-gray-100 rounded-md"
        style={{ height: `${timelineHeight}px` }}
      >
        {/* Single scrollable container for both time markers and items */}
        <div
          ref={timelineScrollRef}
          className="h-full w-full overflow-x-auto overflow-y-hidden !rounded-md overscroll-x-contain"
          style={{
            touchAction: 'pan-x',
            WebkitOverflowScrolling: 'touch',
            willChange: 'scroll-position',
            contain: 'paint'
          }}
          data-timeline-scroll
          onWheel={(e) => {
            // Map vertical wheel to horizontal scroll for better UX (desktop)
            const el = timelineScrollRef.current;
            if (!el) return;
            const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
            if (delta !== 0) {
              e.preventDefault();
              el.scrollLeft += delta;
            }
          }}
          onContextMenu={(e) => {
            const target = e.target as HTMLElement;
            if (!target.closest('[data-timeline-index]') && !target.closest('.timeline-item')) {
              e.preventDefault();
              onEmptyAreaContextMenu?.(e);
            }
          }}
          onDragLeave={(e) => {
            if (e.currentTarget === e.target) {
              stopAutoScroll();
            }
          }}
        >
          <div
            className="relative h-full"
            style={{ width: `${(totalDuration * pixelsPerSecond) + scrollTailPx}px` }}
          >
            {/* Scrub bar indicator - spans full height */}
            {totalDuration > 0 && (
              <ScrubBar
                currentTime={currentTime}
                totalDuration={totalDuration}
                pixelsPerSecond={pixelsPerSecond}
                isScrubbing={isScrubbing}
                onScrubStart={beginScrub}
                height={timelineHeight}
              />
            )}

            {/* Time markers at the top */}
            <div className="absolute top-0 left-0 right-0 h-10 border-t border-gray-200 dark:border-gray-700">
              {startTimes.map((time, index) => (
                <div
                  key={`marker-${index}`}
                  className="absolute top-0 flex flex-col items-center pt-1"
                  style={{ left: `${(time * pixelsPerSecond) + 3}px` }}
                >
                  <div className="w-px h-2 bg-gray-400 dark:bg-gray-600 -mt-1" />
                  <span className="text-[10px] text-gray-500 dark:text-gray-400 font-mono">
                    {Math.floor(time)}s
                  </span>
                </div>
              ))}

              {/* End marker */}
              {totalDuration > 0 && (
                <div
                  className="absolute top-0 flex flex-col items-center pt-1"
                  style={{ left: `${(totalDuration * pixelsPerSecond) + 3}px` }}
                >
                  <div className="w-px h-2 bg-gray-400 dark:bg-gray-600 -mt-1" />
                  <span className="text-[10px] text-gray-500 dark:text-gray-400 font-mono">
                    {Math.floor(totalDuration)}s
                  </span>
                </div>
              )}
            </div>

            {/* Timeline Items - positioned below time markers */}
            <div className={`absolute left-0 right-0 px-4 ${
              rows.length === 0 ? 'flex justify-center items-center' : ''
            }`}
            style={{ top: '48px', bottom: '16px' }}>
              <TimelineItems
                rows={rows}
                startTimes={startTimes}
                pixelsPerSecond={pixelsPerSecond}
                selectedItemId={selectedItemId}
                draggedItemId={dragState.draggedItemId}
                dropIndex={dragState.dropIndex}
                onSelectItem={actions.onSelectItem}
                onDurationChange={onDurationChange}
                onDragStart={handleDragStart}
                onDragEnd={handleDragEnd}
                onDragOver={(e, idx) => { handleDragOver(e, idx); checkAutoScroll(e.clientX); }}
                onDrop={handleDrop}
                onContextMenu={onContextMenu}
                allContent={allContent}
                integrations={integrations}
                onSelectIntegration={onSelectIntegration}
                onContentUploaded={onContentUploaded}
                onAddContentItem={onAddContentItem}
              />
            </div>
          </div>
        </div>
      </div>

    </div>
  );
}

// Main exported component with context provider
export default function BroadcastTimeline(props: BroadcastTimelineProps) {
  return (
    <TimelineProvider
      rows={props.rows}
      onReorder={props.onReorder}
      onDurationChange={props.onDurationChange}
      onRemoveItem={props.onRemoveItem}
      onSelectionChange={props.onSelectItem}
      onContextMenu={props.onContextMenu}
      onEmptyAreaContextMenu={props.onEmptyAreaContextMenu}
      videoRef={props.videoRef}
      // Config uses defaults from TimelineContext
    >
      <BroadcastTimelineInner {...props} />
    </TimelineProvider>
  );
}
