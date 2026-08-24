import React, { useState, useEffect, useCallback, useMemo } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft, Monitor, Plus, Trash2 } from "lucide-react";
import { Confirm, Inspector, Page, haptic, useToast } from "@/ds";
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

const RECURRENCES: Recurrence[] = ["none", "daily", "weekly", "monthly"];

const RECURRENCE_LABEL: Record<Recurrence, string> = {
  none: "Once",
  daily: "Daily",
  weekly: "Weekly",
  monthly: "Monthly",
};

function toCivil(value: string): string {
  return value.length === 16 ? `${value}:00` : value;
}

function fromCivil(value: string): string {
  return value.slice(0, 16);
}

function formatDuration(ms: number): string {
  return ms % 60000 === 0 ? `${ms / 60000} min` : `${ms / 1000} s`;
}

function formatWindowWhen(row: ProgramWindow): string {
  const start = row.start_local.replace("T", " ").slice(0, 16);
  return `${RECURRENCE_LABEL[row.recurrence]} · ${start} · ${formatDuration(row.duration_ms)}`;
}

export const ScreenDetail: React.FC<ScreenDetailProps> = ({ screenId }) => {
  const navigate = useNavigate();
  const toast = useToast();

  const [screen, setScreen] = useState<SignageScreen | null>(null);
  const [programs, setPrograms] = useState<SignageProgram[]>([]);
  const [groups, setGroups] = useState<SignageGroup[]>([]);
  const [loading, setLoading] = useState(true);
  const [deleting, setDeleting] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [error, setError] = useState("");

  const [name, setName] = useState("");
  const [selectedProgramId, setSelectedProgramId] = useState<string>("");

  const [overrideProgramId, setOverrideProgramId] = useState<string>("");
  const [overrideUntil, setOverrideUntil] = useState("");
  const [settingOverride, setSettingOverride] = useState(false);

  const [showWindowForm, setShowWindowForm] = useState(false);
  const [editingWindowId, setEditingWindowId] = useState<string | null>(null);
  const [wfProgram, setWfProgram] = useState("");
  const [wfStart, setWfStart] = useState("");
  const [wfDurationMin, setWfDurationMin] = useState("60");
  const [wfRecurrence, setWfRecurrence] = useState<Recurrence>("none");
  const [wfTimezone, setWfTimezone] = useState(
    Intl.DateTimeFormat().resolvedOptions().timeZone,
  );
  const [wfPriority, setWfPriority] = useState("0");
  const [wfEnabled, setWfEnabled] = useState(true);
  const [windowFormError, setWindowFormError] = useState("");
  const [savingWindow, setSavingWindow] = useState(false);

  const programNames = useMemo(
    () => new Map(programs.map((p) => [p.id, p.name])),
    [programs],
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
        fetchPrograms().then(setPrograms).catch(() => undefined),
        fetchGroups().then(setGroups).catch(() => undefined),
      ]);
      setLoading(false);
    };
    void loadData();
  }, [refreshScreen]);

  const activeOverride =
    screen?.intent.over && screen.intent.over.until_unix_ms > Date.now()
      ? screen.intent.over
      : null;
  const group = screen?.group
    ? groups.find((g) => g.id === screen.group) ?? null
    : null;

  const saveName = async () => {
    if (!screen) return;
    const next = name.trim();
    if (!next || next === screen.name) return;
    setError("");
    try {
      await saveScreen({ ...screen, name: next });
      haptic("save");
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to rename screen");
      haptic("error");
    }
  };

  const handleGroupChange = async (groupId: string) => {
    setError("");
    try {
      await setScreenGroup(screenId, groupId || null);
      haptic("save");
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to move screen");
      haptic("error");
    }
  };

  const handleProgramChange = async (programId: string) => {
    setSelectedProgramId(programId);
    setError("");
    try {
      if (programId) await assignProgramToScreen(screenId, programId);
      else await removeProgramFromScreen(screenId);
      haptic("save");
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to assign program");
      haptic("error");
    }
  };

  const handleSetOverride = async () => {
    if (!overrideProgramId || !overrideUntil) {
      setError("Pick a program and an end time for the override");
      return;
    }
    setSettingOverride(true);
    setError("");
    try {
      await setScreenOverride(
        screenId,
        overrideProgramId,
        new Date(overrideUntil).getTime(),
      );
      setOverrideProgramId("");
      setOverrideUntil("");
      haptic("save");
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to set override");
      haptic("error");
    } finally {
      setSettingOverride(false);
    }
  };

  const handleClearOverride = async () => {
    setError("");
    try {
      await clearScreenOverride(screenId);
      haptic("save");
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to clear override");
      haptic("error");
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

  const closeWindowForm = () => {
    setShowWindowForm(false);
    setEditingWindowId(null);
    resetWindowForm();
  };

  const handleEditWindow = (row: ProgramWindow) => {
    setEditingWindowId(row.id);
    setWfProgram(row.program);
    setWfStart(fromCivil(row.start_local));
    setWfDurationMin(String(row.duration_ms / 60000));
    setWfRecurrence(row.recurrence);
    setWfTimezone(row.timezone);
    setWfPriority(String(row.priority));
    setWfEnabled(row.enabled);
    setWindowFormError("");
    setShowWindowForm(true);
  };

  const handleSaveWindow = async () => {
    setWindowFormError("");
    if (!wfProgram || !wfStart || !wfTimezone.trim()) {
      setWindowFormError("Program, start, and timezone are required");
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
      until_unix_ms: existing?.until_unix_ms ?? null,
      priority,
      enabled: wfEnabled,
      timezone: wfTimezone.trim(),
    };

    setSavingWindow(true);
    try {
      if (editingWindowId) {
        await updateScheduleWindow(screenId, {
          id: editingWindowId,
          program: wfProgram,
          ...window,
        });
      } else {
        await addScheduleWindow(screenId, wfProgram, window);
      }
      closeWindowForm();
      haptic("save");
      await refreshScreen();
    } catch (err) {
      setWindowFormError(err instanceof Error ? err.message : "Failed to save window");
      haptic("error");
    } finally {
      setSavingWindow(false);
    }
  };

  const handleDeleteWindow = async (windowId: string) => {
    try {
      await deleteScheduleWindow(screenId, windowId);
      haptic("delete");
      await refreshScreen();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete window");
      haptic("error");
    }
  };

  const handleDelete = async () => {
    setDeleting(true);
    setError("");
    try {
      await deleteScreen(screenId);
      haptic("delete");
      navigate({ to: "/screen-list" });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete screen");
      haptic("error");
      setDeleting(false);
    }
  };

  if (loading) {
    return (
      <Page>
        <p className="ds-hint">Loading screen…</p>
      </Page>
    );
  }

  if (!screen) {
    return (
      <Page>
        <p className="ds-danger-text">Screen not found</p>
      </Page>
    );
  }

  const directProgramName = screen.intent.base
    ? programNames.get(screen.intent.base.member) ?? "Unknown program"
    : null;
  const groupProgramName = group?.intent.base
    ? programNames.get(group.intent.base.member) ?? "Unknown program"
    : null;
  const overrideProgramName = activeOverride
    ? programNames.get(activeOverride.choice.member) ?? "Unknown program"
    : null;

  const playingName = overrideProgramName ?? directProgramName ?? groupProgramName;
  const playingKicker = activeOverride
    ? `Override until ${new Date(activeOverride.until_unix_ms).toLocaleString()}`
    : directProgramName
      ? "This screen’s own choice"
      : group
        ? `Following ${group.name}`
        : "Nothing assigned";

  return (
    <Page>
      <header className="ds-page-head">
        <div style={{ display: "flex", alignItems: "center", gap: 4, minWidth: 0 }}>
          <button
            type="button"
            className="ds-icon"
            aria-label="Back"
            onClick={() => navigate({ to: "/screen-list" })}
          >
            <ArrowLeft size={20} />
          </button>
          <h1 className="ds-page-title">{screen.name}</h1>
        </div>
        <button
          type="button"
          className="ds-icon"
          aria-label="Delete screen"
          onClick={() => setDeleteOpen(true)}
        >
          <Trash2 size={18} />
        </button>
      </header>

      {error && <p className="ds-danger-text">{error}</p>}

      <div className="ds-hero">
        <span className="ds-bezel">
          {playingName ? (
            <span className="ds-bezel-copy">
              <em>{activeOverride ? "Override" : "Playing"}</em>
              <strong>{playingName}</strong>
            </span>
          ) : (
            <Monitor size={22} strokeWidth={1.6} />
          )}
        </span>
        <div className="ds-hero-copy">
          <h2>{playingName ?? "Idle"}</h2>
          <p>{playingKicker}</p>
        </div>
      </div>

      <section className="ds-set">
        <div className="ds-set-head">
          <div>
            <h2>Screen</h2>
            <p>Name, network, and the standing program.</p>
          </div>
        </div>
        <label className="ds-set-row">
          <span>Name</span>
          <input
            className="ds-input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            onBlur={() => void saveName()}
          />
        </label>
        <label className="ds-set-row">
          <span>Network</span>
          <select
            className="ds-input"
            value={screen.group ?? ""}
            onChange={(event) => void handleGroupChange(event.target.value)}
          >
            <option value="">No network</option>
            {groups.map((g) => (
              <option key={g.id} value={g.id}>
                {g.name}
              </option>
            ))}
          </select>
        </label>
        <label className="ds-set-row">
          <span>Program</span>
          <select
            className="ds-input"
            value={selectedProgramId}
            onChange={(event) => void handleProgramChange(event.target.value)}
          >
            <option value="">None (follow network)</option>
            {programs.map((program) => (
              <option key={program.id} value={program.id}>
                {program.name}
              </option>
            ))}
          </select>
        </label>
      </section>

      <section className="ds-set">
        <div className="ds-set-head">
          <div>
            <h2>
              Override{" "}
              {activeOverride && <span className="ds-badge is-warn">Active</span>}
            </h2>
            <p>Takes the screen until a stated moment, then the standing choice returns.</p>
          </div>
        </div>
        {activeOverride ? (
          <div className="ds-event">
            <div>
              <strong>{overrideProgramName}</strong>
              <em>Until {new Date(activeOverride.until_unix_ms).toLocaleString()}</em>
            </div>
            <button
              type="button"
              className="ds-btn ds-btn-ghost"
              onClick={() => void handleClearOverride()}
            >
              Clear
            </button>
          </div>
        ) : (
          <>
            <label className="ds-set-row">
              <span>Program</span>
              <select
                className="ds-input"
                value={overrideProgramId}
                onChange={(event) => setOverrideProgramId(event.target.value)}
              >
                <option value="">Select…</option>
                {programs.map((program) => (
                  <option key={program.id} value={program.id}>
                    {program.name}
                  </option>
                ))}
              </select>
            </label>
            <div className="ds-set-row">
              <span>Until</span>
              <div className="ds-page-actions">
                <input
                  className="ds-input"
                  type="datetime-local"
                  value={overrideUntil}
                  onChange={(event) => setOverrideUntil(event.target.value)}
                />
                <button
                  type="button"
                  className="ds-btn ds-btn-solid"
                  disabled={settingOverride || !overrideProgramId || !overrideUntil}
                  onClick={() => void handleSetOverride()}
                >
                  {settingOverride ? "Setting…" : "Set"}
                </button>
              </div>
            </div>
          </>
        )}
      </section>

      <section className="ds-set">
        <div className="ds-set-head">
          <div>
            <h2>Schedule</h2>
            <p>Windows that take the screen at a time. Resolution stays with the engine.</p>
          </div>
          <button
            type="button"
            className="ds-btn ds-btn-ghost"
            onClick={() => {
              setEditingWindowId(null);
              resetWindowForm();
              setShowWindowForm(true);
            }}
          >
            <Plus size={16} />
            Add
          </button>
        </div>
        {screen.schedule.length > 0 ? (
          screen.schedule.map((row) => (
            <div
              key={row.id}
              className={`ds-event${row.enabled ? "" : " is-off"}`}
            >
              <div>
                <strong>{programNames.get(row.program) ?? "Unknown program"}</strong>
                <em>{formatWindowWhen(row)}</em>
              </div>
              <div className="ds-page-actions">
                <button
                  type="button"
                  className="ds-btn ds-btn-quiet"
                  onClick={() => handleEditWindow(row)}
                >
                  Edit
                </button>
                <button
                  type="button"
                  className="ds-icon"
                  aria-label="Delete window"
                  onClick={() => void handleDeleteWindow(row.id)}
                >
                  <Trash2 size={16} />
                </button>
              </div>
            </div>
          ))
        ) : (
          <p className="ds-hint" style={{ padding: "4px 16px 16px" }}>
            No scheduled windows.
          </p>
        )}
      </section>

      <Inspector
        open={showWindowForm}
        onOpenChange={(open) => {
          if (!open) closeWindowForm();
        }}
        className="ds-app-setup"
        title={editingWindowId ? "Edit window" : "Add window"}
        actions={
          <>
            <button
              type="button"
              className="ds-btn ds-btn-quiet"
              onClick={closeWindowForm}
            >
              Cancel
            </button>
            <button
              type="button"
              className="ds-btn ds-btn-solid"
              disabled={savingWindow}
              onClick={() => void handleSaveWindow()}
            >
              {savingWindow ? "Saving…" : editingWindowId ? "Update" : "Create"}
            </button>
          </>
        }
      >
        <label className="ds-field">
          <span>Program</span>
          <select
            className="ds-input"
            value={wfProgram}
            onChange={(event) => setWfProgram(event.target.value)}
          >
            <option value="">Select…</option>
            {programs.map((program) => (
              <option key={program.id} value={program.id}>
                {program.name}
              </option>
            ))}
          </select>
        </label>
        <label className="ds-field">
          <span>Start (local)</span>
          <input
            className="ds-input"
            type="datetime-local"
            value={wfStart}
            onChange={(event) => setWfStart(event.target.value)}
          />
        </label>
        <label className="ds-field">
          <span>Duration (minutes)</span>
          <input
            className="ds-input"
            type="number"
            min={1}
            value={wfDurationMin}
            onChange={(event) => setWfDurationMin(event.target.value)}
          />
        </label>
        <label className="ds-field">
          <span>Recurrence</span>
          <select
            className="ds-input"
            value={wfRecurrence}
            onChange={(event) => setWfRecurrence(event.target.value as Recurrence)}
          >
            {RECURRENCES.map((r) => (
              <option key={r} value={r}>
                {RECURRENCE_LABEL[r]}
              </option>
            ))}
          </select>
        </label>
        <label className="ds-field">
          <span>Timezone (IANA)</span>
          <input
            className="ds-input"
            value={wfTimezone}
            placeholder="America/Chicago"
            onChange={(event) => setWfTimezone(event.target.value)}
          />
        </label>
        <label className="ds-field">
          <span>Priority</span>
          <input
            className="ds-input"
            type="number"
            value={wfPriority}
            onChange={(event) => setWfPriority(event.target.value)}
          />
        </label>
        <label
          className="ds-field"
          style={{ flexDirection: "row", alignItems: "center", gap: 8 }}
        >
          <input
            type="checkbox"
            checked={wfEnabled}
            onChange={(event) => setWfEnabled(event.target.checked)}
          />
          <span>Enabled</span>
        </label>
        {windowFormError && <p className="ds-danger-text">{windowFormError}</p>}
      </Inspector>

      <Confirm
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        title={`Delete “${screen.name}”?`}
        description="This cannot be undone."
        confirmLabel={deleting ? "Deleting…" : "Delete"}
        danger
        onConfirm={handleDelete}
      />
    </Page>
  );
};
