import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ArrowLeft,
  Hash,
  KeyRound,
  Laptop,
  Palette,
  ShieldCheck,
  ShieldPlus,
  SlidersHorizontal,
  Tag,
  Trash2,
  Users,
  UsersRound,
  X,
} from "lucide-react";

import { rpc, spaceRpc } from "../api";
import type { AssignmentDto, LabelDto, MemberDto, ProjectDto, TeamDto } from "../types";
import { memberName } from "./Avatar";
import { catalogColor } from "./colors";
import { ColorPicker } from "./ColorPicker";
import * as ask from "./dialogs";
import { ProjectIcon, StatusIcon } from "./icons";
import { Members } from "./Members";
import { TeamsPanel } from "./TeamsPanel";
import { Combobox } from "./Picker";
import { Button, IconButton, TextArea, TextInput } from "@astryxdesign/core";
import { cn, navigationItem } from "./primitives";
import {
  SettingsField,
  SettingsPageHeader,
  SettingsSection,
  SettingsSurface,
} from "./settingsLayout";

type Tab = "general" | "teams" | "members" | "devices" | "labels" | "workflow" | "access";

const TABS: readonly Tab[] = ["general", "teams", "members", "devices", "labels", "workflow", "access"];

/** Narrow a route value to a sub-page. The route already refuses names it does
 *  not know, so this is the second half of one contract rather than a second
 *  list — but it is the half TypeScript can see. */
function isTab(value: string | null | undefined): value is Tab {
  return value !== null && value !== undefined && (TABS as readonly string[]).includes(value);
}

/**
 * The settings surface — the place a space is administered like an application.
 *
 * It is a real destination (a `settings` view/route), not a modal, because it hosts
 * several editors that each own state; a popover would throw that away on the first
 * outside click. The left rail is the taxonomy Linear uses — General, Labels,
 * Workflow — over the engine's now-mutable catalog (space rename, label lifecycle,
 * workflow states), which until recently was create-only.
 */
export function Settings({
  spaceId,
  spaceName,
  spaceDescription,
  labels,
  projects,
  teams,
  members,
  readOnly,
  revision,
  tab: routeTab,
  onTabChange,
  onError,
  onExit,
}: {
  spaceId: string;
  spaceName: string;
  spaceDescription: string;
  labels: LabelDto[];
  projects: ProjectDto[];
  teams: TeamDto[];
  members: MemberDto[];
  readOnly: boolean;
  /** Bumped by the doorbell; re-reads the panels that fetch. */
  revision: number;
  /** The sub-page, from the route. `null` is General. */
  tab: string | null;
  /** Report a sub-page change so the address can follow. Writing the URL here
   *  did not survive: `App` re-formats it from the route on every render. */
  onTabChange: (tab: string | null) => void;
  onError: (message: string) => void;
  /** Leave settings and return to the app — the workspace sidebar is collapsed
   *  while this page is open, so this is the way back. */
  onExit: () => void;
}) {
  /**
   * The sub-page comes from the route, not from `window.location` read here.
   *
   * It used to be local state seeded from `?tab=`, which wrote the parameter on
   * every change and read it back on mount — and still lost it, because `App`
   * re-formats the whole address from `ViewerRoute` and `formatRoute` had never
   * heard of `tab`. So `/settings?tab=members` put you on General, and the one
   * thing the parameter existed for — a linkable, reloadable sub-page, and a
   * driver that can `open` one without clicking — did not work.
   *
   * `/members` stays honoured as a legacy path.
   */
  const legacyMembersPath =
    window.location.pathname.split("/").filter(Boolean).at(-1) === "members";
  const tab: Tab = isTab(routeTab) ? routeTab : legacyMembersPath ? "members" : "general";
  const setTab = (next: Tab) => onTabChange(next === "general" ? null : next);
  // Reliable driver hook: `lait:nav { tab }` selects a sub-page without a click.
  useEffect(() => {
    const onNav = (event: Event) => {
      const t = (event as CustomEvent).detail?.tab;
      if (isTab(t)) onTabChange(t === "general" ? null : t);
    };
    window.addEventListener("lait:nav", onNav as EventListener);
    return () => window.removeEventListener("lait:nav", onNav as EventListener);
  }, [onTabChange]);
  const tabs: { id: Tab; label: string; icon: React.ReactNode }[] = [
    { id: "general", label: "General", icon: <SlidersHorizontal className="size-icon-sm" /> },
    // Beside Members, and above it, because a team is the container a member
    // is in — which is the order Linear puts them in for the same reason.
    { id: "teams", label: "Teams", icon: <UsersRound className="size-icon-sm" /> },
    { id: "members", label: "Members", icon: <Users className="size-icon-sm" /> },
    { id: "devices", label: "Devices & recovery", icon: <Laptop className="size-icon-sm" /> },
    { id: "labels", label: "Labels", icon: <Tag className="size-icon-sm" /> },
    { id: "workflow", label: "Workflow", icon: <Palette className="size-icon-sm" /> },
    { id: "access", label: "Roles & access", icon: <ShieldCheck className="size-icon-sm" /> },
  ];

  return (
    <div className="bg-sunken flex h-full min-h-0">
      <nav className="flex w-48 shrink-0 flex-col gap-0.5 p-2">
        <button
          onClick={onExit}
          className={cn(navigationItem({ size: "lg" }), "text-mute mb-3")}
        >
          <ArrowLeft className="size-icon-sm" />
          Back to app
        </button>
        {tabs.map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            className={cn(navigationItem({ selected: tab === t.id, size: "lg" }))}
          >
            {t.icon}
            {t.label}
          </button>
        ))}
      </nav>
      <section className="border-line bg-bg m-1 min-h-0 flex-1 overflow-hidden rounded-surface border">
        <div className="h-full overflow-y-auto px-6 py-7">
          <div
            className={cn(
              "mx-auto w-full",
              tab === "teams" || tab === "members" || tab === "labels"
                ? "max-w-4xl"
                : "max-w-2xl",
            )}
          >
            {tab === "general" && (
              <GeneralPanel
                spaceId={spaceId}
                spaceName={spaceName}
                spaceDescription={spaceDescription}
                readOnly={readOnly}
                onError={onError}
              />
            )}
            {tab === "teams" && (
              <TeamsPanel
                spaceId={spaceId}
                teams={teams}
                projects={projects}
                members={members}
                readOnly={readOnly}
                onError={onError}
              />
            )}
            {tab === "members" && (
              <Members
                spaceId={spaceId}
                revision={revision}
                readOnly={readOnly}
                onError={onError}
                embedded
              />
            )}
            {tab === "devices" && (
              <DevicesPanel
                spaceId={spaceId}
                readOnly={readOnly}
                revision={revision}
                onError={onError}
              />
            )}
            {tab === "labels" && (
              <LabelsPanel spaceId={spaceId} labels={labels} readOnly={readOnly} onError={onError} />
            )}
            {tab === "workflow" && (
              <WorkflowPanel
                spaceId={spaceId}
                projects={projects}
                readOnly={readOnly}
                revision={revision}
                onError={onError}
              />
            )}
            {tab === "access" && (
              <AccessPanel
                spaceId={spaceId}
                projects={projects}
                readOnly={readOnly}
                revision={revision}
                onError={onError}
              />
            )}
          </div>
        </div>
      </section>
    </div>
  );
}

/** General — the space's mutable display label, description, and immutable identity. */
function GeneralPanel({
  spaceId,
  spaceName,
  spaceDescription,
  readOnly,
  onError,
}: {
  spaceId: string;
  spaceName: string;
  spaceDescription: string;
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const [name, setName] = useState(spaceName);
  const [description, setDescription] = useState(spaceDescription);
  const [saving, setSaving] = useState(false);
  const [savingDesc, setSavingDesc] = useState(false);
  useEffect(() => setName(spaceName), [spaceName]);
  useEffect(() => setDescription(spaceDescription), [spaceDescription]);
  const dirty = name.trim() !== spaceName && name.trim() !== "";
  const descDirty = description !== spaceDescription;

  const save = async () => {
    setSaving(true);
    try {
      await rpc(spaceId, { cmd: "space_rename", name: name.trim() });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const saveDescription = async () => {
    setSavingDesc(true);
    try {
      await rpc(spaceId, { cmd: "space_describe", description: description.trim() });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSavingDesc(false);
    }
  };

  return (
    <>
      <SettingsPageHeader
        title="General"
        description="Manage the shared name, description, and immutable identity of this space."
      />
      <SettingsSurface>
        <SettingsField
          label="Space name"
          hint="A mutable display label. The space identity never changes."
        >
          <div className="flex items-center gap-2">
            <TextInput
              label="Space name"
              isLabelHidden
              value={name}
              isDisabled={readOnly}
              onChange={setName}
              className="flex-1"
              width="100%"
            />
            <Button
              isDisabled={!dirty || readOnly}
              isLoading={saving}
              onClick={() => void save()}
              label="Update"
              variant="primary"
              size="md"
            />
          </div>
        </SettingsField>
        <SettingsField
          label="Description"
          hint="Shared with everyone in the space."
          align="start"
        >
          <div className="flex flex-col gap-2">
            <TextArea
              label="Description"
              isLabelHidden
              value={description}
              isDisabled={readOnly}
              rows={3}
              placeholder="What is this space for? Goals, scope, links…"
              onChange={setDescription}
              aria-label="Space description"
              width="100%"
            />
            {!readOnly && (
              <div className="flex justify-end">
                <Button
                  isDisabled={!descDirty}
                  isLoading={savingDesc}
                  onClick={() => void saveDescription()}
                  label="Save description"
                  variant="primary"
                  size="md"
                />
              </div>
            )}
          </div>
        </SettingsField>
        <SettingsField
          label="Identity"
          hint="Derived at founding from keys, not the name. It cannot be changed."
        >
          <div className="border-line bg-hover text-dim flex items-center gap-2 rounded-control border px-2 py-1.5 font-mono text-xs">
            <Hash className="text-mute size-icon-sm shrink-0" />
            <span className="min-w-0 truncate">{spaceId}</span>
          </div>
        </SettingsField>
      </SettingsSurface>
    </>
  );
}

/**
 * Devices and recovery custody — the operator's half of a Space.
 *
 * Both halves were unreachable from every head until now, and each was a half
 * flow on its own. Enrolment has three steps on two machines: this one mints a
 * token, the new machine signs it (`host_device_consent`, the host plane —
 * store-free, because a machine joining an actor is a member of nothing yet),
 * and this one adds the signed blob. Shipping only the middle step meant nothing
 * could mint the token it consumes or add the blob it produces.
 *
 * Custody is the remedy the status panel's own warning demands: "Share
 * unreadable" / "Backup unverified" are read straight off `status.recovery`, and
 * a warning naming a repair no surface offers is worse than no warning.
 *
 * Every path here is a path on the machine running lait, not in the browser's
 * file picker: the daemon reads and writes it, and a share is key material that
 * deliberately never travels through this page.
 */
function DevicesPanel({
  spaceId,
  readOnly,
  revision,
  onError,
}: {
  spaceId: string;
  readOnly: boolean;
  revision: number;
  onError: (message: string) => void;
}) {
  const [devices, setDevices] = useState<string[]>([]);
  const [token, setToken] = useState<string | null>(null);
  const [busy, setBusy] = useState("");
  const [custodyPath, setCustodyPath] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [note, setNote] = useState("");

  const load = useCallback(async () => {
    try {
      const reply = await spaceRpc(spaceId, { cmd: "device_list" });
      if (reply.kind === "text") {
        // "no devices" is prose, not a row — rendering it would offer a revoke
        // button for a device that is not there.
        const rows = reply.text.split("\n").filter((line) => line.trim() !== "");
        setDevices(rows.length === 1 && rows[0] === "no devices" ? [] : rows);
      }
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  }, [spaceId, onError]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  const act = async (key: string, fn: () => Promise<string | null>) => {
    setBusy(key);
    setNote("");
    try {
      const said = await fn();
      if (said) setNote(said);
      await load();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy("");
    }
  };

  return (
    <>
      <SettingsPageHeader
        title="Devices & recovery"
        description="Manage the devices that sign as you and the custody material that protects recovery."
      />
      <SettingsSection
        title="Your devices"
        hint="Every machine that signs as you in this space. Each holds its own key; none of them is a copy of another."
      >
        <SettingsSurface>
          {devices.length === 0 ? (
            <div className="text-mute px-4 py-3 text-sm">Only this device.</div>
          ) : (
            <div className="divide-line divide-y">
            {devices.map((line) => (
              <div key={line} className="flex items-center gap-2 px-4 py-3 text-sm">
                <Laptop className="text-mute size-icon-sm shrink-0" />
                <code className="min-w-0 flex-1 truncate">{line}</code>
                {!readOnly && !line.includes("(this device)") && (
                  <IconButton
                    label="Revoke this device"
                    isDisabled={busy !== ""}
                    onClick={() =>
                      void act("revoke", async () => {
                        const device = line.trim().split(/\s+/)[0] ?? "";
                        if (
                          !(await ask.confirm({
                            title: `Revoke device ${device}?`,
                            body: "It stops signing as you. Content it already holds stays readable until an admin rotates the space key.",
                            confirmText: "Revoke",
                            danger: true,
                          }))
                        )
                          return null;
                        const reply = await spaceRpc(spaceId, { cmd: "device_revoke", device });
                        return reply.kind === "ok" ? reply.message : null;
                      })
                    }
                    variant="danger"
                    size="sm"
                    tooltip="Revoke this device"
                    icon={<Trash2 className="size-icon-sm" />}
                  />
                )}
              </div>
            ))}
            </div>
          )}
        </SettingsSurface>
      </SettingsSection>

      {!readOnly && (
        <SettingsSection
          title="Add a device"
          hint="Three steps, two machines. Nothing here leaves this space unencrypted."
        >
          <SettingsSurface>
            <SettingsField
              label="Enrollment token"
              hint="Mint it here, then open lait on the other machine and give it the token. That machine signs its consent."
              align="start"
            >
              <div className="flex flex-col items-end gap-2">
                <Button
                  isLoading={busy === "invite"}
                  isDisabled={busy !== ""}
                  onClick={() =>
                    void act("invite", async () => {
                      const reply = await spaceRpc(spaceId, { cmd: "device_invite" });
                      if (reply.kind === "text") setToken(reply.text.trim());
                      return null;
                    })
                  }
                  icon={<KeyRound className="size-icon-sm" />}
                  label="Mint token"
                  variant="ghost"
                  size="md"
                />
                {token && (
                <code className="border-line bg-hover block w-full rounded-control border p-2 font-mono text-xs break-all">
                  {token}
                </code>
              )}
              </div>
            </SettingsField>
            <SettingsField
              label="Signed consent"
              hint="Paste the consent returned by the other machine to finish enrollment."
            >
              <div className="flex justify-end">
                <Button
                  isLoading={busy === "add"}
                  isDisabled={busy !== ""}
                  onClick={() =>
                    void act("add", async () => {
                      const consent = await ask.prompt({
                        title: "Signed device consent",
                        body: "The hex blob the other machine produced from the token above.",
                        label: "Consent",
                      });
                      if (!consent?.trim()) return null;
                      const reply = await spaceRpc(spaceId, {
                        cmd: "device_add",
                        consent: consent.trim(),
                      });
                      setToken(null);
                      return reply.kind === "ok" ? reply.message : null;
                    })
                  }
                  label="Add device"
                  variant="ghost"
                  size="md"
                />
              </div>
            </SettingsField>
          </SettingsSurface>
        </SettingsSection>
      )}

      {!readOnly && (
        <SettingsSection
          title="Recovery custody"
          hint="Your share of this space's recovery authority, sealed with a passphrase. Export it somewhere you will still have when this machine is gone; import it when the status panel says the share is missing or unreadable."
        >
          <SettingsSurface>
            <SettingsField label="Custody file" hint="A path on the machine running lait.">
              <TextInput
                label="Custody file path"
                isLabelHidden
                value={custodyPath}
                placeholder="Path on the machine running lait"
                onChange={setCustodyPath}
                width="100%"
              />
            </SettingsField>
            <SettingsField label="Passphrase" hint="Seals the exported recovery share.">
              <TextInput
                type="password"
                label="Custody passphrase"
                isLabelHidden
                value={passphrase}
                placeholder="Passphrase"
                onChange={setPassphrase}
                width="100%"
              />
            </SettingsField>
            <SettingsField
              label="Recovery share"
              hint="Export a backup or import one when this machine's share is missing."
              align="start"
            >
              <div className="flex flex-wrap justify-end gap-2">
              <Button
                isLoading={busy === "export"}
                isDisabled={busy !== "" || !custodyPath.trim() || passphrase === ""}
                onClick={() =>
                  void act("export", async () => {
                    const reply = await spaceRpc(spaceId, {
                      cmd: "space_custody_export",
                      path: custodyPath.trim(),
                      passphrase,
                    });
                    setPassphrase("");
                    return reply.kind === "ok" ? reply.message : null;
                  })
                }
                label="Export share"
                variant="ghost"
                size="md"
              />
              <Button
                size="md"
                variant="ghost"
                label="Import share"
                isLoading={busy === "import"}
                isDisabled={busy !== "" || !custodyPath.trim() || passphrase === ""}
                onClick={() =>
                  void act("import", async () => {
                    const reply = await spaceRpc(spaceId, {
                      cmd: "space_custody_import",
                      path: custodyPath.trim(),
                      passphrase,
                      // Never blind: replacing a share that is merely unreadable
                      // by *this* build is how a recoverable space becomes an
                      // unrecoverable one.
                      force: await ask.confirm({
                        title: "Replace an existing share?",
                        body: "Answer yes only if this machine's current share is the broken one. Otherwise the import is refused when a share is already here, which is the safe answer.",
                        confirmText: "Replace it",
                        danger: true,
                      }),
                    });
                    setPassphrase("");
                    return reply.kind === "ok" ? reply.message : null;
                  })
                }
              />
              </div>
              {note && <p className="text-dim mt-2 text-sm">{note}</p>}
            </SettingsField>
          </SettingsSurface>
        </SettingsSection>
      )}
    </>
  );
}

/** Labels — the registry lifecycle the engine gained: create, rename, recolor, delete. */
function LabelsPanel({
  spaceId,
  labels,
  readOnly,
  onError,
}: {
  spaceId: string;
  labels: LabelDto[];
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [newColor, setNewColor] = useState("blue");
  const [editing, setEditing] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return needle ? labels.filter((label) => label.name.toLowerCase().includes(needle)) : labels;
  }, [labels, query]);

  const send = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  const create = () => {
    const name = newName.trim();
    if (!name) return;
    setNewName("");
    setCreating(false);
    void send(() => rpc(spaceId, { cmd: "label_new", name, color: newColor }));
  };

  return (
    <>
      <SettingsPageHeader
        title="Labels"
        description="Shared across every project. Renaming re-points every issue that uses one."
        actions={
          !readOnly && !creating ? (
            <Button label="New label" variant="primary" size="sm" onClick={() => setCreating(true)} />
          ) : undefined
        }
      />
      <div className="mb-4 max-w-md">
        <TextInput
          label="Filter labels"
          isLabelHidden
          value={query}
          onChange={setQuery}
          placeholder="Filter by name…"
          size="sm"
          width="100%"
        />
      </div>

      {!readOnly && creating && (
        <div className="border-line bg-raised mb-4 flex flex-col gap-3 rounded-surface border p-3">
          <input
            autoFocus
            value={newName}
            placeholder="Label name"
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && newName.trim()) create();
              if (e.key === "Escape") setCreating(false);
            }}
            className="border-line focus:border-line-strong rounded-control border bg-transparent px-2 py-1.5 text-sm outline-none"
            aria-label="New label name"
          />
          <ColorPicker value={newColor} onChange={setNewColor} />
          <div className="flex justify-end gap-2">
            <Button
              onClick={() => setCreating(false)}
              label="Cancel"
              variant="secondary"
              elevation="low"
              size="sm"
            />
            <Button
              isDisabled={!newName.trim()}
              onClick={create}
              label="Create label"
              variant="primary"
              size="sm"
            />
          </div>
        </div>
      )}

      {shown.length === 0 ? (
        <div className="text-mute flex min-h-64 items-center justify-center text-sm">
          {labels.length === 0 ? "No labels yet." : `Nothing matches “${query}”.`}
        </div>
      ) : (
      <ul className="border-line divide-line divide-y overflow-hidden rounded-surface border">
        {shown.map((l) =>
          editing === l.id ? (
            <LabelEditor
              key={l.id}
              label={l}
              onCancel={() => setEditing(null)}
              onSave={(name, color) => {
                setEditing(null);
                void send(() => rpc(spaceId, { cmd: "label_edit", label: l.id, name, color }));
              }}
            />
          ) : (
            <li
              key={l.id}
              className="group/label hover:bg-hover flex min-h-ctl-lg items-center gap-2 px-3 py-1.5"
            >
              <span
                className="size-mark-lg shrink-0 rounded-full"
                style={{ background: catalogColor(l.color) }}
              />
              <span className="min-w-0 flex-1 truncate text-sm">{l.name}</span>
              {!readOnly && (
                <span className="flex items-center gap-0.5 opacity-0 group-hover/label:opacity-100 focus-within:opacity-100">
                  <Button onClick={() => setEditing(l.id)} label="Edit" variant="ghost" size="sm" />
                  <IconButton
                    label={`Delete ${l.name}`}
                    onClick={() =>
                      void ask
                        .confirm({
                          title: `Delete label “${l.name}”?`,
                          body: "Issues keep the reference until it's re-created; it just leaves the registry.",
                          confirmText: "Delete",
                          danger: true,
                        })
                        .then((ok) => {
                          if (ok) void send(() => rpc(spaceId, { cmd: "label_delete", label: l.id }));
                        })
                    }
                    variant="ghost"
                    size="sm"
                    tooltip={`Delete ${l.name}`}
                    icon={<Trash2 className="size-icon-sm" />}
                  />
                </span>
              )}
            </li>
          ),
        )}
      </ul>
      )}
    </>
  );
}

function LabelEditor({
  label,
  onCancel,
  onSave,
}: {
  label: LabelDto;
  onCancel: () => void;
  onSave: (name: string, color: string) => void;
}) {
  const [name, setName] = useState(label.name);
  const [color, setColor] = useState(label.color);
  return (
    <li className="bg-raised flex flex-col gap-3 p-3">
      <input
        autoFocus
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && name.trim()) onSave(name.trim(), color);
          if (e.key === "Escape") onCancel();
        }}
        className="border-line focus:border-line-strong rounded-control border bg-transparent px-2 py-1.5 text-sm outline-none"
        aria-label="Label name"
      />
      <ColorPicker value={color} onChange={setColor} />
      <div className="flex justify-end gap-2">
        <Button onClick={onCancel} label="Cancel" variant="secondary" elevation="low" size="sm" />
        <Button
          isDisabled={!name.trim()}
          onClick={() => onSave(name.trim(), color)}
          label="Save"
          variant="primary"
          size="sm"
        />
      </div>
    </li>
  );
}

interface StateWire {
  state_id: string;
  name: string;
  category: string;
  color: string;
}
interface WorkflowWire {
  project_id: string;
  revision: {
    revision_id: string;
    body: { name: string; states: StateWire[]; transitions: unknown[] };
  } | null;
  conflict_heads: string[];
}

/**
 * Workflow — rename and recolor the status columns of a project.
 *
 * The engine already speaks this (`WorkflowSet` is a whole-body CAS replace at the
 * current heads); the viewer only ever read it. This edits the *display* of each
 * state — name and colour — and re-submits the same `state_id`s and transitions, so
 * referential integrity is preserved for free. Adding/removing states (which would
 * rewrite transitions) is deliberately out of scope here.
 */
function WorkflowPanel({
  spaceId,
  projects,
  readOnly,
  revision,
  onError,
}: {
  spaceId: string;
  projects: ProjectDto[];
  readOnly: boolean;
  revision: number;
  onError: (message: string) => void;
}) {
  const [projectKey, setProjectKey] = useState<string | null>(projects[0]?.key ?? null);
  const [wf, setWf] = useState<WorkflowWire | null>(null);
  const [draft, setDraft] = useState<StateWire[]>([]);
  const [saving, setSaving] = useState(false);
  const [editingColor, setEditingColor] = useState<string | null>(null);

  useEffect(() => {
    if (!projectKey) return;
    let alive = true;
    setWf(null);
    void rpc(spaceId, { cmd: "workflow_show", project: projectKey })
      .then((r) => {
        if (!alive) return;
        if (r.kind === "text") {
          const parsed = JSON.parse(r.text) as WorkflowWire;
          setWf(parsed);
          setDraft(parsed.revision?.body.states.map((s) => ({ ...s })) ?? []);
        }
      })
      .catch((e) => {
        if (alive) onError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      alive = false;
    };
  }, [spaceId, projectKey, revision, onError]);

  const dirty = useMemo(() => {
    const original = wf?.revision?.body.states ?? [];
    return draft.some((s, i) => s.name !== original[i]?.name || s.color !== original[i]?.color);
  }, [draft, wf]);

  const save = async () => {
    if (!wf?.revision || !projectKey) return;
    setSaving(true);
    try {
      const body = { ...wf.revision.body, states: draft };
      const heads = [wf.revision.revision_id, ...wf.conflict_heads];
      await rpc(spaceId, {
        cmd: "workflow_set",
        project: projectKey,
        expect_heads: heads,
        body_json: JSON.stringify(body),
      });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const patch = (id: string, change: Partial<StateWire>) =>
    setDraft((d) => d.map((s) => (s.state_id === id ? { ...s, ...change } : s)));
  const selectedProject = projects.find((p) => p.key === projectKey);

  return (
    <>
      <SettingsPageHeader
        title="Workflow"
        description="Configure the states issues move through in each project."
      />
      <SettingsSection
        title="Workflow states"
        hint="Rename and recolor the statuses issues move through. Applies to the selected project."
      >
      <div className="mb-4 flex items-center gap-2">
        <span className="text-mute text-sm">Project</span>
        <Combobox
          label="Project"
          value={
            projectKey
              ? {
                  id: projectKey,
                  label: selectedProject?.name ?? projectKey,
                  ...(selectedProject
                    ? { icon: <ProjectIcon color={catalogColor(selectedProject.color)} /> }
                    : {}),
                }
              : null
          }
          placeholder="Select…"
          options={projects.map((p) => ({
            id: p.key,
            label: p.name,
            icon: <ProjectIcon color={catalogColor(p.color)} />,
            hint: p.key,
          }))}
          onPick={setProjectKey}
        />
      </div>

      {!wf && projectKey && <p className="text-mute text-sm">Loading…</p>}
      {wf && !wf.revision && (
        <p className="text-warn text-sm">This project has unresolved concurrent workflow revisions.</p>
      )}
      {wf?.revision && (
        <>
          <ul className="flex flex-col gap-1">
            {draft.map((s) => (
              <li
                key={s.state_id}
                className="border-line -mx-1 flex items-center gap-2 rounded-control px-1 py-1"
              >
                <div className="relative">
                  <button
                    disabled={readOnly}
                    onClick={() => setEditingColor(editingColor === s.state_id ? null : s.state_id)}
                    aria-label={`Colour of ${s.name}`}
                    className="hover:ring-line-strong rounded-mark p-0.5 hover:ring-1 disabled:opacity-50"
                  >
                    <StatusIcon
                      category={s.category as "backlog"}
                      color={catalogColor(s.color)}
                    />
                  </button>
                  {editingColor === s.state_id && (
                    <div className="border-line-strong bg-raised shadow-overlay absolute left-0 top-7 z-10 rounded-surface border p-2">
                      <ColorPicker
                        value={s.color}
                        onChange={(color) => {
                          patch(s.state_id, { color });
                          setEditingColor(null);
                        }}
                      />
                    </div>
                  )}
                </div>
                <input
                  value={s.name}
                  disabled={readOnly}
                  onChange={(e) => patch(s.state_id, { name: e.target.value })}
                  className="focus:border-line-strong min-w-0 flex-1 rounded-control border border-transparent bg-transparent px-1.5 py-1 text-sm outline-none disabled:opacity-50"
                  aria-label={`Name of ${s.name}`}
                />
                <span className="text-mute text-2xs capitalize">
                  {s.category.replaceAll("_", " ")}
                </span>
              </li>
            ))}
          </ul>
          {!readOnly && (
            <div className="mt-4 flex items-center justify-between">
              <p className="text-mute text-xs">
                Adding or removing states (which rewrites transitions) is CLI-only for now.
              </p>
              <div className="flex gap-2">
                <Button
                  isDisabled={!dirty}
                  onClick={() => setDraft(wf.revision!.body.states.map((s) => ({ ...s })))}
                  label="Reset"
                  variant="secondary"
                  elevation="low"
                  size="sm"
                />
                <Button
                  isDisabled={!dirty}
                  isLoading={saving}
                  onClick={() => void save()}
                  label="Save workflow"
                  variant="primary"
                  size="sm"
                />
              </div>
            </div>
          )}
        </>
      )}
      </SettingsSection>
    </>
  );
}

// ---- Roles & access ---------------------------------------------------------

interface RoleWire {
  role_id: string;
  built_in: boolean;
  revision: {
    revision_id: string;
    body: { name: string; description: string; scope_kind: string; capabilities: string[] };
  } | null;
  conflict_heads: string[];
}

/** The name a role grant carries, falling back to its id. */
function roleName(r: RoleWire): string {
  return r.revision?.body.name ?? r.role_id;
}

/**
 * Roles & access — the plan-04 authority layer, made browser-operable.
 *
 * The engine has always spoken this (`role_list` / `access_list` / `access_grant`
 * / `access_revoke`); until now it was terminal-only, so a browser-first admin
 * could see a *membership* role on the Members page but never grant a scoped
 * capability. This surfaces the role catalogue read-only (authoring a role is a
 * CAS ceremony with its own conflict flow) and makes the *assignment* verbs — grant a
 * role's capabilities to an actor, revoke one — first-class here.
 *
 * A grant expands a role into one assignment per capability, each with its own
 * `grant_id`; revoke is per capability, so the list is grouped by actor and every
 * held capability carries its own revoke handle.
 */
function AccessPanel({
  spaceId,
  projects,
  readOnly,
  revision,
  onError,
}: {
  spaceId: string;
  projects: ProjectDto[];
  readOnly: boolean;
  revision: number;
  onError: (message: string) => void;
}) {
  const [roles, setRoles] = useState<RoleWire[] | null>(null);
  const [members, setMembers] = useState<MemberDto[] | null>(null);
  const [rows, setRows] = useState<AssignmentDto[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [grantActor, setGrantActor] = useState<string | null>(null);
  const [grantRole, setGrantRole] = useState<string | null>(null);
  const [grantProject, setGrantProject] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [r, m, a] = await Promise.all([
        rpc(spaceId, { cmd: "role_list" }),
        spaceRpc(spaceId, { cmd: "members" }),
        rpc(spaceId, { cmd: "access_list" }),
      ]);
      if (r.kind === "text") setRoles(JSON.parse(r.text) as RoleWire[]);
      if (m.kind === "members") setMembers(m.members);
      if (a.kind === "assignments") setRows(a.rows);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  }, [spaceId, onError]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  const nameOf = useCallback(
    (actor: string) => memberName(actor, members?.find((m) => m.key === actor)),
    [members],
  );
  const projectLabel = useCallback(
    (id: string) => projects.find((p) => p.id === id || p.key === id)?.key ?? id,
    [projects],
  );
  const grantProjectDto = projects.find((p) => p.id === grantProject || p.key === grantProject);

  /** Assignments folded by actor, so each person reads as one block. */
  const byActor = useMemo(() => {
    const groups = new Map<string, AssignmentDto[]>();
    for (const row of rows ?? []) {
      const list = groups.get(row.actor) ?? [];
      list.push(row);
      groups.set(row.actor, list);
    }
    return [...groups.entries()].sort((a, b) => nameOf(a[0]).localeCompare(nameOf(b[0])));
  }, [rows, nameOf]);

  const grant = async () => {
    if (!grantActor || !grantRole) return;
    setBusy(true);
    try {
      await rpc(spaceId, {
        cmd: "access_grant",
        actor: grantActor,
        role: grantRole,
        project: grantProject,
      });
      setGrantActor(null);
      setGrantRole(null);
      setGrantProject(null);
      await load();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const revoke = (row: AssignmentDto) =>
    void ask
      .confirm({
        title: `Revoke ${row.capability}?`,
        body: `Removes this one capability from ${nameOf(row.actor)}. Their base membership role is unaffected.`,
        confirmText: "Revoke",
        danger: true,
      })
      .then(async (ok) => {
        if (!ok) return;
        setBusy(true);
        try {
          await rpc(spaceId, { cmd: "access_revoke", grant_id: row.grant_id });
          await load();
        } catch (e) {
          onError(e instanceof Error ? e.message : String(e));
        } finally {
          setBusy(false);
        }
      });

  const grantableRoles = (roles ?? []).filter((r) => r.revision && !r.conflict_heads.length);

  return (
    <>
      <SettingsPageHeader
        title="Roles & access"
        description="Review signed roles and grant additional capabilities at space or project scope."
      />
      <SettingsSection
        title="Roles"
        hint="Named capability sets from the signed policy. Authoring a role is a CAS ceremony — create and edit them with the issues_role_create and issues_role_edit tools."
      >
        {!roles && <p className="text-mute text-sm">Loading…</p>}
        <ul className="flex flex-col gap-2">
          {roles?.map((role) => (
            <li key={role.role_id} className="border-line rounded-surface border p-3">
              <div className="flex items-center gap-2">
                <span className="font-medium">{roleName(role)}</span>
                {role.built_in && (
                  <span className="text-accent flex items-center gap-1 text-2xs" title="Immutable">
                    <ShieldCheck className="size-icon-xs" />
                    built-in
                  </span>
                )}
                <span className="text-mute text-2xs capitalize">
                  {role.revision?.body.scope_kind ?? ""}
                </span>
              </div>
              {role.revision?.body.description && (
                <p className="text-dim mt-1 text-sm">{role.revision.body.description}</p>
              )}
              <ul className="mt-2 flex flex-wrap gap-1">
                {(role.revision?.body.capabilities ?? []).map((c) => (
                  <li
                    key={c}
                    className="border-line-strong text-dim rounded-full border px-2 py-px font-mono text-2xs"
                  >
                    {c}
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      </SettingsSection>

      <SettingsSection
        title="Access grants"
        hint="Capabilities granted to an actor beyond their base membership role, at the Space or a single project."
      >
        {!readOnly && (
          <div className="border-line mb-4 flex flex-wrap items-end gap-2 rounded-surface border p-3">
            <Combobox
              label="Member"
              value={
                grantActor
                  ? { id: grantActor, label: nameOf(grantActor) }
                  : null
              }
              placeholder="Member…"
              options={(members ?? []).map((m) => ({
                id: m.key,
                label: memberName(m.key, m),
                hint: m.role,
              }))}
              onPick={setGrantActor}
            />
            <Combobox
              label="Role"
              value={
                grantRole
                  ? {
                      id: grantRole,
                      label: roleName(
                        grantableRoles.find((r) => r.role_id === grantRole) ?? {
                          role_id: grantRole,
                          built_in: false,
                          revision: null,
                          conflict_heads: [],
                        },
                      ),
                    }
                  : null
              }
              placeholder="Role…"
              options={grantableRoles.map((r) => ({
                id: r.role_id,
                label: roleName(r),
                hint: r.revision?.body.scope_kind ?? "",
              }))}
              onPick={setGrantRole}
            />
            <Combobox
              label="Scope"
              value={{
                id: grantProject ?? "",
                label: grantProject ? projectLabel(grantProject) : "Whole space",
                ...(grantProjectDto
                  ? { icon: <ProjectIcon color={catalogColor(grantProjectDto.color)} /> }
                  : {}),
              }}
              placeholder="Whole space"
              options={[
                { id: "", label: "Whole space" },
                ...projects.map((p) => ({
                  id: p.key,
                  label: p.name,
                  icon: <ProjectIcon color={catalogColor(p.color)} />,
                  hint: p.key,
                })),
              ]}
              onPick={(id) => setGrantProject(id === "" ? null : id)}
            />
            <Button
              isDisabled={!grantActor || !grantRole || busy}
              isLoading={busy}
              onClick={() => void grant()}
              icon={<ShieldPlus className="size-icon-sm" />}
              label="Grant"
              variant="primary"
              size="md"
            />
          </div>
        )}

        {!rows && <p className="text-mute text-sm">Loading…</p>}
        {rows && byActor.length === 0 && (
          <p className="text-mute text-sm">
            No scoped grants. Members act with their base role until granted extra capabilities here.
          </p>
        )}
        <ul className="flex flex-col gap-3">
          {byActor.map(([actor, items]) => (
            <li key={actor} className="border-line rounded-surface border p-3">
              <div className="mb-2 font-medium">{nameOf(actor)}</div>
              <ul className="flex flex-col gap-1">
                {items.map((row) => (
                  <li key={row.grant_id} className="flex items-center gap-2 text-sm">
                    <code className="font-mono text-xs">{row.capability}</code>
                    <span className="text-mute text-2xs">
                      {row.resource.length === 0 ? "space" : projectLabel(row.resource[0] ?? "")}
                    </span>
                    {!readOnly && (
                      <IconButton
                        label={`Revoke ${row.capability}`}
                        isDisabled={busy}
                        className="ml-auto"
                        onClick={() => revoke(row)}
                        variant="danger"
                        size="sm"
                        tooltip={`Revoke ${row.capability}`}
                        icon={<X className="size-icon-sm" />}
                      />
                    )}
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      </SettingsSection>
    </>
  );
}
