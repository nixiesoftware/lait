import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
  Stamp,
  Tag,
  Trash2,
  Unlink,
  UserMinus,
  UserPlus,
  X,
} from "lucide-react";

import { rpc } from "../api";
import { downloadUrl, upload as uploadContent } from "../content";
import { useIssueDetail, useProjectBaselines, useProjectViewerStore } from "../projectStore";
import { clearDraft, loadDraft, saveDraft } from "../core/drafts";
import {
  describeEventRich,
  EDIT_KINDS,
  type EventPhraseContext,
  type NameResolver,
} from "../core/activity";
import { codeUnitSpan } from "../core/anchor";
import { continueTextPreview, textRevision } from "../core/textPreview";
import {
  awarenessReadyFor,
  caretPhrase,
  carets,
  previews,
  typists,
  useLiveAwareness,
  useLiveMutation,
  useLiveTable,
  watching,
  type LiveState,
} from "../live";
import type { BrowserTextPreview } from "../socket";
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
  type Packet,
  type PacketSpec,
  type ProjectDto,
  type WorkflowState,
} from "../types";
import { conflictPhrase, sourcePhrase } from "../core/specs";
import { Avatar, AvatarStack, memberName as nameOf, stackFor } from "./Avatar";
import { LoadingState } from "./AppState";
import { avatarColor, catalogColor } from "./colors";
import { PriorityIcon, ProgressRing, StatusIcon } from "./icons";
import { Markdown } from "./Markdown";
import { MarkdownEditor } from "./MarkdownEditor";
import type {
  RemoteContext,
  RemoteCursor,
  RemoteTextPreview,
  TextSplice,
} from "./CodeMirrorEditor";
import { DatePicker } from "./DatePicker";
import { NewLabelDialog } from "./NewLabel";
import { Combobox, type Option } from "./Picker";
import { Button, Divider, DropdownMenu, DropdownMenuItem, IconButton, Popover, TextInput } from "@astryxdesign/core";
import { ChipButton, LabelChip, cn, interactiveRow } from "./primitives";
import { Disclosure, HeaderActions, RailRow, RailSection, Toast } from "./layout";
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
  onOpenSpec,
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
  /** Open a Spec named by the effective brief, on its own surface. */
  onOpenSpec?: ((spec: string) => void) | undefined;
  onClose: () => void;
  onPrevious?: () => void;
  onNext?: () => void;
}) {
  const projectStore = useProjectViewerStore();
  const detail = useIssueDetail(spaceId, reff);
  const issue = detail.issue;
  const live = useLiveTable(spaceId, issue?.doc_id ?? null);
  const publishAwareness = useLiveAwareness(issue?.doc_id ?? "");
  const liveMutation = useLiveMutation();
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
        <IconButton
          label="Previous issue"
          onClick={() => onPrevious?.()}
          isDisabled={!onPrevious}
          variant="ghost"
          size="sm"
          tooltip="Previous issue"
          icon={<ChevronLeft className="size-icon-sm" />}
        />
        <IconButton
          label="Next issue"
          onClick={() => onNext?.()}
          isDisabled={!onNext}
          variant="ghost"
          size="sm"
          tooltip="Next issue"
          icon={<ChevronRight className="size-icon-sm" />}
        />
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
        <IconButton
          label="Close issue"
          onClick={onClose}
          variant="ghost"
          size="sm"
          tooltip="Close issue  Esc"
          icon={<X className="size-icon-sm" />}
        />
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
                           onClick={() => {
                const action = undoWork.action;
                setUndoWork(null);
                void runWorkAction(action, false);
              }}
                           label="Undo"
                           variant="ghost"
                           size="sm"
                         />}>
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
              onClick={() => document.getElementById("issue-activity")?.scrollIntoView({ block: "start" })}
              label="Review history"
              variant="ghost"
              size="sm"
            />
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
        {/* `text-dim`, not `text-fg`: the rail is reference you read *against*
            the document, and at equal weight the two competed — a status and a
            title cannot both be the loudest thing on the page. One step down
            puts the captions (`mute`), the values (`dim`) and the body (`fg`)
            on three rungs of one ladder, which is the relationship Linear's
            rail keeps. Icons and label chips carry their own colour and are
            untouched; only inherited text moves. */}
        <div className="issue-detail-properties text-dim flex flex-col text-sm">
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
                // `who` takes a **full 64-hex key** — the store method documents
                // why. The key is what we hold and the key is what we send.
                void runCommand(
                  projectStore.toggleAssignee(spaceId, reff, key, !issue.assignees.includes(key)),
                );
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
                void runCommand(projectStore.setEstimate(spaceId, reff, id))
              }
            />
          </RailRow>

          <RailRow label="Due date">
            <DueDate
              value={issue.due_date ?? null}
              readOnly={locked}
              onChange={(due) =>
                void runCommand(projectStore.setDue(spaceId, reff, due === "none" ? null : due))
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
                    void runCommand(
                      projectStore.swapLabel(spaceId, reff, name, next === "__remove__" ? null : next),
                    );
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
                  void runCommand(
                    projectStore.toggleLabel(spaceId, reff, name, !issue.label_names.includes(name)),
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
                // A row, not an action. Astryx's Button adds its own padding and a
                // pill radius, which fight the row's `px-1` and its flat shape.
                <button
                type="button"
                onClick={() => onNavigate(graph.parent!.reff)}
                className={cn(interactiveRow(), "-mx-1 min-w-0 justify-start px-1 text-left")}
              >
                <GitMerge className="text-mute size-icon-sm shrink-0" />
                <span className="min-w-0 truncate font-medium">{graph.parent.title}</span>
              </button>
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

          <LiveRail
            live={live}
            members={members}
            memberOf={memberOf}
          />
        </div>

        <Description
          draftKey={{ spaceId: canonicalSpaceId, reff }}
          value={issue.description}
          readOnly={locked}
          remoteCursors={carets(live.entries)
            .filter((mark) => mark.field === "description" && mark.position.caret === "at")
            .map<RemoteCursor>((mark) => ({
              actor: mark.actor,
              name: nameOf(mark.actor, memberOf(mark.actor)),
              color: avatarColor(mark.actor),
              anchor: mark.position.caret === "at" ? mark.position.position : 0,
              ...(mark.focus?.caret === "at" ? { focus: mark.focus.position } : {}),
              uncertain: mark.uncertain,
            }))}
          remoteContexts={carets(live.entries)
            .filter((mark) => mark.field === "description" && mark.position.caret !== "at")
            .map<RemoteContext>((mark) => ({
              actor: mark.actor,
              name: nameOf(mark.actor, memberOf(mark.actor)),
              color: avatarColor(mark.actor),
              uncertain: mark.uncertain,
            }))}
          remotePreviews={previews(live.entries)
            .filter((mark) => mark.field === "description")
            .map<RemoteTextPreview>((mark) => ({
              actor: mark.actor,
              name: nameOf(mark.actor, memberOf(mark.actor)),
              color: avatarColor(mark.actor),
              base: mark.preview.base,
              result: mark.preview.result,
              index: mark.preview.index,
              delete: mark.preview.delete,
              insert: mark.preview.insert,
              ...(mark.preview.anchor === null ? {} : { anchor: mark.preview.anchor }),
              ...(mark.preview.focus === null ? {} : { focus: mark.preview.focus }),
              uncertain: mark.uncertain,
            }))}
          onAwareness={(anchor, focus, typing, ready, preview) =>
            publishAwareness({
              cursor: anchor === null
                ? null
                : {
                    field: "description",
                    anchor,
                    ...(focus === null ? {} : { focus }),
              },
              typing,
              preview,
              ...(!ready ? { defer: true } : {}),
            })
          }
          onSplice={(splice) => liveMutation(spaceId, {
            cmd: "issue_text_splice",
            reff,
            ...splice,
          })}
          onCheckpoint={() => liveMutation(spaceId, { cmd: "issue_text_checkpoint", reff })}
          onReadLatest={async () => {
            const response = await liveMutation(spaceId, { cmd: "issue_view", reff });
            if (response.kind !== "issue") throw new Error("Issue description is unavailable");
            return response.description;
          }}
          onError={onError}
        />

        <SpecPacket
          spaceId={spaceId}
          reff={issue.reff}
          projectId={issue.project_id}
          readOnly={locked}
          onOpenSpec={onOpenSpec}
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
            members={members}
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
          description={issue.description}
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
              className="placeholder:text-mute block w-full resize-none bg-transparent px-4 py-3 outline-none"
              aria-label="New comment"
              aria-describedby={commentError ? "comment-error" : undefined}
            />
            <div className="flex items-center gap-2 px-2.5 pb-2.5">
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
                variant="ghost"
                size="sm"
                tooltip="Attach a file"
                icon={<Paperclip className="size-icon-sm" />}
              />
              <IconButton
                label={commentError ? "Retry comment" : "Comment"}
                isDisabled={!comment.trim()}
                isLoading={commentPending}
                onClick={() => void submitComment()}
                variant="ghost"
                size="sm"
                tooltip={`${commentError ? "Retry comment" : "Comment"}  ${"Ctrl/⌘ ↵"}`}
                icon={<ArrowUp className="size-icon-sm" />}
              />
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

/**
 * The work brief — what governs this issue, derived rather than written.
 *
 * A projection, never a second source of truth: nothing here is editable, no
 * body text is copied in, and the sections are the engine's own classification
 * rather than a reading of it. What the block adds is the part a reader cannot
 * compute — *why* each item is in force, and whether the brief is whole.
 *
 * Governing leads and stays open. The rest carry counts and expand on demand,
 * because "what must I satisfy" is the question an issue is being worked to
 * answer, and guidance, evidence and records are the ones asked afterwards.
 */
function SpecPacket({
  spaceId,
  reff,
  projectId,
  readOnly,
  onOpenSpec,
}: {
  spaceId: string;
  reff: string;
  projectId: string;
  readOnly: boolean;
  onOpenSpec?: ((spec: string) => void) | undefined;
}) {
  const store = useProjectViewerStore();
  const [packet, setPacket] = useState<Packet | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [binding, setBinding] = useState(false);
  const baselines = useProjectBaselines(spaceId, projectId).data ?? [];

  useEffect(() => {
    let alive = true;
    setPacket(null);
    setError(null);
    void rpc(spaceId, { cmd: "packet", reff })
      .then((response) => {
        if (alive && response.kind === "packet") setPacket(response);
      })
      .catch((reason) => {
        if (alive) setError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => { alive = false; };
  }, [spaceId, reff, reload]);

  // Only issued sets can be pinned: binding to a draft would pin something
  // nobody has agreed to, and the pin is the agreement.
  const issuedBaselines = baselines.filter((candidate) => candidate.issued.length === 1);
  const bind = (baseline: { baseline: string; revision: string } | null) => {
    setBinding(false);
    void store
      .bindBaseline(spaceId, reff, baseline)
      .then(() => setReload((n) => n + 1))
      .catch((reason: unknown) =>
        setError(reason instanceof Error ? reason.message : String(reason)),
      );
  };

  // Unlike the sections below, the binding control is drawn even with nothing
  // bound — "this issue is governed by no agreed set" is a fact worth being able
  // to see and change, not an absence.
  const offerBinding = !readOnly && issuedBaselines.length > 0;
  if (!packet && !error && !offerBinding) return null;
  const sections = packet ? [
    ["Governing", packet.governing],
    ["Guidance", packet.guidance],
    ["Proof", packet.proof],
    ["Record", packet.record],
  ] as const : [];
  // Genuinely empty — no baseline, nothing governing directly, nothing wrong —
  // is the only state that draws nothing. An unreadable packet is an error, and
  // an incomplete one is a warning; neither may render as "there is nothing".
  const empty = packet && sections.every(([, specs]) => specs.length === 0)
    && packet.conflicts.length === 0 && !packet.baseline;
  if (empty && !offerBinding) return null;

  const conflicts = (packet?.conflicts ?? []).map(conflictPhrase);
  const waiting = conflicts.filter((conflict) => conflict.kind === "missing").length;

  return (
    <section className="border-line mx-6 border-t py-5">
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <h2 className="text-sm font-semibold">Effective for this issue</h2>
        {packet?.baseline && (
          <code
            className="text-mute text-2xs"
            title={`${packet.baseline.baseline}@${packet.baseline.revision}`}
          >
            {baselines.find((row) => row.baseline === packet.baseline!.baseline)?.body.name ??
              packet.baseline.baseline}{" "}
            · {short(packet.baseline.revision)}
          </code>
        )}
        {offerBinding && (
          <DropdownMenu
            isMenuOpen={binding}
            onOpenChange={setBinding}
            alignment="end"
            button={{
              className: "ml-auto",
              label: packet?.baseline ? "Change baseline" : "Bind a baseline",
              variant: "secondary",
              elevation: "low",
              size: "md",
            }}
          >
            {issuedBaselines.map((candidate) => (
              <DropdownMenuItem
                key={candidate.baseline}
                label={candidate.body.name}
                // The same stamp the Spec lifecycle uses for `issued` — these
                // candidates are exactly the issued baselines, so the glyph is
                // already spoken for.
                icon={<Stamp className="size-icon-sm" />}
                onClick={() =>
                  bind({ baseline: candidate.baseline, revision: candidate.issued[0]! })
                }
                // The exact revision, because that is what gets pinned — not the
                // baseline, and not whatever it becomes later.
                endContent={<code className="text-mute text-2xs">{short(candidate.issued[0]!)}</code>}
              />
            ))}
            {packet?.baseline && (
              <>
                <Divider />
                <DropdownMenuItem
                  label={<span className="text-danger">Clear binding</span>}
                  icon={<Unlink className="size-icon-sm text-danger" />}
                  onClick={() => bind(null)}
                />
              </>
            )}
          </DropdownMenu>
        )}
      </div>
      {offerBinding && !packet?.baseline && (
        <p className="text-mute mb-2 text-2xs">
          No agreed set is pinned to this issue. What governs it is whatever names it directly.
        </p>
      )}
      {/* The integrity line, before the content it qualifies. "Waiting" and
          "unresolved" are different remedies — one arrives with a sync, the
          other needs somebody to decide — so they are counted apart. */}
      {conflicts.length > 0 && (
        <p className="text-mute mb-2 text-2xs">
          {waiting > 0 && `${waiting} referenced ${waiting === 1 ? "record has" : "records have"} not arrived here yet. `}
          {conflicts.length > waiting && `${conflicts.length - waiting} need a decision.`}
        </p>
      )}
      {error && <p className="text-danger text-xs">{error}</p>}
      {conflicts.map((conflict) => (
        <p key={conflict.text} className="text-warn mb-2 flex items-start gap-2 text-xs">
          <AlertTriangle className="mt-0.5 size-icon-xs shrink-0" />
          {conflict.text}
        </p>
      ))}
      <div className="flex flex-col gap-3">
        {sections.map(([title, specs]) => specs.length > 0 && (
          <PacketSection
            key={title}
            title={title}
            specs={specs}
            open={title === "Governing"}
            onOpenSpec={onOpenSpec}
          />
        ))}
      </div>
    </section>
  );
}

function PacketSection({
  title,
  specs,
  open,
  onOpenSpec,
}: {
  title: string;
  specs: readonly PacketSpec[];
  open: boolean;
  onOpenSpec?: ((spec: string) => void) | undefined;
}) {
  const [expanded, setExpanded] = useState(open);
  return (
    <div>
      <button
        type="button"
        onClick={() => setExpanded((was) => !was)}
        aria-expanded={expanded}
        className="text-mute hover:text-fg mb-1 flex w-full items-center gap-1.5 text-2xs font-semibold tracking-wider uppercase"
      >
        <ChevronRight className={`size-icon-2xs transition-transform ${expanded ? "rotate-90" : ""}`} />
        {title}
        <span className="tabular-nums normal-case">{specs.length}</span>
      </button>
      {expanded && (
        <ul className="flex flex-col gap-1.5">
          {specs.map((spec) => (
            <li
              key={`${spec.spec}@${spec.revision}`}
              className="border-line rounded-surface border px-2.5 py-2 text-xs"
            >
              <div className="flex items-center gap-2">
                {onOpenSpec ? (
                  <button
                    type="button"
                    className="hover:text-accent min-w-0 truncate text-left font-medium"
                    onClick={() => onOpenSpec(spec.spec)}
                  >
                    {spec.title}
                  </button>
                ) : (
                  <span className="min-w-0 truncate font-medium">{spec.title}</span>
                )}
                <span className="text-mute ml-auto shrink-0 capitalize">{spec.kind}</span>
              </div>
              {/* The source route in plain words, not behind a disclosure: an
                  incorporated Guide sits in the governing set beside the
                  requirements, and this line is the only thing that stops it
                  reading as one. */}
              <div className="text-mute mt-0.5 text-2xs">{sourcePhrase(spec.source)}</div>
              <div
                className="text-mute mt-0.5 truncate font-mono text-2xs"
                title={`${spec.spec}@${spec.revision}`}
              >
                {spec.spec}@{short(spec.revision)}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
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
    <DropdownMenu
      alignment="end"
      hasChevron={false}
      menuWidth={208}
      button={{
        label: "More issue actions",
        variant: "ghost",
        size: "sm",
        isIconOnly: true,
        tooltip: "More issue actions",
        icon: <MoreHorizontal className="size-icon-sm" />,
      }}
    >
      <DropdownMenuItem label="Copy issue link" icon={<Link2 className="size-icon-sm" />} onClick={onCopyLink} />
      <DropdownMenuItem
        label="Copy reference"
        icon={<Copy className="size-icon-sm" />}
        onClick={() => void navigator.clipboard.writeText(issueRef)}
      />
      {!locked && !tombstone && (
        <>
          <DropdownMenuItem label="Duplicate issue" icon={<CopyPlus className="size-icon-sm" />} isDisabled={pending} onClick={onDuplicate} />
          <DropdownMenuItem label="Add relation" icon={<Link2 className="size-icon-sm" />} isDisabled={pending} onClick={onRelate} />
          <DropdownMenuItem label="Add sub-issue" icon={<CornerDownRight className="size-icon-sm" />} isDisabled={pending} onClick={onAddSubIssue} />
          <DropdownMenuItem label="Attach a file" icon={<Paperclip className="size-icon-sm" />} isDisabled={pending} onClick={onAttach} />
          <DropdownMenuItem label="Assign issue" icon={<UserPlus className="size-icon-sm" />} isDisabled={pending} onClick={onAssign} />
          <DropdownMenuItem label="Move to project" icon={<MoveRight className="size-icon-sm" />} isDisabled={pending} onClick={onMove} />
        </>
      )}
      {active && !locked && (
        <DropdownMenuItem label="Stop work" icon={<CircleDot className="size-icon-sm" />} isDisabled={pending} onClick={onStop} />
      )}
      {!locked && <Divider />}
      {!locked &&
        (tombstone ? (
          <DropdownMenuItem label="Restore issue" icon={<ArchiveRestore className="size-icon-sm" />} onClick={onRestore} />
        ) : (
          <DropdownMenuItem
            label={<span className="text-danger">Delete issue</span>}
            icon={<Trash2 className="size-icon-sm" />}
            onClick={onDelete}
          />
        ))}
    </DropdownMenu>
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
      variant={following ? "secondary" : "ghost"}
      size="sm"
      isDisabled={readOnly || meKey == null}
      onClick={() => onToggle(!following)}
      label={following ? "Following" : "Follow"}
      icon={following ? <BellOff className="size-icon-sm" /> : <Bell className="size-icon-sm" />}
      tooltip={
        following
          ? "Stop receiving this issue's activity"
          : "Receive this issue's activity in your inbox"
      }
    >
      {following ? "Following" : "Follow"}
      {others > 0 && <span className="text-mute">+{others}</span>}
    </Button>
  );
}

/**
 * Who else is on this issue, and where their carets are.
 *
 * In the rail rather than in `HeaderActions`. That slot is a portal into a node
 * the shell only mounts when the detail pane is the full-width surface, so a
 * facepile there would render nothing — silently, no warning — on every other
 * layout, and it would be absent from this file's own test for the same reason.
 *
 * Nothing here rings a doorbell or bumps a revision. A face arriving is not a
 * change to the issue, and routing it through the projection invalidation would
 * make every glance somebody takes at a document cost every open tab a re-read.
 */
function LiveRail({
  live,
  members,
  memberOf,
}: {
  live: LiveState;
  /** The ACL array itself, because `stackFor` caches its index on that identity. */
  members: MemberDto[];
  memberOf: (key: string) => MemberDto | undefined;
}) {
  const here = watching(live.entries);
  const marks = carets(live.entries);
  const typing = typists(live.entries);

  // The daemon cannot answer at all — an older build, or one with the Live plane
  // off. Drawing "nobody is here" from that would be inventing an answer, and
  // drawing an apology on every issue would be noise about a thing the reader
  // cannot act on.
  if (live.unavailable) return null;
  if (here.length === 0 && marks.length === 0 && typing.length === 0 && !live.partial) return null;

  const name = (actor: string) => nameOf(actor, memberOf(actor));
  const present = here.filter((row) => !row.uncertain);
  // Shown, and shown as a guess. Hiding them is how a collaborator who has gone
  // quiet for a minute disappears from a room they are still in.
  const unsure = here.filter((row) => row.uncertain);

  return (
    <RailSection title="Live">
      {here.length > 0 && (
        <RailRow label="Here now">
          <span className="flex min-w-0 items-center gap-1.5">
            <AvatarStack members={stackFor(present.map((row) => row.actor), members)} />
            {unsure.length > 0 && (
              <AvatarStack
                members={stackFor(unsure.map((row) => row.actor), members)}
                className="opacity-60"
              />
            )}
            <span className="min-w-0 truncate">
              {present.map((row) => name(row.actor)).join(", ")}
              {unsure.length > 0 && (
                <span className="text-mute">
                  {present.length > 0 ? " · " : ""}
                  {unsure.map((row) => name(row.actor)).join(", ")} may have left
                </span>
              )}
            </span>
          </span>
        </RailRow>
      )}

      {typing.length > 0 && (
        <RailRow label="Typing">
          {/* The row's own label says "Typing", so the value is the names and
              nothing else — a verb here would have to pick a number, and the
              fact is coarse enough already. */}
          <span className="text-mute min-w-0 truncate">{typing.map(name).join(", ")}</span>
        </RailRow>
      )}

      {marks.map((mark) => (
        <RailRow key={`${mark.actor} ${mark.field}`} label="Caret">
          <span className={cn("min-w-0 truncate", mark.uncertain && "text-mute")}>
            {name(mark.actor)} — {mark.field}, {caretPhrase(mark.position)}
            {mark.uncertain && " (last known)"}
          </span>
        </RailRow>
      ))}

      {live.partial && (
        <RailRow label="Awareness">
          <span className="text-mute flex min-w-0 items-center gap-1.5">
            <Info className="size-icon-sm shrink-0" />
            <span className="min-w-0 truncate">This node is not hearing from everyone.</span>
          </span>
        </RailRow>
      )}
    </RailSection>
  );
}

/** Decode a legacy inline attachment.
 *
 *  Read-only and permanent. Records written before the content cutover carry
 *  their bytes base64'd inside the issue Body, and those Bodies are in the
 *  field — a reader that dropped this would lose the files rather than migrate
 *  them. There is deliberately no encoder any more: nothing writes that shape. */
const b64ToBytes = (b64: string): Uint8Array =>
  Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

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
    // No size check here any more. The engine owns `max_content_len` and
    // refuses past it with a sentence; a mirrored constant would be a second
    // number to keep in step, and the one a person met would be whichever was
    // smaller — which is how a ceiling starts lying about itself.
    setBusy(true);
    try {
      // Two steps, in this order, because the engine enforces it: the bytes go
      // to the content plane, and only then does the issue name what came back.
      // `uploadContent` streams the file, so a large attachment is never held
      // in this tab as one buffer.
      const stored = await uploadContent(spaceId, file);
      await rpc(spaceId, {
        cmd: "attach",
        reff,
        name: file.name,
        mime: file.type || null,
        content: stored.content,
        size: stored.size,
      });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const download = async (att: AttachmentMetaDto) => {
    try {
      // A content-plane file is fetched by navigation, not by this tab. Pulling
      // megabytes through JavaScript to hand them straight back to the browser
      // is work the browser does better, and it is the difference between a
      // large file downloading and a large file wedging the page.
      //
      // The URL carries no credential — the cookie rides a same-origin request,
      // and the engine refuses a query token on that route anyway.
      if (att.content) {
        const link = document.createElement("a");
        link.href = downloadUrl(spaceId, att.content, att.name);
        link.download = att.name;
        link.click();
        return;
      }
      // A record from before the cutover. Its bytes are inside the Body, so
      // there is nothing to navigate to and the blob path is the only one.
      const r = await rpc(spaceId, { cmd: "attachment_get", reff, id: att.id });
      if (r.kind !== "attachment") return;
      if (r.content) {
        const link = document.createElement("a");
        link.href = downloadUrl(spaceId, r.content, r.name || att.name);
        link.download = r.name || att.name;
        link.click();
        return;
      }
      if (!r.data_b64) {
        onError("this attachment carries neither bytes nor a content id");
        return;
      }
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
          No size is checked here — the engine refuses past its own ceiling and
          says so, which is one number instead of two. */}
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
                    isDisabled={busy}
                    onClick={() => fileRef.current?.click()}
                    variant="ghost"
                    size="sm"
                    tooltip="Attach a file"
                    icon={<Paperclip className="size-icon-sm" />}
                  />
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
              <IconButton
                label={`Download ${att.name}`}
                onClick={() => void download(att)}
                variant="ghost"
                size="sm"
                tooltip={`Download ${att.name}`}
                icon={<Download className="size-icon-sm" />}
              />
              {!readOnly && (
                <IconButton
                  label={`Remove ${att.name}`}
                  onClick={() =>
                    void rpc(spaceId, { cmd: "detach", reff, id: att.id }).catch((e) =>
                      onError(e instanceof Error ? e.message : String(e)),
                    )
                  }
                  variant="ghost"
                  size="sm"
                  tooltip={`Remove ${att.name}`}
                  icon={<Trash2 className="size-icon-sm" />}
                />
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
  members,
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
  /** The ACL, for resolving a related issue's assignees to faces. */
  members: MemberDto[];
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

  /** A related issue's state as the ring every other surface draws it. Falls
   *  back to the tree glyph while the workflow is still arriving. */
  const statusGlyph = (row: Row) => {
    const state = states.find((s) => s.id === row.status);
    return state ? (
      <StatusIcon category={state.category} color={catalogColor(state.color)} />
    ) : (
      <CornerDownRight className="size-icon-xs" />
    );
  };

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
          // The ring and its tally, the same mark a board card carries — one
          // glyph for one idea, wherever you meet it. It replaces a full-width
          // bar that was the only progress meter in the app and read as a
          // loading state parked under the caption.
          count={
            <span className="flex items-center gap-1.5">
              <ProgressRing done={doneChildren} total={graph.children.length} />
              {doneChildren}/{graph.children.length}
            </span>
          }
          {...(readOnly
            ? {}
            : {
                action: (
                  <IconButton
                    label="Add sub-issue"
                    onClick={() => setSubDraft("")}
                    variant="ghost"
                    size="sm"
                    tooltip="Add sub-issue"
                    icon={<Plus className="size-icon-sm" />}
                  />
                ),
              })}
        >
          {graph.children.map((r) => (
            <RelRow
              key={r.reff}
              row={r}
              // The child's own status leads the row, exactly as it does on a
              // board card and a list line — a sub-issue you cannot tell the
              // state of is a link, not a piece of the work.
              icon={statusGlyph(r)}
              trailing={
                r.assignees.length > 0 ? (
                  <AvatarStack members={stackFor(r.assignees, members)} />
                ) : undefined
              }
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
            <TextInput
              label="Sub-issue title"
              isLabelHidden
              hasAutoFocus
              value={subDraft}
              placeholder="Sub-issue title…  (Enter creates, Esc closes)"
              onChange={setSubDraft}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter" && subDraft.trim()) createSub(subDraft.trim());
                if (e.key === "Escape") setSubDraft(null);
              }}
              onBlur={() => {
                if (!subDraft.trim()) setSubDraft(null);
              }}
              aria-label="New sub-issue title"
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
                    variant="ghost"
                    size="sm"
                    tooltip="Add relation"
                    icon={<Plus className="size-icon-sm" />}
                  />
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
              <IconButton
                label="Cancel"
                onClick={() => setAdding(false)}
                variant="ghost"
                size="sm"
                tooltip="Cancel"
                icon={<X className="size-icon-sm" />}
              />
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
  trailing,
  onNavigate,
  onRemove,
}: {
  row: Row;
  icon: React.ReactNode;
  /** What this edge *is* — omitted for sub-issues, where the group says it. */
  kind?: string;
  tone?: "warn";
  /** The row's own metadata, parked on the trailing edge — faces for a
   *  sub-issue. Same slot the pickers' option rows use, same reason: what the
   *  row *is* reads on the left, what it carries reads on the right. */
  trailing?: React.ReactNode;
  onNavigate: (reff: string) => void;
  onRemove?: () => void;
}) {
  return (
    // The row answers the pointer as a whole. It is one target — the button
    // inside fills it — so lighting the row rather than the label is what makes
    // a run of them read as a list you can walk.
    <div className="group/rel hover:bg-hover -mx-1 flex items-center gap-2 rounded-control px-1 py-0.5 text-sm transition-colors">
      {/* A row, not an action — see the note on the parent row above. */}
      <button
        type="button"
        onClick={() => onNavigate(row.reff)}
        aria-label={row.title}
        className={cn(interactiveRow(), "min-w-0 flex-1 shrink justify-start px-1 text-left")}
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
      </button>
      {trailing && <span className="shrink-0">{trailing}</span>}
      {/* Revealed on row hover/focus: the affordance is there when wanted and
          the list stays quiet the rest of the time. */}
      {onRemove && (
        <IconButton
          label="Remove relation"
          tooltip="Remove relation"
          onClick={onRemove}
          className="opacity-0 group-hover/rel:opacity-100 focus-visible:opacity-100"
          variant="ghost"
          size="sm"
          icon={<X className="size-icon-xs" />}
        />
      )}
    </div>
  );
}

type Entry =
  | { at: number; order: number; kind: "comment"; comment: CommentDto }
  | { at: number; order: number; kind: "event"; event: ActivityEvent; repeat?: number };

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
  description,
  readOnly,
  meKey,
  onReact,
  onReply,
  onCopyLink,
  onCreateFromComment,
}: {
  events: ActivityEvent[];
  comments: CommentDto[];
  /** The description as it stands, so an anchored comment can quote the words
   *  it is attached to. Passed down rather than re-fetched: it is the same text
   *  the engine resolved the anchor against on this read. */
  description: string;
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
    out.sort((a, b) => a.at - b.at || a.order - b.order);

    // Then fold runs that would print the same line twice. Editing five labels
    // is five commits and five honest oplog entries, but it is one act, and the
    // feed rendered it as eight consecutive "changed labels" — a wall that
    // buries the events worth reading between them.
    //
    // The test is the RENDERED PHRASE, not the event kind: two rows merge only
    // if they would have been character-for-character identical (same actor,
    // same sentence). That is what keeps this honest — "moved the due date to
    // Aug 14" and "…to Aug 21" say different things and stay two lines, while
    // eight identical sentences become one with a tally. The newest stamp wins,
    // so the time still reads as when the run finished, and a collision anywhere
    // in the run keeps its flag.
    const folded: Entry[] = [];
    for (const entry of out) {
      const prev = folded[folded.length - 1];
      if (
        entry.kind === "event" &&
        prev?.kind === "event" &&
        describeEventRich(prev.event, phraseCtx).phrase ===
          describeEventRich(entry.event, phraseCtx).phrase &&
        prev.event.actor === entry.event.actor
      ) {
        folded[folded.length - 1] = {
          ...entry,
          repeat: (prev.repeat ?? 1) + 1,
          event: {
            ...entry.event,
            ...(prev.event.collision ? { collision: true } : {}),
          },
        };
        continue;
      }
      folded.push(entry);
    }
    return folded;
    // `phraseCtx` is rebuilt every render but only reads `states`/`graph`, which
    // the deps below already track.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [events, comments, states, graph]);

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
    // The one rule in the document, and it earns it: everything above is the
    // issue, everything below is what happened to it. Linear draws exactly this
    // line and no other — the sections above are divided by their captions and
    // by air, which is why adding a second rule anywhere would immediately make
    // this one stop meaning "the history starts here".
    <section
      id="issue-activity"
      className="border-line/70 flex flex-col gap-3 scroll-mt-3 border-t pt-6"
    >
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
          onClick={() => setVisibleCount((count) => count + 40)}
          className="self-start"
          label={`Show ${Math.min(40, entries.length - visibleCount)} earlier changes`}
          variant="ghost"
          size="sm"
        />
      )}

      {visibleEntries.map((entry) =>
        entry.kind === "comment" ? (
          <Comment
            key={`c${entry.order}`}
            comment={entry.comment}
            replies={entry.comment.id ? (repliesByParent.get(entry.comment.id) ?? []) : []}
            description={description}
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
            {...(entry.repeat ? { repeat: entry.repeat } : {})}
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
  description,
  onReply,
  onCopyLink,
  onCreateFromComment,
}: {
  comment: CommentDto;
  replies: CommentDto[];
  /** The text an anchored comment marks, for quoting the words it is on. */
  description: string;
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
      description={description}
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

/**
 * Where an anchored comment is attached, above the comment itself.
 *
 * Three states and three renderings, because they are three different facts.
 *
 * A resolved span quotes the words it is on — which is the whole value of
 * anchoring, and the only rendering that needs the text. `drifted` says the
 * words are gone and shows no position: a stale offset drawn as a number is a
 * highlight over the wrong words, which is worse than no highlight. The comment
 * stays either way, because somebody wrote it and the text moving out from under
 * it does not unwrite it.
 *
 * `unresolved` is not `drifted`. It says nobody worked out where this is, which
 * is a fact about this node rather than about the text, and a reader who is told
 * "the text is gone" when it is not would go looking for a deletion nobody made.
 */
function AnchorNote({
  anchor,
  description,
}: {
  anchor: CommentDto["anchor"];
  description: string;
}) {
  if (!anchor) return null;
  const state = anchor.state;
  if (state.kind === "at") {
    // Converted before slicing. The engine counts Unicode scalars and a JS
    // string indexes UTF-16, so slicing the raw offsets is correct until
    // somebody puts an emoji in the description and silently wrong after.
    const span = codeUnitSpan(description, state.start, state.end);
    const marked = description.slice(span.start, span.end);
    return (
      <p className="text-mute mt-1 truncate text-xs">
        on <span className="text-default">{marked || "\u2014"}</span>
      </p>
    );
  }
  return (
    <p className="text-mute mt-1 text-xs">
      {state.kind === "drifted"
        ? "the text this marked has changed"
        : "this node cannot place this comment"}
    </p>
  );
}

/** A single comment inside the card: header (face, name, time, actions), body,
 *  reaction chips. Shared by the root comment and each reply. */
function CommentBlock({
  comment: c,
  description,
  memberOf,
  readOnly,
  meKey,
  onReact,
  onCopyLink,
  onCreateFromComment,
}: {
  comment: CommentDto;
  description: string;
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
    <div className="group/comment px-4 py-3">
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
            <Popover
              isOpen={picking}
              onOpenChange={setPicking}
              alignment="end"
              content={
                <div className="flex gap-0.5 p-1">
                  {REACTION_EMOJIS.map((emoji) => (
                    <Button
                      key={emoji}
                      onClick={() => {
                        setPicking(false);
                        if (c.id) onReact(c.id, emoji, true);
                      }}
                      aria-label={`React ${emoji}`}
                      className="text-base"
                      label={emoji}
                      variant="ghost"
                      size="sm"
                    />
                  ))}
                </div>
              }
            >
              <IconButton
                label="Add reaction"
                variant="ghost"
                size="sm"
                tooltip="Add reaction"
                icon={<SmilePlus className="size-icon-sm" />}
              />
            </Popover>
            <DropdownMenu
              alignment="end"
              hasChevron={false}
              menuWidth={176}
              button={{
                label: "Comment actions",
                variant: "ghost",
                size: "sm",
                isIconOnly: true,
                tooltip: "Comment actions",
                icon: <MoreHorizontal className="size-icon-sm" />,
              }}
            >
              <DropdownMenuItem
                label="Copy link"
                icon={<Link2 className="size-icon-sm" />}
                onClick={() => onCopyLink(c.id!)}
              />
              <DropdownMenuItem
                label="New issue from comment"
                icon={<CopyPlus className="size-icon-sm" />}
                onClick={() => onCreateFromComment(c.body)}
              />
            </DropdownMenu>
          </span>
        )}
      </div>
      <AnchorNote anchor={c.anchor} description={description} />
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
    <div className="border-line flex items-center gap-2 border-t py-2.5 pr-2.5 pl-4">
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
        variant="ghost"
        size="sm"
        tooltip="Attach a file"
        icon={<Paperclip className="size-icon-sm" />}
      />
      <IconButton
        label="Reply"
        isDisabled={!draft.trim()}
        onClick={submit}
        className="self-end"
        variant="ghost"
        size="sm"
        tooltip="Reply  Ctrl/⌘ ↵"
        icon={<ArrowUp className="size-icon-sm" />}
      />
    </div>
  );
}

function Event({
  event: e,
  states,
  memberOf,
  ctx,
  repeat,
}: {
  event: ActivityEvent;
  states: WorkflowState[];
  memberOf: (key: string) => MemberDto | undefined;
  ctx: EventPhraseContext;
  /** How many identical lines this row stands for — see the fold in `Timeline`. */
  repeat?: number;
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
        {/* The tally, never a re-wording: the sentence is what happened and the
            count is how many times, so a folded run reads as its own line plus
            a number rather than a phrase you have to parse differently. */}
        {repeat && repeat > 1 && (
          <span className="text-dim ml-1 tabular-nums" title={`${repeat} identical changes`}>
            ×{repeat}
          </span>
        )}
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
 * Description: a recoverable optimistic draft streamed as ordered CRDT splices.
 * The quiet window groups activity history only; it never gates replication.
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
  onSplice,
  onCheckpoint,
  onReadLatest,
  onError,
  remoteCursors,
  remoteContexts,
  remotePreviews,
  onAwareness,
}: {
  draftKey: { spaceId: string; reff: string };
  value: string;
  readOnly: boolean;
  onSplice: (splice: TextSplice) => Promise<unknown>;
  onCheckpoint: () => Promise<unknown>;
  onReadLatest: () => Promise<string>;
  onError: (message: string) => void;
  remoteCursors: RemoteCursor[];
  remoteContexts: RemoteContext[];
  remotePreviews: RemoteTextPreview[];
  onAwareness: (
    anchor: number | null,
    focus: number | null,
    typing: boolean,
    ready: boolean,
    preview: BrowserTextPreview | null,
  ) => void;
}) {
  const [draft, setDraft] = useState(
    () => loadDraft(draftKey.spaceId, draftKey.reff, "description") || value,
  );
  const [authoritative, setAuthoritative] = useState(value);
  const [pending, setPending] = useState(0);
  const dirty = useRef(draft !== value);
  const pendingRef = useRef(0);
  const settledText = useRef(value);
  const settledRevision = useRef(textRevision(value));
  const previewMarkdown = useRef<string | null>(null);
  const latestAwareness = useRef({
    anchor: null as number | null,
    focus: null as number | null,
    typing: false,
    markdown: value,
  });
  const writeQueue = useRef<Promise<unknown>>(Promise.resolve());
  const checkpointTimer = useRef<number | null>(null);
  const previewTimer = useRef<number | null>(null);
  const preview = useRef<BrowserTextPreview | null>(null);
  const uncheckpointed = useRef(false);

  const report = useRef(onError);
  report.current = onError;

  const checkpoint = () => {
    if (!uncheckpointed.current) return;
    uncheckpointed.current = false;
    const task = writeQueue.current.then(onCheckpoint);
    writeQueue.current = task.catch(() => undefined);
    void task.catch((error: unknown) => {
      report.current(error instanceof Error ? error.message : String(error));
    });
  };

  useEffect(
    () => () => {
      if (checkpointTimer.current !== null) window.clearTimeout(checkpointTimer.current);
      if (previewTimer.current !== null) window.clearTimeout(previewTimer.current);
    },
    [],
  );

  useEffect(() => {
    if (pendingRef.current === 0) {
      settledText.current = value;
      settledRevision.current = textRevision(value);
      setAuthoritative(value);
    }
  }, [value]);

  useEffect(() => {
    if (dirty.current && draft !== value) {
      saveDraft(draftKey.spaceId, draftKey.reff, "description", draft);
    }
  }, [draftKey.spaceId, draftKey.reff, draft, value]);

  if (readOnly) {
    // A draft typed before the gate flipped (standing revoked mid-edit, or
    // writes that never landed) must not silently vanish behind the
    // read-only face — it survives in local storage, and saying so is the
    // difference between "kept" and "lost".
    const heldDraft = loadDraft(draftKey.spaceId, draftKey.reff, "description");
    return (
      <div className="min-h-ctl-xl py-2">
        {heldDraft && heldDraft !== value && (
          <p className="text-warn mb-2 text-xs">
            You have unsaved local edits to this description from before write
            access was gated — they’re kept on this device and can be copied
            out, but won’t be written until you hold write access again.
          </p>
        )}
        {value ? <Markdown text={value} /> : <span className="text-mute">No description</span>}
      </div>
    );
  }

  return (
    <MarkdownEditor
      value={authoritative}
      placeholder="Add description…"
      className="min-h-ctl-xl py-2"
      remoteCursors={remoteCursors}
      remoteContexts={remoteContexts}
      remotePreviews={remotePreviews}
      acceptRemote={pending === 0}
      onAwareness={(anchor, focus, typing, markdown) => {
        const next = { anchor, focus, typing, markdown };
        latestAwareness.current = next;
        const retiring = anchor === null && !typing;
        if (retiring) {
          if (previewTimer.current !== null) window.clearTimeout(previewTimer.current);
          previewTimer.current = null;
          preview.current = null;
          previewMarkdown.current = null;
        } else if (preview.current && previewMarkdown.current === markdown && anchor !== null) {
          const { anchor: _anchor, focus: _focus, ...rest } = preview.current;
          preview.current = {
            ...rest,
            anchor,
            ...(focus === null ? {} : { focus }),
          };
        }
        onAwareness(
          anchor,
          focus,
          typing,
          awarenessReadyFor(markdown, settledText.current, pendingRef.current, retiring),
          preview.current,
        );
      }}
      onChange={(markdown, splice, change) => {
        dirty.current = true;
        setDraft(markdown);
        saveDraft(draftKey.spaceId, draftKey.reff, "description", markdown);

        const prior = preview.current;
        const cumulative = continueTextPreview(
          prior,
          change.previousRevision,
          settledRevision.current,
          settledText.current,
          markdown,
          splice,
        );
        const held = latestAwareness.current;
        if (cumulative && new TextEncoder().encode(cumulative.insert).byteLength <= 2 * 1024) {
          const inserted = Array.from(cumulative.insert).length;
          const anchor = held.markdown === markdown && held.anchor !== null
            ? held.anchor
            : cumulative.index + inserted;
          const focus = held.markdown === markdown ? held.focus : anchor;
          preview.current = {
            field: "description",
            result: change.resultRevision,
            ...cumulative,
            anchor,
            ...(focus === null ? {} : { focus }),
          };
          previewMarkdown.current = markdown;
        } else {
          preview.current = null;
          previewMarkdown.current = null;
        }

        if (previewTimer.current !== null) window.clearTimeout(previewTimer.current);
        previewTimer.current = window.setTimeout(() => {
          previewTimer.current = null;
          preview.current = null;
          previewMarkdown.current = null;
          const awareness = latestAwareness.current;
          onAwareness(
            awareness.anchor,
            awareness.focus,
            awareness.typing,
            awarenessReadyFor(
              awareness.markdown,
              settledText.current,
              pendingRef.current,
              awareness.anchor === null && !awareness.typing,
            ),
            null,
          );
        }, 1_500);

        pendingRef.current += 1;
        setPending(pendingRef.current);
        if (held.anchor !== null || held.typing) {
          onAwareness(held.anchor, held.focus, held.typing, false, preview.current);
        } else {
          onAwareness(null, null, false, false, preview.current);
        }
        const task = writeQueue.current.then(() => onSplice(splice));
        writeQueue.current = task.catch(() => undefined);
        void task.then(async () => {
          // The final acknowledgement reads the merged CRDT value directly.
          // A new local keystroke that arrives during this read keeps the
          // optimistic buffer held until its own acknowledgement completes.
          const latest = pendingRef.current === 1 ? await onReadLatest() : null;
          pendingRef.current -= 1;
          setPending(pendingRef.current);
          if (pendingRef.current === 0 && latest !== null) {
            settledText.current = latest;
            settledRevision.current = textRevision(latest);
            setAuthoritative(latest);
            dirty.current = false;
            clearDraft(draftKey.spaceId, draftKey.reff, "description");
            const awareness = latestAwareness.current;
            // Once the exact optimistic result is durable, the preview becomes
            // a revision-stamped caret. Keep it until blur so the UI never
            // hands a line-boundary position back to an older CRDT anchor.
            if (awareness.markdown === latest && preview.current !== null) {
              if (previewTimer.current !== null) window.clearTimeout(previewTimer.current);
              previewTimer.current = null;
            }
            onAwareness(
              awareness.anchor,
              awareness.focus,
              awareness.typing,
              awarenessReadyFor(
                awareness.markdown,
                latest,
                0,
                awareness.anchor === null && !awareness.typing,
              ),
              preview.current,
            );
          }
        }).catch((error: unknown) => {
          pendingRef.current = Math.max(0, pendingRef.current - 1);
          setPending(pendingRef.current);
          report.current(error instanceof Error ? error.message : String(error));
        });

        // Human-facing activity remains grouped without delaying the splice.
        uncheckpointed.current = true;
        if (checkpointTimer.current !== null) window.clearTimeout(checkpointTimer.current);
        checkpointTimer.current = window.setTimeout(() => {
          checkpointTimer.current = null;
          checkpoint();
        }, 350);
      }}
      onCommit={() => {
        if (checkpointTimer.current !== null) window.clearTimeout(checkpointTimer.current);
        checkpointTimer.current = null;
        checkpoint();
      }}
    />
  );
}
