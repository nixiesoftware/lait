import { useEffect, useRef, useState } from "react";
import { AlertTriangle, ArrowUp, Ban, ChevronDown, Eye, History, MoreHorizontal, PencilLine, Plus, Stamp, X } from "lucide-react";

import {
  applyLinkDelta,
  authorityPhrase,
  commonAncestor,
  diffBodies,
  emptyDelta,
  groupByKind,
  holds,
  incomingFor,
  linkDelta,
  linkKey,
  linkPhrase,
  SPEC_KIND_LABEL,
  SPEC_KIND_PLURAL,
  SPEC_REL_LABEL,
  SPEC_RELS,
  SPEC_STATE_LABEL,
  standing,
  standingLabel,
  targetLabel,
  transitions,
  verificationGap,
  type LinkDelta,
  type SpecStanding,
  type SpecTransition,
} from "../core/specs";
import {
  DOCUMENT_PREFIX,
  DOCUMENT_SCHEMA,
  documentPlainText,
  upgradeMarkdown,
} from "../core/document";
import {
  useBaseline,
  useBaselineHistory,
  useGrants,
  useProjectBaselines,
  useProjectBoard,
  usePlanGeometry,
  useProjectSpecs,
  useProjectViewerStore,
  useSpecObservations,
  useSpecReferences,
  useSpec,
  useSpecHistory,
} from "../projectStore";
import type {
  AssignmentDto,
  BaselineView,
  MemberDto,
  PlanData,
  Row,
  SpecBody,
  SpecKind,
  SpecLink,
  SpecObservation,
  SpecRef,
  SpecReference,
  SpecRel,
  SpecRevision,
  SpecState,
  SpecTarget,
  SpecView,
} from "../types";
import { ApplicationState } from "./AppState";
import * as ask from "./dialogs";
import { GroupHeader } from "./layout";
import { Document } from "./Document";
import { Markdown } from "./Markdown";
import { MarkdownEditor } from "./MarkdownEditor";
import { PlanIdentity, PlanSurface, planCounts } from "./Plan";
import { NewSpecDialog } from "./NewSpec";
import { Button, DropdownMenu, DropdownMenuItem, IconButton, TextArea, TextInput } from "@astryxdesign/core";
import { Combobox, type Option } from "./Picker";
import { cn, interactiveRow } from "./primitives";
import { short, when } from "./time";

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
  baseline,
  members,
  composing,
  onCompose,
  onOpen,
  onOpenBaseline,
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
  /** The open set. Takes precedence — two nouns, one register, one reader each. */
  baseline: string | null;
  /** The ACL, for the one question this surface asks of it: am I an admin, and
   *  therefore hold every capability without a scoped grant saying so. */
  members: MemberDto[];
  /** The composer: a kind to seed it with, `"any"` to let it ask, `null` shut.
   *  Held by the shell because the toolbar's button is the shell's. */
  composing: SpecKind | "any" | null;
  onCompose: (next: SpecKind | "any" | null) => void;
  onOpen: (spec: string | null) => void;
  onOpenBaseline: (baseline: string | null) => void;
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

  const createBaseline = () => {
    if (!project) return;
    void (async () => {
      const name = await ask.prompt({
        title: "New baseline",
        body: "A named set of exact issued revisions. It starts empty; you pin members next.",
        label: "Name",
        confirmText: "Create",
      });
      if (!name) return;
      try {
        const created = await store.createBaseline(spaceId, project, name, []);
        onOpenBaseline(created.baseline);
      } catch (reason) {
        onError(reason instanceof Error ? reason.message : String(reason));
      }
    })();
  };

  return (
    <>
      {baseline ? (
        <BaselineReader
          spaceId={spaceId}
          baseline={baseline}
          members={members}
          readOnly={readOnly}
          onError={onError}
        />
      ) : spec ? (
        <SpecReader
          spaceId={spaceId}
          project={project}
          spec={spec}
          members={members}
          readOnly={readOnly}
          onOpen={onOpen}
          onError={onError}
        />
      ) : (
        <Register
          spaceId={spaceId}
          project={project}
          readOnly={readOnly}
          onOpen={onOpen}
          onOpenBaseline={onOpenBaseline}
          onCompose={onCompose}
          onComposeBaseline={createBaseline}
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
  onOpenBaseline,
  onCompose,
  onComposeBaseline,
}: {
  spaceId: string;
  project: string | null;
  readOnly: boolean;
  onOpen: (spec: string) => void;
  onOpenBaseline: (baseline: string) => void;
  onCompose: (next: SpecKind | "any") => void;
  onComposeBaseline: () => void;
}) {
  const specs = useProjectSpecs(spaceId, project);
  const baselines = useProjectBaselines(spaceId, project).data ?? [];
  const references = useSpecReferences(spaceId, project).data ?? [];

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
  if (groups.length === 0 && baselines.length === 0) {
    return (
      <ApplicationState
        kind="empty"
        title="No specs yet"
        body="A spec is what the work is meant to satisfy — a goal, a requirement, a design, a record of what was decided."
        action={
          !readOnly && project ? (
            <Button
              onClick={() => onCompose("any")}
              icon={<Plus className="size-icon-sm" />}
              label="New spec"
              variant="primary"
              size="sm"
            />
          ) : undefined
        }
        className="min-h-60"
      />
    );
  }

  return (
    <div className="@container min-h-0 flex-1 overflow-y-auto">
      {/* A different noun, so a different row shape and its own group — not a
          kind alongside the others. A Baseline says which exact revisions were
          agreed together; a Spec says one thing. */}
      {baselines.length > 0 && (
        <section>
          <GroupHeader sticky title="Baselines" count={baselines.length} />
          <ul aria-label="Baselines">
            {baselines.map((row) => (
              <li
                key={row.baseline}
                className={cn(interactiveRow({ size: "lg" }), "flex items-center gap-3 px-4")}
                onClick={() => onOpenBaseline(row.baseline)}
                onKeyDown={(event) => {
                  if (event.target === event.currentTarget && event.key === "Enter") {
                    event.preventDefault();
                    onOpenBaseline(row.baseline);
                  }
                }}
                data-baseline-id={row.baseline}
                tabIndex={0}
              >
                <span className="min-w-0 flex-1 truncate font-medium">{row.body.name}</span>
                <span className="text-mute shrink-0 text-2xs">
                  {row.body.members.length} member{row.body.members.length === 1 ? "" : "s"}
                </span>
                {row.issued.length === 1 && (
                  <span className="text-mute shrink-0 text-2xs">
                    {row.issued[0] === row.revision ? "Issued" : "Issued · draft ahead"}
                  </span>
                )}
                <span className="text-mute w-14 shrink-0 text-right text-2xs tabular-nums">
                  {when(row.body.ts)}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}
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
                  variant="ghost"
                  size="sm"
                  tooltip={`New ${SPEC_KIND_LABEL[kind].toLowerCase()}`}
                  icon={<Plus className="size-icon-sm" />}
                />
              ) : undefined
            }
          />
          <ul aria-label={SPEC_KIND_PLURAL[kind]}>
            {rows.map((row) => (
              <SpecRow
                key={row.spec}
                spec={row}
                gap={verificationGap(row, references)}
                onOpen={onOpen}
              />
            ))}
          </ul>
        </section>
      ))}
      {/* Offered under the documents rather than beside "New spec": assembling a
          set is something you do once there is something to put in it. */}
      {!readOnly && project && specs.data.some((row) => row.issued.length === 1) && (
        <div className="px-4 py-3">
          <Button
            onClick={onComposeBaseline}
            icon={<Plus className="size-icon-sm" />}
            label="New baseline"
            variant="secondary"
            elevation="low"
            size="md"
          />
        </div>
      )}
    </div>
  );
}

function SpecRow({
  spec,
  gap,
  onOpen,
}: {
  spec: SpecView;
  /** In force, and nothing verifies it — the gap a coverage matrix looks for. */
  gap: boolean;
  onOpen: (spec: string) => void;
}) {
  const label = standingLabel(standing(spec));
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
      <span className="min-w-0 flex-1 truncate font-medium">
        {spec.kind === "plan"
          ? <PlanIdentity title={spec.title} compact />
          : spec.title}
      </span>
      {/* Not styled as a warning: nothing is broken, and a wall of amber over
          a project that has not written its proofs yet would be. It is a gap,
          said once, on the rows where it is actually a gap. */}
      {gap && <span className="text-mute shrink-0 text-2xs">unverified</span>}
      {/* A word, not a chip, and only on rows that have one to say. A pill on
          every row turns the column into a wall of the same string and takes the
          eye off the titles, which are what the register is for. Drafts — which
          is what every Spec starts as — say nothing at all. */}
      {label && (
        <span
          className={cn(
            "shrink-0 text-2xs",
            label.tone === "warn" ? "text-warn" : "text-mute",
          )}
        >
          {label.tone === "warn" && (
            <AlertTriangle className="mr-1 inline size-icon-2xs align-[-0.1em]" aria-hidden />
          )}
          {label.text}
        </span>
      )}
      {/* The one thing every document holds from birth that a list needs: which
          of these did I touch last. Right-aligned and quiet — it is how you find
          a row, not something you read. */}
      <span className="text-mute w-14 shrink-0 text-right text-2xs tabular-nums">
        {when(spec.body.ts)}
      </span>
    </li>
  );
}

/**
 * A named, reviewed set of exact issued revisions.
 *
 * The member schedule is the document. Each row is a coordinate that was issued
 * at the moment this set was assembled and cannot drift afterwards — that is the
 * whole difference between a Baseline and a saved search, and it is why the
 * composer refuses anything but an issued revision and why removing a member
 * writes a successor rather than editing what is already out.
 */
function BaselineReader({
  spaceId,
  baseline,
  members,
  readOnly,
  onError,
}: {
  spaceId: string;
  baseline: string;
  members: MemberDto[];
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const store = useProjectViewerStore();
  const resource = useBaseline(spaceId, baseline);
  const history = useBaselineHistory(spaceId, baseline).data ?? [];
  const everySpec = useProjectSpecs(spaceId, null).data ?? [];
  const grants = useGrants(spaceId).data ?? [];
  const admin = members.some((member) => member.me && member.role === "admin");
  const [adding, setAdding] = useState(false);
  const view = resource.data;

  if (resource.error) {
    return (
      <ApplicationState
        kind="unavailable"
        title="Baseline unavailable"
        body="This baseline could not be read from the local replica."
      />
    );
  }
  if (!view) return <ApplicationState kind="loading" title="Loading baseline" />;

  const conflict = view.heads.length > 1;
  const issued = view.issued.length === 1 ? view.issued[0]! : null;
  const draftAhead = issued !== null && issued !== view.revision;
  const byId = new Map(everySpec.map((candidate) => [candidate.spec, candidate]));
  // Only issued revisions may be pinned, so the pool is every Spec with exactly
  // one issued revision that is not already a member.
  const pinned = new Set(view.body.members.map((member) => member.spec));
  const available = everySpec.filter(
    (candidate) => candidate.issued.length === 1 && !pinned.has(candidate.spec),
  );
  const predecessor = history.find((entry) => view.body.members !== entry.body.members
    && entry.revision !== view.revision
    && (history.find((h) => h.revision === view.revision)?.predecessors ?? []).includes(entry.revision));
  const canIssue = admin || grants.some((grant) =>
    grant.capability === "spec.issue" &&
    (grant.resource.length === 0 || grant.resource[0] === view.project));
  const locked = readOnly || conflict;

  const revise = (patch: { name?: string; members?: SpecRef[] }) => {
    void store
      .reviseBaseline(spaceId, view.baseline, view.revision, patch)
      .catch((reason: unknown) => onError(reason instanceof Error ? reason.message : String(reason)));
  };

  const added = predecessor
    ? view.body.members.filter((member) =>
        !predecessor.body.members.some((was) => was.spec === member.spec && was.revision === member.revision))
    : [];
  const removed = predecessor
    ? predecessor.body.members.filter((was) =>
        !view.body.members.some((member) => member.spec === was.spec && member.revision === was.revision))
    : [];

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <article className="mx-auto flex w-full max-w-[52rem] flex-col gap-6 px-10 py-10">
        <header className="flex flex-col gap-2">
          <div className="flex items-center gap-3">
            <span className="text-mute text-2xs font-medium tracking-wide uppercase">Baseline</span>
            <span className="ml-auto text-mute text-xs">{SPEC_STATE_LABEL[view.state]}</span>
            {!locked && view.state !== "issued" && canIssue && (
              <Button
                onClick={() => {
                  void (async () => {
                    const ok = await ask.confirm({
                      title: `Issue revision ${short(view.revision)}?`,
                      body: `Puts these ${view.body.members.length} exact revisions in force as a set. Issues can bind to it from then on.`,
                      confirmText: "Issue",
                    });
                    if (!ok) return;
                    try {
                      await store.setBaselineState(spaceId, view.baseline, view.revision, "issued");
                    } catch (reason) {
                      onError(reason instanceof Error ? reason.message : String(reason));
                    }
                  })();
                }}
                label="Issue set"
                variant="primary"
                size="md"
              />
            )}
          </div>
          <h1 className="text-2xl leading-tight font-semibold tracking-tight">{view.body.name}</h1>
          {draftAhead && issued && (
            <p className="text-dim text-xs">
              Revision <code className="text-mute" title={issued}>{short(issued)}</code> is the
              issued set. This draft has not replaced it.
            </p>
          )}
        </header>

        {conflict && (
          <div className="border-warn/30 bg-warn/5 text-dim flex items-start gap-2 rounded-surface border p-3 text-sm">
            <AlertTriangle className="text-warn mt-0.5 size-icon-sm shrink-0" />
            <span>
              This baseline has {view.heads.length} concurrent heads. Editing and issuing stay
              unavailable until they are resolved.
            </span>
          </div>
        )}

        {/* The compare step the brief asks for, and it is a summary rather than a
            diff view: what changed about a *set* is which members joined and left. */}
        {(added.length > 0 || removed.length > 0) && (
          <p className="text-dim text-xs">
            Against its predecessor: {added.length} added, {removed.length} removed.
          </p>
        )}

        <section>
          <h2 className="text-mute mb-1 flex items-center gap-2 text-2xs font-semibold tracking-wider uppercase">
            Members
            <span className="tabular-nums normal-case">{view.body.members.length}</span>
          </h2>
          {view.body.members.length === 0 && (
            <p className="text-mute text-xs">
              Nothing pinned yet. A set with no members governs nothing.
            </p>
          )}
          <ul className="flex flex-col">
            {view.body.members.map((member) => {
              const spec = byId.get(member.spec);
              const stale = spec && !spec.issued.includes(member.revision);
              return (
                <li
                  key={`${member.spec}@${member.revision}`}
                  className="border-line/35 flex items-center gap-2 border-b py-1.5 text-xs"
                >
                  <span className="text-mute w-24 shrink-0 capitalize">
                    {spec ? SPEC_KIND_LABEL[spec.kind] : "unknown"}
                  </span>
                  <span className="min-w-0 flex-1 truncate">{spec?.title ?? member.spec}</span>
                  {/* The pin is exact, so a Spec that has moved on since is a
                      fact about the set, not a problem with it. */}
                  {stale && <span className="text-mute shrink-0 text-2xs">superseded since</span>}
                  <code className="text-mute shrink-0 text-2xs" title={`${member.spec}@${member.revision}`}>
                    {short(member.revision)}
                  </code>
                  {!locked && (
                    <button
                      type="button"
                      className="text-mute hover:text-danger shrink-0"
                      title="Remove from this set (writes a successor draft)"
                      onClick={() =>
                        revise({
                          members: view.body.members.filter(
                            (candidate) =>
                              !(candidate.spec === member.spec && candidate.revision === member.revision),
                          ),
                        })
                      }
                    >
                      ✕
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
          {!locked && (
            <div className="mt-2">
              {adding ? (
                <div className="border-line rounded-surface border p-2">
                  <p className="text-mute mb-1 text-2xs">
                    Only issued revisions can be pinned. Picking one pins that exact revision, not
                    the document.
                  </p>
                  {available.length === 0 && (
                    <p className="text-mute text-xs">Nothing else is issued in this space yet.</p>
                  )}
                  <ul className="flex flex-col">
                    {available.map((candidate) => (
                      <li key={candidate.spec}>
                        <button
                          type="button"
                          className="hover:bg-hover flex w-full items-center gap-2 rounded-control px-2 py-1 text-left text-xs"
                          onClick={() => {
                            setAdding(false);
                            revise({
                              members: [
                                ...view.body.members,
                                { spec: candidate.spec, revision: candidate.issued[0]! },
                              ],
                            });
                          }}
                        >
                          <span className="text-mute w-24 shrink-0 capitalize">
                            {SPEC_KIND_LABEL[candidate.kind]}
                          </span>
                          <span className="min-w-0 flex-1 truncate">{candidate.title}</span>
                          <code className="text-mute shrink-0 text-2xs">
                            {short(candidate.issued[0]!)}
                          </code>
                        </button>
                      </li>
                    ))}
                  </ul>
                  <Button
                    className="mt-2"
                    onClick={() => setAdding(false)}
                    label="Done"
                    variant="secondary"
                    elevation="low"
                    size="md"
                  />
                </div>
              ) : (
                <Button
                  onClick={() => setAdding(true)}
                  icon={<Plus className="size-icon-sm" />}
                  label="Add an issued revision"
                  variant="secondary"
                  elevation="low"
                  size="md"
                />
              )}
            </div>
          )}
        </section>
      </article>
    </div>
  );
}

/**
 * What this document claims, and what claims it.
 *
 * Both directions, because half a graph answers half the questions: outgoing
 * says what its author asserted, incoming says who else is relying on it — and
 * "what verifies this requirement" is only ever an incoming edge.
 *
 * Grouped by verb and read as sentences. The relation is the claim; a row that
 * showed `VERIFIES` beside an id would make the reader decode an enum to learn
 * something the author said in words.
 *
 * **Incoming is computed from head revisions only.** A link asserted by a
 * revision that has since been superseded is not shown, which is the honest
 * reading — that assertion belongs to a revision nobody is standing behind any
 * more — but it does mean this is the current graph, not the whole one.
 */
function Relations({
  view,
  everySpec,
  baselines,
  rows,
  references,
  readOnly,
  onOpen,
  onCommit,
}: {
  view: SpecView;
  everySpec: SpecView[];
  /** What a relation may name, resolved by the reader — one subscription for
   *  the two blocks that offer them rather than one each. */
  baselines: BaselineView[];
  rows: Row[];
  references: SpecReference[];
  readOnly: boolean;
  onOpen: (spec: string) => void;
  onCommit: (delta: LinkDelta) => void;
}) {
  /** The assertions as this author has them, or `null` while they match the
   *  committed body. Staged rather than written per click: each revise mints a
   *  revision, and wiring up a document's relations one revision at a time
   *  turns the rail — the one place a reader goes to see what changed — into a
   *  column of single-link commits. */
  const [staged, setStaged] = useState<SpecLink[] | null>(null);
  const [composing, setComposing] = useState(false);

  // A new revision — this author's commit landing, or somebody else's arriving
  // — retires the staging. A delta composed against a body that has moved is no
  // longer the edit its author was looking at, and silently carrying it forward
  // would let them save a claim about text they never read.
  useEffect(() => {
    setStaged(null);
    setComposing(false);
  }, [view.revision]);

  const links = staged ?? view.body.links;
  const delta = linkDelta(view.body.links, links);
  const dirty = !emptyDelta(delta);
  const editable = !readOnly;

  const titles = new Map(everySpec.map((candidate) => [candidate.spec, candidate]));
  const outgoing = groupLinks(links.map((link) => ({ link, from: null })));
  const incoming = groupLinks(
    incomingFor(view.spec, references).map((reference) => ({
      link: reference.link,
      from: titles.get(reference.spec) ?? null,
      title: reference.title,
      source: reference.spec,
      revision: reference.revision,
    })),
  );
  if (outgoing.length === 0 && incoming.length === 0 && !editable) return null;

  const drop = (link: SpecLink) =>
    setStaged(links.filter((candidate) => linkKey(candidate) !== linkKey(link)));

  const row = (entry: LinkEntry, direction: "in" | "out") => {
    const open =
      direction === "out" && entry.link.target.kind === "spec"
        ? entry.link.target.spec
        : entry.source;
    const label =
      direction === "out"
        ? (entry.link.target.kind === "spec"
            ? titles.get(entry.link.target.spec)?.title
            : undefined) ?? targetLabel(entry.link.target)
        : (entry.from?.title ?? entry.title ?? entry.source ?? "");
    const coordinate =
      direction === "out"
        ? targetLabel(entry.link.target)
        : `${entry.source}@${short(entry.revision ?? "")}`;
    const added =
      direction === "out" && delta.added.some((link) => linkKey(link) === linkKey(entry.link));
    const namedPlan = open ? titles.get(open) : undefined;
    const identity = namedPlan?.kind === "plan"
      ? <PlanIdentity title={namedPlan.title} compact />
      : label;
    return (
      <li
        key={`${direction}-${linkPhrase(entry.link)}-${entry.source ?? ""}`}
        className="flex items-center gap-2"
      >
        {open ? (
          <button type="button" className="hover:text-accent text-left" onClick={() => onOpen(open)}>
            {identity}
          </button>
        ) : (
          <span>{identity}</span>
        )}
        <code className="text-mute text-2xs" title={coordinate}>
          {coordinate}
        </code>
        {/* Staged, not saved. Said in words on the row that carries it, because
            the difference between "this document claims that" and "I am about to
            make it claim that" is the whole of what the save button decides. */}
        {added && <span className="text-mute text-2xs">not saved yet</span>}
        {direction === "out" && editable && (
          <IconButton
            label={`Remove ${linkPhrase(entry.link)}`}
            tooltip="Remove this relation"
            onClick={() => drop(entry.link)}
            variant="ghost"
            size="sm"
            icon={<X className="size-icon-2xs" />}
          />
        )}
      </li>
    );
  };

  return (
    <section className="flex flex-col gap-3 text-xs">
      {incoming.length > 0 && (
        <div>
          <h2 className="text-mute mb-1 text-2xs font-semibold tracking-wider uppercase">
            Referenced by
          </h2>
          {incoming.map(([rel, entries]) => (
            <div key={rel} className="mb-1">
              <span className="text-dim">
                {entries.length === 1 ? "One document" : `${entries.length} documents`}{" "}
                {SPEC_REL_LABEL[rel]} this
              </span>
              <ul className="text-mute mt-0.5 flex flex-col gap-0.5 pl-3">
                {entries.map((entry) => row(entry, "in"))}
              </ul>
            </div>
          ))}
        </div>
      )}
      {(outgoing.length > 0 || editable) && (
        <div>
          <h2 className="text-mute mb-1 text-2xs font-semibold tracking-wider uppercase">
            This document
          </h2>
          {outgoing.map(([rel, entries]) => (
            <div key={rel} className="mb-1">
              <span className="text-dim capitalize">{SPEC_REL_LABEL[rel]}</span>
              <ul className="text-mute mt-0.5 flex flex-col gap-0.5 pl-3">
                {entries.map((entry) => row(entry, "out"))}
              </ul>
            </div>
          ))}
          {/* Removals leave no row behind to annotate, so they are said here or
              nowhere — and "saved without the thing I deleted" is exactly the
              outcome somebody needs to be able to check before pressing save. */}
          {delta.removed.length > 0 && (
            <p className="text-mute mt-1 text-2xs">
              {delta.removed.length === 1
                ? "One relation removed, not saved yet"
                : `${delta.removed.length} relations removed, not saved yet`}
              : {delta.removed.map((link) => linkPhrase(link)).join(", ")}
            </p>
          )}
          {editable && (
            <div className="mt-2 flex flex-col gap-2">
              {composing ? (
                <RelationComposer
                  self={view.spec}
                  everySpec={everySpec}
                  baselines={baselines}
                  rows={rows}
                  onAdd={(link) => {
                    setComposing(false);
                    // Through the same set semantics the rebase uses, so
                    // asserting something twice is a no-op here exactly as it
                    // is when it lands on a head that moved.
                    setStaged(applyLinkDelta(links, { added: [link], removed: [] }));
                  }}
                  // A Link's argument is the document it sits in. Only a note,
                  // which sits in no document, has to carry its own.
                  onCancel={() => setComposing(false)}
                />
              ) : (
                <span>
                  <Button
                    onClick={() => setComposing(true)}
                    icon={<Plus className="size-icon-sm" />}
                    label="Add a relation"
                    variant="secondary"
                    elevation="low"
                    size="md"
                  />
                </span>
              )}
              {dirty && (
                <div className="flex items-center gap-2">
                  <Button
                    onClick={() => onCommit(delta)}
                    label={
                      delta.added.length + delta.removed.length === 1
                        ? "Save 1 change"
                        : `Save ${delta.added.length + delta.removed.length} changes`
                    }
                    variant="primary"
                    size="md"
                  />
                  <Button
                    onClick={() => setStaged(null)}
                    label="Discard"
                    variant="secondary"
                    elevation="low"
                    size="md"
                  />
                  {/* Saving writes a revision, and on an issued document that
                      revision is a draft successor rather than a change to what
                      governs. Said before the press, not discovered after it. */}
                  <span className="text-mute text-2xs">
                    Saves as one revision
                    {view.state === "issued" ? " — a draft successor, not a change to the issued one" : ""}
                  </span>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

/**
 * Composing one typed assertion: a verb, a thing, and the exact revision of it.
 *
 * The revision is the part that cannot be skipped and the part nobody wants to
 * choose, so it is chosen for them and then stated: the issued revision when a
 * document has one, because an assertion about governing material almost always
 * means the material that governs, and the head otherwise. Stated rather than
 * silent, because "verifies REQ" and "verifies REQ at the revision that was
 * current on Tuesday" are different claims and only one of them is what got
 * written.
 *
 * Issues are the exception the model makes deliberately — `Target::Issue`
 * carries no revision, because an Issue is a stable identity whose changing
 * work state is not a document revision.
 */
function RelationComposer({
  self,
  everySpec,
  baselines,
  rows,
  lead,
  withNote,
  onAdd,
  onCancel,
}: {
  self: string;
  everySpec: SpecView[];
  baselines: BaselineView[];
  rows: Row[];
  /** What the sentence opens with — the difference between the document
   *  asserting something and somebody noticing it. */
  lead?: string;
  /** Ask for the reasoning too. An observation binds nobody, so the argument
   *  behind it is the only thing that makes it worth anything to the next
   *  reader; a Link has a whole document behind it and needs no such field. */
  withNote?: boolean;
  onAdd: (link: SpecLink, note: string) => void;
  onCancel: () => void;
}) {
  const [rel, setRel] = useState<SpecRel>("references");
  const [kind, setKind] = useState<SpecTarget["kind"]>("spec");
  const [target, setTarget] = useState<string | null>(null);
  const [note, setNote] = useState("");

  // Only issued revisions carry a governing claim, but a link may name any
  // revision that exists — so the default prefers what governs and falls back
  // to the head rather than refusing to offer a document nobody has issued.
  const pinned = (candidate: { issued: string[]; revision: string }) =>
    candidate.issued.length === 1 ? candidate.issued[0]! : candidate.revision;

  const specs = everySpec.filter((candidate) => candidate.spec !== self);
  const options: Option[] =
    kind === "spec"
      ? specs.map((candidate) => ({
          id: candidate.spec,
          label: candidate.title,
          kicker: SPEC_KIND_LABEL[candidate.kind],
          hint: short(pinned(candidate)),
        }))
      : kind === "baseline"
        ? baselines.map((candidate) => ({
            id: candidate.baseline,
            label: candidate.body.name,
            kicker: "Baseline",
            hint: short(pinned(candidate)),
          }))
        : rows
            .filter((entry) => !entry.tombstone)
            .map((entry) => ({ id: entry.reff, label: entry.title, kicker: entry.reff }));

  const chosen = options.find((option) => option.id === target) ?? null;

  const build = (): SpecLink | null => {
    if (!target) return null;
    if (kind === "issue") return { rel, target: { kind: "issue", issue: target } };
    if (kind === "spec") {
      const candidate = specs.find((entry) => entry.spec === target);
      return candidate
        ? { rel, target: { kind: "spec", spec: target, revision: pinned(candidate) } }
        : null;
    }
    const candidate = baselines.find((entry) => entry.baseline === target);
    return candidate
      ? { rel, target: { kind: "baseline", baseline: target, revision: pinned(candidate) } }
      : null;
  };

  const link = build();

  return (
    <div className="border-line rounded-surface flex flex-col gap-2 border p-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-mute text-2xs">{lead ?? "This document"}</span>
        <Combobox
          label="Relation"
          heading="Relation"
          value={{ id: rel, label: SPEC_REL_LABEL[rel] }}
          onPick={(id) => setRel(id as SpecRel)}
          options={SPEC_RELS.map((candidate) => ({
            id: candidate,
            label: SPEC_REL_LABEL[candidate],
          }))}
          size="sm"
        />
        <Combobox
          label="Target kind"
          heading="Target"
          value={{ id: kind, label: TARGET_KIND_LABEL[kind] }}
          onPick={(id) => {
            setKind(id as SpecTarget["kind"]);
            setTarget(null);
          }}
          options={(["spec", "baseline", "issue"] as const).map((candidate) => ({
            id: candidate,
            label: TARGET_KIND_LABEL[candidate],
          }))}
          size="sm"
        />
        <Combobox
          label="Choose one"
          heading={TARGET_KIND_LABEL[kind]}
          value={chosen}
          onPick={setTarget}
          options={options}
          emptyText={`Nothing to reference — no ${TARGET_KIND_LABEL[kind].toLowerCase()} is readable here.`}
          size="sm"
          wide
        />
      </div>
      {link && link.target.kind !== "issue" && (
        <p className="text-mute text-2xs">
          Pins revision{" "}
          <code title={link.target.revision}>{short(link.target.revision)}</code> — the exact one,
          which does not follow the document if it is revised.
        </p>
      )}
      {withNote && (
        <TextArea
          label="Why"
          value={note}
          onChange={setNote}
          placeholder="What did you notice, and what makes you think so?"
          rows={2}
          width="100%"
        />
      )}
      <div className="flex items-center gap-2">
        <Button
          onClick={() => {
            if (link) onAdd(link, note.trim());
          }}
          isDisabled={!link}
          label="Add"
          variant="primary"
          size="md"
        />
        <Button onClick={onCancel} label="Cancel" variant="secondary" elevation="low" size="md" />
      </div>
    </div>
  );
}

/** What a Link may point at, in the words the composer offers them. */
const TARGET_KIND_LABEL: Record<SpecTarget["kind"], string> = {
  spec: "Spec",
  baseline: "Baseline",
  issue: "Issue",
};

/**
 * What people have noticed about this document, as opposed to what it says.
 *
 * Its own section, under the relations rather than mixed into them, and phrased
 * from the observer outward — "Omar noticed this conflicts with X" — because the
 * subject of the sentence is the whole distinction. A note rendered like a
 * relation would read as the document's own claim, and the one thing an
 * Observation must never do is look enforcing: it is in no revision, it is not
 * issued with the document, and it never reaches an issue's packet.
 *
 * So the section says that, once, in a line nobody has to hover to find. A
 * reader who takes a note for an order has been failed by this surface, not by
 * the person who wrote it.
 */
function Observations({
  view,
  observations,
  members,
  everySpec,
  baselines,
  rows,
  readOnly,
  grants,
  admin,
  onOpen,
  onObserve,
  onRetract,
}: {
  view: SpecView;
  observations: SpecObservation[];
  members: MemberDto[];
  everySpec: SpecView[];
  baselines: BaselineView[];
  rows: Row[];
  readOnly: boolean;
  grants: AssignmentDto[];
  admin: boolean;
  onOpen: (spec: string) => void;
  onObserve: (link: SpecLink, note: string) => void;
  onRetract: (observation: SpecObservation) => void;
}) {
  const [composing, setComposing] = useState(false);
  const me = members.find((member) => member.me);
  const titles = new Map(everySpec.map((candidate) => [candidate.spec, candidate]));
  const named = (key: string) =>
    members.find((member) => member.key === key)?.alias ?? `${key.slice(0, 10)}…`;

  // Both ends. A note filed against another document that names this one is
  // every bit as much about this one, and a reader who only saw the near half
  // would think nobody had raised it.
  const mine = observations.filter((entry) => entry.spec === view.spec);
  const about = observations.filter(
    (entry) =>
      entry.spec !== view.spec &&
      entry.target.kind === "spec" &&
      entry.target.spec === view.spec,
  );
  const shown = [...mine, ...about];
  if (shown.length === 0 && readOnly) return null;

  // Retraction is the observer's own by right; anyone else is making a
  // judgement about the record, which is the issuing capability's business.
  const mayRetract = (entry: SpecObservation) =>
    !readOnly && (entry.observer === me?.key || holds("spec.issue", view, grants, admin));

  return (
    <section className="flex flex-col gap-2 text-xs">
      <h2 className="text-mute text-2xs font-semibold tracking-wider uppercase">Noticed</h2>
      <p className="text-mute text-2xs">
        Notes about this document, not claims it makes. Nothing here governs any work, is issued
        with the document, or counts as verification.
      </p>
      <ul className="flex flex-col gap-1.5">
        {shown.map((entry) => {
          // The sentence turns around depending on which end this document is.
          // Filed here, it reads "… noticed this conflicts with X"; filed on X,
          // it reads "… noticed X conflicts with this" — same fact, and the one
          // that names this document as the subject has to say so.
          const near = entry.spec === view.spec;
          const far = near ? entry.target : ({ kind: "spec", spec: entry.spec, revision: "" } as const);
          const open = far.kind === "spec" ? far.spec : null;
          const label =
            far.kind === "spec"
              ? titles.get(far.spec)?.title ?? far.spec
              : targetLabel(far);
          const other = open ? (
            <button type="button" className="hover:text-accent" onClick={() => onOpen(open)}>
              {label}
            </button>
          ) : (
            <span>{label}</span>
          );
          return (
            <li key={entry.observation} className="flex flex-col gap-0.5">
              <span className="text-dim flex flex-wrap items-center gap-x-1.5">
                <span className="text-fg">{named(entry.observer)}</span>
                <span>noticed</span>
                {near ? <span>this</span> : other}
                <span>{SPEC_REL_LABEL[entry.rel]}</span>
                {near ? other : <span>this</span>}
                <span className="text-mute text-2xs">{when(entry.ts)}</span>
                {mayRetract(entry) && (
                  <IconButton
                    label="Retract this note"
                    tooltip="Retract this note"
                    onClick={() => onRetract(entry)}
                    variant="ghost"
                    size="sm"
                    icon={<X className="size-icon-2xs" />}
                  />
                )}
              </span>
              {entry.note && <span className="text-mute pl-3">{entry.note}</span>}
            </li>
          );
        })}
      </ul>
      {!readOnly &&
        (composing ? (
          <RelationComposer
            self={view.spec}
            everySpec={everySpec}
            baselines={baselines}
            rows={rows}
            lead="I noticed this"
            withNote
            onAdd={(link, note) => {
              setComposing(false);
              onObserve(link, note);
            }}
            onCancel={() => setComposing(false)}
          />
        ) : (
          <span>
            <Button
              onClick={() => setComposing(true)}
              icon={<Plus className="size-icon-sm" />}
              label="Note something"
              variant="secondary"
              elevation="low"
              size="md"
            />
          </span>
        ))}
    </section>
  );
}

/** One row of the relations block. Outgoing rows carry only the link; incoming
 *  rows additionally carry who asserted it, since that is not on the link. */
interface LinkEntry {
  link: SpecLink;
  from: SpecView | null;
  title?: string;
  source?: string;
  revision?: string;
}

function groupLinks(entries: LinkEntry[]): [SpecRel, LinkEntry[]][] {
  const byRel = new Map<SpecRel, LinkEntry[]>();
  for (const entry of entries) {
    byRel.set(entry.link.rel, [...(byRel.get(entry.link.rel) ?? []), entry]);
  }
  return [...byRel.entries()];
}

/**
 * Turning several heads back into one.
 *
 * Not a merge and not a choice: the result is a **new draft whose predecessors
 * are every head**, so both branches stay in the history and neither is deleted
 * or declared the winner. Picking a base is picking a starting point to edit,
 * which is a different act from picking a survivor.
 *
 * The acknowledgement is not ceremony. The engine compares the expected head set
 * against the live one and refuses a mismatch, so a resolution composed while a
 * third head was arriving must fail rather than quietly drop it — and a person
 * who has not looked at every head is not resolving anything.
 */
function Resolve({
  heads,
  onCancel,
  onCommit,
}: {
  heads: SpecRevision[];
  onCancel: () => void;
  onCommit: (body: SpecBody) => void;
}) {
  const [baseId, setBaseId] = useState(heads[0]?.revision ?? "");
  const base = heads.find((head) => head.revision === baseId) ?? heads[0];
  const [title, setTitle] = useState(base?.body.title ?? "");
  const [text, setText] = useState(base?.body.text ?? "");
  const [acknowledged, setAcknowledged] = useState(false);
  if (!base) return null;
  const documentSchema = text.startsWith(DOCUMENT_PREFIX) ? DOCUMENT_SCHEMA : 0;

  const rebase = (revision: string) => {
    const next = heads.find((head) => head.revision === revision);
    if (!next) return;
    setBaseId(revision);
    setTitle(next.body.title);
    setText(next.body.text);
  };

  return (
    <section className="border-line rounded-surface border">
      <header className="border-line border-b px-3 py-2">
        <h2 className="text-sm font-semibold">Resolve {heads.length} heads</h2>
        <p className="text-mute mt-0.5 text-xs">
          This writes one new draft with every head below as its predecessor. Nothing is
          discarded and no branch is chosen over another.
        </p>
      </header>
      <div className="flex flex-col gap-3 p-3">
        <fieldset className="flex flex-col gap-1.5">
          <legend className="text-mute mb-1 text-2xs font-semibold tracking-wider uppercase">
            Start from
          </legend>
          {heads.map((head) => (
            <label key={head.revision} className="flex items-start gap-2 text-xs">
              <input
                type="radio"
                name="resolve-base"
                checked={head.revision === baseId}
                onChange={() => rebase(head.revision)}
                className="mt-0.5"
              />
              <span className="min-w-0 flex-1">
                <code className="text-mute" title={head.revision}>{short(head.revision)}</code>{" "}
                {head.body.title}
                <span className="text-mute block text-2xs">
                  {SPEC_STATE_LABEL[head.body.state]} · {when(head.body.ts)}
                </span>
              </span>
            </label>
          ))}
        </fieldset>
        <TextInput label="Title" value={title} onChange={setTitle} width="100%" />
        <div className="flex flex-col gap-1.5">
          <span className="text-dim text-xs font-medium">Body</span>
          <MarkdownEditor
          key={baseId}
          value={text}
          documentSchema={documentSchema}
          onChange={(next) => setText(next)}
          onCommit={() => undefined}
          className="min-h-ctl-xl"
          />
        </div>
        <label className="text-dim flex items-start gap-2 text-xs">
          <input
            type="checkbox"
            checked={acknowledged}
            onChange={(event) => setAcknowledged(event.target.checked)}
            className="mt-0.5"
          />
          I have read all {heads.length} heads and this draft accounts for them.
        </label>
        <div className="flex justify-end gap-2">
          <Button onClick={onCancel} label="Cancel" variant="secondary" elevation="low" size="md" />
          <Button
            isDisabled={!acknowledged || !title.trim()}
            onClick={() => onCommit({
              ...base.body,
              title: title.trim(),
              text: text.startsWith(DOCUMENT_PREFIX) ? text : upgradeMarkdown(text).source,
            })}
            label="Create resolution draft"
            variant="primary"
            size="md"
          />
        </div>
      </div>
    </section>
  );
}

/**
 * What changed between two revisions.
 *
 * One column, not two. A Spec body is prose at a reading measure, and splitting
 * it into side-by-side halves costs both of them the measure that makes prose
 * readable — so the redline goes *in* the document, the way a tracked change
 * does, and the two coordinates being compared are named above it instead.
 *
 * Against the common ancestor when there is one and neither side descends from
 * the other: comparing two concurrent heads directly shows the union of two
 * independent edits and blames each on the wrong person.
 */
function Compare({
  history,
  from,
  to,
  onClose,
}: {
  history: SpecRevision[];
  from: SpecRevision;
  to: SpecRevision;
  onClose: () => void;
}) {
  const ancestorId = commonAncestor(history, from.revision, to.revision);
  const ancestor = history.find((entry) => entry.revision === ancestorId);
  // The base is the ancestor only when it is neither side — comparing a
  // revision against its own ancestor is just comparing it against `from`.
  const base =
    ancestor && ancestor.revision !== from.revision && ancestor.revision !== to.revision
      ? ancestor
      : from;
  // A revision comparison is a comparison of what readers saw, not of either
  // hidden serialization. This also keeps a legacy/current boundary revision
  // from looking like the whole document changed during migration.
  const visibleText = (text: string) => documentPlainText(
    text.startsWith(DOCUMENT_PREFIX) ? text : upgradeMarkdown(text).source,
  );
  const changes = diffBodies(
    { ...base.body, text: visibleText(base.body.text) },
    { ...to.body, text: visibleText(to.body.text) },
  );
  const planLines = (plan: PlanData | null) => plan
    ? plan.roots.length > 0
      ? plan.roots.map((root) => `Root: ${root}`)
      : ["Whole-project root"]
    : ["No morphology"];
  const unchanged =
    !changes.title &&
    !changes.state &&
    changes.text.length === 0 &&
    changes.links.added.length === 0 &&
    changes.links.removed.length === 0 &&
    !changes.plan;

  return (
    <section className="border-line rounded-surface border">
      <header className="border-line flex flex-wrap items-center gap-2 border-b px-3 py-2 text-xs">
        <span className="font-medium">Comparing</span>
        <code className="text-mute" title={base.revision}>{short(base.revision)}</code>
        <span className="text-mute">→</span>
        <code className="text-mute" title={to.revision}>{short(to.revision)}</code>
        {base !== from && (
          <span className="text-mute text-2xs">· their common ancestor</span>
        )}
        <Button
          className="ml-auto"
          onClick={onClose}
          label="Close"
          variant="secondary"
          elevation="low"
          size="md"
        />
      </header>
      <div className="flex flex-col gap-3 p-3 text-xs">
        {unchanged && <p className="text-mute">Nothing differs between these two revisions.</p>}
        {changes.title && (
          <div>
            <h3 className="text-mute mb-1 text-2xs font-semibold tracking-wider uppercase">Title</h3>
            <p className="text-danger line-through">{changes.title.from}</p>
            <p className="text-success">{changes.title.to}</p>
          </div>
        )}
        {changes.state && (
          <p className="text-mute">
            State {SPEC_STATE_LABEL[changes.state.from]} → {SPEC_STATE_LABEL[changes.state.to]}
          </p>
        )}
        {(changes.links.added.length > 0 || changes.links.removed.length > 0) && (
          <div>
            <h3 className="text-mute mb-1 text-2xs font-semibold tracking-wider uppercase">
              Relations
            </h3>
            {/* Named assertions, grouped by what they claim — never a diff of
                stringified JSON, which is the one thing the brief rules out. */}
            <ul className="flex flex-col gap-0.5">
              {changes.links.removed.map((link) => (
                <li key={`-${linkPhrase(link)}`} className="text-danger">− {linkPhrase(link)}</li>
              ))}
              {changes.links.added.map((link) => (
                <li key={`+${linkPhrase(link)}`} className="text-success">+ {linkPhrase(link)}</li>
              ))}
            </ul>
          </div>
        )}
        {changes.plan && (
          <div>
            <h3 className="text-mute mb-1 text-2xs font-semibold tracking-wider uppercase">
              Plan roots
            </h3>
            <ul className="font-mono text-2xs leading-5">
              {planLines(changes.plan.from).map((line, index) => (
                <li key={`plan-from-${index}-${line}`} className="text-danger">− {line}</li>
              ))}
              {planLines(changes.plan.to).map((line, index) => (
                <li key={`plan-to-${index}-${line}`} className="text-success">+ {line}</li>
              ))}
            </ul>
          </div>
        )}
        {changes.text.length > 0 && (
          <div>
            <h3 className="text-mute mb-1 text-2xs font-semibold tracking-wider uppercase">Body</h3>
            <pre className="overflow-x-auto font-mono text-2xs leading-5">
              {changes.text.map((op, index) => (
                <div
                  key={`${index}-${op.text}`}
                  className={cn(
                    op.op === "add" && "text-success bg-success/5",
                    op.op === "remove" && "text-danger bg-danger/5",
                    op.op === "same" && "text-mute",
                  )}
                >
                  {op.op === "add" ? "+ " : op.op === "remove" ? "− " : "  "}
                  {op.text || " "}
                </div>
              ))}
            </pre>
          </div>
        )}
      </div>
    </section>
  );
}

/**
 * The document's revisions, collapsed to a line until asked.
 *
 * One revision is not a history, so a document nobody has revised draws nothing
 * here — the rail arrives with the second revision, which is the first moment
 * there is a "before" to go back to.
 *
 * Order is the engine's: predecessors before successors. The stored order is by
 * revision id, and ids are content hashes, so a rail sorted that way would show
 * a document's life shuffled. Concurrent branches are marked rather than
 * flattened; nothing here implies one of them came out on top.
 */
function Rail({
  history,
  view,
  viewing,
  onView,
  onCompare,
}: {
  history: SpecRevision[];
  view: SpecView;
  viewing: string | null;
  onView: (revision: string | null) => void;
  onCompare: (revision: string) => void;
}) {
  const [open, setOpen] = useState(false);
  if (history.length < 2) return null;

  const heads = new Set(view.heads);
  const issued = new Set(view.issued);
  const latest = history[history.length - 1];

  return (
    <div className="text-mute text-2xs">
      <button
        type="button"
        onClick={() => setOpen((was) => !was)}
        aria-expanded={open}
        className="hover:text-fg flex items-center gap-1.5"
      >
        <ChevronDown className={`size-icon-2xs transition-transform ${open ? "" : "-rotate-90"}`} />
        {history.length} revisions
        {latest && <span>· {when(latest.body.ts)}</span>}
      </button>
      {open && (
        <ol className="border-line mt-2 flex flex-col gap-1 border-l pl-3">
          {[...history].reverse().map((entry) => {
            const current = viewing ? entry.revision === viewing : heads.has(entry.revision);
            return (
              <li key={entry.revision} className="group/rev flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => onView(heads.has(entry.revision) ? null : entry.revision)}
                  className={cn(
                    "hover:text-fg flex min-w-0 flex-1 items-center gap-2 text-left",
                    current && "text-fg",
                  )}
                  aria-current={current ? "true" : undefined}
                >
                  <code className="shrink-0" title={entry.revision}>{short(entry.revision)}</code>
                  <span className="shrink-0">{SPEC_STATE_LABEL[entry.body.state]}</span>
                  {/* Which one governs, said here too: the rail is where someone
                      goes to ask "which of these is the real one", and the answer
                      is not always the newest. */}
                  {issued.has(entry.revision) && <span className="text-accent shrink-0">governs</span>}
                  {heads.has(entry.revision) && view.heads.length > 1 && (
                    <span className="text-warn shrink-0">head</span>
                  )}
                  <span className="ml-auto shrink-0">{when(entry.body.ts)}</span>
                </button>
                {entry.revision !== view.revision && (
                  <button
                    type="button"
                    onClick={() => onCompare(entry.revision)}
                    title="Compare with the current revision"
                    className="hover:text-fg shrink-0 opacity-0 transition-opacity group-hover/rev:opacity-100 focus-visible:opacity-100"
                  >
                    ⇄
                  </button>
                )}
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

/**
 * The coordinate of the record being read, kept with the revision rail that
 * selected it rather than promoted to a page banner.
 *
 * A historical revision is expected navigation, not a warning or degraded
 * state. The quiet inline treatment preserves that distinction while the
 * explicit read-only copy keeps the document from looking like an editable
 * draft. Returning to the head is the one action this state needs.
 */
function RevisionNote({
  revision,
  written,
  onCurrent,
}: {
  revision: string;
  written: number;
  onCurrent: () => void;
}) {
  return (
    <div className="text-mute flex min-w-0 items-center gap-2 text-xs" role="status">
      <History className="size-icon-sm shrink-0" />
      <span className="min-w-0 flex-1">
        Revision <code title={revision}>{short(revision)}</code>
        <span aria-hidden="true"> · </span>
        {when(written)}
        <span aria-hidden="true"> · </span>
        read-only
      </span>
      <Button
        onClick={onCurrent}
        label="Current"
        aria-label="View current revision"
        variant="ghost"
        size="sm"
      />
    </div>
  );
}

/**
 * The head's state, and the way out of it.
 *
 * One control, not a button row: `Send for review`, `Issue` and `Withdraw` are
 * the same decision seen from three states, and a row of three verbs would put
 * two of them on screen permanently disabled. The trigger names where the
 * document stands; the menu names where it can go.
 *
 * A transition the actor cannot take stays visible and disabled, carrying the
 * capability it wants. Hiding it would answer "why can't I issue this?" with
 * nothing at all — and issuing is deliberately not ordinary contribution, so
 * that question is one people will have.
 */
/**
 * One glyph per lifecycle state, so a transition names its destination the way
 * a status menu names a status.
 *
 * Keyed by the state being moved TO, not by the verb: "Issue" and "Issued" are
 * the same fact seen from either side of the move, and a reader who learns the
 * stamp on the menu row should recognise it on the pill afterwards. That is the
 * same rule the project swatch follows — one object, one mark, whichever
 * surface you meet it on.
 */
const SPEC_STATE_ICON: Record<SpecState, React.ReactNode> = {
  draft: <PencilLine className="size-icon-sm" />,
  review: <Eye className="size-icon-sm" />,
  issued: <Stamp className="size-icon-sm" />,
  withdrawn: <Ban className="size-icon-sm" />,
};

function Lifecycle({
  view,
  state,
  grants,
  admin,
  readOnly,
  onPick,
}: {
  view: SpecView;
  state: SpecStanding;
  grants: AssignmentDto[];
  admin: boolean;
  readOnly: boolean;
  onPick: (move: SpecTransition) => void;
}) {
  const moves = transitions(state);
  const word = SPEC_STATE_LABEL[view.state];

  // Nothing to offer: a read-only replica, or a conflict the engine will refuse
  // every transition against. The state still reads — it is a fact about the
  // document, not a property of being able to change it.
  if (readOnly || moves.length === 0) {
    return <span className="text-mute text-xs">{word}</span>;
  }

  return (
    <DropdownMenu
      alignment="end"
      // The chevron is Astryx's now — it drew one for every menu trigger, and a
      // second hand-placed one beside it read as two affordances.
      // `label` is both the visible text and the accessible name in Astryx, and
      // those differ here: the pill reads "Draft", the control is "Lifecycle:
      // Draft". `aria-label` carries the longer one.
      button={{
        label: word,
        "aria-label": `Lifecycle: ${word}`,
        variant: "secondary",
        elevation: "low",
        size: "md",
      }}
    >
      {moves.map((move) => {
        const allowed = holds(move.capability, view, grants, admin);
        return (
          <DropdownMenuItem
            key={move.to}
            label={move.label}
            icon={SPEC_STATE_ICON[move.to]}
            isDisabled={!allowed}
            onClick={() => onPick(move)}
            endContent={
              allowed ? undefined : (
                <span className="text-mute text-2xs">Needs {move.capability}</span>
              )
            }
          />
        );
      })}
    </DropdownMenu>
  );
}

/**
 * The document.
 *
 * A title and a body, set as prose. Editing is not a mode: the title takes a
 * caret and the body is a live document, exactly as an issue's are — the two
 * surfaces read the same because they are both documents, and only one of them
 * is about to grow a lifecycle. Its storage language is never a UI concept.
 *
 * What editing *means* here is different, and the difference is the whole model:
 * every commit writes a new immutable revision against the head it was composed
 * on. The engine refuses a write against a stale head rather than merging one,
 * so the reader always sends the revision it is showing.
 */
function SpecReader({
  spaceId,
  project,
  spec,
  members,
  readOnly,
  onOpen,
  onError,
}: {
  spaceId: string;
  /** The project KEY in scope, for the issues a relation can name. */
  project: string | null;
  spec: string;
  members: MemberDto[];
  readOnly: boolean;
  onOpen: (spec: string | null) => void;
  onError: (message: string) => void;
}) {
  const store = useProjectViewerStore();
  const resource = useSpec(spaceId, spec);
  const history = useSpecHistory(spaceId, spec).data ?? [];
  // The whole space, not this project: a relation crosses projects freely, and
  // an incoming edge the reader hid because its author sat elsewhere would be
  // the one case where "what relies on this" quietly lied.
  const everySpec = useProjectSpecs(spaceId, null).data ?? [];
  const references = useSpecReferences(spaceId, null).data ?? [];
  const observations = useSpecObservations(spaceId, null).data ?? [];
  const baselines = useProjectBaselines(spaceId, null).data ?? [];
  const { board } = useProjectBoard(spaceId, project);
  const rows = board?.columns.flatMap((column) => column.rows) ?? [];
  const grants = useGrants(spaceId).data ?? [];
  const admin = members.some((member) => member.me && member.role === "admin");
  const view = resource.data;
  /** A historical revision being read, or `null` for the head. */
  const [viewing, setViewing] = useState<string | null>(null);
  /** Two revisions under comparison, or `null`. */
  const [comparing, setComparing] = useState<{ from: string; to: string } | null>(null);
  const [resolving, setResolving] = useState(false);
  const [upgrading, setUpgrading] = useState(false);
  const [title, setTitle] = useState(view?.title ?? "");
  const titleRef = useRef<HTMLTextAreaElement>(null);
  const body = useRef<string | null>(null);
  const selectedRevision = viewing
    ? history.find((entry) => entry.revision === viewing)
    : undefined;
  const geometryBody = selectedRevision?.body ?? view?.body;
  const geometry = usePlanGeometry(
    spaceId,
    view?.kind === "plan" ? view.project : null,
    geometryBody?.plan?.roots ?? [],
    selectedRevision?.body.generation || null,
  );

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

  const revise = (patch: { title?: string; text?: string; plan?: PlanData | null }) => {
    if (!view) return;
    void store
      .reviseSpec(spaceId, view.spec, view.revision, patch)
      .catch((reason: unknown) => onError(reason instanceof Error ? reason.message : String(reason)));
  };

  const upgradeDocument = () => {
    if (!view || view.body.text.startsWith(DOCUMENT_PREFIX) || upgrading) return;
    setUpgrading(true);
    void store
      .upgradeSpecDocument(
        spaceId,
        view.spec,
        view.revision,
        upgradeMarkdown(view.body.text).source,
      )
      .catch((reason: unknown) => onError(reason instanceof Error ? reason.message : String(reason)))
      .finally(() => setUpgrading(false));
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

  const state = standing(view);
  const past = viewing ? history.find((entry) => entry.revision === viewing) : undefined;
  // A revision that is not the head is a record, not a draft. Editing one would
  // have to mean either rewriting history or silently forking it, and neither is
  // a thing a reader should be able to do by clicking into the past.
  const locked = readOnly || past !== undefined;
  const shown = past?.body ?? view.body;
  const shownDocumentSchema = shown.text.startsWith(DOCUMENT_PREFIX) ? DOCUMENT_SCHEMA : 0;
  const currentDocumentSchema = view.body.text.startsWith(DOCUMENT_PREFIX) ? DOCUMENT_SCHEMA : 0;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <article className="mx-auto flex w-full max-w-[52rem] flex-col gap-6 px-10 py-10">
        <header className="flex flex-col gap-2">
          <div className="flex items-center gap-3">
            {/* Kind, and only kind. It is what the document *is*, chosen once and
                not revisable by typing, so it belongs above the title rather
                than in a row of properties that has nothing else to put in it. */}
            <span className="text-mute text-2xs font-medium tracking-wide uppercase">
              {SPEC_KIND_LABEL[view.kind]}
            </span>
            {/* The lifecycle sits opposite it as a control rather than a badge,
                because for a Spec the transition IS the verb — the same argument
                that keeps Start/Done off an issue and puts them in its status
                picker. It reads the head's state, so it says "Draft" on a fresh
                document: that is the state, and the way to leave it. */}
            <span className="ml-auto flex items-center gap-1">
              <Lifecycle
                view={view}
                state={state}
                grants={grants}
                admin={admin}
                readOnly={locked}
                onPick={(move) => {
                  void (async () => {
                    // The verb, in the title and on the button — a confirmation
                    // that says "Issued" is naming the state it lands in rather
                    // than the thing the button does.
                    const ok = await ask.confirm({
                      title: `${move.label} revision ${short(view.revision)}?`,
                      body: move.describe(short(view.revision)),
                      confirmText: move.label,
                      danger: move.to === "withdrawn",
                    });
                    if (!ok) return;
                    try {
                      await store.setSpecState(spaceId, view.spec, view.revision, move.to);
                    } catch (reason) {
                      onError(reason instanceof Error ? reason.message : String(reason));
                    }
                  })();
                }}
              />
              {!locked && state.conflict === null && currentDocumentSchema === 0 && (
                <DropdownMenu
                  alignment="end"
                  hasChevron={false}
                  menuWidth={208}
                  button={{
                    label: "More spec actions",
                    variant: "ghost",
                    size: "sm",
                    isIconOnly: true,
                    tooltip: "More spec actions",
                    icon: <MoreHorizontal className="size-icon-sm" />,
                  }}
                >
                  <DropdownMenuItem
                    label="Upgrade document"
                    icon={<ArrowUp className="size-icon-sm" />}
                    isDisabled={upgrading}
                    onClick={upgradeDocument}
                  />
                </DropdownMenu>
              )}
            </span>
          </div>
          {/* A textarea, not an input: a long title should wrap rather than
              scroll sideways past the edge of the page. */}
          <textarea
            ref={titleRef}
            value={past ? past.body.title : title}
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
          {/* What this document can do to the work, in words, next to the
              title. A Guide that only *looks* quiet is a Guide someone reads as
              an instruction, and authority that rides on styling does not
              survive monochrome or a screen reader. */}
          <p className="text-dim text-xs">
            {authorityPhrase(view.kind, past ? past.body.state : view.state)}
          </p>
          {view.kind === "plan" && shown.plan && (() => {
            const progress = planCounts(geometry.data);
            return progress.total > 0 ? (
              <p className="text-mute text-xs tabular-nums">
                {progress.closed}/{progress.total} closed
                {progress.ready > 0 && ` · ${progress.ready} ready`}
                {progress.blocked > 0 && ` · ${progress.blocked} blocked`}
                {(progress.cyclic + progress.stalled) > 0
                  && ` · ${progress.cyclic + progress.stalled} structurally unresolved`}
              </p>
            ) : null;
          })()}
          {/* Two simultaneous facts, said as two clauses. Drafting a successor
              does not revoke what was issued, and a page that showed only the
              head would quietly report that the governing truth had gone. */}
          {!past && state.draftAhead && state.issued && (
            <p className="text-dim text-xs">
              Revision <code className="text-mute" title={state.issued}>{short(state.issued)}</code>{" "}
              is issued and governs. This draft has not replaced it.
            </p>
          )}
          <Rail
            history={history}
            view={view}
            viewing={viewing}
            onView={setViewing}
            onCompare={(revision) => setComparing({ from: revision, to: view.revision })}
          />
          {past && (
            <RevisionNote
              revision={past.revision}
              written={past.body.ts}
              onCurrent={() => setViewing(null)}
            />
          )}
        </header>

        {state.conflict && (
          <div
            className="border-warn/30 bg-warn/5 text-dim flex items-start gap-2 rounded-surface border p-3 text-sm"
            role="status"
          >
            <AlertTriangle className="text-warn mt-0.5 size-icon-sm shrink-0" />
            <span className="min-w-0 flex-1">
              {state.conflict === "heads" ? (
                <>
                  This spec has {view.heads.length} concurrent head revisions, so none of them is
                  the current one. Lifecycle actions stay unavailable until a resolution names
                  every head as its predecessor — no revision wins by arriving later.
                </>
              ) : (
                <>
                  {view.issued.length} revisions each claim to govern. Nothing here is the
                  effective issued truth until that is resolved.
                </>
              )}
              <span className="text-mute mt-1 block font-mono text-2xs">
                {(state.conflict === "heads" ? view.heads : view.issued)
                  .map((head) => short(head))
                  .join("  ·  ")}
              </span>
              {state.conflict === "heads" && !readOnly && (
                <span className="mt-2 flex gap-2">
                  {view.heads.length === 2 && (
                    <Button
                      onClick={() =>
                        setComparing({ from: view.heads[0]!, to: view.heads[1]! })
                      }
                      label="Compare heads"
                      variant="secondary"
                      elevation="low"
                      size="md"
                    />
                  )}
                  <Button
                    onClick={() => setResolving(true)}
                    label="Resolve…"
                    variant="primary"
                    size="md"
                  />
                </span>
              )}
            </span>
          </div>
        )}

        {resolving && state.conflict === "heads" && (
          <Resolve
            heads={history.filter((entry) => view.heads.includes(entry.revision))}
            onCancel={() => setResolving(false)}
            onCommit={(body) => {
              setResolving(false);
              void store
                .resolveSpec(spaceId, view.spec, view.heads, body)
                .catch((reason: unknown) =>
                  onError(reason instanceof Error ? reason.message : String(reason)),
                );
            }}
          />
        )}

        {comparing && (() => {
          const from = history.find((entry) => entry.revision === comparing.from);
          const to = history.find((entry) => entry.revision === comparing.to);
          return from && to ? (
            <Compare history={history} from={from} to={to} onClose={() => setComparing(null)} />
          ) : null;
        })()}
        {locked ? (
          <div className="min-h-ctl-xl">
            {shown.text ? (
              shownDocumentSchema ? (
                <Document source={shown.text} />
              ) : (
                <Markdown text={shown.text} />
              )
            ) : (
              !shown.plan && <span className="text-mute">No content</span>
            )}
            {view.kind === "plan" && shown.plan && (
              <>
                {past && !shown.generation && (
                  <p className="text-mute mt-4 text-xs">
                    This revision predates generation coordinates; its Issue morphology is live.
                  </p>
                )}
                <PlanSurface
                  plan={shown.plan}
                  rows={rows}
                  geometry={geometry.data}
                  readOnly
                  historical={Boolean(past && shown.generation)}
                  onSave={() => undefined}
                />
              </>
            )}
          </div>
        ) : (
          <>
            <MarkdownEditor
              // Remount on a new revision so the editor reloads the committed
              // document; it reads `value` at mount and owns it from there.
              key={view.revision}
              value={view.body.text}
              documentSchema={currentDocumentSchema}
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
            {view.kind === "plan" && view.body.plan && (
              <PlanSurface
                plan={view.body.plan}
                rows={rows}
                geometry={geometry.data}
                readOnly={false}
                onSave={(plan) => revise({ plan })}
              />
            )}
          </>
        )}

        {/* After the document, not beside it: a relation is something you look
            up once you have read the thing, and a rail of them next to the prose
            competes with the prose for the reader. */}
        {!past && (
          <Relations
            view={view}
            everySpec={everySpec}
            baselines={baselines}
            rows={rows}
            references={references}
            // A conflicted document refuses every write anyway, so offering the
            // editor there would be a composer that exists to be refused.
            readOnly={locked || state.conflict !== null}
            onOpen={onOpen}
            onCommit={(delta) => {
              void store
                .relateSpec(spaceId, view.spec, view.revision, delta)
                .catch((reason: unknown) =>
                  onError(reason instanceof Error ? reason.message : String(reason)),
                );
            }}
          />
        )}

        {/* After the relations, because a note is a comment on the graph and the
            graph has to be on screen first. Available on a conflicted document
            — unlike every revision-writing control, a note competes for no head
            and is often exactly what somebody wants to leave there. */}
        {!past && (
          <Observations
            view={view}
            observations={observations}
            members={members}
            everySpec={everySpec}
            baselines={baselines}
            rows={rows}
            readOnly={locked}
            grants={grants}
            admin={admin}
            onOpen={onOpen}
            onObserve={(link, note) => {
              void store
                .observeSpec(spaceId, view.spec, link.rel, link.target, note)
                .catch((reason: unknown) =>
                  onError(reason instanceof Error ? reason.message : String(reason)),
                );
            }}
            onRetract={(entry) => {
              void store
                .retractObservation(spaceId, entry.spec, entry.observation)
                .catch((reason: unknown) =>
                  onError(reason instanceof Error ? reason.message : String(reason)),
                );
            }}
          />
        )}
      </article>
    </div>
  );
}
