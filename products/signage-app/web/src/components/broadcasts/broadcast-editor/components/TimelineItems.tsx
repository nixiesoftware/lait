import React from 'react';
import { BroadcastRow, rowDurationSeconds } from '@/components/broadcasts/types';
import type { SignageMedia } from '@/utils/lait/types';
import { Integration } from '@/components/integrations/types/integrations';
import TimelineItem from '@/components/broadcasts/broadcast-editor/components/TimelineItem';
import AddContentButton from '@/components/broadcasts/broadcast-editor/components/AddContentButton';

interface TimelineItemsProps {
  rows: BroadcastRow[];
  startTimes: number[];
  pixelsPerSecond: number;
  selectedItemId: string | null;
  draggedItemId: string | null;
  dropIndex: number | null;
  onSelectItem: (itemId: string) => void;
  onDurationChange: (itemId: string, duration: number) => void;
  onDragStart: (e: React.DragEvent, itemId: string) => void;
  onDragEnd: () => void;
  onDragOver: (e: React.DragEvent, index: number) => void;
  onDrop: (e: React.DragEvent) => void;
  onContextMenu?: (e: React.MouseEvent, item: BroadcastRow) => void;
  onAddContentItem?: (media: SignageMedia) => void;
  showAddButton?: boolean;
  // Library props for the '+' panel
  allContent?: SignageMedia[];
  integrations?: Integration[];
  onSelectIntegration?: (integration: Integration) => void;
  onContentUploaded?: () => void;
}

export function TimelineItems({
  rows,
  startTimes,
  pixelsPerSecond,
  selectedItemId,
  draggedItemId,
  dropIndex,
  onSelectItem,
  onDurationChange,
  onDragStart,
  onDragEnd,
  onDragOver,
  onDrop,
  onContextMenu,
  onAddContentItem,
  showAddButton = true,
  allContent,
  integrations,
  onSelectIntegration,
  onContentUploaded
}: TimelineItemsProps) {
  const totalDuration = rows.reduce((sum, row) => sum + rowDurationSeconds(row), 0);

  return (
    <div
      className="relative"
      style={{
        width: `${(totalDuration * pixelsPerSecond) + 160}px`,
        height: '100%'
      }}
    >
      {/* Timeline Items */}
      {rows.map((row, index) => {
        const isDraggedItem = draggedItemId === row.item.id;
        const draggedIndex = draggedItemId ?
          rows.findIndex(r => r.item.id === draggedItemId) : -1;

        // Calculate drop indicators
        let shouldShowDropIndicatorBefore = false;
        let shouldShowDropIndicatorAfter = false;

        // Determine 5px "scoot" around the drop indicator for UX feedback
        let scootX = 0;
        if (dropIndex !== null && draggedItemId !== null && draggedIndex !== -1 && !isDraggedItem) {
          const showBefore = dropIndex === index;      // inserting before this item
          const showAfter = dropIndex === index + 1;   // inserting after this item

          if (showBefore) {
            // This item is to the right of the indicator
            scootX = 5;
          } else if (showAfter) {
            // This item is to the left of the indicator
            scootX = -5;
          }
        }

        if (dropIndex !== null && draggedItemId !== null && draggedIndex !== -1 && !isDraggedItem) {
          // Simplified, direct mapping from dropIndex to indicator placement
          if (dropIndex === index) {
            shouldShowDropIndicatorBefore = true;
          } else if (dropIndex === index + 1) {
            shouldShowDropIndicatorAfter = true;
          }
        }

        return (
          <React.Fragment key={row.item.id}>
            {/* Drop indicator before item */}
            {shouldShowDropIndicatorBefore && (
              <div
                className="drop-indicator absolute"
                style={{
                  left: `${startTimes[index] * pixelsPerSecond - 7}px`,
                  top: 0,
                  bottom: 0,
                  height: '80px'
                }}
              />
            )}

            <div
              data-timeline-index={index}
              className={`absolute top-0 timeline-item ${isDraggedItem ? 'opacity-50' : ''}`}
              style={{
                left: `${startTimes[index] * pixelsPerSecond}px`,
                top: '0',
                width: `${rowDurationSeconds(row) * pixelsPerSecond}px`,
                transform: scootX !== 0 ? `translateX(${scootX}px)` : undefined,
                transition: 'transform 100ms ease'
              }}
              onDragOver={(e) => onDragOver(e, index)}
              onDrop={onDrop}
            >
              <TimelineItem
                row={row}
                isSelected={selectedItemId === row.item.id}
                isDragging={isDraggedItem}
                zoom={pixelsPerSecond}
                onSelect={() => onSelectItem(row.item.id)}
                onDurationChange={(duration) => onDurationChange(row.item.id, duration)}
                onDragStart={(e) => onDragStart(e, row.item.id)}
                onDragEnd={onDragEnd}
                onContextMenu={(e) => onContextMenu?.(e, row)}
              />
            </div>

            {/* Drop indicator after item */}
            {shouldShowDropIndicatorAfter && (
              <div
                className="drop-indicator absolute"
                style={{
                  left: `${(startTimes[index] + rowDurationSeconds(row)) * pixelsPerSecond - 7}px`,
                  top: 0,
                  bottom: 0,
                  height: '80px'
                }}
              />
            )}
          </React.Fragment>
        );
      })}

      {/* Add button at the end */}
      {showAddButton && rows.length > 0 && (
        <div
          className="absolute top-0"
          style={{
            left: `${(startTimes[rows.length - 1] + rowDurationSeconds(rows[rows.length - 1])) * pixelsPerSecond}px`
          }}
        >
          <AddContentButton
            onAddContentItem={onAddContentItem}
            allContent={allContent}
            integrations={integrations}
            onSelectIntegration={onSelectIntegration}
            onContentUploaded={onContentUploaded}
          />
        </div>
      )}

      {/* Empty state with add button */}
      {rows.length === 0 && showAddButton && (
        <div className="w-full flex items-center justify-center">
          <AddContentButton
            allContent={allContent}
            integrations={integrations}
            onSelectIntegration={onSelectIntegration}
            onContentUploaded={onContentUploaded}
            onAddContentItem={onAddContentItem}
          />
        </div>
      )}
    </div>
  );
}
