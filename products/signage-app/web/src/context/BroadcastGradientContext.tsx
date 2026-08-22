import React, { createContext, useContext, useMemo, useRef, useState, ReactNode } from "react";
import { ColorInfo } from "@/utils/colorExtractor";

type SetColors = (next: ColorInfo | null | ((prev: ColorInfo | null) => ColorInfo | null)) => void;

type ContentId = number;

interface BroadcastGradientContextType {
  colors: ColorInfo | null;
  setColors: SetColors;
  getCachedColors: (id: ContentId) => ColorInfo | undefined;
  setColorsForContent: (id: ContentId, colors: ColorInfo) => void;
  clearCacheForContent: (id: ContentId) => void;
}

const BroadcastGradientContext = createContext<BroadcastGradientContextType | null>(null);

export const useBroadcastGradient = () => {
  const context = useContext(BroadcastGradientContext);
  if (!context) {
    throw new Error("useBroadcastGradient must be used within a BroadcastGradientProvider");
  }
  return context;
};

interface BroadcastGradientProviderProps {
  children: ReactNode;
}

export const BroadcastGradientProvider: React.FC<BroadcastGradientProviderProps> = ({ children }) => {
  const [colors, _setColors] = useState<ColorInfo | null>(null);

  // Cache map held in a ref to avoid re-rendering the tree when cache updates
  const colorsCacheRef = useRef<Map<ContentId, ColorInfo>>(new Map());

  // Debounce rapid updates slightly to avoid jank when thumbnails/palettes resolve in bursts
  const debounceTimer = useRef<number | null>(null);
  const setColors: SetColors = useMemo(() => (next) => {
    if (debounceTimer.current) {
      window.clearTimeout(debounceTimer.current);
      debounceTimer.current = null;
    }
    debounceTimer.current = window.setTimeout(() => {
      if (typeof next === "function") {
        _setColors((prev) => (next as (p: ColorInfo | null) => ColorInfo | null)(prev));
      } else {
        _setColors(next);
      }
      debounceTimer.current = null;
    }, 16); // ~ one frame at 60fps
  }, []);

  const getCachedColors = (id: ContentId) => colorsCacheRef.current.get(id);
  const setColorsForContent = (id: ContentId, value: ColorInfo) => {
    colorsCacheRef.current.set(id, value);
    setColors(value);
  };
  const clearCacheForContent = (id: ContentId) => {
    colorsCacheRef.current.delete(id);
  };

  return (
    <BroadcastGradientContext.Provider value={{ colors, setColors, getCachedColors, setColorsForContent, clearCacheForContent }}>
      {children}
    </BroadcastGradientContext.Provider>
  );
};
