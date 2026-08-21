import React from "react";
import { Trash2, FileStack } from "lucide-react";
import Button from "@/components/ui/button/Button";
import { BaseDetailsModal } from "@/components/ui/BaseDetailsModal";
import { BroadcastSummary } from "@/app/(admin)/(pages)/broadcast-list/BroadcastList";

interface BroadcastDetailsModalProps {
  isOpen: boolean;
  broadcast: BroadcastSummary | null;
  onClose: () => void;
  onDelete?: (id: string) => void;
  onUpdate?: (id: string, name: string) => void;
  onViewItems?: (id: string) => void;
}

export const BroadcastDetailsModal: React.FC<BroadcastDetailsModalProps> = ({
  isOpen,
  broadcast,
  onClose,
  onDelete,
  onUpdate,
  onViewItems
}) => {
  if (!broadcast) return null;

  const handleDelete = () => {
    if (onDelete && confirm(`Are you sure you want to delete "${broadcast.name}"?`)) {
      onDelete(broadcast.id);
    }
  };

  const handleUpdateName = (newName: string) => {
    if (onUpdate) {
      onUpdate(broadcast.id, newName);
    }
  };

  const handleViewItems = () => {
    if (onViewItems) {
      onViewItems(broadcast.id);
      onClose();
    }
  };

  const mediaPreview = (
    <div className="flex flex-col items-center justify-center p-8">
      <FileStack className="w-24 h-24 text-gray-400 dark:text-gray-600" />
      <p className="mt-4 text-2xl font-semibold text-gray-700 dark:text-gray-300">
        {broadcast.contentCount} {broadcast.contentCount === 1 ? 'item' : 'items'}
      </p>
    </div>
  );

  const detailsSections = (
    <>
      <div className="flex justify-between items-center py-1">
        <span className="text-sm lg:text-sm font-medium text-gray-700 dark:text-gray-300">Items</span>
        <span className="text-sm lg:text-sm text-gray-900 dark:text-white">
          {broadcast.contentCount} {broadcast.contentCount === 1 ? 'item' : 'items'}
        </span>
      </div>

      <div className="flex justify-between items-center py-1">
        <span className="text-sm lg:text-sm font-medium text-gray-700 dark:text-gray-300">Playing on</span>
        <span className="text-sm lg:text-sm text-gray-900 dark:text-white">
          {broadcast.assignedScreenCount} {broadcast.assignedScreenCount === 1 ? 'screen' : 'screens'}
        </span>
      </div>
    </>
  );

  const actionButtons = (
    <div className="flex justify-between items-center">
      {onDelete && (
        <Button
          variant="outline"
          onClick={handleDelete}
          className="px-4 p-2 rounded-sm"
        >
          <Trash2 className="w-4 h-4"/>
        </Button>
      )}
      <div className="flex gap-2">
        {onViewItems && (
          <Button
            variant="primary"
            onClick={handleViewItems}
            className="px-4 py-2 rounded-sm"
          >
            Open Broadcast
          </Button>
        )}
      </div>
    </div>
  );

  return (
    <BaseDetailsModal
      isOpen={isOpen}
      onClose={onClose}
      title={broadcast.name}
      onUpdateTitle={onUpdate ? handleUpdateName : undefined}
      mediaPreview={mediaPreview}
      detailsSections={detailsSections}
      actionButtons={actionButtons}
    />
  );
};
