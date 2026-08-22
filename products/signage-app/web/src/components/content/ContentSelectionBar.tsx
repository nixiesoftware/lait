import React from "react";
import { Trash2 } from "lucide-react";
import Button from "@/components/ui/button/Button";

interface ContentSelectionBarProps {
  selectedCount: number;
  onDelete: () => void;
  onClearSelection: () => void;
}

export const ContentSelectionBar: React.FC<ContentSelectionBarProps> = ({
  selectedCount,
  onDelete,
  onClearSelection
}) => {
  if (selectedCount === 0) return null;

  return (
    <div className="flex items-center gap-2">
      <Button
        onClick={onDelete}
        size={"xs"}
        startIcon={<Trash2 className={"w-4 h-4"}/>}
        className="!bg-black !text-white"
      >
        Delete
      </Button>
      <Button
        onClick={onClearSelection}
        size={"xs"}
        className="!bg-white !border-1 !border-gray-600 !text-gray-700"
      >
        Clear
      </Button>
      <span className="text-gray-500 dark:text-gray-300 font-light">
        {selectedCount} item{selectedCount > 1 ? 's' : ''} selected
      </span>
    </div>
  );
};
