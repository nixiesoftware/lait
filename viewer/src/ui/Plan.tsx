import { useEffect, useMemo, useState } from "react";
import { Button, IconButton } from "@astryxdesign/core";
import { PencilLine } from "lucide-react";

import type { GeometryView, PlanData, Row, SpecState } from "../types";
import { Combobox, type Option } from "./Picker";

export function planCounts(geometry: GeometryView | null | undefined) {
  return geometry?.summary?.closure ?? {
    total: 0,
    closed: 0,
    ready: 0,
    blocked: 0,
    cyclic: 0,
    stalled: 0,
  };
}

/** The visible identity shared by every surface that names a Plan Spec. */
export function PlanIdentity({
  title,
  state,
  revision,
  geometry,
  compact = false,
}: {
  title: string;
  state?: SpecState;
  revision?: string;
  geometry?: GeometryView | null;
  compact?: boolean;
}) {
  const progress = planCounts(geometry);
  return (
    <span className="plan-identity min-w-0">
      <span className="text-mute mr-1.5 text-2xs font-medium tracking-wide uppercase">Plan</span>
      <span className="font-medium">{title}</span>
      {!compact && progress.total > 0 && (
        <span className="text-mute ml-2 text-2xs tabular-nums">
          {progress.closed}/{progress.total} closed
        </span>
      )}
      {!compact && state && state !== "draft" && (
        <span className="text-mute ml-2 text-2xs capitalize">{state}</span>
      )}
      {revision && <code className="text-mute ml-2 text-2xs">{revision}</code>}
    </span>
  );
}

function issueOption(row: Row): Option {
  return {
    id: row.doc_id,
    label: row.title,
    kicker: row.key_alias ?? row.reff,
    keywords: [row.reff, row.doc_id, ...(row.key_alias ? [row.key_alias] : [])],
  };
}

export function PlanSeedEditor({
  plan,
  rows,
  readOnly,
  onSave,
}: {
  plan: PlanData;
  rows: readonly Row[];
  readOnly: boolean;
  onSave: (plan: PlanData) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [roots, setRoots] = useState<string[]>(plan.roots);
  useEffect(() => {
    setRoots(plan.roots);
    setEditing(false);
  }, [plan]);
  const options = useMemo(
    () => rows.filter((row) => !row.tombstone).map(issueOption),
    [rows],
  );
  const rowByDoc = useMemo(() => new Map(rows.map((row) => [row.doc_id, row])), [rows]);

  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="text-mute text-2xs font-semibold tracking-wider uppercase">Roots</span>
      {!editing ? (
        <>
          {plan.roots.length === 0 ? (
            <span className="text-dim text-xs">Whole project</span>
          ) : plan.roots.map((root) => {
            const row = rowByDoc.get(root);
            return (
              <span key={root} className="bg-subtle rounded-control px-2 py-1 text-xs">
                <span className="text-mute mr-1 font-mono text-2xs">
                  {row?.key_alias ?? row?.reff ?? root.slice(0, 10)}
                </span>
                {row?.title ?? "Unavailable issue"}
              </span>
            );
          })}
          {!readOnly && (
            <IconButton
              className="ml-auto"
              label="Edit plan roots"
              tooltip="Edit roots"
              variant="ghost"
              size="sm"
              icon={<PencilLine className="size-icon-xs" />}
              onClick={() => setEditing(true)}
            />
          )}
        </>
      ) : (
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
          <Combobox
            multi
            wide
            label="Plan roots"
            face={<span>{roots.length === 0 ? "Whole project" : `${roots.length} roots`}</span>}
            options={options}
            selected={roots}
            onToggle={(root) => setRoots((current) =>
              current.includes(root)
                ? current.filter((candidate) => candidate !== root)
                : [...current, root].sort())}
          />
          <Button
            className="ml-auto"
            label="Cancel"
            variant="secondary"
            elevation="low"
            size="sm"
            onClick={() => {
              setRoots(plan.roots);
              setEditing(false);
            }}
          />
          <Button
            label="Save roots"
            variant="primary"
            size="sm"
            onClick={() => {
              onSave({ roots: [...new Set(roots)].sort() });
              setEditing(false);
            }}
          />
        </div>
      )}
    </div>
  );
}

/**
 * A Plan's surface is its seed and nothing else.
 *
 * Membership, order, blocking and completion are Issue facts, and the Issue
 * surfaces already draw them properly. Restating them here as a second picture
 * asked the reader to decode a diagram to learn something a board says plainly.
 * The one derived fact worth carrying is the closure summary, and that sits in
 * the document header where the title and authority already are.
 *
 * The rule above the roots divides the prose from the plan's metadata; it used
 * to sit below, dividing the seed from a drawing that is no longer here.
 */
export function PlanSurface({
  plan,
  rows,
  readOnly,
  onSave,
}: {
  plan: PlanData;
  rows: readonly Row[];
  readOnly: boolean;
  onSave: (plan: PlanData) => void;
}) {
  return (
    <section className="plan-seed border-line mt-5 border-t pt-4" data-plan-seed-ui>
      <PlanSeedEditor plan={plan} rows={rows} readOnly={readOnly} onSave={onSave} />
    </section>
  );
}
