import React, { useState } from "react";
import { BaseListItem } from "@/components/ui/BaseListItem";
import { InlineEditableText } from "@/components/ui/InlineEditableText";
import { ScrollingText } from "@/components/ui/ScrollingText";
import { BroadcastThumbnail } from "@/components/broadcasts";
import { motion } from "framer-motion";
import type { SignageItem, SignageMedia } from "@/utils/lait/types";
import { BsThreeDotsVertical } from "@react-icons/all-files/bs/BsThreeDotsVertical";
import { Monitor } from "lucide-react";

export interface ScreenInfo {
  id: string;
  name: string;
}

export interface BroadcastListItemProps {
  id: string;
  name: string;
  contentCount: number;
  items?: SignageItem[];
  mediaMap?: Map<string, SignageMedia>;
  assignedScreenCount?: number; // number of screens this broadcast is assigned to
  assignedScreens?: ScreenInfo[]; // list of screens this broadcast is assigned to
  isSelected?: boolean;
  isHighlighted?: boolean;
  isDesktop?: boolean;
  onClick?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  onToggleSelect?: (id: string) => void;
  onUpdateName?: (id: string, name: string) => void;
}

export const BroadcastListItem: React.FC<BroadcastListItemProps> = ({
  id,
  name,
  contentCount,
  items,
  mediaMap,
  assignedScreenCount = 0,
  assignedScreens = [],
  isSelected = false,
  isHighlighted = false,
  isDesktop = true,
  onClick,
  onContextMenu,
  onToggleSelect,
  onUpdateName
}) => {
  const [isHovered, setIsHovered] = useState(false);

  return (
    <BaseListItem
      isSelected={isSelected}
      isHighlighted={isHighlighted}
      onToggleSelect={() => onToggleSelect?.(id)}
      onClick={onClick}
      onContextMenu={onContextMenu}
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => setIsHovered(false)}
      showCheckbox={false}
      isDesktop={isDesktop}
      className={"flex flex-row"}
    >
      <td className="my-2 max-md:py-1 flex items-center flex-1 sm:w-3/5 relative">
        <motion.div layoutId={`broadcast-${id}-preview`} className="max-sm:w-full">
          <div className="max-sm:w-full sm:h-20 aspect-video mr-3 !rounded-md bg-gray-100 dark:bg-gray-800 flex items-center justify-center">
            <BroadcastThumbnail
              items={items}
              mediaMap={mediaMap}
              hideEditButton={true}
            />
          </div>
          {!isDesktop && (
            <>
              <ScrollingText
              text={name}
              className="text-md mt-3 flex font-medium dark:text-white"
              speed={30}
              delay={500}
              isParentHovered={isHovered}/>
              <p className="text-xs max-md:text-xs text-gray-500 dark:text-gray-400">
                {assignedScreenCount || 0} Screens Playing • {contentCount} {contentCount === 1 ? 'item' : 'items'}
              </p>
            </>
          )}
        </motion.div>
        {isDesktop && (
          <div className="min-w-0 w-full flex flex-col justify-between">
            <div>
              <div className="w-fit text-xs sm:text-sm dark:text-white">
                {onUpdateName ? (
                  <InlineEditableText
                    value={name}
                    onSave={(newName) => onUpdateName(id, newName)}
                    displayClassName="dark:text-white"
                    editClassName="dark:text-white"
                    isParentHovered={isHovered}
                  />
                ) : (
                  <ScrollingText
                    text={name}
                    className="dark:text-white"
                    speed={30}
                    delay={500}
                    isParentHovered={isHovered}
                  />
                )}
                <p className="text-[10px] max-md:text-xs text-gray-500 dark:text-gray-400">
                  {contentCount} {contentCount === 1 ? 'item' : 'items'}
                </p>
              </div>
            </div>
          </div>
        )}
      </td>
      <td className="hidden my-2 max-md:py-1 sm:flex w-2/5 items-start relative">
        <div className="flex flex-col w-full pr-2">
          <p className="text-[10px] max-md:text-xs text-gray-800 dark:text-gray-400 mb-1">Playing on:</p>
          {assignedScreens && assignedScreens.length > 0 ? (
            <div className="flex flex-wrap gap-1">
              {assignedScreens.slice(0, 5).map((screen) => (
                <div
                  key={screen.id}
                  className="flex items-center gap-1 px-2 py-1 bg-gray-100 dark:bg-gray-800 rounded-full text-[10px] text-gray-700 dark:text-gray-300"
                >
                  <Monitor className="w-3 h-3" />
                  <span className="max-w-[80px] truncate">{screen.name}</span>
                </div>
              ))}
              {assignedScreens.length > 5 && (
                <div className="flex items-center px-2 py-1 bg-gray-100 dark:bg-gray-800 rounded-full text-[10px] text-gray-700 dark:text-gray-300">
                  +{assignedScreens.length - 5}
                </div>
              )}
            </div>
          ) : (
            <p className="text-[10px] max-md:text-xs text-gray-500 dark:text-gray-400">
              No Screens
            </p>
          )}
        </div>
        <span className="absolute flex flex-row gap-2 sm:right-1 sm:top-1 max-sm:bottom-5 right-0 rounded-md">
        <button onClick={onContextMenu} >
          <BsThreeDotsVertical className="sm:size-4 size-6 text-gray-500 hover:text-gray-900" viewBox="0 0 18 18"/>
        </button>
        </span>
      </td>
    </BaseListItem>
  );
};
