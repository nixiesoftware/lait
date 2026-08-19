import { useCallback, useEffect, useMemo, useState } from "react";
import { Activity as ActivityIcon, AlertTriangle } from "lucide-react";

import { rpc } from "../api";
import { describeEventRich, type EventPhraseContext, type NameResolver } from "../core/activity";
import { groupActivity } from "../core/inbox";
import { boundedTail, indexBy } from "../core/performance";
import type { ActivityEvent, MemberDto, WorkflowState, WorldPublicationId } from "../types";
import { EmptyState, LoadingState } from "./AppState";
import { memberName } from "./Avatar";
import { when } from "./time";
import { Button } from "@astryxdesign/core";
import { interactiveRow } from "./primitives";

const publicationKey = (source: WorldPublicationId) => {
  const hex = (bytes: readonly number[]) => bytes
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  return [
    hex(source.publication.manifest_root),
    hex(source.publication.implementation_digest),
    hex(source.publication.extractor_schema_digest),
    source.materialization,
  ].join(":");
};

const activityKey = (event: ActivityEvent) =>
  event.cursor ?? `${event.doc_id ?? "space"}:${event.seq}`;

const appendUniqueActivity = (
  current: readonly ActivityEvent[],
  incoming: readonly ActivityEvent[],
) => {
  const rows = new Map(current.map((event) => [activityKey(event), event]));
  for (const event of incoming) rows.set(activityKey(event), event);
  return [...rows.values()];
};

/**
 * The space feed.
 *
 * Pulled, never pushed: the doorbell only sets `activity_advanced` — it carries no
 * rows — so this re-reads when it rings (S§7.5). That is the same discipline as
 * every other surface here and the reason a client can never render an event the
 * daemon didn't derive.
 *
 * One `Request` = one commit = one row (S§7.1), so a mutation that moved three
 * fields is *one* entry with three changes rather than three entries. The feed's
 * granularity is the command surface's, by design.
 *
 * **Who did it is `core/activity.ts`'s call, not this file's.** This feed is the
 * per-session ring: local ops stamp their own key, and a remote change arrives as one
 * synthetic `synced` event stamped with *this* node's key — so `synced` is rendered
 * without a name, or the feed would credit you with a teammate's edit. Names for the
 * rest are resolved from the member list, same rule as the durable per-issue history.
 */
export function Activity({
  spaceId,
  members,
  states,
  revision,
  projectIssues,
  projectName,
  onError,
  onOpen,
}: {
  spaceId: string;
  members: MemberDto[];
  /** For naming the states a change moved between. Without these the feed
   *  prints the engine's ids — `in_progress`, not "In Progress". */
  states: WorkflowState[];
  revision: number;
  /** The project's issues by doc id, each with the name a person calls it.
   *  Scopes the feed to this project *and* names what it scoped. */
  projectIssues?: ReadonlyMap<string, string | null>;
  projectName?: string;
  onError: (m: string) => void;
  onOpen: (reff: string) => void;
}) {
  const [events, setEvents] = useState<ActivityEvent[] | null>(null);
  const [visibleCount, setVisibleCount] = useState(80);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [publication, setPublication] = useState<WorldPublicationId | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const memberByKey = useMemo(
    () => indexBy(members, (member) => member.key),
    [members],
  );
  const resolveName: NameResolver = (key) =>
    memberName(key, memberByKey.get(key));
  /** The same context the issue history builds. Held in one object so the two
   *  feeds cannot drift into resolving different halves of the same event. */
  const phraseCtx: EventPhraseContext = useMemo(
    () => ({
      resolveName,
      stateName: (id) => states.find((state) => state.id === id)?.name ?? null,
      // Names a link or parent target — "marked this issue as blocking EXEC-3"
      // rather than a truncated doc id. The issue history has always supplied
      // this; the feed passed only half the context.
      issueLabel: (docId) => projectIssues?.get(docId) ?? null,
    }),
    // `resolveName` closes over `memberByKey`, which is the real dependency.
    [memberByKey, states, projectIssues],
  );
  const scopedEvents = useMemo(
    () => projectIssues
      ? events?.filter((event) => event.doc_id !== null && projectIssues.has(event.doc_id)) ?? null
      : events,
    [events, projectIssues],
  );

  const load = useCallback(
    async (
      alive: () => boolean,
      cursor: string | null = null,
      pinned: WorldPublicationId | null = null,
      append = false,
    ) => {
      if (append) setLoadingMore(true);
      try {
        const r = await rpc(spaceId, {
          cmd: "activity",
          page: { limit: 100, cursor },
        });
        if (!alive() || r.kind !== "activity") return;
        if (pinned && publicationKey(r.page.publication) !== publicationKey(pinned)) {
          throw new Error("Activity continuation crossed publications; refresh the feed");
        }
        setEvents((current) => append
          ? appendUniqueActivity(current ?? [], r.page.items)
          : r.page.items);
        setNextCursor(r.page.next_cursor ?? null);
        setPublication(r.page.publication);
      } catch (e) {
        if (alive()) onError(e instanceof Error ? e.message : String(e));
      } finally {
        if (alive() && append) setLoadingMore(false);
      }
    },
    [spaceId, onError],
  );

  useEffect(() => {
    let alive = true;
    setEvents(null);
    setNextCursor(null);
    setPublication(null);
    setVisibleCount(80);
    void load(() => alive);
    return () => {
      alive = false;
    };
  }, [load, revision]);

  if (!scopedEvents) {
    return (
      <LoadingState
        title="Loading activity"
        body="Reading the local session history."
      />
    );
  }
  if (scopedEvents.length === 0) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <EmptyState
          icon={<ActivityIcon className="size-icon-lg" />}
          title={projectName ? `No activity in ${projectName}` : "No activity yet"}
          body={projectName ? "No matching changes are present on this page." : "Changes made in this session will appear here."}
        />
        {nextCursor && publication && (
          <Button
            onClick={() => void load(() => true, nextCursor, publication, true)}
            isLoading={loadingMore}
            label="Load older activity"
            variant="ghost"
            size="sm"
          />
        )}
      </div>
    );
  }

  return (
    <ul className="min-h-0 flex-1 overflow-y-auto">
      {nextCursor && publication && (
        <li className="border-line/60 border-b p-2 text-center">
          <Button
            onClick={() => void load(() => true, nextCursor, publication, true)}
            isLoading={loadingMore}
            label="Load older activity"
            variant="ghost"
            size="sm"
          />
        </li>
      )}
      {scopedEvents.length > visibleCount && (
        <li className="border-line/60 border-b p-2 text-center">
          <Button
            onClick={() => setVisibleCount((count) => count + 80)}
            label={`Show ${Math.min(80, scopedEvents.length - visibleCount)} older changes`}
            variant="ghost"
            size="sm"
          />
        </li>
      )}
      {/* Newest first: the feed answers "what just happened", not "what happened". */}
      {groupActivity([...boundedTail(scopedEvents, visibleCount)].reverse()).map((group) => {
        const e = group.events[0]!;
        return (
        <li
          key={`${e.seq}-${group.events.length}`}
          onClick={() => onOpen(e.reff)}
          onKeyDown={(event) => {
            if (event.key === "Enter") onOpen(e.reff);
          }}
          tabIndex={0}
          className={`${interactiveRow({ size: "lg" })} flex items-start gap-3 px-4 py-2.5`}
        >
          {/* The name a person calls this issue. The engine's `ActivityEvent`
              carries no key alias — only `reff` and `doc_id` — so the board
              supplies it, which it can do completely: this feed only renders
              under a board, so every event it can draw belongs to a doc the
              board holds. Falls back to the ref when a doc has left the
              projection entirely, which is honest rather than blank. */}
          <span
            className="text-mute w-20 shrink-0 truncate font-mono text-xs tabular-nums"
            title={e.reff}
          >
            {(e.doc_id && projectIssues?.get(e.doc_id)) || e.reff}
          </span>
          <span className="min-w-0 flex-1">
            {group.events.map((event, index) => (
              <span key={event.seq} className={index ? "mt-1 block" : "block"}>
                <Line event={event} ctx={phraseCtx} />
              </span>
            ))}
          </span>
          {/* A concurrent overwrite is worth flagging but never worth blocking on
              (A§9): last-writer-wins already resolved it; you just get told. */}
          {e.collision && (
            <AlertTriangle
              className="text-warn size-icon-sm shrink-0"
              aria-label="Concurrent overwrite detected"
            />
          )}
          <span className="flex shrink-0 items-center gap-2">
            {group.events.length > 1 && <span className="bg-raised text-mute rounded-mark px-1.5 text-2xs">{group.events.length} changes</span>}
            <time className="text-mute text-xs">{when(e.ts)}</time>
          </span>
        </li>
      )})}
    </ul>
  );
}

/**
 * One event, in the words a person would use.
 *
 * `describeEventRich`, not `describeEvent` + `describeChanges`. Both renderers
 * have always been here and the issue history picked the right one; the space
 * feed picked the raw pair, so it printed the protocol at you:
 * `status: backlog → in_progress`, `duedate: — → Aug 21, 2026`, and — under a
 * line that already said "you added an assignee" — the 64-hex device key of
 * you. The rich renderer says "moved from Backlog to In Progress" and
 * "assigned you", from the same event and the same member list.
 *
 * No separate `changes` span any more: the rich phrase already *is* the
 * changes, so appending them printed everything twice.
 */
function Line({ event, ctx }: { event: ActivityEvent; ctx: EventPhraseContext }) {
  const { actor, phrase } = describeEventRich(event, ctx);
  return (
    <span>
      {/* No name when we have no honest one — see core/activity.ts. */}
      {actor && <span className="font-medium">{actor} </span>}
      <span className="text-dim">{phrase}</span>
      {/* `created` and `commented` are the two kinds that carry words of their
          own; everything else is phrased above. */}
      {(event.kind === "created" || event.kind === "commented") && event.text && (
        <span className="text-mute ml-2 text-xs">{event.text}</span>
      )}
    </span>
  );
}
