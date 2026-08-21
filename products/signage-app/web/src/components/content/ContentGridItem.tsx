import React from "react";
import { ContentItemProps, sourceLabel } from "./index";
import { SourceGlyph } from "./SourceGlyph";
import { BaseGridItem } from "@/components/ui/BaseGridItem";
import { InlineEditableText } from "@/components/ui/InlineEditableText";
import { ScrollingText } from "@/components/ui/ScrollingText";

interface ContentGridItemProps {
  content: ContentItemProps;
  onClick: (content: ContentItemProps) => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  isSelected?: boolean;
  onToggleSelect?: (id: string) => void;
  onUpdateName?: (id: string, name: string) => void;
}

export const ContentGridItem: React.FC<ContentGridItemProps> = ({ content, onClick, onContextMenu, isSelected = false, onToggleSelect, onUpdateName }) => {
  const [isHovered, setIsHovered] = React.useState(false);

  return (
    <BaseGridItem
      isSelected={isSelected}
      onToggleSelect={() => !content.isUploading && onToggleSelect?.(content.id)}
      onClick={() => !content.isUploading && onClick(content)}
      onContextMenu={onContextMenu}
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => setIsHovered(false)}
      useCheckbox={!!onToggleSelect}
      className={"cursor-pointer"}
      thumbnailContent={
      <div className={`aspect-[9/8] px-4 py-3 min-md:rounded-sm rounded-md flex items-center justify-center
          ${isSelected ? 'ring-1 ring-brand-500 ring-offset-2 dark:ring-offset-gray-900' : ''}
          bg-gray-100 dark:bg-gray-800 group-hover:bg-gray-200 dark:group-hover:bg-gray-700 transition-colors`}>
        <div className="w-full h-full flex items-center justify-center">
          {content.isUploading ? (
            <div className="flex flex-col items-center justify-center gap-1 animate-pulse text-gray-500 dark:text-gray-400">
              <span className="text-sm font-medium">Uploading…</span>
              <span className="text-xs truncate max-w-[150px]">{content.name}</span>
            </div>
          ) : content.source.source === "card" ? (
            <div
              className="w-full h-full rounded-sm flex items-center justify-center p-2 text-center"
              style={{ backgroundColor: content.source.background, color: content.source.foreground }}
            >
              <span className="text-sm font-semibold break-words">{content.source.title}</span>
            </div>
          ) : (
            <div className="flex flex-col items-center justify-center gap-2 text-gray-400 dark:text-gray-500">
              <SourceGlyph source={content.source} className="w-8 h-8" />
              <span className="text-[10px] font-medium uppercase tracking-wide">{sourceLabel(content.source)}</span>
            </div>
          )}
        </div>
      </div>
      }
      textContent={
        <>
          <div className="text-xs font-medium text-left w-full dark:text-white mt-2">
            {onUpdateName ? (
              <InlineEditableText
                value={content.name}
                onSave={(newName) => onUpdateName(content.id, newName)}
                displayClassName="dark:text-white"
                editClassName="text-xs font-medium dark:text-white max-w-full"
                isParentHovered={isHovered}
              />
            ) : (
              <ScrollingText
                text={content.name}
                className="text-xs font-medium dark:text-white"
                speed={30}
                delay={500}
                isParentHovered={isHovered}
              />
            )}
          </div>
          <p className="text-[10px] sm:text-[8px] font-normal text-left truncate flex justify-between text-gray-500 dark:text-white">
            <span className="flex items-center gap-1">
              <SourceGlyph source={content.source} className="size-3 sm:size-2" />
              {sourceLabel(content.source)}
            </span>
            <span>
              {content.width && content.height ? `${content.width}x${content.height}` : ""}
            </span>
          </p>
        </>
      }
    />
  );
};
