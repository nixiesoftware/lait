import { useMemo, useState } from "react";
import { ArrowLeft, Search, Trash2, UserPlus } from "lucide-react";

import { rpc } from "../api";
import { projectsOf } from "../core/teams";
import type { MemberDto, ProjectDto, TeamDto } from "../types";
import { Avatar, memberName } from "./Avatar";
import * as ask from "./dialogs";
import { Combobox } from "./Picker";
import { Button, IconButton, TextInput } from "@astryxdesign/core";
import { Badge } from "./primitives";
import { SettingsPageHeader } from "./settingsLayout";
import { EmptyState } from "./AppState";

/**
 * Teams — the administration surface for the grouping the sidebar navigates by.
 *
 * Three states of one panel rather than three routes: a list, a team, and the
 * create form. They are three *steps*, not three destinations — you arrive at
 * the second and third from the first and leave them the same way — and the
 * settings shell already owns the address (`?tab=teams`), so giving each step
 * its own would mean a second addressing scheme inside a page that has one.
 *
 * The engine has had all of this since GOV-7 and nothing had ever called it.
 * `team_set` is one verb for create, edit and delete: omitting `team` mints,
 * naming it edits, and `remove` deletes. That is kept rather than fanned out
 * into three client commands, because a rename and a create differ only by
 * whether the id already exists.
 */
export function TeamsPanel({
  spaceId,
  teams,
  projects,
  members,
  readOnly,
  onError,
}: {
  spaceId: string;
  teams: TeamDto[];
  projects: ProjectDto[];
  members: MemberDto[];
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const [step, setStep] = useState<{ at: "list" } | { at: "team"; id: string } | { at: "new" }>({
    at: "list",
  });

  const send = async (fn: () => Promise<unknown>): Promise<boolean> => {
    try {
      await fn();
      return true;
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
      return false;
    }
  };

  if (step.at === "new") {
    return (
      <CreateTeam
        readOnly={readOnly}
        onBack={() => setStep({ at: "list" })}
        onCreate={async (name, key) => {
          if (await send(() => rpc(spaceId, { cmd: "team_set", name, key }))) {
            setStep({ at: "list" });
          }
        }}
      />
    );
  }

  if (step.at === "team") {
    const team = teams.find((candidate) => candidate.id === step.id);
    // Deleted from another device while it was open. Falling back to the list
    // beats an empty page insisting on a team that is gone.
    if (!team) return <TeamList teams={teams} projects={projects} readOnly={readOnly} onOpen={(id) => setStep({ at: "team", id })} onNew={() => setStep({ at: "new" })} onDelete={() => undefined} />;
    return (
      <TeamDetail
        spaceId={spaceId}
        team={team}
        projects={projects}
        members={members}
        readOnly={readOnly}
        onBack={() => setStep({ at: "list" })}
        onError={onError}
      />
    );
  }

  return (
    <TeamList
      teams={teams}
      projects={projects}
      readOnly={readOnly}
      onOpen={(id) => setStep({ at: "team", id })}
      onNew={() => setStep({ at: "new" })}
      onDelete={async (team) => {
        const owned = projectsOf(team, projects);
        const confirmed = await ask.confirm({
          title: `Delete ${team.name}?`,
          // The consequence, stated, because it is not obvious and it is not
          // destructive in the way "delete" usually implies: the projects
          // survive, they just stop being grouped.
          body:
            owned.length === 0
              ? "The team owns no projects."
              : `${owned.length} project${owned.length === 1 ? "" : "s"} will move to No team. Nothing in them is deleted.`,
          confirmText: "Delete team",
          danger: true,
        });
        if (confirmed) await send(() => rpc(spaceId, { cmd: "team_set", team: team.id, remove: true }));
      }}
    />
  );
}

/** The list: filter, create, and one row per team. */
function TeamList({
  teams,
  projects,
  readOnly,
  onOpen,
  onNew,
  onDelete,
}: {
  teams: TeamDto[];
  projects: ProjectDto[];
  readOnly: boolean;
  onOpen: (id: string) => void;
  onNew: () => void;
  onDelete: (team: TeamDto) => void;
}) {
  const [query, setQuery] = useState("");
  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return teams;
    return teams.filter(
      (team) =>
        team.name.toLowerCase().includes(needle) || team.key.toLowerCase().includes(needle),
    );
  }, [query, teams]);

  return (
    <section>
      <SettingsPageHeader
        title="Teams"
        description="Group projects and members into durable areas of ownership."
        actions={
          !readOnly && teams.length > 0 ? (
            <Button label="Create team" variant="primary" size="sm" onClick={onNew} />
          ) : undefined
        }
      />
      <div className="mb-4 max-w-md">
        <TextInput
          label="Filter teams"
          isLabelHidden
          value={query}
          onChange={setQuery}
          placeholder="Filter by name…"
          size="sm"
          width="100%"
        />
      </div>

      {teams.length === 0 ? (
        <EmptyState
          art="people"
          title="Teams"
          body="A durable area of ownership, holding the projects and members that belong together."
          action={!readOnly ? <Button label="Create team" variant="primary" size="sm" onClick={onNew} /> : undefined}
        />
      ) : (
        <div className="overflow-hidden">
          <div className="text-mute grid grid-cols-[minmax(0,1fr)_6rem_7rem_7rem] gap-3 px-3 py-2 pr-12 text-2xs">
            <span>Name</span>
            <span>Key</span>
            <span className="text-right">Projects</span>
            <span className="text-right">Members</span>
          </div>
          <div className="bg-sunken text-mute rounded-row px-3 py-2 text-2xs">
            Active&nbsp; {shown.length}
          </div>
        <ul className="mt-1 flex flex-col gap-0.5">
          {shown.length === 0 && <li className="text-mute px-3 py-3 text-sm">Nothing matches “{query}”.</li>}
          {shown.map((team) => {
            const owned = projectsOf(team, projects);
            return (
              <li key={team.id} className="group/team hover:bg-hover flex min-h-ctl-lg items-center rounded-row">
                <button
                  onClick={() => onOpen(team.id)}
                  className="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_6rem_7rem_7rem] items-center gap-3 px-3 py-2 text-left outline-none"
                >
                  <span className="text-fg min-w-0 truncate text-sm font-medium">{team.name}</span>
                  <span className="text-mute font-mono text-2xs">{team.key}</span>
                  <span className="text-mute text-right text-2xs tabular-nums">
                    {owned.length}
                  </span>
                  <span className="text-mute text-right text-2xs tabular-nums">
                    {team.members.length}
                  </span>
                </button>
                {!readOnly && (
                  <IconButton
                    label={`Delete ${team.name}`}
                    tooltip="Delete team"
                    variant="ghost"
                    size="sm"
                    className="opacity-0 group-hover/team:opacity-100 focus-visible:opacity-100"
                    onClick={() => onDelete(team)}
                    icon={<Trash2 className="size-icon-sm" />}
                  />
                )}
              </li>
            );
          })}
        </ul>
        </div>
      )}
    </section>
  );
}

/**
 * The create form.
 *
 * Name and identifier only. The engine takes an icon and a lead too, and both
 * are left to the detail page: a create form asks for the least that makes the
 * thing exist, and every field beyond that is one more decision between wanting
 * a team and having one.
 */
function CreateTeam({
  readOnly,
  onBack,
  onCreate,
}: {
  readOnly: boolean;
  onBack: () => void;
  onCreate: (name: string, key: string) => void;
}) {
  const [name, setName] = useState("");
  const [key, setKey] = useState("");
  const [touchedKey, setTouchedKey] = useState(false);
  // Suggested from the name until someone types their own, then left alone —
  // the identifier is the thing you are least likely to have an opinion about
  // and the most likely to be annoyed by re-typing.
  const suggested = name.trim().slice(0, 4).toUpperCase().replace(/[^A-Z0-9]/g, "");
  const identifier = touchedKey ? key : suggested;
  const ready = name.trim().length > 0 && identifier.length > 0;

  return (
    <section className="mb-8">
      <button
        onClick={onBack}
        className="text-mute hover:text-fg mb-4 flex items-center gap-1 text-sm"
      >
        <ArrowLeft className="size-icon-sm" />
        Teams
      </button>
      <SettingsPageHeader
        title="Create a new team"
        description="A team owns projects. Its Issues and Projects views are scoped to that ownership."
        className="mb-4"
      />
      <div className="border-line flex flex-col gap-4 rounded-surface border p-4">
        <Field label="Name" hint="What the sidebar calls it.">
          <TextInput
            label="Team name"
            isLabelHidden
            value={name}
            onChange={setName}
            placeholder="e.g. Platform"
            size="sm"
            width="220px"
            hasAutoFocus
          />
        </Field>
        <Field label="Identifier" hint="Used in addresses — /teams/PLAT/issues.">
          <TextInput
            label="Team identifier"
            isLabelHidden
            value={identifier}
            onChange={(next) => {
              setTouchedKey(true);
              setKey(next.toUpperCase());
            }}
            placeholder="e.g. PLAT"
            size="sm"
            width="220px"
          />
        </Field>
      </div>
      <div className="mt-4 flex justify-end">
        <Button
          label="Create team"
          variant="primary"
          size="sm"
          isDisabled={!ready || readOnly}
          onClick={() => {
            if (ready) onCreate(name.trim(), identifier);
          }}
        />
      </div>
    </section>
  );
}

/** One team: its identity, its members, and what it owns. */
function TeamDetail({
  spaceId,
  team,
  projects,
  members,
  readOnly,
  onBack,
  onError,
}: {
  spaceId: string;
  team: TeamDto;
  projects: ProjectDto[];
  members: MemberDto[];
  readOnly: boolean;
  onBack: () => void;
  onError: (message: string) => void;
}) {
  const [name, setName] = useState(team.name);
  const [key, setKey] = useState(team.key);
  const [memberQuery, setMemberQuery] = useState("");
  const [roleFilter, setRoleFilter] = useState("all");
  const owned = projectsOf(team, projects);
  const teamMembers = useMemo(
    () =>
      team.members.map((memberKey) => ({
        key: memberKey,
        member: members.find((candidate) => candidate.key === memberKey),
      })),
    [members, team.members],
  );
  const shownMembers = useMemo(() => {
    const needle = memberQuery.trim().toLowerCase();
    return teamMembers.filter(({ key: memberKey, member }) => {
      const matchesRole =
        roleFilter === "all" ||
        (roleFilter === "agents" ? Boolean(member?.sponsor) : member?.role === roleFilter);
      const matchesQuery =
        !needle ||
        [memberName(memberKey, member), member?.alias, member?.did, memberKey]
          .filter(Boolean)
          .some((value) => value!.toLowerCase().includes(needle));
      return matchesRole && matchesQuery;
    });
  }, [memberQuery, roleFilter, teamMembers]);
  const availableMembers = useMemo(
    () => members.filter((candidate) => !team.members.includes(candidate.key)),
    [members, team.members],
  );

  const send = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };
  // Committed on blur rather than behind a Save button: every other editable
  // field in this app writes on blur, and a form with one Save in a page
  // without them is a second convention.
  const commit = (field: "name" | "key", value: string) => {
    const next = value.trim();
    if (!next || next === team[field]) return;
    void send(() => rpc(spaceId, { cmd: "team_set", team: team.id, [field]: next }));
  };

  return (
    <section className="mb-8">
      <button
        onClick={onBack}
        className="text-mute hover:text-fg mb-4 flex items-center gap-1 text-sm"
      >
        <ArrowLeft className="size-icon-sm" />
        Teams
      </button>
      <SettingsPageHeader
        title={team.name}
        description={
          owned.length === 0
            ? "Owns no projects yet — assign one from its overview."
            : `Owns ${owned.length} project${owned.length === 1 ? "" : "s"}.`
        }
        className="mb-4"
      />

      <div className="border-line mt-4 flex flex-col gap-4 rounded-surface border p-4">
        <Field label="Name">
          <TextInput
            label="Team name"
            isLabelHidden
            value={name}
            onChange={setName}
            onBlur={() => commit("name", name)}
            size="sm"
            width="220px"
            isDisabled={readOnly}
          />
        </Field>
        <Field label="Identifier" hint="Changing this changes the team's addresses.">
          <TextInput
            label="Team identifier"
            isLabelHidden
            value={key}
            onChange={(next) => setKey(next.toUpperCase())}
            onBlur={() => commit("key", key)}
            size="sm"
            width="220px"
            isDisabled={readOnly}
          />
        </Field>
      </div>

      <h3 className="mt-6 text-sm font-semibold">Projects</h3>
      <ul className="mt-2 flex flex-col gap-0.5">
        {owned.length === 0 && (
          <li className="text-mute text-sm">
            None. A project joins a team from its own overview.
          </li>
        )}
        {owned.map((project) => (
          <li key={project.id} className="flex items-center gap-2 px-2 py-1.5 text-sm">
            <span className="min-w-0 flex-1 truncate">{project.name}</span>
            <span className="text-mute shrink-0 font-mono text-2xs">{project.key}</span>
          </li>
        ))}
      </ul>

      <section className="mt-9">
        <h2 className="text-xl font-semibold tracking-tight">Team members</h2>

        <div className="mt-4 flex items-center gap-2">
          <div className="w-full max-w-xs">
            <TextInput
              label="Search team members"
              isLabelHidden
              value={memberQuery}
              onChange={setMemberQuery}
              placeholder="Search by name or identity"
              startIcon={<Search className="size-icon-sm" />}
              size="sm"
              width="100%"
            />
          </div>
          <Combobox
            label="Filter team members by role"
            value={TEAM_MEMBER_FILTERS.find((option) => option.id === roleFilter) ?? TEAM_MEMBER_FILTERS[0]!}
            options={TEAM_MEMBER_FILTERS}
            onPick={setRoleFilter}
            size="md"
          />
          <div className="flex-1" />
          {!readOnly &&
            (availableMembers.length > 0 ? (
              <Combobox
                label="Add a member"
                value={null}
                placeholder="Add a member"
                face={
                  <>
                    <UserPlus className="size-icon-sm" />
                    <span>Add a member</span>
                  </>
                }
                options={availableMembers.map((candidate) => ({
                  id: candidate.key,
                  label: memberName(candidate.key, candidate),
                  icon: (
                    <Avatar
                      deviceKey={candidate.key}
                      alias={candidate.alias}
                      me={candidate.me}
                      size="sm"
                    />
                  ),
                  hint: teamMemberRole(candidate.role),
                  keywords: [candidate.alias, candidate.did ?? "", candidate.key],
                }))}
                onPick={(memberKey) =>
                  void send(() =>
                    rpc(spaceId, {
                      cmd: "team_set",
                      team: team.id,
                      add_members: [memberKey],
                    }),
                  )
                }
                className="border-accent bg-accent text-accent-fg hover:bg-accent/90"
                size="md"
                wide
              />
            ) : (
              <Button label="Add a member" variant="primary" size="sm" isDisabled />
            ))}
        </div>

        <div className="mt-5 overflow-hidden">
          <div className="text-mute border-line grid grid-cols-[minmax(0,1.35fr)_minmax(0,1fr)_9rem_2rem] items-center gap-4 border-b px-1 py-2 text-2xs">
            <span>Name</span>
            <span>Identity</span>
            <span>Role</span>
            <span aria-hidden />
          </div>
          {shownMembers.length === 0 ? (
            <div className="text-mute flex min-h-28 items-center justify-center text-sm">
              {team.members.length === 0
                ? "No team members yet."
                : `Nothing matches “${memberQuery || TEAM_MEMBER_FILTERS.find((option) => option.id === roleFilter)?.label}”.`}
            </div>
          ) : (
            <ul className="divide-line divide-y">
              {shownMembers.map(({ key: memberKey, member }) => (
                <li
                  key={memberKey}
                  className="group/member grid min-h-ctl-xl grid-cols-[minmax(0,1.35fr)_minmax(0,1fr)_9rem_2rem] items-center gap-4 px-1 py-2"
                >
                  <span className="flex min-w-0 items-center gap-2.5">
                    <Avatar
                      deviceKey={memberKey}
                      {...(member ? { alias: member.alias, me: member.me } : {})}
                      className="size-avatar-lg"
                    />
                    <span className="min-w-0">
                      <span className="block truncate text-sm font-medium">
                        {memberName(memberKey, member)}
                      </span>
                      <span className="text-mute block truncate text-xs">
                        {member?.me ? "You" : member?.sponsor ? "Sponsored agent" : member?.alias || "Member"}
                      </span>
                    </span>
                  </span>
                  <code className="text-mute truncate text-xs" title={member?.did ?? memberKey}>
                    {member?.did ?? memberKey}
                  </code>
                  <span>
                    <Badge
                      tone={member?.role === "admin" || member?.role === "administrator" ? "accent" : "neutral"}
                      className="rounded-mark border-0"
                    >
                      {teamMemberRole(member?.role)}
                    </Badge>
                  </span>
                  {!readOnly && (
                    <IconButton
                      label={`Remove ${memberName(memberKey, member)} from team`}
                      tooltip="Remove from team"
                      variant="ghost"
                      size="sm"
                      className="opacity-0 group-hover/member:opacity-100 focus-visible:opacity-100"
                      onClick={() =>
                        void send(() =>
                          rpc(spaceId, {
                            cmd: "team_set",
                            team: team.id,
                            remove_members: [memberKey],
                          }),
                        )
                      }
                      icon={<Trash2 className="size-icon-sm" />}
                    />
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      </section>
    </section>
  );
}

const TEAM_MEMBER_FILTERS = [
  { id: "all", label: "All" },
  { id: "admin", label: "Workspace admins" },
  { id: "member", label: "Members" },
  { id: "contributor", label: "Contributors" },
  { id: "viewer", label: "Viewers" },
  { id: "agents", label: "Agents" },
];

function teamMemberRole(role?: string): string {
  return (
    {
      admin: "Workspace admin",
      administrator: "Workspace admin",
      member: "Member",
      contributor: "Contributor",
      viewer: "Viewer",
    } as Record<string, string>
  )[role ?? ""] ?? "Member";
}

/** A labelled row in a settings card — label and hint left, control right. */
function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-4">
      <div className="min-w-0 flex-1">
        <div className="text-sm">{label}</div>
        {hint && <div className="text-mute text-2xs">{hint}</div>}
      </div>
      {children}
    </div>
  );
}
