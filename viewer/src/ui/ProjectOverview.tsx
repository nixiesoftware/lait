import { useRef, useState } from "react";
import { Archive, ArchiveRestore, MoreHorizontal, Plus, UserPlus } from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { rpc } from "../api";
import {
  useProjectMilestones,
  useProjectUpdates,
  useProjectViewerStore,
} from "../projectStore";
import { milestonePercent, milestoneProgress } from "../core/milestone";
import type { MemberDto, MilestoneDto, ProjectDto } from "../types";
import { Avatar, memberName } from "./Avatar";
import { catalogColor } from "./colors";
import { ColorPicker } from "./ColorPicker";
import { DatePicker } from "./DatePicker";
import { Markdown } from "./Markdown";
import { MarkdownEditor } from "./MarkdownEditor";
import { Combobox } from "./Picker";
import { MilestoneIcon } from "./icons";
import { MenuContent, MenuItem, RailCard, RailRow } from "./layout";
import { Button, IconButton, Input, PopoverContent } from "./primitives";
import { when } from "./time";
import * as Popover from "@radix-ui/react-popover";

/** The health signals a project update can carry — Linear's on-track palette. */
const HEALTH: Record<string, { label: string; tone: string }> = {
  on_track: { label: "On track", tone: "text-ok" },
  at_risk: { label: "At risk", tone: "text-warn" },
  off_track: { label: "Off track", tone: "text-danger" },
};

/** unix seconds -> YYYY-MM-DD (UTC), the wire format the engine + DatePicker share. */
function toInput(secs: number | null | undefined): string | null {
  if (secs == null) return null;
  return new Date(secs * 1000).toISOString().slice(0, 10);
}

/**
 * A project's overview — the document a project became.
 *
 * A lait project used to be `{name, key, color}`; the catalog now carries a
 * description, a lead, and a planned window, so this is the page that edits them.
 * Every field is a `project_edit` on the way out (the same LWW catalog write the
 * settings labels page uses); there is no project doc/body yet, so the description
 * is a catalog register — good for an overview paragraph, not a wiki.
 */
export function ProjectOverview({
  spaceId,
  project,
  members,
  counts,
  readOnly,
  onError,
  onOpenMilestone,
}: {
  spaceId: string;
  project: ProjectDto;
  members: MemberDto[];
  counts: { backlog: number; active: number; done: number; total: number };
  readOnly: boolean;
  onError: (message: string) => void;
  /** Scope the project's issue surfaces to one milestone (`""` = unscoped
   *  issues). A milestone is a filter, so this navigates rather than selects. */
  onOpenMilestone: (milestone: string) => void;
}) {
  const edit = async (patch: Record<string, string | boolean | null>) => {
    try {
      await rpc(spaceId, { cmd: "project_edit", project: project.key, ...patch });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  const lead = members.find((m) => m.key === project.lead);
  const { backlog, active, done, total } = counts;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="@container min-h-0 flex-1 overflow-y-auto p-6">
        {/* Wider than it was, and all of the extra width goes to the rail.
            264px is a property column — enough for a value and nothing else. The
            rail now packs a glyph, a name, a percentage, a count and a date onto
            one scannable line, and that needs ~340. The prose keeps its 35rem
            measure, so the document did not get harder to read to pay for it.

            The split is a **container** query, not `md:`. The viewport is not
            what this layout is short of — the sidebar is — so a viewport
            breakpoint happily put two columns in 588px and left the document
            216px wide, which is four words a line. 54rem is the width at which a
            fixed 340px rail still leaves the prose something worth reading;
            under it the rail stacks beneath the document at full width. */}
        <div className="mx-auto flex max-w-5xl flex-col gap-8 @[54rem]:grid @[54rem]:grid-cols-[minmax(0,1fr)_340px]">
          {/* Title + description. The measure is capped at the same 35rem the
              issue body uses — a project overview is the same kind of document,
              and two prose columns at different widths in one app read as two
              apps. */}
          <div className="min-w-0 max-w-[35rem]">
            <div className="mb-4 flex items-center gap-2">
              {!readOnly ? (
                <Popover.Root>
                  <Popover.Trigger asChild>
                    <button
                      aria-label="Project colour"
                      className="hover:ring-line-strong rounded-mark p-0.5 hover:ring-1"
                    >
                      <span
                        className="block size-mark-xl rounded-mark"
                        style={{ background: catalogColor(project.color) }}
                      />
                    </button>
                  </Popover.Trigger>
                  <PopoverContent align="start" className="p-2">
                    <ColorPicker
                      value={project.color}
                      onChange={(color) => void edit({ color })}
                    />
                  </PopoverContent>
                </Popover.Root>
              ) : (
                <span
                  className="block size-mark-xl rounded-mark"
                  style={{ background: catalogColor(project.color) }}
                />
              )}
              <input
                defaultValue={project.name}
                readOnly={readOnly}
                onBlur={(e) => {
                  const next = e.target.value.trim();
                  if (next && next !== project.name) void edit({ name: next });
                }}
                // Same size as an issue title: both are the name of the document
                // you are looking at, and the overview was a step smaller for no
                // reason other than that it was written on a different day.
                className="min-w-0 flex-1 bg-transparent text-2xl font-semibold tracking-tight outline-none"
                aria-label="Project name"
              />
              {project.archived && (
                <span className="border-line text-mute rounded-mark border px-1.5 py-px text-2xs">
                  Archived
                </span>
              )}
              {!readOnly && (
                <IconButton
                  label={project.archived ? "Restore project" : "Archive project"}
                  onClick={() => void edit({ archived: !project.archived })}
                >
                  {project.archived ? (
                    <ArchiveRestore className="size-icon-sm" />
                  ) : (
                    <Archive className="size-icon-sm" />
                  )}
                </IconButton>
              )}
            </div>
            <Description
              value={project.description ?? ""}
              readOnly={readOnly}
              onSave={(description) => void edit({ description })}
            />
            <Updates
              spaceId={spaceId}
              projectId={project.id}
              members={members}
              readOnly={readOnly}
              onError={onError}
            />
          </div>

          {/* A console, not an aside.

              The issue rail recedes because the body is the point: captions over
              a hairline column, nothing to click. This one does the opposite —
              cards with their own verbs, rows that navigate, folds that persist.
              A project is a thing you steer; an issue is a thing you read, and
              lending them one grammar made the steering surface as quiet as the
              reading one.

              What survives from that rail is the rule worth keeping: an unset
              property still reads as the verb that sets it. */}
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
        </div>
      </div>
    </div>
  );
}

/** The overview paragraph — the same live document the issue body is, so the two
 *  surfaces are written the same way as well as read the same way. */
function Description({
  value,
  readOnly,
  onSave,
}: {
  value: string;
  readOnly: boolean;
  onSave: (v: string) => void;
}) {
  const [, setDraft] = useState(value);
  const dirty = useRef(false);

  if (readOnly) {
    return (
      <div className="min-h-16 py-2">
        {value ? <Markdown text={value} /> : <span className="text-mute">No description</span>}
      </div>
    );
  }

  return (
    <MarkdownEditor
      value={value}
      placeholder="Describe this project — goals, scope, links."
      className="min-h-16 py-2"
      onChange={(markdown) => {
        dirty.current = true;
        setDraft(markdown);
      }}
      onCommit={() => {
        if (!dirty.current) return;
        dirty.current = false;
        setDraft((current) => {
          if (current !== value) onSave(current);
          return current;
        });
      }}
    />
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
  onError,
  onOpen,
}: {
  spaceId: string;
  projectId: string;
  readOnly: boolean;
  onError: (message: string) => void;
  onOpen: (milestone: string) => void;
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
          >
            <Plus className="size-icon-sm" />
          </IconButton>
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
            onOpen={() => onOpen(m.id)}
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
              onClick={() => onOpen("")}
              className="text-mute hover:bg-hover hover:text-fg flex h-ctl-lg w-full items-center gap-2 rounded-control px-1 text-left text-xs outline-none focus-visible:ring-1 focus-visible:ring-accent/50"
            >
              <MilestoneIcon progress="none" />
              <span className="min-w-0 flex-1 truncate">No milestone</span>
            </button>
          </li>
        )}
      </ol>

      {!readOnly && composing && (
        <div className="border-line mt-1 flex flex-col gap-1.5 border-t pt-2">
          <Input
            autoFocus
            size="sm"
            value={draft}
            placeholder="Milestone name…"
            onChange={(e) => setDraft(e.target.value)}
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
              variant="primary"
              size="sm"
              className="ml-auto"
              disabled={!draft.trim() || adding}
              onClick={() => void add()}
            >
              Add
            </Button>
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
  onOpen,
  onRemove,
  onMoveUp,
  onMoveDown,
}: {
  milestone: MilestoneDto;
  readOnly: boolean;
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
    <li className="group/ms hover:bg-hover flex h-ctl-lg items-center rounded-control">
      <button
        type="button"
        onClick={onOpen}
        aria-label={`Show issues in ${m.name}`}
        className="text-dim group-hover/ms:text-fg flex h-full min-w-0 flex-1 items-center gap-2 rounded-control px-1 text-left text-xs outline-none focus-visible:ring-1 focus-visible:ring-accent/50"
      >
        <MilestoneIcon progress={milestoneProgress(m)} />
        <span className="min-w-0 flex-1 truncate">{m.name}</span>
        <span className="text-mute shrink-0 tabular-nums">
          {pct}% of {m.total}
        </span>
        {m.target_date != null && (
          <span className="text-mute shrink-0 tabular-nums">{shortDate(m.target_date)}</span>
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
        <DropdownMenu.Root>
          <DropdownMenu.Trigger asChild>
            <IconButton
              label={`Milestone actions for ${m.name}`}
              className="text-mute hover:text-fg mr-0.5 shrink-0"
            >
              <MoreHorizontal className="size-icon-sm" />
            </IconButton>
          </DropdownMenu.Trigger>
          <MenuContent align="end">
            {onMoveUp && <MenuItem onSelect={onMoveUp}>Move up</MenuItem>}
            {onMoveDown && <MenuItem onSelect={onMoveDown}>Move down</MenuItem>}
            <MenuItem danger onSelect={onRemove}>
              Remove milestone
            </MenuItem>
          </MenuContent>
        </DropdownMenu.Root>
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

/**
 * The project updates feed (SCOPE-1) — an append-only stream of status posts.
 *
 * Each update is an immutable record in the engine's grow-only `project_updates`
 * log (a catalog map, not a per-project doc: an update is authored once, so a
 * record is the honest shape and it needs no collaborative-text merge). This
 * posts via `project_update_post` and reads the store's `updates` resource, so
 * a teammate's post arrives on the doorbell rather than on our next reload.
 */
function Updates({
  spaceId,
  projectId,
  members,
  readOnly,
  onError,
}: {
  spaceId: string;
  projectId: string;
  members: MemberDto[];
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const store = useProjectViewerStore();
  const resource = useProjectUpdates(spaceId, projectId);
  const updates = resource.data ?? null;
  const [draft, setDraft] = useState("");
  const [health, setHealth] = useState("");
  const [posting, setPosting] = useState(false);

  const post = async () => {
    const body = draft.trim();
    if (!body) return;
    setPosting(true);
    try {
      await rpc(spaceId, { cmd: "project_update_post", project: projectId, body, health });
      setDraft("");
      setHealth("");
      await store.ensureUpdates(spaceId, projectId, true);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setPosting(false);
    }
  };

  return (
    <section className="mt-8">
      <h2 className="text-mute mb-3 text-2xs font-semibold tracking-wider uppercase">Updates</h2>

      {!readOnly && (
        <div className="border-line focus-within:border-line-strong mb-4 rounded-surface border bg-[var(--field-bg)] p-3 transition-colors">
          <textarea
            value={draft}
            rows={2}
            placeholder="Post a status update — what changed, what's next…"
            onChange={(e) => setDraft(e.target.value)}
            className="placeholder:text-mute w-full resize-none bg-transparent text-sm outline-none"
            aria-label="New project update"
          />
          <div className="mt-2 flex items-center gap-2">
            <Combobox
              label="Health"
              value={{ id: health, label: health ? (HEALTH[health]?.label ?? health) : "No health" }}
              placeholder="Health"
              options={[
                { id: "", label: "No health" },
                { id: "on_track", label: "On track" },
                { id: "at_risk", label: "At risk" },
                { id: "off_track", label: "Off track" },
              ]}
              onPick={setHealth}
            />
            <Button
              variant="primary"
              size="md"
              className="ml-auto"
              disabled={!draft.trim() || posting}
              loading={posting}
              onClick={() => void post()}
            >
              Post update
            </Button>
          </div>
        </div>
      )}

      {resource.error != null && <p className="text-danger text-sm">Couldn't load updates.</p>}
      {!updates && resource.error == null && <p className="text-mute text-sm">Loading…</p>}
      {updates && updates.length === 0 && (
        <p className="text-mute text-sm">No updates yet.</p>
      )}
      <ol className="flex flex-col gap-4">
        {updates?.map((u) => {
          const author = members.find((m) => m.key === u.author);
          const h = u.health ? HEALTH[u.health] : undefined;
          return (
            <li key={u.id} className="border-line border-l-2 pl-3">
              <div className="mb-1 flex items-center gap-2 text-sm">
                {author && <Avatar deviceKey={u.author} alias={author.alias} me={author.me} size="sm" />}
                <span className="font-medium">{memberName(u.author, author)}</span>
                {h && <span className={`text-2xs ${h.tone}`}>· {h.label}</span>}
                <span className="text-mute ml-auto text-xs">{when(u.ts)}</span>
              </div>
              <div className="text-sm">
                <Markdown text={u.body} />
              </div>
            </li>
          );
        })}
      </ol>
    </section>
  );
}
