import React from 'react';
import { Play, Pause, SkipBack, SkipForward } from 'lucide-react';

interface TimelineControlsProps {
  isPlaying: boolean;
  currentTime: number;
  totalDuration: number;
  onPlayToggle: () => void;
  onSeek: (time: number) => void;
  onSkipToStart?: () => void;
  onSkipToEnd?: () => void;
  className?: string;
}

export function TimelineControls({
  isPlaying,
  currentTime,
  totalDuration,
  onPlayToggle,
  onSeek,
  onSkipToStart,
  onSkipToEnd,
  className = ""
}: TimelineControlsProps) {
  const formatTime = (seconds: number) => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  };

  const handleSkipToStart = () => {
    if (onSkipToStart) {
      onSkipToStart();
    } else {
      onSeek(0);
    }
  };

  const handleSkipToEnd = () => {
    if (onSkipToEnd) {
      onSkipToEnd();
    } else {
      onSeek(totalDuration);
    }
  };

  return (
    <div className={`absolute -top-12 z-20 flex items-center gap-2 ${className}`}>
      {/* Skip to start */}
      <button
        onClick={handleSkipToStart}
        className="p-1.5 rounded hover:bg-gray-700 transition-colors"
        title="Skip to start"
      >
        <SkipBack className="w-5 h-5 sm:w-6 sm:h-6 text-gray-300" fill="currentColor"/>
      </button>

      {/* Play/Pause */}
      <button
        onClick={onPlayToggle}
        className="p-1 sm:p-1.5 hover:bg-gray-700 rounded transition-colors drop-shadow-md drop-shadow-white"
        title={isPlaying ? "Pause" : "Play"}
      >
        {isPlaying ? (
          <Pause className="w-6 h-6 sm:w-8 sm:h-8 text-gray-300" fill="currentColor"/>
        ) : (
          <Play className="w-6 h-6 sm:w-8 sm:h-8 text-gray-300" fill="currentColor"/>
        )}
      </button>

      {/* Skip to end */}
      <button
        onClick={handleSkipToEnd}
        className="p-1.5 rounded hover:bg-gray-700 transition-colors"
        title="Skip to end"
      >
        <SkipForward className="w-5 h-5 sm:w-6 sm:h-6 text-gray-300" fill="currentColor"/>
      </button>

      {/* Time display */}
      <div className="text-lg font-mono text-gray-400 ml-2">
        <span className="text-gray-200">
          {formatTime(currentTime)}
        </span>
        <span className="mx-1">/</span>
        <span>{formatTime(totalDuration)}</span>
      </div>
    </div>
  );
}
