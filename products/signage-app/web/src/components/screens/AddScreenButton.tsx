import React from "react";
import {MdAddToQueue} from "react-icons/md";
import {BiPlus} from "@react-icons/all-files/bi/BiPlus";

export interface AddScreenButtonProps {
  onOpenModal: () => void;
  className?: string;
}

export const AddScreenButton: React.FC<AddScreenButtonProps> = ({ onOpenModal, className }) => {
  return (
    <button
      className={`px-3 py-2 text-sm font-medium bg-brand-500 text-white shadow-theme-xs hover:bg-brand-600 disabled:bg-brand-300
                  flex items-center gap-1.5 rounded-lg whitespace-nowrap transition ${className || ''}`}
      onClick={onOpenModal}
    >
      <BiPlus className="size-4"/>
      Add Screen
    </button>
  );
};

