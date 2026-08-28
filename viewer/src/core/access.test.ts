import { describe, expect, it } from "vitest";

import type { AssignmentDto, AssignmentOrigin } from "../types";
import { extrasOf, foldAccess } from "./access";

function row(
  actor: string,
  capability: string,
  origin: AssignmentOrigin | undefined,
  extra: Partial<AssignmentDto> = {},
): AssignmentDto {
  return {
    grant_id: `${actor}:${capability}:${extra.resource?.[0] ?? ""}:${origin?.kind ?? "none"}`,
    actor,
    world: "com.lait.issues",
    capability,
    resource: [],
    ...(origin ? { origin } : {}),
    ...extra,
  };
}

describe("foldAccess", () => {
  it("folds everything that came with membership into one line that is not a grant", () => {
    const [you] = foldAccess([
      row("me", "space.admin", { kind: "founder" }),
      row("me", "space.contributor", { kind: "founder" }),
      row("me", "space.issue.read", { kind: "founder" }),
    ]);
    expect(you?.membership?.kinds).toEqual(["founder"]);
    expect(you?.membership?.roles).toEqual([]);
    expect(you?.membership?.capabilities.map((c) => c.capability)).toEqual([
      "space.admin",
      "space.contributor",
      "space.issue.read",
    ]);
    expect(you?.grants).toEqual([]);
    expect(you && extrasOf(you)).toBe(0);
  });

  it("keeps the same capability granted twice as one held thing with both ids", () => {
    // Seeding a Space can install one capability under two grant ids. Two rows
    // saying `space.admin` teach nothing the first did not — but revoking the
    // capability has to revoke both, so the ids are kept.
    const [you] = foldAccess([
      row("me", "space.admin", { kind: "founder" }, { grant_id: "g1" }),
      row("me", "space.admin", { kind: "admission", role: "lait.administrator" }, { grant_id: "g2" }),
    ]);
    expect(you?.membership?.capabilities).toEqual([
      { capability: "space.admin", world: "com.lait.issues", scope: "", grantIds: ["g1", "g2"] },
    ]);
    expect(you?.membership?.kinds).toEqual(["founder", "admission"]);
    expect(you?.membership?.roles).toEqual(["lait.administrator"]);
  });

  it("groups a role grant's expansion into one revocable thing per role, scope and World", () => {
    const grant = (cap: string, scope: string) =>
      row(
        "them",
        cap,
        { kind: "grant", role: "lait.contributor", definition_ref: "ab" },
        { resource: scope ? [scope] : [] },
      );
    const [them] = foldAccess([
      grant("space.contributor", "prj_eng"),
      grant("space.issue.read", "prj_eng"),
      grant("space.contributor", ""),
    ]);
    expect(them?.membership).toBeNull();
    expect(them?.grants.map((g) => [g.role, g.scope, g.capabilities, g.grantIds.length])).toEqual([
      ["lait.contributor", "prj_eng", ["space.contributor", "space.issue.read"], 2],
      ["lait.contributor", "", ["space.contributor"], 1],
    ]);
  });

  it("lists an unrecorded origin as itself — never membership, never a grant", () => {
    const [them] = foldAccess([
      row("them", "space.issue.read", undefined),
      row("them", "space.contributor", { kind: "admission", role: "lait.contributor" }),
    ]);
    expect(them?.unrecorded.map((c) => c.capability)).toEqual(["space.issue.read"]);
    expect(them?.membership?.capabilities.map((c) => c.capability)).toEqual(["space.contributor"]);
    expect(them?.grants).toEqual([]);
    expect(them && extrasOf(them)).toBe(1);
  });

  it("keeps a grant whose role the World could not name apart by its reference", () => {
    const [them] = foldAccess([
      row(
        "them",
        "space.signage.read",
        { kind: "grant", definition_ref: "01" },
        { world: "com.lait.signage" },
      ),
      row(
        "them",
        "space.signage.manage",
        { kind: "grant", definition_ref: "02" },
        { world: "com.lait.signage" },
      ),
    ]);
    expect(them?.grants.map((g) => [g.role, g.definitionRef, g.capabilities])).toEqual([
      [null, "01", ["space.signage.read"]],
      [null, "02", ["space.signage.manage"]],
    ]);
  });

  it("keeps actors apart and in first-seen order", () => {
    const folded = foldAccess([
      row("b", "space.issue.read", { kind: "admission" }),
      row("a", "space.issue.read", { kind: "admission" }),
      row("b", "space.contributor", { kind: "grant", role: "lait.contributor" }),
    ]);
    expect(folded.map((f) => f.actor)).toEqual(["b", "a"]);
    expect(folded[0]?.grants).toHaveLength(1);
    expect(folded[1]?.grants).toHaveLength(0);
  });
});
