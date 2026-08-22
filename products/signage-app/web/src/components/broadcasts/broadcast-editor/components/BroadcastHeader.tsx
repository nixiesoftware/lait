import React, { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import {ArrowLeft, MonitorUpIcon} from "lucide-react";
import {InlineEditableText} from "@/components/ui/InlineEditableText";
import {CircleCheck, SaveIcon} from "../../../../../public/images/icons/theme-icons";
import Button from "@/components/ui/button/Button";
import { goBack } from "@/utils/navigation/goBack";

interface BroadcastHeaderProps {
  broadcastName: string;
  isDirty: boolean;
  isSaving: boolean;
  onSave: () => void;
  onNameChange?: (name: string) => void;
  onShowContentLibrary?: () => void;
  onExitClick?: () => void;
  onDiscard?: () => void;
}

export default function BroadcastHeader({
  broadcastName,
  isDirty,
  isSaving,
  onSave,
  onNameChange,
  onExitClick,
  onDiscard,
}: BroadcastHeaderProps) {
  const navigate = useNavigate();
  const [editedName] = useState(broadcastName);

  const handleSaveClick = () => {
    if (!isDirty || isSaving) return;
    onSave();
  };

  const handleBackClick = (e: React.MouseEvent) => {
    e.preventDefault();
    if (isDirty && onExitClick) {
      onExitClick();
    } else {
      // Use safe goBack with fallback to broadcasts index
      goBack(navigate, "/broadcast-list");
    }
  };

  const getSaveButtonContent = () => {
    if (isSaving) {
      return (
        <>
          <div className="animate-spin rounded-full h-4 w-4 border-2 border-white border-t-transparent" />
          <span className="hidden sm:inline">Saving...</span>
        </>
      );
    }

    if (!isDirty) {
      return (
        <>
          <span className="sm:inline">Saved</span>
        </>
      );
    }

    return (
      <>
        <span className="sm:inline">Save</span>
      </>
    );
  };

  return (
    <header className="h-14 py-1 flex items-center justify-between">
        <div className="flex items-center gap-4">
          {/* Back Button */}
          <button
            onClick={handleBackClick}
            className="p-2 hover:bg-gray-50/20 dark:hover:bg-gray-800/20 rounded-md transition-colors"
          >
            <ArrowLeft className="w-5 h-5 text-white dark:text-gray-200" />
          </button>
        </div>

      <div className="flex items-center gap-2">
        {/* Assign to Screen Button - Hidden on mobile*/}
        <button
          className="hidden items-center gap-2 px-4 py-2 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-800 rounded-sm transition-colors"
        >
          <MonitorUpIcon className="w-4 h-4" />
          <span className="hidden md:inline">Send to Screen</span>
        </button>

        {/* Broadcast Name */}
        <div className="hidden sm:flex flex-col items-center gap-2 flex-1 sm:flex-none relative">
          <InlineEditableText
            value={editedName}
            onSave={(newName) => onNameChange ? onNameChange(newName) : null}
            className="min-md:text-md text-lg text-white"
          />
        </div>

        {/* Discard Button */}
        <Button
          onClick={onDiscard}
          disabled={!isDirty || isSaving}
          className={`
            ${!isDirty
            ? '!hidden'
            : 'bg-purple-50/20 hover:bg-purple-50/40'
          }
          `}
        >
          Discard
        </Button>

        <Button
          onClick={handleSaveClick}
          disabled={!isDirty || isSaving}
          className={`
            ${isDirty
              ? '!bg-purple-50/90 hover:!bg-purple-100/90 !text-gray-900'
              : '!bg-gray-100/30 dark:!bg-gray-800/30 !text-gray-200 dark:!text-gray-300 cursor-not-allowed'
            }
            ${isSaving ? 'opacity-75' : ''}
          `}
        >
          {getSaveButtonContent()}
        </Button>
      </div>
    </header>
  );
}
