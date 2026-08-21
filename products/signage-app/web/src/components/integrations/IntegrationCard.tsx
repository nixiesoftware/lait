import React from "react";

export interface IntegrationCardProps {
  id: string;
  name: string;
  description: string;
  icon?: React.ReactNode;
  // configured drives the badge ("Configured" vs "Not configured") and is
  // the same flag the broadcast editor uses to filter its app picker —
  // a card showing "Configured" should always be addable to a broadcast.
  configured?: boolean;
  onClick?: (id: string, name: string) => void;
}

export const IntegrationCard: React.FC<IntegrationCardProps> = ({
  id,
  name,
  description,
  icon,
  configured,
  onClick,
}) => {
  return (
    <div
      onClick={() => onClick?.(id, name)}
      className="cursor-pointer bg-white dark:bg-[var(--dark-card)] dark:border-gray-800 border border-solid rounded-md p-4"
    >
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center space-x-3 min-w-0">
          {icon && <div className="text-2xl">{icon}</div>}
          <h3 className="text-xl font-semibold dark:text-white/90 truncate">{name}</h3>
        </div>
        {configured !== undefined && (
          <span
            className={
              configured
                ? "shrink-0 inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium bg-emerald-100 text-emerald-800 dark:bg-emerald-900/40 dark:text-emerald-300"
                : "shrink-0 inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium bg-gray-100 text-gray-700 dark:bg-gray-700 dark:text-gray-200"
            }
          >
            {configured ? "Configured" : "Not configured"}
          </span>
        )}
      </div>
      <p className="text-sm text-gray-500 mt-1">ID: {id}</p>
      <p className="mt-2 text-sm dark:text-white/80">{description}</p>
    </div>
  );
};
