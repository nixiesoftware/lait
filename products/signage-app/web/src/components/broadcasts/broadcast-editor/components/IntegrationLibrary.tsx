import React, { useState } from "react";
import { Integration } from "@/components/integrations/types/integrations";
import { Plus } from "lucide-react";

interface Props {
  integrations: Integration[];
  onActivate: (integration: Integration) => void;
}

export default function IntegrationLibrary({ integrations, onActivate }: Props) {
  const [activated, setActivated] = useState<Set<string>>(new Set());

  const handleActivate = (integration: Integration) => {
    if (!activated.has(integration.id)) {
      setActivated((prev) => new Set(prev).add(integration.id));
      onActivate(integration);
    }
  };

  const activeList = integrations.filter((i) => activated.has(i.id));
  const inactiveList = integrations.filter((i) => !activated.has(i.id));

return (
    <div className="space-y-1 p-4" onMouseDown={(e) => e.stopPropagation()} onClick={(e) => e.stopPropagation()} onWheel={(e) => e.stopPropagation()}>
      {activeList.map((integration) => (
        <div
          key={integration.id}
          className="flex items-center gap-3 p-2 bg-brand-50 dark:bg-brand-950 rounded-sm cursor-default"
        >
          <div className="w-6 h-6 flex items-center justify-center flex-shrink-0">
            <Plus className="w-4 h-4 text-brand-500" />
          </div>
          <div className="flex-1 min-w-0">
            <p className="text-sm font-medium text-brand-700 dark:text-brand-300 truncate">
              {integration.title}
            </p>
            <p className="text-xs text-brand-600 dark:text-brand-400 truncate">
              {integration.id}
            </p>
          </div>
        </div>
      ))}

      {activeList.length > 0 && inactiveList.length > 0 && (
        <div className="border-t border-gray-200 dark:border-gray-700 my-2" />
      )}

      {inactiveList.map((integration) => (
        <div
          key={integration.id}
          onClick={(e) => {
            e.stopPropagation();
            handleActivate(integration);
          }}
          className="flex items-center gap-3 p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-sm cursor-pointer group"
        >
          <div className="w-6 h-6 flex items-center justify-center flex-shrink-0">
            <Plus className="w-4 h-4 text-gray-500 dark:text-gray-400" />
          </div>
          <div className="flex-1 min-w-0">
            <p className="text-sm font-medium text-gray-900 dark:text-white truncate">
              {integration.title}
            </p>
            <p className="text-xs text-gray-500 dark:text-gray-400 truncate">
              {integration.id}
            </p>
          </div>
        </div>
      ))}
    </div>
  );
}

