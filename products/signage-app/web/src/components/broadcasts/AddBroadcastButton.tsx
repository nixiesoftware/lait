import React from "react";
import {BiPlus} from "@react-icons/all-files/bi/BiPlus";
import { useCreateBroadcast } from "@/utils/navigation/hooks";

interface CreateBroadcastButtonProps {
  onAdd?: () => void;
}

export const AddBroadcastButton: React.FC<CreateBroadcastButtonProps> = ({ onAdd }) => {
  const { handleCreate, isCreating } = useCreateBroadcast(onAdd);

  const handleClick = async () => {
    await handleCreate(); // Uses default name "Untitled Broadcast"
  };

  return (
    <button className="px-3 py-2 text-sm font-medium bg-brand-500 text-white shadow-theme-xs hover:bg-brand-600 disabled:bg-brand-300
                        flex items-center gap-1.5 rounded-lg whitespace-nowrap transition" onClick={handleClick} disabled={isCreating}>
      <BiPlus className="size-4"/>
      {isCreating ? "Creating..." : "New broadcast"}
    </button>
  );
};
