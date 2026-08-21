import React from "react";
import { FileImage, FileVideo, AppWindow, RadioTower } from "lucide-react";
import { mediaChip, mediaKind } from "@/components/broadcasts/types";
import type { SignageMedia } from "@/utils/lait/types";
import { isImageContent, isVideoContent } from "@/utils/uploads/contentTypeUtils";

interface ContentPreviewProps {
  content: SignageMedia | null;
  /** Kept for the timeline's playback plumbing; no video element renders yet. */
  videoRef?: React.RefObject<HTMLVideoElement | null>;
  onContextMenu?: (e: React.MouseEvent) => void;
}

export default function ContentPreview({
  content,
  onContextMenu
}: ContentPreviewProps) {
  if (!content) {
    return (
      <div className="h-full flex items-center justify-center p-2 sm:p-4">
        <div className={`text-center aspect-[16/9] bg-gray-600 rounded-lg shadow-lg overflow-hidden flex flex-col items-center justify-center
            h-full
            min-md:max-h-[55vh]
            max-sm:h-auto max-sm:w-full
          `}
        >
          <div className="w-16 h-16 mb-2 flex items-center justify-center rounded-full bg-gray-800/30 dark:bg-gray-700/30">
            <FileImage className="w-8 h-8 text-gray-800 dark:text-gray-800" />
          </div>
          <p className="text-md font-semibold text-gray-800 dark:text-gray-400">Add media below</p>
        </div>
      </div>
    );
  }

  const source = content.source;
  const kind = mediaKind(content);

  const placeholderIcon = isImageContent(kind)
    ? <FileImage className="w-12 h-12 text-gray-500" />
    : isVideoContent(kind)
      ? <FileVideo className="w-12 h-12 text-gray-500" />
      : source.source === 'live'
        ? <RadioTower className="w-12 h-12 text-gray-500" />
        : <AppWindow className="w-12 h-12 text-gray-500" />;

  return (
    <div className="h-full w-full flex items-center justify-center min-md:p-2 sm:p-4">
      {/* Slide Container with strict 16:9 aspect ratio, responsive scaling */}
      <div
        className={`
          relative aspect-[16/9] rounded-lg shadow-lg bg-black overflow-hidden
          h-full
          min-md:max-h-[55vh]
          max-sm:h-auto max-sm:w-full
        `}
        onContextMenu={(e) => {
          if (content && onContextMenu) {
            e.preventDefault();
            onContextMenu(e);
          }
        }}
      >

        {/* Slide border effect */}
        <div className="absolute inset-0 border-2 border-gray-800 rounded-lg pointer-events-none" />

        <div className="w-full h-full rounded-lg bg-gradient-to-br from-gray-900 to-black overflow-hidden flex items-center justify-center relative z-[1]">
          {source.source === 'card' ? (
            <div
              className="w-full h-full flex flex-col items-center justify-center px-8 text-center"
              style={{ background: source.background, color: source.foreground }}
            >
              <p className="text-3xl font-semibold">{source.title}</p>
              {source.body && <p className="mt-3 text-lg opacity-80">{source.body}</p>}
            </div>
          ) : (
            // Stored bytes have no browser URL yet; render the entry's
            // facts instead of inventing one.
            <div className="text-center px-6">
              <div className="w-24 h-24 mx-auto mb-4 rounded-lg bg-gray-800 flex items-center justify-center">
                {placeholderIcon}
              </div>
              <p className="text-gray-300 font-medium truncate max-w-[60vw]">{content.name}</p>
              <span className="mt-2 inline-block text-xs px-2 py-0.5 rounded bg-gray-800 text-gray-400">
                {mediaChip(content)}
              </span>
              {content.width && content.height ? (
                <p className="mt-2 text-xs text-gray-500">{content.width}×{content.height}</p>
              ) : null}
            </div>
          )}
        </div>

        {/* Slide number indicator */}
        <div className="absolute bottom-4 right-4 bg-black/50 backdrop-blur-sm px-3 py-1 rounded-md">
          <span className="text-xs text-gray-400 font-mono">16:9</span>
        </div>
      </div>
    </div>
  );
}
