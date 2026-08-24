import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { Monitor, Plus } from "lucide-react";
import {
  Confirm,
  DeviceRow,
  Empty,
  Inspector,
  Page,
  PageHeader,
  PageSearch,
  PageStatus,
  Picker,
  Prompt,
  haptic,
  useToast,
  type MenuItem,
  type PickItem,
} from "@/ds";
import {
  assignProgramToScreen,
  createScreen,
  deleteScreen,
  fetchScreens,
  saveScreen,
  setScreenGroup,
} from "@/utils/screens/api";
import {
  assignProgramToGroup,
  createGroup,
  deleteGroup,
  fetchGroups,
  saveGroup,
} from "@/utils/networks/api";
import { fetchPrograms } from "@/utils/broadcasts/api";
import type { SignageGroup, SignageProgram, SignageScreen } from "@/utils/lait/types";

export const ScreenList: React.FC = () => {
  const navigate = useNavigate();
  const { q: searchQuery } = useSearch({ strict: false }) as { q?: string };
  const toast = useToast();

  const [query, setQuery] = useState(searchQuery || "");
  const [screens, setScreens] = useState<SignageScreen[]>([]);
  const [networks, setNetworks] = useState<SignageGroup[]>([]);
  const [programs, setPrograms] = useState<SignageProgram[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [filter, setFilter] = useState<string | "all" | "ungrouped">("all");

  const [inspect, setInspect] = useState<SignageScreen | null>(null);
  const [networkInspect, setNetworkInspect] = useState<SignageGroup | null>(null);
  const [renameScreen, setRenameScreen] = useState("");
  const [renameNetwork, setRenameNetwork] = useState("");

  const [createScreenOpen, setCreateScreenOpen] = useState(false);
  const [createNetworkOpen, setCreateNetworkOpen] = useState(false);
  const [creatingScreen, setCreatingScreen] = useState(false);
  const [creatingNetwork, setCreatingNetwork] = useState(false);

  const [deleteScreenId, setDeleteScreenId] = useState<string | null>(null);
  const [deleteNetwork, setDeleteNetwork] = useState<SignageGroup | null>(null);

  const [assignScreen, setAssignScreen] = useState<SignageScreen | null>(null);
  const [assignNetwork, setAssignNetwork] = useState<SignageGroup | null>(null);
  const [moveScreen, setMoveScreen] = useState<SignageScreen | null>(null);

  useEffect(() => {
    setQuery(searchQuery || "");
  }, [searchQuery]);

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const [screensData, networksData, programsData] = await Promise.all([
        fetchScreens(),
        fetchGroups(),
        fetchPrograms(),
      ]);
      setScreens(screensData);
      setNetworks(networksData);
      setPrograms(programsData);
    } catch (err) {
      setError((err as Error).message || "Failed to fetch screens");
      setScreens([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const programNames = useMemo(
    () => new Map(programs.map((p) => [p.id, p.name])),
    [programs],
  );
  const networkNames = useMemo(
    () => new Map(networks.map((n) => [n.id, n.name])),
    [networks],
  );

  const activeOverride = (screen: SignageScreen) =>
    screen.intent.over && screen.intent.over.until_unix_ms > Date.now()
      ? screen.intent.over
      : null;

  const directProgramName = (screen: SignageScreen) => {
    const member = screen.intent.base?.member;
    return member ? programNames.get(member) ?? null : null;
  };

  const openDetail = (id: string) => navigate({ to: `/screen-list/${id}` });

  const saveScreenName = async (id: string, name: string) => {
    const screen = screens.find((s) => s.id === id);
    if (!screen || !name.trim()) return;
    try {
      await saveScreen({ ...screen, name: name.trim() });
      haptic("save");
      await load();
    } catch (err) {
      toast.show("Failed to rename screen", (err as Error).message);
      haptic("error");
    }
  };

  const confirmDeleteScreen = async () => {
    if (!deleteScreenId) return;
    try {
      await deleteScreen(deleteScreenId);
      if (inspect?.id === deleteScreenId) setInspect(null);
      haptic("delete");
      await load();
    } catch (err) {
      toast.show("Failed to delete screen", (err as Error).message);
      haptic("error");
    } finally {
      setDeleteScreenId(null);
    }
  };

  const confirmDeleteNetwork = async () => {
    if (!deleteNetwork) return;
    try {
      await deleteGroup(deleteNetwork.id);
      if (filter === deleteNetwork.id) setFilter("all");
      setNetworkInspect(null);
      haptic("delete");
      await load();
    } catch (err) {
      toast.show("Failed to delete network", (err as Error).message);
      haptic("error");
    } finally {
      setDeleteNetwork(null);
    }
  };

  const filtered = screens
    .filter((s) => {
      if (filter !== "all") {
        if (filter === "ungrouped" && s.group) return false;
        if (filter !== "ungrouped" && s.group !== filter) return false;
      }
      if (query) {
        const q = query.toLowerCase();
        const nameMatch = s.name.toLowerCase().includes(q);
        const programMatch = directProgramName(s)?.toLowerCase().includes(q);
        if (!nameMatch && !programMatch) return false;
      }
      return true;
    })
    .sort((a, b) => a.name.localeCompare(b.name));

  const menuFor = (screen: SignageScreen): MenuItem[] => [
    { label: "Open", onPick: () => openDetail(screen.id) },
    {
      label: "Open in new tab",
      onPick: () => window.open(`/screen-list/${screen.id}`, "_blank"),
    },
    {
      label: "Details",
      onPick: () => {
        setInspect(screen);
        setRenameScreen(screen.name);
      },
    },
    { label: "Assign a program", onPick: () => setAssignScreen(screen) },
    { label: "Move to network…", onPick: () => setMoveScreen(screen) },
    {
      label: "Delete",
      danger: true,
      onPick: () => setDeleteScreenId(screen.id),
    },
  ];

  const programItems: PickItem[] = programs.map((p) => ({
    id: p.id,
    label: p.name,
    hint: `${p.items.length} ${p.items.length === 1 ? "clip" : "clips"}`,
  }));

  const moveItems: PickItem[] = [
    ...networks.map((net) => ({
      id: net.id,
      label: net.name,
      disabled: moveScreen?.group === net.id,
      hint: moveScreen?.group === net.id ? "Current" : undefined,
    })),
    ...(networks.length > 0
      ? [{ id: "", label: "Remove from network", danger: true }]
      : []),
  ];

  return (
    <Page>
      <PageHeader title="Screens" icon={<Monitor size={20} />}>
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          onClick={() => setCreateScreenOpen(true)}
        >
          <Plus size={16} />
          Add screen
        </button>
        <button
          type="button"
          className="ds-btn ds-btn-ghost"
          onClick={() => setCreateNetworkOpen(true)}
        >
          New network
        </button>
      </PageHeader>

      <PageStatus loading={loading && screens.length === 0} error={error} />

      <div className="ds-chips" style={{ marginBottom: 12 }}>
        <button
          type="button"
          className={`ds-chip${filter === "all" ? " is-on" : ""}`}
          onClick={() => setFilter("all")}
        >
          All ({screens.length})
        </button>
        {networks.map((net) => (
          <button
            type="button"
            key={net.id}
            className={`ds-chip${filter === net.id ? " is-on" : ""}`}
            onClick={() => {
              if (filter === net.id) {
                setNetworkInspect(net);
                setRenameNetwork(net.name);
              } else {
                setFilter(net.id);
              }
            }}
            onContextMenu={(event) => {
              event.preventDefault();
              setNetworkInspect(net);
              setRenameNetwork(net.name);
            }}
          >
            {net.name} ({screens.filter((s) => s.group === net.id).length})
          </button>
        ))}
        <button
          type="button"
          className={`ds-chip${filter === "ungrouped" ? " is-on" : ""}`}
          onClick={() => setFilter("ungrouped")}
        >
          Ungrouped ({screens.filter((s) => !s.group).length})
        </button>
      </div>

      <div className="ds-toolbar">
        <PageSearch
          value={query}
          onChange={setQuery}
          placeholder="Filter screens…"
        />
      </div>

      {screens.length === 0 && !loading ? (
        <Empty title="Add your first screen.">
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            onClick={() => setCreateScreenOpen(true)}
          >
            Add screen
          </button>
        </Empty>
      ) : filtered.length === 0 ? (
        <Empty title="No screens match this filter." />
      ) : (
        <div className="ds-devices">
          {filtered.map((screen) => {
            const override = activeOverride(screen);
            const program = override
              ? (programNames.get(override.choice.member) ?? "Unknown program")
              : directProgramName(screen);
            const group = screen.group
              ? (networkNames.get(screen.group) ?? null)
              : null;
            return (
              <DeviceRow
                key={screen.id}
                name={screen.name}
                meta={[
                  override ? `Override · ${program}` : (program ?? "No program"),
                  group ?? "No network",
                ].join(" · ")}
                onOpen={() => openDetail(screen.id)}
                menu={menuFor(screen)}
                more={menuFor(screen)}
              >
                {program ? (
                  <span className="ds-bezel-copy">
                    <em>{override ? "Override" : "Playing"}</em>
                    <strong>{program}</strong>
                  </span>
                ) : (
                  <Monitor size={22} strokeWidth={1.6} />
                )}
              </DeviceRow>
            );
          })}
        </div>
      )}

      <Inspector
        open={inspect != null}
        onOpenChange={(open) => {
          if (!open) setInspect(null);
        }}
        title={inspect?.name ?? "Screen"}
        actions={
          inspect && (
            <>
              <button
                type="button"
                className="ds-btn ds-btn-solid"
                onClick={() => openDetail(inspect.id)}
              >
                Open
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-ghost"
                onClick={() => setAssignScreen(inspect)}
              >
                Assign program
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-danger"
                onClick={() => setDeleteScreenId(inspect.id)}
              >
                Delete
              </button>
            </>
          )
        }
      >
        {inspect && (
          <>
            <label className="ds-field">
              <span>Name</span>
              <input
                className="ds-input"
                value={renameScreen}
                onChange={(event) => setRenameScreen(event.target.value)}
                onBlur={() => {
                  if (renameScreen.trim() && renameScreen.trim() !== inspect.name) {
                    void saveScreenName(inspect.id, renameScreen);
                  }
                }}
              />
            </label>
            <p className="ds-hint">
              Network · {inspect.group ? networkNames.get(inspect.group) ?? inspect.group : "None"}
            </p>
            <p className="ds-hint">
              Program · {directProgramName(inspect) ?? "Follows network"}
            </p>
          </>
        )}
      </Inspector>

      <Inspector
        open={networkInspect != null}
        onOpenChange={(open) => {
          if (!open) setNetworkInspect(null);
        }}
        title={networkInspect?.name ?? "Network"}
        actions={
          networkInspect && (
            <>
              <button
                type="button"
                className="ds-btn ds-btn-ghost"
                onClick={() => setAssignNetwork(networkInspect)}
              >
                Assign program
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-danger"
                onClick={() => setDeleteNetwork(networkInspect)}
              >
                Delete
              </button>
            </>
          )
        }
      >
        {networkInspect && (
          <>
            <label className="ds-field">
              <span>Name</span>
              <input
                className="ds-input"
                value={renameNetwork}
                onChange={(event) => setRenameNetwork(event.target.value)}
                onBlur={() => {
                  if (
                    renameNetwork.trim() &&
                    renameNetwork.trim() !== networkInspect.name
                  ) {
                    void saveGroup({ ...networkInspect, name: renameNetwork.trim() })
                      .then(() => {
                        haptic("save");
                        return load();
                      })
                      .catch((err) => {
                        toast.show("Failed to rename network", (err as Error).message);
                        haptic("error");
                      });
                  }
                }}
              />
            </label>
            <p className="ds-hint">
              {screens.filter((s) => s.group === networkInspect.id).length} screens
            </p>
            <p className="ds-hint">
              Program ·{" "}
              {networkInspect.intent.base
                ? (programNames.get(networkInspect.intent.base.member) ??
                  networkInspect.intent.base.member)
                : "None"}
            </p>
          </>
        )}
      </Inspector>

      <Prompt
        open={createScreenOpen}
        onOpenChange={setCreateScreenOpen}
        title="Name this screen"
        placeholder="Lobby, reception…"
        confirmLabel="Add screen"
        busy={creatingScreen}
        onSubmit={async (name) => {
          setCreatingScreen(true);
          try {
            await createScreen(name);
            haptic("save");
            await load();
          } finally {
            setCreatingScreen(false);
          }
        }}
      />

      <Prompt
        open={createNetworkOpen}
        onOpenChange={setCreateNetworkOpen}
        title="Name this network"
        placeholder="Building A…"
        confirmLabel="Create network"
        busy={creatingNetwork}
        onSubmit={async (name) => {
          setCreatingNetwork(true);
          try {
            await createGroup(name);
            haptic("save");
            await load();
          } finally {
            setCreatingNetwork(false);
          }
        }}
      />

      <Picker
        open={assignScreen != null}
        onOpenChange={(open) => {
          if (!open) setAssignScreen(null);
        }}
        title={`Assign a program to ${assignScreen?.name ?? "this screen"}`}
        items={programItems}
        empty="Create a program first."
        onPick={(id) => {
          if (!assignScreen) return;
          void assignProgramToScreen(assignScreen.id, id)
            .then(() => {
              haptic("save");
              return load();
            })
            .catch((err) => {
              toast.show("Failed to assign program", (err as Error).message);
              haptic("error");
            });
        }}
      />

      <Picker
        open={assignNetwork != null}
        onOpenChange={(open) => {
          if (!open) setAssignNetwork(null);
        }}
        title={`Assign a program to ${assignNetwork?.name ?? "this network"}`}
        items={programItems}
        empty="Create a program first."
        onPick={(id) => {
          if (!assignNetwork) return;
          void assignProgramToGroup(assignNetwork.id, id)
            .then(() => {
              haptic("save");
              return load();
            })
            .catch((err) => {
              toast.show("Failed to assign program", (err as Error).message);
              haptic("error");
            });
        }}
      />

      <Picker
        open={moveScreen != null}
        onOpenChange={(open) => {
          if (!open) setMoveScreen(null);
        }}
        title={`Move ${moveScreen?.name ?? "screen"}`}
        items={moveItems}
        empty="Create a network first."
        onPick={(id) => {
          if (!moveScreen) return;
          void setScreenGroup(moveScreen.id, id || null)
            .then(() => {
              haptic("save");
              return load();
            })
            .catch((err) => {
              toast.show("Failed to move screen", (err as Error).message);
              haptic("error");
            });
        }}
      />

      <Confirm
        open={deleteScreenId != null}
        onOpenChange={(open) => {
          if (!open) setDeleteScreenId(null);
        }}
        title={`Delete “${screens.find((s) => s.id === deleteScreenId)?.name ?? inspect?.name ?? "this screen"}”?`}
        description="This cannot be undone."
        confirmLabel="Delete"
        danger
        onConfirm={confirmDeleteScreen}
      />

      <Confirm
        open={deleteNetwork != null}
        onOpenChange={(open) => {
          if (!open) setDeleteNetwork(null);
        }}
        title={`Delete “${deleteNetwork?.name}”?`}
        description="Screens in this network will become ungrouped."
        confirmLabel="Delete"
        danger
        onConfirm={confirmDeleteNetwork}
      />
    </Page>
  );
};
