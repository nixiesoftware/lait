import type { AssignmentDto, AssignmentOriginKind } from "../types";

/**
 * The access list, folded the way a person reads it.
 *
 * The engine answers `access_list` with one row per effective assignment —
 * every capability, however it got there. Drawn as-is that list offers a
 * founder their own membership as eight revocable extras, because the founder
 * policy, an admission and a grant made on this page all land as the same
 * flat row. Each row now says why it exists (`origin`), and this fold is what
 * turns that into three shapes:
 *
 * - **membership** — everything that came with being a member (founder
 *   policy, admission, a role change, sponsorship), folded to one line per
 *   actor. Its verbs are on the Members page; nothing here revokes it.
 * - **grants** — what was granted on top, one per role grant: the same role
 *   at the same scope in the same World is one thing, revoked as one thing.
 * - **unrecorded** — rows whose origin was never written. They are listed as
 *   themselves and each revokes alone: an absence is not a kind, so it is
 *   neither folded into membership nor called a grant.
 */

/** One capability at one scope in one World, and every grant id that says so. */
export interface HeldCapability {
  capability: string;
  world: string;
  /** Project id or key; empty for the whole Space. */
  scope: string;
  grantIds: string[];
}

export interface MembershipLine {
  /** Every kind that contributed — `founder` alone for a founder, `admission`
   *  plus `membership` for someone promoted after joining. */
  kinds: AssignmentOriginKind[];
  /** The role ids the origins named, distinct, in first-seen order. Empty for
   *  a founder: the founder policy is not a role. */
  roles: string[];
  capabilities: HeldCapability[];
}

export interface RoleGrant {
  /** Stable across reloads — what a row keys and a revoke names. */
  key: string;
  /** The role id, when the owning World named it. */
  role: string | null;
  /** The opaque reference, when the World did not name it — still enough to
   *  keep two grants apart. */
  definitionRef: string | null;
  world: string;
  scope: string;
  capabilities: string[];
  grantIds: string[];
}

export interface ActorAccess {
  actor: string;
  membership: MembershipLine | null;
  grants: RoleGrant[];
  unrecorded: HeldCapability[];
}

const MEMBERSHIP: ReadonlySet<AssignmentOriginKind> = new Set<AssignmentOriginKind>([
  "founder",
  "admission",
  "membership",
  "sponsorship",
]);

/** Whether an origin kind means "came with being a member". */
export function isMembershipKind(kind: AssignmentOriginKind): boolean {
  return MEMBERSHIP.has(kind);
}

function scopeOf(row: AssignmentDto): string {
  return row.resource[0] ?? "";
}

function hold(into: HeldCapability[], row: AssignmentDto): void {
  const scope = scopeOf(row);
  const existing = into.find(
    (c) => c.capability === row.capability && c.world === row.world && c.scope === scope,
  );
  if (existing) existing.grantIds.push(row.grant_id);
  else into.push({ capability: row.capability, world: row.world, scope, grantIds: [row.grant_id] });
}

/** Fold assignment rows by actor. Order within an actor is first-seen; the
 *  caller orders actors, because names are its to resolve. */
export function foldAccess(rows: readonly AssignmentDto[]): ActorAccess[] {
  const byActor = new Map<string, ActorAccess>();
  const grantIndex = new Map<string, RoleGrant>();
  for (const row of rows) {
    let entry = byActor.get(row.actor);
    if (!entry) {
      entry = { actor: row.actor, membership: null, grants: [], unrecorded: [] };
      byActor.set(row.actor, entry);
    }
    const origin = row.origin;
    if (!origin) {
      hold(entry.unrecorded, row);
      continue;
    }
    if (isMembershipKind(origin.kind)) {
      const line = entry.membership ?? { kinds: [], roles: [], capabilities: [] };
      entry.membership = line;
      if (!line.kinds.includes(origin.kind)) line.kinds.push(origin.kind);
      if (origin.role && !line.roles.includes(origin.role)) line.roles.push(origin.role);
      hold(line.capabilities, row);
      continue;
    }
    const scope = scopeOf(row);
    const named = origin.role ?? origin.definition_ref ?? "";
    const key = [row.actor, row.world, named, scope].join(" ");
    let grant = grantIndex.get(key);
    if (!grant) {
      grant = {
        key,
        role: origin.role ?? null,
        definitionRef: origin.definition_ref ?? null,
        world: row.world,
        scope,
        capabilities: [],
        grantIds: [],
      };
      grantIndex.set(key, grant);
      entry.grants.push(grant);
    }
    if (!grant.capabilities.includes(row.capability)) grant.capabilities.push(row.capability);
    grant.grantIds.push(row.grant_id);
  }
  return [...byActor.values()];
}

/** How many things an actor holds beyond membership — what the header counts. */
export function extrasOf(access: ActorAccess): number {
  return access.grants.length + access.unrecorded.length;
}
