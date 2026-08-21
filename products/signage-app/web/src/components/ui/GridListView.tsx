import React, { ReactNode } from "react";

interface GridListViewProps<T> {
  items: T[];
  viewMode: "grid" | "list";
  onViewModeChange?: (mode: "grid" | "list") => void;
  renderGridItem?: (item: T, index: number) => ReactNode;
  renderListItem?: (item: T, index: number) => ReactNode;
  gridClassName?: string;
  listClassName?: string;
  maxItems?: number;
  emptyMessage?: string;
  isDesktop?: boolean;
  listHeaders?: ReactNode;
}

export function GridListView<T>({
  items,
  viewMode,
  renderGridItem,
  renderListItem,
  maxItems,
  gridClassName = "grid gap-4 grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6",
  listClassName = "rounded-sm overflow-x-hidden",
  emptyMessage = "No items found",
  listHeaders
}: GridListViewProps<T>) {
  if (items.length === 0) {
    return (
      <div className="flex items-center justify-center">
        <p className="text-gray-500 dark:text-gray-400">{emptyMessage}</p>
      </div>
    );
  }

  // Apply maxItems limit if specified
  const displayItems = maxItems ? items.slice(0, maxItems) : items;

  return (
    <>
      {/* Content display */}
      {viewMode === "grid" ? (
        <div className={gridClassName}>
          {displayItems.map((item, index) => renderGridItem && (renderGridItem(item, index)))}
        </div>
      ) : (
        <div className={listClassName}>
          <table className="w-full table-fixed">
            {listHeaders && (
              <thead>
                {listHeaders}
              </thead>
            )}
            <tbody className="divide-y divide-gray-200 dark:divide-gray-700">
              {displayItems.map((item, index) => renderListItem &&(renderListItem(item, index)))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
