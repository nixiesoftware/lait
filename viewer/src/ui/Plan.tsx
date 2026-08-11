import { useEffect, useMemo, useState } from "react";
import { Button, IconButton } from "@astryxdesign/core";
import { PencilLine, X } from "lucide-react";

import type {
  GeometryEdge,
  GeometryNode,
  GeometryView,
  PlanData,
  Row,
  SpecState,
} from "../types";
import { parseDocument } from "../core/document";
import { RichDocument } from "./Markdown";
import { Combobox, type Option } from "./Picker";
import { cn } from "./primitives";
import { useRefs } from "./RefChip";

export function planCounts(geometry: GeometryView | null | undefined) {
  return geometry?.closure ?? {
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
    <div className="border-line flex flex-wrap items-center gap-2 border-b pb-3">
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

interface Point { x: number; y: number; node: GeometryNode }

const NODE_LIMIT = 700;

/**
 * Deterministic, bounded layout. Dependency depth provides one field, stable
 * ordinal another; containment bends children toward parents. Nothing is saved
 * and no random or time-dependent simulation can make replicas disagree.
 */
export function layoutGeometry(geometry: GeometryView): {
  points: Map<string, Point>;
  width: number;
  height: number;
  clipped: number;
} {
  const required = new Set([
    ...geometry.roots,
    ...geometry.residuals.flatMap((residual) => residual.at),
  ]);
  const priority = geometry.nodes.filter((node) => required.has(node.row.doc_id));
  const selected = geometry.nodes.length <= NODE_LIMIT
    ? geometry.nodes
    : priority.length >= NODE_LIMIT
      ? priority
      : [
          ...priority,
          ...geometry.nodes
            .filter((node) => !required.has(node.row.doc_id))
            .slice(0, NODE_LIMIT - priority.length),
        ];
  const visible = new Set(selected.map((node) => node.row.doc_id));
  const points = new Map<string, Point>();
  const width = 840;
  let top = 38;

  for (const component of geometry.components) {
    const nodes = selected.filter((node) =>
      node.component === component.id && visible.has(node.row.doc_id));
    if (nodes.length === 0) continue;
    const maxLayer = Math.max(0, ...nodes.map((node) => node.layer ?? 0));
    const columns = new Map<number, GeometryNode[]>();
    for (const node of nodes) {
      const layer = node.layer ?? maxLayer + 1;
      columns.set(layer, [...(columns.get(layer) ?? []), node]);
    }
    const rows = Math.max(...[...columns.values()].map((column) => column.length));
    const bandHeight = Math.max(112, rows * 42 + 42);
    const xStep = Math.min(160, 700 / Math.max(1, maxLayer + 1));
    for (const [layer, column] of [...columns].sort(([a], [b]) => a - b)) {
      column.sort((a, b) => a.ordinal - b.ordinal || a.row.doc_id.localeCompare(b.row.doc_id));
      column.forEach((node, index) => {
        const parent = node.parent ? points.get(node.parent) : undefined;
        const spread = bandHeight / (column.length + 1);
        const naturalY = top + spread * (index + 1);
        const y = parent ? naturalY * 0.72 + parent.y * 0.28 : naturalY;
        points.set(node.row.doc_id, {
          x: 70 + layer * xStep + (node.hierarchy_depth % 2) * 8,
          y,
          node,
        });
      });
    }
    top += bandHeight + 28;
  }
  return { points, width, height: Math.max(160, top), clipped: geometry.nodes.length - selected.length };
}

function curve(edge: GeometryEdge, from: Point, to: Point): string {
  if (edge.role === "containment") {
    const middle = (from.y + to.y) / 2;
    return `M ${from.x} ${from.y} C ${from.x + 24} ${middle}, ${to.x - 24} ${middle}, ${to.x} ${to.y}`;
  }
  const bend = Math.max(24, Math.abs(to.x - from.x) * 0.42);
  return `M ${from.x} ${from.y} C ${from.x + bend} ${from.y}, ${to.x - bend} ${to.y}, ${to.x} ${to.y}`;
}

const closureTone: Record<GeometryNode["closure"], string> = {
  closed: "fill-success",
  ready: "fill-accent",
  blocked: "fill-warn",
  cycle: "fill-danger",
  stalled: "fill-mute",
};

const closureMarkTone: Record<GeometryNode["closure"], string> = {
  closed: "bg-success",
  ready: "bg-accent",
  blocked: "bg-warn",
  cycle: "bg-danger",
  stalled: "bg-mute",
};

const residualLabel: Record<string, string> = {
  root_missing: "Missing root",
  dependency_cycle: "Dependency loop",
  blocked_frontier: "Blocked frontier",
  due_order_conflict: "Due-order conflict",
  unattached: "Unattached issue",
  closure_frontier: "Closure frontier",
};

export function PlanMorphology({
  geometry,
  historical = false,
  onOpenIssue,
}: {
  geometry: GeometryView;
  historical?: boolean | undefined;
  onOpenIssue?: ((reff: string) => void) | undefined;
}) {
  const refs = useRefs();
  const layout = useMemo(() => layoutGeometry(geometry), [geometry]);
  const [selected, setSelected] = useState<string | null>(null);
  const chosen = selected ? layout.points.get(selected)?.node : undefined;
  const roots = new Set(geometry.roots);

  return (
    <div className="pt-4">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h2 className="text-sm font-semibold">Morphology</h2>
        <span className="text-mute text-2xs tabular-nums">
          {geometry.closure.closed}/{geometry.closure.total} closed
          {geometry.closure.ready > 0 && ` · ${geometry.closure.ready} ready`}
          {geometry.closure.blocked > 0 && ` · ${geometry.closure.blocked} blocked`}
          {geometry.closure.cyclic > 0 && ` · ${geometry.closure.cyclic} cyclic`}
          {geometry.closure.stalled > 0 && ` · ${geometry.closure.stalled} stalled`}
        </span>
        <code className="text-mute ml-auto text-2xs" title={geometry.generation}>
          {geometry.generation.slice(0, 8)}
        </code>
        {historical && <span className="text-mute text-2xs">historical generation</span>}
      </div>

      {geometry.nodes.length === 0 ? (
        <p className="text-mute py-8 text-center text-sm">
          No Issue morphology exists at this generation.
        </p>
      ) : (
        <div className="border-line mt-3 overflow-auto rounded-surface border bg-subtle/20">
          <svg
            className="block min-w-full"
            viewBox={`0 0 ${layout.width} ${layout.height}`}
            style={{ height: Math.min(680, Math.max(180, layout.height)), width: layout.width }}
            role="img"
            aria-label={`Plan morphology with ${geometry.nodes.length} issues in ${geometry.components.length} components`}
          >
            {geometry.edges.map((edge) => {
              const from = layout.points.get(edge.from);
              const to = layout.points.get(edge.to);
              if (!from || !to) return null;
              return (
                <path
                  key={`${edge.from}:${edge.relation}:${edge.to}`}
                  d={curve(edge, from, to)}
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={edge.role === "constraint" ? 1.7 : 1}
                  strokeDasharray={edge.role === "association" ? "3 5" : undefined}
                  className={cn(
                    "text-line",
                    edge.role === "containment" && "text-dim",
                    edge.role === "equivalence" && "text-accent",
                  )}
                />
              );
            })}
            {[...layout.points.values()].map(({ x, y, node }) => {
              const root = roots.has(node.row.doc_id);
              const active = selected === node.row.doc_id;
              return (
                <g
                  key={node.row.doc_id}
                  transform={`translate(${x} ${y})`}
                  role="button"
                  tabIndex={0}
                  aria-label={`${node.row.key_alias ?? node.row.reff}: ${node.row.title}; ${node.closure}`}
                  onClick={() => setSelected(node.row.doc_id)}
                  onDoubleClick={() => (onOpenIssue ?? refs?.open)?.(node.row.reff)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") (onOpenIssue ?? refs?.open)?.(node.row.reff);
                  }}
                  className="cursor-pointer outline-none"
                >
                  {root && <circle r="10" className="fill-none stroke-accent" strokeWidth="1.5" />}
                  <circle
                    r={active ? 7 : 5.5}
                    className={cn(closureTone[node.closure], "stroke-surface")}
                    strokeWidth="2"
                  />
                  {geometry.nodes.length <= 90 && (
                    <text x="10" y="4" className="fill-current text-[9px] text-dim">
                      {node.row.key_alias ?? node.row.reff}
                    </text>
                  )}
                </g>
              );
            })}
          </svg>
        </div>
      )}

      {layout.clipped > 0 && (
        <p className="text-mute mt-2 text-2xs">
          Dense overview: {layout.clipped} additional issues remain in the computed closure and counts.
        </p>
      )}

      {chosen && (
        <button
          type="button"
          className="border-line hover:bg-hover mt-3 flex w-full items-start gap-2 rounded-control border px-3 py-2 text-left text-xs"
          onClick={() => (onOpenIssue ?? refs?.open)?.(chosen.row.reff)}
        >
          <span className={cn("mt-1 size-mark-xs shrink-0 rounded-full", closureMarkTone[chosen.closure])} />
          <span className="min-w-0 flex-1">
            <span className="text-mute mr-2 font-mono text-2xs">
              {chosen.row.key_alias ?? chosen.row.reff}
            </span>
            <span className="font-medium">{chosen.row.title}</span>
            <span className="text-mute mt-1 block">
              {chosen.closure}
              {chosen.layer != null && ` · layer ${chosen.layer}`}
              {chosen.slack != null && ` · slack ${chosen.slack}`}
              {chosen.facets.length > 0 && ` · ${chosen.facets.map((facet) => facet.label).join(" · ")}`}
            </span>
          </span>
          <X
            className="text-mute size-icon-xs"
            aria-label="Clear selection"
            onClick={(event) => {
              event.stopPropagation();
              setSelected(null);
            }}
          />
        </button>
      )}

      {geometry.residuals.length > 0 && (
        <section className="border-line mt-4 border-t pt-3">
          <h3 className="text-mute text-2xs font-semibold tracking-wider uppercase">
            Open loci · {geometry.residuals.length}
          </h3>
          <ul className="mt-2 grid gap-1.5 sm:grid-cols-2">
            {geometry.residuals.map((residual, index) => (
              <li key={`${residual.kind}:${residual.at.join(",")}:${index}`} className="text-xs">
                <span className="text-dim">{residualLabel[residual.kind] ?? residual.kind}</span>
                {residual.layer != null && <span className="text-mute"> · layer {residual.layer}</span>}
                <span className="text-mute block truncate font-mono text-2xs" title={residual.at.join(", ")}>
                  {residual.at.join(" · ")}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

export function PlanSurface({
  plan,
  rows,
  geometry,
  readOnly,
  historical,
  onSave,
  onOpenIssue,
}: {
  plan: PlanData;
  rows: readonly Row[];
  geometry?: GeometryView | null | undefined;
  readOnly: boolean;
  historical?: boolean | undefined;
  onSave: (plan: PlanData) => void;
  onOpenIssue?: ((reff: string) => void) | undefined;
}) {
  return (
    <section className="plan-morphology my-5" data-plan-morphology-ui>
      <PlanSeedEditor plan={plan} rows={rows} readOnly={readOnly} onSave={onSave} />
      {geometry ? (
        <PlanMorphology geometry={geometry} historical={historical} onOpenIssue={onOpenIssue} />
      ) : (
        <p className="text-mute py-8 text-center text-sm">Computing morphology…</p>
      )}
    </section>
  );
}

/** A read-only document followed by its Issue-derived morphology. */
export function PlanDocument({
  source,
  plan,
  rows,
  geometry,
  historical,
}: {
  source: string;
  plan: PlanData;
  rows: readonly Row[];
  geometry?: GeometryView | null | undefined;
  historical?: boolean | undefined;
}) {
  const blocks = useMemo(() => parseDocument(source), [source]);
  return (
    <div className="plan-document">
      <RichDocument blocks={blocks} />
      <PlanSurface
        plan={plan}
        rows={rows}
        geometry={geometry}
        readOnly
        historical={historical}
        onSave={() => undefined}
      />
    </div>
  );
}
