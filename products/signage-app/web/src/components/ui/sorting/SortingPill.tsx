import React from "react";
import { ChevronDown, ChevronUp } from "lucide-react";

export type SortDirection = "asc" | "desc" | null;

interface SortingPillProps {
  label: string;
  value: SortDirection;
  onChange: (direction: SortDirection) => void;
  className?: string;
}

export const SortingPill: React.FC<SortingPillProps> = ({
  label,
  value,
  onChange,
  className = ""
}) => {
  const handleClick = () => {
    if (value === null) {
      onChange("asc");
    } else if (value === "asc") {
      onChange("desc");
    } else {
      onChange(null);
    }
  };

  return (
    <button
      onClick={handleClick}
      className={`
        inline-flex items-center gap-1 px-3 py-1.5
        text-sm sm:text-xs font-medium rounded-full
        transition-colors duration-150
        ${value ?
          'bg-black text-white hover:bg-gray-700' :
          'bg-gray-100 text-gray-700 hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-300 dark:hover:bg-gray-600'
        }
        ${className}
      `}
    >
      <span>{label}</span>
      {value && (
        value === "asc" ?
          <ChevronUp className="size-3" /> :
          <ChevronDown className="size-3" />
      )}
    </button>
  );
};
