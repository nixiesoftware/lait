import { useEffect, useMemo, useState } from "react";
import { Button, Popover } from "@astryxdesign/core";

import { rpc } from "../../api";
import type { ProjectDto, StatusCategory } from "../../types";
import { catalogColor } from "../colors";
import { ColorPicker } from "../ColorPicker";
import { ProjectIcon, StatusIcon } from "../icons";
import { Combobox } from "../Picker";
import { cn } from "../primitives";
import { EmptyState } from "../AppState";
import { SettingsPageHeader, SettingsSection } from "../settingsLayout";

interface StateWire {
  state_id: string;
  name: string;
  category: string;
  color: string;
}
interface WorkflowWire {
  project_id: string;
  revision: {
    revision_id: string;
    body: { name: string; states: StateWire[]; transitions: unknown[] };
  } | null;
  conflict_heads: string[];
}

/** The categories in lifecycle order, with the heading each group carries. */
const CATEGORIES: readonly {
  id: StatusCategory;
  label: string;
  hint: string;
}[] = [
  { id: "backlog", label: "Backlog", hint: "Not started" },
  { id: "active", label: "Active", hint: "Started" },
  { id: "done", label: "Done", hint: "Completed or closed" },
];

/**
 * Workflow — the status columns of a project, grouped the way issues move.
 *
 * Linear draws its statuses under category bands (Backlog, Unstarted, Started,
 * Completed, Canceled) and that is the reading order that matters: a person
 * scanning for "where does In Review sit" wants the lifecycle, not the list
 * order. The engine's categories are three, so there are three bands.
 *
 * Editing is the *display* of each state — name and colour — re-submitted with
 * the same `state_id`s and transitions, so referential integrity is preserved
 * for free. Adding or removing a state rewrites transitions, which is a
 * different ceremony and is deliberately not on this page.
 */
export function WorkflowPanel({
  spaceId,
  projects,
  readOnly,
  revision,
  onError,
}: {
  spaceId: string;
  projects: ProjectDto[];
  readOnly: boolean;
  revision: number;
  onError: (message: string) => void;
}) {
  const [projectKey, setProjectKey] = useState<string | null>(projects[0]?.key ?? null);
  // Projects arrive after a direct load of this page, so the first one is
  // chosen when it appears and not only when the panel mounted.
  useEffect(() => {
    if (projectKey === null && projects[0]) setProjectKey(projects[0].key);
  }, [projects, projectKey]);
  const [wf, setWf] = useState<WorkflowWire | null>(null);
  const [draft, setDraft] = useState<StateWire[]>([]);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!projectKey) return;
    let alive = true;
    setWf(null);
    void rpc(spaceId, { cmd: "workflow_show", project: projectKey })
      .then((r) => {
        if (!alive) return;
        if (r.kind === "text") {
          const parsed = JSON.parse(r.text) as WorkflowWire;
          setWf(parsed);
          setDraft(parsed.revision?.body.states.map((s) => ({ ...s })) ?? []);
        }
      })
      .catch((e) => {
        if (alive) onError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      alive = false;
    };
  }, [spaceId, projectKey, revision, onError]);

  const dirty = useMemo(() => {
    const original = wf?.revision?.body.states ?? [];
    return draft.some((s, i) => s.name !== original[i]?.name || s.color !== original[i]?.color);
  }, [draft, wf]);

  const save = async () => {
    if (!wf?.revision || !projectKey) return;
    setSaving(true);
    try {
      const body = { ...wf.revision.body, states: draft };
      const heads = [wf.revision.revision_id, ...wf.conflict_heads];
      await rpc(spaceId, {
        cmd: "workflow_set",
        project: projectKey,
        expect_heads: heads,
        body_json: JSON.stringify(body),
      });
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const patch = (id: string, change: Partial<StateWire>) =>
    setDraft((d) => d.map((s) => (s.state_id === id ? { ...s, ...change } : s)));
  const selectedProject = projects.find((p) => p.key === projectKey);

  /** States under their band, in the order the body lists them. A category
   *  the body never uses still draws its band, empty — the absence is the
   *  fact worth seeing ("this project has no done state"). */
  const grouped = useMemo(
    () =>
      CATEGORIES.map((category) => ({
        ...category,
        states: draft.filter((s) => s.category === category.id),
      })),
    [draft],
  );
  const stray = draft.filter((s) => !CATEGORIES.some((c) => c.id === s.category));

  return (
    <>
      <SettingsPageHeader
        title="Workflow"
        description="The statuses issues move through, from backlog to done. Each project has its own."
        actions={
          projects.length > 0 ? (
            <Combobox
              label="Project"
              value={
                projectKey
                  ? {
                      id: projectKey,
                      label: selectedProject?.name ?? projectKey,
                      ...(selectedProject
                        ? {
                            icon: <ProjectIcon color={catalogColor(selectedProject.color)} />,
                          }
                        : {}),
                    }
                  : null
              }
              placeholder="Select a project…"
              options={projects.map((p) => ({
                id: p.key,
                label: p.name,
                icon: <ProjectIcon color={catalogColor(p.color)} />,
                hint: p.key,
              }))}
              onPick={setProjectKey}
              size="md"
            />
          ) : undefined
        }
      />

      {projects.length === 0 && (
        <EmptyState
          art="projects"
          title="Workflow"
          body="A workflow belongs to a project: the statuses its issues move through. Create a project and its workflow appears here."
        />
      )}
      {!wf && projectKey && <p className="text-mute text-sm">Loading…</p>}
      {wf && !wf.revision && (
        <p className="text-warn text-sm">
          This project has unresolved concurrent workflow revisions.
        </p>
      )}

      {wf?.revision && (
        <SettingsSection
          title="Statuses"
          hint="Click a status to rename it, or its icon to recolour it. Changes are held until you save."
        >
          <div className="border-line overflow-hidden rounded-surface border">
            {grouped.map((group) => (
              <div key={group.id}>
                <div className="bg-sunken text-mute flex items-center gap-2 px-3 py-1.5 text-2xs">
                  <span className="font-semibold tracking-wider uppercase">{group.label}</span>
                  <span>· {group.hint}</span>
                  <span className="ml-auto tabular-nums">
                    {group.states.length} {group.states.length === 1 ? "status" : "statuses"}
                  </span>
                </div>
                {group.states.length === 0 ? (
                  <p className="text-mute px-3 py-2 text-xs italic">No status in this category.</p>
                ) : (
                  <ul className="divide-line divide-y">
                    {group.states.map((s) => (
                      <StateRow
                        key={s.state_id}
                        state={s}
                        category={group.id}
                        readOnly={readOnly}
                        onPatch={(change) => patch(s.state_id, change)}
                      />
                    ))}
                  </ul>
                )}
              </div>
            ))}
            {stray.length > 0 && (
              <div>
                <div className="bg-sunken text-mute px-3 py-1.5 text-2xs font-semibold tracking-wider uppercase">
                  Other
                </div>
                <ul className="divide-line divide-y">
                  {stray.map((s) => (
                    <StateRow
                      key={s.state_id}
                      state={s}
                      category="backlog"
                      readOnly={readOnly}
                      onPatch={(change) => patch(s.state_id, change)}
                    />
                  ))}
                </ul>
              </div>
            )}
          </div>

          {!readOnly && (
            <div className="mt-4 flex items-center justify-between gap-4">
              <p className="text-mute text-xs">
                Adding or removing a status rewrites transitions and is not offered here yet.
              </p>
              <div className="flex shrink-0 gap-2">
                <Button
                  isDisabled={!dirty}
                  onClick={() => setDraft(wf.revision!.body.states.map((s) => ({ ...s })))}
                  label="Reset"
                  variant="secondary"
                  size="sm"
                />
                <Button
                  isDisabled={!dirty}
                  isLoading={saving}
                  onClick={() => void save()}
                  label="Save workflow"
                  variant="primary"
                  size="sm"
                />
              </div>
            </div>
          )}
        </SettingsSection>
      )}
    </>
  );
}

function StateRow({
  state,
  category,
  readOnly,
  onPatch,
}: {
  state: StateWire;
  category: StatusCategory;
  readOnly: boolean;
  onPatch: (change: Partial<StateWire>) => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <li className="hover:bg-hover flex min-h-ctl-lg items-center gap-2 px-3 py-1">
      <Popover
        isOpen={open}
        onOpenChange={setOpen}
        alignment="start"
        width={224}
        content={
          <div className="p-3">
            <ColorPicker
              value={state.color}
              onChange={(color) => {
                setOpen(false);
                onPatch({ color });
              }}
            />
          </div>
        }
      >
        <button
          type="button"
          disabled={readOnly}
          aria-label={`Colour of ${state.name}`}
          title={readOnly ? undefined : "Change colour"}
          className="hover:ring-line-strong flex size-ctl-sm items-center justify-center rounded-control hover:ring-1 disabled:cursor-default disabled:hover:ring-0"
        >
          <StatusIcon category={category} color={catalogColor(state.color)} />
        </button>
      </Popover>
      <input
        value={state.name}
        disabled={readOnly}
        onChange={(e) => onPatch({ name: e.target.value })}
        aria-label={`Name of ${state.name}`}
        className={cn(
          "hover:border-line-strong focus:border-line-strong h-ctl-sm min-w-0 flex-1 rounded-control border border-transparent bg-transparent px-2 text-sm font-medium outline-none",
          "disabled:hover:border-transparent",
        )}
      />
      <code className="text-mute shrink-0 font-mono text-2xs" title={state.state_id}>
        {state.state_id}
      </code>
    </li>
  );
}
