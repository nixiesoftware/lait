import { useEffect, useState } from "react";
import { AlertTriangle, ArrowRight, ShieldCheck, X } from "lucide-react";

import { rpc } from "../api";
import type { RoleProjection, StatusCategory } from "../types";
import { catalogColor } from "./colors";
import { StatusIcon } from "./icons";
import { Dialog, IconButton } from "@astryxdesign/core";

/**
 * The governance viewers — a project's workflow and the space's roles, read-only.
 *
 * These answer the question the errors couldn't: a gated transition refuses with
 * "that change conflicts…" or a demand failure, and until now the browser had no
 * way to see *what the rule was*. `role_list` is an exact-publication page;
 * continuation never falls forward to a newer policy generation.
 *
 * Read-only on purpose. Editing a workflow or a role is a CAS ceremony
 * (`expect_heads` / `expect_revision`) whose conflict flow deserves its own
 * design pass; a half-built editor over signed policy would be worse than the
 * CLI it papers over.
 */

// ---- the wire shapes (defensively partial — parsed from pretty JSON) --------

interface WorkflowStateWire {
  state_id: string;
  name: string;
  category: string;
  color: string;
}

interface WorkflowTransitionWire {
  transition_id: string;
  source_state_ids: string[];
  destination_state_id: string;
  demand_template: DemandWire;
}

type DemandWire =
  | { op: "require"; capability: string; resource: { kind: string } }
  | { op: "all"; children: DemandWire[] }
  | { op: "any"; children: DemandWire[] };

interface WorkflowShowWire {
  project_id: string;
  revision: {
    revision_id: string;
    body: { name: string; states: WorkflowStateWire[]; transitions: WorkflowTransitionWire[] };
  } | null;
  conflict_heads: string[];
}

interface RoleWire {
  role_id: string;
  built_in: boolean;
  revision: {
    revision_id: string;
    body: {
      name: string;
      description: string;
      scope_kind: string;
      capabilities: string[];
    };
  } | null;
  conflict_heads: string[];
}

function roleFromProjection({ summary, revision }: RoleProjection): RoleWire {
  return { ...summary, revision: revision ?? null };
}

/** One sentence for a demand template: what the gate asks of the actor. */
function demandPhrase(d: DemandWire): string {
  switch (d.op) {
    case "require":
      return `${d.capability} @ ${d.resource.kind}`;
    case "all":
      return d.children.map(demandPhrase).join(" AND ");
    case "any":
      return d.children.map(demandPhrase).join(" OR ");
  }
}

function Shell({
  title,
  onClose,
  footer,
  children,
}: {
  title: string;
  onClose: () => void;
  /** Pinned below the scroll, like every other dialog's footer. What it holds
   *  — which revision this is and where to change it — is the one line you
   *  need *while* reading the policy, and inside the scroll it was only
   *  visible after you had read all of it. */
  footer?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <Dialog
      isOpen
      onOpenChange={(o) => !o && onClose()}
      width={560}
      purpose="form"
      aria-labelledby="governance-heading"
    >
      <header className="border-line flex shrink-0 items-center gap-2 border-b px-4 py-3">
        <h2 id="governance-heading" className="font-semibold">{title}</h2>
        {/* It had no `onClick` at all — the third dialog in this pass with a
            close button that did nothing. Escape still worked, which is why it
            went unnoticed. */}
        <IconButton
            label="Close"
            className="ml-auto"
            onClick={onClose}
            variant="ghost"
            size="sm"
            tooltip="Close  Esc"
            icon={<X className="size-icon-md" />}
          />
      </header>
      <div className="flex min-h-0 flex-col gap-4 overflow-y-auto p-4">{children}</div>
      {footer && (
        <footer className="border-line text-mute shrink-0 border-t px-4 py-3 text-xs">
          {footer}
        </footer>
      )}
    </Dialog>
  );
}

/** Multiple heads = concurrent edits nobody has resolved; edits are blocked
 *  until somebody picks one. `fix` names the surface that can, because a note
 *  naming no surface is a dead end. */
function ConflictNote({ heads, fix }: { heads: string[]; fix: string }) {
  if (heads.length === 0) return null;
  return (
    <p className="text-warn border-warn/40 flex items-start gap-2 rounded-surface border p-2 text-sm">
      <AlertTriangle className="mt-0.5 size-icon-sm shrink-0" />
      <span>
        {heads.length} concurrent revisions are unresolved — ordinary edits are blocked until an
        admin resolves them from {fix}.
      </span>
    </p>
  );
}

export function WorkflowDialog({
  spaceId,
  projectKey,
  onClose,
}: {
  spaceId: string;
  /** The board's project — the workflow shown is this project's. */
  projectKey: string;
  onClose: () => void;
}) {
  const [wf, setWf] = useState<WorkflowShowWire | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void rpc(spaceId, { cmd: "workflow_show", project: projectKey })
      .then((r) => {
        if (!alive) return;
        if (r.kind === "text") setWf(JSON.parse(r.text) as WorkflowShowWire);
      })
      .catch((e) => {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      alive = false;
    };
  }, [spaceId, projectKey]);

  const nameOf = (id: string) =>
    wf?.revision?.body.states.find((s) => s.state_id === id)?.name ?? id;

  return (
    <Shell
      title={`Workflow — ${projectKey}`}
      onClose={onClose}
      footer={
        wf?.revision ? (
          <>
            Revision <code className="font-mono">{wf.revision.revision_id.slice(0, 12)}…</code> —
            editable from Settings → Workflow.
          </>
        ) : null
      }
    >
      {error && <p className="text-danger text-sm">{error}</p>}
      {!wf && !error && <p className="text-mute text-sm">Loading…</p>}
      {wf && (
        <>
          <ConflictNote
            heads={wf.conflict_heads}
            fix="Settings → Workflow"
          />
          {wf.revision && (
            <>
              <section>
                <h3 className="text-mute mb-2 text-2xs font-semibold tracking-wider uppercase">
                  States
                </h3>
                <ul className="flex flex-col gap-1">
                  {wf.revision.body.states.map((s) => (
                    <li key={s.state_id} className="flex items-center gap-2 text-sm">
                      <StatusIcon
                        category={s.category as StatusCategory}
                        color={catalogColor(s.color)}
                      />
                      <span>{s.name}</span>
                      {s.name.trim().toLowerCase() !== s.category.replaceAll("_", " ") && (
                        <span className="text-mute text-2xs capitalize">
                          {s.category.replaceAll("_", " ")}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              </section>
              <section>
                <h3 className="text-mute mb-2 text-2xs font-semibold tracking-wider uppercase">
                  Transitions & gates
                </h3>
                <ul className="flex flex-col gap-1.5">
                  {wf.revision.body.transitions.map((t) => (
                    <li key={t.transition_id} className="text-sm">
                      <span className="flex items-center gap-1.5">
                        <span>{t.source_state_ids.map(nameOf).join(", ")}</span>
                        <ArrowRight className="text-mute size-icon-xs shrink-0" />
                        <span>{nameOf(t.destination_state_id)}</span>
                      </span>
                      <span className="text-mute font-mono text-2xs">
                        requires {demandPhrase(t.demand_template)}
                      </span>
                    </li>
                  ))}
                </ul>
              </section>
            </>
          )}
        </>
      )}
    </Shell>
  );
}

export function RolesDialog({ spaceId, onClose }: { spaceId: string; onClose: () => void }) {
  const [roles, setRoles] = useState<RoleWire[] | null>(null);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void rpc(spaceId, { cmd: "role_list", page: { limit: 100, cursor: null } })
      .then((r) => {
        if (!alive) return;
        if (r.kind === "roles") {
          setRoles(r.page.items.map(roleFromProjection));
          setNextCursor(r.page.next_cursor ?? null);
        }
      })
      .catch((e) => {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      alive = false;
    };
  }, [spaceId]);

  const loadMore = async () => {
    if (!nextCursor || loadingMore) return;
    setLoadingMore(true);
    try {
      const r = await rpc(spaceId, {
        cmd: "role_list",
        page: { limit: 100, cursor: nextCursor },
      });
      if (r.kind === "roles") {
        setRoles((current) => [
          ...(current ?? []),
          ...r.page.items.map(roleFromProjection).filter(
            (candidate) => !(current ?? []).some((role) => role.role_id === candidate.role_id),
          ),
        ]);
        setNextCursor(r.page.next_cursor ?? null);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingMore(false);
    }
  };

  return (
    <Shell title="Roles" onClose={onClose}>
      {error && <p className="text-danger text-sm">{error}</p>}
      {!roles && !error && <p className="text-mute text-sm">Loading…</p>}
      {roles?.map((role) => (
        <section key={role.role_id} className="border-line rounded-surface border p-3">
          <div className="flex items-center gap-2">
            <span className="font-medium">{role.revision?.body.name ?? role.role_id}</span>
            {role.built_in && (
              <span className="text-accent flex items-center gap-1 text-2xs" title="Immutable">
                <ShieldCheck className="size-icon-xs" />
                built-in
              </span>
            )}
            <span className="text-mute text-2xs capitalize">
              {role.revision?.body.scope_kind ?? ""}
            </span>
          </div>
          {role.revision?.body.description && (
            <p className="text-dim mt-1 text-sm">{role.revision.body.description}</p>
          )}
          <ConflictNote heads={role.conflict_heads} fix="the issues_role_resolve tool" />
          <ul className="mt-2 flex flex-wrap gap-1">
            {(role.revision?.body.capabilities ?? []).map((c) => (
              <li
                key={c}
                className="border-line-strong text-dim rounded-full border px-2 py-px font-mono text-2xs"
              >
                {c}
              </li>
            ))}
          </ul>
        </section>
      ))}
      {roles && (
        <div className="flex items-center justify-between gap-3">
          <p className="text-mute text-xs">
            Custom roles are authored with the <code className="font-mono">issues_role_create</code>{" "}
            and <code className="font-mono">issues_role_edit</code> tools.
          </p>
          {nextCursor && (
            <button
              type="button"
              className="border-line rounded-surface border px-3 py-1.5 text-xs"
              disabled={loadingMore}
              onClick={() => void loadMore()}
            >
              {loadingMore ? "Loading…" : "Load more"}
            </button>
          )}
        </div>
      )}
    </Shell>
  );
}
