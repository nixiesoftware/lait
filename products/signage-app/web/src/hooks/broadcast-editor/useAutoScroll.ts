import { useRef, useCallback, useEffect } from 'react';

interface UseAutoScrollProps {
  scrollContainerRef: React.RefObject<HTMLDivElement | null>;
  isDragging?: boolean;
  scrollSpeed?: number;
  edgeThreshold?: number;
}

interface UseAutoScrollReturn {
  startAutoScroll: (direction: 'left' | 'right') => void;
  stopAutoScroll: () => void;
  checkAutoScroll: (clientX: number) => void;
}

export function useAutoScroll({
  scrollContainerRef,
  isDragging = false,
  scrollSpeed = 8,
  edgeThreshold = 50
}: UseAutoScrollProps): UseAutoScrollReturn {
  const scrollIntervalRef = useRef<NodeJS.Timeout | null>(null);

  // Stop auto-scrolling
  const stopAutoScroll = useCallback(() => {
    if (scrollIntervalRef.current) {
      clearInterval(scrollIntervalRef.current);
      scrollIntervalRef.current = null;
    }
  }, []);

  // Start auto-scrolling in a direction
  const startAutoScroll = useCallback((direction: 'left' | 'right') => {
    // Don't start if already scrolling
    if (scrollIntervalRef.current) return;

    scrollIntervalRef.current = setInterval(() => {
      if (!scrollContainerRef.current) return;

      const container = scrollContainerRef.current;
      const currentScroll = container.scrollLeft;
      const maxScroll = container.scrollWidth - container.clientWidth;

      if (direction === 'left' && currentScroll > 0) {
        container.scrollTo({
          left: Math.max(0, currentScroll - scrollSpeed),
          behavior: 'instant' as ScrollBehavior
        });
      } else if (direction === 'right' && currentScroll < maxScroll) {
        container.scrollTo({
          left: Math.min(maxScroll, currentScroll + scrollSpeed),
          behavior: 'instant' as ScrollBehavior
        });
      } else {
        // Stop if we've reached the edge
        stopAutoScroll();
      }
    }, 16); // ~60fps
  }, [scrollContainerRef, scrollSpeed, stopAutoScroll]);

  // Check if we should auto-scroll based on mouse position
  const checkAutoScroll = useCallback((clientX: number) => {
    if (!scrollContainerRef.current || !isDragging) {
      stopAutoScroll();
      return;
    }

    const container = scrollContainerRef.current;
    const containerRect = container.getBoundingClientRect();
    
    // Get mouse position relative to container
    const containerMouseX = clientX - containerRect.left;

    if (containerMouseX < edgeThreshold) {
      startAutoScroll('left');
    } else if (containerMouseX > containerRect.width - edgeThreshold) {
      startAutoScroll('right');
    } else {
      stopAutoScroll();
    }
  }, [scrollContainerRef, isDragging, edgeThreshold, startAutoScroll, stopAutoScroll]);

  // Clean up on unmount or when dragging stops
  useEffect(() => {
    if (!isDragging) {
      stopAutoScroll();
    }

    return () => {
      stopAutoScroll();
    };
  }, [isDragging, stopAutoScroll]);

  return {
    startAutoScroll,
    stopAutoScroll,
    checkAutoScroll
  };
}