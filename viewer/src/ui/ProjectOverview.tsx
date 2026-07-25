import { useCallback, useEffect, useRef, useState } from "react";
import { Archive, ArchiveRestore, UserPlus, X } from "lucide-react";

import { rpc } from "../api";
import type { MemberDto, MilestoneDto, ProjectDto, ProjectUpdateDto } from "../types";
import { Avatar, memberName } from "./Avatar";
import { catalogColor } from "./colors";
import { ColorPicker } from "./ColorPicker";
import { DatePicker } from "./DatePicker";
import { Markdown } from "./Markdown";
import { MarkdownEditor } from "./MarkdownEditor";
import { Combobox } from "./Picker";
import { RailRow, RailSection } from "./layout";
import { Button, IconButton, PopoverContent } from "./primitives";
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
}: {
  spaceId: string;
  project: ProjectDto;
  members: MemberDto[];
  counts: { backlog: number; active: number; done: number; total: number };
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

  const lead = members.find((m) => m.key === project.lead);
  const { backlog, active, done, total } = counts;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="min-h-0 flex-1 overflow-y-auto p-6">
        <div className="mx-auto grid max-w-4xl gap-8 md:grid-cols-[minmax(0,1fr)_264px]">
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
                      className="hover:ring-line-strong rounded p-0.5 hover:ring-1"
                    >
                      <span
                        className="block size-4 rounded"
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
                  className="block size-4 rounded"
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
                <span className="border-line text-mute rounded border px-1.5 py-px text-2xs">
                  Archived
                </span>
              )}
              {!readOnly && (
                <IconButton
                  label={project.archived ? "Restore project" : "Archive project"}
                  onClick={() => void edit({ archived: !project.archived })}
                >
                  {project.archived ? (
                    <ArchiveRestore className="size-3.5" />
                  ) : (
                    <Archive className="size-3.5" />
                  )}
                </IconButton>
              )}
            </div>
            <Description
              value={project.description ?? ""}
              readOnly={readOnly}
              onSave={(description) => void edit({ description })}
            />
            <Milestones
              spaceId={spaceId}
              projectKey={project.key}
              readOnly={readOnly}
              onError={onError}
            />
            <Updates
              spaceId={spaceId}
              projectKey={project.key}
              members={members}
              readOnly={readOnly}
              onError={onError}
            />
          </div>

          {/* The same aside the issue page carries: no label column, an unset
              property reads as the verb that sets it, and captions group the
              runs. Two surfaces describing the same kind of object should not
              describe it in two different grammars. */}
          <div className="md:border-line flex flex-col gap-3 text-sm md:border-l md:pl-6">
            <RailSection>
            <RailRow label="Lead">
              <Combobox
                variant="property"
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
                      <UserPlus className="text-mute size-3.5 shrink-0" />
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
                variant="property"
                value={toInput(project.start_date)}
                disabled={readOnly}
                placeholder="Add start date"
                ariaLabel="Start date"
                onChange={(next) => void edit({ start: next ?? "none" })}
              />
            </RailRow>
            <RailRow label="Target date">
              <DatePicker
                variant="property"
                value={toInput(project.target_date)}
                disabled={readOnly}
                placeholder="Add target date"
                ariaLabel="Target date"
                onChange={(next) => void edit({ target: next ?? "none" })}
              />
            </RailRow>
            </RailSection>

            {/* Progress earns a caption rather than a row label: it is the one
                entry here that is read rather than set, and Linear's project
                rail gives it the same treatment. */}
            <RailSection title="Progress">
              <RailRow label="Progress">
                <div className="flex w-full flex-col gap-1">
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
                    {total === 0 ? "No issues yet" : `${done}/${total} done · ${active} active`}
                  </span>
                </div>
              </RailRow>
            </RailSection>
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
 */
function Milestones({
  spaceId,
  projectKey,
  readOnly,
  onError,
}: {
  spaceId: string;
  projectKey: string;
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const [milestones, setMilestones] = useState<MilestoneDto[] | null>(null);
  const [draft, setDraft] = useState("");
  const [target, setTarget] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);

  const load = useCallback(async () => {
    try {
      const r = await rpc(spaceId, { cmd: "milestone_list", project: projectKey });
      if (r.kind === "milestones") setMilestones(r.milestones);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  }, [spaceId, projectKey, onError]);

  useEffect(() => {
    void load();
  }, [load]);

  const add = async () => {
    const name = draft.trim();
    if (!name) return;
    setAdding(true);
    try {
      await rpc(spaceId, {
        cmd: "milestone_set",
        project: projectKey,
        name,
        target,
      });
      setDraft("");
      setTarget(null);
      await load();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await rpc(spaceId, { cmd: "milestone_set", project: projectKey, milestone: id, remove: true });
      await load();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  if (milestones !== null && milestones.length === 0 && readOnly) return null;
  return (
    <section className="mt-8">
      <h2 className="text-mute mb-3 text-2xs font-semibold tracking-wider uppercase">
        Milestones
      </h2>
      {milestones === null && <p className="text-mute text-sm">Loading…</p>}
      {milestones !== null && milestones.length === 0 && (
        <p className="text-mute mb-2 text-sm">No milestones yet.</p>
      )}
      <ol className="flex flex-col gap-2">
        {milestones?.map((m) => {
          const pct = m.total === 0 ? 0 : Math.round((m.done / m.total) * 100);
          return (
            <li
              key={m.id}
              className="border-line group flex items-center gap-3 rounded border px-3 py-2"
            >
              <div className="min-w-0 flex-1">
                <div className="flex items-baseline gap-2 text-sm">
                  <span className="text-ink truncate font-medium">{m.name}</span>
                  {m.target_date != null && (
                    <span className="text-mute text-xs">→ {toInput(m.target_date)}</span>
                  )}
                  <span className="text-mute ml-auto shrink-0 text-xs">
                    {m.done}/{m.total} · {pct}%
                  </span>
                </div>
                <span className="bg-line mt-1.5 block h-1 w-full overflow-hidden rounded-full">
                  <span className="bg-ok block h-full" style={{ width: `${pct}%` }} />
                </span>
              </div>
              {!readOnly && (
                <IconButton
                  label={`Remove milestone ${m.name}`}
                  className="opacity-0 group-hover:opacity-100"
                  onClick={() => void remove(m.id)}
                >
                  <X className="size-3.5" />
                </IconButton>
              )}
            </li>
          );
        })}
      </ol>
      {!readOnly && (
        <div className="mt-2 flex items-center gap-2">
          <input
            value={draft}
            placeholder="New milestone…"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && draft.trim()) void add();
            }}
            className="border-line focus:border-line-strong placeholder:text-mute min-w-0 flex-1 rounded border bg-transparent px-2 py-1 text-sm outline-none"
            aria-label="New milestone name"
          />
          <DatePicker
            variant="property"
            value={target}
            placeholder="Target"
            ariaLabel="Milestone target date"
            onChange={setTarget}
          />
          <Button variant="outline" disabled={!draft.trim() || adding} onClick={() => void add()}>
            Add
          </Button>
        </div>
      )}
    </section>
  );
}

/**
 * The project updates feed (SCOPE-1) — an append-only stream of status posts.
 *
 * Each update is an immutable record in the engine's grow-only `project_updates`
 * log (a catalog map, not a per-project doc: an update is authored once, so a
 * record is the honest shape and it needs no collaborative-text merge). This
 * posts via `project_update_post` and reads via `project_updates`; the doorbell
 * is not wired here, so it reloads after its own post.
 */
function Updates({
  spaceId,
  projectKey,
  members,
  readOnly,
  onError,
}: {
  spaceId: string;
  projectKey: string;
  members: MemberDto[];
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const [updates, setUpdates] = useState<ProjectUpdateDto[] | null>(null);
  const [draft, setDraft] = useState("");
  const [health, setHealth] = useState("");
  const [posting, setPosting] = useState(false);

  const load = useCallback(async () => {
    try {
      const r = await rpc(spaceId, { cmd: "project_updates", project: projectKey });
      if (r.kind === "updates") setUpdates(r.updates);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  }, [spaceId, projectKey, onError]);

  useEffect(() => {
    void load();
  }, [load]);

  const post = async () => {
    const body = draft.trim();
    if (!body) return;
    setPosting(true);
    try {
      await rpc(spaceId, { cmd: "project_update_post", project: projectKey, body, health });
      setDraft("");
      setHealth("");
      await load();
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
        <div className="border-line mb-4 rounded border p-3">
          <textarea
            value={draft}
            rows={2}
            placeholder="Post a status update — what changed, what's next…"
            onChange={(e) => setDraft(e.target.value)}
            className="placeholder:text-mute w-full resize-y bg-transparent text-sm outline-none"
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

      {!updates && <p className="text-mute text-sm">Loading…</p>}
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
