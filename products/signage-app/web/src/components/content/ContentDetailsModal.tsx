import React from "react";
import { Trash2 } from "lucide-react";
import Button from "@/components/ui/button/Button";
import { ContentItemProps, sourceLabel } from "./index";
import { SourceGlyph } from "./SourceGlyph";
import { BaseDetailsModal } from "@/components/ui/BaseDetailsModal";

interface ContentDetailsModalProps {
  isOpen: boolean;
  content: ContentItemProps | null;
  onClose: () => void;
  onDelete?: () => void;
  onUpdate?: (id: string, name: string) => void;
}

function formatDuration(ms: number): string {
  const totalSeconds = Math.round(ms / 1000);
  const mins = Math.floor(totalSeconds / 60);
  const secs = totalSeconds % 60;
  return `${mins}:${secs.toString().padStart(2, '0')}`;
}

function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}

export const ContentDetailsModal: React.FC<ContentDetailsModalProps> = ({
  isOpen,
  content,
  onClose,
  onDelete,
  onUpdate
}) => {
  if (!content) return null;

  const handleUpdateName = (newName: string) => {
    if (onUpdate) {
      onUpdate(content.id, newName);
    }
  };

  // Stored bytes have no browsable URL yet — the download route is a
  // follow-up — so the preview is a placeholder, never an <img>/<video>.
  const mediaPreview = content.source.source === "card" ? (
    <div
      className="w-full h-full flex items-center justify-center p-4 text-center"
      style={{ backgroundColor: content.source.background, color: content.source.foreground }}
    >
      <span className="text-lg font-semibold break-words">{content.source.title}</span>
    </div>
  ) : (
    <div className="w-full h-full flex flex-col items-center justify-center gap-3 bg-gray-100 dark:bg-gray-800 text-gray-400 dark:text-gray-500">
      <SourceGlyph source={content.source} className="w-10 h-10" />
      <span className="text-xs font-medium uppercase tracking-wide">{sourceLabel(content.source)}</span>
    </div>
  );

  const detailsSections = (
    <>
      <div className="flex justify-between items-center py-1">
        <span className="text-sm lg:text-sm font-medium text-gray-700 dark:text-gray-300">Type</span>
        <span className="flex flex-row items-center gap-x-1 text-sm lg:text-sm text-gray-900 dark:text-white">
          <SourceGlyph source={content.source} className="w-3 h-3 pt-0.5" />
          {sourceLabel(content.source)}
        </span>
      </div>

      <div className="flex justify-between items-center py-1">
        <span className="text-sm lg:text-sm font-medium text-gray-700 dark:text-gray-300">Dimensions</span>
        <span className="text-sm lg:text-sm text-gray-900 dark:text-white">
          {content.width && content.height ? `${content.width}x${content.height}` : "Not available"}
        </span>
      </div>

      {content.duration_ms != null && (
        <div className="flex justify-between items-center py-1">
          <span className="text-sm lg:text-sm font-medium text-gray-700 dark:text-gray-300">Duration</span>
          <span className="text-sm lg:text-sm text-gray-900 dark:text-white">
            {formatDuration(content.duration_ms)}
          </span>
        </div>
      )}

      {content.source.source === "stored" && (
        <div className="flex justify-between items-center py-1">
          <span className="text-sm lg:text-sm font-medium text-gray-700 dark:text-gray-300">Size</span>
          <span className="text-sm lg:text-sm text-gray-900 dark:text-white">
            {formatSize(content.source.size)}
          </span>
        </div>
      )}
    </>
  );

  const actionButtons = (
    <div className="flex justify-between items-center">
      {onDelete && (
        <Button
          variant="outline"
          onClick={onDelete}
          className="px-5 py-3 rounded-sm"
        >
          <Trash2 className="w-5 h-5"/>
        </Button>
      )}
    </div>
  );

  return (
    <BaseDetailsModal
      isOpen={isOpen}
      onClose={onClose}
      title={content.name}
      onUpdateTitle={onUpdate ? handleUpdateName : undefined}
      mediaPreview={mediaPreview}
      detailsSections={detailsSections}
      actionButtons={actionButtons}
    />
  );
};
