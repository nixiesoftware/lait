import type {
  AssignmentDto,
  PacketConflict,
  PacketSource,
  SpecBody,
  SpecKind,
  SpecLink,
  SpecReference,
  SpecRel,
  SpecRevision,
  SpecState,
  SpecTarget,
  SpecView,
} from "../types";

/**
 * What the product calls the parts of a Spec.
 *
 * One home, for one reason: the register, the reader, the create flow and every
 * surface after them must use the same word for the same fact, and "we agreed to
 * in review" is not a mechanism. A label that exists in exactly one place cannot
 * drift; a label copied into four components already has.
 *
 * Only the vocabulary the client actually *draws* lives here. Packet source
 * routes and authority phrasing are real facts in the engine, but nothing
 * renders them yet — they arrive in this file when a surface arrives that shows
 * them, not before. A dictionary of words nobody says is how the words stop
 * matching what the screen does.
 */

/**
 * The kinds, in the order the chain runs (docs/SPECS.md): intent, then the
 * outcomes it demands, then the sequence, the solution, and the amendments —
 * followed by the non-enforcing and after-the-fact material.
 *
 * The order is the product's, not the alphabet's, and it is what the register
 * groups by: a project read top to bottom should read as the chain.
 */
export const SPEC_KINDS = [
  "goal",
  "requirement",
  "plan",
  "design",
  "order",
  "guide",
  "proof",
  "verdict",
  "waiver",
  "record",
] as const satisfies readonly SpecKind[];

export const SPEC_KIND_LABEL: Record<SpecKind, string> = {
  goal: "Goal",
  requirement: "Requirement",
  plan: "Plan",
  design: "Design",
  order: "Order",
  guide: "Guide",
  proof: "Proof",
  verdict: "Verdict",
  waiver: "Waiver",
  record: "Record",
};

/** The plural, for a group of them. English is irregular enough to be worth a
 *  table rather than an `+ "s"` that will one day meet a kind ending in `s`. */
export const SPEC_KIND_PLURAL: Record<SpecKind, string> = {
  goal: "Goals",
  requirement: "Requirements",
  plan: "Plans",
  design: "Designs",
  order: "Orders",
  guide: "Guides",
  proof: "Proof",
  verdict: "Verdicts",
  waiver: "Waivers",
  record: "Records",
};

/**
 * One line on what each kind is *for*, shown where the choice is made.
 *
 * Kind is the only thing a new Spec is asked to declare, and it is the one
 * decision the writer cannot revise later by typing — so the picker owes them
 * enough to choose correctly the first time. Phrased as what the document does,
 * because that is the question someone creating one is actually holding.
 */
export const SPEC_KIND_BLURB: Record<SpecKind, string> = {
  goal: "Why this exists — the intent the work serves.",
  requirement: "An outcome or constraint the work must satisfy.",
  plan: "How the work is sequenced.",
  design: "How the solution is built.",
  order: "A change or instruction that amends issued work.",
  guide: "Advice and context. Never enforcing.",
  proof: "What was inspected, analysed, demonstrated or tested.",
  verdict: "The conclusion drawn from evidence.",
  waiver: "A scoped release from a requirement.",
  record: "A decision or as-built fact worth keeping.",
};

export function specKindLabel(kind: SpecKind): string {
  return SPEC_KIND_LABEL[kind] ?? kind;
}

/**
 * The register's rows, grouped into the chain's order.
 *
 * Empty kinds are absent, not empty groups: unlike a status column — which
 * exists because the workflow says it does, whether or not anything is in it —
 * a kind with no documents is not a bucket somebody left open. It is a kind
 * nobody has written yet, and drawing ten headings over one document is the
 * exact opposite of what this surface is for.
 */
export function groupByKind(specs: readonly SpecView[]): { kind: SpecKind; specs: SpecView[] }[] {
  return SPEC_KINDS.map((kind) => ({
    kind,
    specs: specs.filter((spec) => spec.kind === kind),
  })).filter((group) => group.specs.length > 0);
}

// ---- lifecycle --------------------------------------------------------------

export const SPEC_STATE_LABEL: Record<SpecState, string> = {
  draft: "Draft",
  review: "In review",
  issued: "Issued",
  withdrawn: "Withdrawn",
};

/**
 * Where a Spec stands, from the three facts a `SpecView` reports.
 *
 * `state`/`revision` describe the **head** — the newest revision, which may be a
 * draft. `issued` names the revision that actually governs, which is a different
 * question with a different answer: drafting a successor does not revoke the
 * issued predecessor, so a Spec routinely has a draft head *and* an issued
 * revision at the same time. Collapsing the two is the specific mistake the
 * whole model exists to prevent, so nothing derives one from the other.
 *
 * Concurrent heads are not a state. They are the absence of one: no revision is
 * authoritative, and the engine refuses every transition until an explicit
 * resolution names all of them.
 */
export interface SpecStanding {
  /** No authoritative revision. `heads` = concurrent successors; `issued` =
   *  concurrent controlling revisions. Either suppresses every transition. */
  conflict: "heads" | "issued" | null;
  /** The one revision that governs, when exactly one does. */
  issued: string | null;
  /** An issued revision exists and the head is not it — successor in progress. */
  draftAhead: boolean;
  /** The head's own state. */
  head: SpecState;
}

export function standing(spec: SpecView): SpecStanding {
  const conflict = spec.heads.length > 1 ? "heads" : spec.issued.length > 1 ? "issued" : null;
  const issued = spec.issued.length === 1 ? spec.issued[0]! : null;
  return {
    conflict,
    issued,
    draftAhead: issued !== null && issued !== spec.revision,
    head: spec.state,
  };
}

/**
 * What a register row says about its lifecycle, or `null` for nothing to say.
 *
 * A plain draft returns `null` on purpose. Every Spec starts as one, so a word
 * there would appear on every row of a fresh project — a column of the same
 * string, which is the shape that turns a readable index into a status wall.
 * The word arrives when the document does something.
 *
 * "Issued · draft ahead" is deliberately two clauses rather than one collapsed
 * state: the issued revision still governs while its successor is written, and a
 * row that showed only the head would say the issued truth had gone away.
 */
export function standingLabel(
  state: SpecStanding,
): { text: string; tone: "quiet" | "warn" } | null {
  if (state.conflict === "heads") return { text: "Concurrent heads", tone: "warn" };
  if (state.conflict === "issued") return { text: "Concurrent issued", tone: "warn" };
  if (state.issued) {
    return { text: state.draftAhead ? "Issued · draft ahead" : "Issued", tone: "quiet" };
  }
  if (state.head === "withdrawn") return { text: "Withdrawn", tone: "quiet" };
  if (state.head === "review") return { text: "In review", tone: "quiet" };
  return null;
}

// ---- authority --------------------------------------------------------------

/**
 * What this document can and cannot do to the work — one sentence, in words.
 *
 * The question a reader actually holds is "can I act on this, or is it context",
 * and neither the kind nor the state answers it alone: a Requirement in draft
 * enforces nothing, and a Guide enforces nothing *ever*, however it is
 * referenced. Both facts have to be said, because a Guide that merely looks
 * quiet is a Guide someone will read as an instruction.
 *
 * Said plainly rather than encoded. VIEW-17's rule is that authority never rides
 * on colour, and a sentence is the only treatment that survives monochrome, a
 * screen reader, and someone who has not learned the palette.
 */
export function authorityPhrase(kind: SpecKind, state: SpecState): string {
  const enforcing = kind === "requirement" || kind === "design" || kind === "order" || kind === "waiver";
  if (enforcing) {
    if (state === "issued") {
      return kind === "waiver"
        ? "In force — it releases work from a requirement within its scope."
        : "In force — work that binds this must satisfy it.";
    }
    if (state === "withdrawn") return "No longer in force. Kept as a record of what once governed.";
    return "Not in force yet. Governing material takes effect when it is issued.";
  }
  switch (kind) {
    case "guide":
      return "Advice. Never enforcing, however it is referenced.";
    case "goal":
      return "Intent. It explains why the work exists rather than constraining it.";
    case "plan":
      return "Sequencing. It orders the work rather than constraining what it must satisfy.";
    case "proof":
      return "Evidence of what was checked. Not a decision about it.";
    case "verdict":
      return "A decision drawn from evidence.";
    case "record":
      return "A record of what was decided or built.";
    default:
      return "";
  }
}

/**
 * Is anything standing behind this — and should the absence be said?
 *
 * Only for issued enforcing material. Before issuance "nothing verifies this" is
 * noise about a document still being written; after it, it is the gap a coverage
 * matrix exists to find, and it is the one absence worth drawing.
 *
 * Read from the reference set rather than from head bodies. A Proof whose head
 * dropped the link while its *issued* revision still asserts it would look like
 * a gap, and one whose superseded revision asserted it would look like coverage
 * — both wrong, and wrong in the direction that matters. An edge only counts
 * here when the revision asserting it is current or governing.
 */
export function verificationGap(
  spec: SpecView,
  references: readonly SpecReference[],
): boolean {
  const enforcing = spec.kind === "requirement" || spec.kind === "design";
  if (!enforcing || spec.issued.length !== 1) return false;
  return !references.some(
    (reference) =>
      (reference.head || reference.issued) &&
      reference.spec !== spec.spec &&
      (reference.link.rel === "verifies" || reference.link.rel === "validates") &&
      reference.link.target.kind === "spec" &&
      reference.link.target.spec === spec.spec,
  );
}

/** The assertions pointing at this document that anyone still stands behind. */
export function incomingFor(
  spec: string,
  references: readonly SpecReference[],
): SpecReference[] {
  return references.filter(
    (reference) =>
      reference.spec !== spec &&
      (reference.head || reference.issued) &&
      reference.link.target.kind === "spec" &&
      reference.link.target.spec === spec,
  );
}

// ---- relations --------------------------------------------------------------

/**
 * The relation verbs, as the sentence each one makes.
 *
 * A row reads "verifies spc_…@abc1234", not "VERIFIES" beside an id: a relation
 * is a claim its author made about an exact target, and the enum name is the
 * storage spelling of that claim rather than the claim itself. `references` in
 * particular has to read as informative — it is the one verb that deliberately
 * never becomes enforcing, and a bare tag would let it look like the others.
 */
export const SPEC_REL_LABEL: Record<SpecRel, string> = {
  derives: "derives from",
  decomposes: "decomposes into",
  implements: "implements",
  governs: "governs",
  amends: "amends",
  supersedes: "supersedes",
  clarifies: "clarifies",
  incorporates: "incorporates",
  references: "references",
  verifies: "verifies",
  validates: "validates",
  waives: "waives",
  records: "records",
  conflicts: "conflicts with",
  depends: "depends on",
};

/** What a Link's target is, as a coordinate a reader can copy. */
export function targetLabel(target: SpecTarget): string {
  switch (target.kind) {
    case "issue":
      return target.issue;
    case "spec":
      return `${target.spec}@${short(target.revision)}`;
    case "baseline":
      return `${target.baseline}@${short(target.revision)}`;
  }
}

export function linkPhrase(link: SpecLink): string {
  return `${SPEC_REL_LABEL[link.rel]} ${targetLabel(link.target)}`;
}

// ---- comparing revisions ----------------------------------------------------

export interface LineOp {
  op: "same" | "add" | "remove";
  text: string;
}

/**
 * A line diff, by longest common subsequence.
 *
 * Lines rather than words or characters: a Spec body is prose in Markdown, and
 * the unit a reader reviews a change in is the paragraph they can point at. A
 * character diff of rewritten prose produces confetti nobody can read.
 *
 * Bounded, because this runs in the render path. Beyond the cap the honest
 * answer is "this was replaced" rather than a diff that locks the tab: the
 * quadratic table is what makes LCS exact, and there is no cheap exact version.
 */
export function diffLines(from: string, to: string, cap = 800): LineOp[] {
  const a = from.length ? from.split("\n") : [];
  const b = to.length ? to.split("\n") : [];
  if (a.length > cap || b.length > cap) {
    return [
      ...a.map((text): LineOp => ({ op: "remove", text })),
      ...b.map((text): LineOp => ({ op: "add", text })),
    ];
  }

  // table[i][j] = length of the LCS of a[i..] and b[j..]
  const table: number[][] = Array.from({ length: a.length + 1 }, () =>
    new Array<number>(b.length + 1).fill(0),
  );
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      table[i]![j] = a[i] === b[j]
        ? table[i + 1]![j + 1]! + 1
        : Math.max(table[i + 1]![j]!, table[i]![j + 1]!);
    }
  }

  const out: LineOp[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ op: "same", text: a[i]! });
      i++;
      j++;
    } else if (table[i + 1]![j]! >= table[i]![j + 1]!) {
      out.push({ op: "remove", text: a[i]! });
      i++;
    } else {
      out.push({ op: "add", text: b[j]! });
      j++;
    }
  }
  while (i < a.length) out.push({ op: "remove", text: a[i++]! });
  while (j < b.length) out.push({ op: "add", text: b[j++]! });
  return out;
}

export interface BodyDiff {
  title: { from: string; to: string } | null;
  state: { from: SpecState; to: SpecState } | null;
  text: LineOp[];
  links: { added: SpecLink[]; removed: SpecLink[] };
}

/** A key that identifies a Link by what it asserts about which exact target. */
function linkKey(link: SpecLink): string {
  const target = link.target;
  const at =
    target.kind === "issue"
      ? target.issue
      : target.kind === "spec"
        ? `${target.spec}@${target.revision}`
        : `${target.baseline}@${target.revision}`;
  return `${link.rel}:${target.kind}:${at}`;
}

/**
 * What changed between two revisions, per field.
 *
 * Links are compared as a set of typed assertions, never as text: "this now
 * `verifies` REQ-3 instead of REQ-2" is a different claim about the world from
 * a string that happens to differ, and a stringified JSON diff of them is the
 * thing VIEW-22's non-goals rule out by name.
 */
export function diffBodies(from: SpecBody, to: SpecBody): BodyDiff {
  const before = new Map(from.links.map((link) => [linkKey(link), link]));
  const after = new Map(to.links.map((link) => [linkKey(link), link]));
  return {
    title: from.title === to.title ? null : { from: from.title, to: to.title },
    state: from.state === to.state ? null : { from: from.state, to: to.state },
    text: from.text === to.text ? [] : diffLines(from.text, to.text),
    links: {
      added: [...after].filter(([key]) => !before.has(key)).map(([, link]) => link),
      removed: [...before].filter(([key]) => !after.has(key)).map(([, link]) => link),
    },
  };
}

/**
 * The newest revision both of these descend from.
 *
 * What a comparison is actually *against* when neither side is the other's
 * ancestor: diffing two concurrent heads head-to-head shows the union of two
 * independent edits and attributes each to the wrong author. The ancestor is
 * what makes "who changed what" answerable.
 *
 * `null` when they share nothing — a partial replica missing the joining
 * revisions, which is a different answer from "they diverged at the root".
 */
export function commonAncestor(
  history: readonly SpecRevision[],
  a: string,
  b: string,
): string | null {
  const parents = new Map(history.map((entry) => [entry.revision, entry.predecessors]));
  const reach = (from: string): Set<string> => {
    const seen = new Set<string>();
    const queue = [from];
    while (queue.length) {
      const at = queue.pop()!;
      if (seen.has(at)) continue;
      seen.add(at);
      queue.push(...(parents.get(at) ?? []));
    }
    return seen;
  };
  const shared = [...reach(a)].filter((revision) => reach(b).has(revision));
  if (shared.length === 0) return null;
  // The maximal one: a shared revision that no other shared revision descends
  // from is the most recent, and that is the one a reader means by "ancestor".
  const descendsFromAnother = (candidate: string) =>
    shared.some((other) => other !== candidate && reach(other).has(candidate));
  const maximal = shared.filter((candidate) => !descendsFromAnother(candidate));
  return maximal.sort()[0] ?? shared.sort()[0] ?? null;
}

// ---- packet -----------------------------------------------------------------

/**
 * Why this revision is in the brief.
 *
 * Load-bearing rather than decorative, and specifically in the governing
 * section: incorporation pulls an exact target into the governing set whatever
 * its kind, so an incorporated Guide sits beside the Requirements. The only
 * thing standing between that and "the guide is an order" is this sentence, so
 * it may not hide behind a disclosure.
 */
export function sourcePhrase(source: PacketSource): string {
  switch (source.route) {
    case "baseline":
      return "Pinned by this issue's baseline";
    case "direct":
      return "Governs this issue directly";
    case "incorporated":
      return `Incorporated by ${source.spec}`;
  }
}

/**
 * What is wrong with the brief, and — the part that matters — whether it is
 * something to wait for or something to do.
 *
 * A Body that has not arrived converges on its own. A Baseline nobody issued,
 * or a Spec with two issued revisions, does not: someone has to act. Rendering
 * both as "conflict" would tell a reader to sit and wait for a person.
 */
export function conflictPhrase(
  conflict: PacketConflict,
): { text: string; kind: "missing" | "unissued" | "conflict" } {
  switch (conflict.reason) {
    case "missing_baseline":
      return { text: `Baseline ${conflict.baseline} has not arrived here yet.`, kind: "missing" };
    case "missing_baseline_revision":
      return {
        text: `Baseline revision ${conflict.baseline}@${short(conflict.revision)} has not arrived here yet.`,
        kind: "missing",
      };
    case "baseline_not_issued":
      return {
        text: `The bound baseline revision ${short(conflict.revision)} is not issued, so nothing it names is in force.`,
        kind: "unissued",
      };
    case "missing_spec":
      return { text: `Spec ${conflict.spec} has not arrived here yet.`, kind: "missing" };
    case "missing_spec_revision":
      return {
        text: `Revision ${short(conflict.revision)} of ${conflict.spec} has not arrived here yet.`,
        kind: "missing",
      };
    case "issued_spec_conflict":
      return {
        text: `${conflict.spec} has concurrent issued revisions, so none of them governs.`,
        kind: "conflict",
      };
    case "missing_incorporated":
      return {
        text: `Incorporated revision ${short(conflict.revision)} of ${conflict.spec} has not arrived here yet.`,
        kind: "missing",
      };
  }
}

/** The same 8-character truncation the rest of the client uses for coordinates. */
const short = (revision: string) => revision.slice(0, 8);

/** The capability a transition demands — `contract.rs` `SPEC_CAPABILITIES`. */
export type SpecCapability = "spec.write" | "spec.issue";

export interface SpecTransition {
  to: SpecState;
  label: string;
  capability: SpecCapability;
  /** What the reader says before it happens, naming the exact revision. */
  describe: (revision: string) => string;
}

/**
 * The transitions this head can take, in the order the lifecycle runs.
 *
 * Legality mirrors `implementation.rs` `SpecState` exactly rather than
 * approximating it, because an offered action the engine refuses is worse than
 * an absent one. Review comes from a draft; issuing comes from a draft or a
 * revision in review; withdrawal ends issued truth, so it is offered only while
 * there *is* issued truth to end — and never from the head's own state, which is
 * why it reads `standing.issued` rather than `standing.head`.
 *
 * A conflict returns nothing at all. The engine rejects a transition whose
 * expected head is one of several, so offering one would be a button that exists
 * to fail.
 */
export function transitions(state: SpecStanding): SpecTransition[] {
  if (state.conflict) return [];
  const out: SpecTransition[] = [];
  if (state.head === "draft") {
    out.push({
      to: "review",
      label: "Send for review",
      capability: "spec.write",
      describe: (revision) => `Puts revision ${revision} in review. It does not govern yet.`,
    });
  }
  if (state.head === "draft" || state.head === "review") {
    out.push({
      to: "issued",
      label: "Issue",
      capability: "spec.issue",
      describe: (revision) =>
        state.issued
          ? `Makes revision ${revision} the governing truth, superseding ${state.issued}.`
          : `Makes revision ${revision} the governing truth.`,
    });
  }
  if (state.issued) {
    out.push({
      to: "withdrawn",
      label: "Withdraw",
      capability: "spec.issue",
      describe: () =>
        `Ends revision ${state.issued} as governing truth. It stays readable in history.`,
    });
  }
  return out;
}

/**
 * Does this actor hold `capability` over this Spec's project?
 *
 * Deliberately permissive at the edges: a Space-wide assignment (empty resource)
 * counts, and an admin holds everything. The engine is the authority and refuses
 * what it must — this only decides whether to *offer* the action, and hiding a
 * transition someone may legitimately take is the worse error of the two.
 */
export function holds(
  capability: SpecCapability,
  spec: SpecView,
  grants: readonly AssignmentDto[],
  admin: boolean,
): boolean {
  if (admin) return true;
  // Drafting and review ride the ordinary contributor demand, which every
  // writing member satisfies (`demand_project_work`). Only issuing is scoped.
  if (capability === "spec.write") return true;
  return grants.some(
    (grant) =>
      grant.capability === capability &&
      (grant.resource.length === 0 || grant.resource[0] === spec.project),
  );
}
