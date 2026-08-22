import React, { useState } from "react";
import { Modal } from "@/components/ui/modal";
import Button from "@/components/ui/button/Button";
import Label from "@/components/form/Label";
import Input from "@/components/form/input/InputField";

interface NetworkNameModalProps {
  isOpen: boolean;
  onClose: () => void;
  onConfirm: (name: string) => void;
  isCreating?: boolean;
}

export const NetworkNameModal: React.FC<NetworkNameModalProps> = ({
  isOpen,
  onClose,
  onConfirm,
  isCreating = false
}) => {
  const [networkName, setNetworkName] = useState("");
  const [error, setError] = useState("");

  const handleConfirm = () => {
    // Validate the input
    if (!networkName.trim()) {
      setError("Network name is required");
      return;
    }

    // Call the confirm callback with the network name
    onConfirm(networkName.trim());

    // Reset the form
    setNetworkName("");
    setError("");
  };

  const handleClose = () => {
    // Reset the form state when closing
    setNetworkName("");
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
      showCloseButton={!isCreating}
      className="max-w-sm !h-fit m-auto top-0 bottom-0 rounded-md
        border-1 border-gray-300 shadow-md z-30 bg-white"
    >
      <div className="relative w-full flex flex-col justify-between items-start overflow-y-auto p-6 gap-4">
        <div className="flex flex-1 flex-col gap-2 w-full">
          <h4 className="text-lg font-semibold text-gray-800 dark:text-white">
            Give your network a name
          </h4>

          <div className="w-full">
            <Input
              type="text"
              placeholder="e.g., Office Network, Store Displays"
              value={networkName}
              onChange={(e) => {
                setNetworkName(e.target.value);
                setError(""); // Clear error on input change
              }}
              disabled={isCreating}
              className={`!h-9  ${error ? "border-red-500" : ""}`}
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
            {isCreating ? "Creating..." : "Create Network"}
          </Button>
        </div>
      </div>
    </Modal>
  );
};
