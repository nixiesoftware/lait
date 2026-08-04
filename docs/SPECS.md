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

## Driving it

```http
POST /api/spaces/{id}/worlds/issues/rpc
{"cmd":"spec_new","project":"ENG","kind":"requirement","title":"Login is race-free","text":"…"}
{"cmd":"spec_state","spec":"spc_…","expected":"<revision>","state":"review"}
{"cmd":"spec_state","spec":"spc_…","expected":"<revision>","state":"issued"}

{"cmd":"baseline_new","project":"ENG","name":"Login v1",
 "members":[{"spec":"spc_…","revision":"<revision>"}]}
{"cmd":"baseline_state","baseline":"bas_…","expected":"<revision>","state":"issued"}
{"cmd":"issue_baseline","reff":"ENG-42","baseline":{"baseline":"bas_…","revision":"<revision>"}}
{"cmd":"packet","reff":"ENG-42"}
```

`expected` is a compare-and-swap on the document's current revision: a stale
one is refused rather than silently overwritten.

The same operations are the `issues_spec_*`, `issues_baseline_*`,
`issues_issue_baseline` and `issues_packet` MCP tools. The browser issue detail
renders the effective Packet.

## Web

A project's Specs are a destination of their own, at
`/spaces/:space/projects/:project/specs`, with an open document at `?spec=spc_…`.

The surface draws what has happened to a document and nothing else. A fresh
draft is a kind, a title and a body; each further fact brings exactly one
affordance with it and takes it away again when it goes.

- **Register.** One list, two nouns. Specs group by kind in the order the chain
  runs; Baselines are their own row shape. A row says nothing about its
  lifecycle until it has one, then says it as a word — `Issued`,
  `Issued · draft ahead`, `Concurrent heads`. An issued requirement nothing
  verifies reads `unverified`.
- **Reader.** Kind, an authority sentence, the title, the lifecycle control that
  *is* the transition, and the body. A second revision brings the rail; an issued
  predecessor under a draft head brings the line that says so; concurrent heads
  bring a banner and suppress every transition.
- **Compare and resolve.** Two revisions diff by line over title, body and typed
  links, against their common ancestor when neither descends from the other.
  Resolution writes one draft whose predecessors are every head.
- **Baselines.** A member schedule of exact issued revisions, a compare against
  the predecessor before issuing, and bind/replace/clear on an Issue.
- **Packet.** The derived brief on Issue detail: the bound set, an integrity line
  separating what will arrive on its own from what needs a decision, and each
  item's source route in words — which is what keeps an incorporated Guide from
  reading as an order.

Authority is legible because it is enforced: `spec.write` rides the contributor
demand, so any writing member drafts and sends for review, while `spec.issue`
needs a project grant or `space.admin`. A transition the actor cannot take stays
visible and disabled, naming the capability it wants.

This is a schema cutoff. The Spec and Baseline Bodies begin at their current
schema; there is no legacy markdown importer, compatibility adapter, or staged
migration path.
