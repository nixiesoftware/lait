import React from "react";

interface CheckboxProps {
  checked?: boolean;
  onChange?: (checked: boolean) => void;
  disabled?: boolean;
  indeterminate?: boolean;
  size?: "sm" | "md" | "lg";
  variant?: "default" | "primary" | "success" | "warning" | "danger";
  showOnGroupHover?: boolean;
  label?: string;
  className?: string;
  id?: string;
  name?: string;
}

export const Checkbox: React.FC<CheckboxProps> = ({
  checked = false,
  onChange,
  disabled = false,
  indeterminate = false,
  size = "md",
  variant = "primary",
  showOnGroupHover = false,
  label,
  className = "",
  id
}) => {
  const handleClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!disabled && onChange) {
      onChange(!checked);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === ' ' || e.key === 'Enter') {
      e.preventDefault();
      if (!disabled && onChange) {
        onChange(!checked);
      }
    }
  };

  const sizeClasses = {
    sm: "w-4 h-4",
    md: "w-5 h-5",
    lg: "w-6 h-6"
  };

  const checkmarkSizes = {
    sm: "w-2.5 h-2.5",
    md: "w-3 h-3",
    lg: "w-5 h-5"
  };

  const variantClasses = {
    default: {
      checked: "bg-gray-600 border-gray-600 hover:border-gray-700",
      unchecked: "bg-white border-gray-300 hover:border-gray-400",
    },
    primary: {
      checked: "bg-brand-500 border-brand-500 hover:border-brand-600",
      unchecked: "bg-white border-gray-300 hover:border-brand-400"
    },
    success: {
      checked: "bg-green-500 border-green-500 hover:border-green-600",
      unchecked: "bg-white border-gray-300 hover:border-green-400"
    },
    warning: {
      checked: "bg-yellow-500 border-yellow-500 hover:border-yellow-600",
      unchecked: "bg-white border-gray-300 hover:border-yellow-400"
    },
    danger: {
      checked: "bg-red-500 border-red-500 hover:border-red-600",
      unchecked: "bg-white border-gray-300 hover:border-red-400"
    }
  };

  const currentVariant = variantClasses[variant];
  const isChecked = checked || indeterminate;

  const checkboxElement = (
    <div
      role="checkbox"
      aria-checked={indeterminate ? "mixed" : checked}
      aria-disabled={disabled}
      tabIndex={disabled ? -1 : 0}
      onClick={handleClick}
      onKeyDown={handleKeyDown}
      className={`
        ${sizeClasses[size]}
        rounded-sm
        border-1
        ${isChecked ? currentVariant.checked : currentVariant.unchecked}
        ${disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}
        flex
        items-center
        justify-center
        transition-all
        duration-150
        ${showOnGroupHover && !isChecked ? 'opacity-0 group-hover:opacity-100' : 'opacity-100'}
        ${className}
      `}
    >
      {indeterminate ? (
        <div className={`${checkmarkSizes[size]} bg-white rounded-sm`} style={{ height: '2px' }} />
      ) : checked ? (
        <svg
          className={`${checkmarkSizes[size]} text-white`}
          fill="currentColor"
          viewBox="0 0 20 20"
        >
          <path
            fillRule="evenodd"
            d="M16.707 5.293a1 1 0 010 1.414l-8 8a1 1 0 01-1.414 0l-4-4a1 1 0 011.414-1.414L8 12.586l7.293-7.293a1 1 0 011.414 0z"
            clipRule="evenodd"
          />
        </svg>
      ) : null}
    </div>
  );

  if (label) {
    return (
      <label
        htmlFor={id}
        className={`flex items-center gap-2 ${disabled ? 'cursor-not-allowed' : 'cursor-pointer'}`}
      >
        {checkboxElement}
        <span className={`select-none ${disabled ? 'opacity-50' : ''}`}>
          {label}
        </span>
      </label>
    );
  }

  return checkboxElement;
};
