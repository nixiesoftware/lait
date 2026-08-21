import React, { createContext, useContext, useState, useCallback } from 'react';

interface UploadingItem {
  tempId: string;
  fileName: string;
  fileType: string;
  progress?: number;
}

interface UploadContextType {
  uploadingItems: Map<string, UploadingItem>;
  addUploadingItem: (tempId: string, fileName: string, fileType: string) => void;
  updateUploadProgress: (tempId: string, progress: number) => void;
  removeUploadingItem: (tempId: string) => void;
  isUploading: (tempId: string) => boolean;
  clearAllUploads: () => void;
}

const UploadContext = createContext<UploadContextType | undefined>(undefined);

export const UploadProvider: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [uploadingItems, setUploadingItems] = useState<Map<string, UploadingItem>>(new Map());

  const addUploadingItem = useCallback((tempId: string, fileName: string, fileType: string) => {
    setUploadingItems(prev => {
      const next = new Map(prev);
      next.set(tempId, { tempId, fileName, fileType, progress: 0 });
      return next;
    });
  }, []);

  const updateUploadProgress = useCallback((tempId: string, progress: number) => {
    setUploadingItems(prev => {
      const next = new Map(prev);
      const item = next.get(tempId);
      if (item) {
        next.set(tempId, { ...item, progress });
      }
      return next;
    });
  }, []);

  const removeUploadingItem = useCallback((tempId: string) => {
    setUploadingItems(prev => {
      const next = new Map(prev);
      next.delete(tempId);
      return next;
    });
  }, []);

  const isUploading = useCallback((tempId: string) => {
    return uploadingItems.has(tempId);
  }, [uploadingItems]);

  const clearAllUploads = useCallback(() => {
    setUploadingItems(new Map());
  }, []);

  return (
    <UploadContext.Provider value={{
      uploadingItems,
      addUploadingItem,
      updateUploadProgress,
      removeUploadingItem,
      isUploading,
      clearAllUploads
    }}>
      {children}
    </UploadContext.Provider>
  );
};

export const useUploadContext = () => {
  const context = useContext(UploadContext);
  if (!context) {
    throw new Error('useUploadContext must be used within an UploadProvider');
  }
  return context;
};