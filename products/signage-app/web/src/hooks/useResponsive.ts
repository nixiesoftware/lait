import { useEffect, useState } from "react";

export const TABLET_MIN_WIDTH = 640; // Tailwind md breakpoint by default

export function useIsTabletUp() {
  const [isTabletUp, setIsTabletUp] = useState<boolean>(false);
  const [width, setWidth] = useState<number>(
    typeof window !== "undefined" ? window.innerWidth : 0
  );

  useEffect(() => {
    const handleResize = () => {
      const w = window.innerWidth;
      setWidth(w);
      setIsTabletUp(w >= TABLET_MIN_WIDTH);
    };

    // Initialize on mount
    handleResize();
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  return { isTabletUp, width } as const;
}

