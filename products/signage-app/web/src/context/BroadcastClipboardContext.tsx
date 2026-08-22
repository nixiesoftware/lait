import React, { createContext, useContext, useState, ReactNode } from 'react';
import { BroadcastRow } from '@/components/broadcasts/types';

interface BroadcastClipboardContextType {
  clipboardItem: BroadcastRow | null;
  copyItem: (item: BroadcastRow) => void;
  hasClipboardItem: boolean;
  clearClipboard: () => void;
}

const BroadcastClipboardContext = createContext<BroadcastClipboardContextType | undefined>(undefined);

export function BroadcastClipboardProvider({ children }: { children: ReactNode }) {
  const [clipboardItem, setClipboardItem] = useState<BroadcastRow | null>(null);

  const copyItem = (item: BroadcastRow) => {
    setClipboardItem(item);
  };

  const clearClipboard = () => {
    setClipboardItem(null);
  };

  return (
    <BroadcastClipboardContext.Provider
      value={{
        clipboardItem,
        copyItem,
        hasClipboardItem: clipboardItem !== null,
        clearClipboard
      }}
    >
      {children}
    </BroadcastClipboardContext.Provider>
  );
}

export function useBroadcastClipboard() {
  const context = useContext(BroadcastClipboardContext);
  if (context === undefined) {
    throw new Error('useBroadcastClipboard must be used within a BroadcastClipboardProvider');
  }
  return context;
}
