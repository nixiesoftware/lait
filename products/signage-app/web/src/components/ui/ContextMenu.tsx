import React, { useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';

export interface ContextMenuItem {
  label: string;
  icon?: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  divider?: boolean;
}

interface ContextMenuProps {
  isOpen: boolean;
  position: { x: number; y: number };
  onClose: () => void;
  header?: React.ReactNode;
  items: ContextMenuItem[];
}

export const ContextMenu: React.FC<ContextMenuProps> = ({
  isOpen,
  position,
  onClose,
  header,
  items
}) => {
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        // Call onClose which will set the justClosed flag in the hook
        onClose();
      }
    };

    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        onClose();
      }
    };

    // Add listeners
    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleEscape);

    // Cleanup
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [isOpen, onClose]);

  useEffect(() => {
    if (!isOpen || !menuRef.current) return;

    const menu = menuRef.current;
    const viewportW = window.innerWidth;
    const viewportH = window.innerHeight;
    const margin = 8; // minimal margin to keep menu away from edges

    // Measure after it's laid out once at the requested position
    const rect = menu.getBoundingClientRect();

    // Prefer placing slightly below/right of the cursor
    let x = position.x + 10;
    let y = position.y + 10;

    // If overflowing right, flip to the left side of the cursor
    if (x + rect.width > viewportW - margin) {
      x = position.x - rect.width - 10;
    }
    // If still overflowing or negative, clamp within viewport
    if (x < margin) x = margin;
    if (x + rect.width > viewportW - margin) x = Math.max(margin, viewportW - rect.width - margin);

    // If overflowing bottom, flip above the cursor
    if (y + rect.height > viewportH - margin) {
      y = position.y - rect.height - 10;
    }
    // Clamp top/bottom within viewport
    if (y < margin) y = margin;
    if (y + rect.height > viewportH - margin) y = Math.max(margin, viewportH - rect.height - margin);

    // Apply adjusted position
    menu.style.left = `${x}px`;
    menu.style.top = `${y}px`;
  }, [isOpen, position]);

  if (!isOpen) return null;

  const menuContent = (
    <div
      ref={menuRef}
      className="fixed z-50 min-w-[200px] bg-white dark:bg-gray-800 rounded-md shadow-lg border border-gray-200 dark:border-gray-700 py-1 context-menu-bounce"
      style={{
        // Seed with the requested position; an effect will immediately adjust
        left: `${position.x}px`,
        top: `${position.y}px`
      }}
    >
      {header && (
        <>
          <div className="px-3 py-2 text-sm">
            {header}
          </div>
          <div className="border-t border-gray-200 dark:border-gray-700" />
        </>
      )}

      {items.map((item, index) => {
        if (item.divider) {
          return (
            <div
              key={`divider-${index}`}
              className="border-t border-gray-200 dark:border-gray-700 my-1"
            />
          );
        }

        return (
          <button
            key={index}
            onClick={() => {
              if (!item.disabled && item.onClick) {
                item.onClick();
                onClose();
              }
            }}
            disabled={item.disabled}
            className={`w-full px-3 py-2 text-left text-sm flex items-center gap-2 transition-colors
              ${item.disabled
                ? 'text-gray-400 dark:text-gray-500 cursor-not-allowed'
                : 'text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700'
              }`}
          >
            {item.icon && <span className="w-4 h-4">{item.icon}</span>}
            <span>{item.label}</span>
          </button>
        );
      })}
    </div>
  );

  // Use portal to render at document root
  return typeof document !== 'undefined' ? createPortal(menuContent, document.body) : null;
};
