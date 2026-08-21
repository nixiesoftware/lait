import React from "react";
import { Modal } from "@/components/ui/modal";
import Button from "@/components/ui/button/Button";

interface ConfirmationModalProps {
  isOpen: boolean;
  onClose: () => void;
  onConfirm: () => void;
  title: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  showCloseButton: boolean;
  variant?: "danger" | "warning" | "default";
}

export const ConfirmationModal: React.FC<ConfirmationModalProps> = ({
  isOpen,
  onClose,
  onConfirm,
  title,
  message,
  showCloseButton = false,
  confirmText = "Confirm",
  cancelText = "Cancel",
  variant = "default"
}) => {
  const handleConfirm = () => {
    onConfirm();
    onClose();
  };

  const getButtonVariant = () => {
    switch (variant) {
      case "danger":
        return "danger";
      case "warning":
        return "danger"; // Use danger for warning since warning variant doesn't exist
      default:
        return "primary";
    }
  };

  return (
    <Modal isOpen={isOpen} transformOrigin={"middle center"} onClose={onClose} showCloseButton={showCloseButton}
           className="z-50 max-w-sm !h-fit m-auto top-0 bottom-0 rounded-md
        border-1 border-gray-300 shadow-md">
      <div className="relative w-full flex flex-col justify-between items-start flex-wrap overflow-y-auto p-6 gap-4">
        <div className="flex flex-1 flex-col gap-2">
          <h4 className="text-lg font-semibold text-gray-800 dark:text-white">
            {title}
          </h4>
          <p className="text-sm text-gray-600 dark:text-gray-300">
            {message}
          </p>
        </div>
        <div className="flex flex-1 gap-2 self-end">
          <Button size="xs" variant="outline" onClick={onClose} className="rounded-sm">
            {cancelText}
          </Button>
          <Button size="xs" variant={getButtonVariant()} onClick={handleConfirm} className="rounded-sm">
            {confirmText}
          </Button>
        </div>
      </div>
    </Modal>
  );
};
