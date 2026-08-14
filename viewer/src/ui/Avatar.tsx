import { User } from "lucide-react";

import type { MemberDto } from "../types";
import { avatarColor } from "./colors";
import { faceUrl, useFace } from "./faces";
import { cn } from "./primitives";
import { short } from "./time";

/**
 * A member's display name — the one naming rule, shared.
 *
 * `you` for yourself, the local petname if one is set, the key's head otherwise.
 * Never a nick off the wire: `MemberDto.alias` is local and never synced, which is
 * exactly why it can be trusted (Members.tsx). Shared rather than re-declared,
 * because the timeline, the activity feed, and the detail footer all name the same
 * people and must not drift on how.
 */
export function memberName(key: string, member: MemberDto | undefined): string {
  if (member?.me) return "you";
  return member?.alias.trim() || short(key);
}

/**
 * A member, as a circle.
 *
 * Identity is drawn from the only thing everyone agrees on — the key — with
 * two authored layers riding on top, both resolved through the address book:
 * the name (`MemberDto.alias`, decorated by the daemon from Cards) and, when
 * a Card carries one, the **face** (`useFace`, the book's stored picture —
 * canonical for known coding agents, authored for everyone else). The book is
 * how applications resolve identity; this component keeps no matching table
 * of its own.
 *
 * The colour is derived from the key, which makes it the one honest half of
 * the identity: two nodes that disagree about what to *call* someone still
 * draw them the same colour, because they agree about the key. The letter and
 * the face come from each node's own book and can differ between nodes — that
 * is the model working, not a bug (S non-goal 6: names are advisory, keys are
 * authenticated).
 *
 * An unnamed member gets a glyph rather than a letter pulled from their key.
 * A hex digit is not an initial; rendering `7` as though it named someone
 * would be inventing an identity the system does not have.
 */
export function Avatar({
  deviceKey,
  alias,
  me,
  size = "md",
  className,
}: {
  /** The ed25519 key — the thing colour is derived from. */
  deviceKey: string;
  /** The book's name for them. May be empty; then we draw a glyph, not a hex digit. */
  alias?: string;
  /** Renders the "you" ring. The one member you never have to identify by name. */
  me?: boolean;
  size?: "sm" | "md";
  className?: string;
}) {
  const name = alias?.trim() ?? "";
  const label = me ? "you" : name || `${deviceKey.slice(0, 8)}…`;
  const color = avatarColor(deviceKey);
  // The authored face, from the book. `me` keeps the ring either way.
  const face = useFace(deviceKey);

  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center justify-center overflow-hidden rounded-full font-medium text-white select-none",
        size === "sm" ? "size-avatar-sm text-[8px]" : "size-avatar-md text-[9px]",
        // `me` gets a ring rather than a different colour: the colour still has to
        // be the key's, or you'd be the one member whose avatar means something else.
        me && "ring-accent ring-1 ring-offset-1 ring-offset-[var(--color-bg)]",
        className,
      )}
      style={{ background: color }}
      role="img"
      aria-label={label}
      title={label}
    >
      {face ? (
        <img src={faceUrl(face)} alt="" aria-hidden="true" className="size-full object-cover" />
      ) : name ? (
        // One grapheme, not `name[0]` — a surrogate pair (an emoji petname) would
        // otherwise render as half a character.
        [...name][0]?.toUpperCase()
      ) : (
        <User className={size === "sm" ? "size-icon-2xs" : "size-icon-xs"} strokeWidth={2.5} />
      )}
    </span>
  );
}

/**
 * Assignee keys → what `AvatarStack` needs, resolved against the ACL.
 *
 * A key with no matching member is kept, not dropped: someone removed from the
 * space is still the person the issue says is assigned, and silently vanishing
 * them would make the row disagree with the document. They get a colour and a glyph,
 * which is exactly as much as we honestly know about them.
 */
export function stackFor(
  keys: readonly string[],
  members: readonly MemberDto[],
): Array<{ key: string; alias: string; me: boolean }> {
  const indexed = memberIndexes.get(members) ?? indexMembers(members);
  return keys.map((key) => {
    const m = indexed.get(key);
    return { key, alias: m?.alias ?? "", me: m?.me ?? false };
  });
}

/** Row and card rendering calls `stackFor` once per record. Cache the immutable
 * ACL projection by array identity so that becomes O(assignees), not O(rows ×
 * assignees × members), without pushing lookup plumbing through every component. */
const memberIndexes = new WeakMap<readonly MemberDto[], ReadonlyMap<string, MemberDto>>();

function indexMembers(members: readonly MemberDto[]): ReadonlyMap<string, MemberDto> {
  const indexed = new Map(members.map((member) => [member.key, member]));
  memberIndexes.set(members, indexed);
  return indexed;
}

/**
 * Several members, overlapped — the board/list summary.
 *
 * Caps at `max` and counts the rest, because the row this sits in is 32px and the
 * point of it is to be read at a glance, not enumerated.
 */
export function AvatarStack({
  members,
  max = 3,
  className,
}: {
  members: Array<{ key: string; alias?: string; me?: boolean }>;
  max?: number;
  className?: string;
}) {
  if (members.length === 0) return null;
  const shown = members.slice(0, max);
  const rest = members.length - shown.length;

  return (
    <span className={cn("flex shrink-0 items-center", className)}>
      {shown.map((m, i) => (
        <Avatar
          key={m.key}
          deviceKey={m.key}
          {...(m.alias !== undefined ? { alias: m.alias } : {})}
          {...(m.me !== undefined ? { me: m.me } : {})}
          size="sm"
          // Overlap, with the earlier avatar on top so the stack reads left-to-right.
          className={cn(i > 0 && "-ml-1.5", "ring-bg ring-1")}
        />
      ))}
      {rest > 0 && <span className="text-mute ml-1 text-2xs tabular-nums">+{rest}</span>}
    </span>
  );
}
