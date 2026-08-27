import { describe, expect, it } from "vitest";

import {
  applyLinkDelta,
  authorityPhrase,
  baselineCards,
  commonAncestor,
  conflictPhrase,
  diffBodies,
  diffLines,
  emptyDelta,
  groupByKind,
  holds,
  specCards,
  incomingFor,
  linkDelta,
  linkPhrase,
  standing,
  sourcePhrase,
  standingLabel,
  transitions,
  verificationGap,
} from "./specs";
import type {
  AssignmentDto,
  SpecKind,
  SpecLink,
  SpecReference,
  SpecRevision,
  SpecState,
  SpecView,
} from "../types";

function spec(
  patch: Partial<SpecView> & { spec?: string; links?: SpecView["body"]["links"] } = {},
): SpecView {
  const id = patch.spec ?? "spc_1";
  const kind: SpecKind = patch.kind ?? "requirement";
  const state: SpecState = patch.state ?? "draft";
  const revision = patch.revision ?? "rev_head";
  return {
    spec: id,
    project: "prj_1",
    kind,
    title: patch.title ?? "Login is race-free",
    state,
    revision,
    heads: patch.heads ?? [revision],
    issued: patch.issued ?? [],
    body: {
      spec: id,
      project: "prj_1",
      kind,
      title: patch.title ?? "Login is race-free",
      text: "",
      state,
      links: patch.links ?? [],
      publication: {
        manifest_root: Array(32).fill(1),
        implementation_digest: Array(32).fill(2),
        extractor_schema_digest: Array(32).fill(3),
      },
      author: "act_1",
      ts: 1,
    },
  };
}

describe("spec standing", () => {
  it("keeps the issued revision and the head as separate facts", () => {
    // The case the whole model exists for: a draft successor is being written
    // and the issued predecessor still governs. Neither derives from the other.
    const state = standing(spec({ state: "draft", revision: "rev_2", issued: ["rev_1"] }));
    expect(state).toEqual({ conflict: null, issued: "rev_1", draftAhead: true, head: "draft" });
    expect(standingLabel(state)).toEqual({ text: "Issued · draft ahead", tone: "quiet" });
  });

  it("says nothing about a plain draft", () => {
    // Every Spec starts here, so a word would be a column of one string.
    expect(standingLabel(standing(spec()))).toBeNull();
  });

  it("reads issued, in review and withdrawn from the head", () => {
    const issued = spec({ state: "issued", revision: "rev_1", issued: ["rev_1"] });
    expect(standing(issued).draftAhead).toBe(false);
    expect(standingLabel(standing(issued))).toEqual({ text: "Issued", tone: "quiet" });
    expect(standingLabel(standing(spec({ state: "review" })))?.text).toBe("In review");
    // A withdrawal ends issued truth, so the engine reports no issued revision
    // and the head is the only thing left to say.
    expect(standingLabel(standing(spec({ state: "withdrawn" })))?.text).toBe("Withdrawn");
  });

  it("treats concurrent heads as the absence of a state, not a state", () => {
    const state = standing(spec({ heads: ["rev_a", "rev_b"] }));
    expect(state.conflict).toBe("heads");
    expect(standingLabel(state)).toEqual({ text: "Concurrent heads", tone: "warn" });
    // No winner by ordering, and nothing offered against a head the engine will
    // refuse to accept as *the* expected one.
    expect(transitions(state)).toEqual([]);
  });

  it("reports concurrent issued revisions distinctly from concurrent heads", () => {
    const state = standing(spec({ issued: ["rev_a", "rev_b"] }));
    expect(state.conflict).toBe("issued");
    expect(state.issued).toBeNull();
    expect(standingLabel(state)?.text).toBe("Concurrent issued");
  });
});

describe("spec transitions", () => {
  it("offers review then issue from a draft", () => {
    const moves = transitions(standing(spec({ state: "draft" })));
    expect(moves.map((move) => move.to)).toEqual(["review", "issued"]);
    expect(moves.map((move) => move.capability)).toEqual(["spec.write", "spec.issue"]);
  });

  it("drops review once the revision is already in it", () => {
    const moves = transitions(standing(spec({ state: "review" })));
    expect(moves.map((move) => move.to)).toEqual(["issued"]);
  });

  it("offers withdrawal only while there is issued truth to end", () => {
    expect(transitions(standing(spec({ state: "issued", revision: "rev_1", issued: ["rev_1"] })))
      .map((move) => move.to)).toEqual(["withdrawn"]);
    // Withdrawal keys off the issued revision, not the head — a draft successor
    // over an issued predecessor can still end it.
    expect(transitions(standing(spec({ state: "draft", revision: "rev_2", issued: ["rev_1"] })))
      .map((move) => move.to)).toEqual(["review", "issued", "withdrawn"]);
    // Nothing governs after a withdrawal, so there is nothing left to withdraw.
    expect(transitions(standing(spec({ state: "withdrawn" })))).toEqual([]);
  });

  it("names what an issue supersedes before it happens", () => {
    const over = transitions(standing(spec({ state: "draft", revision: "rev_2", issued: ["rev_1"] })));
    expect(over.find((move) => move.to === "issued")?.describe("rev_2")).toContain("superseding rev_1");
    const first = transitions(standing(spec({ state: "draft" })));
    expect(first.find((move) => move.to === "issued")?.describe("rev_1")).not.toContain("superseding");
  });
});

describe("spec capabilities", () => {
  const grant = (capability: string, resource: string[]): AssignmentDto => ({
    grant_id: "g",
    actor: "act_1",
    world: "com.lait.issues",
    capability,
    resource,
  });

  it("lets any writer draft but not issue", () => {
    const view = spec();
    expect(holds("spec.write", view, [], false)).toBe(true);
    expect(holds("spec.issue", view, [], false)).toBe(false);
  });

  it("accepts a grant scoped to this project, or to the whole space", () => {
    const view = spec();
    expect(holds("spec.issue", view, [grant("spec.issue", ["prj_1"])], false)).toBe(true);
    expect(holds("spec.issue", view, [grant("spec.issue", [])], false)).toBe(true);
    expect(holds("spec.issue", view, [grant("spec.issue", ["prj_other"])], false)).toBe(false);
    expect(holds("spec.issue", view, [grant("spec.write", ["prj_1"])], false)).toBe(false);
  });

  it("gives an admin everything without a scoped grant", () => {
    expect(holds("spec.issue", spec(), [], true)).toBe(true);
  });
});

describe("editing links", () => {
  const to = (spec: string, revision = "rev_1"): SpecLink => ({
    rel: "verifies",
    target: { kind: "spec", spec, revision },
  });

  it("reports what a staged edit added and removed", () => {
    const delta = linkDelta([to("spc_a"), to("spc_b")], [to("spc_b"), to("spc_c")]);
    expect(delta.added).toEqual([to("spc_c")]);
    expect(delta.removed).toEqual([to("spc_a")]);
    expect(emptyDelta(delta)).toBe(false);
    expect(emptyDelta(linkDelta([to("spc_a")], [to("spc_a")]))).toBe(true);
  });

  it("treats the same verb at a different revision as a different claim", () => {
    const delta = linkDelta([to("spc_a", "rev_1")], [to("spc_a", "rev_2")]);
    expect(delta.added).toEqual([to("spc_a", "rev_2")]);
    expect(delta.removed).toEqual([to("spc_a", "rev_1")]);
  });

  it("replays onto a moved head without dropping the other author's addition", () => {
    // We staged c over {a, b}; meanwhile someone else added d and removed b.
    const delta = linkDelta([to("spc_a"), to("spc_b")], [to("spc_a"), to("spc_b"), to("spc_c")]);
    const rebased = applyLinkDelta([to("spc_a"), to("spc_d")], delta);
    expect(rebased).toEqual([to("spc_a"), to("spc_d"), to("spc_c")]);
  });

  it("satisfies a removal whose target is already gone", () => {
    const delta = linkDelta([to("spc_a")], []);
    expect(applyLinkDelta([to("spc_b")], delta)).toEqual([to("spc_b")]);
  });

  it("never duplicates a claim the moved head already carries", () => {
    const delta = linkDelta([], [to("spc_a")]);
    expect(applyLinkDelta([to("spc_a")], delta)).toEqual([to("spc_a")]);
  });
});

describe("comparing revisions", () => {
  const body = (patch: Partial<SpecView["body"]> = {}) => ({ ...spec().body, ...patch });

  it("diffs prose by line, keeping the unchanged context", () => {
    const ops = diffLines("one\ntwo\nthree", "one\ntwo and a half\nthree");
    expect(ops).toEqual([
      { op: "same", text: "one" },
      { op: "remove", text: "two" },
      { op: "add", text: "two and a half" },
      { op: "same", text: "three" },
    ]);
  });

  it("gives up on a diff too large to compute rather than hanging the render", () => {
    const big = Array.from({ length: 20 }, (_, i) => `line ${i}`).join("\n");
    const ops = diffLines(big, `${big}\nmore`, 5);
    expect(ops.every((op) => op.op !== "same")).toBe(true);
  });

  it("compares links as typed assertions, not as text", () => {
    const before = body({
      links: [
        { rel: "verifies", target: { kind: "spec", spec: "spc_x", revision: "rev_1" } },
        { rel: "references", target: { kind: "issue", issue: "ENG-1" } },
      ],
    });
    const after = body({
      links: [
        // The same target, a different claim about it — one removal, one addition.
        { rel: "governs", target: { kind: "spec", spec: "spc_x", revision: "rev_1" } },
        { rel: "references", target: { kind: "issue", issue: "ENG-1" } },
      ],
    });
    const changes = diffBodies(before, after);
    expect(changes.links.removed.map((link) => link.rel)).toEqual(["verifies"]);
    expect(changes.links.added.map((link) => link.rel)).toEqual(["governs"]);
    expect(changes.text).toEqual([]);
  });

  it("reads a link as the sentence its author asserted", () => {
    expect(
      linkPhrase({ rel: "verifies", target: { kind: "spec", spec: "spc_x", revision: "abcdef1234" } }),
    ).toBe("verifies spc_x@abcdef12");
  });

  it("finds the newest revision two heads share", () => {
    const history: SpecRevision[] = [
      { revision: "root", predecessors: [], body: body() },
      { revision: "mid", predecessors: ["root"], body: body() },
      { revision: "left", predecessors: ["mid"], body: body() },
      { revision: "right", predecessors: ["mid"], body: body() },
    ];
    // `mid`, not `root`: the ancestor a reader means is the most recent one,
    // because that is where the two branches actually parted.
    expect(commonAncestor(history, "left", "right")).toBe("mid");
    expect(commonAncestor(history, "left", "mid")).toBe("mid");
  });

  it("reports no ancestor when the joining revisions are not held here", () => {
    const history: SpecRevision[] = [
      { revision: "left", predecessors: ["absent"], body: body() },
      { revision: "right", predecessors: ["also_absent"], body: body() },
    ];
    expect(commonAncestor(history, "left", "right")).toBeNull();
  });
});

describe("authority", () => {
  it("says a guide never enforces, whatever state it reaches", () => {
    for (const state of ["draft", "review", "issued"] as SpecState[]) {
      expect(authorityPhrase("guide", state)).toContain("Never enforcing");
    }
  });

  it("distinguishes in force from not yet in force for governing kinds", () => {
    expect(authorityPhrase("requirement", "draft")).toContain("Not in force yet");
    expect(authorityPhrase("requirement", "issued")).toContain("In force");
    expect(authorityPhrase("requirement", "withdrawn")).toContain("No longer in force");
    // A waiver governs by releasing, which is a different sentence from binding.
    expect(authorityPhrase("waiver", "issued")).toContain("releases work");
  });

  it("keeps evidence and the decision drawn from it apart", () => {
    expect(authorityPhrase("proof", "issued")).toContain("Not a decision");
    expect(authorityPhrase("verdict", "issued")).toContain("A decision");
  });
});

describe("verification coverage", () => {
  const requirement = spec({ spec: "spc_req", kind: "requirement", issued: ["rev_head"] });

  const reference = (
    rel: SpecView["body"]["links"][number]["rel"],
    standing: { head?: boolean; issued?: boolean } = { head: true },
  ): SpecReference => ({
    spec: "spc_proof",
    revision: "rev_p",
    kind: "proof",
    title: "Login test run",
    link: { rel, target: { kind: "spec", spec: "spc_req", revision: "rev_head" } },
    head: standing.head ?? false,
    issued: standing.issued ?? false,
  });

  it("reports a gap only once something is actually in force", () => {
    expect(verificationGap(requirement, [])).toBe(true);
    // A draft nobody has issued is a document being written, not a gap.
    expect(verificationGap(spec({ kind: "requirement" }), [])).toBe(false);
    // Guidance is not the kind of thing proof attaches to.
    expect(verificationGap(spec({ kind: "guide", issued: ["rev_head"] }), [])).toBe(false);
  });

  it("closes once anything verifies or validates it", () => {
    expect(verificationGap(requirement, [reference("verifies")])).toBe(false);
    expect(verificationGap(requirement, [reference("validates")])).toBe(false);
  });

  it("does not count a mere reference as verification", () => {
    expect(verificationGap(requirement, [reference("references")])).toBe(true);
  });

  /**
   * The bug the reverse-edge query exists to fix. A Proof whose head dropped the
   * link still verifies through its issued revision, and a Proof that asserted
   * one only in a superseded revision does not verify at all — neither is
   * answerable from head bodies, and both are wrong in the direction that
   * matters for a coverage indicator.
   */
  it("counts the revision that governs, not merely the newest one", () => {
    expect(verificationGap(requirement, [reference("verifies", { issued: true })])).toBe(false);
    expect(
      verificationGap(requirement, [reference("verifies", { head: false, issued: false })]),
    ).toBe(true);
  });

  it("gives the same standing rule to incoming edges", () => {
    expect(incomingFor("spc_req", [reference("verifies")])).toHaveLength(1);
    expect(incomingFor("spc_req", [reference("verifies", { head: false })])).toHaveLength(0);
  });
});

describe("packet vocabulary", () => {
  it("names the route a governing item arrived by", () => {
    expect(sourcePhrase({ route: "direct" })).toContain("directly");
    expect(sourcePhrase({ route: "baseline", baseline: "bas_1" })).toContain("baseline");
    // The one that matters: an incorporated guide sits in the governing set, and
    // this sentence is what stops it reading as an order.
    expect(sourcePhrase({ route: "incorporated", spec: "spc_g", revision: "rev_1" })).toContain(
      "Incorporated by spc_g",
    );
  });

  it("separates what will arrive on its own from what needs a person", () => {
    expect(conflictPhrase({ reason: "missing_spec", spec: "spc_1" }).kind).toBe("missing");
    expect(
      conflictPhrase({ reason: "baseline_not_issued", baseline: "bas_1", revision: "rev_1" }).kind,
    ).toBe("unissued");
    expect(conflictPhrase({ reason: "issued_spec_conflict", spec: "spc_1" }).kind).toBe("conflict");
  });
});

describe("register grouping", () => {
  it("orders by the chain, not the reply, and omits kinds nobody has written", () => {
    const groups = groupByKind([
      spec({ spec: "a", kind: "record" }),
      spec({ spec: "b", kind: "goal" }),
      spec({ spec: "c", kind: "requirement" }),
    ]);
    expect(groups.map((group) => group.kind)).toEqual(["goal", "requirement", "record"]);
  });
});

describe("register cards", () => {
  const head = { revision: "r1", title: "Login is race-free", state: "draft" as const, author: "a", ts: 7 };
  const row = {
    spec: "spc_1", project: "prj_1", kind: "requirement" as const,
    heads: ["r1"], issued: ["r0"], conflicted: false, head,
  };

  it("joins a summary with its one head", () => {
    expect(specCards([row])).toEqual([{
      spec: "spc_1", project: "prj_1", kind: "requirement", title: "Login is race-free",
      state: "draft", revision: "r1", heads: ["r1"], issued: ["r0"], ts: 7,
    }]);
  });

  it("offers neither a row without a head nor a conflicted one", () => {
    // No head: the corpus has not posted it yet, or the heads are concurrent.
    expect(specCards([{ ...row, head: null }])).toEqual([]);
    expect(specCards([{ ...row, heads: ["r1", "r2"], head: null }])).toEqual([]);
    // One head but concurrent issued revisions: nothing of it is authoritative.
    expect(specCards([{ ...row, issued: ["r0", "r0b"], conflicted: true }])).toEqual([]);
    expect(baselineCards([{
      baseline: "bsl_1", project: "prj_1", heads: ["b1"], issued: ["b0", "b0b"], conflicted: true,
      head: { revision: "b1", name: "E0", state: "draft", author: "a", ts: 1 },
    }])).toEqual([]);
  });
});
