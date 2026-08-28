import { useId, useRef, useState } from "react";
import { Archive, ArchiveRestore, ChevronRight, SquarePen, UserPlus, UsersRound } from "lucide-react";

import { rpc } from "../api";
import {
  useProjectMilestones,
  useProjectUpdates,
  useProjectViewerStore,
} from "../projectStore";
import { milestonePercent, milestoneProgress } from "../core/milestone";
import type { MemberDto, MilestoneDto, ProjectDto, TeamDto } from "../types";
import { Avatar, memberName } from "./Avatar";
import { catalogColor } from "./colors";
import { ColorPicker } from "./ColorPicker";
import { DatePicker } from "./DatePicker";
import { Markdown } from "./Markdown";
import { MarkdownEditor } from "./MarkdownEditor";
import { Combobox } from "./Picker";
import { MilestoneIcon, ProjectIcon } from "./icons";
import { Button, IconButton, Popover } from "@astryxdesign/core";
import { cn, titleText } from "./primitives";
import { when } from "./time";

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

/** The caption over a movement of the document — Description, Milestones,
 *  Updates. One string, because three sections at three weights read as three
 *  pages stapled together. */
const CAPTION = "text-mute mb-3 text-2xs font-semibold tracking-wider uppercase";

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
  teams,
  readOnly,
  onError,
}: {
  spaceId: string;
  project: ProjectDto;
  members: MemberDto[];
  /** Every team in the space, for the owner picker. */
  teams: TeamDto[];
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const edit = async (patch: Record<string, string | boolean | null>) => {
    try {
      await rpc(spaceId, { cmd: "project_edit", project: project.key, ...patch });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto p-6">
        {/* One column. The rail is the shell's — it holds the milestone SCOPE,
            which has to survive a hop to Issues, and this page does not. What
            the rail does not hold is anything this page is ABOUT: the project's
            properties are stated here, because a document that loses its facts
            when a panel is shut is not the document about that thing. */}
        <div className="mx-auto max-w-3xl">
          {/* The measure is capped at the same 35rem the issue body uses — a
              project overview is the same kind of document, and two prose
              columns at different widths in one app read as two apps. */}
          <div className="min-w-0 max-w-[35rem]">
            <Identity project={project} readOnly={readOnly} onEdit={edit} />
            <Properties
              project={project}
              members={members}
              teams={teams}
              readOnly={readOnly}
              onEdit={edit}
            />
            <section className="mt-6">
              <h2 className={CAPTION}>Description</h2>
              <Description
                value={project.description ?? ""}
                readOnly={readOnly}
                placeholder="Describe this project — goals, scope, links."
                empty="No description"
                onSave={(description) => void edit({ description })}
              />
            </section>
            <MilestoneDocument
              spaceId={spaceId}
              projectId={project.id}
              readOnly={readOnly}
              onError={onError}
            />
            <Updates
              spaceId={spaceId}
              projectId={project.id}
              members={members}
              readOnly={readOnly}
              onError={onError}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * The project's face: its mark, and its name.
 *
 * The mark is a BLOCK above the name rather than a glyph beside it. Inline, at
 * `icon-lg`, it read as a list row that had been given a page — the same 20px
 * dot the sidebar draws, with a slightly larger word after it. A project is the
 * only thing in this app a person names, colours and then looks at for months,
 * and the header is where that colour is the project's own rather than a
 * category stripe. The fill is the catalog colour at 14%, so a project reads as
 * itself at a glance and the glyph inside still holds its contrast.
 */
function Identity({
  project,
  readOnly,
  onEdit,
}: {
  project: ProjectDto;
  readOnly: boolean;
  onEdit: (patch: Record<string, string | boolean | null>) => void;
}) {
  const color = catalogColor(project.color);
  // `ctl-xl` (40px) rather than a literal 48: the block is a control — it opens
  // the colour picker — so its size belongs on the ladder that moves with
  // density, and a hand-written 48 would be the one thing in the header that
  // does not answer a density change.
  const mark = (
    <span
      className="flex size-ctl-xl items-center justify-center rounded-control"
      style={{ background: `color-mix(in oklab, ${color} 14%, transparent)` }}
    >
      <ProjectIcon color={color} className="size-icon-lg" />
    </span>
  );

  return (
    <div className="mb-5">
      {readOnly ? (
        mark
      ) : (
        <Popover
          alignment="start"
          content={
            <div className="p-2">
              <ColorPicker value={project.color} onChange={(color) => onEdit({ color })} />
            </div>
          }
        >
          <button
            aria-label="Project colour"
            className="hover:ring-line-strong rounded-control outline-none hover:ring-1 focus-visible:ring-1 focus-visible:ring-accent/50"
          >
            {mark}
          </button>
        </Popover>
      )}
      <div className="mt-3 flex items-center gap-2">
        <input
          defaultValue={project.name}
          readOnly={readOnly}
          onBlur={(e) => {
            const next = e.target.value.trim();
            if (next && next !== project.name) onEdit({ name: next });
          }}
          // Same size as an issue title: both are the name of the document
          // you are looking at, and the overview was a step smaller for no
          // reason other than that it was written on a different day.
          className={cn(
            titleText({ level: "document" }),
            "min-w-0 flex-1 bg-transparent outline-none",
          )}
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
            onClick={() => onEdit({ archived: !project.archived })}
            variant="ghost"
            size="sm"
            tooltip={project.archived ? "Restore project" : "Archive project"}
            icon={
              project.archived ? (
                <ArchiveRestore className="size-icon-sm" />
              ) : (
                <Archive className="size-icon-sm" />
              )
            }
          />
        )}
      </div>
    </div>
  );
}

/**
 * Lead, team and the planned window — IN THE DOCUMENT, which is the correction.
 *
 * These four writes lived in `ProjectRail`, and `railOpen` is a preference that
 * persists. Shut it once and a project's Overview could no longer show or set
 * anything about the project: an empty one drew a name, a placeholder and an
 * update composer, and not one fact. Linear puts a project's properties on the
 * page for this reason — collapsing a panel should cost you navigation, never
 * facts — and it is why the rail keeps the milestone scope and gives these up.
 * A property is a fact the document states; a filter is a control the shell
 * holds.
 *
 * One wrapping run of `quiet` controls rather than a labelled column. The rail
 * needed the terms because a narrow stack of bare values does not scan; a row
 * reading left to right does, and `Lead Set lead Team Set team` is a sentence
 * built out of a table — the same thing the issue document's compact strip
 * already found out. The one term kept is the row's own, which names the run.
 */
function Properties({
  project,
  members,
  teams,
  readOnly,
  onEdit,
}: {
  project: ProjectDto;
  members: MemberDto[];
  teams: TeamDto[];
  readOnly: boolean;
  onEdit: (patch: Record<string, string | boolean | null>) => void;
}) {
  const lead = members.find((m) => m.key === project.lead);
  const team = teams.find((candidate) => candidate.id === project.team);

  return (
    <div className="flex items-start gap-3 text-sm">
      <span className="text-mute flex min-h-ctl-md shrink-0 items-center">Properties</span>
      {/* 12px between controls, against the `quiet` tone's own `-mx-1` bleed on
          each side — so the hover pills stand 4px apart and the words 12,
          which is the pitch Linear's row reads at. At the 8px a rail row uses
          the bleed would close the gap entirely and two chips would touch. */}
      <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-0.5">
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
          onPick={(id) => onEdit({ lead: id === "none" ? "none" : id })}
        />
        {/* Which team owns this project — the write the sidebar's grouping
            reads. Before the dates because it decides where the project *is*,
            and the dates only decide when it happens.

            Offered even with no teams yet, so the control is where you learn
            the concept exists; picking "No team" is the same write as clearing
            a lead. */}
        <Combobox
          tone="quiet"
          label="Team"
          disabled={readOnly}
          value={team ? { id: team.id, label: team.name, hint: team.key } : null}
          face={
            team ? undefined : (
              <>
                <UsersRound className="text-mute size-icon-sm shrink-0" />
                <span className="text-mute">{teams.length === 0 ? "No teams yet" : "Set team"}</span>
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
          onPick={(id) => onEdit({ team: id === "none" ? "" : id })}
        />
        <DatePicker
          tone="quiet"
          value={toInput(project.start_date)}
          disabled={readOnly}
          placeholder="Add start date"
          ariaLabel="Start date"
          onChange={(next) => onEdit({ start: next ?? "none" })}
        />
        <DatePicker
          tone="quiet"
          value={toInput(project.target_date)}
          disabled={readOnly}
          placeholder="Add target date"
          ariaLabel="Target date"
          onChange={(next) => onEdit({ target: next ?? "none" })}
        />
      </div>
    </div>
  );
}

/** The overview paragraph — the same live document the issue body is, so the two
 *  surfaces are written the same way as well as read the same way.
 *
 *  Shared with the milestone bodies below: they are the same kind of prose in the
 *  same document, so they get the same editor and the same commit-on-blur rule
 *  rather than a second copy of this dirty-tracking dance. */
function Description({
  value,
  readOnly,
  placeholder,
  empty,
  className = "min-h-16 py-2",
  onSave,
}: {
  value: string;
  readOnly: boolean;
  placeholder: string;
  /** Read-only text when there is no body. Omit to render nothing — a milestone
   *  with no prose should take no room, where a project with none has a page to
   *  explain. */
  empty?: string;
  /** The editor's resting size. A project's body reserves a paragraph because
   *  the page is about writing it; a milestone's reserves nothing and takes the
   *  height of its own empty line, because an undescribed stage is normal and
   *  four in a row should not open a field of blank space above the Updates.
   *
   *  No `min-h-*` rung here on purpose: the ladder in `designSystem.test.ts` is
   *  for CONTROL heights, and a prose body is a measure, not a control. Padding
   *  plus the editor's own line is the honest way to say "one line". */
  className?: string;
  onSave: (v: string) => void;
}) {
  const [, setDraft] = useState(value);
  const dirty = useRef(false);

  if (readOnly) {
    if (!value && !empty) return null;
    return (
      <div className={className}>
        {value ? <Markdown text={value} /> : <span className="text-mute">{empty}</span>}
      </div>
    );
  }

  return (
    <MarkdownEditor
      value={value}
      placeholder={placeholder}
      className={className}
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
 * The milestones as *document* — the second face of the same records the rail
 * indexes, and the duplication is the point.
 *
 * The rail answers "where are we": one scannable line per stage, and a click
 * that narrows the issues. This answers "what is this stage": the prose that
 * explains what the milestone means, in the column where a project's other
 * prose already lives. Linear carries both for the same reason, and the two are
 * one resource under one key, so they can never disagree.
 *
 * It draws nothing until a project has milestones — the rail's empty state is
 * the one that teaches, and two empty states for one absence is one too many.
 */
function MilestoneDocument({
  spaceId,
  projectId,
  readOnly,
  onError,
}: {
  spaceId: string;
  projectId: string;
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const store = useProjectViewerStore();
  const resource = useProjectMilestones(spaceId, projectId);
  const milestones = resource.data ?? [];

  const edit = async (id: string, patch: { name?: string; description?: string }) => {
    try {
      await rpc(spaceId, { cmd: "milestone_set", project: projectId, milestone: id, ...patch });
      await store.ensureMilestones(spaceId, projectId, true);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  if (milestones.length === 0) return null;
  return (
    <section className="mt-8">
      <h2 className={CAPTION}>Milestones</h2>
      <ol className="flex flex-col">
        {milestones.map((m) => (
          <MilestoneSection
            key={m.id}
            milestone={m}
            readOnly={readOnly}
            onRename={(name) => void edit(m.id, { name })}
            onDescribe={(description) => void edit(m.id, { description })}
          />
        ))}
      </ol>
      {resource.nextCursor && (
        <div className="flex justify-center py-3">
          <Button
            onClick={() => void resource.loadMore()}
            label="Load more milestones"
            variant="ghost"
            size="sm"
          />
        </div>
      )}
    </section>
  );
}

/**
 * One milestone as a section of the project document.
 *
 * The heading carries the glyph, the name, and the progress — the same three
 * facts the rail row carries, because they are the same milestone, but spelled
 * for a document: `2 issues · 50%` rather than `50% of 2`, since prose has room
 * for the noun and a rail does not.
 *
 * Collapsible and open by default. Nine stages with bodies is a long page, and
 * the fold is what makes the list navigable once it is; hiding them by default
 * would hide the writing this section exists to show.
 */
function MilestoneSection({
  milestone: m,
  readOnly,
  onRename,
  onDescribe,
}: {
  milestone: MilestoneDto;
  readOnly: boolean;
  onRename: (name: string) => void;
  onDescribe: (description: string) => void;
}) {
  const [open, setOpen] = useState(true);
  const bodyId = useId();
  const pct = milestonePercent(m);

  return (
    <li className="border-line/70 border-b py-3 last:border-b-0">
      <div className="group/sec flex items-center gap-2">
        <button
          type="button"
          onClick={() => setOpen((was) => !was)}
          aria-expanded={open}
          aria-controls={bodyId}
          aria-label={open ? `Collapse ${m.name}` : `Expand ${m.name}`}
          className="text-mute hover:text-fg -ml-5 flex size-icon-md shrink-0 items-center justify-center rounded-control opacity-0 outline-none group-hover/sec:opacity-100 focus-visible:opacity-100 focus-visible:ring-1 focus-visible:ring-accent/50"
        >
          <ChevronRight className={cn("size-icon-sm transition-transform", open && "rotate-90")} />
        </button>
        <MilestoneIcon progress={milestoneProgress(m)} />
        <input
          defaultValue={m.name}
          readOnly={readOnly}
          onBlur={(e) => {
            const next = e.target.value.trim();
            if (next && next !== m.name) onRename(next);
            else e.target.value = m.name;
          }}
          // Keyed on the name so an edit from the rail, the CLI or a peer lands
          // here: an uncontrolled input keeps its own value forever otherwise,
          // and this heading would quietly disagree with the row above it.
          key={m.name}
          className="min-w-0 flex-1 bg-transparent text-base font-semibold tracking-tight outline-none"
          aria-label={`Milestone name — ${m.name}`}
        />
        <span className="text-mute shrink-0 text-xs tabular-nums">
          {m.total === 1 ? "1 issue" : `${m.total} issues`} · {pct}%
        </span>
      </div>
      {open && (
        <div id={bodyId} className="pl-6">
          <Description
            value={m.description ?? ""}
            readOnly={readOnly}
            placeholder="What is this stage — goal, scope, what lands."
            className="py-1"
            onSave={onDescribe}
          />
        </div>
      )}
    </li>
  );
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
  const [composing, setComposing] = useState(false);

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

  // Empty and untouched, the composer is the wrong shape. A live textarea, a
  // caption, "No updates yet." and a disabled primary button say one absence
  // four times — and the disabled button, being the only filled thing on the
  // page, says it loudest. A project with posts in it is the other case: there
  // the composer stands open above its own feed, which is what a feed wants.
  const empty = updates != null && updates.length === 0;
  const composerOpen = !empty || composing;

  return (
    <section className="mt-8">
      <h2 className={CAPTION}>Updates</h2>

      {!readOnly && empty && !composing && (
        <button
          type="button"
          onClick={() => setComposing(true)}
          className="control-hover-outline border-line hover:border-line-strong text-mute hover:text-fg flex w-full items-center justify-center gap-2 rounded-surface border px-3 py-6 text-sm outline-none transition-colors focus-visible:ring-1 focus-visible:ring-accent/50"
        >
          <SquarePen className="size-icon-sm" aria-hidden />
          Write the first project update
        </button>
      )}

      {!readOnly && composerOpen && (
        <div className="control-hover-outline border-line focus-within:border-line-strong mb-4 rounded-surface border bg-[var(--field-bg)] p-3 transition-colors">
          <textarea
            value={draft}
            rows={2}
            autoFocus={composing}
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
            {/* `sm`, to stand 28px like the picker beside it.
                Astryx's `md` button is 32px and our control ladder's `md` is
                28, so a `size="md"` on each put two controls of the same
                nominal size in one row at two different heights. Where those
                two vocabularies meet in a single row, the row's height is the
                one that has to win — the composer can keep a 32px commit
                because its footer is a region away from the pill row. */}
            <Button
              className="ml-auto"
              isDisabled={!draft.trim() || posting}
              isLoading={posting}
              onClick={() => void post()}
              label="Post update"
              variant="primary"
              size="sm"
            />
          </div>
        </div>
      )}

      {resource.error != null && <p className="text-danger text-sm">Couldn't load updates.</p>}
      {!updates && resource.error == null && <p className="text-mute text-sm">Loading…</p>}
      {readOnly && empty && <p className="text-mute text-sm">No updates yet.</p>}
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
      {resource.nextCursor && (
        <div className="flex justify-center py-3">
          <Button
            onClick={() => void resource.loadMore()}
            label="Load more updates"
            variant="ghost"
            size="sm"
          />
        </div>
      )}
    </section>
  );
}
