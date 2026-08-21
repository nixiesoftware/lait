import React, { ReactNode } from "react";
import { Checkbox } from "./checkbox/Checkbox";

interface BaseListItemProps {
  isSelected?: boolean;
  isHighlighted?: boolean;
  onToggleSelect?: () => void;
  onClick?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  onMouseEnter?: () => void;
  onMouseLeave?: () => void;
  children: ReactNode;
  className?: string;
  showCheckbox?: boolean;
  isDesktop?: boolean;
}

export const BaseListItem: React.FC<BaseListItemProps> = ({
  isSelected = false,
  isHighlighted = false,
  onToggleSelect,
  onClick,
  onContextMenu,
  onMouseEnter,
  onMouseLeave,
  children,
  className = "",
  showCheckbox = true,
  isDesktop = true
}) => {
  const handleClick = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    onClick?.();
  };

  return (
    <tr
      onClick={handleClick}
      onContextMenu={onContextMenu}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      className={`cursor-pointer group ring-offset-2 ring-inset rounded-sm
        ${isSelected ? 'ring-1 ring-brand-400 bg-brand-400/20 dark:bg-blue-800/20' : ''}
        ${isHighlighted && !isSelected ? 'ring-brand-200' : ''} ${className}`}
    >
      {showCheckbox && (
        <td className="max-sm:hidden px-1 py-4 whitespace-nowrap align-middle text-center w-12">
          <Checkbox
            checked={isSelected}
            onChange={() => onToggleSelect?.()}
            size={isDesktop ? 'md' : 'sm'}
            showOnGroupHover={isDesktop}
          />
        </td>
      )}
      {children}
    </tr>
  );
};
