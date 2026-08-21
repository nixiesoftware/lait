import React, { useState, useEffect, useRef } from "react";
import {
  Play,
  Pause,
  Trash2,
  Clock,
  FileText
} from "lucide-react";
import { mediaKind } from "@/components/broadcasts/types";
import type { SignageMedia } from "@/utils/lait/types";
import { isImageContent, isVideoContent } from "@/utils/uploads/contentTypeUtils";
import {MosqueIcon} from "../../../../../public/images/icons/theme-icons";
import {AiFillVideoCamera} from "@react-icons/all-files/ai/AiFillVideoCamera";
import {HiMiniPhoto} from "react-icons/hi2";

interface FloatingControlsProps {
  content: SignageMedia;
  duration: number;
  videoRef?: React.RefObject<HTMLVideoElement | null>;
  onDurationChange: (duration: number) => void;
  onRemove: () => void;
  durationInputRef?: React.RefObject<HTMLInputElement | HTMLButtonElement | null>;
  isBroadcastPlaying?: boolean;
  onBroadcastPlayToggle?: () => void;
  broadcastCurrentTime?: number;
  broadcastTotalDuration?: number;
}

export default function FloatingControls({
  content,
  duration,
  videoRef,
  onDurationChange,
  onRemove,
  durationInputRef,
  isBroadcastPlaying = false,
  onBroadcastPlayToggle,
  broadcastCurrentTime = 0,
  broadcastTotalDuration = 0
}: FloatingControlsProps) {
  const [, setIsPlaying] = useState(false);
  const [, setIsMuted] = useState(true);
  const [, setCurrentTime] = useState(0);
  const [showDurationEdit, setShowDurationEdit] = useState(false);
  const [tempDuration, setTempDuration] = useState(duration.toString());
  const durationButtonRef = useRef<HTMLButtonElement>(null);
  const durationInputInternalRef = useRef<HTMLInputElement>(null);

  // Expose ref to parent
  useEffect(() => {
    if (durationInputRef && durationInputRef.current !== durationButtonRef.current) {
      (durationInputRef as React.MutableRefObject<HTMLInputElement | HTMLButtonElement | null>).current = showDurationEdit ? durationInputInternalRef.current : durationButtonRef.current;
    }
  }, [showDurationEdit, durationInputRef]);

  // Update temp duration when duration prop changes
  useEffect(() => {
    setTempDuration(duration.toString());
  }, [duration]);

  // Update video state
  useEffect(() => {
    if (!videoRef?.current) return;

    const video = videoRef.current;

    const handleTimeUpdate = () => {
      setCurrentTime(video.currentTime);
    };

    const handlePlay = () => setIsPlaying(true);
    const handlePause = () => setIsPlaying(false);
    const handleVolumeChange = () => setIsMuted(video.muted);

    video.addEventListener('timeupdate', handleTimeUpdate);
    video.addEventListener('play', handlePlay);
    video.addEventListener('pause', handlePause);
    video.addEventListener('volumechange', handleVolumeChange);

    // Set initial state
    setIsPlaying(!video.paused);
    setIsMuted(video.muted);

    return () => {
      video.removeEventListener('timeupdate', handleTimeUpdate);
      video.removeEventListener('play', handlePlay);
      video.removeEventListener('pause', handlePause);
      video.removeEventListener('volumechange', handleVolumeChange);
    };
  }, [videoRef]);

  const getFileIcon = () => {
    const kind = mediaKind(content);
    if (isImageContent(kind)) return <HiMiniPhoto className="text-green-700 w-5 h-5" />;
    if (isVideoContent(kind)) return <AiFillVideoCamera className="text-brand-700/70 w-5 h-5" />;
    if (kind === 'athan') return <MosqueIcon className="text-red-700/70 w-5 h-5" fill="currentColor" />;
    return <FileText className="w-5 h-5" />;
  };

  const formatTime = (seconds: number) => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  };

  // Unused but kept for potential future use
  // const handlePlayPause = () => {
  //   if (!videoRef?.current) return;
  //
  //   if (isPlaying) {
  //     videoRef.current.pause();
  //   } else {
  //     videoRef.current.play();
  //   }
  // };
  //
  // const handleMuteToggle = () => {
  //   if (!videoRef?.current) return;
  //   videoRef.current.muted = !isMuted;
  // };

  const handleDurationSubmit = () => {
    const newDuration = parseInt(tempDuration);
    if (!isNaN(newDuration) && newDuration > 0) {
      onDurationChange(newDuration);
    } else {
      // Reset to current duration if invalid input
      setTempDuration(duration.toString());
    }
    setShowDurationEdit(false);
  };

  return (
    <div className="rounded-md px-2 sm:px-3 flex items-center gap-2 w-full">
      {/* File Info - Shrinkable */}
      <div className="flex items-center gap-2 min-w-0 flex-shrink">
        {getFileIcon()}
        <div className="min-w-0">
          <p className="text-sm font-medium text-gray-900 dark:text-white truncate max-w-[100px] sm:max-w-[200px]">
            {content.name}
          </p>
        </div>
      </div>

      {/* Center Controls */}
      <div className="flex-1 flex items-center justify-center gap-2">
        {/* Broadcast Play Control - Always visible */}
        {onBroadcastPlayToggle && (
          <>
            <button
              onClick={onBroadcastPlayToggle}
              className="p-1 sm:p-1.5 hover:bg-gray-300 dark:hover:bg-gray-700 rounded transition-colors"
              title={isBroadcastPlaying ? "Pause broadcast" : "Play broadcast"}
            >
              {isBroadcastPlaying ? (
                <Pause className="w-5 h-5 max-md:w-4 max-md:h-4 text-gray-500 dark:text-gray-300" fill="currentColor"/>
              ) : (
                <Play className="w-5 h-5 max-md:w-4 max-md:h-4 text-gray-500 dark:text-gray-300" fill="currentColor"/>
              )}
            </button>
            <span className="text-md max-md:text-sm text-gray-600 dark:text-gray-400">
              {formatTime(broadcastCurrentTime)} / {formatTime(broadcastTotalDuration)}
            </span>
          </>
        )}

        {/* Duration Editor */}
        <div className="flex items-center gap-1 ml-4 rounded bg-gray-300 hover:bg-gray-200 px-2 py-1 animate transition-colors duration-200 group">
          <Clock className="w-5 h-5 max-md:w-4 max-md:h-4 text-gray-500 dark:text-gray-400" />
          {showDurationEdit ? (
            <input
              ref={durationInputInternalRef}
              type="number"
              value={tempDuration}
              onChange={(e) => setTempDuration(e.target.value)}
              onBlur={handleDurationSubmit}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleDurationSubmit();
                if (e.key === 'Escape') {
                  setTempDuration(duration.toString());
                  setShowDurationEdit(false);
                }
              }}
              className="w-12 sm:w-16 px-1 text-md outline-none max-md:text-sm bg-gray-300 dark:bg-gray-700 group-hover:bg-gray-300 rounded"
              autoFocus
              min="1"
            />
          ) : (
            <button
              ref={durationButtonRef}
              onClick={() => setShowDurationEdit(true)}
              className="floating-controls-duration-button text-md max-md:text-sm text-gray-700 dark:text-gray-300 hover:text-gray-900 dark:hover:text-white transition-colors"
            >
              {duration}s
            </button>
          )}
        </div>
      </div>

      {/* Delete Button - Far End */}
      <button
        onClick={onRemove}
        className="p-1 sm:p-1.5 hover:bg-red-50 dark:hover:bg-red-900/20 rounded transition-colors flex-shrink-0 group ml-auto"
        title="Remove from broadcasts"
      >
        <Trash2 className="w-5 h-5 max-md:w-4 max-md:h-4 text-red-500 group-hover:text-red-600 dark:text-gray-400 dark:group-hover:text-red-400 transition-colors"/>
      </button>
    </div>
  );
}
