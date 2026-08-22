import React, { createContext, useContext, useState, ReactNode } from "react";

type SidebarView = 'files' | 'apps' | 'screens' | 'broadcasts' | null;

interface BroadcastEditorSidebarContextType {
  activeView: SidebarView;
  setActiveView: (view: SidebarView) => void;
  isExpanded: boolean;
  setIsExpanded: (expanded: boolean) => void;
}

const BroadcastEditorSidebarContext = createContext<BroadcastEditorSidebarContextType | undefined>(undefined);

export const useBroadcastEditorSidebar = () => {
  const context = useContext(BroadcastEditorSidebarContext);
  if (!context) {
    throw new Error("useBroadcastEditorSidebar must be used within BroadcastEditorSidebarProvider");
  }
  return context;
};

interface BroadcastEditorSidebarProviderProps {
  children: ReactNode;
}

export const BroadcastEditorSidebarProvider: React.FC<BroadcastEditorSidebarProviderProps> = ({ children }) => {
  const [activeView, setActiveView] = useState<SidebarView>(null);
  const [isExpanded, setIsExpanded] = useState(false);

  return (
    <BroadcastEditorSidebarContext.Provider
      value={{
        activeView,
        setActiveView,
        isExpanded,
        setIsExpanded
      }}
    >
      {children}
    </BroadcastEditorSidebarContext.Provider>
  );
};
