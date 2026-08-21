import React, { useState, useEffect } from "react";
import { formatMMSS, parseMMSS } from "@/utils/time/timeFormat"

interface DurationInputProps {
  /** Current duration in seconds */
  value: number;
  /** Called with new duration (in seconds) on blur or Enter */
  onChange: (seconds: number) => void;
  /** Optional styling */
  className?: string;
}

/**
 * Format seconds as M:SS (e.g. 90 → "1:30").
 */
export default function DurationInput({ value, onChange, className }: DurationInputProps) {
  const [inputValue, setInputValue] = useState(formatMMSS(value));

  // Keep displayed string in sync when `value` changes externally
  useEffect(() => {
    setInputValue(formatMMSS(value));
  }, [value]);

  // Commit changes: parse and call onChange, then format display
  const commit = () => {
    const seconds = parseMMSS(inputValue);
    if (seconds !== null) {
      if (seconds !== value) onChange(seconds);
      setInputValue(formatMMSS(seconds));
    } else {
      // reset to previous valid value
      setInputValue(formatMMSS(value));
    }
  };

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    // allow only digits and colon, max two parts of up to 2 digits
    const raw = e.target.value;
    const filtered = raw.replace(/[^\d:]/g, "");
    const parts = filtered.split(":");
    if (parts.length <= 2 && parts.every(part => /^\d{0,2}$/.test(part))) {
      setInputValue(filtered);
    }
  };

  const handleBlur = () => commit();
  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      commit();
      (e.target as HTMLInputElement).blur();
    }
  };

  return (
    <input
      type="text"
      className={className}
      value={inputValue}
      placeholder="0:00"
      onChange={handleChange}
      onBlur={handleBlur}
      onKeyDown={handleKeyDown}
    />
  );
}

