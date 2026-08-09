import { Divider, DropdownMenuItem, DropdownMenuSubMenu } from "@astryxdesign/core";
import { Check, CheckSquare, Copy, ExternalLink, SignalHigh, Tag, UserRound } from "lucide-react";

import type { LabelDto, MemberDto, Priority, WorkflowState } from "../types";
import { PRIORITY_ORDER } from "../types";
import { Avatar, memberName } from "./Avatar";
import { catalogColor } from "./colors";
import type { IssueMutators } from "./fields";
import { PriorityIcon, StatusIcon } from "./icons";

/**
 * The verbs an issue carries, as menu entries — one definition, every surface.
 *
 * WHY THIS IS ITS OWN MODULE. It began inside `IssueList`'s row, which was the
 * right place while the list was the only thing that could open a menu over an
 * issue. The moment the board wanted the same menu, the choice was to export it
 * or to write it again, and writing it again is how two menus over the same
 * object start disagreeing about whether picking the current status is a toggle
 * or a no-op. A board card and a list row are two pictures of one issue; they
 * get one set of verbs.
 *
 * WHAT A CALLER STILL OWNS. Opening. `ContextMenu` wants the trigger, and the
 * trigger is the caller's element — a `<li>` here, an article there — so this
 * module returns the CONTENTS and never the shell. It also means a surface with
 * no selection model (the calendar) simply omits `selection` and loses that one
 * row rather than being handed a checkbox it cannot honour.
 *
 * THE FLY-OUTS ARE THE POINT. Status, assignee, priority and labels are the
 * same four writes the property chips make, reachable without aiming at a chip
 * five pixels tall. Each submenu marks the current value and picking it again
 * is a NO-OP, not a toggle — matching the chips exactly, because a menu that
 * un-set a status on second click would be a different control wearing the same
 * name.
 */
export function IssueMenuItems({
  reff,
  status,
  priority,
  assignees,
  labelNames,
  states,
  members,
  labels,
  mutators,
  locked,
  onOpen,
  selection,
}: {
  reff: string;
  status: string;
  priority: Priority;
  assignees: readonly string[];
  labelNames: readonly string[];
  states: WorkflowState[];
  members: MemberDto[];
  labels: LabelDto[];
  mutators: IssueMutators;
  /** Read-only space, or a provisional/deleted issue: readable, not writable. */
  locked: boolean;
  onOpen: (reff: string) => void;
  /**
   * The multi-select row, for surfaces that HAVE a selection. Absent means the
   * surface has none — which is a different thing from having one that is empty,
   * and the difference is why this is optional rather than a `checked=false`.
   */
  selection?: { checked: boolean; onToggle: (reff: string) => void };
}) {
  return (
    <>
      <DropdownMenuItem
        label="Open focused"
        icon={<ExternalLink className="size-icon-sm" />}
        onClick={() => onOpen(reff)}
      />
      <DropdownMenuItem
        label="Copy link"
        icon={<Copy className="size-icon-sm" />}
        onClick={() => {
          const url = new URL(window.location.href);
          url.searchParams.set("issue", reff);
          url.searchParams.set("focus", "1");
          void navigator.clipboard.writeText(url.toString());
        }}
      />
      {!locked && (
        <>
          <Divider />
          <DropdownMenuSubMenu label="Status" icon={<SignalHigh className="size-icon-sm" />}>
            {states.map((s) => (
              <DropdownMenuItem
                key={s.id}
                label={s.name}
                icon={<StatusIcon category={s.category} color={catalogColor(s.color)} />}
                endContent={s.id === status ? <Check className="size-icon-xs" /> : undefined}
                onClick={() => {
                  if (s.id !== status) mutators.setStatus(reff, s.id);
                }}
              />
            ))}
          </DropdownMenuSubMenu>
          <DropdownMenuSubMenu label="Assignee" icon={<UserRound className="size-icon-sm" />}>
            {members.length === 0 && <DropdownMenuItem label="No members yet" isDisabled />}
            {members.map((m) => (
              <DropdownMenuItem
                key={m.key}
                label={memberName(m.key, m)}
                icon={<Avatar deviceKey={m.key} alias={m.alias} me={m.me} size="sm" />}
                endContent={assignees.includes(m.key) ? <Check className="size-icon-xs" /> : undefined}
                onClick={() => mutators.toggleAssignee(reff, m.key, !assignees.includes(m.key))}
              />
            ))}
          </DropdownMenuSubMenu>
          <DropdownMenuSubMenu label="Priority" icon={<PriorityIcon priority={priority} />}>
            {[...PRIORITY_ORDER].reverse().map((p) => (
              <DropdownMenuItem
                key={p}
                label={<span className="capitalize">{p === "none" ? "No priority" : p}</span>}
                icon={<PriorityIcon priority={p} tone="neutral" />}
                endContent={p === priority ? <Check className="size-icon-xs" /> : undefined}
                onClick={() => {
                  if (p !== priority) mutators.setPriority(reff, p);
                }}
              />
            ))}
          </DropdownMenuSubMenu>
          <DropdownMenuSubMenu label="Labels" icon={<Tag className="size-icon-sm" />}>
            {labels.length === 0 && <DropdownMenuItem label="No labels yet" isDisabled />}
            {labels.map((l) => {
              const on = labelNames.includes(l.name);
              return (
                <DropdownMenuItem
                  key={l.id}
                  label={<span className="capitalize">{l.name}</span>}
                  icon={
                    <span
                      className="size-mark-sm shrink-0 rounded-full"
                      style={{ background: catalogColor(l.color) }}
                    />
                  }
                  endContent={on ? <Check className="size-icon-xs" /> : undefined}
                  onClick={() => mutators.toggleLabel(reff, l.name, !on)}
                />
              );
            })}
          </DropdownMenuSubMenu>
          {selection && <Divider />}
        </>
      )}
      {selection && (
        <DropdownMenuItem
          label={selection.checked ? "Remove from selection" : "Add to selection"}
          icon={<CheckSquare className="size-icon-sm" />}
          onClick={() => selection.onToggle(reff)}
        />
      )}
    </>
  );
}
