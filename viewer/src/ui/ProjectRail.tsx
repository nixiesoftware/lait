import { useState } from "react";
import { ArrowDown, ArrowUp, MoreHorizontal, Plus, Trash2, UserPlus, UsersRound } from "lucide-react";

import { rpc } from "../api";
import { useProjectMilestones, useProjectViewerStore } from "../projectStore";
import { milestonePercent, milestoneProgress } from "../core/milestone";
import type { MemberDto, MilestoneDto, ProjectDto, TeamDto } from "../types";
import { Avatar, memberName } from "./Avatar";
import { DatePicker } from "./DatePicker";
import { Combobox } from "./Picker";
import { MilestoneIcon } from "./icons";
import { RailCard, RailRow } from "./layout";
import { Button, DropdownMenu, DropdownMenuItem, IconButton, TextInput } from "@astryxdesign/core";
import { cn } from "./primitives";

/** unix seconds -> YYYY-MM-DD (UTC), the wire format the engine + DatePicker share. */
function toInput(secs: number | null | undefined): string | null {
  if (secs == null) return null;
  return new Date(secs * 1000).toISOString().slice(0, 10);
}

/**
 * The project's console — and it now belongs to the project SHELL, not to the
 * overview page.
 *
 * It used to live inside `ProjectOverview`, which made "the project's
 * properties" a thing you could only see on one of the project's five faces.
 * The moment a milestone became a filter that answer stopped holding: you click
 * a milestone to narrow the issues, land on Issues, and the panel you clicked
 * from vanished — taking with it the only way to clear the filter or pick a
 * different stage. A console you have to navigate away from to use is not a
 * console.
 *
 * So it is drawn beside every project surface, and the shell owns whether it is
 * open. Overview, Activity and the three issue layouts all keep it.
 */
export function ProjectRail({
  spaceId,
  project,
  members,
  teams,
  counts,
  readOnly,
  activeMilestone,
  onError,
  onOpenMilestone,
}: {
  spaceId: string;
  project: ProjectDto;
  members: MemberDto[];
  /** Every team in the space, for the owner picker. */
  teams: TeamDto[];
  counts: { backlog: number; active: number; done: number; total: number };
  readOnly: boolean;
  /** The `mls_` id currently scoping the issue surfaces, `""` for the
   *  No-milestone bucket, or `null` when nothing is scoped. */
  activeMilestone: string | null;
  onError: (message: string) => void;
  /** Pass `null` to clear the scope. */
  onOpenMilestone: (milestone: string | null) => void;
}) {
  const edit = async (patch: Record<string, string | boolean | null>) => {
    try {
      await rpc(spaceId, { cmd: "project_edit", project: project.key, ...patch });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  const lead = members.find((m) => m.key === project.lead);
  const team = teams.find((candidate) => candidate.id === project.team);
  const { backlog, active, done, total } = counts;

  return (
          <div className="flex flex-col gap-2 text-sm">
            <RailCard title="Properties" id="properties">
            <RailRow label="Lead">
              <Combobox
                tone="quiet"
                label="Lead"
                disabled={readOnly}
                value={
                  lead
                    ? {
                        id: lead.key,
                        label: memberName(lead.key, lead),
                        icon: <Avatar deviceKey={lead.key} alias={lead.alias} me={lead.me} size="sm" />,
                      }
                    : null
                }
                face={
                  lead ? undefined : (
                    <>
                      <UserPlus className="text-mute size-icon-sm shrink-0" />
                      <span className="text-mute">Set lead</span>
                    </>
                  )
                }
                options={[
                  { id: "none", label: "No lead" },
                  ...members.map((m) => ({
                    id: m.key,
                    label: memberName(m.key, m),
                    icon: <Avatar deviceKey={m.key} alias={m.alias} me={m.me} size="sm" />,
                    hint: m.key.slice(0, 6),
                    keywords: [m.key, m.alias],
                  })),
                ]}
                onPick={(id) => void edit({ lead: id === "none" ? "none" : id })}
              />
            </RailRow>
            {/* Which team owns this project — the write the sidebar's grouping
                reads. Above the dates because it decides where the project
                *is*, and the dates only decide when it happens.

                Offered even with no teams yet, so the row is where you learn
                the concept exists; picking "No team" is the same write as
                clearing a lead. */}
            <RailRow label="Team">
              <Combobox
                tone="quiet"
                label="Team"
                disabled={readOnly}
                value={team ? { id: team.id, label: team.name, hint: team.key } : null}
                face={
                  team ? undefined : (
                    <>
                      <UsersRound className="text-mute size-icon-sm shrink-0" />
                      <span className="text-mute">
                        {teams.length === 0 ? "No teams yet" : "Set team"}
                      </span>
                    </>
                  )
                }
                options={[
                  { id: "none", label: "No team" },
                  ...teams.map((candidate) => ({
                    id: candidate.id,
                    label: candidate.name,
                    hint: candidate.key,
                    keywords: [candidate.key, candidate.name],
                  })),
                ]}
                onPick={(id) => void edit({ team: id === "none" ? "" : id })}
              />
            </RailRow>
            <RailRow label="Start date">
              <DatePicker
                tone="quiet"
                value={toInput(project.start_date)}
                disabled={readOnly}
                placeholder="Add start date"
                ariaLabel="Start date"
                onChange={(next) => void edit({ start: next ?? "none" })}
              />
            </RailRow>
            <RailRow label="Target date">
              <DatePicker
                tone="quiet"
                value={toInput(project.target_date)}
                disabled={readOnly}
                placeholder="Add target date"
                ariaLabel="Target date"
                onChange={(next) => void edit({ target: next ?? "none" })}
              />
            </RailRow>
            </RailCard>

            <Milestones
              spaceId={spaceId}
              projectId={project.id}
              readOnly={readOnly}
              active={activeMilestone}
              onError={onError}
              onOpen={onOpenMilestone}
            />

            {/* Read rather than set, so it is the one card with no verb in its
                header. The legend leads because the counts are the answer and
                the bar is the shape of it — Linear's project rail makes the same
                call, and a bare bar over a sentence made you read the sentence
                to learn what the colours meant. */}
            <RailCard title="Progress" id="progress">
              <div className="flex flex-col gap-2 pt-1">
                <div className="flex items-baseline gap-4 text-xs">
                  <span className="flex items-baseline gap-1.5">
                    <span className="bg-mute inline-block size-mark-sm rounded-mark" aria-hidden />
                    <span className="text-mute">Scope</span>
                    <span className="text-ink tabular-nums">{total}</span>
                  </span>
                  <span className="flex items-baseline gap-1.5">
                    <span className="bg-ok inline-block size-mark-sm rounded-mark" aria-hidden />
                    <span className="text-mute">Completed</span>
                    <span className="text-ink tabular-nums">{done}</span>
                  </span>
                </div>
                <span className="bg-line flex h-1.5 w-full gap-0.5 overflow-hidden rounded-full">
                  {total === 0 ? null : (
                    <>
                      {backlog > 0 && <span className="bg-mute" style={{ flex: backlog }} />}
                      {active > 0 && <span className="bg-accent" style={{ flex: active }} />}
                      {done > 0 && <span className="bg-ok" style={{ flex: done }} />}
                    </>
                  )}
                </span>
                <span className="text-mute text-2xs">
                  {total === 0 ? "No issues yet" : `${active} active · ${backlog} backlog`}
                </span>
              </div>
            </RailCard>
          </div>
  );
}

/**
 * The project's milestones (SCOPE-1): named targets with derived progress.
 * Records live in the catalog's `project_milestones` map; the counts are
 * derived by the engine from issues' milestone pointers, never stored.
 *
 * Read through the world store rather than a local `useState`, because "derived
 * from issues" means the bars go stale on a write this component never sees:
 * dragging a card to Done changes a percentage here. The store already declares
 * that dependency (`ensureMilestones`), so the doorbell refetches; a private
 * copy simply would not hear about it, which is what this used to be.
 */
function Milestones({
  spaceId,
  projectId,
  readOnly,
  active,
  onError,
  onOpen,
}: {
  spaceId: string;
  projectId: string;
  readOnly: boolean;
  active: string | null;
  onError: (message: string) => void;
  onOpen: (milestone: string | null) => void;
}) {
  const store = useProjectViewerStore();
  const resource = useProjectMilestones(spaceId, projectId);
  const milestones = resource.data ?? null;
  const [draft, setDraft] = useState("");
  const [target, setTarget] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [composing, setComposing] = useState(false);

  // A local write rings the doorbell too, but the round trip is longer than the
  // one we just made — force the resource so the row lands with the click.
  const reload = () => store.ensureMilestones(spaceId, projectId, true);

  const add = async () => {
    const name = draft.trim();
    if (!name) return;
    setAdding(true);
    try {
      await rpc(spaceId, {
        cmd: "milestone_set",
        project: projectId,
        name,
        target,
      });
      setDraft("");
      setTarget(null);
      setComposing(false);
      await reload();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await rpc(spaceId, { cmd: "milestone_set", project: projectId, milestone: id, remove: true });
      await reload();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  /**
   * Move one step, expressed as "put me on the far side of my neighbour".
   *
   * The engine's placement vocabulary is relative (`before`/`after` a sibling),
   * not positional, which is what makes a move one record write instead of a
   * renumbering. So "up" is `before` the milestone above me — the list this
   * component is already rendering supplies the neighbour.
   */
  const move = async (index: number, delta: -1 | 1) => {
    const list = milestones ?? [];
    const neighbour = list[index + delta];
    if (!neighbour) return;
    try {
      await rpc(spaceId, {
        cmd: "milestone_set",
        project: projectId,
        milestone: list[index]!.id,
        pos: delta < 0 ? { at: "before", reff: neighbour.id } : { at: "after", reff: neighbour.id },
      });
      await reload();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  const empty = milestones !== null && milestones.length === 0;

  return (
    <RailCard
      title="Milestones"
      id="milestones"
      action={
        !readOnly && (
          <IconButton
            label="Add milestone"
            onClick={() => setComposing((open) => !open)}
            variant="ghost"
            size="sm"
            tooltip="Add milestone"
            icon={<Plus className="size-icon-sm" />}
          />
        )
      }
    >
      {resource.error != null && <p className="text-danger py-1 text-xs">Couldn't load milestones.</p>}
      {milestones === null && resource.error == null && (
        <p className="text-mute py-1 text-xs">Loading…</p>
      )}
      {/* The empty state teaches rather than reports. "No milestones yet" told
          you a fact you could already see; this says what the feature is for,
          which is the only thing you need at the moment you have none. */}
      {empty && !composing && (
        <p className="text-mute py-1 text-xs leading-relaxed">
          {readOnly
            ? "No milestones."
            : "Break the project into stages, each with its own target date and progress."}
        </p>
      )}

      <ol className="flex flex-col">
        {milestones?.map((m, index) => (
          <MilestoneRow
            key={m.id}
            milestone={m}
            readOnly={readOnly}
            // Scoping is a toggle: clicking the milestone you are already inside
            // is how you get back out, and it is the same click that got you in.
            active={active === m.id}
            dimmed={active !== null && active !== m.id}
            onOpen={() => onOpen(active === m.id ? null : m.id)}
            onRemove={() => void remove(m.id)}
            onMoveUp={index > 0 ? () => void move(index, -1) : undefined}
            onMoveDown={index < milestones.length - 1 ? () => void move(index, 1) : undefined}
          />
        ))}
        {/* The bucket, and the reason it is here rather than only in the filter
            menu: "what has nobody scoped yet" is the question this list is for,
            and it is the one row a per-milestone index cannot otherwise offer.
            Hidden until there is a milestone, because with none it would just be
            a link to every issue in the project. */}
        {!empty && milestones !== null && (
          <li>
            <button
              type="button"
              onClick={() => onOpen(active === "" ? null : "")}
              className={cn(
                "text-mute hover:bg-hover hover:text-fg flex h-ctl-lg w-full items-center gap-2 rounded-control px-1 text-left text-xs outline-none focus-visible:ring-1 focus-visible:ring-accent/50",
                active === "" && "bg-active text-fg",
                active !== null && active !== "" && "opacity-45",
              )}
            >
              <MilestoneIcon progress="none" />
              <span className="min-w-0 flex-1 truncate">No milestone</span>
              {active === "" && <span className="text-accent shrink-0">Clear filter</span>}
            </button>
          </li>
        )}
      </ol>

      {!readOnly && composing && (
        <div className="border-line mt-1 flex flex-col gap-1.5 border-t pt-2">
          <TextInput
            label="Milestone name"
            isLabelHidden
            hasAutoFocus
            size="sm"
            value={draft}
            placeholder="Milestone name…"
            onChange={setDraft}
            width="100%"
            onKeyDown={(e) => {
              if (e.key === "Enter" && draft.trim()) void add();
              if (e.key === "Escape") setComposing(false);
            }}
            aria-label="New milestone name"
          />
          <div className="flex items-center gap-1">
            <DatePicker
              tone="quiet"
              value={target}
              placeholder="Target"
              ariaLabel="Milestone target date"
              onChange={setTarget}
            />
            <Button
              className="ml-auto"
              isDisabled={!draft.trim() || adding}
              onClick={() => void add()}
              label="Add"
              variant="primary"
              size="sm"
            />
          </div>
        </div>
      )}
    </RailCard>
  );
}

/**
 * One row of the milestone index.
 *
 * Reads as `◆ Beta · 50% of 4 · Sep 1`, which is the order you ask the questions
 * in: what stage, how far along, how many, by when. The glyph leads because it is
 * the only part you can read without reading — a column of diamonds tells you the
 * shape of the project before any of the words do.
 *
 * `50% of 4` rather than `2/4 · 50%`: the old row printed the same fact twice in
 * two alphabets, and in a rail this narrow the percentage is what you scan and
 * the count is the footnote.
 */
function MilestoneRow({
  milestone: m,
  readOnly,
  active,
  dimmed,
  onOpen,
  onRemove,
  onMoveUp,
  onMoveDown,
}: {
  milestone: MilestoneDto;
  readOnly: boolean;
  /** This milestone is scoping the issue surfaces right now. */
  active: boolean;
  /** Some OTHER milestone is. */
  dimmed: boolean;
  onOpen: () => void;
  onRemove: () => void;
  /** Absent at the ends of the list — the verb is offered only where it works.
   *  Explicitly `| undefined` because `exactOptionalPropertyTypes` is on: an
   *  optional prop and a prop that may be passed as `undefined` are different
   *  types here, and the caller computes these. */
  onMoveUp?: (() => void) | undefined;
  onMoveDown?: (() => void) | undefined;
}) {
  const pct = milestonePercent(m);
  return (
    // The hover surface is the whole row, menu included. It used to live on the
    // name button, so the highlight stopped short of the `…` and the control you
    // were reaching for sat outside the thing that had lit up to say it was
    // reachable — the row looked like it ended where it did not.
    <li
      className={cn(
        "group/ms hover:bg-hover flex h-ctl-lg items-center rounded-control transition-opacity",
        active && "bg-active",
        // The unselected stages recede rather than disappear. Which milestone is
        // scoping the list is only legible against the ones that are not, and a
        // filtered rail that showed one row would have thrown away the context
        // that makes the filter mean something.
        dimmed && "opacity-45 hover:opacity-100",
      )}
    >
      <button
        type="button"
        onClick={onOpen}
        aria-label={active ? `Clear the ${m.name} filter` : `Show issues in ${m.name}`}
        className={cn(
          "flex h-full min-w-0 flex-1 items-center gap-2 rounded-control px-1 text-left text-xs outline-none focus-visible:ring-1 focus-visible:ring-accent/50",
          active ? "text-fg" : "text-dim group-hover/ms:text-fg",
        )}
      >
        <MilestoneIcon progress={milestoneProgress(m)} />
        <span className="min-w-0 flex-1 truncate">{m.name}</span>
        {/* The numbers step aside for the way out. A row that is already
            filtering has said what it counts; what you need from it next is the
            undo, and it goes where your eye already is rather than in a bar at
            the bottom of a list you may have scrolled past. */}
        {active ? (
          <span className="text-accent shrink-0">Clear filter</span>
        ) : (
          <>
            <span className="text-mute shrink-0 tabular-nums">
              {pct}% of {m.total}
            </span>
            {m.target_date != null && (
              <span className="text-mute shrink-0 tabular-nums">{shortDate(m.target_date)}</span>
            )}
          </>
        )}
      </button>
      {!readOnly && (
        // A menu, not a bare `×`. Removal is the least reversible thing you can
        // do to a milestone, and it had been sitting where a menu belongs — one
        // hover away from a row you were only trying to click through.
        //
        // Always drawn, never hover-revealed. This rail is a console: a verb that
        // only exists once you have found it is a verb you have to already know
        // about, and the cost of showing it is one muted glyph per row.
        <DropdownMenu
          alignment="end"
          hasChevron={false}
          button={{
            label: `Milestone actions for ${m.name}`,
            className: "text-mute hover:text-fg mr-0.5 shrink-0",
            variant: "ghost",
            size: "sm",
            isIconOnly: true,
            tooltip: `Milestone actions for ${m.name}`,
            icon: <MoreHorizontal className="size-icon-sm" />,
          }}
        >
          {onMoveUp && (
            <DropdownMenuItem
              label="Move up"
              icon={<ArrowUp className="size-icon-sm" />}
              onClick={onMoveUp}
            />
          )}
          {onMoveDown && (
            <DropdownMenuItem
              label="Move down"
              icon={<ArrowDown className="size-icon-sm" />}
              onClick={onMoveDown}
            />
          )}
          {/* Astryx's menu item has no destructive tone, and `label` is a
              ReactNode — so the tone rides on the label rather than becoming a
              generated variant for one item. The icon takes the same colour so
              the row reads as one destructive thing rather than a red word
              beside a neutral glyph. */}
          <DropdownMenuItem
            label={<span className="text-danger">Remove milestone</span>}
            icon={<Trash2 className="size-icon-sm text-danger" />}
            onClick={onRemove}
          />
        </DropdownMenu>
      )}
    </li>
  );
}

/** `Sep 1` — the rail has no room for a full date and no need for the year on a
 *  target that is almost always months away, not years. */
function shortDate(secs: number): string {
  return new Date(secs * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    timeZone: "UTC",
  });
}
