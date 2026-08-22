export const formatMMSS = (seconds: number): string => {
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
};

/**
 * Parse a string in M:SS or SS format to total seconds.
 * Returns null if invalid.
 */
export const parseMMSS = (value: string): number | null => {
  const parts = value.split(":").map(p => parseInt(p, 10));
  if (parts.length === 2 && parts.every(n => !isNaN(n))) {
    return parts[0] * 60 + parts[1];
  }
  if (parts.length === 1 && !isNaN(parts[0])) {
    return parts[0];
  }
  return null;
};


