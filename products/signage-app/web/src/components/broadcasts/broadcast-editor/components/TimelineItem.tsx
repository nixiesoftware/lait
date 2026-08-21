import React, { useState, useRef, useEffect } from "react";
import { BroadcastRow, mediaChip, mediaKind, rowDurationSeconds } from "@/components/broadcasts/types";
import { isImageContent, isVideoContent } from "@/utils/uploads/contentTypeUtils";
import { GripVertical, FileImage, FileVideo, AppWindow } from "lucide-react";

interface TimelineItemProps {
  row: BroadcastRow;
  isSelected: boolean;
  isDragging?: boolean;
  zoom: number;
  onSelect: () => void;
  onDurationChange?: (duration: number) => void;
  onDragStart?: (e: React.DragEvent) => void;
  onDragEnd?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
}

const TimelineItem = React.memo(({
  row,
  isSelected,
  isDragging = false,
  zoom,
  onSelect,
  onDurationChange,
  onDragStart,
  onDragEnd,
  onContextMenu
}: TimelineItemProps) => {
  const durationSeconds = rowDurationSeconds(row);
  const kind = mediaKind(row.media);

  const [isResizing, setIsResizing] = useState(false);
  const [showDurationEdit, setShowDurationEdit] = useState(false);
  const [tempDuration, setTempDuration] = useState(durationSeconds.toString());
  const resizeStartX = useRef(0);
  const resizeStartWidth = useRef(0);
  const [itemWidth, setItemWidth] = useState(0);

  // Touch helpers for swipe-suppression and long-press context menu
  const touchMovedRef = useRef(false);
  const touchStartRef = useRef<{ x: number; y: number } | null>(null);
  const longPressTimerRef = useRef<number | null>(null);
  const longPressFiredRef = useRef(false);

  // Update item width when zoom or duration changes
  useEffect(() => {
    setItemWidth(durationSeconds * zoom);
  }, [durationSeconds, zoom]);

  useEffect(() => {
    setTempDuration(durationSeconds.toString());
  }, [durationSeconds]);

  const handleResizeStart = (e: React.MouseEvent) => {
    e.stopPropagation();
    setIsResizing(true);
    resizeStartX.current = e.clientX;
    resizeStartWidth.current = durationSeconds;

    const handleMouseMove = (e: MouseEvent) => {
      const deltaX = e.clientX - resizeStartX.current;
      const deltaDuration = deltaX / zoom;
      const newDuration = Math.max(1, Math.round(resizeStartWidth.current + deltaDuration));

      if (onDurationChange) {
        onDurationChange(newDuration);
      }
    };

    const handleMouseUp = () => {
      setIsResizing(false);
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
  };

  const handleDurationSubmit = () => {
    const newDuration = parseInt(tempDuration);
    if (!isNaN(newDuration) && newDuration > 0) {
      if (onDurationChange) {
        onDurationChange(newDuration);
      }
    } else {
      setTempDuration(durationSeconds.toString());
    }
    setShowDurationEdit(false);
  };

  const getFileIcon = () => {
    if (isImageContent(kind)) return <FileImage className="w-4 h-4" />;
    if (isVideoContent(kind)) return <FileVideo className="w-4 h-4" />;
    return <AppWindow className="w-4 h-4" />;
  };

  const source = row.media.source;

  return (
    <div
      className={`
        group relative h-20 rounded-md mr-3 inset-ring-0 ring-inset-2 box-content transition-all overflow-hidden select-none
        ${isSelected
          ? 'ring-brand-500 ring-2 shadow-md shadow-black/30'
          : 'ring-gray-200 dark:ring-gray-700 hover:ring-gray-300 dark:hover:ring-gray-600 hover:ring-2'
        }
        ${isResizing ? 'cursor-col-resize' : isDragging ? 'cursor-grabbing' : 'cursor-grab'}
        ${isDragging ? 'shadow-2xl' : ''}
      `}
      onClick={(e) => {
        if (touchMovedRef.current || longPressFiredRef.current) {
          e.stopPropagation();
          e.preventDefault();
          touchMovedRef.current = false;
          longPressFiredRef.current = false;
          return;
        }
        onSelect();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        onContextMenu?.(e);
      }}
      onPointerDown={(e) => {
        if (e.pointerType !== 'touch') return;
        if (typeof (e as React.SyntheticEvent).persist === 'function') {
          (e as React.SyntheticEvent).persist();
        }

        touchStartRef.current = { x: e.clientX, y: e.clientY };
        touchMovedRef.current = false;
        longPressFiredRef.current = false;
        if (longPressTimerRef.current) window.clearTimeout(longPressTimerRef.current);
        longPressTimerRef.current = window.setTimeout(() => {
          if (!touchMovedRef.current && touchStartRef.current) {
            longPressFiredRef.current = true;
            try {
              e.stopPropagation();
              e.preventDefault();
            } catch {}

            if ('vibrate' in navigator) {
              try { navigator.vibrate(30); } catch {}
            }
            const margin = 12;
            const rawX = touchStartRef.current.x;
            const rawY = touchStartRef.current.y;
            const vw = window.innerWidth;
            const vh = window.innerHeight;
            const safeClientX = Math.max(margin, Math.min(vw - margin, rawX));
            const safeClientY = Math.max(margin, Math.min(vh - margin, rawY));
            const safePageX = window.scrollX + safeClientX;
            const safePageY = window.scrollY + safeClientY;

            const targetEl = e.currentTarget as HTMLElement;
            const nativeEvt = new MouseEvent('contextmenu', {
              bubbles: true,
              cancelable: true,
              clientX: safeClientX,
              clientY: safeClientY,
              screenX: safeClientX,
              screenY: safeClientY,
            });
            Object.defineProperty(nativeEvt, 'pageX', { value: safePageX });
            Object.defineProperty(nativeEvt, 'pageY', { value: safePageY });

            const syntheticEvt = {
              preventDefault: () => {},
              stopPropagation: () => {},
              clientX: safeClientX,
              clientY: safeClientY,
              pageX: safePageX,
              pageY: safePageY,
              currentTarget: targetEl,
              target: targetEl,
              nativeEvent: nativeEvt,
            } as unknown as React.MouseEvent;
            onSelect();
            onContextMenu?.(syntheticEvt);
          }
        }, 600);
      }}
      onPointerMove={(e) => {
        if (e.pointerType !== 'touch' || !touchStartRef.current) return;
        const dx = Math.abs(e.clientX - touchStartRef.current.x);
        const dy = Math.abs(e.clientY - touchStartRef.current.y);
        if (dx > 6 || dy > 6) {
          touchMovedRef.current = true;
          if (longPressTimerRef.current) {
            window.clearTimeout(longPressTimerRef.current);
            longPressTimerRef.current = null;
          }
        }
      }}
      onPointerUp={(e) => {
        if (longPressTimerRef.current) {
          window.clearTimeout(longPressTimerRef.current);
          longPressTimerRef.current = null;
        }
        if (e.pointerType === 'touch' && longPressFiredRef.current) {
          e.preventDefault();
          e.stopPropagation();
        }
      }}
      onPointerCancel={() => {
        if (longPressTimerRef.current) {
          window.clearTimeout(longPressTimerRef.current);
          longPressTimerRef.current = null;
        }
        longPressFiredRef.current = false;
      }}
      draggable={!isResizing}
      style={{ touchAction: 'pan-x' }}
      onDragStart={(e) => {
        if (isResizing) {
          e.preventDefault();
          return;
        }

        const dragImage = document.createElement('img');
        dragImage.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAAUEBAAAACwAAAAAAQABAAACAkQBADs=';
        e.dataTransfer.setDragImage(dragImage, 0, 0);

        if (onDragStart) {
          onDragStart(e);
        }
      }}
      onDragEnd={() => {
        if (onDragEnd) {
          onDragEnd();
        }
      }}
    >
      {/* Background — placeholder tiles; stored bytes have no browser URL */}
      <div className="absolute inset-0 overflow-hidden">
        {source.source === 'card' ? (
          <div
            className="w-full h-full flex items-center justify-center px-2"
            style={{ background: source.background, color: source.foreground }}
          >
            <span className="text-xs font-medium truncate">{source.title}</span>
          </div>
        ) : source.source === 'kind' ? (
          <div className="w-full h-full bg-gradient-to-br from-purple-500 to-purple-700 flex items-center justify-center gap-1.5 px-2 text-white">
            {getFileIcon()}
            <span className="text-[10px] truncate" style={{ maxWidth: `${Math.max(0, itemWidth - 40)}px` }}>
              {row.media.name}
            </span>
          </div>
        ) : (
          <div className="w-full h-full bg-gray-700 flex items-center justify-center gap-1.5 px-2 text-gray-300">
            {getFileIcon()}
            <span className="text-[10px] truncate" style={{ maxWidth: `${Math.max(0, itemWidth - 40)}px` }}>
              {row.media.name}
            </span>
          </div>
        )}
      </div>

      {/* Dark overlay for better readability */}
      <div className="absolute inset-0 bg-gradient-to-b from-transparent via-black/40 to-black/30" />

      {/* Drag Handle */}
      <div className="absolute left-2 top-1/2 -translate-y-1/2 opacity-0 group-hover:opacity-100 transition-opacity cursor-move z-10">
        <div className="p-1 bg-black/50 rounded">
          <GripVertical className="w-4 h-4 text-white" />
        </div>
      </div>

      {/* Content */}
      <div className="relative h-full flex px-4 z-10 items-end content-end">
        <div className="flex-1 min-w-0 my-2 ml-auto w-fit">
          <div className="flex items-center gap-2 self-end ml-auto">
            {showDurationEdit ? (
              <input
                type="number"
                value={tempDuration}
                onChange={(e) => setTempDuration(e.target.value)}
                onBlur={handleDurationSubmit}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') handleDurationSubmit();
                  if (e.key === 'Escape') {
                    setTempDuration(durationSeconds.toString());
                    setShowDurationEdit(false);
                  }
                }}
                className="w-6 px-1 text-xs bg-gray-100 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-xs [appearance:textfield]"
                autoFocus
                onClick={(e) => e.stopPropagation()}
              />
            ) : (
              <span
                className="text-xs text-white/80 hover:text-white cursor-grabbing drop-shadow-lg"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowDurationEdit(true);
                }}
              >
                {durationSeconds}s
              </span>
            )}
          </div>
        </div>
      </div>

      {/* Resize Handle */}
      <div
        className="absolute right-0 top-0 bottom-0 w-2 hover:bg-brand-500/20 transition-colors z-10 cursor-col-resize"
        onMouseDown={handleResizeStart}
      >
        <div className="absolute right-0 top-1/2 -translate-y-1/2 w-0.5 h-8 bg-white/50" />
      </div>

      {/* Duration Overlay */}
      {zoom > 15 && (
        <div className="absolute bottom-1 right-2 text-xs text-white/90 opacity-0 group-hover:opacity-100 transition-opacity z-10 drop-shadow-lg">
          {mediaChip(row.media)} · {durationSeconds}s
        </div>
      )}
    </div>
  );
}, (prevProps, nextProps) => {
  return (
    prevProps.row.item === nextProps.row.item &&
    prevProps.row.media === nextProps.row.media &&
    prevProps.isSelected === nextProps.isSelected &&
    prevProps.isDragging === nextProps.isDragging &&
    prevProps.zoom === nextProps.zoom
  );
});

TimelineItem.displayName = 'TimelineItem';

export default TimelineItem;
