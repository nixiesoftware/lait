import {useCallback, useEffect, useRef, useState} from 'react';
import {BroadcastRow, mediaKind, rowDurationSeconds} from '@/components/broadcasts/types';
import {isVideoContent} from '@/utils/uploads/contentTypeUtils';

interface UsePlaybackControlsProps {
  rows: BroadcastRow[];
  startTimes: number[];
  totalDuration: number;
  onSelectItem: (itemId: string) => void;
  videoRef?: React.RefObject<HTMLVideoElement | null>;
}

interface UsePlaybackControlsReturn {
  isPlaying: boolean;
  currentTime: number;
  togglePlayback: () => void;
  setCurrentTime: (time: number | ((prev: number) => number)) => void;
  seekToTime: (time: number) => void;
  getCurrentItemIndex: () => number;
  formatTime: (seconds: number) => string;
}

export function usePlaybackControls({
  rows,
  startTimes,
  totalDuration,
  onSelectItem,
  videoRef
}: UsePlaybackControlsProps): UsePlaybackControlsReturn {
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const playIntervalRef = useRef<NodeJS.Timeout | null>(null);

  // Format time helper
  const formatTime = useCallback((seconds: number) => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  }, []);

  // Get current item index based on time - not memoized to avoid circular deps
  const getCurrentItemIndex = () => {
    return startTimes.findIndex((start, idx) => {
      const end = start + (rows[idx] ? rowDurationSeconds(rows[idx]) : 0);
      return currentTime >= start && currentTime < end;
    });
  };

  // Seek to specific time and handle video
  const seekToTime = useCallback((time: number) => {
    const clampedTime = Math.max(0, Math.min(totalDuration, time));
    setCurrentTime(clampedTime);

    // Find and select the item at this time
    const currentIndex = startTimes.findIndex((start, idx) => {
      const end = start + (rows[idx] ? rowDurationSeconds(rows[idx]) : 0);
      return clampedTime >= start && clampedTime < end;
    });

    if (currentIndex >= 0 && rows[currentIndex]) {
      // Queue the selection to avoid setState during render
      setTimeout(() => {
        onSelectItem(rows[currentIndex].item.id);
      }, 0);

      // If it's a video, seek to the correct position
      if (isVideoContent(mediaKind(rows[currentIndex].media)) && videoRef?.current) {
        const itemStartTime = startTimes[currentIndex];
        videoRef.current.currentTime = clampedTime - itemStartTime;

        // If we're playing, ensure video continues
        if (isPlaying && videoRef.current.paused) {
          videoRef.current.play().catch(console.error);
        }
      }
    }
  }, [totalDuration, startTimes, rows, onSelectItem, videoRef, isPlaying]);

  // Main playback toggle
  const togglePlayback = useCallback(() => {
    if (isPlaying) {
      // Stop playback
      setIsPlaying(false);
      if (playIntervalRef.current) {
        clearInterval(playIntervalRef.current);
        playIntervalRef.current = null;
      }
      // Pause video if playing
      if (videoRef?.current && !videoRef.current.paused) {
        videoRef.current.pause();
      }
    } else {
      // Start playback
      setIsPlaying(true);

      // Find which item we're currently on
      const currentIndex = getCurrentItemIndex();

      if (currentIndex >= 0 && rows[currentIndex]) {
        const currentRow = rows[currentIndex];
        const itemStartTime = startTimes[currentIndex];
        const timeIntoItem = currentTime - itemStartTime;

        // Queue the selection to avoid setState during render
        setTimeout(() => {
          onSelectItem(currentRow.item.id);
        }, 0);

        // If it's a video, start playing from the correct position
        if (isVideoContent(mediaKind(currentRow.media)) && videoRef?.current) {
          videoRef.current.currentTime = timeIntoItem;
          videoRef.current.play().catch(console.error);
        }
      }

      // Start the playback timer
      playIntervalRef.current = setInterval(() => {
        setCurrentTime(prev => {
          const newTime = prev + 0.1;

          // Check for end of broadcast
          if (newTime >= totalDuration) {
            // Stop playback and reset
            setIsPlaying(false);
            if (playIntervalRef.current) {
              clearInterval(playIntervalRef.current);
              playIntervalRef.current = null;
            }
            if (videoRef?.current && !videoRef.current.paused) {
              videoRef.current.pause();
            }
            return 0; // Loop back to start
          }

          // Check if we've moved to a new item
          const newIndex = startTimes.findIndex((start, idx) => {
            const end = start + (rows[idx] ? rowDurationSeconds(rows[idx]) : 0);
            return newTime >= start && newTime < end;
          });

          const prevIndex = startTimes.findIndex((start, idx) => {
            const end = start + (rows[idx] ? rowDurationSeconds(rows[idx]) : 0);
            return prev >= start && prev < end;
          });

          // Handle item transitions
          if (newIndex >= 0 && rows[newIndex] && newIndex !== prevIndex) {
            // Queue the selection to avoid setState during render
            setTimeout(() => {
              onSelectItem(rows[newIndex].item.id);
            }, 0);

            // Handle video transitions
            if (videoRef?.current) {
              // Stop previous video if playing
              if (!videoRef.current.paused) {
                videoRef.current.pause();
              }

              // Start new video if applicable
              if (isVideoContent(mediaKind(rows[newIndex].media))) {
                // Start video immediately for new item
                videoRef.current.currentTime = 0;
                videoRef.current.play().catch(console.error);
              }
            }
          } else if (newIndex >= 0 && rows[newIndex] && isVideoContent(mediaKind(rows[newIndex].media)) && videoRef?.current) {
            // Update video time if still on the same video item
            const itemStartTime = startTimes[newIndex];
            const timeIntoItem = newTime - itemStartTime;

            // Only update if significantly different (avoid constant seeking)
            if (Math.abs(videoRef.current.currentTime - timeIntoItem) > 0.5) {
              videoRef.current.currentTime = timeIntoItem;
            }

            // Ensure video is playing
            if (videoRef.current.paused) {
              videoRef.current.play().catch(console.error);
            }
          }

          return newTime;
        });
      }, 100); // Update every 100ms
    }
  }, [isPlaying, totalDuration, rows, startTimes, onSelectItem, videoRef]);
  // Note: currentTime is intentionally omitted to prevent infinite loops
  // getCurrentItemIndex is not memoized for the same reason

  // Cleanup on unmount
  useEffect(() => {
    const video = videoRef?.current;
    return () => {
      if (playIntervalRef.current) {
        clearInterval(playIntervalRef.current);
      }
      if (video && !video.paused) {
        video.pause();
      }
    };
  }, [videoRef]);

  // Sync video playback state
  useEffect(() => {
    if (!isPlaying && videoRef?.current && !videoRef.current.paused) {
      videoRef.current.pause();
    }
  }, [isPlaying, videoRef]);

  return {
    isPlaying,
    currentTime,
    togglePlayback,
    setCurrentTime,
    seekToTime,
    getCurrentItemIndex,
    formatTime
  };
}
