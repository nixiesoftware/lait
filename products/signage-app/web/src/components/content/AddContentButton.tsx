import React, { useRef, useState } from "react";
import { uploadContentAll } from "@/utils/content/api";
import { UploadIcon } from "lucide-react";

interface AddContentButtonProps {
  onAdd?: () => void;
  onFilesSelected?: (files: File[]) => void;
  className?: string;
}

export const AddContentButton: React.FC<AddContentButtonProps> = ({ onAdd, onFilesSelected, className = "" }) => {
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [uploading, setUploading] = useState(false);

  const uploadFiles = async (files: File[]) => {
    setUploading(true);

    try {
      const outcome = await uploadContentAll(files);
      if (outcome.refused.length > 0) {
        alert(outcome.refused.map((r) => r.reason).join("\n"));
      }
      if (outcome.uploaded.length > 0) {
        onAdd?.();
      }
    } catch (err) {
      console.error("Upload error:", err);
    } finally {
      setUploading(false);
    }
  };

  const handleFileSelect = async (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files && e.target.files.length > 0) {
      const files = Array.from(e.target.files);

      if (onFilesSelected) {
        onFilesSelected(files);
      } else {
        await uploadFiles(files);
      }

      // Important: reset the input so selecting the same file again still triggers 'change'
      // This also prevents stale listeners on some browsers
      e.target.value = '';
    }
  };

  const handleClick = () => {
    fileInputRef.current?.click();
  };

  return (
    <>
      <button
        onClick={handleClick}
        disabled={uploading}
        className={`flex text-md font-medium items-center justify-center gap-2 px-2.5 py-1 bg-brand-500 text-white rounded-sm hover:bg-brand-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${className}`}
      >
        <UploadIcon className="w-4 h-4 text-white dark:text-gray-200" />
        {uploading ? "Uploading..." : "Upload"}
      </button>
      <input
        ref={fileInputRef}
        type="file"
        multiple
        accept="image/*,video/*"
        onChange={handleFileSelect}
        className="hidden"
      />
    </>
  );
};
