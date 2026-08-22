import React, { useRef, useState, useEffect, ReactNode } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";

export interface SidebarContentModalProps {
  isOpen: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  className?: string;
}

export const SidebarContentModal: React.FC<SidebarContentModalProps> = ({
  isOpen,
  onClose,
  title,
  children,
  className = "",
}) => {
  const [touchStart, setTouchStart] = useState(0);
  const [touchEnd, setTouchEnd] = useState(0);
  const [isModalReady, setIsModalReady] = useState(false);
  const modalRef = useRef<HTMLDivElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (isOpen) {
      setTouchStart(0);
      setTouchEnd(0);
      requestAnimationFrame(() => {
        setIsModalReady(true);
      });

      // Prevent body scroll when modal is open
      document.body.style.overflow = 'hidden';
    } else {
      setIsModalReady(false);
      document.body.style.overflow = '';
    }

    return () => {
      document.body.style.overflow = '';
    };
  }, [isOpen]);

  // Prevent outside components from receiving events when modal is open
  useEffect(() => {
    if (!isOpen) return;

    const handleGlobalMouseDown = (e: MouseEvent) => {
      // Only stop propagation to document-level handlers, not within the modal
      if (backdropRef.current?.contains(e.target as Node) ||
          modalRef.current?.contains(e.target as Node)) {
        // Stop the event from bubbling to document-level handlers
        // but allow it to work within the modal
        e.stopPropagation();
      }
    };

    // Use capture phase to intercept before bubble phase document handlers
    document.addEventListener('mousedown', handleGlobalMouseDown, true);

    return () => {
      document.removeEventListener('mousedown', handleGlobalMouseDown, true);
    };
  }, [isOpen]);

  const handleTouchStart = (e: React.TouchEvent) => {
    setTouchStart(e.targetTouches[0].clientY);
  };

  const handleTouchMove = (e: React.TouchEvent) => {
    setTouchEnd(e.targetTouches[0].clientY);
  };

  const handleTouchEnd = () => {
    if (!touchStart || !touchEnd) return;

    const distance = touchStart - touchEnd;
    const isSwipeDown = distance < -50;

    if (isSwipeDown && touchStart < 100) {
      onClose();
    }
  };

  const handleClose = () => {
    setIsModalReady(false);
    onClose();
  };

  if (!isOpen) return null;
  if (typeof document === 'undefined') return null;

  // Render the modal in a portal to escape any transformed/overflow-hidden ancestors
  return createPortal(
    <>
      {/* Darkened and blurred background overlay */}
      <div
        ref={backdropRef}
        className={`z-[10000] fixed inset-0 bg-black/50 backdrop-blur-sm transition-opacity duration-300 ${
          isModalReady ? 'opacity-100' : 'opacity-0'
        }`}
        onMouseDown={(e) => {
          // Just stop propagation, don't use stopImmediatePropagation
          e.stopPropagation();
        }}
      />

      {/* Modal Content - Slides up from bottom */}
      <div
        ref={modalRef}
        className={`z-[10001] fixed bottom-0 left-0 w-screen transform transition-transform duration-300 ease-out ${
          isModalReady ? 'translate-y-0' : 'translate-y-full'
        } ${className}`}
        // Use top + bottom to reserve header space reliably across mobile browsers
        style={{ top: '6rem', bottom: 0 }}
        onTouchStart={handleTouchStart}
        onTouchMove={handleTouchMove}
        onTouchEnd={handleTouchEnd}
      >
        <div className="bg-white dark:bg-gray-800 rounded-t-2xl shadow-2xl h-full flex flex-col">
          {/* Header with title and close button */}
          <div className="flex items-center justify-between px-4 py-4 border-b border-gray-200 dark:border-gray-700">
            <h3 className="text-lg font-semibold text-gray-900 dark:text-white">
              {title}
            </h3>
            <button
              onClick={handleClose}
              className="flex h-10 w-10 align-right items-center justify-center rounded-full text-gray-400 transition-colors hover:bg-gray-200 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-gray-700 dark:hover:text-white"
            >
              <X className="w-5 h-5"/>
            </button>
          </div>

          {/* Swipe indicator for mobile */}
          <div className="flex justify-center py-2">
            <div className="w-12 h-1 bg-gray-300 dark:bg-gray-600 rounded-full" />
          </div>

          {/* Scrollable uploads */}
          <div className="flex-1 overflow-hidden">
            {children}
          </div>
        </div>
      </div>
    </>,
    document.body
  );
};
