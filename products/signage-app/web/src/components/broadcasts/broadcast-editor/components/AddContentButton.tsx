import React, { useState, useRef, useEffect } from "react";
import { createPortal } from "react-dom";
import { Plus, X, ArrowLeft } from "lucide-react";
import { BagIcon, CloudUploadIcon } from "../../../../../public/images/icons/theme-icons";
import ContentLibrary from "@/components/broadcasts/broadcast-editor/components/ContentLibrary";
import IntegrationLibrary from "@/components/broadcasts/broadcast-editor/components/IntegrationLibrary";
import { SidebarContentModal } from "@/components/ui/SidebarContentModal";
import type { SignageMedia } from "@/utils/lait/types";
import type { Integration } from "@/components/integrations/types/integrations";
import { AnimatePresence, motion } from "framer-motion";

interface AddContentButtonProps {
  allContent?: SignageMedia[];
  integrations?: Integration[];
  onSelectIntegration?: (integration: Integration) => void;
  onContentUploaded?: () => void;
  onAddContentItem?: (media: SignageMedia) => void;
}

const AddContentButton = React.memo(({
  allContent,
  integrations,
  onSelectIntegration,
  onContentUploaded,
  onAddContentItem
}: AddContentButtonProps) => {
  const [showAddMenu, setShowAddMenu] = useState(false);
  const addBtnRef = useRef<HTMLDivElement | null>(null);
  const addMenuRef = useRef<HTMLDivElement | null>(null);
  const [menuPosition, setMenuPosition] = useState<{ left: number; top?: number; bottom?: number; placeBelow: boolean; width?: number } | null>(null);
  const [panelView, setPanelView] = useState<'none' | 'files' | 'apps'>('none');
  const [isMobile, setIsMobile] = useState(false);
  const [isMobileModalOpen, setIsMobileModalOpen] = useState(false);
  const [mobileView, setMobileView] = useState<'chooser' | 'files' | 'apps'>('chooser');

  // Determine mobile/desktop
  useEffect(() => {
    const update = () => setIsMobile(typeof window !== 'undefined' ? window.innerWidth < 768 : false);
    update();
    window.addEventListener('resize', update);
    return () => window.removeEventListener('resize', update);
  }, []);

  // Close on outside click / escape
  useEffect(() => {
    if (!showAddMenu) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (addBtnRef.current && !addBtnRef.current.contains(event.target as Node)) {
        setShowAddMenu(false);
      }
    };

    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setShowAddMenu(false);
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleEscape);

    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [showAddMenu, addBtnRef]);

  // Position the floating menu near the + button
  useEffect(() => {
    if (!showAddMenu && panelView === 'none') return;
    const updatePosition = () => {
      if (!addBtnRef.current) return;
      const rect = addBtnRef.current.getBoundingClientRect();
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const margin = 12;
      const centerX = rect.left + rect.width / 2;

      if (panelView === 'none') {
        const menuHeight = 120;
        const left = centerX - 48;
        const spaceBelow = vh - rect.bottom;
        const placeBelow = spaceBelow >= menuHeight + margin;

        if (placeBelow) {
          const top = rect.bottom + 8;
          setMenuPosition({ left, top, placeBelow });
        } else {
          const bottom = vh - rect.top + 8;
          setMenuPosition({ left, bottom, placeBelow });
        }
      } else {
        if (isMobile) return;

        const panelMaxWidth = 520;
        const panelWidth = Math.min(panelMaxWidth, vw - margin * 2);
        const panelHeight = Math.min(Math.round(vh * 0.6), vh - margin * 2);

        const defaultLeft = centerX - panelWidth / 2;
        const left = Math.max(margin, Math.min(vw - panelWidth - margin, Math.round(defaultLeft)));

        const spaceBelow = vh - rect.bottom;
        const placeBelow = spaceBelow >= panelHeight + margin;

        let bottom: number;
        if (placeBelow) {
          bottom = rect.bottom - margin;
          if (bottom + panelHeight > vh - margin) {
            bottom = vh - panelHeight - margin;
          }
        } else {
          bottom = vh - rect.top + 8;
          if (bottom < margin) {
            bottom = margin;
          }
        }

        setMenuPosition({ left, top: undefined, placeBelow, width: panelWidth, bottom: bottom });
      }
    };
    updatePosition();
    window.addEventListener('scroll', updatePosition, true);
    window.addEventListener('resize', updatePosition);

    const timelineScroller = document.querySelector('[data-timeline-scroll]');
    if (timelineScroller) {
      timelineScroller.addEventListener('scroll', updatePosition, { passive: true });
    }

    return () => {
      window.removeEventListener('scroll', updatePosition, true);
      window.removeEventListener('resize', updatePosition);
      if (timelineScroller) {
        timelineScroller.removeEventListener('scroll', updatePosition);
      }
    };
  }, [showAddMenu, panelView, isMobile]);

  return (
    <div
      ref={addBtnRef}
      data-testid="add-tile"
      className="group relative h-20 w-20 rounded-md mr-3 flex items-center justify-center
                 bg-gray-200 dark:bg-gray-800/50 hover:bg-gray-300 dark:hover:bg-gray-600
                 transition-all duration-200 cursor-pointer"
      onClick={(e) => {
        e.stopPropagation();
        if (panelView !== 'none') {
          setPanelView('none');
        }
        setShowAddMenu((v) => !v);
      }}
      title="Add content to broadcast"
    >
      <div className="flex items-center justify-center w-10 h-10">
        <Plus className="w-8 h-8 text-gray-500 dark:text-gray-600
                       group-hover:text-gray-600 dark:group-hover:text-gray-900 transition-colors" />
      </div>

      {/* Floating chooser menu (mobile and desktop). Large panel is desktop-only. */}
      {menuPosition && typeof document !== 'undefined' && createPortal(
        <AnimatePresence>
          {showAddMenu && panelView === 'none' ? (
            <motion.div
              key="add-chooser"
              ref={addMenuRef}
              initial={{ opacity: 0, y: (menuPosition.placeBelow ? 8 : -8), scale: 0.96 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: (menuPosition.placeBelow ? 6 : -6), scale: 0.98 }}
              transition={{ duration: 0.18, ease: [0.2, 0.8, 0.2, 1] }}
              style={{
                position: 'fixed',
                left: menuPosition.left,
                top: menuPosition.top,
                bottom: menuPosition.bottom,
                transform: 'translateX(-50%)',
                width: 95
              }}
              className="z-[10000] p-2"
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
              onWheel={(e) => e.stopPropagation()}
            >
              <motion.div
                className="grid grid-cols-1 gap-2"
                initial="hidden"
                animate="show"
                exit="hidden"
                variants={{
                  hidden: { transition: { staggerChildren: 0.04, staggerDirection: -1 } },
                  show: { transition: { staggerChildren: 0.06, delayChildren: 0.05 } },
                }}
              >
                <motion.button
                  className="flex flex-col align-middle justify-center items-center aspect-square gap-1 w-full py-2 rounded-md bg-white shadow-md hover:bg-gray-50 dark:hover:bg-gray-600 text-xs font-medium text-gray-800 dark:text-gray-100"
                  onClick={(e) => {
                    e.stopPropagation();
                    setShowAddMenu(false);
                    if (isMobile) {
                      setMobileView('files');
                      setIsMobileModalOpen(true);
                    } else {
                      setPanelView('files');
                    }
                  }}
                  variants={{ hidden: { opacity: 0, y: 8 }, show: { opacity: 1, y: 0 } }}
                  transition={{ duration: 0.16, ease: [0.2, 0.8, 0.2, 1] }}
                >
                  <CloudUploadIcon className="w-7 h-7"/>
                  Uploads
                </motion.button>
                <motion.button
                  className="flex flex-col align-middle justify-center items-center aspect-square gap-1 w-full py-2 rounded-sm bg-white shadow-md hover:bg-gray-100 dark:hover:bg-gray-600 text-xs font-medium text-gray-800 dark:text-gray-100"
                  onClick={(e) => {
                    e.stopPropagation();
                    setShowAddMenu(false);
                    if (isMobile) {
                      setMobileView('apps');
                      setIsMobileModalOpen(true);
                    } else {
                      setPanelView('apps');
                    }
                  }}
                  variants={{ hidden: { opacity: 0, y: 8 }, show: { opacity: 1, y: 0 } }}
                  transition={{ duration: 0.16, ease: [0.2, 0.8, 0.2, 1] }}
                >
                  <BagIcon className="w-7 h-7"/>
                  Apps
                </motion.button>
              </motion.div>
            </motion.div>
          ) : null}
        </AnimatePresence>,
        document.body
      )}

      {menuPosition && typeof document !== 'undefined' && !isMobile && createPortal(
        <AnimatePresence>
          {panelView !== 'none' ? (
            <motion.div
              key="add-panel"
              ref={addMenuRef}
              initial={{ opacity: 0, y: (menuPosition.placeBelow ? 8 : -8), scale: 0.98 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: (menuPosition.placeBelow ? 6 : -6), scale: 0.98 }}
              transition={{ duration: 0.3, ease: [0.2, 0.8, 0.2, 1] }}
              style={{
                position: 'fixed',
                left: menuPosition.left,
                bottom: menuPosition.bottom,
                ...(menuPosition.top === undefined && menuPosition.bottom !== undefined ? { top: menuPosition.top } : {}),
                width: menuPosition.width ? `${menuPosition.width}px` : '520px'
              }}
              className={`z-99 max-h-[60vh] overflow-y-auto bg-white dark:bg-gray-800 rounded-md shadow-xl border border-gray-200 dark:border-gray-700`}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
              onWheel={(e) => e.stopPropagation()}
            >
              {/* Panel header */}
              <div className="flex items-center justify-between px-3 py-2 border-b border-gray-200 dark:border-gray-700">
                <div className="flex items-center gap-2">
                  <button
                    className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700"
                    onClick={(e) => { e.stopPropagation(); setPanelView('none'); setShowAddMenu(true); }}
                    aria-label="Back"
                  >
                    <ArrowLeft className="w-4 h-4"/>
                  </button>
                  <span className="text-sm font-medium text-gray-700 dark:text-gray-200">
                    {panelView === 'files' ? 'Uploads' : 'Apps'}
                  </span>
                </div>
                <button
                  className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700"
                  onClick={(e) => { e.stopPropagation(); setPanelView('none'); setShowAddMenu(false); }}
                  aria-label="Close"
                >
                  <X className="w-4 h-4"/>
                </button>
              </div>
              {/* Panel content */}
              <div className="max-h-[35vh] overflow-x-hidden overflow-y-auto p-3 pb-20" onMouseDown={(e) => e.stopPropagation()} onClick={(e) => e.stopPropagation()} onWheel={(e) => e.stopPropagation()}>
                {panelView === 'files' && allContent && (
                  <ContentLibrary
                    allContent={allContent}
                    onAddContent={(content) => {
                      if (onAddContentItem) onAddContentItem(content);
                    }}
                    onContentUploaded={onContentUploaded}
                  />
                )}
                {panelView === 'apps' && integrations && (
                  <IntegrationLibrary
                    integrations={integrations}
                    onActivate={(integration) => {
                      if (onSelectIntegration) {
                        onSelectIntegration(integration);
                      }
                      setPanelView('none');
                      setShowAddMenu(false);
                    }}
                  />
                )}
              </div>
            </motion.div>
          ) : null}
        </AnimatePresence>,
        document.body
      )}

      {/* Mobile slide-up modal */}
      {isMobile && (
        <SidebarContentModal
          isOpen={isMobileModalOpen}
          onClose={() => {
            setIsMobileModalOpen(false);
          }}
          title={mobileView === 'files' ? 'Uploads' : 'Apps'}
          className=""
        >
          <div className="h-full w-full">
            {mobileView === 'files' && allContent && (
              <div className="h-full overflow-y-auto">
                <ContentLibrary
                  allContent={allContent}
                  onAddContent={(content) => {
                    if (onAddContentItem) onAddContentItem(content);
                  }}
                  onRequestClose={() => setIsMobileModalOpen(false)}
                  onContentUploaded={onContentUploaded}
                />
              </div>
            )}
            {mobileView === 'apps' && integrations && (
              <div className="h-full overflow-y-auto">
                <IntegrationLibrary
                  integrations={integrations}
                  onActivate={(integration) => {
                    if (onSelectIntegration) {
                      onSelectIntegration(integration);
                    }
                    setIsMobileModalOpen(false);
                  }}
                />
              </div>
            )}
          </div>
        </SidebarContentModal>
      )}
    </div>
  );
}, (prevProps, nextProps) => {
  return prevProps.allContent === nextProps.allContent && prevProps.integrations === nextProps.integrations;
});

AddContentButton.displayName = 'AddContentButton';

export default AddContentButton;
