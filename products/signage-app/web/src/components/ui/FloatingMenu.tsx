import React, { useEffect, useRef, useState } from "react";

interface FloatingMenuProps {
  isOpen: boolean;
  onClose: () => void;
  anchorRef: React.RefObject<HTMLElement | null>;
  children: React.ReactNode;
  className?: string;
  position?: "top" | "bottom" | "left" | "right";
  align?: "start" | "center" | "end";
}

export const FloatingMenu: React.FC<FloatingMenuProps> = ({
  isOpen,
  onClose,
  anchorRef,
  children,
  className = "",
  position = "top",
  align = "center"
}) => {
  const menuRef = useRef<HTMLDivElement>(null);
  const [menuPosition, setMenuPosition] = useState({ top: 0, left: 0 });

  // Calculate menu position based on anchor element
  useEffect(() => {
    if (isOpen && anchorRef.current && menuRef.current) {
      const anchorRect = anchorRef.current.getBoundingClientRect();
      const menuRect = menuRef.current.getBoundingClientRect();
      
      let top = 0;
      let left = 0;

      // Position calculation
      switch (position) {
        case "top":
          top = anchorRect.top - menuRect.height - 8;
          break;
        case "bottom":
          top = anchorRect.bottom + 8;
          break;
        case "left":
          left = anchorRect.left - menuRect.width - 8;
          top = anchorRect.top;
          break;
        case "right":
          left = anchorRect.right + 8;
          top = anchorRect.top;
          break;
      }

      // Alignment calculation
      if (position === "top" || position === "bottom") {
        switch (align) {
          case "start":
            left = anchorRect.left;
            break;
          case "center":
            left = anchorRect.left + (anchorRect.width - menuRect.width) / 2;
            break;
          case "end":
            left = anchorRect.right - menuRect.width;
            break;
        }
      } else {
        switch (align) {
          case "start":
            top = anchorRect.top;
            break;
          case "center":
            top = anchorRect.top + (anchorRect.height - menuRect.height) / 2;
            break;
          case "end":
            top = anchorRect.bottom - menuRect.height;
            break;
        }
      }

      // Ensure menu stays within viewport
      const padding = 10;
      top = Math.max(padding, Math.min(top, window.innerHeight - menuRect.height - padding));
      left = Math.max(padding, Math.min(left, window.innerWidth - menuRect.width - padding));

      setMenuPosition({ top, left });
    }
  }, [isOpen, position, align, anchorRef]);

  // Handle click outside
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        isOpen &&
        menuRef.current &&
        anchorRef.current &&
        !menuRef.current.contains(event.target as Node) &&
        !anchorRef.current.contains(event.target as Node)
      ) {
        onClose();
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, [isOpen, onClose, anchorRef]);

  if (!isOpen) return null;

  return (
    <div
      ref={menuRef}
      className={`fixed z-50 context-menu-bounce ${className}`}
      style={{
        top: `${menuPosition.top}px`,
        left: `${menuPosition.left}px`
      }}
      onClick={(e) => e.stopPropagation()}
    >
      {children}
    </div>
  );
};