import { useEffect, useMemo, useState } from "react";
import { Activity as ActivityIcon, AlertTriangle } from "lucide-react";

import { rpc } from "../api";
import { describeEventRich, type EventPhraseContext, type NameResolver } from "../core/activity";
import { groupActivity } from "../core/inbox";
import { boundedTail, indexBy } from "../core/performance";
import type { ActivityEvent, MemberDto, WorkflowState } from "../types";
import { EmptyState, LoadingState } from "./AppState";
import { memberName } from "./Avatar";
import { when } from "./time";
import { Button } from "@astryxdesign/core";
import { interactiveRow } from "./primitives";

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
  projectDocIds,
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
  projectDocIds?: ReadonlySet<string>;
  projectName?: string;
  onError: (m: string) => void;
  onOpen: (reff: string) => void;
}) {
  const [events, setEvents] = useState<ActivityEvent[] | null>(null);
  const [visibleCount, setVisibleCount] = useState(80);
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
    }),
    // `resolveName` closes over `memberByKey`, which is the real dependency.
    [memberByKey, states],
  );
  const scopedEvents = useMemo(
    () => projectDocIds
      ? events?.filter((event) => event.doc_id !== null && projectDocIds.has(event.doc_id)) ?? null
      : events,
    [events, projectDocIds],
  );

  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const r = await rpc(spaceId, { cmd: "activity", since: 0 });
        if (alive && r.kind === "activity") setEvents(r.events);
      } catch (e) {
        if (alive) onError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [spaceId, revision, onError]);

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
      <EmptyState
        icon={<ActivityIcon className="size-icon-lg" />}
        title={projectName ? `No activity in ${projectName}` : "No activity yet"}
        body={projectName ? "Changes to this project's issues will appear here." : "Changes made in this session will appear here."}
      />
    );
  }

  return (
    <ul className="min-h-0 flex-1 overflow-y-auto">
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
          <span className="text-mute w-20 shrink-0 truncate font-mono text-xs tabular-nums">
            {e.reff}
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
