# Specs, revisions, and baselines

An Issue says what work is happening. A Spec says what that work is meant to
satisfy. They are different durable truths and neither is stored inside the
other's markdown.

## Model

- `Spec` is the stable identity of project truth. Its `Kind` is `goal`,
  `requirement`, `plan`, `design`, `order`, `guide`, `proof`, `verdict`,
  `waiver`, or `record`.
- `Revision` is immutable content with exact predecessor revisions. Concurrent
  successors remain visible as conflict heads; they are never resolved by
  last-writer-wins.
- `Link` is a typed edge to an exact Spec revision, exact Baseline revision, or
  stable Issue. `governs` is enforcing. `incorporates` pulls exact referenced
  Specs into an effective packet. `references` is informative and never
  becomes enforcing by implication.
- `Baseline` is a named, reviewed set of exact issued Spec revisions. It is the
  issue tracker's equivalent of an issued drawing set or configuration
  baseline.
- `Packet` is the derived effective brief for one Issue. It groups governing
  truth, guidance, proof, records, and unresolved conflicts. A Packet is a
  projection, not another replicated source of truth.

The lifecycle is `draft → review → issued → withdrawn`. Every transition writes
a new immutable revision. Drafting a successor does not silently revoke its
issued predecessor. A later issued successor replaces it; a withdrawn successor
ends it. Publishing and withdrawal require the project-scoped issuing
capability, while drafting and review use the writing capability.

## From intent to completed work

The deterministic chain is:

1. Goals establish intent.
2. Requirements state outcomes and constraints.
3. Plans sequence the work.
4. Designs specify the solution.
5. Orders amend or direct issued work.
6. Guides remain explicitly non-enforcing.
7. A Baseline freezes the exact issued set used by an Issue.
8. Proof and Verdict Specs record verification and acceptance.
9. Record Specs preserve decisions and as-built facts.

An Issue may be governed directly by an issued Spec with a `governs` Link, or
may pin an exact issued Baseline. Its Packet is the only supported way for a
client or agent to answer “what governs this work now?” without reimplementing
the graph rules.

## CLI

```console
lait issues spec new ENG requirement "Login is race-free" --text "…"
lait issues spec review spc_… --expect <revision>
lait issues spec issue spc_… --expect <revision>

lait issues baseline new ENG "Login v1" --member spc_…@<revision>
lait issues baseline issue bas_… --expect <revision>
lait issues baseline bind ENG-42 bas_…@<revision>
lait issues packet ENG-42
```

The same operations and typed schemas are exposed by the Issues application
protocol and MCP tools. The browser issue detail renders the effective Packet.

## Web

A project's Specs are a destination of their own, at
`/spaces/:space/projects/:project/specs`, with an open document at `?spec=spc_…`.

The surface draws what has happened to a document and nothing else. Today that
is its kind, title, body and author: a Spec is created, read, and revised there,
and each commit writes a new immutable revision against the head the reader was
showing. Lifecycle state, the revision trail, exact coordinates, Baselines and
traceability are real facts in the engine that this surface does not yet draw —
each arrives with the affordance that reads it, rather than as a column reserved
for a document that has not entered it.

This is a schema cutoff. The Spec and Baseline Bodies begin at their current
schema; there is no legacy markdown importer, compatibility adapter, or staged
migration path.
