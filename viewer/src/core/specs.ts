import type { SpecKind, SpecView } from "../types";

/**
 * What the product calls the parts of a Spec.
 *
 * One home, for one reason: the register, the reader, the create flow and every
 * surface after them must use the same word for the same fact, and "we agreed to
 * in review" is not a mechanism. A label that exists in exactly one place cannot
 * drift; a label copied into four components already has.
 *
 * Only the vocabulary the client actually *draws* lives here. Lifecycle state,
 * authority phrasing and packet source routes are real facts in the engine, but
 * nothing renders them yet — they arrive in this file when a surface arrives
 * that shows them, not before. A dictionary of words nobody says is how the
 * words stop matching what the screen does.
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
