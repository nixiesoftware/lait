import React, { useState } from "react";
import { BaseGridItem } from "@/components/ui/BaseGridItem";
import { ScrollingText } from "@/components/ui/ScrollingText";
import { BroadcastThumbnail } from "./BroadcastThumbnail";
import type { SignageItem, SignageMedia } from "@/utils/lait/types";

export interface BroadcastGridItemProps {
  id: string;
  name: string;
  contentCount: number;
  items?: SignageItem[];
  mediaMap?: Map<string, SignageMedia>;
  isSelected?: boolean;
  onClick?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  onToggleSelect?: (id: string) => void;
  onUpdateName?: (id: string, name: string) => void;
}

export const BroadcastGridItem: React.FC<BroadcastGridItemProps> = ({
  id,
  name,
  items,
  mediaMap,
  isSelected = false,
  onClick,
  onContextMenu,
  onToggleSelect,
}) => {
  const [isHovered, setIsHovered] = useState(false);

  return (
    <BaseGridItem
      isSelected={isSelected}
      onToggleSelect={() => onToggleSelect?.(id)}
      onClick={onClick}
      onContextMenu={onContextMenu}
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => setIsHovered(false)}
      useCheckbox={false}
      className={"!aspect-video"}
      thumbnailContent={
        <div className={"max-w-full relative"}>
          <BroadcastThumbnail
            items={items}
            mediaMap={mediaMap}
            className={"!rounded-lg"}
            hideEditButton={true}
            showItemCount={false}
          />
          <div className={`absolute bottom-2 left-2 text-sm font-medium text-left w-full mt-1 ${items && items.length > 0 ? 'text-white mix-blend-luminosity' : 'text-gray-700'}`}>
            <ScrollingText
              text={name}
              speed={30}
              delay={500}
              isParentHovered={isHovered}
            />
          </div>
        </div>
      }
    />
  );
};
