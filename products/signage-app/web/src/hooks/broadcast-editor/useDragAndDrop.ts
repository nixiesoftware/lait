import {useState, useRef, useCallback, useEffect} from 'react';
import { BroadcastRow } from '@/components/broadcasts/types';
import { useTimelineContext } from '@/context/TimelineContext';

interface DragState {
  draggedItemId: string | null;
  dropIndex: number | null;
  isDragging: boolean;
  dragPosition: { x: number; y: number } | null;
  dragOffset: { x: number; y: number };
}

interface UseDragAndDropProps {
  rows: BroadcastRow[];
  onReorder: (rows: BroadcastRow[]) => void;
  scrollContainerRef: React.RefObject<HTMLDivElement | null>;
  pixelsPerSecond?: number;
}

interface UseDragAndDropReturn {
  dragState: DragState;
  handleDragStart: (e: React.DragEvent, itemId: string) => void;
  handleDragEnd: () => void;
  handleDragOver: (e: React.DragEvent, index: number) => void;
  handleDrop: (e: React.DragEvent) => void;
  // Touch handlers
  handleTouchStart: (e: React.TouchEvent, itemId: string) => void;
  handleTouchMove: (e: React.TouchEvent) => void;
  handleTouchEnd: (e: React.TouchEvent) => void;
  isItemBeingDragged: (itemId: string) => boolean;
  getDropIndicatorPosition: (index: number) => 'before' | 'after' | null;
}

// Context-aware version
export function useDragAndDropWithContext(
  timelineScrollRef: React.RefObject<HTMLDivElement | null>
): UseDragAndDropReturn {
  const { state, actions } = useTimelineContext();
  const { rows } = state;
  const { onReorder, setDraggedItemId, setDropIndex, setIsDragging } = actions;
  const { draggedItemId, dropIndex } = state.ui;

  // Local state for drag position and touch-specific items
  const [dragPosition, setDragPosition] = useState<{ x: number; y: number } | null>(null);
  const [dragOffset, setDragOffset] = useState<{ x: number; y: number }>({ x: 0, y: 0 });
  const [touchDraggedItem, setTouchDraggedItem] = useState<string | null>(null);
  const [touchDragActive, setTouchDragActive] = useState(false);
  const [, setIsPreparingToDrag] = useState(false);

  // Refs for touch
  const cachedContainerRect = useRef<DOMRect | null>(null);
  const touchStartPos = useRef<{ x: number; y: number } | null>(null);
  const touchTimer = useRef<NodeJS.Timeout | null>(null);

  // Combined drag state
  const dragState: DragState = {
    draggedItemId: draggedItemId || touchDraggedItem,
    dropIndex,
    isDragging: !!(draggedItemId || touchDraggedItem),
    dragPosition,
    dragOffset
  };

  // Mouse drag handlers
  const handleDragStart = useCallback((e: React.DragEvent, itemId: string) => {
    setDraggedItemId(itemId);
    setIsDragging(true);
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', itemId);
    document.body.classList.add('dragging');
  }, [setDraggedItemId, setIsDragging]);

  const handleDragEnd = useCallback(() => {
    setDraggedItemId(null);
    setDropIndex(null);
    setIsDragging(false);
    setDragPosition(null);
    document.body.classList.remove('dragging');
  }, [setDraggedItemId, setDropIndex, setIsDragging]);

  const handleDragOver = useCallback((e: React.DragEvent, index: number) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';

    const activeDraggedItem = draggedItemId || touchDraggedItem;
    if (activeDraggedItem === null) return;

    const draggedIndex = rows.findIndex(r => r.item.id === activeDraggedItem);
    if (draggedIndex === -1) return;

    const element = e.currentTarget as HTMLElement;
    const rect = element.getBoundingClientRect();
    const mouseX = e.clientX - rect.left;
    const elementWidth = rect.width;

    // Compute boundary-based drop index (absolute, before removal adjustment)
    const targetDropIndex = mouseX < elementWidth / 2 ? index : index + 1;

    // If dropping into the same position (no-op), clear indicator
    if (targetDropIndex === draggedIndex || targetDropIndex === draggedIndex + 1) {
      if (dropIndex !== null) setDropIndex(null);
      return;
    }

    if (targetDropIndex !== dropIndex) {
      setDropIndex(targetDropIndex);
    }
  }, [draggedItemId, touchDraggedItem, rows, dropIndex, setDropIndex]);

  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();

    const activeDraggedItem = draggedItemId || touchDraggedItem;
    if (activeDraggedItem === null || dropIndex === null) {
      handleDragEnd();
      return;
    }

    const draggedIndex = rows.findIndex(r => r.item.id === activeDraggedItem);
    if (draggedIndex === -1) {
      handleDragEnd();
      return;
    }

    // Convert boundary dropIndex to insertion index after removing the dragged item
    let targetIndex = dropIndex;
    if (draggedIndex < targetIndex) {
      targetIndex = targetIndex - 1;
    }

    // Only reorder if we're actually moving
    if (draggedIndex !== targetIndex) {
      const newRows = [...rows];
      const [draggedRow] = newRows.splice(draggedIndex, 1);
      newRows.splice(targetIndex, 0, draggedRow);
      onReorder(newRows);
    }

    handleDragEnd();
  }, [draggedItemId, touchDraggedItem, dropIndex, rows, onReorder, handleDragEnd]);

  // Touch handlers
  const handleTouchStart = useCallback((e: React.TouchEvent, itemId: string) => {
    const touch = e.touches[0];
    touchStartPos.current = { x: touch.clientX, y: touch.clientY };

    const element = e.currentTarget as HTMLElement;
    const elementRect = element.getBoundingClientRect();
    const initialOffset = {
      x: touch.clientX - elementRect.left,
      y: touch.clientY - elementRect.top
    };

    // Clear any existing timer
    if (touchTimer.current) {
      clearTimeout(touchTimer.current);
    }

    // Start timer for long press detection
    touchTimer.current = setTimeout(() => {
      setTouchDraggedItem(itemId);
      setTouchDragActive(true);
      setIsPreparingToDrag(false);
      setIsDragging(true);

      if (timelineScrollRef.current) {
        cachedContainerRect.current = timelineScrollRef.current.getBoundingClientRect();
      }

      if (touchStartPos.current) {
        setDragPosition({ x: touchStartPos.current.x, y: touchStartPos.current.y });
      }

      setDragOffset(initialOffset);

      // Haptic feedback if available
      if ('vibrate' in navigator) {
        navigator.vibrate(50);
      }

      document.body.classList.add('dragging');
    }, 400); // 400ms long press

    // Show preparing state after 200ms
    setTimeout(() => {
      if (touchTimer.current) {
        setIsPreparingToDrag(true);
      }
    }, 200);
  }, [timelineScrollRef, setIsDragging]);

  const handleTouchMove = useCallback((e: React.TouchEvent) => {
    const touch = e.touches[0];

    // Check if we moved too far before long press activated
    if (touchStartPos.current && !touchDragActive) {
      const dx = Math.abs(touch.clientX - touchStartPos.current.x);
      const dy = Math.abs(touch.clientY - touchStartPos.current.y);

      if (dx > 10 || dy > 10) {
        // Cancel drag preparation
        if (touchTimer.current) {
          clearTimeout(touchTimer.current);
          touchTimer.current = null;
        }
        setIsPreparingToDrag(false);
        return;
      }
    }

    if (!touchDragActive || !touchDraggedItem) return;

    e.preventDefault();
    setDragPosition({ x: touch.clientX, y: touch.clientY });

    // Find the element under the touch point
    const elements = document.elementsFromPoint(touch.clientX, touch.clientY);
    const timelineItem = elements.find(el =>
      el.classList.contains('timeline-item') &&
      el.getAttribute('data-item-index')
    );

    if (timelineItem) {
      const index = parseInt(timelineItem.getAttribute('data-item-index') || '0', 10);
      handleDragOver(e as unknown as React.DragEvent, index);
    }
  }, [touchDragActive, touchDraggedItem, handleDragOver]);

  const handleTouchEnd = useCallback((e: React.TouchEvent) => {
    // Clear timer
    if (touchTimer.current) {
      clearTimeout(touchTimer.current);
      touchTimer.current = null;
    }

    setIsPreparingToDrag(false);

    if (!touchDragActive || !touchDraggedItem) {
      return;
    }

    e.preventDefault();

    // Find the element under the touch point
    const touch = e.changedTouches[0];
    const elements = document.elementsFromPoint(touch.clientX, touch.clientY);
    const timelineItem = elements.find(el =>
      el.classList.contains('timeline-item') &&
      el.getAttribute('data-item-index')
    );

    if (timelineItem && dropIndex !== null) {
      handleDrop(e as unknown as React.DragEvent);
    }

    // Clean up all touch drag state
    setTouchDraggedItem(null);
    setTouchDragActive(false);
    setDragPosition(null);
    setDropIndex(null);
    setDragOffset({ x: 0, y: 0 });
    setIsDragging(false);
    cachedContainerRect.current = null;
    touchStartPos.current = null;
    document.body.classList.remove('dragging');
  }, [touchDragActive, touchDraggedItem, dropIndex, handleDrop, setDropIndex, setIsDragging]);

  return {
    dragState,
    handleDragStart,
    handleDragEnd,
    handleDragOver,
    handleDrop,
    handleTouchStart,
    handleTouchMove,
    handleTouchEnd,
    isItemBeingDragged: (itemId: string) => dragState.draggedItemId === itemId,
    getDropIndicatorPosition: (index: number) => {
      if (dropIndex === null) return null;
      if (dropIndex === index) return 'before';
      if (dropIndex === index + 1) return 'after';
      return null;
    }
  };
}

// Original version kept for backward compatibility
export function useDragAndDrop({
  rows,
  onReorder,
  scrollContainerRef
}: UseDragAndDropProps): UseDragAndDropReturn {
  // Drag state
  const [draggedItemId, setDraggedItemId] = useState<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const [dragPosition, setDragPosition] = useState<{ x: number; y: number } | null>(null);
  const [dragOffset, setDragOffset] = useState<{ x: number; y: number }>({ x: 0, y: 0 });

  // Touch-specific state
  const [touchDraggedItem, setTouchDraggedItem] = useState<string | null>(null);
  const [touchDragActive, setTouchDragActive] = useState(false);
  const [, setIsPreparingToDrag] = useState(false);

  // Refs for touch
  const cachedContainerRect = useRef<DOMRect | null>(null);
  const touchStartPos = useRef<{ x: number; y: number } | null>(null);
  const touchTimer = useRef<NodeJS.Timeout | null>(null);

  // Combined drag state
  const dragState: DragState = {
    draggedItemId: draggedItemId || touchDraggedItem,
    dropIndex,
    isDragging: !!(draggedItemId || touchDraggedItem),
    dragPosition,
    dragOffset
  };

  // Mouse drag handlers
  const handleDragStart = useCallback((e: React.DragEvent, itemId: string) => {
    setDraggedItemId(itemId);
    e.dataTransfer.effectAllowed = 'move';
    // Store the item ID in dataTransfer for reference
    e.dataTransfer.setData('text/plain', itemId);
    document.body.classList.add('dragging');
  }, []);

  const handleDragEnd = useCallback(() => {
    setDraggedItemId(null);
    setDropIndex(null);
    setDragPosition(null);
    document.body.classList.remove('dragging');
  }, []);

  const handleDragOver = useCallback((e: React.DragEvent, index: number) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';

    const activeDraggedItem = draggedItemId || touchDraggedItem;
    if (activeDraggedItem === null) return;

    const draggedIndex = rows.findIndex(r => r.item.id === activeDraggedItem);
    if (draggedIndex === -1) return;

    const element = e.currentTarget as HTMLElement;
    const rect = element.getBoundingClientRect();
    const mouseX = e.clientX - rect.left;
    const elementWidth = rect.width;

    // Determine drop position based on mouse position in element
    let targetDropIndex;
    if (mouseX < elementWidth / 2) {
      targetDropIndex = index; // Drop before
    } else {
      targetDropIndex = index + 1; // Drop after
    }

    // Don't allow dropping in the same position
    if (targetDropIndex === draggedIndex || targetDropIndex === draggedIndex + 1) {
      setDropIndex(null);
    } else {
      setDropIndex(targetDropIndex);
    }
  }, [draggedItemId, touchDraggedItem, rows]);

  const handleDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();

    const activeDraggedItem = draggedItemId || touchDraggedItem;

    if (activeDraggedItem === null || dropIndex === null) {
      handleDragEnd();
      return;
    }

    const draggedIndex = rows.findIndex(r => r.item.id === activeDraggedItem);
    if (draggedIndex === -1) {
      handleDragEnd();
      return;
    }

    // Don't do anything if dropping in the same position
    if (draggedIndex === dropIndex || (draggedIndex === dropIndex - 1 && dropIndex > draggedIndex)) {
      handleDragEnd();
      return;
    }

    const newRows = [...rows];
    const [removed] = newRows.splice(draggedIndex, 1);

    // Calculate the actual insertion index
    let insertIndex = dropIndex;
    if (draggedIndex < dropIndex) {
      insertIndex = dropIndex - 1;
    }

    newRows.splice(insertIndex, 0, removed);
    onReorder(newRows);
    handleDragEnd();
  }, [draggedItemId, touchDraggedItem, dropIndex, rows, onReorder, handleDragEnd]);

  // Touch handlers
  const handleTouchStart = useCallback((e: React.TouchEvent, itemId: string) => {
    const touch = e.touches[0];
    touchStartPos.current = { x: touch.clientX, y: touch.clientY };

    const element = e.currentTarget as HTMLElement;
    const elementRect = element.getBoundingClientRect();
    const initialOffset = {
      x: touch.clientX - elementRect.left,
      y: touch.clientY - elementRect.top
    };

    // Clear any existing timer
    if (touchTimer.current) {
      clearTimeout(touchTimer.current);
    }

    // Start timer for long press detection
    touchTimer.current = setTimeout(() => {
      setTouchDraggedItem(itemId);
      setTouchDragActive(true);
      setIsPreparingToDrag(false);

      if (scrollContainerRef.current) {
        cachedContainerRect.current = scrollContainerRef.current.getBoundingClientRect();
      }

      if (touchStartPos.current) {
        setDragPosition({ x: touchStartPos.current.x, y: touchStartPos.current.y });
      }

      setDragOffset(initialOffset);

      // Haptic feedback if available
      if ('vibrate' in navigator) {
        navigator.vibrate(50);
      }

      document.body.classList.add('dragging');
    }, 400); // 400ms long press

    // Show preparing state after 200ms
    setTimeout(() => {
      if (touchTimer.current) {
        setIsPreparingToDrag(true);
      }
    }, 200);
  }, [scrollContainerRef]);

  const handleTouchMove = useCallback((e: React.TouchEvent) => {
    const touch = e.touches[0];

    // Check if we moved too far before long press activated
    if (touchStartPos.current && !touchDragActive) {
      const dx = Math.abs(touch.clientX - touchStartPos.current.x);
      const dy = Math.abs(touch.clientY - touchStartPos.current.y);

      if (dx > 10 || dy > 10) {
        // Cancel drag preparation
        if (touchTimer.current) {
          clearTimeout(touchTimer.current);
          touchTimer.current = null;
        }
        setIsPreparingToDrag(false);
        return;
      }
    }

    // If drag is active, update position
    if (touchDragActive && touchDraggedItem !== null) {
      e.preventDefault();
      e.stopPropagation();
      setDragPosition({ x: touch.clientX, y: touch.clientY });

      // Find which item we're over
      const element = document.elementFromPoint(touch.clientX, touch.clientY);
      if (element) {
        const timelineItem = element.closest('[data-timeline-index]');
        if (timelineItem) {
          const index = parseInt(timelineItem.getAttribute('data-timeline-index') || '-1');
          if (index >= 0) {
            const itemRect = timelineItem.getBoundingClientRect();
            const touchPosInItem = touch.clientX - itemRect.left;
            const itemWidth = itemRect.width;

            const draggedIndex = rows.findIndex(r => r.item.id === touchDraggedItem);
            let newDropIndex = index;

            if (touchPosInItem > itemWidth / 2) {
              newDropIndex = index + 1;
            }

            if (newDropIndex !== draggedIndex && newDropIndex !== draggedIndex + 1) {
              setDropIndex(newDropIndex);
            } else {
              setDropIndex(null);
            }
          }
        }
      }
    }
  }, [touchDragActive, touchDraggedItem, rows]);

  const handleTouchEnd = useCallback((e: React.TouchEvent) => {
    // Clear timer
    if (touchTimer.current) {
      clearTimeout(touchTimer.current);
      touchTimer.current = null;
    }

    setIsPreparingToDrag(false);

    // If drag was active, handle drop
    if (touchDragActive && touchDraggedItem !== null) {
      e.preventDefault();

      if (dropIndex !== null) {
        const draggedIndex = rows.findIndex(r => r.item.id === touchDraggedItem);
        if (draggedIndex !== -1 && draggedIndex !== dropIndex) {
          const newRows = [...rows];
          const [removed] = newRows.splice(draggedIndex, 1);

          let insertIndex = dropIndex;
          if (draggedIndex < dropIndex) {
            insertIndex = dropIndex - 1;
          }

          newRows.splice(insertIndex, 0, removed);
          onReorder(newRows);
        }
      }
    }

    // Clean up all touch drag state
    setTouchDraggedItem(null);
    setTouchDragActive(false);
    setDragPosition(null);
    setDropIndex(null);
    setDragOffset({ x: 0, y: 0 });
    cachedContainerRect.current = null;
    touchStartPos.current = null;
    document.body.classList.remove('dragging');
  }, [touchDragActive, touchDraggedItem, dropIndex, rows, onReorder]);

  // Simple global dragover to allow dropping
  useEffect(() => {
    if (draggedItemId === null) return;

    const handleGlobalDragOver = (e: DragEvent) => {
      e.preventDefault();
      e.dataTransfer!.dropEffect = 'move';
    };

    document.addEventListener('dragover', handleGlobalDragOver);

    return () => {
      document.removeEventListener('dragover', handleGlobalDragOver);
    };
  }, [draggedItemId]);

  // Utility functions
  const isItemBeingDragged = useCallback((itemId: string) => {
    return draggedItemId === itemId || touchDraggedItem === itemId;
  }, [draggedItemId, touchDraggedItem]);

  const getDropIndicatorPosition = useCallback((index: number) => {
    if (dropIndex === null || !dragState.isDragging) return null;

    const activeDraggedItem = draggedItemId || touchDraggedItem;
    const draggedIndex = rows.findIndex(r => r.item.id === activeDraggedItem);

    if (index === dropIndex && draggedIndex > dropIndex) {
      return 'before';
    } else if (index === dropIndex && draggedIndex < dropIndex - 1) {
      return 'before';
    } else if (dropIndex === rows.length && index === rows.length - 1) {
      return 'after';
    }

    return null;
  }, [dropIndex, draggedItemId, touchDraggedItem, rows, dragState.isDragging]);

  return {
    dragState,
    handleDragStart,
    handleDragEnd,
    handleDragOver,
    handleDrop,
    handleTouchStart,
    handleTouchMove,
    handleTouchEnd,
    isItemBeingDragged,
    getDropIndicatorPosition
  };
}
