import React, { useState, useRef, useEffect } from "react";
import { ChevronDown, X } from "lucide-react";

interface FilterPillProps {
  label: string;
  options: { value: string; label: string }[];
  value: string | null;
  onChange: (value: string | null) => void;
  className?: string;
}

export const FilterPill: React.FC<FilterPillProps> = ({
  label,
  options,
  value,
  onChange,
  className = ""
}) => {
  const [isOpen, setIsOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  const selectedOption = options.find(opt => opt.value === value);

  return (
    <div className="relative" ref={dropdownRef}>
      <button
        onClick={() => setIsOpen(!isOpen)}
        className={`
          inline-flex items-center gap-1 px-3 py-2
          text-sm sm:text-[10px] font-medium rounded-full
          transition-colors duration-150
          ${value ?
            'bg-brand-500 text-white hover:bg-brand-600' :
            'bg-gray-100 text-gray-700 hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-300 dark:hover:bg-gray-600'
          }
          ${className}
        `}
      >
        <span>{selectedOption ? selectedOption.label : label}</span>
        {value ? (
          <X
            className="w-3 h-3"
            onClick={(e) => {
              e.stopPropagation();
              onChange(null);
              setIsOpen(false);
            }}
          />
        ) : (
          <ChevronDown className="w-3 h-3" />
        )}
      </button>

      {isOpen && (
        <div className="absolute top-full mt-1 z-10 bg-white dark:bg-gray-800 rounded-sm shadow-lg border border-gray-200 dark:border-gray-700 min-w-[150px]">
          {options.map((option) => (
            <button
              key={option.value}
              onClick={() => {
                onChange(option.value);
                setIsOpen(false);
              }}
              className={`
                block w-full text-left px-4 py-2 text-sm
                hover:bg-gray-100 dark:hover:bg-gray-700
                ${option.value === value ? 'bg-gray-50 dark:bg-gray-700/50 font-medium' : ''}
                ${option === options[0] ? 'rounded-t-sm' : ''}
                ${option === options[options.length - 1] ? 'rounded-b-sm' : ''}
              `}
            >
              {option.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
};
