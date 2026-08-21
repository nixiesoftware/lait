import React, { useRef, useState, useEffect, ReactNode } from "react";
import { X } from "lucide-react";
import { ScrollingText } from "@/components/ui/ScrollingText";
import { PencilIcon } from "../../../public/images/icons/theme-icons";
import { useIsTabletUp } from "@/hooks/useResponsive";

export interface BaseDetailsModalProps {
  isOpen: boolean;
  onClose: () => void;
  title: string;
  onUpdateTitle?: (newTitle: string) => void;
  mediaPreview?: ReactNode;
  detailsSections?: ReactNode;
  actionButtons?: ReactNode;
  className?: string;
}

export const BaseDetailsModal: React.FC<BaseDetailsModalProps> = ({
  isOpen,
  onClose,
  title,
  onUpdateTitle,
  mediaPreview,
  detailsSections,
  actionButtons,
  className = ""
}) => {
  const [touchStart, setTouchStart] = useState(0);
  const [touchEnd, setTouchEnd] = useState(0);
  const [isModalReady, setIsModalReady] = useState(false);
  const [isEditingTitle, setIsEditingTitle] = useState(false);
  const [editedTitle, setEditedTitle] = useState("");
  const [originalTitle, setOriginalTitle] = useState("");
  const [isTitleHovered, setIsTitleHovered] = useState(false);
  const modalRef = useRef<HTMLDivElement>(null);
  const titleInputRef = useRef<HTMLInputElement>(null);
  
  // Use the responsive hook to determine desktop/tablet view
  const { isTabletUp } = useIsTabletUp();

  useEffect(() => {
    if (isOpen) {
      setTouchStart(0);
      setTouchEnd(0);
      setEditedTitle(title);
      setOriginalTitle(title);
      requestAnimationFrame(() => {
        setIsModalReady(true);
      });
    } else {
      setIsModalReady(false);
    }
  }, [isOpen, title]);

  useEffect(() => {
    if (isEditingTitle && titleInputRef.current) {
      titleInputRef.current.focus();
      titleInputRef.current.select();
    }
  }, [isEditingTitle]);

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

  const handleTitleClick = () => {
    if (onUpdateTitle) {
      setIsEditingTitle(true);
    }
  };

  const handleTitleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setEditedTitle(e.target.value);
  };

  const handleTitleBlur = () => {
    setIsEditingTitle(false);
    if (editedTitle !== originalTitle && editedTitle.trim() !== "" && onUpdateTitle) {
      setOriginalTitle(editedTitle);
      if (isTabletUp) {
        onUpdateTitle(editedTitle.trim());
      }
    }
  };

  const handleTitleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.currentTarget.blur();
    } else if (e.key === "Escape") {
      setEditedTitle(originalTitle);
      setIsEditingTitle(false);
    }
  };

  const handleClose = () => {
    setIsEditingTitle(false);
    setIsModalReady(false);
    if (!isTabletUp && onUpdateTitle && editedTitle !== title && editedTitle.trim() !== "") {
      onUpdateTitle(editedTitle.trim());
    }
    onClose();
  };

  if (!isOpen) return null;

  // Desktop/Tablet sidebar layout
  if (isTabletUp) {
    return (
      <div
        ref={modalRef}
        className={`modal-right border-gray-200 bottom-3 right-3 z-999999 ${
          isModalReady ? 'translate-x-0' : 'translate-x-full'
        } transform transition-transform duration-200 ease-[cubic-bezier(0.645,0.045,0.355,1)] h-full overflow-clip bg-white dark:bg-gray-900 border-l border-gray-200 dark:border-gray-700 ${className}`}
        style={{
          width: '33.33vw'
        }}
      >
        <div className="h-full flex flex-col">
          {/* Header with title and close button */}
          <div className="flex items-center justify-between px-3 py-2 border-b border-gray-200 dark:border-gray-700">
            {isEditingTitle && onUpdateTitle ? (
              <input
                ref={titleInputRef}
                type="text"
                value={editedTitle}
                onChange={handleTitleChange}
                onBlur={handleTitleBlur}
                onKeyDown={handleTitleKeyDown}
                className="overflow-ellipsis max-w-70 text-md w-full font-semibold bg-transparent outline-none border-b-gray-300 dark:border-b-gray-700 border-b-2 border-dotted dark:text-white focus:border-b-2 dark:focus:border-b-2"
              />
            ) : (
              <span
                onClick={handleTitleClick}
                onMouseEnter={() => setIsTitleHovered(true)}
                onMouseLeave={() => setIsTitleHovered(false)}
                className={`flex items-center gap-x-1 group max-w-60 ${onUpdateTitle ? 'cursor-pointer' : ''}`}
              >
                <ScrollingText
                  text={editedTitle}
                  className={`text-md font-semibold dark:text-white ${
                    onUpdateTitle ? 'cursor-text border-b-2 border-dotted border-transparent group-hover:border-b-gray-300 group-hover:text-gray-800 dark:group-hover:border-b-gray-700' : ''
                  } transition-all`}
                  speed={30}
                  delay={500}
                  isParentHovered={isTitleHovered}
                />
                {onUpdateTitle && (
                  <PencilIcon className="group-hover:text-gray-700 dark:group-hover:text-gray-500 transition-all cursor-pointer dark:text-gray-400 flex-shrink-0" width={"24px"} height={"24px"} viewBox={"0 0 22 22"}/>
                )}
              </span>
            )}
            <button
              onClick={handleClose}
              className="flex h-7 w-7 align-right items-center justify-center rounded-full text-gray-400 transition-colors hover:bg-gray-200 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-white"
            >
              <X className="w-6 h-6"/>
            </button>
          </div>

          {/* Scrollable uploads */}
          <div className="flex-1 overflow-y-auto">
            {/* Media Preview */}
            {mediaPreview && (
              <div className="mb-2 bg-gray-100 dark:bg-gray-800 border-b-gray-200 border-b-1">
                <div className="overflow-hidden shadow-inner flex items-center justify-center">
                  {mediaPreview}
                </div>
              </div>
            )}

            {/* Details Section */}
            {detailsSections && (
              <div className="space-y-2 p-3 pt-0">
                {detailsSections}
              </div>
            )}
          </div>

          {/* Button Section */}
          {actionButtons && (
            <div className="px-6 py-4 border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
              {actionButtons}
            </div>
          )}
        </div>
      </div>
    );
  }

  // Mobile modal layout
  return (
    <>
      {/* Darkened and blurred background overlay */}
      <div
        className={`z-[9999999] fixed inset-0 bg-black/50 backdrop-blur-sm transition-opacity duration-300 ${
          isModalReady ? 'opacity-100' : 'opacity-0'
        }`}
        onClick={handleClose}
      />

      {/* Modal Content - Slides up from bottom */}
      <div
        ref={modalRef}
        className={`z-[9999999] max-w-vw fixed bottom-0 transform transition-transform duration-300 ease-out left-0 ${
          isModalReady ? 'translate-y-0' : 'translate-y-full'
        } ${className}`}
        style={{ height: 'calc(100vh - 3rem)', width: '100vw', top: '3rem' }}
        onTouchStart={handleTouchStart}
        onTouchMove={handleTouchMove}
        onTouchEnd={handleTouchEnd}
      >
        <div className="bg-white dark:bg-gray-900 rounded-t-lg shadow-2xl h-full flex flex-col">
          {/* Header with title and close button */}
          <div className="flex items-center justify-between px-6 py-4 border-b border-gray-200 dark:border-gray-700">
            {isEditingTitle && onUpdateTitle ? (
              <input
                ref={titleInputRef}
                type="text"
                value={editedTitle}
                onChange={handleTitleChange}
                onBlur={handleTitleBlur}
                onKeyDown={handleTitleKeyDown}
                className="text-2xl w-full font-semibold bg-transparent outline-none border-b-gray-300 dark:border-b-gray-700 border-b-2 border-dotted dark:text-white focus:border-b-2 dark:focus:border-b-2"
              />
            ) : (
              <span
                onClick={handleTitleClick}
                onMouseEnter={() => setIsTitleHovered(true)}
                onMouseLeave={() => setIsTitleHovered(false)}
                className={`flex items-center gap-x-1 group max-w-full ${onUpdateTitle ? 'cursor-pointer' : ''}`}
              >
                <ScrollingText
                  text={editedTitle}
                  className={`text-2xl font-semibold dark:text-white ${
                    onUpdateTitle ? 'cursor-text border-b-2 border-dotted border-transparent group-hover:border-b-gray-300 group-hover:text-gray-800 dark:group-hover:border-b-gray-700' : ''
                  } transition-all`}
                  speed={30}
                  delay={500}
                  isParentHovered={isTitleHovered}
                />
                {onUpdateTitle && (
                  <PencilIcon className="group-hover:text-gray-700 dark:group-hover:text-gray-500 transition-all cursor-pointer dark:text-gray-400 flex-shrink-0" width={"26px"} height={"26px"} viewBox={"0 0 22 22"}/>
                )}
              </span>
            )}
            <button
              onClick={handleClose}
              className="flex h-10 w-10 align-right items-center justify-center rounded-full text-gray-400 transition-colors hover:bg-gray-200 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-white"
            >
              <X className="w-6 h-6"/>
            </button>
          </div>

          {/* Swipe indicator for mobile */}
          <div className="flex justify-center py-2 lg:hidden">
            <div className="w-12 h-1 bg-gray-300 dark:bg-gray-600 rounded-full" />
          </div>

          {/* Scrollable uploads */}
          <div className="flex-1 overflow-y-auto px-6 py-6">
            {/* Centered media preview */}
            {mediaPreview && (
              <div className="max-w-4xl mx-auto mb-8">
                <div className="aspect-video bg-gray-100 dark:bg-gray-800 rounded-xl overflow-hidden shadow-inner flex items-center justify-center">
                  {mediaPreview}
                </div>
              </div>
            )}

            {/* Details section */}
            {detailsSections && (
              <div className="max-w-2xl mx-auto space-y-4">
                {detailsSections}
              </div>
            )}
          </div>

          {/* Fixed button row at bottom */}
          {actionButtons && (
            <div className="px-6 py-4 border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
              {actionButtons}
            </div>
          )}
        </div>
      </div>
    </>
  );
};
