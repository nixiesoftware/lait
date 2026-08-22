import { useCallback, useEffect, useState } from "react";
import { IntegrationCard } from "@/components/integrations/IntegrationCard";
import { IntegrationConfigModal } from "@/components/integrations/IntegrationConfigModal";
import { KINDS, KindDefinition, fetchConfigs } from "@/utils/apps/api";
import type { SignageConfig } from "@/utils/lait/types";

export default function Integrations() {
  const [configs, setConfigs] = useState<SignageConfig[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editingKind, setEditingKind] = useState<KindDefinition | null>(null);

  const reload = useCallback(async () => {
    try {
      setError(null);
      setConfigs(await fetchConfigs());
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load integrations");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  const configFor = (kind: string): SignageConfig | null =>
    configs.find((c) => c.kind === kind) ?? null;

  return (
    <div className="space-y-4">
      {loading && (
        <p className="text-sm text-gray-500 dark:text-gray-400">Loading integrations…</p>
      )}
      {error && (
        <div className="rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-800 dark:border-red-800 dark:bg-red-900/30 dark:text-red-300">
          {error}
        </div>
      )}
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
        {KINDS.map((kind) => (
          <IntegrationCard
            key={kind.kind}
            id={kind.kind}
            name={kind.label}
            description={kind.description}
            configured={configFor(kind.kind) != null}
            onClick={() => setEditingKind(kind)}
          />
        ))}
      </div>
      <IntegrationConfigModal
        kind={editingKind}
        config={editingKind ? configFor(editingKind.kind) : null}
        onClose={() => setEditingKind(null)}
        onSaved={() => reload()}
      />
    </div>
  );
}
