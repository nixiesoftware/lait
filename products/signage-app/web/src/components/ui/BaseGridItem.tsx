import React, { ReactNode } from "react";
import { Checkbox } from "./checkbox/Checkbox";
import {HorizontaLDots} from "../../../public/images/icons/theme-icons";

interface BaseGridItemProps {
  isSelected?: boolean;
  onToggleSelect?: () => void;
  onClick?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  onMouseEnter?: () => void;
  onMouseLeave?: () => void;
  thumbnailContent: ReactNode;
  textContent?: ReactNode;
  className?: string;
  useCheckbox?: boolean;
  useDots?: boolean;
}

export const BaseGridItem: React.FC<BaseGridItemProps> = ({
  isSelected = false,
  onToggleSelect,
  onClick,
  onContextMenu,
  onMouseEnter,
  onMouseLeave,
  thumbnailContent,
  textContent,
  className = "",
  useCheckbox = true,
  useDots = true
}) => {
  const handleClick = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    onClick?.();
  };

  return (
    <div
      onClick={handleClick}
      onContextMenu={onContextMenu}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      className={`rounded-xs group w-full ${className}`}
    >
      <div className={`relative`}>
        {useCheckbox && onToggleSelect && (
          <Checkbox
          checked={isSelected}
          onChange={onToggleSelect}
          size="lg"
          showOnGroupHover={true}
          className="absolute top-2 left-2 z-10"/>
        )}

        {useDots && (
          <button onClick={onContextMenu} className="min-md:hidden flex group-hover:flex absolute top-2 right-2 z-10">
            <HorizontaLDots className="min-md:h-6 min-md:w-6 w-7 h-7 text-black bg-white shadow-sm hover:bg-gray-300 active:bg-gray-400 rounded-sm animate transition-colors duration-100 ease-in" viewBox="0 0 24 24"/>
          </button>
        )}

        {thumbnailContent}
      </div>

      {/* Info section */}
      {textContent && (
        <div>
          {textContent}
        </div>
      )}
    </div>
  );
};
