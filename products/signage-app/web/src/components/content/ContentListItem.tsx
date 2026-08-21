import React from "react";
import { ContentItemProps, sourceLabel } from "./index";
import { SourceGlyph } from "./SourceGlyph";
import { Checkbox } from "@/components/ui/checkbox/Checkbox";
import { InlineEditableText } from "@/components/ui/InlineEditableText";
import { ScrollingText } from "@/components/ui/ScrollingText";

interface ContentListItemProps {
  content: ContentItemProps;
  onClick: (content: ContentItemProps) => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  isHighlighted?: boolean;
  isSelected?: boolean;
  isDesktop?: boolean;
  onToggleSelect?: (id: string) => void;
  onUpdateName?: (id: string, name: string) => void;
}

export const ContentListItem: React.FC<ContentListItemProps> = ({
  content,
  onClick,
  onContextMenu,
  isHighlighted = false,
  isSelected = false,
  isDesktop = true,
  onToggleSelect,
  onUpdateName,
}) => {
  const [isHovered, setIsHovered] = React.useState(false);

  const handleClick = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (!content.isUploading) {
      onClick(content);
    }
  };

  return (
    <tr
      onClick={handleClick}
      onContextMenu={onContextMenu}
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => setIsHovered(false)}
      className={` cursor-pointer group ring-offset-2 ring-inset rounded-sm
      ${isSelected ? 'ring-1 ring-brand-400 bg-brand-400/20 dark:bg-blue-800/20' : ''}
      ${isHighlighted && !isSelected ? 'ring-brand-200' : ''}`}
    >
      <td className="px-4 py-4 flex-wrap whitespace-normal align-middle text-center">
        <Checkbox
          checked={isSelected}
          onChange={() => !content.isUploading && onToggleSelect?.(content.id)}
          size={isDesktop ? 'md':'sm'}
          showOnGroupHover={isDesktop}
          disabled={content.isUploading}
        />
      </td>
      <td className="pr-2 py-4 max-md:py-1 whitespace-nowrap">
        <div className="flex items-center">
          <div className="w-12 h-12 mr-3 rounded overflow-hidden flex-shrink-0 max-md:hidden">
            {content.isUploading ? (
              <div className="w-full h-full flex items-center justify-center bg-gray-100 dark:bg-gray-800 animate-pulse">
                <span className="text-[10px] font-medium text-gray-500 dark:text-gray-400">…</span>
              </div>
            ) : content.source.source === "card" ? (
              <div
                className="w-full h-full flex items-center justify-center"
                style={{ backgroundColor: content.source.background, color: content.source.foreground }}
              >
                <SourceGlyph source={content.source} className="w-4 h-4" />
              </div>
            ) : (
              <div className="w-full h-full flex items-center justify-center bg-gray-100 dark:bg-gray-800 text-gray-400 dark:text-gray-500">
                <SourceGlyph source={content.source} className="w-4 h-4" />
              </div>
            )}
          </div>
          <div className="min-w-0 max-w-full">
            <div className="text-sm font-medium max-md:text-xs dark:text-white">
              {onUpdateName ? (
                <InlineEditableText
                  value={content.name}
                  onSave={(newName) => onUpdateName(content.id, newName)}
                  displayClassName="dark:text-white"
                  editClassName="text-sm font-medium max-md:text-xs dark:text-white"
                  isParentHovered={isHovered}
                />
              ) : (
                <ScrollingText
                  text={content.name}
                  className="text-sm font-medium max-md:text-xs dark:text-white"
                  speed={20}
                  delay={500}
                  isParentHovered={isHovered}
                />
              )}
            </div>
          </div>
        </div>
      </td>
      <td className="pr-2 py-4 whitespace-nowrap text-xs max-md:text-xs text-gray-500 dark:text-gray-300">
        <span className="flex items-center gap-1">
          <SourceGlyph source={content.source} className="w-3 h-3" />
          {sourceLabel(content.source)}
        </span>
      </td>
      <td className="pr-2 py-4 whitespace-nowrap text-sm max-md:text-xs text-gray-500 dark:text-gray-300">
        {content.width && content.height ? `${content.width}x${content.height}` : "-"}
      </td>
    </tr>
  );
};
