import React, { useState } from "react";
import { Modal } from "@/components/ui/modal";
import Button from "@/components/ui/button/Button";
import Label from "@/components/form/Label";
import Input from "@/components/form/input/InputField";

interface ScreenNameModalProps {
  isOpen: boolean;
  onClose: () => void;
  onConfirm: (name: string) => void;
  isCreating?: boolean;
}

export const ScreenNameModal: React.FC<ScreenNameModalProps> = ({
  isOpen,
  onClose,
  onConfirm,
  isCreating = false
}) => {
  const [screenName, setScreenName] = useState("");
  const [error, setError] = useState("");

  const handleConfirm = () => {
    // Validate the input
    if (!screenName.trim()) {
      setError("Screen name is required");
      return;
    }

    // Call the confirm callback with the screen name
    onConfirm(screenName.trim());

    // Reset the form
    setScreenName("");
    setError("");
  };

  const handleClose = () => {
    // Reset the form state when closing
    setScreenName("");
    setError("");
    onClose();
  };

  const handleKeyPress = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !isCreating) {
      handleConfirm();
    }
  };

  return (
    <Modal
      isOpen={isOpen}
      transformOrigin="middle center"
      onClose={handleClose}
      showCloseButton={false}
      className="max-w-sm !h-fit m-auto top-0 bottom-0 rounded-md border-1 border-gray-300 z-30 shadow-md"
    >
      <div className="relative w-full flex flex-col justify-between items-start overflow-y-auto p-6 gap-4">
        <div className="flex flex-1 flex-col gap-2 w-full">
          <h4 className="text-lg font-semibold text-gray-800 dark:text-white">
            Give your screen a name
          </h4>

          <div className="w-full">
            <Input
              type="text"
              placeholder="e.g., Lobby Display, Reception Screen"
              value={screenName}
              onChange={(e) => {
                setScreenName(e.target.value);
                setError(""); // Clear error on input change
              }}
              onKeyDown={handleKeyPress}
              disabled={isCreating}
              className={`!h-9  ${error ? "border-red-500" : ""}`}
              autoFocus
            />
            {error && (
              <p className="text-xs text-red-500 mt-1">{error}</p>
            )}
          </div>
        </div>

        <div className="flex flex-1 gap-2 self-end">
          <Button
            size="xs"
            variant="outline"
            onClick={handleClose}
            className="rounded-sm"
            disabled={isCreating}
          >
            Cancel
          </Button>
          <Button
            size="xs"
            variant="primary"
            onClick={handleConfirm}
            className="rounded-sm"
            disabled={isCreating}
          >
            {isCreating ? "Creating..." : "Add Screen"}
          </Button>
        </div>
      </div>
    </Modal>
  );
};
