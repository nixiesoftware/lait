import {useCallback, useState} from 'react';

interface UseTimelineScrubbingProps {
  totalDuration: number;
  pixelsPerSecond: number;
  scrollContainerRef: React.RefObject<HTMLDivElement | null>;
  onTimeChange: (time: number) => void;
  onScrubStart?: () => void;
  onScrubEnd?: () => void;
}

interface UseTimelineScrubbingReturn {
  isScrubbing: boolean;
  handleScrubStart: (e: React.MouseEvent | React.TouchEvent) => void;
  calculateTimeFromPosition: (clientX: number) => number;
}

export function useTimelineScrubbing({
  totalDuration,
  pixelsPerSecond,
  scrollContainerRef,
  onTimeChange,
  onScrubStart,
  onScrubEnd
}: UseTimelineScrubbingProps): UseTimelineScrubbingReturn {
  const [isScrubbing, setIsScrubbing] = useState(false);

  // Calculate time from mouse/touch position
  const calculateTimeFromPosition = useCallback((clientX: number): number => {
    if (!scrollContainerRef.current) return 0;

    const rect = scrollContainerRef.current.getBoundingClientRect();
    const scrollLeft = scrollContainerRef.current.scrollLeft;
    const x = clientX - rect.left + scrollLeft - 16; // Account for padding
    return Math.max(0, Math.min(totalDuration, x / pixelsPerSecond));
  }, [totalDuration, pixelsPerSecond, scrollContainerRef]);

  // Handle scrubbing start
  const handleScrubStart = useCallback((e: React.MouseEvent | React.TouchEvent) => {
    e.preventDefault();
    setIsScrubbing(true);

    // Call optional callback
    onScrubStart?.();

    const updateScrubPosition = (clientX: number) => {
      const time = calculateTimeFromPosition(clientX);
      onTimeChange(time);
    };

    const handleMouseMove = (e: MouseEvent) => {
      updateScrubPosition(e.clientX);
    };

    const handleTouchMove = (e: TouchEvent) => {
      if (e.touches.length > 0) {
        updateScrubPosition(e.touches[0].clientX);
      }
    };

    const handleEnd = () => {
      setIsScrubbing(false);
      onScrubEnd?.();

      // Clean up event listeners
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('touchmove', handleTouchMove);
      document.removeEventListener('mouseup', handleEnd);
      document.removeEventListener('touchend', handleEnd);
    };

    // Initial position update
    if ('touches' in e) {
      const touchEvent = e as React.TouchEvent;
      if (touchEvent.touches.length > 0) {
        updateScrubPosition(touchEvent.touches[0].clientX);
      }
      document.addEventListener('touchmove', handleTouchMove, { passive: false });
      document.addEventListener('touchend', handleEnd);
    } else {
      const mouseEvent = e as React.MouseEvent;
      updateScrubPosition(mouseEvent.clientX);
      document.addEventListener('mousemove', handleMouseMove);
      document.addEventListener('mouseup', handleEnd);
    }
  }, [calculateTimeFromPosition, onTimeChange, onScrubStart, onScrubEnd]);

  return {
    isScrubbing,
    handleScrubStart,
    calculateTimeFromPosition
  };
}
