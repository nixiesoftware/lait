import { useMemo, useState } from "react";
import { ArrowLeft, Trash2, Users } from "lucide-react";

import { rpc } from "../api";
import { projectsOf } from "../core/teams";
import type { MemberDto, ProjectDto, TeamDto } from "../types";
import { memberName } from "./Avatar";
import * as ask from "./dialogs";
import { Button, IconButton, TextInput } from "@astryxdesign/core";
import { cn, navigationItem } from "./primitives";

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
    <section className="mb-8">
      <div className="mb-4 flex items-center gap-2">
        <h2 className="flex-1 text-base font-semibold">Teams</h2>
        {!readOnly && (
          <Button label="Create team" variant="primary" size="sm" onClick={onNew} />
        )}
      </div>
      {/* Only once there is enough to filter. A search field over two rows is
          furniture. */}
      {teams.length > 4 && (
        <div className="mb-3">
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
      )}

      {teams.length === 0 ? (
        <p className="text-mute text-sm">
          No teams yet. A team owns projects, and the sidebar navigates by them — Issues, Projects
          and Roadmap, each scoped to what that team owns.
        </p>
      ) : (
        <ul className="flex flex-col gap-0.5">
          {shown.length === 0 && <li className="text-mute text-sm">Nothing matches “{query}”.</li>}
          {shown.map((team) => {
            const owned = projectsOf(team, projects);
            return (
              <li key={team.id} className="group/team flex items-center gap-2">
                <button
                  onClick={() => onOpen(team.id)}
                  className={cn(navigationItem({ size: "lg" }), "min-w-0 flex-1")}
                >
                  <span className="text-fg min-w-0 flex-1 truncate text-sm">{team.name}</span>
                  <span className="text-mute shrink-0 font-mono text-2xs">{team.key}</span>
                  <span className="text-mute w-20 shrink-0 text-right text-2xs tabular-nums">
                    {owned.length} project{owned.length === 1 ? "" : "s"}
                  </span>
                  <span className="text-mute w-20 shrink-0 text-right text-2xs tabular-nums">
                    {team.members.length} member{team.members.length === 1 ? "" : "s"}
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
      <h2 className="text-base font-semibold">Create a new team</h2>
      <p className="text-mute mt-0.5 text-sm">
        A team owns projects. The sidebar gets Issues, Projects and Roadmap for it, each scoped to
        what it owns.
      </p>
      <div className="border-line mt-4 flex flex-col gap-4 rounded-surface border p-4">
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
  const owned = projectsOf(team, projects);

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
      <h2 className="text-base font-semibold">{team.name}</h2>
      <p className="text-mute mt-0.5 text-sm">
        {owned.length === 0
          ? "Owns no projects yet — assign one from its overview."
          : `Owns ${owned.length} project${owned.length === 1 ? "" : "s"}.`}
      </p>

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

      <h3 className="mt-6 flex items-center gap-1.5 text-sm font-semibold">
        <Users className="size-icon-sm" />
        Members
      </h3>
      <ul className="mt-2 flex flex-col gap-0.5">
        {team.members.length === 0 && <li className="text-mute text-sm">Nobody yet.</li>}
        {team.members.map((who) => {
          const member = members.find((candidate) => candidate.key === who);
          return (
            <li key={who} className="group/member flex items-center gap-2 px-2 py-1.5 text-sm">
              <span className="min-w-0 flex-1 truncate">
                {memberName(who, member)}
              </span>
              {!readOnly && (
                <IconButton
                  label="Remove from team"
                  tooltip="Remove from team"
                  variant="ghost"
                  size="sm"
                  className="opacity-0 group-hover/member:opacity-100 focus-visible:opacity-100"
                  onClick={() =>
                    void send(() =>
                      rpc(spaceId, { cmd: "team_set", team: team.id, remove_members: [who] }),
                    )
                  }
                  icon={<Trash2 className="size-icon-sm" />}
                />
              )}
            </li>
          );
        })}
      </ul>
      {!readOnly && (
        <div className="mt-2 flex flex-wrap gap-1">
          {members
            .filter((candidate) => !team.members.includes(candidate.key))
            .map((candidate) => (
              <Button
                key={candidate.key}
                label={`+ ${memberName(candidate.key, candidate)}`}
                variant="ghost"
                size="sm"
                onClick={() =>
                  void send(() =>
                    rpc(spaceId, {
                      cmd: "team_set",
                      team: team.id,
                      add_members: [candidate.key],
                    }),
                  )
                }
              />
            ))}
        </div>
      )}
    </section>
  );
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
