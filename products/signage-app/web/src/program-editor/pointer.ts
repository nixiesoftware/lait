import { useEffect, useState } from "react";

/** Primary pointer is a finger. Chrome's device toolbar reports this too. */
export function useCoarsePointer(): boolean {
  const [coarse, setCoarse] = useState(
    () =>
      typeof window !== "undefined" &&
      window.matchMedia("(pointer: coarse)").matches,
  );
  useEffect(() => {
    const media = window.matchMedia("(pointer: coarse)");
    const onChange = () => setCoarse(media.matches);
    onChange();
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);
  return coarse;
}

/** Long-press is hold-to-lift. Do not let the browser menu steal it. */
export function suppressCoarseContextMenu(event: Event): void {
  if (!window.matchMedia("(pointer: coarse)").matches) return;
  event.preventDefault();
  event.stopPropagation();
}
