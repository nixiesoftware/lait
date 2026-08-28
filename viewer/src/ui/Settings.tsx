import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowLeft,
  Bell,
  Hash,
  KeyRound,
  Laptop,
  Palette,
  RotateCcw,
  Search,
  ShieldCheck,
  ShieldPlus,
  SlidersHorizontal,
  Tag,
  Trash2,
  UserRound,
  Users,
  UsersRound,
} from "lucide-react";

import { rpc, spaceRpc } from "../api";
import type {
  AssignmentDto,
  LabelDto,
  MemberDto,
  ProjectDto,
  RoleProjection,
  TeamDto,
} from "../types";
import { Avatar, memberName } from "./Avatar";
import { catalogColor } from "./colors";
import * as ask from "./dialogs";
import { ProjectIcon } from "./icons";
import { Members } from "./Members";
import { TeamsPanel } from "./TeamsPanel";
import { Combobox } from "./Picker";
import { Button, TextArea, TextInput } from "@astryxdesign/core";
import { Badge, cn, navigationItem } from "./primitives";
import {
  SettingsField,
  SettingsPageHeader,
  SettingsSection,
  SettingsSurface,
} from "./settingsLayout";
import { LabelsPanel } from "./settings/Labels";
import { SETTINGS_GROUPS, isSettingsTab, searchSettings, type SettingsTab } from "./settings/pages";
import {
  PreferencesPanel,
  type DensityPreference,
  type ThemePreference,
} from "./settings/Preferences";
import { NotificationsPanel } from "./settings/Notifications";
import { ProfilePanel } from "./settings/Profile";
import { WorkflowPanel } from "./settings/Workflow";

const TAB_ICON: Record<SettingsTab, React.ReactNode> = {
  preferences: <SlidersHorizontal className="size-icon-sm" />,
  profile: <UserRound className="size-icon-sm" />,
  notifications: <Bell className="size-icon-sm" />,
  general: <Hash className="size-icon-sm" />,
  members: <Users className="size-icon-sm" />,
  teams: <UsersRound className="size-icon-sm" />,
  access: <ShieldCheck className="size-icon-sm" />,
  devices: <Laptop className="size-icon-sm" />,
  labels: <Tag className="size-icon-sm" />,
  workflow: <Palette className="size-icon-sm" />,
};

/** The pages that draw a table and want the wider column. */
const WIDE: ReadonlySet<SettingsTab> = new Set(["teams", "members", "labels"]);

/**
 * The settings surface — the place a space is administered like an application.
 *
 * It is a real destination (a `settings` view/route), not a modal, because it hosts
 * several editors that each own state; a popover would throw that away on the first
 * outside click. The left rail is grouped the way Linear groups its own —
 * **Personal** (what this person wants, on this device), **Issues** (the vocabulary
 * every project shares), **Administration** (the space itself) — with a search box
 * above it that answers to a page's rows and not only its name, and the teams
 * listed beneath so a team is one click from anywhere in settings.
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
  theme,
  onThemeChange,
  density,
  onDensityChange,
  onOpenShortcuts,
  onForget,
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
  theme: ThemePreference;
  onThemeChange: (theme: ThemePreference) => void;
  density: DensityPreference;
  onDensityChange: (density: DensityPreference) => void;
  onOpenShortcuts: () => void;
  /** Deregister this space on this device. Absent when the host cannot. */
  onForget?: () => void;
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
  const tab: SettingsTab = isSettingsTab(routeTab)
    ? routeTab
    : legacyMembersPath
      ? "members"
      : "general";
  const setTab = (next: SettingsTab) => onTabChange(next === "general" ? null : next);
  // Reliable driver hook: `lait:nav { tab }` selects a sub-page without a click.
  useEffect(() => {
    const onNav = (event: Event) => {
      const t = (event as CustomEvent).detail?.tab;
      if (isSettingsTab(t)) onTabChange(t === "general" ? null : t);
    };
    window.addEventListener("lait:nav", onNav as EventListener);
    return () => window.removeEventListener("lait:nav", onNav as EventListener);
  }, [onTabChange]);

  /** A team chosen from the rail: opens Teams on that team. Held here rather
   *  than in the route because the team page is a step inside Teams, and the
   *  rail is the one other place that can start that step. */
  const [teamFocus, setTeamFocus] = useState<string | null>(null);
  const openTeam = (id: string) => {
    setTeamFocus(id);
    setTab("teams");
  };

  const isAdmin = members.some((m) => m.me && m.role === "admin");

  return (
    <div className="bg-sunken flex h-full min-h-0">
      <SettingsRail tab={tab} teams={teams} onPick={setTab} onPickTeam={openTeam} onExit={onExit} />
      <section className="border-line bg-bg m-1 min-h-0 flex-1 overflow-hidden rounded-surface border">
        <div className="h-full overflow-y-auto px-6 py-8">
          <div className={cn("mx-auto w-full", WIDE.has(tab) ? "max-w-4xl" : "max-w-2xl")}>
            {tab === "preferences" && (
              <PreferencesPanel
                theme={theme}
                onThemeChange={onThemeChange}
                density={density}
                onDensityChange={onDensityChange}
                onOpenShortcuts={onOpenShortcuts}
              />
            )}
            {tab === "profile" && (
              <ProfilePanel
                spaceId={spaceId}
                spaceName={spaceName}
                revision={revision}
                onError={onError}
              />
            )}
            {tab === "notifications" && <NotificationsPanel spaceId={spaceId} />}
            {tab === "general" && (
              <GeneralPanel
                spaceId={spaceId}
                spaceName={spaceName}
                spaceDescription={spaceDescription}
                readOnly={readOnly}
                isAdmin={isAdmin}
                memberCount={members.length}
                projectCount={projects.length}
                onError={onError}
                {...(onForget ? { onForget } : {})}
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
                focus={teamFocus}
                onFocusConsumed={() => setTeamFocus(null)}
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
              <LabelsPanel
                spaceId={spaceId}
                labels={labels}
                readOnly={readOnly}
                onError={onError}
              />
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

/**
 * The rail: back, search, then the pages under their group headings, then the
 * teams. Typing filters the pages to the ones that answer — by name first, and
 * by a row on the page second, with the row's word shown so the person knows
 * why Preferences turned up for "theme". Enter opens the first answer.
 */
function SettingsRail({
  tab,
  teams,
  onPick,
  onPickTeam,
  onExit,
}: {
  tab: SettingsTab;
  teams: TeamDto[];
  onPick: (tab: SettingsTab) => void;
  onPickTeam: (id: string) => void;
  onExit: () => void;
}) {
  const [query, setQuery] = useState("");
  const searching = query.trim() !== "";
  const matches = useMemo(() => searchSettings(query), [query]);
  const searchRef = useRef<HTMLInputElement>(null);

  const pick = (next: SettingsTab) => {
    setQuery("");
    onPick(next);
  };

  return (
    <nav className="flex w-56 shrink-0 flex-col gap-0.5 overflow-y-auto p-2" aria-label="Settings">
      <button onClick={onExit} className={cn(navigationItem({ size: "lg" }), "text-mute mb-2")}>
        <ArrowLeft className="size-icon-sm" />
        Back to app
      </button>

      <div className="mb-3 px-0.5">
        <TextInput
          ref={searchRef}
          label="Search settings"
          isLabelHidden
          value={query}
          onChange={setQuery}
          placeholder="Search…"
          startIcon={<Search className="size-icon-sm" />}
          size="sm"
          width="100%"
          onKeyDown={(e) => {
            if (e.key === "Enter" && matches[0]) pick(matches[0].page.tab);
            if (e.key === "Escape") setQuery("");
          }}
        />
      </div>

      {searching ? (
        matches.length === 0 ? (
          <p className="text-mute px-2 py-2 text-sm">Nothing matches “{query.trim()}”.</p>
        ) : (
          <ul className="flex flex-col gap-0.5" role="list">
            {matches.map(({ page, via }, i) => (
              <li key={page.tab}>
                <button
                  onClick={() => pick(page.tab)}
                  className={cn(navigationItem({ selected: i === 0, size: "lg" }))}
                >
                  {TAB_ICON[page.tab]}
                  <span className="min-w-0 flex-1 truncate">{page.label}</span>
                  {via && <span className="text-mute truncate text-2xs">{via}</span>}
                </button>
              </li>
            ))}
          </ul>
        )
      ) : (
        <>
          {SETTINGS_GROUPS.map((group) => (
            <div key={group} className="mb-3">
              <h2 className="text-mute px-2 pb-1 pt-1 text-2xs font-semibold tracking-wider uppercase">
                {group}
              </h2>
              <ul className="flex flex-col gap-0.5" role="list">
                {matches
                  .filter(({ page }) => page.group === group)
                  .map(({ page }) => (
                    <li key={page.tab}>
                      <button
                        onClick={() => pick(page.tab)}
                        aria-current={tab === page.tab ? "page" : undefined}
                        className={cn(
                          navigationItem({
                            selected: tab === page.tab,
                            size: "lg",
                          }),
                        )}
                      >
                        {TAB_ICON[page.tab]}
                        {page.label}
                      </button>
                    </li>
                  ))}
              </ul>
            </div>
          ))}
          {teams.length > 0 && (
            <div className="mb-3">
              <h2 className="text-mute px-2 pb-1 pt-1 text-2xs font-semibold tracking-wider uppercase">
                Your teams
              </h2>
              <ul className="flex flex-col gap-0.5" role="list">
                {teams.map((team) => (
                  <li key={team.id}>
                    <button
                      onClick={() => onPickTeam(team.id)}
                      className={cn(navigationItem({ size: "lg" }))}
                    >
                      <span className="text-mute w-icon-sm shrink-0 text-center font-mono text-2xs">
                        {team.key.slice(0, 2)}
                      </span>
                      <span className="min-w-0 flex-1 truncate">{team.name}</span>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      )}
    </nav>
  );
}

/** General — the space's mutable display label, description, immutable
 *  identity, and the two things a person does to a space as a whole. */
function GeneralPanel({
  spaceId,
  spaceName,
  spaceDescription,
  readOnly,
  isAdmin,
  memberCount,
  projectCount,
  onError,
  onForget,
}: {
  spaceId: string;
  spaceName: string;
  spaceDescription: string;
  readOnly: boolean;
  isAdmin: boolean;
  memberCount: number;
  projectCount: number;
  onError: (message: string) => void;
  onForget?: () => void;
}) {
  const [name, setName] = useState(spaceName);
  const [description, setDescription] = useState(spaceDescription);
  const [saving, setSaving] = useState(false);
  const [savingDesc, setSavingDesc] = useState(false);
  const [rotating, setRotating] = useState(false);
  const [note, setNote] = useState("");
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
      await rpc(spaceId, {
        cmd: "space_describe",
        description: description.trim(),
      });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSavingDesc(false);
    }
  };

  /**
   * Rotating the key is what removing a member does implicitly; offered on its
   * own for the case where a device was lost rather than a member removed. It
   * seals everything written from now on under a key only current members
   * hold — and nothing written before, which the confirmation says.
   */
  const rotate = async () => {
    const ok = await ask.confirm({
      title: "Rotate the space key?",
      body: "Everything written from now on is sealed under a new key that only current members receive. Copies anyone already holds stay readable. Members on other devices pick the new key up on their next sync.",
      confirmText: "Rotate key",
      danger: true,
    });
    if (!ok) return;
    setRotating(true);
    setNote("");
    try {
      const reply = await spaceRpc(spaceId, { cmd: "key_rotate" });
      setNote((reply.kind === "ok" && reply.message) || "The space key was rotated.");
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setRotating(false);
    }
  };

  return (
    <>
      <SettingsPageHeader
        title="General"
        description="The shared name and description of this space, and the identity it cannot change."
      />
      <SettingsSection title="Space">
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
                onKeyDown={(e) => {
                  if (e.key === "Enter" && dirty && !readOnly) void save();
                }}
              />
              <Button
                isDisabled={!dirty || readOnly}
                isLoading={saving}
                onClick={() => void save()}
                label="Update"
                variant="primary"
                size="sm"
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
                    size="sm"
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
          <SettingsField label="In this space" hint="What the space holds right now">
            <p className="text-dim text-right text-sm tabular-nums">
              {memberCount} {memberCount === 1 ? "member" : "members"} · {projectCount}{" "}
              {projectCount === 1 ? "project" : "projects"}
            </p>
          </SettingsField>
        </SettingsSurface>
      </SettingsSection>

      {(onForget || (isAdmin && !readOnly)) && (
        <SettingsSection
          title="Danger zone"
          hint="Each of these asks before it acts, and says what it does not undo."
        >
          <SettingsSurface className="border-danger/40">
            {isAdmin && !readOnly && (
              <SettingsField
                label="Rotate the space key"
                hint="Seal everything from now on under a key only current members hold. Use it when a device is lost."
              >
                <div className="flex flex-col items-end gap-1">
                  <Button
                    label="Rotate key…"
                    variant="danger"
                    size="sm"
                    isLoading={rotating}
                    icon={<RotateCcw className="size-icon-sm" />}
                    onClick={() => void rotate()}
                  />
                  {note && <p className="text-dim text-right text-xs">{note}</p>}
                </div>
              </SettingsField>
            )}
            {onForget && (
              <SettingsField
                label="Forget this space on this device"
                hint="Removes it from this device's list. The encrypted store stays on disk, and no other device is affected."
              >
                <div className="flex justify-end">
                  <Button
                    label="Forget…"
                    variant="danger"
                    size="sm"
                    icon={<Trash2 className="size-icon-sm" />}
                    onClick={onForget}
                  />
                </div>
              </SettingsField>
            )}
          </SettingsSurface>
        </SettingsSection>
      )}
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
        description="The machines that sign as you, and the custody material that protects recovery."
      />
      <SettingsSection
        title="Your devices"
        hint="Every machine that signs as you in this space. Each holds its own key; none of them is a copy of another."
      >
        <SettingsSurface>
          {devices.length === 0 ? (
            <div className="flex items-center gap-3 px-4 py-3 text-sm">
              <Laptop className="text-mute size-icon-sm shrink-0" />
              <span className="min-w-0 flex-1">This device</span>
              <span className="text-mute text-2xs">current</span>
            </div>
          ) : (
            devices.map((line) => {
              const current = line.includes("(this device)");
              const id = line.trim().split(/\s+/)[0] ?? "";
              return (
                <div key={line} className="flex items-center gap-3 px-4 py-3 text-sm">
                  <Laptop className="text-mute size-icon-sm shrink-0" />
                  <code className="min-w-0 flex-1 truncate font-mono text-xs" title={line}>
                    {id}
                  </code>
                  {current ? (
                    <span className="text-mute text-2xs">current</span>
                  ) : (
                    !readOnly && (
                      <Button
                        label="Revoke"
                        variant="ghost"
                        size="sm"
                        isDisabled={busy !== ""}
                        onClick={() =>
                          void act("revoke", async () => {
                            if (
                              !(await ask.confirm({
                                title: `Revoke device ${id}?`,
                                body: "It stops signing as you. Content it already holds stays readable until an admin rotates the space key.",
                                confirmText: "Revoke",
                                danger: true,
                              }))
                            )
                              return null;
                            const reply = await spaceRpc(spaceId, {
                              cmd: "device_revoke",
                              device: id,
                            });
                            return reply.kind === "ok" ? reply.message : null;
                          })
                        }
                      />
                    )
                  )}
                </div>
              );
            })
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
                      const reply = await spaceRpc(spaceId, {
                        cmd: "device_invite",
                      });
                      if (reply.kind === "text") setToken(reply.text.trim());
                      return null;
                    })
                  }
                  icon={<KeyRound className="size-icon-sm" />}
                  label="Mint token"
                  variant="secondary"
                  size="sm"
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
                  variant="secondary"
                  size="sm"
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
                  variant="secondary"
                  size="sm"
                />
                <Button
                  size="sm"
                  variant="secondary"
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
              {note && <p className="text-dim mt-2 text-right text-sm">{note}</p>}
            </SettingsField>
          </SettingsSurface>
        </SettingsSection>
      )}
    </>
  );
}

// ---- Roles & access ---------------------------------------------------------

interface RoleWire {
  role_id: string;
  built_in: boolean;
  revision: {
    revision_id: string;
    body: {
      name: string;
      description: string;
      scope_kind: string;
      capabilities: string[];
    };
  } | null;
  conflict_heads: string[];
}

/** One capability a member holds at one scope, however many grant ids say so. */
interface Grant {
  capability: string;
  /** Project id or key; empty for the whole space. */
  scope: string;
  world: string;
  grantIds: string[];
}

function roleFromProjection({ summary, revision }: RoleProjection): RoleWire {
  return { ...summary, revision: revision ?? null };
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
  const [roleCursor, setRoleCursor] = useState<string | null>(null);
  const [members, setMembers] = useState<MemberDto[] | null>(null);
  const [rows, setRows] = useState<AssignmentDto[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [grantActor, setGrantActor] = useState<string | null>(null);
  const [grantRole, setGrantRole] = useState<string | null>(null);
  const [grantProject, setGrantProject] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [r, m, a] = await Promise.all([
        rpc(spaceId, { cmd: "role_list", page: { limit: 100, cursor: null } }),
        spaceRpc(spaceId, { cmd: "members" }),
        rpc(spaceId, { cmd: "access_list" }),
      ]);
      if (r.kind === "roles") {
        setRoles(r.page.items.map(roleFromProjection));
        setRoleCursor(r.page.next_cursor ?? null);
      }
      if (m.kind === "members") setMembers(m.members);
      if (a.kind === "assignments") setRows(a.rows);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  }, [spaceId, onError]);

  const loadMoreRoles = async () => {
    if (!roleCursor || busy) return;
    setBusy(true);
    try {
      const response = await rpc(spaceId, {
        cmd: "role_list",
        page: { limit: 100, cursor: roleCursor },
      });
      if (response.kind === "roles") {
        setRoles((current) => [
          ...(current ?? []),
          ...response.page.items
            .map(roleFromProjection)
            .filter(
              (candidate) => !(current ?? []).some((role) => role.role_id === candidate.role_id),
            ),
        ]);
        setRoleCursor(response.page.next_cursor ?? null);
      }
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void load();
  }, [load, revision]);

  const nameOf = useCallback(
    (actor: string) =>
      memberName(
        actor,
        members?.find((m) => m.key === actor),
      ),
    [members],
  );
  const projectLabel = useCallback(
    (id: string) => projects.find((p) => p.id === id || p.key === id)?.key ?? id,
    [projects],
  );
  const grantProjectDto = projects.find((p) => p.id === grantProject || p.key === grantProject);

  /**
   * Assignments folded by actor, so each person reads as one block — and
   * within a block, folded by (capability, scope), because seeding a Space
   * can grant the same capability twice under two grant ids, and two rows
   * saying `space.admin · space` teach nothing the first did not. A folded row
   * keeps every grant id it stands for, so revoking it revokes the capability
   * rather than one copy of it.
   */
  const byActor = useMemo(() => {
    const groups = new Map<string, Map<string, Grant>>();
    for (const row of rows ?? []) {
      const scope = row.resource[0] ?? "";
      const key = `${row.capability}\u0000${scope}`;
      const forActor = groups.get(row.actor) ?? new Map<string, Grant>();
      const existing = forActor.get(key);
      if (existing) existing.grantIds.push(row.grant_id);
      else
        forActor.set(key, {
          capability: row.capability,
          scope,
          world: row.world,
          grantIds: [row.grant_id],
        });
      groups.set(row.actor, forActor);
    }
    return [...groups.entries()]
      .map(([actor, grants]) => [actor, [...grants.values()]] as const)
      .sort((a, b) => nameOf(a[0]).localeCompare(nameOf(b[0])));
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

  const revoke = (actor: string, grant: Grant) =>
    void ask
      .confirm({
        title: `Revoke ${grant.capability}?`,
        body: `Removes this capability from ${nameOf(actor)} at ${
          grant.scope ? projectLabel(grant.scope) : "the whole space"
        }${grant.grantIds.length > 1 ? ` — all ${grant.grantIds.length} grants of it` : ""}. Their base membership role is unaffected.`,
        confirmText: "Revoke",
        danger: true,
      })
      .then(async (ok) => {
        if (!ok) return;
        setBusy(true);
        try {
          for (const grant_id of grant.grantIds) {
            await rpc(spaceId, { cmd: "access_revoke", grant_id });
          }
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
        {roles && roles.length === 0 && (
          <SettingsSurface>
            <p className="text-mute px-4 py-3 text-sm">None beyond the built-in memberships.</p>
          </SettingsSurface>
        )}
        {roles && roles.length > 0 && (
          <SettingsSurface>
            {roles.map((role) => (
              <div key={role.role_id} className="px-4 py-3">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-semibold">{roleName(role)}</span>
                  {role.built_in && (
                    <span
                      className="text-accent flex items-center gap-1 text-2xs"
                      title="Immutable"
                    >
                      <ShieldCheck className="size-icon-xs" />
                      built-in
                    </span>
                  )}
                  <span className="text-mute text-2xs capitalize">
                    {role.revision?.body.scope_kind ?? ""}
                  </span>
                </div>
                {role.revision?.body.description && (
                  <p className="text-dim mt-0.5 text-xs">{role.revision.body.description}</p>
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
              </div>
            ))}
          </SettingsSurface>
        )}
        {roleCursor && (
          <Button
            type="button"
            variant="secondary"
            size="sm"
            isDisabled={busy}
            label={busy ? "Loading…" : "Load more roles"}
            onClick={() => void loadMoreRoles()}
            className="mt-3"
          />
        )}
      </SettingsSection>

      <SettingsSection
        title="Access grants"
        hint="Capabilities granted to an actor beyond their base membership role, at the Space or a single project."
      >
        {!readOnly && (
          <div className="border-line bg-raised mb-4 flex flex-wrap items-end gap-2 rounded-surface border p-3">
            <Combobox
              label="Member"
              value={grantActor ? { id: grantActor, label: nameOf(grantActor) } : null}
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
                  ? {
                      icon: <ProjectIcon color={catalogColor(grantProjectDto.color)} />,
                    }
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
              size="sm"
            />
          </div>
        )}

        {!rows && <p className="text-mute text-sm">Loading…</p>}
        {rows && byActor.length === 0 && (
          <SettingsSurface>
            <p className="text-mute px-4 py-3 text-sm">
              No scoped grants. Members act with their base role until granted more here.
            </p>
          </SettingsSurface>
        )}
        {byActor.length > 0 && (
          <div className="border-line overflow-hidden rounded-surface border">
            <div className="text-mute border-line grid grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)_7rem_5rem] items-center gap-4 border-b px-3 py-2 text-2xs">
              <span>Capability</span>
              <span>Scope</span>
              <span>World</span>
              <span aria-hidden />
            </div>
            {byActor.map(([actor, grants]) => {
              const member = members?.find((m) => m.key === actor);
              return (
                <div key={actor}>
                  <div className="bg-sunken flex items-center gap-2 px-3 py-1.5">
                    <Avatar
                      deviceKey={actor}
                      {...(member ? { alias: member.alias, me: member.me } : {})}
                      size="sm"
                    />
                    <span className="text-sm font-medium">{nameOf(actor)}</span>
                    {member && <span className="text-mute text-2xs capitalize">{member.role}</span>}
                    <span className="text-mute ml-auto text-2xs tabular-nums">
                      {grants.length} {grants.length === 1 ? "capability" : "capabilities"}
                    </span>
                  </div>
                  <ul className="divide-line divide-y">
                    {grants.map((grant) => {
                      const scoped = projects.find(
                        (p) => p.id === grant.scope || p.key === grant.scope,
                      );
                      return (
                        <li
                          key={`${grant.capability}:${grant.scope}`}
                          className="group/grant hover:bg-hover grid min-h-ctl-lg grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)_7rem_5rem] items-center gap-4 px-3 py-1"
                        >
                          <span className="flex min-w-0 items-center gap-2">
                            <code className="truncate font-mono text-xs">{grant.capability}</code>
                            {grant.grantIds.length > 1 && (
                              <Badge
                                tone="neutral"
                                title={`Granted ${grant.grantIds.length} times`}
                              >
                                ×{grant.grantIds.length}
                              </Badge>
                            )}
                          </span>
                          <span className="flex min-w-0 items-center gap-1.5 text-xs">
                            {scoped ? (
                              <>
                                <ProjectIcon color={catalogColor(scoped.color)} />
                                <span className="truncate">{scoped.name}</span>
                                <span className="text-mute font-mono text-2xs">{scoped.key}</span>
                              </>
                            ) : grant.scope ? (
                              <code className="text-dim truncate font-mono text-2xs">
                                {grant.scope}
                              </code>
                            ) : (
                              <span className="text-dim">Whole space</span>
                            )}
                          </span>
                          <code
                            className="text-mute truncate font-mono text-2xs"
                            title={grant.world}
                          >
                            {grant.world}
                          </code>
                          <span className="flex justify-end">
                            {!readOnly && (
                              <Button
                                label="Revoke"
                                variant="ghost"
                                size="sm"
                                isDisabled={busy}
                                className="opacity-0 group-hover/grant:opacity-100 focus-visible:opacity-100"
                                onClick={() => revoke(actor, grant)}
                              />
                            )}
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                </div>
              );
            })}
          </div>
        )}
      </SettingsSection>
    </>
  );
}
