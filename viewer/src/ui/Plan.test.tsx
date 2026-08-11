import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { upgradeMarkdown } from "../core/document";
import type { GeometryNode, GeometryView, PlanData, Row } from "../types";
import { PlanDocument, PlanSeedEditor, layoutGeometry, planCounts } from "./Plan";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

function row(reff: string): Row {
  return {
    reff,
    doc_id: reff,
    project_id: "prj_plan",
    key_alias: reff.toUpperCase(),
    title: `Issue ${reff}`,
    status: "active",
    priority: "none",
    assignee_summary: "",
    assignees: [],
    tombstone: false,
    provisional: false,
  };
}

function node(reff: string, layer: number, ordinal: number): GeometryNode {
  return {
    row: row(reff),
    component: "component-1",
    layer,
    ordinal,
    hierarchy_depth: 0,
    children: [],
    blocked_by: [],
    blocks: [],
    closure: reff === "one" ? "closed" : "ready",
    facets: [],
  };
}

const GEOMETRY: GeometryView = {
  schema_version: 1,
  generation: "ab".repeat(32),
  project: "prj_plan",
  roots: ["one"],
  nodes: [node("one", 0, 0), node("two", 1, 1), node("three", 1, 2)],
  edges: [
    { from: "one", relation: "blocks", role: "constraint", to: "two" },
    { from: "one", relation: "blocks", role: "constraint", to: "three" },
  ],
  components: [{
    id: "component-1",
    nodes: ["one", "two", "three"],
    roots: ["one"],
    terminals: ["two", "three"],
    loops: [],
  }],
  residuals: [{ kind: "closure_frontier", component: "component-1", layer: 1, at: ["two", "three"], requires: [] }],
  closure: { total: 3, closed: 1, ready: 2, blocked: 0, cyclic: 0, stalled: 0 },
};

describe("Plan morphology", () => {
  let host: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });

  afterEach(() => {
    act(() => root.unmount());
    host.remove();
  });

  it("uses compiled closure and lays the same topology out identically", () => {
    expect(planCounts(GEOMETRY)).toEqual(GEOMETRY.closure);
    const first = layoutGeometry(GEOMETRY);
    const second = layoutGeometry(GEOMETRY);
    expect([...first.points].map(([id, point]) => [id, point.x, point.y]))
      .toEqual([...second.points].map(([id, point]) => [id, point.x, point.y]));
    expect(first.points.get("two")?.x).toBeGreaterThan(first.points.get("one")?.x ?? 0);
  });

  it("keeps prose ordinary and projects morphology after the document", () => {
    const source = upgradeMarkdown("Before.\n\nAfter.").source;
    act(() => root.render(
      <PlanDocument source={source} plan={{ roots: ["one"] }} rows={GEOMETRY.nodes.map((item) => item.row)} geometry={GEOMETRY} />,
    ));
    const text = host.textContent ?? "";
    expect(text.indexOf("Before.")).toBeLessThan(text.indexOf("After."));
    expect(text.indexOf("After.")).toBeLessThan(text.indexOf("Morphology"));
    expect(text).toContain("Closure frontier");
  });

  it("writes only a canonical root set", () => {
    const onSave = vi.fn();
    const plan: PlanData = { roots: ["one"] };
    act(() => root.render(
      <PlanSeedEditor plan={plan} rows={[row("one"), row("two")]} readOnly={false} onSave={onSave} />,
    ));
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="Edit plan roots"]')!.click());
    const save = [...host.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.trim() === "Save roots");
    act(() => save!.click());
    expect(onSave).toHaveBeenCalledWith({ roots: ["one"] });
  });
});
