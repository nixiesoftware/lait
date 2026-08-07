import { PRIORITY_ORDER, type ActivityEvent, type FieldChange } from "../types";

/**
 * What an activity event *says*, and who it may name.
 *
 * There are now **two** feeds behind `ActivityEvent`, and they attribute
 * differently — conflating them is what rots this file:
 *
 * - **Per-issue history** (`Request::History`) reads the issue's oplog **on disk**
 *   (`engine::history`). Each change carries the real committer in `actor` (an
 *   ed25519 key) and a real `ts`; it survives daemon restarts and attributes a
 *   *teammate's* change to the teammate. `actor_nick` is **empty** here — the daemon
 *   no longer resolves it, so the client must resolve `actor` itself. There is no
 *   `synced` event in this feed; you see the actual ops.
 * - **Space activity** (`Request::Activity`) is still the per-session ring. A
 *   remote change arrives as one synthetic `synced` event stamped with the *local*
 *   node's key. That key is not the author, so `synced` must be rendered **without a
 *   name** — the exact non-goal-6 trap (in-doc attribution is advisory), which the
 *   inbox already avoids by never guessing an actor for non-comments.
 *
 * So attribution is one rule that covers both: resolve `actor` (a key) through the
 * caller's resolver — except `synced`, which has no honest name. The resolver lives
 * in the UI because that is where the member list is; this module stays dumb about
 * *how* a key becomes a name and only decides *whether* there is one to show.
 *
 * Most kinds also carry no words of their own (`text: ""`, `changes: []` for
 * `assigned`/`labeled`/`moved`/…), so the phrasing comes from here — the daemon
 * never wrote any.
 */

/** Resolve an actor key to a display name (member alias, "you", or a short key). */
export type NameResolver = (key: string) => string;

/** Present tense, third person, no trailing punctuation. */
const PHRASE: Readonly<Record<string, string>> = {
  created: "created the issue",
  edited: "edited",
  started: "started it",
  finished: "finished it",
  stopped: "stopped it",
  moved: "moved it",
  assigned: "added an assignee",
  unassigned: "removed an assignee",
  labeled: "changed labels",
  commented: "commented",
  deleted: "deleted the issue",
  member_added: "added a member",
  member_removed: "removed a member",
  // No name is attached to this one — see the module note.
  synced: "changed by a peer",
};

/**
 * The one kind whose actor is a real, in-document claim.
 *
 * A comment's author is written into the CRDT by whoever wrote it, so it survives
 * sync and means something on every node. History's per-op `actor` is now also a
 * genuine claim (it travels in the commit message), but it is still advisory
 * (non-goal 6) — which is fine, because attribution here is a display nicety, not an
 * authorization decision.
 */
const ATTRIBUTABLE = new Set(["commented"]);

/**
 * Who and what an event says.
 *
 * `actor` is `null` whenever there is no honest name — a `synced` event, or an event
 * with no actor at all. Callers render the phrase alone in that case rather than
 * substituting "someone", which would imply we know there was a someone and merely
 * lost the name.
 */
export function describeEvent(
  e: ActivityEvent,
  resolveName?: NameResolver,
): { actor: string | null; phrase: string } {
  const phrase = PHRASE[e.kind] ?? e.kind;

  // A remote change in the *activity* feed is stamped with the local node's key, so
  // there is no honest author to name — the whole reason this special case exists.
  if (e.kind === "synced") return { actor: null, phrase };

  // Otherwise `actor` is the real committer (history) or this node (its own ops).
  // Resolve the key to a name; the caller owns the fallback chain (alias → you →
  // short key), because it is the caller that holds the member list.
  if (e.actor && resolveName) return { actor: resolveName(e.actor), phrase };

  // No resolver available: the daemon-resolved nick is the only remaining signal,
  // and it is empty in the history feed — so this yields `null` there, which is
  // honest (a name we cannot supply) rather than wrong.
  const nick = e.actor_nick?.trim();
  return { actor: nick || null, phrase };
}

/** Whether this event's author is a claim the document itself carries. */
export const isAttributable = (e: ActivityEvent): boolean => ATTRIBUTABLE.has(e.kind);

/**
 * Resolvers the rich phrasing can borrow from the caller. All optional: a missing
 * resolver degrades to the raw id, never to a wrong name.
 */
export interface EventPhraseContext {
  resolveName?: NameResolver;
  /** Workflow state id → display name ("in_progress" → "In Progress"). */
  stateName?: (id: string) => string | null | undefined;
  /** Doc id → "KEY-7 Title" label, for link/parent targets. */
  issueLabel?: (docId: string) => string | null | undefined;
}

const shortId = (id: string) => `${id.slice(0, 8)}…`;
const cap = (s: string) => (s ? s[0]!.toUpperCase() + s.slice(1) : s);

/** The kinds whose `changes` describe issue-field edits (WorkState events carry
 *  a status change and sometimes a self-(un)assignment rider). */
export const EDIT_KINDS: ReadonlySet<string> = new Set(["edited", "started", "finished", "stopped"]);

/**
 * One sentence per change, in the words a person would use — "moved from Backlog
 * to Todo", not "status: backlog → in_progress". Returns `null` for a no-op
 * change, with one exception: a description edit travels as `— → —` (the daemon
 * doesn't ship the two texts), and that is still a real edit worth a clause.
 */
function editClause(c: FieldChange, ctx: EventPhraseContext, actorKey?: string | null): string | null {
  const state = (id: string) => ctx.stateName?.(id) ?? id;
  if (c.field === "description") return "updated the description";
  if ((c.from ?? "—") === (c.to ?? "—")) return null;
  switch (c.field) {
    case "status":
      if (!c.to) return "cleared the status";
      return c.from ? `moved from ${state(c.from)} to ${state(c.to)}` : `set status to ${state(c.to)}`;
    case "priority": {
      const from = c.from ?? "none";
      const to = c.to ?? "none";
      if (to === "none") return "removed the priority";
      if (from === "none") return `set priority to ${cap(to)}`;
      const dir =
        PRIORITY_ORDER.indexOf(to as (typeof PRIORITY_ORDER)[number]) >
        PRIORITY_ORDER.indexOf(from as (typeof PRIORITY_ORDER)[number])
          ? "raised"
          : "lowered";
      return `${dir} priority from ${cap(from)} to ${cap(to)}`;
    }
    case "title":
      return c.to ? `renamed the issue to “${c.to.length > 60 ? `${c.to.slice(0, 60)}…` : c.to}”` : "cleared the title";
    case "duedate":
      if (!c.to) return "removed the due date";
      return `${c.from ? "moved" : "set"} the due date to ${renderDue(c.to)}`;
    case "estimate":
      return c.to ? `set the estimate to ${c.to}` : "removed the estimate";
    case "assignees": {
      // The WorkState self-assignment rider ("@me"), a key that IS the actor's,
      // or someone else's. The middle case is the one that used to read badly:
      // the resolver answers "you" for your own key, so a self-assignment came
      // out as "you assigned you" — grammatical, and not what anyone says.
      const who = (k: string) =>
        k === "@me" || (actorKey !== null && actorKey !== undefined && k === actorKey)
          ? "themselves"
          : (ctx.resolveName?.(k) ?? shortId(k));
      return c.to ? `assigned ${who(c.to)}` : c.from ? `unassigned ${who(c.from)}` : null;
    }
    default:
      return `${c.field}: ${c.from ?? "—"} → ${c.to ?? "—"}`;
  }
}

/**
 * `describeEvent`, but phrased as the sentence a person would write — the way
 * Linear narrates its timeline. Attribution is `describeEvent`'s, unchanged
 * (including the `synced` no-name rule); only the words differ:
 *
 * - Field edits name the field's *display* values ("moved from Backlog to Todo",
 *   "raised priority from High to Urgent") via the caller's resolvers.
 * - Relation events name the other issue ("marked this issue as blocking NIX-101 …")
 *   when `issueLabel` can, and fall back to a short doc id when it can't.
 * - `created` says only that — the durable event lists every initial field, and
 *   reciting them buries the one fact that matters.
 */
export function describeEventRich(
  e: ActivityEvent,
  ctx: EventPhraseContext = {},
): { actor: string | null; phrase: string } {
  const { actor, phrase: plain } = describeEvent(e, ctx.resolveName);
  const target = (docId: string) => ctx.issueLabel?.(docId) ?? shortId(docId);

  const phrase = (() => {
    if (e.kind === "created") return "created the issue";
    if (EDIT_KINDS.has(e.kind)) {
      const clauses = e.changes
        .map((c) => editClause(c, ctx, e.actor))
        .filter((s): s is string => s !== null);
      return clauses.length > 0 ? clauses.join(", ") : plain;
    }
    switch (e.kind) {
      case "assigned":
      case "unassigned": {
        const names = e.changes
          .map((c) => (e.kind === "assigned" ? c.to : c.from))
          .filter((k): k is string => !!k)
          .map((k) =>
            k === "@me" || (e.actor !== null && k === e.actor)
              ? "themselves"
              : (ctx.resolveName?.(k) ?? shortId(k)),
          );
        if (names.length === 0) return plain;
        return `${e.kind} ${names.join(", ")}`;
      }
      case "linked":
      case "unlinked": {
        // `text` is "{kind} {target_doc_id}" — see `IssueIntent::Link`.
        const space = e.text.indexOf(" ");
        if (space < 0) return plain;
        const label = target(e.text.slice(space + 1));
        const add = e.kind === "linked";
        switch (e.text.slice(0, space)) {
          case "blocks":
            return add ? `marked this issue as blocking ${label}` : `no longer blocks ${label}`;
          case "relates":
            return `${add ? "added" : "removed"} related issue ${label}`;
          case "duplicates":
            return add
              ? `marked this issue as a duplicate of ${label}`
              : `unmarked ${label} as a duplicate`;
          default:
            return `${add ? "linked" : "unlinked"} ${label}`;
        }
      }
      case "parented":
        return !e.text || e.text === "unparented"
          ? "removed the parent"
          : `set the parent to ${target(e.text)}`;
      case "milestoned":
        return !e.text || e.text === "none"
          ? "removed the milestone"
          : `set the milestone to ${e.text}`;
      case "cycled":
        return !e.text || e.text === "none" ? "removed the cycle" : `set the cycle to ${e.text}`;
      case "attached":
        return e.text ? `attached ${e.text}` : "attached a file";
      case "detached":
        return e.text ? `removed attachment ${e.text}` : "removed an attachment";
      default:
        return plain;
    }
  })();

  return { actor, phrase };
}

/**
 * `status: backlog → done`, for the events that populate `changes`.
 *
 * No-op changes are dropped. The durable-history projection of a `created` event
 * lists *every* field, including ones that went `— → —` (a container that was
 * created empty: `comments`, an empty `description`). Rendering those is noise that
 * makes the one real change ("→ backlog") hard to find, so a change whose before and
 * after read the same is omitted.
 *
 * `duedate` values are stored as unix seconds; the history phrase renders them
 * as the calendar date they name (UTC — same convention as `ui/time.dueLabel`),
 * because "1784937600" is a fact and "Jul 25" is information.
 */
function renderDue(v: string): string {
  const ts = Number(v);
  if (Number.isFinite(ts) && ts > 0) {
    return new Date(ts * 1000).toLocaleDateString(undefined, {
      timeZone: "UTC",
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
  return v;
}

export function describeChanges(e: ActivityEvent): string {
  const render = (field: string, v: string | null): string => {
    if (v === null) return "—";
    if (field === "duedate") return renderDue(v);
    return v;
  };
  return e.changes
    .filter((c) => (c.from ?? "—") !== (c.to ?? "—"))
    .map((c) => `${c.field}: ${render(c.field, c.from ?? null)} → ${render(c.field, c.to ?? null)}`)
    .join(", ");
}
