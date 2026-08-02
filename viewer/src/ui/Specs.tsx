import { useEffect, useRef, useState } from "react";
import { Plus } from "lucide-react";

import { groupByKind, SPEC_KIND_LABEL, SPEC_KIND_PLURAL } from "../core/specs";
import { useProjectSpecs, useProjectViewerStore, useSpec } from "../projectStore";
import type { SpecKind, SpecView } from "../types";
import { ApplicationState } from "./AppState";
import { GroupHeader } from "./layout";
import { Markdown } from "./Markdown";
import { MarkdownEditor } from "./MarkdownEditor";
import { NewSpecDialog } from "./NewSpec";
import { Button, IconButton, cn, interactiveRow } from "./primitives";
import { when } from "./time";

/**
 * The project's Specs — an Issue says what work is happening, a Spec says what
 * that work is meant to satisfy.
 *
 * **A Spec here is a document, and almost nothing else.** It has a kind, a
 * title, a body and an author, because those are the facts it holds the moment
 * it exists. Everything the engine can additionally record about one — its
 * revision trail, its lifecycle state, the exact revision that governs, what it
 * is bound into, what verifies it — is a fact that *happens* to a document
 * later, and this surface draws none of it yet.
 *
 * That is the rule the rest of this surface will be built by, so it is worth
 * stating before there is anything to state it about: **what a Spec draws is a
 * function of what has happened to it.** A row does not reserve a column for a
 * lifecycle it has not entered, and the reader does not print a revision
 * coordinate while there is only one revision for it to name. Each fact earns
 * exactly one affordance when it arrives, and gives it back when it goes.
 *
 * The alternative — draw the whole schema and grey out what is absent — is how a
 * clean tracker becomes compliance software: every document, however small,
 * pays the visual cost of the largest document the model can express.
 */
export function Specs({
  spaceId,
  project,
  projectName,
  readOnly,
  spec,
  composing,
  onCompose,
  onOpen,
  onError,
}: {
  spaceId: string;
  /** The project handle the register is scoped to — a KEY, or `null` for the
   *  whole space. Creation needs one, so it is offered only when there is one. */
  project: string | null;
  projectName: string;
  readOnly: boolean;
  /** The open document, or `null` for the register. */
  spec: string | null;
  /** The composer: a kind to seed it with, `"any"` to let it ask, `null` shut.
   *  Held by the shell because the toolbar's button is the shell's. */
  composing: SpecKind | "any" | null;
  onCompose: (next: SpecKind | "any" | null) => void;
  onOpen: (spec: string | null) => void;
  onError: (message: string) => void;
}) {
  const store = useProjectViewerStore();

  const create = (kind: SpecKind, title: string) => {
    if (!project) return;
    onCompose(null);
    void store
      .createSpec(spaceId, project, kind, title)
      // Straight into the document. A create that returns you to the list makes
      // you find the thing you just made, and the body is empty precisely
      // because writing it is the next thing you were going to do.
      .then((created) => onOpen(created.spec))
      .catch((reason: unknown) => onError(reason instanceof Error ? reason.message : String(reason)));
  };

  return (
    <>
      {spec ? (
        <SpecReader spaceId={spaceId} spec={spec} readOnly={readOnly} onError={onError} />
      ) : (
        <Register
          spaceId={spaceId}
          project={project}
          readOnly={readOnly}
          onOpen={onOpen}
          onCompose={onCompose}
        />
      )}
      {composing !== null && project && (
        <NewSpecDialog
          projectName={projectName}
          {...(composing === "any" ? {} : { kind: composing })}
          onCancel={() => onCompose(null)}
          onCreate={create}
        />
      )}
    </>
  );
}

/**
 * The register.
 *
 * Grouped by kind rather than filtered by it, and in the chain's order rather
 * than the alphabet's: read top to bottom, a project's documents should read as
 * intent, then the outcomes it demands, then how they are met. Kinds nobody has
 * written are absent — unlike a status column, which exists because the workflow
 * says so whether or not anything is in it, an unused kind is not a bucket
 * somebody left open.
 *
 * The row is a title and a time, and that is the whole grammar. There is no key
 * column because a Spec has no per-project alias to put in one, and no state
 * chip because a draft is what every document here is until something happens
 * to it — a badge that appears on every row is a column of the same word.
 */
function Register({
  spaceId,
  project,
  readOnly,
  onOpen,
  onCompose,
}: {
  spaceId: string;
  project: string | null;
  readOnly: boolean;
  onOpen: (spec: string) => void;
  onCompose: (next: SpecKind | "any") => void;
}) {
  const specs = useProjectSpecs(spaceId, project);

  if (specs.error) {
    return (
      <ApplicationState
        kind="unavailable"
        title="Specs unavailable"
        body="This project's specs could not be read from the local replica. Known issues remain available."
      />
    );
  }
  if (!specs.data) {
    return <ApplicationState kind="loading" title="Loading specs" />;
  }

  const groups = groupByKind(specs.data);
  if (groups.length === 0) {
    return (
      <ApplicationState
        kind="empty"
        title="No specs yet"
        body="A spec is what the work is meant to satisfy — a goal, a requirement, a design, a record of what was decided."
        action={
          !readOnly && project ? (
            <Button variant="primary" onClick={() => onCompose("any")}>
              <Plus className="size-icon-sm" /> New spec
            </Button>
          ) : undefined
        }
        className="min-h-60"
      />
    );
  }

  return (
    <div className="@container min-h-0 flex-1 overflow-y-auto">
      {groups.map(({ kind, specs: rows }) => (
        <section key={kind}>
          <GroupHeader
            sticky
            title={SPEC_KIND_PLURAL[kind]}
            count={rows.length}
            actions={
              !readOnly && project ? (
                // Always visible, like the issue list's: adding another of the
                // kind you are already reading is the header's one action.
                <IconButton
                  label={`New ${SPEC_KIND_LABEL[kind].toLowerCase()}`}
                  onClick={() => onCompose(kind)}
                >
                  <Plus className="size-icon-sm" />
                </IconButton>
              ) : undefined
            }
          />
          <ul aria-label={SPEC_KIND_PLURAL[kind]}>
            {rows.map((row) => (
              <SpecRow key={row.spec} spec={row} onOpen={onOpen} />
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}

function SpecRow({ spec, onOpen }: { spec: SpecView; onOpen: (spec: string) => void }) {
  return (
    <li
      className={cn(interactiveRow({ size: "lg" }), "flex items-center gap-3 px-4")}
      onClick={() => onOpen(spec.spec)}
      onKeyDown={(event) => {
        if (event.target === event.currentTarget && event.key === "Enter") {
          event.preventDefault();
          onOpen(spec.spec);
        }
      }}
      data-spec-id={spec.spec}
      tabIndex={0}
    >
      <span className="min-w-0 flex-1 truncate font-medium">{spec.title}</span>
      {/* The one thing every document holds from birth that a list needs: which
          of these did I touch last. Right-aligned and quiet — it is how you find
          a row, not something you read. */}
      <span className="text-mute shrink-0 text-2xs tabular-nums">{when(spec.body.ts)}</span>
    </li>
  );
}

/**
 * The document.
 *
 * A title and a body, set as prose. Editing is not a mode: the title takes a
 * caret and the body is live Markdown, exactly as an issue's are — the two
 * surfaces read the same because they are both documents, and only one of them
 * is about to grow a lifecycle.
 *
 * What editing *means* here is different, and the difference is the whole model:
 * every commit writes a new immutable revision against the head it was composed
 * on. The engine refuses a write against a stale head rather than merging one,
 * so the reader always sends the revision it is showing.
 */
function SpecReader({
  spaceId,
  spec,
  readOnly,
  onError,
}: {
  spaceId: string;
  spec: string;
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const store = useProjectViewerStore();
  const resource = useSpec(spaceId, spec);
  const view = resource.data;
  const [title, setTitle] = useState(view?.title ?? "");
  const titleRef = useRef<HTMLTextAreaElement>(null);
  const body = useRef<string | null>(null);

  // The authoritative title wins whenever it changes underneath — a doorbell
  // mid-typing is the one case this loses to, and it is the same trade the
  // issue title makes.
  useEffect(() => {
    if (view) setTitle(view.title);
  }, [view?.spec, view?.revision]); // eslint-disable-line react-hooks/exhaustive-deps

  // Grow the title to its content: reset to `auto` first, because `scrollHeight`
  // never reports less than the height already set.
  useEffect(() => {
    const el = titleRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [title]);

  const revise = (patch: { title?: string; text?: string }) => {
    if (!view) return;
    void store
      .reviseSpec(spaceId, view.spec, view.revision, patch)
      .catch((reason: unknown) => onError(reason instanceof Error ? reason.message : String(reason)));
  };

  if (resource.error) {
    return (
      <ApplicationState
        kind="unavailable"
        title="Spec unavailable"
        body="This spec could not be read from the local replica."
      />
    );
  }
  if (!view) return <ApplicationState kind="loading" title="Loading spec" />;

  const locked = readOnly;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <article className="mx-auto flex w-full max-w-[52rem] flex-col gap-6 px-10 py-10">
        <header className="flex flex-col gap-2">
          {/* Kind, and only kind. It is what the document *is*, chosen once and
              not revisable by typing, so it belongs above the title rather than
              in a row of properties that has nothing else to put in it. */}
          <span className="text-mute text-2xs font-medium tracking-wide uppercase">
            {SPEC_KIND_LABEL[view.kind]}
          </span>
          {/* A textarea, not an input: a long title should wrap rather than
              scroll sideways past the edge of the page. */}
          <textarea
            ref={titleRef}
            value={title}
            readOnly={locked}
            rows={1}
            onChange={(event) => setTitle(event.target.value)}
            onBlur={() => {
              const next = title.trim();
              if (!next || next === view.title) {
                setTitle(view.title);
                return;
              }
              revise({ title: next });
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                titleRef.current?.blur();
              }
              if (event.key === "Escape") {
                setTitle(view.title);
                titleRef.current?.blur();
              }
            }}
            className="resize-none overflow-hidden bg-transparent text-2xl leading-tight font-semibold tracking-tight outline-none"
            aria-label="Title"
          />
        </header>
        {locked ? (
          <div className="min-h-ctl-xl">
            {view.body.text ? (
              <Markdown text={view.body.text} />
            ) : (
              <span className="text-mute">No content</span>
            )}
          </div>
        ) : (
          <MarkdownEditor
            // Remount on a new revision so the editor reloads the committed
            // document; it reads `value` at mount and owns it from there.
            key={view.revision}
            value={view.body.text}
            placeholder="Write the spec…"
            className="min-h-ctl-xl"
            onChange={(markdown) => {
              body.current = markdown;
            }}
            onCommit={() => {
              const next = body.current;
              body.current = null;
              if (next !== null && next !== view.body.text) revise({ text: next });
            }}
          />
        )}
      </article>
    </div>
  );
}
