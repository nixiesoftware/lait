import React from "react";
import { SortingPill, SortDirection } from "@/components/ui/sorting/SortingPill";
import { FilterPill } from "@/components/ui/sorting/FilterPill";

export interface SortingState {
  nameSort: SortDirection;
  dimensionSort: SortDirection;
  typeFilter: string | null;
}

interface ContentSortingBarProps {
  sortingState: SortingState;
  onSortingChange: (state: SortingState) => void;
  availableTypes?: string[];
  hideTypeFilter?: boolean;
  hideDimensionSort?: boolean;
}

export const ContentSortingBar: React.FC<ContentSortingBarProps> = ({
  sortingState,
  onSortingChange,
  availableTypes = ["image", "video"],
  hideTypeFilter = false,
  hideDimensionSort = false
}) => {
  const handleNameSort = (direction: SortDirection) => {
    onSortingChange({
      ...sortingState,
      nameSort: direction,
      dimensionSort: null // Clear other sorts
    });
  };

  const handleDimensionSort = (direction: SortDirection) => {
    onSortingChange({
      ...sortingState,
      dimensionSort: direction,
      nameSort: null // Clear other sorts
    });
  };

  const handleTypeFilter = (type: string | null) => {
    onSortingChange({
      ...sortingState,
      typeFilter: type
    });
  };

  const typeOptions = availableTypes.map(type => ({
    value: type,
    label: type.charAt(0).toUpperCase() + type.slice(1)
  }));

  return (
    <div className="flex items-center gap-1">
      <SortingPill
        label="Name"
        value={sortingState.nameSort}
        onChange={handleNameSort}
      />
      {!hideTypeFilter && (
        <FilterPill
          label="Type"
          options={typeOptions}
          value={sortingState.typeFilter}
          onChange={handleTypeFilter}
        />
      )}
      {!hideDimensionSort && (
        <SortingPill
          label="Dimensions"
          value={sortingState.dimensionSort}
          onChange={handleDimensionSort}
        />
      )}
    </div>
  );
};
