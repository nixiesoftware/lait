import type { ActivityEvent, InboxEntry } from "../types";

export type InboxKind = "assigned" | "comment" | "status";

export interface InboxPreferences {
  kinds: Record<InboxKind, boolean>;
  grouping: "cause" | "chronological";
  snoozed: Record<string, number>;
}

export const defaultInboxPreferences = (): InboxPreferences => ({
  kinds: { assigned: true, comment: true, status: true },
  grouping: "cause",
  snoozed: {},
});

export const inboxEntryKey = (entry: InboxEntry): string =>
  `${entry.doc_id}:${entry.ts}:${entry.kind}`;

export function visibleInboxEntries(
  entries: InboxEntry[],
  preferences: InboxPreferences,
  now: number,
): InboxEntry[] {
  return entries.filter((entry) => {
    const kind = entry.kind as InboxKind;
    return (preferences.kinds[kind] ?? true) && (preferences.snoozed[inboxEntryKey(entry)] ?? 0) <= now;
  });
}

export interface ActivityGroup {
  reff: string;
  events: ActivityEvent[];
}

/** Groups adjacent changes to the same issue made within a five-minute burst. */
export function groupActivity(events: ActivityEvent[]): ActivityGroup[] {
  const groups: ActivityGroup[] = [];
  for (const event of events) {
    const previous = groups.at(-1);
    const previousEvent = previous?.events.at(-1);
    if (
      previous &&
      previous.reff === event.reff &&
      previousEvent &&
      Math.abs(previousEvent.ts - event.ts) <= 5 * 60
    ) {
      previous.events.push(event);
    } else {
      groups.push({ reff: event.reff, events: [event] });
    }
  }
  return groups;
}

/** What each cause is called wherever the inbox is configured. */
export function inboxCauseLabel(kind: string): string {
  return (
    { assigned: "Assignments", comment: "Comments and mentions", status: "Status changes", chronological: "Latest" }[
      kind
    ] ?? "Other"
  );
}

const preferencesKey = (spaceId: string) => `lait.inbox-preferences:${spaceId}`;

/** Per-space, per-device inbox preferences — read by the Inbox on mount and by
 *  the Notifications settings page, which is the second place they are set. */
export function loadInboxPreferences(spaceId: string): InboxPreferences {
  try {
    const saved = JSON.parse(localStorage.getItem(preferencesKey(spaceId)) ?? "null") as Partial<InboxPreferences> | null;
    const defaults = defaultInboxPreferences();
    return saved
      ? { ...defaults, ...saved, kinds: { ...defaults.kinds, ...saved.kinds }, snoozed: saved.snoozed ?? {} }
      : defaults;
  } catch {
    return defaultInboxPreferences();
  }
}

export function persistInboxPreferences(spaceId: string, value: InboxPreferences): void {
  try {
    localStorage.setItem(preferencesKey(spaceId), JSON.stringify(value));
  } catch {
    // Memory remains authoritative for this page.
  }
}
