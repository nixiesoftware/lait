import React, { useState, useEffect, useCallback, useMemo } from "react";
import { useNavigate } from "@tanstack/react-router";
import Button from "@/components/ui/button/Button";
import Input from "@/components/form/input/InputField";
import { useModal } from "@/hooks/useModal";
import { ConfirmationModal } from "@/components/ui/ConfirmationModal";
import { ArrowLeft, Calendar, Check, Plus, Trash2, X } from "lucide-react";
import { PencilIcon } from "../../../../../../public/images/icons/theme-icons";
import {
  fetchScreen,
  saveScreen,
  deleteScreen,
  assignProgramToScreen,
  removeProgramFromScreen,
  setScreenGroup,
} from "@/utils/screens/api";
import { fetchPrograms } from "@/utils/broadcasts/api";
import { fetchGroups } from "@/utils/networks/api";
import {
  addScheduleWindow,
  updateScheduleWindow,
  deleteScheduleWindow,
  setScreenOverride,
  clearScreenOverride,
} from "@/utils/schedules/api";
import type {
  ProgramWindow,
  Recurrence,
  ScheduleWindow,
  SignageGroup,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

interface ScreenDetailProps {
  screenId: string;
}

interface AlertProps {
  variant: "success" | "error";
  children: React.ReactNode;
  onClose?: () => void;
}

const Alert: React.FC<AlertProps> = ({ variant, children, onClose }) => {
  const bgColor = variant === "success" ? "bg-green-50 border-green-200 text-green-800 dark:bg-green-900/50 dark:border-green-800 dark:text-green-200" : "bg-red-50 border-red-200 text-red-800 dark:bg-red-900/50 dark:border-red-800 dark:text-red-200";

  return (
    <div className={`absolute bottom-3 mx-auto border rounded-lg p-4 ${bgColor}`}>
      <div className="flex justify-between items-start">
        <div>{children}</div>
        {onClose && (
          <button onClick={onClose} className="ml-2 text-current opacity-70 hover:opacity-100">
            ×
          </button>
        )}
      </div>
    </div>
  );
};

const RECURRENCES: Recurrence[] = ["none", "daily", "weekly", "monthly"];

/** datetime-local carries a civil datetime; the wire wants seconds too. */
function toCivil(value: string): string {
  return value.length === 16 ? `${value}:00` : value;
}

function fromCivil(value: string): string {
  return value.slice(0, 16);
}

function formatDuration(ms: number): string {
  return ms % 60000 === 0 ? `${ms / 60000} min` : `${ms / 1000} s`;
}

export const ScreenDetail: React.FC<ScreenDetailProps> = ({ screenId }) => {
  const navigate = useNavigate();
  const deleteModal = useModal();

  const [screen, setScreen] = useState<SignageScreen | null>(null);
  const [programs, setPrograms] = useState<SignageProgram[]>([]);
  const [groups, setGroups] = useState<SignageGroup[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState("");
  const [success, setSuccess] = useState("");

  // Form states
  const [name, setName] = useState("");
  const [selectedProgramId, setSelectedProgramId] = useState<string>("");

  // Override form
  const [overrideProgramId, setOverrideProgramId] = useState<string>("");
  const [overrideUntil, setOverrideUntil] = useState("");
  const [settingOverride, setSettingOverride] = useState(false);

  // Schedule window form
  const [showWindowForm, setShowWindowForm] = useState(false);
  const [editingWindowId, setEditingWindowId] = useState<string | null>(null);
  const [wfProgram, setWfProgram] = useState("");
  const [wfStart, setWfStart] = useState("");
  const [wfDurationMin, setWfDurationMin] = useState("60");
  const [wfRecurrence, setWfRecurrence] = useState<Recurrence>("none");
  const [wfTimezone, setWfTimezone] = useState(
    Intl.DateTimeFormat().resolvedOptions().timeZone
  );
  const [wfPriority, setWfPriority] = useState("0");
  const [wfEnabled, setWfEnabled] = useState(true);
  const [windowFormError, setWindowFormError] = useState("");
  const [savingWindow, setSavingWindow] = useState(false);

  const programNames = useMemo(
    () => new Map(programs.map((p) => [p.id, p.name])),
    [programs]
  );

  const refreshScreen = useCallback(async () => {
    try {
      const data = await fetchScreen(screenId);
      setScreen(data);
      if (data) {
        setName(data.name);
        setSelectedProgramId(data.intent.base?.member ?? "");
      }
      return data;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch screen");
      return null;
    }
  }, [screenId]);

  useEffect(() => {
    const loadData = async () => {
      setLoading(true);
      await Promise.all([
        refreshScreen(),
        fetchPrograms().then(setPrograms).catch((err) => {
          console.error("Failed to fetch broadcasts:", err);
        }),
        fetchGroups().then(setGroups).catch((err) => {
          console.error("Failed to fetch networks:", err);
        }),
      ]);
      setLoading(false);
    };

    loadData();
  }, [refreshScreen]);

  // The ladder's inputs — resolution belongs to the engine
  const activeOverride =
    screen?.intent.over && screen.intent.over.until_unix_ms > Date.now()
      ? screen.intent.over
      : null;
  const group = screen?.group
    ? groups.find((g) => g.id === screen.group) ?? null
    : null;

  const handleSaveAll = async () => {
    if (!screen) return;
    setSaving(true);
    setError("");
    setSuccess("");

    try {
      const successMessages: string[] = [];

      if (name.trim() && name.trim() !== screen.name) {
        await saveScreen({ ...screen, name: name.trim() });
        successMessages.push("Screen renamed");
      }

      const currentDirect = screen.intent.base?.member ?? "";
      if (selectedProgramId !== currentDirect) {
        if (selectedProgramId) {
          await assignProgramToScreen(screenId, selectedProgramId);
          successMessages.push("Broadcast assigned");
        } else {
          await removeProgramFromScreen(screenId);
          successMessages.push("Broadcast cleared");
        }
      }

      if (successMessages.length > 0) {
        setSuccess(successMessages.join(" and ") + " successfully");
        await refreshScreen();
      } else {
        setSuccess("No changes to save");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to save changes");
    } finally {
      setSaving(false);
    }
  };

  const handleGroupChange = async (groupId: string) => {
    setError("");
    try {
      await setScreenGroup(screenId, groupId || null);
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to move screen");
    }
  };

  const handleClearDirect = async () => {
    setError("");
    try {
      await removeProgramFromScreen(screenId);
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to clear broadcast");
    }
  };

  const handleSetOverride = async () => {
    if (!overrideProgramId || !overrideUntil) {
      setError("Pick a broadcast and an end time for the override");
      return;
    }
    setSettingOverride(true);
    setError("");
    try {
      await setScreenOverride(
        screenId,
        overrideProgramId,
        new Date(overrideUntil).getTime()
      );
      setOverrideProgramId("");
      setOverrideUntil("");
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to set override");
    } finally {
      setSettingOverride(false);
    }
  };

  const handleClearOverride = async () => {
    setError("");
    try {
      await clearScreenOverride(screenId);
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to clear override");
    }
  };

  const resetWindowForm = () => {
    setWfProgram("");
    setWfStart("");
    setWfDurationMin("60");
    setWfRecurrence("none");
    setWfTimezone(Intl.DateTimeFormat().resolvedOptions().timeZone);
    setWfPriority("0");
    setWfEnabled(true);
    setWindowFormError("");
  };

  const handleEditWindow = (row: ProgramWindow) => {
    setEditingWindowId(row.id);
    setWfProgram(row.program);
    setWfStart(fromCivil(row.window.start_local));
    setWfDurationMin(String(row.window.duration_ms / 60000));
    setWfRecurrence(row.window.recurrence);
    setWfTimezone(row.window.timezone);
    setWfPriority(String(row.window.priority));
    setWfEnabled(row.window.enabled);
    setWindowFormError("");
    setShowWindowForm(true);
  };

  const handleSaveWindow = async () => {
    setWindowFormError("");
    if (!wfProgram || !wfStart || !wfTimezone.trim()) {
      setWindowFormError("Broadcast, start, and timezone are required");
      return;
    }
    const durationMin = Number(wfDurationMin);
    if (!Number.isFinite(durationMin) || durationMin <= 0) {
      setWindowFormError("Duration must be a positive number of minutes");
      return;
    }
    const priority = Number(wfPriority);
    if (!Number.isInteger(priority)) {
      setWindowFormError("Priority must be an integer");
      return;
    }

    const existing = editingWindowId
      ? screen?.schedule.find((row) => row.id === editingWindowId)
      : undefined;
    const window: ScheduleWindow = {
      start_local: toCivil(wfStart),
      duration_ms: Math.round(durationMin * 60000),
      recurrence: wfRecurrence,
      until_unix_ms: existing?.window.until_unix_ms ?? null,
      priority,
      enabled: wfEnabled,
      timezone: wfTimezone.trim(),
    };

    setSavingWindow(true);
    try {
      if (editingWindowId) {
        await updateScheduleWindow(screenId, {
          id: editingWindowId,
          window,
          program: wfProgram,
        });
      } else {
        await addScheduleWindow(screenId, wfProgram, window);
      }
      setShowWindowForm(false);
      setEditingWindowId(null);
      resetWindowForm();
      await refreshScreen();
    } catch (err) {
      setWindowFormError(err instanceof Error ? err.message : "Failed to save window");
    } finally {
      setSavingWindow(false);
    }
  };

  const handleDeleteWindow = async (windowId: string) => {
    try {
      await deleteScheduleWindow(screenId, windowId);
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete window");
    }
  };

  const handleDelete = async () => {
    setDeleting(true);
    setError("");

    try {
      await deleteScreen(screenId);
      navigate({ to: "/screen-list" });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete screen");
      setDeleting(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <div className="text-lg">Loading screen details...</div>
      </div>
    );
  }

  if (!screen) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <div className="text-lg text-red-500">Screen not found</div>
      </div>
    );
  }

  const handleBackClick = () => {
    window.history.back();
  };

  const directProgramName = screen.intent.base
    ? programNames.get(screen.intent.base.member) ?? "Unknown broadcast"
    : null;
  const groupProgramName = group?.intent.base
    ? programNames.get(group.intent.base.member) ?? "Unknown broadcast"
    : null;

  return (
    <div className="space-y-6">
      {error && (
        <Alert variant="error" onClose={() => setError("")}>
          {error}
        </Alert>
      )}

      {success && (
        <Alert variant="success" onClose={() => setSuccess("")}>
          {success}
        </Alert>
      )}

      <div className="flex flex-row justify-between items-center">
        <div className="flex flex-row items-center gap-4">
          {/* Back Button */}
          <button
            onClick={handleBackClick}
            className="p-2 hover:bg-gray-800/20 dark:hover:bg-gray-50/20 rounded-md transition-colors"
          >
            <ArrowLeft className="w-5 h-5 text-gray-800 dark:text-white" />
          </button>
          <h2 className="text-3xl font-semibold dark:text-white">{screen.name}</h2>
        </div>
        <button
          onClick={deleteModal.openModal}
          className="text-red-600 !border-none hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-900/20 mr-2 p-2 rounded-md"
        >
          <Trash2 className="w-5 h-5"/>
        </button>
      </div>

      {/* Screen settings: name, network, direct broadcast */}
      <div className="border border-gray-200 dark:border-gray-700 rounded-lg p-4 space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
          <div>
            <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Name</label>
            <Input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="!h-11"
            />
          </div>
          <div>
            <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Network</label>
            <select
              className="h-11 w-full appearance-none rounded-md border border-gray-300 px-4 py-2.5 text-sm shadow-theme-xs focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10 dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 dark:focus:border-brand-800"
              value={screen.group ?? ""}
              onChange={(e) => handleGroupChange(e.target.value)}
            >
              <option value="">No network</option>
              {groups.map((g) => (
                <option key={g.id} value={g.id}>{g.name}</option>
              ))}
            </select>
          </div>
          <div>
            <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Broadcast</label>
            <select
              className="h-11 w-full appearance-none rounded-md border border-gray-300 px-4 py-2.5 text-sm shadow-theme-xs focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10 dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 dark:focus:border-brand-800"
              value={selectedProgramId}
              onChange={(e) => setSelectedProgramId(e.target.value)}
            >
              <option value="">None (follow network)</option>
              {programs.map((program) => (
                <option key={program.id} value={program.id}>
                  {program.name}
                </option>
              ))}
            </select>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            onClick={handleSaveAll}
            disabled={saving || !name.trim()}
            className="px-8"
          >
            {saving ? "Saving..." : "Save"}
          </Button>
          {screen.intent.base && (
            <Button variant="outline" onClick={handleClearDirect}>
              Clear broadcast
            </Button>
          )}
          <p className="text-sm text-gray-500 dark:text-gray-400">
            {directProgramName
              ? `This screen chooses "${directProgramName}" itself.`
              : group
                ? groupProgramName
                  ? `No choice of its own — follows "${group.name}" (${groupProgramName}).`
                  : `No choice of its own — follows "${group.name}", which has no broadcast either.`
                : "No broadcast chosen and no network to fall back to."}
          </p>
        </div>
      </div>

      {/* Override — beats everything until a stated moment */}
      <div className="border border-gray-200 dark:border-gray-700 rounded-lg p-4 space-y-3">
        <div className="flex items-center gap-2">
          <h3 className="text-lg font-medium dark:text-white">Override</h3>
          {activeOverride && (
            <span className="text-xs px-2 py-0.5 bg-amber-100 text-amber-800 dark:bg-amber-900/50 dark:text-amber-200 rounded-full">
              Active
            </span>
          )}
        </div>
        {activeOverride ? (
          <div className="flex flex-wrap items-center gap-3">
            <p className="text-sm text-gray-700 dark:text-gray-300">
              {programNames.get(activeOverride.choice.member) ?? "Unknown broadcast"}
              {" "}until {new Date(activeOverride.until_unix_ms).toLocaleString()}
            </p>
            <Button size="sm" variant="outline" onClick={handleClearOverride}>
              <X className="w-4 h-4 mr-1" />
              Clear override
            </Button>
          </div>
        ) : (
          <div className="flex flex-wrap items-end gap-3">
            <div>
              <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Broadcast</label>
              <select
                value={overrideProgramId}
                onChange={(e) => setOverrideProgramId(e.target.value)}
                className="h-9 rounded-md border border-gray-300 px-3 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
              >
                <option value="">Select broadcast...</option>
                {programs.map((program) => (
                  <option key={program.id} value={program.id}>{program.name}</option>
                ))}
              </select>
            </div>
            <div>
              <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Until</label>
              <input
                type="datetime-local"
                value={overrideUntil}
                onChange={(e) => setOverrideUntil(e.target.value)}
                className="h-9 rounded-md border border-gray-300 px-3 py-1.5 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
              />
            </div>
            <Button
              size="sm"
              onClick={handleSetOverride}
              disabled={settingOverride || !overrideProgramId || !overrideUntil}
            >
              {settingOverride ? "Setting..." : "Set override"}
            </Button>
          </div>
        )}
      </div>

      {/* Schedule Section */}
      <div className="border border-gray-200 dark:border-gray-700 rounded-lg p-4">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            <Calendar className="w-4 h-4 text-gray-500 dark:text-gray-400" />
            <h3 className="text-lg font-medium dark:text-white">Scheduled Broadcasts</h3>
          </div>
          {!showWindowForm && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setEditingWindowId(null);
                resetWindowForm();
                setShowWindowForm(true);
              }}
            >
              <Plus className="w-4 h-4 mr-1" />
              Add Window
            </Button>
          )}
        </div>

        {/* Window form (add or edit) */}
        {showWindowForm && (
          <div className="mb-4 p-3 bg-gray-50 dark:bg-gray-800 rounded-md border border-gray-200 dark:border-gray-700 space-y-3">
            <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Broadcast</label>
                <select
                  value={wfProgram}
                  onChange={(e) => setWfProgram(e.target.value)}
                  className="h-9 w-full rounded-md border border-gray-300 px-3 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
                >
                  <option value="">Select broadcast...</option>
                  {programs.map((program) => (
                    <option key={program.id} value={program.id}>{program.name}</option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Start (local time)</label>
                <input
                  type="datetime-local"
                  value={wfStart}
                  onChange={(e) => setWfStart(e.target.value)}
                  className="h-9 w-full rounded-md border border-gray-300 px-3 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
                />
              </div>
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Duration (minutes)</label>
                <input
                  type="number"
                  min="1"
                  value={wfDurationMin}
                  onChange={(e) => setWfDurationMin(e.target.value)}
                  className="h-9 w-full rounded-md border border-gray-300 px-3 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
                />
              </div>
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Recurrence</label>
                <select
                  value={wfRecurrence}
                  onChange={(e) => setWfRecurrence(e.target.value as Recurrence)}
                  className="h-9 w-full rounded-md border border-gray-300 px-3 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
                >
                  {RECURRENCES.map((r) => (
                    <option key={r} value={r}>{r}</option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Timezone (IANA)</label>
                <input
                  type="text"
                  value={wfTimezone}
                  onChange={(e) => setWfTimezone(e.target.value)}
                  placeholder="America/Chicago"
                  className="h-9 w-full rounded-md border border-gray-300 px-3 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
                />
              </div>
              <div className="flex items-end gap-3">
                <div className="flex-1">
                  <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">Priority</label>
                  <input
                    type="number"
                    value={wfPriority}
                    onChange={(e) => setWfPriority(e.target.value)}
                    className="h-9 w-full rounded-md border border-gray-300 px-3 text-sm dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10"
                  />
                </div>
                <label className="flex items-center gap-2 h-9 cursor-pointer select-none">
                  <input
                    type="checkbox"
                    checked={wfEnabled}
                    onChange={(e) => setWfEnabled(e.target.checked)}
                    className="w-4 h-4 rounded border-gray-300 text-brand-600 focus:ring-brand-500 dark:border-gray-600 dark:bg-gray-800"
                  />
                  <span className="text-sm text-gray-700 dark:text-gray-300">Enabled</span>
                </label>
              </div>
            </div>
            {windowFormError && (
              <p className="text-xs text-red-600 dark:text-red-400">{windowFormError}</p>
            )}
            <div className="flex gap-2">
              <Button size="sm" onClick={handleSaveWindow} disabled={savingWindow}>
                <Check className="w-4 h-4 mr-1" />
                {savingWindow ? "Saving..." : editingWindowId ? "Update" : "Create"}
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={() => { setShowWindowForm(false); setEditingWindowId(null); resetWindowForm(); }}
              >
                Cancel
              </Button>
            </div>
          </div>
        )}

        {/* Window table — the schedule as data; what plays now is the engine's call */}
        {screen.schedule.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs text-gray-500 dark:text-gray-400 border-b border-gray-200 dark:border-gray-700">
                  <th className="py-2 pr-3 font-medium">Broadcast</th>
                  <th className="py-2 pr-3 font-medium">Start (local)</th>
                  <th className="py-2 pr-3 font-medium">Duration</th>
                  <th className="py-2 pr-3 font-medium">Recurrence</th>
                  <th className="py-2 pr-3 font-medium">Timezone</th>
                  <th className="py-2 pr-3 font-medium">Priority</th>
                  <th className="py-2 pr-3 font-medium">Enabled</th>
                  <th className="py-2 font-medium"></th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
                {screen.schedule.map((row) => (
                  <tr key={row.id} className={row.window.enabled ? "" : "opacity-50"}>
                    <td className="py-2 pr-3 font-medium dark:text-white">
                      {programNames.get(row.program) ?? "Unknown broadcast"}
                    </td>
                    <td className="py-2 pr-3 text-gray-600 dark:text-gray-300 whitespace-nowrap">
                      {row.window.start_local.replace("T", " ")}
                    </td>
                    <td className="py-2 pr-3 text-gray-600 dark:text-gray-300 whitespace-nowrap">
                      {formatDuration(row.window.duration_ms)}
                    </td>
                    <td className="py-2 pr-3 text-gray-600 dark:text-gray-300">
                      {row.window.recurrence}
                    </td>
                    <td className="py-2 pr-3 text-gray-600 dark:text-gray-300 whitespace-nowrap">
                      {row.window.timezone}
                    </td>
                    <td className="py-2 pr-3 text-gray-600 dark:text-gray-300">
                      {row.window.priority}
                    </td>
                    <td className="py-2 pr-3 text-gray-600 dark:text-gray-300">
                      {row.window.enabled ? "Yes" : "No"}
                    </td>
                    <td className="py-2">
                      <div className="flex items-center gap-1 justify-end">
                        <button
                          onClick={() => handleEditWindow(row)}
                          className="p-1.5 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 rounded-md hover:bg-gray-100 dark:hover:bg-gray-800"
                          title="Edit"
                        >
                          <PencilIcon className="w-4 h-4" viewBox="0 0 22 22" />
                        </button>
                        <button
                          onClick={() => handleDeleteWindow(row.id)}
                          className="p-1.5 text-gray-400 hover:text-red-600 dark:hover:text-red-400 rounded-md hover:bg-gray-100 dark:hover:bg-gray-800"
                          title="Delete"
                        >
                          <Trash2 className="w-4 h-4" />
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          !showWindowForm && (
            <p className="text-sm text-gray-400 dark:text-gray-500 text-center py-4">
              No scheduled broadcasts. Add a window to schedule content.
            </p>
          )
        )}
      </div>

      <ConfirmationModal
        isOpen={deleteModal.isOpen}
        onClose={deleteModal.closeModal}
        showCloseButton={false}
        onConfirm={handleDelete}
        title={`Delete "${screen.name}"`}
        message={`This action cannot be undone.`}
        confirmText={deleting ? "Deleting..." : "Delete"}
        variant="danger"
      />
    </div>
  );
};
