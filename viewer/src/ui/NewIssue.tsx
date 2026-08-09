import { cn } from "@lait/ui";
import { Dialog, Divider, DropdownMenu, DropdownMenuItem, Switch } from "@astryxdesign/core";
import { useEffect, useRef, useState } from "react";
import { LayoutTemplate, Maximize2, Minimize2, Trash2, X } from "lucide-react";

import { rpc } from "../api";
import { clearDraft, loadDraft, loadFields, saveDraft, saveFields } from "../core/drafts";
import { loadTemplates, removeTemplate, saveTemplate, type IssueTemplate } from "../core/templates";
import * as ask from "./dialogs";

import {
  PRIORITY_LABEL,
  PRIORITY_ORDER,
  type LabelDto,
  type MemberDto,
  type Priority,
  type ProjectDto,
  type WorkflowState,
} from "../types";
import { Avatar, AvatarStack } from "./Avatar";
import { catalogColor } from "./colors";
import { PriorityIcon, StatusIcon } from "./icons";
import { Combobox } from "./Picker";
import { DatePicker } from "./DatePicker";
import { NewLabelDialog } from "./NewLabel";
import { Button, IconButton } from "@astryxdesign/core";

import { short } from "./time";

/**
 * The composer.
 *
 * A tracker's most-used surface, so it is a real dialog rather than the labelled
 * text box it replaced: title and description read as *the document*, borderless
 * and unlabelled — the placeholder is the label — and the fields you might set sit
 * underneath as pills you can ignore. Filing an issue should cost a title and
 * Enter; everything else is optional and stays out of the way until wanted.
 *
 * One wrinkle worth naming: `issue_new` takes title/body/priority/labels/assignees
 * but **not status** — a new issue lands in `DEFAULT_STATUS` by construction. So
 * when you open the composer from a column's `+`, honouring that column costs a
 * second request (`issue_edit`), and therefore a second commit and a second
 * activity row (S§7.1). That is an honest record of what happened — filed, then
 * moved — and it only happens when you asked for a non-default column.
 */
export function NewIssue({
  spaceId,
  canonicalSpaceId,
  projectKey,
  projects,
  states,
  labels,
  members,
  defaultStatus,
  presentation = "dialog",
  onClose,
  onExpand,
  onCollapse,
  onError,
  onCreated,
}: {
  spaceId: string;
  canonicalSpaceId: string;
  projectKey: string;
  projects: ProjectDto[];
  states: WorkflowState[];
  labels: LabelDto[];
  members: MemberDto[];
  /** The column you opened this from, if any. */
  defaultStatus?: string | undefined;
  /**
   * A sheet over the board, or the work area itself.
   *
   * One component either way. The composer's parts — the crumb, the document,
   * the pill row, the commit — are the same parts at both sizes, and the moment
   * there are two of them they start disagreeing about which fields exist. What
   * changes is the frame around them and how much room the description gets.
   */
  presentation?: "dialog" | "page";
  onClose: () => void;
  /** Take the draft to its own route. Absent when there is nowhere to expand
   *  to — the composer has to know a project key to have an address. */
  onExpand?: ((project: string) => void) | undefined;
  /** Back to the sheet. Present only in `page`. */
  onCollapse?: (() => void) | undefined;
  onError: (m: string) => void;
  onCreated: (message: string) => void;
}) {
  const draftSubject = `new:${projectKey}`;
  // Read once, at mount. The composer is remounted by the expand/collapse hop,
  // and this is what carries the whole draft — prose and pills — across it.
  const [saved] = useState(() => loadFields(canonicalSpaceId, draftSubject));
  const [title, setTitle] = useState(() => loadDraft(canonicalSpaceId, draftSubject, "new-title"));
  const [body, setBody] = useState(() => loadDraft(canonicalSpaceId, draftSubject, "new-body"));
  const [priority, setPriority] = useState<Priority>((saved.priority as Priority) ?? "none");
  const [project, setProject] = useState(saved.project ?? projectKey);
  const [due, setDue] = useState(saved.due ?? "");
  // The column you opened from outranks the draft: asking for Backlog's `+` and
  // landing in In Review because that is what a stale draft said is the control
  // ignoring the thing you just did.
  const [status, setStatus] = useState(
    defaultStatus ?? saved.status ?? states[0]?.id ?? "backlog",
  );
  /** Label **names** — `issue_new` resolves names, not ids, and creates on first use. */
  const [picked, setPicked] = useState<string[]>(saved.labels ?? []);
  /** Assignee **keys** — `index::resolve_device` takes `me` or a full 64-hex key. */
  const [assignees, setAssignees] = useState<string[]>(saved.assignees ?? []);
  const [busy, setBusy] = useState(false);
  const [again, setAgain] = useState(false);
  const [newLabel, setNewLabel] = useState<string | null>(null);
  const [templates, setTemplates] = useState(() => loadTemplates(canonicalSpaceId));
  const [templateMenu, setTemplateMenu] = useState(false);
  const [failure, setFailure] = useState("");
  const [recovered] = useState(() =>
    Boolean(
      loadDraft(canonicalSpaceId, draftSubject, "new-title") ||
      loadDraft(canonicalSpaceId, draftSubject, "new-body"),
    ),
  );
  /** Whether there is anything to discard — prose or pills. Drives the one
   *  control in the header that is not always there. */
  const dirty = Boolean(title || body || picked.length || assignees.length || due ||
    priority !== "none" || project !== projectKey);

  const state = states.find((s) => s.id === status) ?? null;
  const landsIn = states[0]?.id ?? "backlog";

  /**
   * The title takes focus, and `autoFocus` is not enough to give it.
   *
   * Astryx's `Dialog` moves focus into the sheet itself once it opens — its
   * header's title normally receives it — and that move runs *after* React has
   * honoured `autoFocus`. The composer has no `DialogHeader` (the title is the
   * document, in the body), so focus landed on the first focusable thing in the
   * sheet instead: the Templates button. Press `c`, start typing, and nothing
   * happened; press Enter and you opened the template menu.
   *
   * The composer's whole promise is that filing an issue costs a title and
   * Enter, so this is not a detail. A frame after mount is what it takes to
   * land after Astryx's own move — `hasAutoFocus` on Astryx's `TextInput`
   * works, but the title is deliberately not one of those.
   */
  const titleRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const frame = requestAnimationFrame(() => titleRef.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, []);

  /**
   * Escape leaves, at both sizes.
   *
   * The dialog gets this from Astryx. The page does not — it is ordinary
   * content in the work area, which is the point — so it says so itself. The
   * listener is on the document because the composer's own inputs
   * `stopPropagation` on keydown (to keep the app's global keymap off a
   * half-typed title), and a handler on the section would never see the key
   * that mattered.
   *
   * Capture phase, for the same reason: by the bubble phase the input has
   * already stopped it.
   */
  useEffect(() => {
    if (presentation !== "page") return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      event.preventDefault();
      onClose();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [presentation, onClose]);

  const applyTemplate = (t: IssueTemplate) => {
    if (t.title) setTitle(t.title);
    if (t.body) setBody(t.body);
    setPriority(t.priority);
    if (t.status) setStatus(t.status);
    setPicked(t.labels);
    setAssignees(t.assignees);
    setTemplateMenu(false);
  };
  const saveAsTemplate = async () => {
    setTemplateMenu(false);
    const name = await ask.prompt({
      title: "Save as template",
      body: "Stored on this device, for this space. Applies the current fields to a new issue.",
      label: "Template name",
      defaultValue: title.trim(),
    });
    if (!name?.trim()) return;
    const id = `${Date.now().toString(36)}-${name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`;
    setTemplates(
      saveTemplate(canonicalSpaceId, {
        id,
        name: name.trim(),
        title: title.trim(),
        body: body.trim(),
        priority,
        status,
        labels: picked,
        assignees,
      }),
    );
    onCreated(`Saved template “${name.trim()}”`);
  };

  useEffect(
    () => saveDraft(canonicalSpaceId, draftSubject, "new-title", title),
    [canonicalSpaceId, draftSubject, title],
  );
  useEffect(
    () => saveDraft(canonicalSpaceId, draftSubject, "new-body", body),
    [canonicalSpaceId, draftSubject, body],
  );
  // The pills persist on the same terms as the prose. `project` and `status`
  // record only a *departure* from the default: storing the default would make
  // a draft that pins the composer to a column you happened to be looking at
  // once, and re-opening from a different `+` would be overruled by history.
  useEffect(
    () =>
      saveFields(canonicalSpaceId, draftSubject, {
        ...(project !== projectKey ? { project } : {}),
        ...(status !== (states[0]?.id ?? "backlog") ? { status } : {}),
        ...(priority !== "none" ? { priority } : {}),
        ...(due ? { due } : {}),
        labels: picked,
        assignees,
      }),
    [canonicalSpaceId, draftSubject, project, projectKey, status, states, priority, due, picked, assignees],
  );

  /** Forget the prose. The pills are the "scaffolding" that survives a
   *  Create-more, so they are cleared separately, by {@link forgetDraft}. */
  const forgetProse = () => {
    clearDraft(canonicalSpaceId, draftSubject, "new-title");
    clearDraft(canonicalSpaceId, draftSubject, "new-body");
  };
  /** Forget the whole draft — what "there is no draft here any more" means. */
  const forgetDraft = () => {
    forgetProse();
    saveFields(canonicalSpaceId, draftSubject, {});
  };

  const create = async () => {
    const t = title.trim();
    if (!t || busy) return;
    setBusy(true);
    setFailure("");
    let created: string | null = null;
    try {
      const r = await rpc(spaceId, {
        cmd: "issue_new",
        title: t,
        ...(body.trim() ? { body: body.trim() } : {}),
        ...(priority !== "none" ? { priority } : {}),
        ...(picked.length ? { labels: picked } : {}),
        ...(assignees.length ? { assignees } : {}),
        // ALWAYS named, never inferred.
        //
        // This used to send `project` only when it differed from the one you
        // opened the composer in — on the theory that the daemon would fill in
        // the obvious answer. It does not have one. A null project is resolved
        // through the CLI's chain (git branch, then `project.default`, then
        // "is there exactly one?"), and a browser satisfies none of those
        // links, so on any space with a second project the chain ran out and
        // refused: "no project chosen and no single default — pass -p
        // <project>".
        //
        // Which made the composer work only when you picked a project you were
        // NOT in — the one case that took the branch — and fail on the default
        // every time. `board` learned this same lesson (see `useProjectBoard`);
        // this is the write side of it.
        project,
        ...(due ? { due } : {}),
      });
      if (r.kind === "ref") created = r.reff;
      // `issue_new` can't set status, so honour a non-default column with a
      // follow-up rather than pretending the field exists.
      if (r.kind === "ref" && status !== landsIn) {
        await rpc(spaceId, { cmd: "issue_edit", reff: r.reff, status });
      }
      if (again) {
        // "Create more": keep the scaffolding, clear the prose. Filing five
        // related issues shouldn't mean re-picking the same labels five times.
        forgetProse();
        setTitle("");
        setBody("");
        onCreated(`Created ${created ?? "issue"} · ready for another`);
      } else {
        forgetDraft();
        onCreated(`Created ${created ?? "issue"}`);
        onClose();
      }
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (created) {
        forgetDraft();
        onError(`Created ${created}, but an optional field was not applied: ${message}`);
        onClose();
      } else {
        setFailure(message);
      }
    } finally {
      setBusy(false);
    }
  };

  const page = presentation === "page";

  const composer = (
    <>
          <header className="flex items-center gap-2 px-4 pt-3">
            <span className="border-line text-dim rounded-mark border px-1.5 py-px font-mono text-2xs">
              {projectKey}
            </span>
            <span className="text-mute">›</span>
            <h2 id="new-issue-heading" className="text-dim text-sm">New issue</h2>
            {/* Verb first, then the icons, as one cluster on the trailing edge.
                A worded button between two glyphs reads as an interruption in a
                row of tools; ahead of them it reads as what it is — the one
                thing here you might *do* to the draft rather than to the frame
                around it. The cluster owns the `ml-auto` so its left edge does
                not move when Discard comes and goes. */}
            <span className="ml-auto flex shrink-0 items-center gap-2">
            {dirty && !busy && (
              <Button
                onClick={() => {
                  forgetDraft();
                  setTitle("");
                  setBody("");
                  onClose();
                }}
                label="Discard draft"
                tooltip="Drafts are saved on this device only"
                variant="ghost"
                size="sm"
              />
            )}
            <DropdownMenu
              isMenuOpen={templateMenu}
              onOpenChange={setTemplateMenu}
              alignment="end"
              hasChevron={false}
              menuWidth={224}
              button={{
                label: "Templates",
                variant: "ghost",
                size: "sm",
                isIconOnly: true,
                tooltip: "Templates",
                icon: <LayoutTemplate className="size-icon-md" />,
              }}
            >
              <div className="text-mute px-2 py-1 text-2xs font-semibold uppercase">Templates</div>
              {templates.length === 0 && (
                <p className="text-mute px-2 py-1 text-xs">None yet — fill the fields, then save.</p>
              )}
              {/* Not a DropdownMenuItem: a template row carries a second verb
                  (delete) on its trailing edge, and an item is one action. */}
              {templates.map((t) => (
                <div key={t.id} className="hover:bg-hover flex items-center rounded-control">
                  <button
                    onClick={() => applyTemplate(t)}
                    className="min-w-0 flex-1 truncate px-2 py-1.5 text-left text-sm"
                  >
                    {t.name}
                  </button>
                  <IconButton
                    label={`Delete template ${t.name}`}
                    className="mr-0.5"
                    onClick={() => setTemplates(removeTemplate(canonicalSpaceId, t.id))}
                    variant="ghost"
                    size="sm"
                    tooltip={`Delete template ${t.name}`}
                    icon={<Trash2 className="size-icon-sm" />}
                  />
                </div>
              ))}
              <Divider />
              <DropdownMenuItem
                label="Save current as template…"
                icon={<LayoutTemplate className="size-icon-sm" />}
                isDisabled={!title.trim()}
                onClick={() => void saveAsTemplate()}
              />
            </DropdownMenu>
            {onExpand && !page && (
              <IconButton
                label="Expand to full page"
                onClick={() => onExpand(project)}
                variant="ghost"
                size="sm"
                tooltip="Expand to full page"
                icon={<Maximize2 className="size-icon-md" />}
              />
            )}
            {onCollapse && page && (
              <IconButton
                label="Collapse to a dialog"
                onClick={onCollapse}
                variant="ghost"
                size="sm"
                tooltip="Collapse to a dialog"
                icon={<Minimize2 className="size-icon-md" />}
              />
            )}
            <IconButton
              label="Close"
              onClick={onClose}
              variant="ghost"
              size="sm"
              tooltip="Close  Esc"
              icon={<X className="size-icon-md" />}
            />
            </span>
          </header>

          <div className="flex min-h-0 flex-col gap-1 px-4 pt-2">
            {/* Borderless: this reads as the document, not a form. */}
            <input
              ref={titleRef}
              value={title}
              placeholder="Issue title"
              onChange={(e) => setTitle(e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") {
                  e.preventDefault();
                  void create();
                }
              }}
              aria-label="Issue title"
              className="placeholder:text-mute bg-transparent text-lg font-semibold outline-none"
            />
            {/* Two rows in the sheet, the rest of the page when expanded. It
                reserved three and left a band of empty sheet above the pills
                until you typed into it — the composer claiming room for prose
                nobody had written yet. */}
            <textarea
              value={body}
              rows={2}
              placeholder="Add description…"
              onChange={(e) => setBody(e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                // Enter is a newline here; the chord submits.
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  void create();
                }
              }}
              aria-label="Description"
              // A floor, not a fill. Stretching it to the page's height was the
              // first thing I tried and it was clearly wrong: the pill row and
              // the commit button ended up pinned 600px below the title, so the
              // fields you set on the way to filing an issue were a journey
              // away from the words you set them about. The composer keeps its
              // natural flow at both sizes; the page just gives the prose more
              // floor to start from.
              className={cn(
                "placeholder:text-mute resize-none bg-transparent outline-none",
                page && "min-h-40",
              )}
            />
          </div>

          <div className="flex flex-wrap items-center gap-2 px-4 py-3">
            <Combobox
              label="Project"
              // Square, like every other surface that identifies a project —
              // sidebar, project card, breadcrumb crumb. No `swatchSlot`: a chip
              // row has no column for the label to line up against.
              swatchShape="square"
              value={(() => {
                const chosen = projects.find((candidate) => candidate.key === project);
                // The face carries the swatch the menu just showed you. Without
                // it, picking a project makes its colour disappear.
                return {
                  id: project,
                  label: chosen?.name ?? project,
                  ...(chosen ? { swatch: catalogColor(chosen.color) } : {}),
                };
              })()}
              options={projects.map((candidate) => ({
                id: candidate.key,
                label: candidate.name,
                hint: candidate.key,
                swatch: catalogColor(candidate.color),
              }))}
              onPick={setProject}
            />
            <Combobox
              label="Status"
              value={
                state
                  ? {
                      id: state.id,
                      label: state.name,
                      icon: <StatusIcon category={state.category} color={catalogColor(state.color)} />,
                    }
                  : null
              }
              options={states.map((s) => ({
                id: s.id,
                label: s.name,
                icon: <StatusIcon category={s.category} color={catalogColor(s.color)} />,
              }))}
              onPick={setStatus}
            />
            <Combobox
              label="Priority"
              value={{
                id: priority,
                label: priority,
                icon: <PriorityIcon priority={priority} />,
              }}
              // `none` is a real engine value, not an absence — so the Combobox
              // sees a set value and renders it at full strength. In this row it
              // is still an unset field and has to read like Assignees and
              // Labels beside it, so the face states the muted verb itself.
              // `capitalize` rides with the real values only: it would render
              // the verb as "Priority" → "Priority" but "urgent" → "Urgent",
              // and putting it on the trigger capitalises both.
              face={
                <>
                  <PriorityIcon priority={priority} />
                  <span className={priority === "none" ? "text-mute" : "min-w-0 truncate capitalize"}>
                    {priority === "none" ? "Priority" : priority}
                  </span>
                </>
              }
              options={[...PRIORITY_ORDER].reverse().map((p) => ({
                id: p,
                label: PRIORITY_LABEL[p],
                icon: <PriorityIcon priority={p} tone="neutral" />,
              }))}
              onPick={(id) => setPriority(id as Priority)}
            />
            <Combobox
              multi
              label="Assignees"
              selected={assignees}
              emptyText="No members yet"
              face={
                assignees.length === 0 ? (
                  <span className="text-mute">Assignees</span>
                ) : (
                  <span className="flex items-center gap-1.5">
                    <AvatarStack
                      members={assignees.map((k) => {
                        const m = members.find((x) => x.key === k);
                        return { key: k, alias: m?.alias ?? "", me: m?.me ?? false };
                      })}
                    />
                    <span>{assignees.length === 1 ? nameFor(assignees[0]!, members) : assignees.length}</span>
                  </span>
                )
              }
              options={members.map((m) => ({
                id: m.key,
                label: nameFor(m.key, members),
                icon: <Avatar deviceKey={m.key} alias={m.alias} me={m.me} size="sm" />,
                hint: m.key.slice(0, 6),
                keywords: [m.key, m.alias],
              }))}
              onToggle={(key) =>
                setAssignees((a) => (a.includes(key) ? a.filter((x) => x !== key) : [...a, key]))
              }
            />
            <Combobox
              multi
              label="Labels"
              selected={picked}
              emptyText="No labels yet"
              face={
                picked.length === 0 ? (
                  <span className="text-mute">Labels</span>
                ) : (
                  <span>{picked.join(", ")}</span>
                )
              }
              // `id` is the **name**: `issue_new` resolves label names and creates
              // unknown ones on first use, so the name is the identity here.
              options={labels.map((l) => ({
                id: l.name,
                label: l.name,
                swatch: catalogColor(l.color),
                keywords: [l.id],
              }))}
              onToggle={(name) =>
                setPicked((p) => (p.includes(name) ? p.filter((x) => x !== name) : [...p, name]))
              }
              // A typed-but-unknown name gets a colour first: the colour step
              // registers it via `label_new`, then it joins the picked set and
              // `issue_new` attaches the now-coloured label by name.
              onCreate={(name) => setNewLabel(name)}
            />
            <DatePicker
              tone="outline"
              value={due || null}
              placeholder="Due date"
              onChange={(next) => setDue(next ?? "")}
            />
          </div>

          {/* One row, and it never wraps.
              It used to be three things and a full-width caption: the caption
              claimed its own line, which pushed the switch, Discard draft, a
              keyboard badge and the commit onto a second — 85px of footer under
              a 52px pill row, carrying no content. Discard moved to the header,
              the caption became a tooltip there, and the ⏎ badge is gone: it
              was the app's only `<kbd>` outside a menu row, an 18px square-
              cornered box beside a 32px pill, and the button already answers to
              Enter.

              The left slot is a status region that is usually empty. When it is
              not, what it has to say — a failure, a recovered draft — is worth
              the width it takes, and it shrinks rather than reflowing the row. */}
          <footer className="border-line flex items-center gap-3 border-t px-4 py-3">
            {(failure || recovered) && (
              <span
                className={cn("min-w-0 truncate text-xs", failure ? "text-danger" : "text-mute")}
                role={failure ? "alert" : "status"}
              >
                {failure
                  ? `Not created. Draft remains on this device: ${failure}`
                  : "Recovered local draft"}
              </span>
            )}
            <span className="ml-auto flex shrink-0 items-center gap-3">
              <Switch label="Create more" value={again} onChange={setAgain} size="sm" />
              <Button
                isDisabled={!title.trim()}
                isLoading={busy}
                onClick={() => void create()}
                label={busy ? "Creating…" : "Create issue"}
                tooltip="Create issue  ⏎"
                variant="primary"
                size="md"
              />
            </span>
          </footer>
    </>
  );

  return (
    <>
    {page ? (
      /* The work area itself. Not a fullscreen Dialog: a modal that fills the
         viewport still traps focus and hides the board behind a backdrop, and
         the whole point of expanding is that the draft has become a place you
         are rather than something in front of what you were doing. The sidebar
         and the space header stay live, Back works, and the address is real.

         `max-w-prose-wide` on the regions, not the frame: a title input that
         runs the full width of a 1400px window is a worse editor than the
         640px sheet it expanded out of. The frame is the page; the measure is
         still a document's. */
      <section
        aria-labelledby="new-issue-heading"
        className="[&>*]:mx-auto [&>*]:w-full [&>*]:max-w-3xl flex min-h-0 flex-1 flex-col overflow-y-auto pt-2"
      >
        {composer}
      </section>
    ) : (
      /* The title lives in the body as the composer's own input, so the header
         names the dialog rather than rendering a second title. `purpose="form"`
         keeps a stray backdrop click from discarding a half-written issue.

         Named by its own heading. `DialogHeader` would do this for us, but the
         composer cannot use one — its title is the document, in the body — and
         without the wiring the sheet announces as an unlabelled dialog. */
      <Dialog
        isOpen
        onOpenChange={(o) => !o && onClose()}
        width={640}
        purpose="form"
        aria-labelledby="new-issue-heading"
      >
        {composer}
      </Dialog>
    )}
    {newLabel !== null && (
      <NewLabelDialog
        name={newLabel}
        onCancel={() => setNewLabel(null)}
        onCreate={(labelName, color) => {
          setNewLabel(null);
          // Register the label with its colour, then add it to the picked set —
          // `issue_new` attaches by name, so the label already carries its colour
          // by the time the issue is created.
          void rpc(spaceId, { cmd: "label_new", name: labelName, color })
            .then(() => setPicked((p) => (p.includes(labelName) ? p : [...p, labelName])))
            .catch((e) => onError(e instanceof Error ? e.message : String(e)));
        }}
      />
    )}
    </>
  );
}

/** `you` for yourself, the local petname if set, the key's head otherwise. */
function nameFor(key: string, members: MemberDto[]): string {
  const m = members.find((x) => x.key === key);
  if (m?.me) return "you";
  return m?.alias.trim() || short(key);
}
