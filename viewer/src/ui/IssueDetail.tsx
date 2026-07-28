import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import * as Popover from "@radix-ui/react-popover";
import {
  AlertTriangle,
  ArchiveRestore,
  ArrowUp,
  Ban,
  Bell,
  BellOff,
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  CircleDot,
  Copy,
  CopyPlus,
  CornerDownRight,
  Download,
  Gauge,
  GitMerge,
  Info,
  Link2,
  Milestone,
  MoreHorizontal,
  MoveRight,
  Paperclip,
  Pencil,
  Plus,
  RefreshCw,
  SmilePlus,
  Tag,
  Trash2,
  UserMinus,
  UserPlus,
  X,
} from "lucide-react";

import { rpc } from "../api";
import { useIssueDetail, useProjectViewerStore } from "../projectStore";
import { clearDraft, loadDraft, saveDraft } from "../core/drafts";
import {
  describeEventRich,
  EDIT_KINDS,
  type EventPhraseContext,
  type NameResolver,
} from "../core/activity";
import type { Field as PredictField } from "../core/overlay";
import type { IssueField } from "../core/registry";
import { inverseWorkAction, workTarget } from "../core/workflow";
import { boundedTail } from "../core/performance";
import {
  type AttachmentMetaDto,
  type GraphView,
  type LinkDto,
  type Priority,
  type Row,
  PRIORITY_ORDER,
  tsToDate,
  type ActivityEvent,
  type CommentDto,
  type IssueView,
  type LabelDto,
  type MemberDto,
  type ProjectDto,
  type WorkflowState,
} from "../types";
import { Avatar, AvatarStack, memberName as nameOf } from "./Avatar";
import { LoadingState } from "./AppState";
import { catalogColor } from "./colors";
import { PriorityIcon, StatusIcon } from "./icons";
import { Markdown } from "./Markdown";
import { MarkdownEditor } from "./MarkdownEditor";
import { DatePicker } from "./DatePicker";
import { NewLabelDialog } from "./NewLabel";
import { Combobox, type Option } from "./Picker";
import { Button, ChipButton, cn, IconButton, Input, LabelChip, PopoverContent } from "./primitives";
import {
  Disclosure,
  HeaderActions,
  MenuContent,
  MenuItem,
  RailRow,
  RailSection,
  Toast,
} from "./layout";
import * as ask from "./dialogs";
import { dueToInput, dueTone, short, when } from "./time";

/**
 * The issue detail — co-visible beside the list, not an overlay.
 *
 * The TUI called this "peek" and kept it deliberately *off* the overlay stack so a
 * picker could sit over it while the list still rendered behind. Same reasoning
 * here: it is a third panel, so it does not steal the keymap and the list keeps
 * moving under `j`/`k` while you read.
 *
 * Every edit is a `Request` on the way out and a doorbell on the way back. Nothing
 * here refetches after a write: the daemon rings, the doorbell reloads the row, and
 * the detail re-reads with it. That is what keeps this pane and the list from ever
 * disagreeing about what an issue says.
 */
export function IssueDetail({
  spaceId,
  canonicalSpaceId,
  reff,
  states,
  members,
  labels,
  projects,
  readOnly,
  tombstone,
  openField,
  onOpenField,
  onError,
  onDelete,
  onPredict,
  onNavigate,
  onClose,
  onPrevious,
  onNext,
}: {
  spaceId: string;
  canonicalSpaceId: string;
  reff: string;
  states: WorkflowState[];
  /** The signed ACL, for the assignee picker. Keys are the only real identity. */
  members: MemberDto[];
  labels: LabelDto[];
  projects: ProjectDto[];
  readOnly: boolean;
  /** Whether the board says this issue is deleted — `IssueView` doesn't carry
   *  the tombstone, but the row does, and it decides Delete vs Restore. */
  tombstone: boolean;
  /** Which picker a keybinding wants open, if any. */
  openField: IssueField | null;
  onOpenField: (f: IssueField | null) => void;
  onError: (m: string) => void;
  onDelete: (reff: string) => void;
  /** Predict `(doc, field)` locally, then send. The doorbell retires the guess. */
  onPredict: (doc: string, field: PredictField, value: string, send: () => Promise<unknown>) => Promise<boolean>;
  /** Select another issue — following a graph edge (parent, sub-issue, blocker). */
  onNavigate: (reff: string) => void;
  onClose: () => void;
  onPrevious?: () => void;
  onNext?: () => void;
}) {
  const projectStore = useProjectViewerStore();
  const detail = useIssueDetail(spaceId, reff);
  const issue = detail.issue;
  const events = detail.history.data ?? [];
  const graph = detail.graph.data ?? null;
  const milestones = detail.milestones.data ?? [];
  const [draft, setDraft] = useState(() => loadDraft(canonicalSpaceId, reff, "title"));
  const [comment, setComment] = useState(() => loadDraft(canonicalSpaceId, reff, "comment"));
  const [commentPending, setCommentPending] = useState(false);
  const [commentError, setCommentError] = useState<string | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  /** A label name the picker wants to mint — opens the colour step. */
  const [newLabel, setNewLabel] = useState<string | null>(null);
  /** The relation composer, and the inline sub-issue composer (`null` = closed).
   *  Both live here so the overflow menu can open them when their group is not
   *  on screen — an empty group renders nothing, including its own `+`. */
  const [relating, setRelating] = useState(false);
  const [subDraft, setSubDraft] = useState<string | null>(null);
  const [undoWork, setUndoWork] = useState<{
    message: string;
    action: "start" | "done" | "stop";
  } | null>(null);
  const titleRef = useRef<HTMLTextAreaElement>(null);
  const commentRef = useRef<HTMLTextAreaElement>(null);

  useEffect(
    () => saveDraft(canonicalSpaceId, reff, "comment", comment),
    [canonicalSpaceId, reff, comment],
  );

  useEffect(() => {
    if (!issue) return;
    if (draft !== issue.title) saveDraft(canonicalSpaceId, reff, "title", draft);
    else clearDraft(canonicalSpaceId, reff, "title");
  }, [canonicalSpaceId, reff, draft, issue]);

  useEffect(() => {
    if (!undoWork) return;
    const timeout = window.setTimeout(() => setUndoWork(null), 6000);
    return () => window.clearTimeout(timeout);
  }, [undoWork]);

  useEffect(() => {
    if (issue) setDraft((current) => current || issue.title);
  }, [issue]);

  // Grow the title to its content. This used to be `rows={length / 40}`, a guess
  // at the wrap point that was calibrated for the old 18px type — at 24px it was
  // short by a line and the title scrolled inside its own box. Measuring is both
  // exact and immune to the next type change: reset to `auto` first, because
  // `scrollHeight` never reports *less* than the height already set.
  useEffect(() => {
    const el = titleRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [draft]);

  const edit = useCallback(
    async (patch: { title?: string; description?: string }) => {
      try {
        await rpc(spaceId, { cmd: "issue_edit", reff, ...patch });
      } catch (e) {
        onError(e instanceof Error ? e.message : String(e));
      }
    },
    [spaceId, reff, onError],
  );

  /** Writes with no predictable row field — the doorbell is the only feedback. */
  const send = useCallback(
    async (fn: () => Promise<unknown>) => {
      try {
        await fn();
      } catch (e) {
        onError(e instanceof Error ? e.message : String(e));
      }
    },
    [onError],
  );

  const runCommand = useCallback(async (command: Promise<boolean>): Promise<boolean> => {
    try {
      return await command;
    } catch (error) {
      onError(error instanceof Error ? error.message : String(error));
      return false;
    }
  }, [onError]);

  const memberOf = useCallback(
    (key: string): MemberDto | undefined => members.find((m) => m.key === key),
    [members],
  );

  if (!issue) {
    return <aside className="border-line flex h-full border-l"><LoadingState title="Loading issue" body="Reading the local issue document." /></aside>;
  }

  const state = states.find((s) => s.id === issue.status);
  const project = projects.find((p) => p.id === issue.project_id);
  const locked = readOnly || issue.provisional;

  const runWorkAction = async (
    action: "start" | "done" | "stop",
    recordUndo = true,
  ) => {
    if (pendingAction) return;
    const target = workTarget(states, action);
    const previousCategory = state?.category ?? "backlog";
    setPendingAction(action);
    try {
      const accepted = target
        ? await onPredict(issue.doc_id, "status", target.id, () =>
          rpc(spaceId, { cmd: `issue_${action}`, reff }),
        )
        : await rpc(spaceId, { cmd: `issue_${action}`, reff }).then(() => true);
      if (!accepted) return;
      if (recordUndo) {
        setUndoWork({
          message:
            action === "done"
              ? "Issue completed"
              : action === "stop"
                ? "Work stopped"
                : "Work started",
          action: inverseWorkAction(action, previousCategory),
        });
      }
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setPendingAction(null);
    }
  };

  const saveTitle = () => {
    const next = draft.trim();
    if (!next || next === issue.title) {
      setDraft(issue.title);
      clearDraft(canonicalSpaceId, reff, "title");
      return;
    }
    void runCommand(projectStore.editTitle(spaceId, reff, next)).then((accepted) => {
      if (accepted) clearDraft(canonicalSpaceId, reff, "title");
    });
  };

  const submitComment = async () => {
    const body = comment.trim();
    if (!body || commentPending) return;
    setCommentPending(true);
    setCommentError(null);
    try {
      await rpc(spaceId, { cmd: "comment", reff, body });
      setComment("");
      clearDraft(canonicalSpaceId, reff, "comment");
      commentRef.current?.focus();
    } catch (error) {
      setCommentError(error instanceof Error ? error.message : String(error));
    } finally {
      setCommentPending(false);
    }
  };

  const duplicateIssue = async () => {
    if (pendingAction) return;
    setPendingAction("duplicate");
    try {
      const result = await rpc(spaceId, {
        cmd: "issue_new",
        title: `${issue.title} (copy)`,
        project: issue.project_id,
        body: issue.description || null,
        priority: issue.priority,
        labels: issue.label_names,
        assignees: issue.assignees,
        due: issue.due_date != null ? dueToInput(issue.due_date) : null,
        estimate: issue.estimate ?? null,
      });
      if (result.kind === "ref") onNavigate(result.reff);
    } catch (error) {
      onError(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingAction(null);
    }
  };

  const pickerOpen = (f: IssueField) => openField === f;
  const setPicker = (f: IssueField) => (o: boolean) => onOpenField(o ? f : null);

  return (
    <aside className="issue-detail @container flex h-full min-h-0 flex-col overflow-y-auto">
      {/* The trail is the shell's — the issue is a hop on it, not a surface with
          its own bar. What is still ours is the verbs: paging to a neighbour and
          the overflow both need state that lives in here, so they travel up to
          the header rather than dragging the state down to it. */}
      <HeaderActions>
        <IconButton label="Previous issue" onClick={onPrevious} disabled={!onPrevious}>
          <ChevronLeft className="size-icon-sm" />
        </IconButton>
        <IconButton label="Next issue" onClick={onNext} disabled={!onNext}>
          <ChevronRight className="size-icon-sm" />
        </IconButton>
        <IssueOverflow
          issueRef={issue.key_alias ?? issue.reff}
          active={state?.category === "active"}
          locked={locked}
          tombstone={tombstone}
          pending={pendingAction !== null}
          onCopyLink={() => void navigator.clipboard.writeText(window.location.href)}
          onDuplicate={() => void duplicateIssue()}
          onRelate={() => setRelating(true)}
          onAddSubIssue={() => setSubDraft("")}
          onAttach={() => document.getElementById("issue-attach")?.click()}
          onAssign={() => onOpenField("assignee")}
          onMove={() => onOpenField("project")}
          onStop={() => void runWorkAction("stop")}
          onRestore={() => void send(() => rpc(spaceId, { cmd: "issue_restore", reff: issue.reff }))}
          onDelete={() => onDelete(issue.reff)}
        />
        <IconButton label="Close issue" chord="Esc" onClick={onClose}>
          <X className="size-icon-sm" />
        </IconButton>
      </HeaderActions>

      <div className="issue-detail-body flex flex-col gap-4 p-4">
        {Boolean(detail.body.error || detail.secondaryError) && (
          <div className="border-warn/30 bg-warn/5 text-dim rounded-surface border px-3 py-2 text-sm" role="status">
            Some issue details could not be refreshed. Known content remains available.
          </div>
        )}
        {tombstone && (
          <div className="border-danger/30 bg-danger/5 text-dim rounded-surface border px-3 py-2 text-sm">
            This issue is deleted. Restore it from the More actions menu.
          </div>
        )}
        {undoWork && (
          <Toast action={<Button
              variant="ghost"
              onClick={() => {
                const action = undoWork.action;
                setUndoWork(null);
                void runWorkAction(action, false);
              }}
            >
              Undo
            </Button>}>
            {undoWork.message}
          </Toast>
        )}
        {events.some((event) => event.collision) && (
          <div className="border-warn/30 bg-warn/5 text-dim flex items-start gap-2 rounded-surface border p-3 text-sm" role="status">
            <AlertTriangle className="text-warn mt-0.5 size-icon-sm shrink-0" />
            <span className="min-w-0 flex-1">
              Concurrent edits converged to the current values. Review the marked history entry;
              if its outcome is not what you intended, reapply the field above as a new explicit change.
            </span>
            <Button
              variant="ghost"
              onClick={() => document.getElementById("issue-activity")?.scrollIntoView({ block: "start" })}
            >
              Review history
            </Button>
          </div>
        )}
        {issue.provisional && (
          <div className="border-warn/30 bg-warn/5 text-dim flex gap-2 rounded-surface border p-3 text-sm">
            <Info className="text-warn mt-0.5 size-icon-sm shrink-0" />
            <span>
              This issue is known to the local catalog, but its body is still arriving. Metadata may be incomplete; editing stays unavailable until the projection is ready.
            </span>
          </div>
        )}
        {!!issue.corrupt_records?.length && (
          <details className="border-danger/30 bg-danger/5 rounded-surface border p-3 text-sm">
            <summary className="text-danger flex items-center gap-2 font-medium">
              <AlertTriangle className="size-icon-sm" />
              {issue.corrupt_records.length} stored {issue.corrupt_records.length === 1 ? "record needs" : "records need"} attention
            </summary>
            <ul className="text-dim mt-2 flex flex-col gap-1 pl-5 text-xs">
              {issue.corrupt_records.map((record, index) => (
                <li key={`${record.locus}-${index}`}>
                  <code>{record.locus}</code>: {record.reason}
                </li>
              ))}
            </ul>
          </details>
        )}
        {/* A textarea, not an input: a long title should wrap rather than scroll
            sideways past the edge of the pane. */}
        <textarea
          ref={titleRef}
          value={draft}
          readOnly={locked}
          rows={1}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={saveTitle}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              titleRef.current?.blur();
            }
            if (e.key === "Escape") {
              setDraft(issue.title);
              clearDraft(canonicalSpaceId, reff, "title");
              titleRef.current?.blur();
            }
          }}
          // The document's own title, sized like one. It was `text-lg` (18px)
          // over a 13px body — barely three points of hierarchy for the most
          // important string on the page. `text-2xl` against the 15px body is
          // the ratio Hashnode and Mintlify give an article's h1.
          className="issue-detail-title resize-none overflow-hidden bg-transparent text-2xl leading-tight font-semibold tracking-tight outline-none"
          aria-label="Title"
        />

        {/*
          No Start/Done/Stop buttons here, deliberately.

          `start`/`done`/`stop` are real verbs with their own `Request`s, and the
          temptation is to give them a button row. Linear does not work that way:
          its issue detail is a title, a properties list, and a timeline — the
          status picker *is* the action, and every verb lives on a key and in the
          palette. lait's verbs are reachable exactly there (`S`/`D`/`O`, and by
          name in ⌘K). A button row would be a second, louder way to do what the
          Status row above already does, and it would be the one piece of this pane
          that came from somewhere else.
        */}
        <div className="issue-detail-properties flex flex-col text-sm">
          <RailSection title="Properties">
          <RailRow label="Status">
            <Combobox
              tone="quiet"
              label="Status"
              disabled={locked}
              open={pickerOpen("status")}
              onOpenChange={setPicker("status")}
              value={{
                id: issue.status,
                label: state?.name ?? issue.status,
                ...(state
                  ? {
                      icon: (
                        <StatusIcon category={state.category} color={catalogColor(state.color)} />
                      ),
                    }
                  : {}),
              }}
              options={states.map((s) => ({
                id: s.id,
                label: s.name,
                icon: <StatusIcon category={s.category} color={catalogColor(s.color)} />,
              }))}
              onPick={(id) =>
                void runCommand(projectStore.setStatus(spaceId, reff, id))
              }
            />
          </RailRow>

          <RailRow label="Priority">
            <Combobox
              tone="quiet"
              label="Priority"
              disabled={locked}
              open={pickerOpen("priority")}
              onOpenChange={setPicker("priority")}
              value={{
                id: issue.priority,
                label: issue.priority,
                icon: <PriorityIcon priority={issue.priority} />,
              }}
              // `none` is a real engine value, not an absence — but in the rail
              // it is still an unset property, so it wears the verb. `capitalize`
              // rides with the real values only: it would render the verb as
              // "Set Priority".
              face={
                <>
                  <PriorityIcon priority={issue.priority} />
                  <span
                    className={
                      issue.priority === "none" ? "text-mute" : "min-w-0 truncate capitalize"
                    }
                  >
                    {issue.priority === "none" ? "Set priority" : issue.priority}
                  </span>
                </>
              }
              // Highest first: the list you scan top-down should start where the
              // urgency does.
              options={[...PRIORITY_ORDER].reverse().map((p) => ({
                id: p,
                label: p,
                icon: <PriorityIcon priority={p} />,
              }))}
              onPick={(id) =>
                void runCommand(projectStore.setPriority(spaceId, reff, id))
              }
            />
          </RailRow>

          <RailRow label="Assignees">
            <Combobox
              tone="quiet"
              multi
              label="Assignees"
              disabled={locked}
              open={pickerOpen("assignee")}
              onOpenChange={setPicker("assignee")}
              selected={issue.assignees}
              emptyText={members.length ? "No matches" : "No members yet"}
              face={
                issue.assignees.length === 0 ? (
                  <>
                    <UserPlus className="text-mute size-icon-sm shrink-0" />
                    <span className="text-mute">Assign</span>
                  </>
                ) : (
                  <span className="flex min-w-0 items-center gap-1.5">
                    <AvatarStack
                      members={issue.assignees.map((k) => ({
                        key: k,
                        alias: memberOf(k)?.alias ?? "",
                        me: memberOf(k)?.me ?? false,
                      }))}
                    />
                    <span className="truncate">
                      {issue.assignees.map((k) => nameOf(k, memberOf(k))).join(", ")}
                    </span>
                  </span>
                )
              }
              options={members.map((m) => ({
                id: m.key,
                label: nameOf(m.key, m),
                icon: <Avatar deviceKey={m.key} alias={m.alias} me={m.me} size="sm" />,
                // The key prefix, because the petname is the *unverified* half of
                // the identity — Members.tsx makes the same point at full width.
                hint: m.key.slice(0, 6),
                keywords: [m.key, m.alias],
              }))}
              onToggle={(key) => {
                const add = !issue.assignees.includes(key);
                // `who` takes `me`/`@me` or a **full 64-hex key** — `index::resolve_device`
                // does not consult the member directory, so a petname would 404. The
                // key is what we hold and the key is what we send.
                void send(() => rpc(spaceId, { cmd: "assign", reff, who: [key], add }));
              }}
            />
          </RailRow>

          <RailRow label="Estimate">
            <Combobox
              tone="quiet"
              label="Estimate"
              disabled={locked}
              value={
                issue.estimate != null
                  ? { id: String(issue.estimate), label: `${issue.estimate} pt` }
                  : null
              }
              face={
                <>
                  <Gauge className="text-mute size-icon-sm shrink-0" />
                  <span className={issue.estimate == null ? "text-mute" : "min-w-0 truncate"}>
                    {issue.estimate != null ? `${issue.estimate} pt` : "Set estimate"}
                  </span>
                </>
              }
              // Fibonacci-ish, Linear's default scale; "None" clears. The
              // engine stores a bare number — the scale is a team convention.
              options={[
                { id: "none", label: "None" },
                ...[1, 2, 3, 5, 8, 13].map((n) => ({ id: String(n), label: `${n} pt` })),
              ]}
              onPick={(id) =>
                void send(() => rpc(spaceId, { cmd: "issue_edit", reff, estimate: id }))
              }
            />
          </RailRow>

          <RailRow label="Due date">
            <DueDate
              value={issue.due_date ?? null}
              readOnly={locked}
              onChange={(due) =>
                void send(() => rpc(spaceId, { cmd: "issue_edit", reff, due }))
              }
            />
          </RailRow>
          </RailSection>

          <RailSection title="Labels">
          <RailRow label="Labels">
            {/* Every chip is its own trigger. The row used to be one control
                whose face happened to be a run of chips, so clicking any label
                — or the gap beside it — opened the same multi-select; there was
                no way to say "this one, but a different one". Now a chip opens
                a picker that swaps it, and the trailing `+` is the only control
                that adds. */}
            <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-2">
              {issue.label_names.map((name) => (
                <Combobox
                  key={name}
                  tone="bare" size="none"
                  label={`Change label ${name}`}
                  disabled={locked}
                  value={{ id: name, label: name }}
                  face={
                    <LabelChip
                      name={name}
                      color={labels.find((l) => l.name === name)?.color ?? "gray"}
                    />
                  }
                  options={[
                    { id: "__remove__", label: `Remove ${name}` },
                    ...labels
                      .filter((l) => l.name === name || !issue.label_names.includes(l.name))
                      .map((l) => ({
                        id: l.name,
                        label: l.name,
                        swatch: catalogColor(l.color),
                        keywords: [l.id],
                      })),
                  ]}
                  onPick={(next) => {
                    if (next === name) return;
                    // One swap, two requests, in this order: the engine's label
                    // op is add-or-remove on a name set, so a rename is a
                    // detach and an attach. Removing first keeps the set from
                    // briefly holding both.
                    void send(async () => {
                      await rpc(spaceId, { cmd: "label", reff, remove: [name] });
                      if (next !== "__remove__") {
                        await rpc(spaceId, { cmd: "label", reff, add: [next] });
                      }
                    });
                  }}
                />
              ))}
              <Combobox
                tone="quiet"
                multi
                label="Add label"
                disabled={locked}
                open={pickerOpen("label")}
                onOpenChange={setPicker("label")}
                // The registry is keyed by id, but `Request::Label` resolves **names**
                // (`replica.rs::label`), so the selection is tracked by name too —
                // matching what we send rather than translating at the boundary.
                selected={issue.label_names}
                emptyText={labels.length ? "No matches" : "No labels yet"}
                face={
                  issue.label_names.length === 0 ? (
                    <>
                      <Tag className="text-mute size-icon-sm shrink-0" />
                      <span className="text-mute">Add label</span>
                    </>
                  ) : (
                    <Plus className="text-mute size-icon-sm shrink-0" />
                  )
                }
                options={labels.map((l) => ({
                  id: l.name,
                  label: l.name,
                  swatch: catalogColor(l.color),
                  keywords: [l.id],
                }))}
                onToggle={(name) => {
                  const on = issue.label_names.includes(name);
                  void send(() =>
                    rpc(spaceId, {
                      cmd: "label",
                      reff,
                      ...(on ? { remove: [name] } : { add: [name] }),
                    }),
                  );
                }}
                // A brand-new label gets a colour before it exists: the picker hands
                // the name off to the colour step, which registers it via `label_new`
                // and then attaches it — rather than minting it gray on first use.
                onCreate={(name) => setNewLabel(name)}
              />
            </span>
          </RailRow>
          </RailSection>

          <RailSection title="Project">
          {graph?.parent && (
            <RailRow label="Parent">
              <Button
                onClick={() => onNavigate(graph.parent!.reff)}
                className="-mx-1 min-w-0 justify-start px-1 text-left"
              >
                <GitMerge className="text-mute size-icon-sm shrink-0" />
                <span className="min-w-0 truncate font-medium">{graph.parent.title}</span>
              </Button>
            </RailRow>
          )}

          <RailRow label="Project">
            <Combobox
              tone="quiet"
              label="Project"
              swatchShape="square"
              disabled={locked}
              open={pickerOpen("project")}
              onOpenChange={setPicker("project")}
              value={
                project
                  ? { id: project.id, label: project.name, swatch: catalogColor(project.color) }
                  : { id: issue.project_id, label: issue.project_key ?? "—" }
              }
              options={projects.map((p) => ({
                id: p.id,
                label: p.name,
                swatch: catalogColor(p.color),
                hint: p.key,
                keywords: [p.key],
              }))}
              onPick={(id) => {
                if (id === issue.project_id) return;
                // `issue_move` carries project *and* position; sending only the
                // project leaves `pos` null, which the daemon reads as "don't
                // reorder" rather than "move to top".
                void send(() => rpc(spaceId, { cmd: "issue_move", reff, project: id }));
              }}
            />
          </RailRow>

          {(milestones.length > 0 || issue.milestone) && (
            <RailRow label="Milestone">
              <Combobox
                tone="quiet"
                label="Milestone"
                disabled={locked}
                value={
                  issue.milestone
                    ? {
                        id: issue.milestone,
                        label:
                          milestones.find((m) => m.id === issue.milestone)?.name ??
                          issue.milestone,
                      }
                    : null
                }
                face={
                  <>
                    <Milestone className="text-mute size-icon-sm shrink-0" />
                    <span className={issue.milestone ? "min-w-0 truncate" : "text-mute"}>
                      {issue.milestone
                        ? (milestones.find((m) => m.id === issue.milestone)?.name ??
                          issue.milestone)
                        : "Set milestone"}
                    </span>
                  </>
                }
                options={[
                  { id: "none", label: "None" },
                  ...milestones.map((m) => ({
                    id: m.id,
                    label: m.name,
                    hint: `${m.done}/${m.total}`,
                  })),
                ]}
                onPick={(id) =>
                  void send(() =>
                    rpc(spaceId, {
                      cmd: "issue_milestone",
                      reff,
                      milestone: id === "none" ? null : id,
                    }),
                  )
                }
              />
            </RailRow>
          )}
          </RailSection>

          <RailSection title="Notifications">
          <RailRow label="Notifications">
            <FollowToggle
              issue={issue}
              meKey={members.find((m) => m.me)?.key ?? null}
              readOnly={locked}
              onToggle={(on) => void send(() => rpc(spaceId, { cmd: "follow", reff, on }))}
            />
          </RailRow>
          </RailSection>
        </div>

        <Description
          draftKey={{ spaceId: canonicalSpaceId, reff }}
          value={issue.description}
          readOnly={locked}
          onSave={(description) => void edit({ description })}
        />

        <Attachments
          spaceId={spaceId}
          reff={issue.reff}
          attachments={issue.attachments ?? []}
          readOnly={locked}
          onError={onError}
        />

        {graph && (
          <Relations
            graph={graph}
            spaceId={spaceId}
            reff={issue.reff}
            projectId={issue.project_id}
            states={states}
            readOnly={locked}
            send={send}
            onNavigate={onNavigate}
            adding={relating}
            setAdding={setRelating}
            subDraft={subDraft}
            setSubDraft={setSubDraft}
          />
        )}

        <Timeline
          key={reff}
          events={events}
          comments={issue.comments}
          memberOf={memberOf}
          states={states}
          graph={graph}
          readOnly={locked}
          meKey={members.find((m) => m.me)?.key ?? null}
          onReact={(comment, emoji, on) =>
            void send(() => rpc(spaceId, { cmd: "react", reff, comment, emoji, on }))
          }
          onReply={(replyTo, body) =>
            void send(() => rpc(spaceId, { cmd: "comment", reff, body, reply_to: replyTo }))
          }
          onCopyLink={(commentId) => {
            const url = new URL(window.location.href);
            url.searchParams.set("issue", issue.reff);
            url.searchParams.set("comment", commentId);
            void navigator.clipboard.writeText(url.toString());
          }}
          onCreateFromComment={(body) =>
            void (async () => {
              const title = body.split("\n")[0]!.slice(0, 80).trim() || "Follow-up";
              const r = await rpc(spaceId, {
                cmd: "issue_new",
                title,
                project: issue.project_id,
                body,
              });
              if (r.kind === "ref") onNavigate(r.reff);
            })()
          }
        />

        {!locked && (
          /* One surface, not two stacked ones. The actions used to sit in a
             bordered footer strip, which drew a rule across the composer and
             spent a full row on a hint that never changes. Linear puts the
             send control inside the field at the bottom right and lets the
             keyboard shortcut live on its tooltip — the affordance is the
             button, and the hint is there when you go looking for it. */
          <div className="border-line focus-within:border-line-strong bg-raised shadow-raised rounded-surface border">
            <textarea
              ref={commentRef}
              value={comment}
              placeholder="Leave a comment…"
              onChange={(e) => {
                setComment(e.target.value);
                setCommentError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && comment.trim()) {
                  e.preventDefault();
                  void submitComment();
                }
              }}
              rows={3}
              className="placeholder:text-mute block w-full resize-none bg-transparent p-2 outline-none"
              aria-label="New comment"
              aria-describedby={commentError ? "comment-error" : undefined}
            />
            <div className="flex items-center gap-2 px-2 pb-2">
              {commentError && (
                <span
                  id="comment-error"
                  className="text-danger min-w-0 flex-1 truncate text-xs"
                  role="alert"
                >
                  Comment not sent. Your draft is safe.
                </span>
              )}
              <span className="ml-auto" />
              <IconButton
                label="Attach a file"
                onClick={() => document.getElementById("issue-attach")?.click()}
              >
                <Paperclip className="size-icon-sm" />
              </IconButton>
              <IconButton
                label={commentError ? "Retry comment" : "Comment"}
                chord="Ctrl/⌘ ↵"
                disabled={!comment.trim()}
                loading={commentPending}
                onClick={() => void submitComment()}
              >
                <ArrowUp className="size-icon-sm" />
              </IconButton>
            </div>
          </div>
        )}

        <footer className="text-mute border-line mt-2 border-t pt-3 text-xs">
          Opened by {nameOf(issue.created_by, memberOf(issue.created_by))} ·{" "}
          {when(issue.created_at)}
        </footer>
      </div>
      {newLabel !== null && (
        <NewLabelDialog
          name={newLabel}
          onCancel={() => setNewLabel(null)}
          onCreate={(labelName, color) => {
            setNewLabel(null);
            // Two requests, in order: register the label with its colour, then
            // attach it. `label add` on an existing name only attaches, so the
            // colour set here is the one that sticks.
            void send(async () => {
              await rpc(spaceId, { cmd: "label_new", name: labelName, color });
              await rpc(spaceId, { cmd: "label", reff, add: [labelName] });
            });
          }}
        />
      )}
    </aside>
  );
}

function IssueOverflow({
  issueRef,
  active,
  locked,
  tombstone,
  pending,
  onCopyLink,
  onDuplicate,
  onRelate,
  onAddSubIssue,
  onAttach,
  onAssign,
  onMove,
  onStop,
  onRestore,
  onDelete,
}: {
  issueRef: string;
  active: boolean;
  locked: boolean;
  tombstone: boolean;
  pending: boolean;
  onCopyLink: () => void;
  onDuplicate: () => void;
  onRelate: () => void;
  onAddSubIssue: () => void;
  onAttach: () => void;
  onAssign: () => void;
  onMove: () => void;
  onStop: () => void;
  onRestore: () => void;
  onDelete: () => void;
}) {
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <IconButton label="More issue actions"><MoreHorizontal className="size-icon-sm" /></IconButton>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <MenuContent align="end" className="min-w-52">
          <MenuItem onSelect={onCopyLink}><Link2 className="size-icon-sm" /> Copy issue link</MenuItem>
          <MenuItem onSelect={() => void navigator.clipboard.writeText(issueRef)}><Copy className="size-icon-sm" /> Copy reference</MenuItem>
          {!locked && !tombstone && (
            <>
              <MenuItem disabled={pending} onSelect={onDuplicate}><CopyPlus className="size-icon-sm" /> Duplicate issue</MenuItem>
              <MenuItem disabled={pending} onSelect={onRelate}><Link2 className="size-icon-sm" /> Add relation</MenuItem>
              <MenuItem disabled={pending} onSelect={onAddSubIssue}><CornerDownRight className="size-icon-sm" /> Add sub-issue</MenuItem>
              <MenuItem disabled={pending} onSelect={onAttach}><Paperclip className="size-icon-sm" /> Attach a file</MenuItem>
              <MenuItem disabled={pending} onSelect={onAssign}><UserPlus className="size-icon-sm" /> Assign issue</MenuItem>
              <MenuItem disabled={pending} onSelect={onMove}><MoveRight className="size-icon-sm" /> Move to project</MenuItem>
            </>
          )}
          {active && !locked && <MenuItem disabled={pending} onSelect={onStop}><CircleDot className="size-icon-sm" /> Stop work</MenuItem>}
          {!locked && <DropdownMenu.Separator className="bg-line my-1 h-px" />}
          {!locked && (tombstone
            ? <MenuItem onSelect={onRestore}><ArchiveRestore className="size-icon-sm" /> Restore issue</MenuItem>
            : <MenuItem danger onSelect={onDelete}><Trash2 className="size-icon-sm" /> Delete issue</MenuItem>)}
        </MenuContent>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

/** Follow/unfollow (INBOX-9): subscribe to activity without holding the assignment. */
function FollowToggle({
  issue,
  meKey,
  readOnly,
  onToggle,
}: {
  issue: IssueView;
  meKey: string | null;
  readOnly: boolean;
  onToggle: (on: boolean) => void;
}) {
  const followers = issue.followers ?? [];
  const following = meKey != null && followers.includes(meKey);
  const others = followers.length - (following ? 1 : 0);
  return (
    <Button
      type="button"
      variant={following ? "active" : "ghost"}
      disabled={readOnly || meKey == null}
      onClick={() => onToggle(!following)}
      title={following ? "Stop receiving this issue's activity" : "Receive this issue's activity in your inbox"}
    >
      {following ? <BellOff className="size-icon-sm" /> : <Bell className="size-icon-sm" />}
      {following ? "Following" : "Follow"}
      {others > 0 && <span className="text-mute">+{others}</span>}
    </Button>
  );
}

/** Base64 helpers for the attachment payloads (standard alphabet, padded). */
const bufToB64 = (buf: ArrayBuffer): string => {
  const bytes = new Uint8Array(buf);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
};
const b64ToBytes = (b64: string): Uint8Array =>
  Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

/** The engine's cap (contract.rs MAX_ATTACHMENT_BYTES), mirrored for a
 *  friendly refusal before the bytes ever leave the browser. */
const MAX_ATTACHMENT_BYTES = 256 * 1024;

/**
 * Attachments (CREATE-5): bounded files riding the issue document's own
 * sync + encryption. Metadata comes with the view; payloads are fetched only
 * on download.
 */
function Attachments({
  spaceId,
  reff,
  attachments,
  readOnly,
  onError,
}: {
  spaceId: string;
  reff: string;
  attachments: AttachmentMetaDto[];
  readOnly: boolean;
  onError: (m: string) => void;
}) {
  const fileRef = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);

  const upload = async (file: File) => {
    if (file.size > MAX_ATTACHMENT_BYTES) {
      onError(
        `${file.name} is ${Math.ceil(file.size / 1024)} KiB — attachments are capped at ${MAX_ATTACHMENT_BYTES / 1024} KiB`,
      );
      return;
    }
    setBusy(true);
    try {
      const data_b64 = bufToB64(await file.arrayBuffer());
      await rpc(spaceId, {
        cmd: "attach",
        reff,
        name: file.name,
        mime: file.type || null,
        data_b64,
      });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const download = async (att: AttachmentMetaDto) => {
    try {
      const r = await rpc(spaceId, { cmd: "attachment_get", reff, id: att.id });
      if (r.kind !== "attachment") return;
      const bytes = b64ToBytes(r.data_b64);
      const blob = new Blob([bytes.buffer as ArrayBuffer], {
        type: r.mime || "application/octet-stream",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = r.name || att.name;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <>
      {/* Always mounted, never shown: the overflow menu's "Attach a file" opens
          this picker, and it has to exist even when the section below does not.
          The 256 KiB cap is the engine's, mirrored so the refusal happens
          before the bytes leave the browser. */}
      <input
        id="issue-attach"
        ref={fileRef}
        type="file"
        className="hidden"
        disabled={busy || readOnly}
        onChange={(e) => {
          const file = e.target.files?.[0];
          e.target.value = "";
          if (file) void upload(file);
        }}
      />
      {attachments.length > 0 && (
        <Disclosure
          title="Attachments"
          count={attachments.length}
          {...(readOnly
            ? {}
            : {
                action: (
                  <IconButton
                    label="Attach a file"
                    disabled={busy}
                    onClick={() => fileRef.current?.click()}
                  >
                    <Paperclip className="size-icon-sm" />
                  </IconButton>
                ),
              })}
        >
        <ul className="flex flex-col gap-1">
          {attachments.map((att) => (
            <li
              key={att.id}
              className="border-line hover:bg-surface-2 group flex items-center gap-2 rounded-control border px-2 py-1 text-sm"
            >
              <Paperclip className="text-mute size-icon-sm shrink-0" />
              <span className="text-ink min-w-0 flex-1 truncate">{att.name}</span>
              <span className="text-mute shrink-0 text-xs">
                {Math.max(1, Math.round(att.size / 1024))} KiB
              </span>
              <IconButton label={`Download ${att.name}`} onClick={() => void download(att)}>
                <Download className="size-icon-sm" />
              </IconButton>
              {!readOnly && (
                <IconButton
                  label={`Remove ${att.name}`}
                  onClick={() =>
                    void rpc(spaceId, { cmd: "detach", reff, id: att.id }).catch((e) =>
                      onError(e instanceof Error ? e.message : String(e)),
                    )
                  }
                >
                  <Trash2 className="size-icon-sm" />
                </IconButton>
              )}
            </li>
          ))}
        </ul>
        </Disclosure>
      )}
    </>
  );
}

/**
 * The due-date control — the shared `DatePicker` wearing the property row's tone.
 *
 * The engine speaks unix seconds here but the picker speaks `YYYY-MM-DD` (UTC — the
 * engine stores UTC midnight), so this thin wrapper is the one conversion: seconds
 * in via `dueToInput`, and `null` back out becomes the request's `"none"`. The
 * traffic-light tone (overdue/soon/later) rides in as the trigger's colour.
 */
function DueDate({
  value,
  readOnly,
  onChange,
}: {
  value: number | null;
  readOnly: boolean;
  onChange: (due: string) => void;
}) {
  const tone =
    value !== null ? { overdue: "text-danger", soon: "text-warn", later: "" }[dueTone(value)] : "";
  return (
    <DatePicker
      tone="quiet"
      value={value !== null ? dueToInput(value) : null}
      disabled={readOnly}
      placeholder="Add due date"
      className={tone}
      onChange={(next) => onChange(next ?? "none")}
    />
  );
}

/**
 * The kinds of edge a human adds. The engine has three link kinds plus the
 * parent tree; "blocked by" and "sub-issue" are the same verbs with the ends
 * swapped, spelled out because that is how people think about them (and how
 * Linear's relation menu names them).
 */
const RELATION_KINDS = [
  { id: "blocks", label: "Blocks" },
  { id: "blocked-by", label: "Blocked by" },
  { id: "relates", label: "Related to" },
  { id: "duplicates", label: "Duplicate of" },
  { id: "parent", label: "Parent" },
  { id: "sub-issue", label: "Sub-issue (existing)" },
] as const;
type RelationKind = (typeof RELATION_KINDS)[number]["id"];

/**
 * The issue graph — parent, sub-issues, blockers, links — read from `GraphView`,
 * and now written back through it: every edge can be added and removed here
 * (`IssueLink`/`IssueUnlink`/`IssueParent`), and a sub-issue can be created
 * in place (an `issue_new` and then an `issue_parent` — two commits, two
 * activity rows, which is the honest record of what happened).
 *
 * `blocked_by` is the daemon's transitive computation (issues that block this one
 * and are still open), not just direct `blocks` edges — so it's shown as its own
 * warning line and offers no remove: cutting an edge two hops away from here
 * would be action at a distance. The direct edge is removable in its own group.
 */
function Relations({
  graph,
  spaceId,
  reff,
  projectId,
  states,
  readOnly,
  send,
  onNavigate,
  adding,
  setAdding,
  subDraft,
  setSubDraft,
}: {
  graph: GraphView;
  spaceId: string;
  reff: string;
  /** The issue's project — where a quick-created sub-issue is filed. */
  projectId: string;
  states: WorkflowState[];
  readOnly: boolean;
  send: (fn: () => Promise<unknown>) => Promise<void>;
  onNavigate: (reff: string) => void;
  /**
   * Both composers are owned by the page, not by this section. A group that is
   * empty does not render, so its own `+` cannot be the only way to fill it —
   * the first relation and the first sub-issue have to be startable from the
   * overflow menu, which lives a component up.
   */
  adding: boolean;
  setAdding: (open: boolean) => void;
  subDraft: string | null;
  setSubDraft: (draft: string | null) => void;
}) {
  const [kind, setKind] = useState<RelationKind>("blocks");
  /** Every live issue in the space, fetched when the picker first opens. */
  const [candidates, setCandidates] = useState<Row[] | null>(null);

  useEffect(() => {
    if (!adding || candidates !== null) return;
    let alive = true;
    // `all: true` on purpose: a duplicate's canonical is often already Done.
    // Tombstoned rows stay out — linking to a deleted issue is a dead edge.
    void rpc(spaceId, { cmd: "list", project: null, filter: { all: true } })
      .then((r) => {
        if (alive && r.kind === "list") {
          setCandidates(r.rows.filter((x) => !x.tombstone && x.reff !== reff));
        }
      })
      .catch(() => {
        if (alive) setCandidates([]);
      });
    return () => {
      alive = false;
    };
  }, [adding, candidates, spaceId, reff]);

  const relate = (target: string) => {
    setAdding(false);
    void send(() => {
      switch (kind) {
        case "blocked-by":
          // Same edge, other end: `target` blocks this issue.
          return rpc(spaceId, { cmd: "issue_link", reff: target, kind: "blocks", target: reff });
        case "parent":
          return rpc(spaceId, { cmd: "issue_parent", reff, parent: target });
        case "sub-issue":
          return rpc(spaceId, { cmd: "issue_parent", reff: target, parent: reff });
        default:
          return rpc(spaceId, { cmd: "issue_link", reff, kind, target });
      }
    });
  };

  const confirmRemove = (body: string, remove: () => Promise<unknown>) =>
    void ask
      .confirm({
        title: "Remove relationship?",
        body,
        confirmText: "Remove",
        danger: true,
      })
      .then((confirmed) => {
        if (confirmed) return send(remove);
      });

  const unlink = (l: LinkDto) =>
    confirmRemove(`Remove the ${l.kind} relationship with ${l.row.key_alias ?? l.row.reff}?`, () =>
      // `direction` says which end this issue is; the unlink must name the same
      // ordered pair the link did or `blocks`/`duplicates` would miss the edge.
      l.direction === "out"
        ? rpc(spaceId, { cmd: "issue_unlink", reff, kind: l.kind, target: l.row.reff })
        : rpc(spaceId, { cmd: "issue_unlink", reff: l.row.reff, kind: l.kind, target: reff }),
    );

  const createSub = (title: string) => {
    setSubDraft("");
    void send(async () => {
      const r = await rpc(spaceId, { cmd: "issue_new", title, project: projectId });
      if (r.kind === "ref") {
        await rpc(spaceId, { cmd: "issue_parent", reff: r.reff, parent: reff });
      }
    });
  };

  const blocks = graph.links.filter((l) => l.kind === "blocks");
  const related = graph.links.filter((l) => l.kind === "relates");
  const dupes = graph.links.filter((l) => l.kind === "duplicates");
  const doneChildren = graph.children.filter(
    (c) => states.find((s) => s.id === c.status)?.category === "done",
  ).length;

  const empty =
    graph.children.length === 0 &&
    graph.blocked_by.length === 0 &&
    graph.links.length === 0;
  if (empty && readOnly) return null;

  const removable = !readOnly;

  // Every relation that is not the parent tree, in one group. Four captions —
  // Blocked by, Blocks, Related, Duplicates — over one row each is four
  // sections announcing themselves louder than their contents; Linear and Jira
  // both fold them into a single "Links" list and let each row name its own
  // kind. `blocked_by` keeps its warning tone inside that list, because it is
  // the one entry here that is a problem rather than a fact.
  const links: Array<{
    key: string;
    row: Row;
    kind: string;
    tone?: "warn";
    icon: React.ReactNode;
    onRemove?: () => void;
  }> = [
    ...graph.blocked_by.map((r) => ({
      key: `blocked-${r.reff}`,
      row: r,
      kind: "Blocked by",
      tone: "warn" as const,
      icon: <Ban className="text-warn size-icon-xs" />,
    })),
    ...[
      { list: blocks, kind: "Blocks" },
      { list: related, kind: "Related to" },
      { list: dupes, kind: "Duplicate of" },
    ].flatMap(({ list, kind }) =>
      list
        // An inbound `blocks` edge and a transitive `blocked_by` entry are the
        // same fact from two directions, and the graph reports both — so the
        // blocker was listed twice, once as "Blocked by" and once as
        // "Blocks ←". The warning row is the one worth keeping.
        .filter(
          (l) =>
            !(
              kind === "Blocks" &&
              l.direction === "in" &&
              graph.blocked_by.some((b) => b.reff === l.row.reff)
            ),
        )
        .map((l) => ({
        key: `${kind}-${l.direction}-${l.row.reff}`,
        row: l.row,
        // Direction is the glyph's job. Spelling it into the label as well
        // rendered as "← Related to ←".
        kind,
        icon: (
          <span className="text-mute text-2xs" title={l.direction === "in" ? "incoming" : "outgoing"}>
            {l.direction === "in" ? "←" : "→"}
          </span>
        ),
        ...(removable ? { onRemove: () => unlink(l) } : {}),
      })),
    ),
  ];

  return (
    <section className="flex flex-col">
      {(graph.children.length > 0 || subDraft !== null) && (
        <Disclosure
          title="Sub-issues"
          // `done/total`, Linear's sub-issue progress at a glance.
          count={`${doneChildren}/${graph.children.length}`}
          {...(readOnly
            ? {}
            : {
                action: (
                  <IconButton label="Add sub-issue" onClick={() => setSubDraft("")}>
                    <Plus className="size-icon-sm" />
                  </IconButton>
                ),
              })}
        >
          {graph.children.length > 0 && (
            <div
              className="bg-line h-1.5 overflow-hidden rounded-full"
              role="progressbar"
              aria-label="Sub-issue completion"
              aria-valuemin={0}
              aria-valuemax={graph.children.length}
              aria-valuenow={doneChildren}
            >
              <span
                className="bg-ok block h-full rounded-full transition-[width]"
                style={{ width: `${(doneChildren / graph.children.length) * 100}%` }}
              />
            </div>
          )}
          {graph.children.map((r) => (
            <RelRow
              key={r.reff}
              row={r}
              icon={<CornerDownRight className="size-icon-xs" />}
              onNavigate={onNavigate}
              {...(removable
                ? {
                    onRemove: () =>
                      confirmRemove(`Detach ${r.key_alias ?? r.reff} from this issue?`, () =>
                        rpc(spaceId, { cmd: "issue_parent", reff: r.reff, parent: null }),
                      ),
                  }
                : {})}
            />
          ))}
          {subDraft !== null && (
            <Input
              autoFocus
              value={subDraft}
              placeholder="Sub-issue title…  (Enter creates, Esc closes)"
              onChange={(e) => setSubDraft(e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter" && subDraft.trim()) createSub(subDraft.trim());
                if (e.key === "Escape") setSubDraft(null);
              }}
              onBlur={() => {
                if (!subDraft.trim()) setSubDraft(null);
              }}
              aria-label="New sub-issue title"
              className=""
            />
          )}
        </Disclosure>
      )}

      {(links.length > 0 || adding) && (
        <Disclosure
          title="Links"
          count={links.length}
          {...(readOnly
            ? {}
            : {
                action: (
                  <IconButton
                    id="issue-add-relation"
                    label="Add relation"
                    onClick={() => setAdding(true)}
                  >
                    <Plus className="size-icon-sm" />
                  </IconButton>
                ),
              })}
        >
          {links.map((l) => (
            <RelRow
              key={l.key}
              row={l.row}
              icon={l.icon}
              kind={l.kind}
              {...(l.tone ? { tone: l.tone } : {})}
              onNavigate={onNavigate}
              {...(l.onRemove ? { onRemove: l.onRemove } : {})}
            />
          ))}
          {adding && (
            <div className="flex items-center gap-2 py-1">
              <Combobox
                label="Relation"
                value={{
                  id: kind,
                  label: RELATION_KINDS.find((k) => k.id === kind)?.label ?? kind,
                }}
                options={RELATION_KINDS.map((k) => ({ id: k.id, label: k.label }))}
                onPick={(id) => setKind(id as RelationKind)}
              />
              <Combobox
                label="Issue"
                value={null}
                placeholder="Issue…"
                emptyText={candidates === null ? "Loading…" : "No issues"}
                options={(candidates ?? []).map(issueOption)}
                onPick={relate}
              />
              <IconButton label="Cancel" onClick={() => setAdding(false)}>
                <X className="size-icon-sm" />
              </IconButton>
            </div>
          )}
        </Disclosure>
      )}
    </section>
  );
}

/** How an issue reads inside a picker: its handle, then its title; searchable by both. */
function issueOption(r: Row): Option {
  return {
    id: r.reff,
    label: r.title,
    hint: r.key_alias ?? r.reff,
    keywords: [r.reff, ...(r.key_alias ? [r.key_alias] : [])],
  };
}

/**
 * One navigable edge: click opens that issue in this same pane. A `div` holding
 * two buttons rather than one button, because "open" and "remove" are separate
 * gestures and nested buttons are invalid HTML the keyboard can't reach.
 *
 * The row names its own relation now. Four groups of one — Blocked by, Blocks,
 * Related, Duplicates — spent four captions on four rows; carrying the kind in
 * the row lets all of them live in one list, which is how Linear and Jira both
 * draw it.
 */
function RelRow({
  row,
  icon,
  kind,
  tone,
  onNavigate,
  onRemove,
}: {
  row: Row;
  icon: React.ReactNode;
  /** What this edge *is* — omitted for sub-issues, where the group says it. */
  kind?: string;
  tone?: "warn";
  onNavigate: (reff: string) => void;
  onRemove?: () => void;
}) {
  return (
    <div className="group/rel -mx-1 flex items-center gap-2 rounded-control px-1 py-0.5 text-sm">
      <Button
        onClick={() => onNavigate(row.reff)}
        className="min-w-0 flex-1 shrink justify-start px-1 text-left"
      >
        <span className="flex size-icon-xs shrink-0 items-center justify-center">{icon}</span>
        {kind && (
          <span
            className={cn(
              "w-20 shrink-0 truncate text-2xs",
              tone === "warn" ? "text-warn" : "text-mute",
            )}
          >
            {kind}
          </span>
        )}
        <span className="text-mute w-16 shrink-0 truncate font-mono text-2xs tabular-nums">
          {row.key_alias ?? row.reff}
        </span>
        <span className="min-w-0 flex-1 truncate font-medium">{row.title}</span>
      </Button>
      {onRemove && (
        <IconButton
          label="Remove relation"
          onClick={onRemove}
          // Revealed on row hover/focus: the affordance is there when wanted and
          // the list stays quiet the rest of the time.
          className="opacity-0 group-hover/rel:opacity-100 focus-visible:opacity-100"
        >
          <X className="size-icon-xs" />
        </IconButton>
      )}
    </div>
  );
}

type Entry =
  | { at: number; order: number; kind: "comment"; comment: CommentDto }
  | { at: number; order: number; kind: "event"; event: ActivityEvent };

/**
 * Comments and activity, in one chronological stream.
 *
 * The two halves come from different places, and the events one changed under this
 * pane: `Request::History` now reads the issue's oplog **on disk** (`engine::history`)
 * rather than a session ring. So the timeline is durable — it survives daemon
 * restarts — and every event carries the *real* committer in `actor`, a teammate
 * included. The daemon leaves `actor_nick` empty, so the name is resolved here
 * against the member list (see `describeEvent`); reading `actor_nick` — as this used
 * to — now shows nothing.
 *
 * - **Comments come from the issue document.** They sync and carry a real author.
 * - **Events come from the durable oplog.** Real actors, real timestamps, no
 *   synthetic `synced` marker (that belongs to the space Activity feed).
 *
 * `commented` events are dropped: a comment is already rendered from the document,
 * so keeping its event too would double-print it.
 *
 * The visual weight follows the split. A comment is a card with a face; an event is
 * one muted line. That is Better Stack's timeline and Linear's, for the same reason
 * in both: the events are context, the comments are the conversation, and drawing
 * them alike makes you read the furniture.
 */
function Timeline({
  events,
  comments,
  memberOf,
  states,
  graph,
  readOnly,
  meKey,
  onReact,
  onReply,
  onCopyLink,
  onCreateFromComment,
}: {
  events: ActivityEvent[];
  comments: CommentDto[];
  memberOf: (key: string) => MemberDto | undefined;
  /** The workflow, so a status change can say "Backlog", not "backlog". */
  states: WorkflowState[];
  /** The issue's graph neighborhood — how a link event names the other issue. */
  graph: GraphView | null;
  readOnly: boolean;
  /** My member key — how "did I already react" is answered. */
  meKey: string | null;
  onReact: (comment: string, emoji: string, on: boolean) => void;
  onReply: (replyTo: string, body: string) => void;
  onCopyLink: (commentId: string) => void;
  onCreateFromComment: (body: string) => void;
}) {
  const [visibleCount, setVisibleCount] = useState(40);
  // The naming policy lives here, where the member list is: a key becomes an alias,
  // "you", or a short prefix. `describeEvent` only decides *whether* there is a name.
  const resolveName: NameResolver = (key) => nameOf(key, memberOf(key));
  // Link/parent events carry the other issue's doc id; the graph is the one piece
  // of state already at hand that can turn it into "NIX-90 Title…". An issue that
  // has since left the neighborhood falls back to a short id — rare, and honest.
  const issueLabel = useMemo(() => {
    const map = new Map<string, string>();
    const add = (r: Row | null | undefined) => {
      if (!r) return;
      const title = r.title.length > 40 ? `${r.title.slice(0, 40)}…` : r.title;
      map.set(r.doc_id, `${r.key_alias ?? r.reff} ${title}`);
    };
    if (graph) {
      add(graph.parent);
      graph.children.forEach(add);
      graph.blocked_by.forEach(add);
      graph.links.forEach((l) => add(l.row));
    }
    return (doc: string) => map.get(doc) ?? null;
  }, [graph]);
  const phraseCtx: EventPhraseContext = {
    resolveName,
    stateName: (id) => states.find((s) => s.id === id)?.name ?? null,
    issueLabel,
  };
  const entries = useMemo<Entry[]>(() => {
    const out: Entry[] = [
      // Roots only: a reply renders nested under its parent, not as its own
      // timeline entry — the thread reads as one exchange.
      ...comments
        .filter((c) => !c.parent)
        .map((c, i) => ({ at: c.ts, order: i, kind: "comment" as const, comment: c })),
      ...events
        .filter((e) => e.kind !== "commented")
        .map((e) => ({ at: e.ts, order: e.seq, kind: "event" as const, event: e })),
    ];
    // Oldest first — a timeline you read downward, like the conversation it is.
    // `order` breaks ties: whole-second stamps mean a burst of edits all land on
    // the same `ts`, and without it they shuffle on every render.
    return out.sort((a, b) => a.at - b.at || a.order - b.order);
  }, [events, comments]);

  const repliesByParent = useMemo(() => {
    const indexed = new Map<string, CommentDto[]>();
    for (const comment of comments) {
      if (!comment.parent) continue;
      indexed.set(comment.parent, [...(indexed.get(comment.parent) ?? []), comment]);
    }
    return indexed;
  }, [comments]);
  const visibleEntries = boundedTail(entries, visibleCount);

  return (
    <section id="issue-activity" className="flex flex-col gap-3 scroll-mt-3">
      {/* A true title, not the uppercase micro-label the rail sections use: this
          is the page's second heading (Linear draws it the same way), and the
          conversation below it deserves more than furniture-weight type. */}
      <div className="flex min-h-ctl-sm items-center gap-2">
        <h3 className="text-fg text-base font-semibold">Activity</h3>
        {comments.length > 0 && (
          <span className="text-mute text-xs">{comments.length} comments</span>
        )}
        <span
          title="This issue's full history, read from its change log on disk — it survives restarts and shows who made each change. (The space-wide Activity view is a lighter, per-session feed.)"
          className="text-mute ml-auto cursor-help"
        >
          <Info className="size-icon-xs" />
        </span>
      </div>

      {entries.length === 0 && <p className="text-mute text-sm">Nothing yet.</p>}
      {entries.length > visibleCount && (
        <Button
          variant="ghost"
          onClick={() => setVisibleCount((count) => count + 40)}
          className="self-start"
        >
          Show {Math.min(40, entries.length - visibleCount)} earlier changes
        </Button>
      )}

      {visibleEntries.map((entry) =>
        entry.kind === "comment" ? (
          <Comment
            key={`c${entry.order}`}
            comment={entry.comment}
            replies={entry.comment.id ? (repliesByParent.get(entry.comment.id) ?? []) : []}
            memberOf={memberOf}
            readOnly={readOnly}
            meKey={meKey}
            onReact={onReact}
            onReply={onReply}
            onCopyLink={onCopyLink}
            onCreateFromComment={onCreateFromComment}
          />
        ) : (
          <Event
            key={`e${entry.order}`}
            event={entry.event}
            states={states}
            memberOf={memberOf}
            ctx={phraseCtx}
          />
        ),
      )}
    </section>
  );
}

/** The fixed reaction palette — Linear's set, no free-typing an emoji here
 *  (the engine accepts any single emoji; the CLI can send exotic ones). */
const REACTION_EMOJIS = ["👍", "❤️", "🎉", "😄", "🚀", "👀"] as const;

/**
 * One comment thread, as a card: the root comment, its replies flush beneath it,
 * and a standing reply composer as the card's footer. Linear's shape, for
 * Linear's reason — the card is what separates the conversation from the event
 * furniture around it, and a composer that is already there makes replying one
 * keystroke instead of a hover hunt.
 */
function Comment({
  comment: c,
  replies,
  memberOf,
  readOnly,
  meKey,
  onReact,
  onReply,
  onCopyLink,
  onCreateFromComment,
}: {
  comment: CommentDto;
  replies: CommentDto[];
  memberOf: (key: string) => MemberDto | undefined;
  readOnly: boolean;
  meKey: string | null;
  onReact: (comment: string, emoji: string, on: boolean) => void;
  onReply: (replyTo: string, body: string) => void;
  onCopyLink: (commentId: string) => void;
  onCreateFromComment: (body: string) => void;
}) {
  const block = (comment: CommentDto) => (
    <CommentBlock
      comment={comment}
      memberOf={memberOf}
      readOnly={readOnly}
      meKey={meKey}
      onReact={onReact}
      onCopyLink={onCopyLink}
      onCreateFromComment={onCreateFromComment}
    />
  );
  return (
    <article className="border-line bg-raised shadow-raised rounded-surface border">
      {block(c)}
      {replies.map((r, i) => (
        <div key={r.id ?? `r${i}`} className="border-line border-t">
          {block(r)}
        </div>
      ))}
      {/* Pre-identity comments (no id) cannot anchor replies — the composer
          simply doesn't exist for them, rather than existing and failing.
          Replies to a reply re-anchor to the root: one level. */}
      {!readOnly && !!c.id && (
        <ReplyComposer meKey={meKey} memberOf={memberOf} onSubmit={(body) => onReply(c.id!, body)} />
      )}
    </article>
  );
}

/** A single comment inside the card: header (face, name, time, actions), body,
 *  reaction chips. Shared by the root comment and each reply. */
function CommentBlock({
  comment: c,
  memberOf,
  readOnly,
  meKey,
  onReact,
  onCopyLink,
  onCreateFromComment,
}: {
  comment: CommentDto;
  memberOf: (key: string) => MemberDto | undefined;
  readOnly: boolean;
  meKey: string | null;
  onReact: (comment: string, emoji: string, on: boolean) => void;
  onCopyLink: (commentId: string) => void;
  onCreateFromComment: (body: string) => void;
}) {
  const member = memberOf(c.author);
  const [picking, setPicking] = useState(false);
  // Pre-identity comments (no id) cannot anchor reactions or links — the
  // affordances simply don't exist for them, rather than existing and failing.
  const canAct = !readOnly && !!c.id;

  return (
    <div className="group/comment p-3">
      <div className="flex items-center gap-2">
        <Avatar
          deviceKey={c.author}
          // The in-doc `author_nick` is what the author *claimed*; the local alias is
          // what you decided they are. Prefer yours — it is the half that was verified.
          alias={member?.alias || c.author_nick || ""}
          me={member?.me ?? false}
        />
        <span className="min-w-0 truncate font-medium">
          {member ? nameOf(c.author, member) : (c.author_nick ?? short(c.author))}
        </span>
        {/* Unix SECONDS — `tsToDate` is the only place that's converted. */}
        <time className="text-mute shrink-0 text-xs" dateTime={tsToDate(c.ts).toISOString()}>
          {when(c.ts)}
        </time>
        {canAct && (
          /* Anchored in the header, so revealing them never reflows the thread. */
          <span className="ml-auto flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover/comment:opacity-100 focus-within:opacity-100 has-[[data-state=open]]:opacity-100">
            <Popover.Root open={picking} onOpenChange={setPicking}>
              <Popover.Trigger asChild>
                <IconButton aria-label="Add reaction" label="Add reaction">
                  <SmilePlus className="size-icon-sm" />
                </IconButton>
              </Popover.Trigger>
              <PopoverContent align="end" className="flex gap-0.5 p-1">
                {REACTION_EMOJIS.map((emoji) => (
                  <Button
                    key={emoji}
                    onClick={() => {
                      setPicking(false);
                      if (c.id) onReact(c.id, emoji, true);
                    }}
                    aria-label={`React ${emoji}`}
                    size="icon"
                    className="text-base"
                  >
                    {emoji}
                  </Button>
                ))}
              </PopoverContent>
            </Popover.Root>
            <DropdownMenu.Root>
              <DropdownMenu.Trigger asChild>
                <IconButton label="Comment actions">
                  <MoreHorizontal className="size-icon-sm" />
                </IconButton>
              </DropdownMenu.Trigger>
              <DropdownMenu.Portal>
                <MenuContent align="end" className="min-w-44">
                  <MenuItem onSelect={() => onCopyLink(c.id!)}>
                    <Link2 className="size-icon-sm" /> Copy link
                  </MenuItem>
                  <MenuItem onSelect={() => onCreateFromComment(c.body)}>
                    <CopyPlus className="size-icon-sm" /> New issue from comment
                  </MenuItem>
                </MenuContent>
              </DropdownMenu.Portal>
            </DropdownMenu.Root>
          </span>
        )}
      </div>
      <Markdown text={c.body} density="tight" className="mt-1.5" />
      {(c.reactions?.length ?? 0) > 0 && (
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          {(c.reactions ?? []).map((r) => {
            const mine = meKey !== null && r.actors.includes(meKey);
            return (
              <ChipButton
                key={r.emoji}
                disabled={!canAct}
                onClick={() => c.id && onReact(c.id, r.emoji, !mine)}
                title={r.actors.map((a) => nameOf(a, memberOf(a))).join(", ")}
                aria-pressed={mine}
              >
                {r.emoji}
                <span className="tabular-nums">{r.actors.length}</span>
              </ChipButton>
            );
          })}
        </div>
      )}
    </div>
  );
}

/** The card's standing footer: your face, a field, a send button. Always there,
 *  like Linear's "Leave a reply…" — not summoned from a hover menu. */
function ReplyComposer({
  meKey,
  memberOf,
  onSubmit,
}: {
  meKey: string | null;
  memberOf: (key: string) => MemberDto | undefined;
  onSubmit: (body: string) => void;
}) {
  const [draft, setDraft] = useState("");
  const submit = () => {
    const body = draft.trim();
    if (!body) return;
    onSubmit(body);
    setDraft("");
  };
  const me = meKey ? memberOf(meKey) : undefined;
  return (
    <div className="border-line flex items-center gap-2 border-t p-2 pl-3">
      {meKey && <Avatar deviceKey={meKey} alias={me?.alias ?? ""} me size="sm" />}
      <textarea
        value={draft}
        placeholder="Leave a reply…"
        rows={Math.min(6, draft.split("\n").length)}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && draft.trim()) {
            e.preventDefault();
            submit();
          }
        }}
        aria-label="Reply"
        className="placeholder:text-mute min-w-0 flex-1 resize-none bg-transparent py-0.5 text-sm outline-none"
      />
      <IconButton
        label="Attach a file"
        onClick={() => document.getElementById("issue-attach")?.click()}
        className="self-end"
      >
        <Paperclip className="size-icon-sm" />
      </IconButton>
      <IconButton
        label="Reply"
        chord="Ctrl/⌘ ↵"
        disabled={!draft.trim()}
        onClick={submit}
        className="self-end"
      >
        <ArrowUp className="size-icon-sm" />
      </IconButton>
    </div>
  );
}

function Event({
  event: e,
  states,
  memberOf,
  ctx,
}: {
  event: ActivityEvent;
  states: WorkflowState[];
  memberOf: (key: string) => MemberDto | undefined;
  ctx: EventPhraseContext;
}) {
  const { actor, phrase } = describeEventRich(e, ctx);

  return (
    <div className="text-mute flex items-start gap-2 text-xs">
      {/* A fixed column as wide as a comment avatar, so the timeline's icons and
          the cards' faces share one left edge. */}
      <span className="flex h-4 w-avatar-md shrink-0 items-center justify-center">
        <EventGlyph event={e} states={states} memberOf={memberOf} />
      </span>
      <span className="min-w-0 flex-1">
        {/* No actor means we genuinely don't know — see core/activity.ts. Printing
            "someone" would claim we know there was a someone and lost the name. */}
        {actor && <span className="text-dim font-medium">{actor} </span>}
        {phrase}
        <span aria-hidden="true"> · </span>
        <time dateTime={tsToDate(e.ts).toISOString()}>{when(e.ts)}</time>
        {/* A concurrent overwrite is worth flagging but never worth blocking on
            (A§9): last-writer-wins already resolved it; you just get told. */}
        {e.collision && (
          <AlertTriangle
            className="text-warn size-icon-xs ml-1 inline-block align-text-top"
            aria-label="Concurrent overwrite"
          />
        )}
      </span>
    </div>
  );
}

/**
 * The icon column earns its width: each event kind draws its own glyph — the
 * target state's circle for a move, the priority bars for a priority change, the
 * actor's face for "created the issue" — so the timeline can be scanned by shape
 * before it is read.
 */
function EventGlyph({
  event: e,
  states,
  memberOf,
}: {
  event: ActivityEvent;
  states: WorkflowState[];
  memberOf: (key: string) => MemberDto | undefined;
}) {
  if (e.kind === "created" && e.actor) {
    const member = memberOf(e.actor);
    return (
      <Avatar deviceKey={e.actor} alias={member?.alias ?? ""} me={member?.me ?? false} size="sm" />
    );
  }
  if (EDIT_KINDS.has(e.kind)) {
    const changed = (field: string) =>
      e.changes.find((c) => c.field === field && (c.from ?? "—") !== (c.to ?? "—"));
    const status = changed("status");
    if (status?.to) {
      const s = states.find((st) => st.id === status.to);
      if (s) return <StatusIcon category={s.category} color={catalogColor(s.color)} />;
    }
    const priority = changed("priority");
    if (priority) return <PriorityIcon priority={(priority.to ?? "none") as Priority} />;
    if (changed("duedate")) return <CalendarDays className="size-icon-sm" />;
    if (changed("estimate")) return <Gauge className="size-icon-sm" />;
    if (e.changes.some((c) => c.field === "assignees")) {
      return e.changes.some((c) => c.field === "assignees" && c.to) ? (
        <UserPlus className="size-icon-sm" />
      ) : (
        <UserMinus className="size-icon-sm" />
      );
    }
    return <Pencil className="size-icon-sm" />;
  }
  switch (e.kind) {
    case "assigned":
      return <UserPlus className="size-icon-sm" />;
    case "unassigned":
      return <UserMinus className="size-icon-sm" />;
    case "labeled":
      return <Tag className="size-icon-sm" />;
    case "linked":
    case "unlinked":
      if (e.text.startsWith("blocks ")) return <Ban className="text-danger size-icon-sm" />;
      if (e.text.startsWith("duplicates ")) return <CopyPlus className="size-icon-sm" />;
      return <Link2 className="size-icon-sm" />;
    case "parented":
      return <CornerDownRight className="size-icon-sm" />;
    case "milestoned":
      return <Milestone className="size-icon-sm" />;
    case "cycled":
      return <RefreshCw className="size-icon-sm" />;
    case "attached":
    case "detached":
      return <Paperclip className="size-icon-sm" />;
    case "deleted":
      return <Trash2 className="size-icon-sm" />;
    case "restored":
      return <ArchiveRestore className="size-icon-sm" />;
    case "moved":
      return <MoveRight className="size-icon-sm" />;
    default:
      return <CircleDot className="size-icon-xs" />;
  }
}

/**
 * Description: a draft you commit, not a field that saves per keystroke — a
 * doorbell mid-typing would otherwise fight the cursor.
 *
 * There is no edit mode left. The body is a live Markdown document: typing
 * `## ` makes a heading where the caret is, and the markup never appears as
 * markup. What used to be here — rendered prose that swapped to a raw textarea
 * on click — meant the thing you wrote and the thing everyone read were never
 * on screen at the same time.
 *
 * `readOnly` still renders through `core/markdown.ts` rather than a disabled
 * editor: a locked issue should cost nothing to display, and the read-only
 * renderer is the one that draws callouts and Shiki-highlighted code.
 */
function Description({
  draftKey,
  value,
  readOnly,
  onSave,
}: {
  draftKey: { spaceId: string; reff: string };
  value: string;
  readOnly: boolean;
  onSave: (v: string) => void;
}) {
  const [draft, setDraft] = useState(
    () => loadDraft(draftKey.spaceId, draftKey.reff, "description") || value,
  );
  const dirty = useRef(draft !== value);

  useEffect(() => {
    if (dirty.current && draft !== value) {
      saveDraft(draftKey.spaceId, draftKey.reff, "description", draft);
    }
  }, [draftKey.spaceId, draftKey.reff, draft, value]);

  if (readOnly) {
    return (
      <div className="min-h-ctl-xl py-2">
        {value ? <Markdown text={value} /> : <span className="text-mute">No description</span>}
      </div>
    );
  }

  return (
    <MarkdownEditor
      value={value}
      placeholder="Add description…"
      className="min-h-ctl-xl py-2"
      onChange={(markdown) => {
        dirty.current = true;
        setDraft(markdown);
      }}
      onCommit={() => {
        if (!dirty.current) return;
        dirty.current = false;
        clearDraft(draftKey.spaceId, draftKey.reff, "description");
        // `draft` is a keystroke behind on the very last character, so read the
        // committed value from state at call time rather than closing over it.
        setDraft((current) => {
          if (current !== value) onSave(current);
          return current;
        });
      }}
    />
  );
}
