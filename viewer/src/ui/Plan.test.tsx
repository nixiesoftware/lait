import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GeometryView, PlanData, Row } from "../types";
import { PlanSeedEditor, PlanSurface, planCounts } from "./Plan";

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

const CLOSURE: GeometryView["closure"] = {
  total: 3,
  closed: 1,
  ready: 2,
  blocked: 0,
  cyclic: 0,
  stalled: 0,
};

describe("Plan surface", () => {
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

  it("reads closure from the compiled geometry and zero from nothing", () => {
    expect(planCounts({ closure: CLOSURE } as GeometryView)).toEqual(CLOSURE);
    expect(planCounts(null).total).toBe(0);
  });

  // The seed is the whole surface: a Plan names roots, and the Issue surfaces
  // own membership, order and completion. A regression here would be a second
  // picture of the work growing back under the document.
  it("names its roots and draws nothing else", () => {
    act(() => root.render(
      <PlanSurface
        plan={{ roots: ["one"] }}
        rows={[row("one"), row("two")]}
        readOnly
        onSave={() => undefined}
      />,
    ));
    const text = host.textContent ?? "";
    expect(text).toContain("ONE");
    expect(text).toContain("Issue one");
    expect(text).not.toContain("Morphology");
    expect(text).not.toContain("Open loci");
    expect(host.querySelector("svg")).toBeNull();
  });

  it("says so when the seed is the whole project", () => {
    act(() => root.render(
      <PlanSurface plan={{ roots: [] }} rows={[row("one")]} readOnly onSave={() => undefined} />,
    ));
    expect(host.textContent).toContain("Whole project");
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
