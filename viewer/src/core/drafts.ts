const PREFIX = "lait.draft";

export type DraftKind =
  | "new-title"
  | "new-body"
  | "new-fields"
  | "title"
  | "description"
  | "comment";

export function loadDraft(spaceId: string, subject: string, kind: DraftKind): string {
  try {
    return localStorage.getItem(key(spaceId, subject, kind)) ?? "";
  } catch {
    return "";
  }
}

export function saveDraft(spaceId: string, subject: string, kind: DraftKind, value: string): void {
  try {
    const k = key(spaceId, subject, kind);
    if (value) localStorage.setItem(k, value);
    else localStorage.removeItem(k);
  } catch {
    // Draft persistence is private convenience; editing must still work when
    // storage is unavailable or disabled.
  }
}

export function clearDraft(spaceId: string, subject: string, kind: DraftKind): void {
  saveDraft(spaceId, subject, kind, "");
}

/**
 * The composer's non-prose state — the pills, not the words.
 *
 * Title and body were the only things a draft kept, so closing the composer and
 * reopening it restored what you had *typed* and silently dropped the status,
 * priority, labels, assignees, due date and project you had *set*. That was
 * already wrong. Expanding the composer onto its own route makes it load-
 * bearing: the hop remounts the component, and a draft that loses half itself
 * on the way is not a draft.
 *
 * Every field is optional on read. A stored blob is whatever the build that
 * wrote it knew about, so an older or newer shape degrades to "this field was
 * not set" rather than throwing away the whole draft — see {@link loadFields}.
 */
export interface DraftFields {
  project?: string;
  status?: string;
  priority?: string;
  due?: string;
  labels?: string[];
  assignees?: string[];
}

/**
 * Read the composer's fields, treating anything unreadable as no draft at all.
 *
 * A draft is a convenience, and the one thing it must never do is stop the
 * composer opening. Corrupt JSON, a hand-edited value, a blob from a build that
 * stored something else — all of it resolves to `{}`, and the caller falls back
 * to its own defaults.
 */
export function loadFields(spaceId: string, subject: string): DraftFields {
  const raw = loadDraft(spaceId, subject, "new-fields");
  if (!raw) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const source = parsed as Record<string, unknown>;
    // `exactOptionalPropertyTypes` is on, so a field that was not understood
    // must be *absent*, not present-and-undefined. Each reader emits either a
    // one-key object or nothing, and the result is their union.
    const text = (name: keyof DraftFields, v: unknown) =>
      typeof v === "string" && v ? { [name]: v } : {};
    const list = (name: keyof DraftFields, v: unknown) =>
      Array.isArray(v) && v.length && v.every((x) => typeof x === "string")
        ? { [name]: v as string[] }
        : {};
    return {
      ...text("project", source["project"]),
      ...text("status", source["status"]),
      ...text("priority", source["priority"]),
      ...text("due", source["due"]),
      ...list("labels", source["labels"]),
      ...list("assignees", source["assignees"]),
    };
  } catch {
    return {};
  }
}

/** Store the set fields, and nothing else: an all-defaults composer leaves no
 *  blob behind, so "is there a draft" stays a question storage can answer. */
export function saveFields(spaceId: string, subject: string, fields: DraftFields): void {
  const kept = prune(fields);
  saveDraft(
    spaceId,
    subject,
    "new-fields",
    Object.keys(kept).length ? JSON.stringify(kept) : "",
  );
}

/** Empty is not set. An unset field and a field explicitly cleared back to its
 *  default are the same state, so neither is written — which is what keeps
 *  "there is a stored blob" equivalent to "something was set". */
function prune(fields: DraftFields): DraftFields {
  return Object.fromEntries(
    Object.entries(fields).filter(
      ([, v]) => v !== undefined && v !== "" && !(Array.isArray(v) && v.length === 0),
    ),
  ) as DraftFields;
}

function key(spaceId: string, subject: string, kind: DraftKind): string {
  return `${PREFIX}:${encodeURIComponent(spaceId)}:${encodeURIComponent(subject)}:${kind}`;
}
