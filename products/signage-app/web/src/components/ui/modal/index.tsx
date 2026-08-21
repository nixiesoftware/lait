import React, { useRef, useEffect } from "react";
import { AnimatePresence, motion } from "framer-motion";

interface ModalProps {
  isOpen: boolean;
  onClose: () => void;
  className?: string;
  transformOrigin?: string;
  children: React.ReactNode;
  showCloseButton?: boolean; // New prop to control close button visibility
  isFullscreen?: boolean; // Default to false for backwards compatibility
  hideOverlay?: boolean;
  /**
   * Animation mode for the inner content.
   * - "spring": slide-in with spring and subtle blur fade (default)
   * - "size": slide-in with width/height animation instead of scale to preserve pseudo-element size
   */
  animationMode?: "spring" | "size";
}

export const Modal: React.FC<ModalProps> = ({
  isOpen,
  onClose,
  children,
  className,
  transformOrigin = "bottom",
  showCloseButton = true, // Default to true for backwards compatibility
  isFullscreen = false,
  hideOverlay = false,
}) => {
  const modalRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };

    if (isOpen) {
      document.addEventListener("keydown", handleEscape);
    }

    return () => {
      document.removeEventListener("keydown", handleEscape);
    };
  }, [isOpen, onClose]);

  useEffect(() => {
    // Body overflow is now controlled by the global overflow-hidden class
    // We no longer need to manipulate it here
  }, [isOpen]);

  const contentClasses = isFullscreen
    ? "w-full h-full"
    : `fixed w-full h-full bg-white`;

  return (
    <AnimatePresence>
      {isOpen && (
        <div className={`flex items-center justify-center`}>
          {!hideOverlay && (
            <motion.div
              className="absolute inset-0 h-full w-full bg-gray-100/50 backdrop-blur-[2px]"
              onClick={onClose}
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              transition={{ duration: 0.2, ease: "easeOut" }}
            />
          )}
          <motion.div
            ref={modalRef}
            initial={{ opacity: 0, translateY: -50, rotateX: 100, transformOrigin: `${transformOrigin}` }}
            animate={{  opacity: 1, translateY: 0, rotateX: 0, transformOrigin: `${transformOrigin}` }}
            exit={{  opacity: 0, translateY: -200, rotateX: 0, transformOrigin: `${transformOrigin}` }}
            transition={{ type: "spring", stiffness: 280, damping: 25, mass: 0.8 }}
            className={`${contentClasses} ${className}`}
            role="dialog"
            aria-modal="true"
            onClick={(e) => e.stopPropagation()}
          >
            {showCloseButton && (
              <button
                onClick={onClose}
                className="absolute right-1 top-1 z-999 flex h-9.5 w-9.5 items-center justify-center rounded-full text-gray-400 transition-colors hover:bg-gray-200 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-white sm:right-2 sm:top-2 sm:h-8 sm:w-8"
              >
                <svg
                  width="24"
                  height="24"
                  viewBox="0 0 24 24"
                  fill="none"
                  xmlns="http://www.w3.org/2000/svg"
                >
                  <path
                    fillRule="evenodd"
                    clipRule="evenodd"
                    d="M6.04289 16.5413C5.65237 16.9318 5.65237 17.565 6.04289 17.9555C6.43342 18.346 7.06658 18.346 7.45711 17.9555L11.9987 13.4139L16.5408 17.956C16.9313 18.3466 17.5645 18.3466 17.955 17.956C18.3455 17.5655 18.3455 16.9323 17.955 16.5418L13.4129 11.9997L17.955 7.4576C18.3455 7.06707 18.3455 6.43391 17.955 6.04338C17.5645 5.65286 16.9313 5.65286 16.5408 6.04338L11.9987 10.5855L7.45711 6.0439C7.06658 5.65338 6.43342 5.65338 6.04289 6.0439C5.65237 6.43442 5.65237 7.06759 6.04289 7.45811L10.5845 11.9997L6.04289 16.5413Z"
                    fill="currentColor"
                  />
                </svg>
              </button>
            )}
              {children}
          </motion.div>
        </div>
      )}
    </AnimatePresence>
  );
};
