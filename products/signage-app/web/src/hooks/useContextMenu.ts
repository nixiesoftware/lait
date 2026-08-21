import { useState, useCallback, useEffect, useRef } from 'react';

interface ContextMenuState<T> {
  isOpen: boolean;
  position: { x: number; y: number };
  data: T | null;
}

// Global flag to track if ANY context menu was just closed
let globalClickBlockerTimeout: NodeJS.Timeout | null = null;
let globalClickBlockerActive = false;

export function useContextMenu<T = unknown>() {
  const [state, setState] = useState<ContextMenuState<T>>({
    isOpen: false,
    position: { x: 0, y: 0 },
    data: null
  });
  
  const clickBlockerRef = useRef<((e: MouseEvent) => void) | null>(null);

  const openContextMenu = useCallback((event: React.MouseEvent, data?: T) => {
    event.preventDefault();
    event.stopPropagation();
    
    setState({
      isOpen: true,
      position: { x: event.clientX, y: event.clientY },
      data: data || null
    });
  }, []);

  const closeContextMenu = useCallback(() => {
    setState(prev => ({ ...prev, isOpen: false }));
    
    // Install a global click blocker immediately
    if (!globalClickBlockerActive) {
      globalClickBlockerActive = true;
      
      // Create the click blocker function
      const blockNextClick = (e: MouseEvent) => {
        // Only block left clicks (button === 0)
        if (e.button === 0) {
          e.preventDefault();
          e.stopPropagation();
          if (e.stopImmediatePropagation) {
            e.stopImmediatePropagation();
          }
        }
        
        // Remove the blocker after blocking one click
        document.removeEventListener('click', blockNextClick, true);
        globalClickBlockerActive = false;
        
        // Clear the timeout since we've already blocked a click
        if (globalClickBlockerTimeout) {
          clearTimeout(globalClickBlockerTimeout);
          globalClickBlockerTimeout = null;
        }
      };
      
      // Add the blocker in capture phase to intercept before any React handlers
      document.addEventListener('click', blockNextClick, true);
      
      // Also remove the blocker after a timeout in case no click happens
      if (globalClickBlockerTimeout) {
        clearTimeout(globalClickBlockerTimeout);
      }
      globalClickBlockerTimeout = setTimeout(() => {
        document.removeEventListener('click', blockNextClick, true);
        globalClickBlockerActive = false;
        globalClickBlockerTimeout = null;
      }, 100);
      
      // Store reference for cleanup
      clickBlockerRef.current = blockNextClick;
    }
  }, []);
  
  // Cleanup on unmount
  useEffect(() => {
    return () => {
      // Clean up any active click blocker if this component unmounts
      if (clickBlockerRef.current) {
        document.removeEventListener('click', clickBlockerRef.current, true);
      }
      if (globalClickBlockerTimeout) {
        clearTimeout(globalClickBlockerTimeout);
        globalClickBlockerTimeout = null;
      }
    };
  }, []);

  return {
    isOpen: state.isOpen,
    position: state.position,
    data: state.data,
    openContextMenu,
    closeContextMenu
  };
}
