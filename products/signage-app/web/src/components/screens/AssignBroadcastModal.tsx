import React, { useState, useEffect } from "react";
import { Modal } from "@/components/ui/modal";
import Button from "@/components/ui/button/Button";
import { Layers, Check, Search } from "lucide-react";
import { fetchPrograms } from "@/utils/broadcasts/api";

interface BroadcastOption {
  id: string;
  name: string;
  contentCount?: number;
}

interface AssignBroadcastModalProps {
  isOpen: boolean;
  onClose: () => void;
  screenId: string;
  screenName: string;
  currentBroadcastId?: string;
  onAssign: (screenId: string, broadcastId: string) => Promise<void>;
}

export const AssignBroadcastModal: React.FC<AssignBroadcastModalProps> = ({
  isOpen,
  onClose,
  screenId,
  screenName,
  currentBroadcastId,
  onAssign
}) => {
  const [broadcasts, setBroadcasts] = useState<BroadcastOption[]>([]);
  const [selectedBroadcastId, setSelectedBroadcastId] = useState<string | null>(currentBroadcastId || null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [searchTerm, setSearchTerm] = useState("");
  const [saving, setSaving] = useState(false);

  // Fetch broadcasts when modal opens
  const fetchBroadcasts = React.useCallback(async () => {
    setLoading(true);
    setError("");

    try {
      const data = await fetchPrograms();

      setBroadcasts(data.map((p) => ({
        id: p.id,
        name: p.name,
        contentCount: p.items.length
      })));
    } catch (err) {
      setError("Failed to load broadcasts");
      console.error(err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (isOpen) {
      fetchBroadcasts();
    }
  }, [isOpen, fetchBroadcasts]);


  const handleAssign = async () => {
    if (!selectedBroadcastId) return;

    setSaving(true);
    setError("");

    try {
      await onAssign(screenId, selectedBroadcastId);
      onClose();
    } catch (err) {
      setError((err as Error).message || "Failed to assign broadcast");
    } finally {
      setSaving(false);
    }
  };

  const filteredBroadcasts = broadcasts.filter(p =>
    p.name.toLowerCase().includes(searchTerm.toLowerCase())
  );

  return (
    <Modal isOpen={isOpen} onClose={onClose} className="max-w-xl !h-fit m-auto top-0 bottom-0 rounded-md
        border-1 border-gray-300 shadow-md p-6">
      <div className="space-y-4">
        <div>
          <h3 className="text-xl font-semibold mb-2 dark:text-white">Assign Broadcast to Screen</h3>
          <p className="text-sm text-gray-600 dark:text-gray-400">
            Select a broadcast to display on &quot;{screenName}&quot;
          </p>
        </div>

        {/* Search */}
        <div className="relative">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-gray-400" />
          <input
            type="text"
            placeholder="Search broadcasts..."
            value={searchTerm}
            onChange={(e) => setSearchTerm(e.target.value)}
            className="w-full pl-10 pr-4 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-800 text-gray-900 dark:text-white focus:outline-none focus:ring-2 focus:ring-brand-500"
          />
        </div>

        {/* Broadcast List */}
        <div className="border border-gray-200 dark:border-gray-700 rounded-lg max-h-80 overflow-y-auto">
          {loading ? (
            <div className="p-8 text-center text-gray-500">Loading broadcasts...</div>
          ) : error ? (
            <div className="p-8 text-center text-red-500">{error}</div>
          ) : filteredBroadcasts.length === 0 ? (
            <div className="p-8 text-center text-gray-500">
              {searchTerm ? "No broadcasts found" : "No broadcasts available"}
            </div>
          ) : (
            <div className="divide-y divide-gray-200 dark:divide-gray-700">
              {filteredBroadcasts.map((broadcast) => (
                <label
                  key={broadcast.id}
                  className="flex items-center p-4 hover:bg-gray-50 dark:hover:bg-gray-800 cursor-pointer transition-colors"
                >
                  <input
                    type="radio"
                    name="broadcast"
                    value={broadcast.id}
                    checked={selectedBroadcastId === broadcast.id}
                    onChange={() => setSelectedBroadcastId(broadcast.id)}
                    className="mr-3 text-brand-500 focus:ring-brand-500"
                  />
                  <div className="flex-1">
                    <div className="flex items-center gap-2">
                      <Layers className="w-4 h-4 text-gray-400" />
                      <span className="font-medium text-gray-900 dark:text-white">
                        {broadcast.name}
                      </span>
                      {currentBroadcastId === broadcast.id && (
                        <span className="text-xs bg-green-100 dark:bg-green-900 text-green-700 dark:text-green-300 px-2 py-0.5 rounded">
                          Current
                        </span>
                      )}
                    </div>
                    {broadcast.contentCount !== undefined && (
                      <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
                        {broadcast.contentCount} {broadcast.contentCount === 1 ? 'item' : 'items'}
                      </p>
                    )}
                  </div>
                  {selectedBroadcastId === broadcast.id && (
                    <Check className="w-5 h-5 text-brand-500" />
                  )}
                </label>
              ))}
            </div>
          )}
        </div>

        {error && (
          <p className="text-sm text-red-500">{error}</p>
        )}

        {/* Actions */}
        <div className="flex justify-end gap-3">
          <Button variant="outline" onClick={onClose} disabled={saving}>
            Cancel
          </Button>
          <Button
            onClick={handleAssign}
            disabled={!selectedBroadcastId || saving || selectedBroadcastId === currentBroadcastId}
          >
            {saving ? "Broadcasting..." : "Broadcast"}
          </Button>
        </div>
      </div>
    </Modal>
  );
};
