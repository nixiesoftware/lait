import React from 'react';
import {TriangleDown} from "../../../../../public/images/icons/theme-icons";

interface ScrubBarProps {
  currentTime: number;
  pixelsPerSecond: number;
  totalDuration: number;
  isScrubbing: boolean;
  onScrubStart?: (e: React.MouseEvent | React.TouchEvent) => void;
  height?: number;
  showTimeTooltip?: boolean;
  showHandle?: boolean;
}

export function ScrubBar({
  currentTime,
  pixelsPerSecond,
  isScrubbing,
  onScrubStart,
  height = 200,
  showTimeTooltip = true,
  showHandle = true
}: ScrubBarProps) {
  // Format time for display
  const formatTime = (seconds: number) => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  };

  return (
    <div
      className="absolute w-0.75 bg-gray-800 cursor-ew-resize group"
      style={{
        left: `${(currentTime * pixelsPerSecond) + 12}px`,
        top: 24,
        bottom: 0,
        height: `${height}px`,
        zIndex: 100,
        transition: isScrubbing ? 'none' : 'left 100ms linear'
      }}
      onMouseDown={(e) => onScrubStart?.(e)}
      onTouchStart={(e) => onScrubStart?.(e)}
    >
      {/* Indicator handle */}
      {showHandle && (
        <div className="absolute -top-6 left-1/2 -translate-x-1/2 text-gray-800">
          <TriangleDown className="h-3 w-3" fill="currentColor" viewBox="0 0 500 500"/>
        </div>
      )}

      {/* Time tooltip */}
      {showTimeTooltip && (
        <div className={`absolute ${isScrubbing ? 'group-hover:block': 'hidden '} -top-1 left-1/2 -translate-x-1/2 bg-black text-white text-xs px-2 py-1 rounded whitespace-nowrap`}>
          {formatTime(currentTime)}
        </div>
      )}
    </div>
  );
}
