import React, { useState } from "react";
import { BaseListItem } from "@/components/ui/BaseListItem";
import { ScrollingText } from "@/components/ui/ScrollingText";

export interface ScreenListItemProps {
  name: string;
  groupName: string | null;
  programName: string | null;
  overrideProgramName: string | null;
  overrideUntilMs: number | null;
  isSelected?: boolean;
  isHighlighted?: boolean;
  onClick?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
}

export const ScreenListItem: React.FC<ScreenListItemProps> = ({
  name,
  groupName,
  programName,
  overrideProgramName,
  overrideUntilMs,
  isSelected = false,
  isHighlighted = false,
  onClick,
  onContextMenu,
}) => {
  const [isHovered, setIsHovered] = useState(false);

  return (
    <BaseListItem
      isSelected={isSelected}
      isHighlighted={isHighlighted}
      onClick={onClick}
      onContextMenu={onContextMenu}
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => setIsHovered(false)}
      showCheckbox={false}
      className={"border-0"}
    >
      <td className="bg-white my-1 px-4 py-2 w-full flex flex-row justify-between items-center font-medium text-gray-800 dark:text-gray-200">
        <div className="flex flex-1 gap-2 items-center min-w-0">
          <ScrollingText
            text={name}
            speed={30}
            delay={500}
            isParentHovered={isHovered}
            className={"text-sm"}
          />
          {groupName && (
            <span className="text-[10px] font-medium text-gray-500 dark:text-gray-400 bg-gray-100 dark:bg-gray-800 px-1.5 py-0.5 rounded-sm shrink-0">
              {groupName}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {overrideProgramName ? (
            <>
              <span className="text-xs text-gray-600 dark:text-gray-300 truncate max-w-[12rem]">
                {overrideProgramName}
              </span>
              <span className="inline-flex items-center bg-amber-100 text-amber-800 dark:bg-amber-900/50 dark:text-amber-200 text-[10px] font-semibold px-1.5 py-0.5 rounded-sm">
                Override until {overrideUntilMs != null ? new Date(overrideUntilMs).toLocaleString() : ""}
              </span>
            </>
          ) : programName ? (
            <span className="text-xs text-gray-600 dark:text-gray-300 truncate max-w-[12rem]">
              {programName}
            </span>
          ) : (
            <span className="text-xs text-gray-400 dark:text-gray-500">No program</span>
          )}
        </div>
      </td>
    </BaseListItem>
  );
};
