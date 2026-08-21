import React from "react";
import { Layers } from "lucide-react";

export interface ScreenGridItemProps {
  name: string;
  groupName: string | null;
  programName: string | null;
  overrideProgramName: string | null;
  overrideUntilMs: number | null;
  isSelected?: boolean;
  onClick?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
}

export const ScreenGridItem: React.FC<ScreenGridItemProps> = ({
  name,
  groupName,
  programName,
  overrideProgramName,
  overrideUntilMs,
  isSelected = false,
  onClick,
  onContextMenu,
}) => {
  return (
    <div
      className={`relative w-full aspect-video rounded-lg overflow-hidden cursor-pointer group border border-gray-200 dark:border-gray-700 ${
        isSelected ? "ring-2 ring-brand-500" : ""
      }`}
      onClick={(e) => { e.preventDefault(); e.stopPropagation(); onClick?.(); }}
      onContextMenu={onContextMenu}
    >
      {/* Intended program — the screen's own standing choice, never a
          device-derived preview */}
      <div className="absolute inset-0 bg-gradient-to-br from-gray-100 to-gray-200 dark:from-gray-800 dark:to-gray-900 flex flex-col items-center justify-center gap-1.5 p-4">
        {overrideProgramName ? (
          <>
            <p className="text-sm font-medium text-gray-800 dark:text-gray-100 text-center truncate max-w-full">
              {overrideProgramName}
            </p>
            <span className="inline-flex items-center bg-amber-100 text-amber-800 dark:bg-amber-900/50 dark:text-amber-200 text-[10px] font-semibold px-1.5 py-0.5 rounded-sm">
              Override until {overrideUntilMs != null ? new Date(overrideUntilMs).toLocaleString() : ""}
            </span>
          </>
        ) : programName ? (
          <div className="flex items-center gap-1.5 max-w-full">
            <Layers className="size-4 shrink-0 text-gray-400" />
            <p className="text-sm font-medium text-gray-800 dark:text-gray-100 truncate">
              {programName}
            </p>
          </div>
        ) : (
          <p className="text-sm text-gray-400 dark:text-gray-500">No program assigned</p>
        )}
      </div>

      {/* Bottom bar: screen name + group */}
      <div className="absolute inset-x-0 bottom-0 p-3 flex items-center justify-between gap-2 bg-white/70 dark:bg-black/40 backdrop-blur-sm">
        <p className="text-sm font-medium text-gray-800 dark:text-gray-100 truncate">{name}</p>
        {groupName && (
          <span className="text-[10px] font-medium text-gray-500 dark:text-gray-400 bg-gray-100 dark:bg-gray-800 px-1.5 py-0.5 rounded-sm shrink-0 truncate max-w-[40%]">
            {groupName}
          </span>
        )}
      </div>
    </div>
  );
};
