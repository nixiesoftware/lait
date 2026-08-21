import React from "react";
import { SearchIcon, X } from "lucide-react";

interface PageSearchBarProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
}

export const PageSearchBar: React.FC<PageSearchBarProps> = ({
  value,
  onChange,
  placeholder = "Filter...",
}) => {
  const updateQuery = (newValue: string) => {
    onChange(newValue);
    // Update URL search param without full navigation
    const url = new URL(window.location.href);
    if (newValue) {
      url.searchParams.set("q", newValue);
    } else {
      url.searchParams.delete("q");
    }
    window.history.replaceState({}, "", url.toString());
  };

  return (
    <div className="relative">
      <SearchIcon className="absolute left-2.5 top-1/2 -translate-y-1/2 size-3.5 text-gray-400 pointer-events-none" />
      <input
        type="text"
        value={value}
        onChange={(e) => updateQuery(e.target.value)}
        placeholder={placeholder}
        className="w-full sm:w-44 rounded-md border border-gray-200 bg-white py-1.5 pl-8 pr-7 text-xs text-gray-700 placeholder:text-gray-400 focus:border-brand-300 focus:outline-none focus:ring-1 focus:ring-brand-300 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-300 dark:placeholder:text-gray-500"
      />
      {value && (
        <button
          onClick={() => updateQuery("")}
          className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
        >
          <X className="size-3" />
        </button>
      )}
    </div>
  );
};
