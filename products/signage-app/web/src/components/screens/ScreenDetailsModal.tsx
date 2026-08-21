import React from "react";
import { BaseDetailsModal } from "@/components/ui/BaseDetailsModal";
import { Hash, Layers, Network, Clock, Eye, Trash2 } from "lucide-react";
import Button from "@/components/ui/button/Button";
import type { SignageScreen } from "@/utils/lait/types";

interface ScreenDetailsModalProps {
  isOpen: boolean;
  screen: SignageScreen | null;
  groupName: string | null;
  programName: string | null;
  onClose: () => void;
  onDelete?: (id: string) => void;
  onUpdate: (id: string, name: string) => void;
  onViewScreen: (id: string) => void;
  onAssignBroadcast?: (screenId: string) => void;
}

export const ScreenDetailsModal: React.FC<ScreenDetailsModalProps> = ({
  isOpen,
  screen,
  groupName,
  programName,
  onClose,
  onDelete,
  onUpdate,
  onViewScreen,
  onAssignBroadcast
}) => {
  if (!screen) return null;

  const override =
    screen.intent.over && screen.intent.over.until_unix_ms > Date.now()
      ? screen.intent.over
      : null;

  const detailItems: {
    icon: React.ReactNode;
    label: string;
    value: string;
    valueClassName?: string;
    action?: () => void;
    actionLabel?: string;
  }[] = [
    {
      icon: <Hash className="w-4 h-4" />,
      label: "Screen ID",
      value: screen.id
    },
    {
      icon: <Network className="w-4 h-4" />,
      label: "Network",
      value: groupName || "None"
    },
    {
      icon: <Layers className="w-4 h-4" />,
      label: "Assigned Broadcast",
      value: programName || "None",
      action: onAssignBroadcast ? () => onAssignBroadcast(screen.id) : undefined,
      actionLabel: programName ? "Change" : "Assign"
    },
    ...(override
      ? [{
          icon: <Clock className="w-4 h-4" />,
          label: "Override",
          value: `Until ${new Date(override.until_unix_ms).toLocaleString()}`,
          valueClassName: "text-amber-600 dark:text-amber-400"
        }]
      : []),
    {
      icon: <Layers className="w-4 h-4" />,
      label: "Scheduled Windows",
      value: String(screen.schedule.length)
    }
  ];

  return (
    <BaseDetailsModal
      isOpen={isOpen}
      onClose={onClose}
      title={screen.name}
      onUpdateTitle={(newName) => onUpdate(screen.id, newName)}
      detailsSections={
        <div className="space-y-4">
          {detailItems.map((item, index) => (
            <div key={index} className="flex items-start justify-between">
              <div className="flex items-center gap-3">
                <div className="text-gray-400">{item.icon}</div>
                <div>
                  <div className="text-sm text-gray-500 dark:text-gray-400">{item.label}</div>
                  <div className={`text-sm font-medium dark:text-white ${item.valueClassName || ''}`}>
                    {item.value}
                  </div>
                </div>
              </div>
              {item.action && (
                <Button
                  size="sm"
                  variant="outline"
                  onClick={item.action}
                >
                  {item.actionLabel}
                </Button>
              )}
            </div>
          ))}
        </div>
      }
      actionButtons={
        <div className="flex gap-2">
          <Button
            size="sm"
            onClick={() => onViewScreen(screen.id)}
          >
            <Eye className="w-4 h-4 mr-2" />
            View Screen
          </Button>
          {onDelete && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => onDelete(screen.id)}
            >
              <Trash2 className="w-4 h-4 mr-2" />
              Delete
            </Button>
          )}
        </div>
      }
    />
  );
};
