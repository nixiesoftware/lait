import React, { useState, useRef, useEffect } from "react";
import { PencilIcon } from "../../../public/images/icons/theme-icons";
import { ScrollingText } from "./ScrollingText";

interface InlineEditableTextProps {
  value: string;
  onSave: (newValue: string) => void;
  className?: string;
  editClassName?: string;
  displayClassName?: string;
  showIcon?: boolean;
  iconHover?: boolean;
  iconSize?: { width: string; height: string; viewBox: string };
  onEditStart?: () => void;
  onEditEnd?: () => void;
  placeholder?: string;
  isParentHovered?: boolean;
}

export const InlineEditableText: React.FC<InlineEditableTextProps> = ({
  value,
  onSave,
  className = "",
  editClassName = "",
  displayClassName = "",
  showIcon = true,
  iconHover = true,
  iconSize = { width: "16px", height: "16px", viewBox: "0 0 22 22" },
  onEditStart,
  onEditEnd,
  placeholder = "Enter text",
  isParentHovered = false
}) => {
  const [isEditing, setIsEditing] = useState(false);
  const [editedValue, setEditedValue] = useState(value);
  const [originalValue, setOriginalValue] = useState(value);
  const inputRef = useRef<HTMLInputElement>(null);

  // Update values when prop changes
  useEffect(() => {
    setEditedValue(value);
    setOriginalValue(value);
  }, [value]);

  useEffect(() => {
    if (isEditing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [isEditing]);

  const handleClick = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsEditing(true);
    onEditStart?.();
  };

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setEditedValue(e.target.value);
  };

  const handleBlur = () => {
    setIsEditing(false);
    // Only save if the value has changed
    if (editedValue !== originalValue && editedValue.trim() !== "") {
      setOriginalValue(editedValue);
      onSave(editedValue.trim());
    } else {
      // Reset to original if empty or unchanged
      setEditedValue(originalValue);
    }
    onEditEnd?.();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.currentTarget.blur();
    } else if (e.key === "Escape") {
      setEditedValue(originalValue);
      setIsEditing(false);
      onEditEnd?.();
    }
  };

  const handleInputClick = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
  };

  if (isEditing) {
    return (
      <input
        ref={inputRef}
        type="text"
        value={editedValue}
        onChange={handleChange}
        onBlur={handleBlur}
        onKeyDown={handleKeyDown}
        onClick={handleInputClick}
        placeholder={placeholder}
        className={`bg-transparent outline-none border-b-2 border-dotted border-gray-300 dark:border-gray-600 focus:border-gray-300 dark:focus:border-gray-600 transition-colors ${editClassName} ${className}`}
      />
    );
  }

  return (
    <div
      onClick={handleClick}
      className={`group flex items-center gap-x-1 cursor-text w-full ${className}`}
    >
      <div className="min-w-0 flex-1 relative">
        <ScrollingText
          text={editedValue}
          className={`border-b-2 border-dotted border-transparent group-hover:border-gray-300 dark:group-hover:border-gray-600 transition-all ${displayClassName}`}
          speed={30}
          delay={500}
          isParentHovered={isParentHovered}
        />
      </div>
      {showIcon && (
        <PencilIcon
          className={`${iconHover ? 'opacity-0 group-hover:opacity-100' : 'opacity-100'} text-gray-400 group-hover:text-gray-600 dark:group-hover:text-gray-400 transition-all cursor-pointer flex-shrink-0`}
          width={iconSize.width}
          height={iconSize.height}
          viewBox={iconSize.viewBox}
        />
      )}
    </div>
  );
};
