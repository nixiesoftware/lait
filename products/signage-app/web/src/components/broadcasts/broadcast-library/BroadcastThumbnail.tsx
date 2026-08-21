import React, { useState, useEffect, useRef } from "react";
import { ChevronLeft, ChevronRight, FileImage, FileVideo, AppWindow } from "lucide-react";
import { mediaChip, mediaKind } from "@/components/broadcasts/types";
import type { SignageItem, SignageMedia } from "@/utils/lait/types";
import { isImageContent, isVideoContent } from "@/utils/uploads/contentTypeUtils";
import { PencilIcon } from "../../../../public/images/icons/theme-icons";

interface BroadcastThumbnailProps {
  items?: SignageItem[];
  /** Library entries by id — the join the caller already holds. */
  mediaMap?: Map<string, SignageMedia>;
  className?: string;
  hideEditButton?: boolean;
  showItemCount?: boolean;
}

export const BroadcastThumbnail: React.FC<BroadcastThumbnailProps> = ({
  items = [],
  mediaMap,
  className = "",
  hideEditButton = false,
  showItemCount = true,
}) => {
  const [currentIndex, setCurrentIndex] = useState(0);
  const [isHovered, setIsHovered] = useState(false);
  const intervalRef = useRef<NodeJS.Timeout | null>(null);

  // Auto-advance on hover
  useEffect(() => {
    if (isHovered && items.length > 1) {
      intervalRef.current = setInterval(() => {
        setTimeout(() => {
          setCurrentIndex((prev) => (prev + 1) % items.length);
        }, 800);
      }, 2000);
    } else if (intervalRef.current) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }

    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
      }
    };
  }, [isHovered, items.length]);

  const handlePrevious = (e: React.MouseEvent) => {
    e.stopPropagation();
    setTimeout(() => {
      setCurrentIndex((prev) => (prev - 1 + items.length) % items.length);
    }, 200);
  };

  const handleNext = (e: React.MouseEvent) => {
    e.stopPropagation();
    setTimeout(() => {
      setCurrentIndex((prev) => (prev + 1) % items.length);
    }, 200);
  };

  const handleDotClick = (index: number, e: React.MouseEvent) => {
    e.stopPropagation();
    if (index !== currentIndex) {
      setTimeout(() => {
        setCurrentIndex(index);
      }, 50);
    }
  };

  // Reset to first item when not hovered
  useEffect(() => {
    if (!isHovered) {
      setCurrentIndex(0);
    }
  }, [isHovered]);

  const renderTile = (media: SignageMedia) => {
    if (media.source.source === 'card') {
      return (
        <div
          className="w-full h-full flex items-center justify-center px-2"
          style={{ background: media.source.background, color: media.source.foreground }}
        >
          <p className="text-sm font-medium truncate">{media.source.title}</p>
        </div>
      );
    }
    if (media.source.source === 'kind') {
      return (
        <div className="w-full h-full bg-gradient-to-br from-purple-500 to-purple-700 flex flex-col items-center justify-center gap-1 text-white">
          <AppWindow className="w-6 h-6" />
          <p className="text-xs font-medium truncate max-w-full px-2">{media.name}</p>
        </div>
      );
    }
    // Stored bytes have no browser URL yet — the tile carries the facts.
    const kind = mediaKind(media);
    return (
      <div className="w-full h-full bg-gray-200 dark:bg-gray-700 flex flex-col items-center justify-center gap-1 text-gray-600 dark:text-gray-300">
        {isImageContent(kind) ? (
          <FileImage className="w-6 h-6" />
        ) : isVideoContent(kind) ? (
          <FileVideo className="w-6 h-6" />
        ) : (
          <FileImage className="w-6 h-6" />
        )}
        <p className="text-xs font-medium truncate max-w-full px-2">{media.name}</p>
        <span className="text-[9px] px-1 py-0.5 rounded bg-black/10 dark:bg-white/10">{mediaChip(media)}</span>
      </div>
    );
  };

  if (items.length === 0) {
    return (
      <div className={`relative w-full h-full aspect-video bg-gray-100 dark:bg-gray-800 rounded-sm flex items-center justify-center ${className}`}>
        <p className="text-sm text-gray-500 dark:text-gray-400">Empty</p>
      </div>
    );
  }

  return (
    <div
      className={`relative w-full h-full aspect-video rounded-sm bg-gray-100 dark:bg-gray-800 overflow-hidden group ${className}`}
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => setIsHovered(false)}
    >
      {/* Content Slider Container */}
      <div className="relative w-full h-full">
        {!hideEditButton && (
          <button className="sm:hidden absolute top-3 right-3 z-999 p-1 rounded-sm bg-white group-hover:bg-gray-100 shadow-md">
            <PencilIcon className="w-5 h-5 text-gray-600 group-hover:text-gray-800" viewBox="0 0 22 22"/>
          </button>
        )}
        <div
          className="flex h-full transition-transform duration-500 ease-in-out"
          style={{
            transform: `translateX(-${currentIndex * 100}%)`,
          }}
        >
          {items.map((item) => {
            const media = mediaMap?.get(item.media);

            return (
              <div
                key={item.id}
                className="w-full h-full flex-shrink-0"
              >
                {media ? (
                  <div className="relative w-full h-full">
                    {renderTile(media)}
                  </div>
                ) : (
                  <div className="w-full h-full bg-gray-200 dark:bg-gray-700 animate-pulse" />
                )}
              </div>
            );
          })}
        </div>
      </div>

      {/* Hover Controls */}
      {isHovered && items.length > 1 && (
        <>
          {/* Previous Button */}
          <button
            onClick={handlePrevious}
            className="absolute left-1 top-1/2 -translate-y-1/2 bg-black/50 hover:bg-black/70 text-white rounded-full p-1 transition-all opacity-0 group-hover:opacity-100"
          >
            <ChevronLeft className="size-3" />
          </button>

          {/* Next Button */}
          <button
            onClick={handleNext}
            className="absolute right-1 top-1/2 -translate-y-1/2 bg-black/50 hover:bg-black/70 text-white rounded-full p-1 transition-all opacity-0 group-hover:opacity-100"
          >
            <ChevronRight className="size-3" />
          </button>

          {/* Dot Indicators */}
          <div className="absolute bottom-2 left-1/2 -translate-x-1/2 flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
            {items.map((_, index) => (
              <button
                key={index}
                onClick={(e) => handleDotClick(index, e)}
                className={`w-1.5 h-1.5 rounded-full transition-all ${
                  index === currentIndex
                    ? 'bg-white w-3'
                    : 'bg-white/50 hover:bg-white/70'
                }`}
              />
            ))}
          </div>
        </>
      )}

      {/* Item count badge */}
      {(items.length > 0 && showItemCount) && (
        <div className="absolute bottom-1 right-1 bg-black/50 text-white text-[8px] px-2 py-1 rounded">
          {items.length} {items.length === 1 ? 'item' : 'items'}
        </div>
      )}
    </div>
  );
};
