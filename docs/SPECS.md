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
- `Observation` is a retractable note *about* the graph, filed against a Spec by
  whoever noticed it. It is the deliberate inverse of a Link on every axis: it
  carries its own observer, it sits in no revision and so never enters a content
  hash, issuing the document neither adopts nor freezes it, it lives in a CRDT
  set so two observers converge instead of colliding, and it is retractable on
  its own. **An Observation never reaches a Packet and never counts as
  verification coverage.**
- `PlanData` is the seed of a Plan revision: zero or more same-project Issue
  roots. An empty seed means the whole project. It stores no phases, membership,
  position, completion, or drawing. Blueprint compiles those from canonical
  Issue relations and metadata at one exact World generation.
- `Geometry` is that deterministic projection: connected components,
  dependency layers, hierarchy depth, closure states, facets, and positioned
  residual loci. It is reproducible output, never replicated plan truth.

A Link is what a document *says*; an Observation is what somebody *noticed*. The
distinction exists because some truths — this requirement conflicts with that
one, this design depends on that one, this proof turns out to cover a second
requirement — belong to neither endpoint's author, and laundering them through a
document forces an amendment to material the observer may not own and, on issued
material, a draft successor that announces a change to governing truth which is
not happening.

The lifecycle is `draft → review → issued → withdrawn`. Every transition writes
a new immutable revision. Drafting a successor does not silently revoke its
issued predecessor. A later issued successor replaces it; a withdrawn successor
ends it. Publishing and withdrawal require the project-scoped issuing
capability, while drafting and review use the writing capability.

## From intent to completed work

The deterministic chain is:

1. Goals establish intent.
2. Requirements state outcomes and constraints.
3. Plans name the work whose order Blueprint should derive.
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

{"cmd":"spec_revise","spec":"spc_…","expected":"<revision>",
 "plan":{"roots":["iss_…","iss_…"]}}
{"cmd":"geometry","project":"ENG","roots":["iss_…","iss_…"]}

{"cmd":"spec_observe","spec":"spc_…","rel":"conflicts",
 "target":{"kind":"spec","spec":"spc_…","revision":"<revision>"},
 "note":"both claim the session limit and they disagree"}
{"cmd":"spec_retract","spec":"spc_…","observation":"obs_…"}

{"cmd":"baseline_new","project":"ENG","name":"Login v1",
 "members":[{"spec":"spc_…","revision":"<revision>"}]}
{"cmd":"baseline_state","baseline":"bas_…","expected":"<revision>","state":"issued"}
{"cmd":"issue_baseline","reff":"ENG-42","baseline":{"baseline":"bas_…","revision":"<revision>"}}
{"cmd":"packet","reff":"ENG-42"}
```

`expected` is a compare-and-swap on the document's current revision: a stale
one is refused rather than silently overwritten.

For `spec_revise`, omitting `plan` preserves the current seed, sending
`plan: null` removes it, and sending a value replaces it. Structured Plan data
is accepted only on `kind: "plan"`. Roots are canonical, sorted, unique,
same-project Issue ids, with a maximum of 32. An empty root set deliberately
selects the whole project. The read-only migration decoder accepts the retired
phase-shaped Plan JSON and collapses its Issue membership into roots; every new
write emits only the root shape.

Every Spec revision records the Manifest root it was composed against. Current
Plan readers query current Issue morphology, so changing a status or relation
moves the open loci without revising the document. A historical revision asks
Lait for that exact retained World generation and therefore reconstructs the
Issue shape that author could see. Revisions written before generation
coordinates remain readable and say plainly that their morphology is live.

`spec_observe` and `spec_retract` carry no `expected`: a note is not a revision
and does not compete for the head, so two observers never refuse each other.
Observing rides the writing capability. Retraction is the observer's own by
right; taking back somebody else's is a judgement about the record and needs the
project's issuing capability.

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
- **Plans.** Every Plan is named by the shared `Plan` kind label and its Spec
  title; it gains no second plan number. Prose remains an ordinary low-friction
  document. Below it, the reader draws the Issue-derived morphology: branches,
  convergence, containment, disconnected organic patches, cycles, blocked
  frontiers, and terminal closure loci. Editing a Plan means editing only its
  roots. Project, team, status, label, milestone, assignee, hierarchy, and
  dependency facts stay on their canonical Issue-world primitives. Selecting a
  node opens the canonical Issue. Dense views bound drawing work while the
  server's closure and counts continue to cover the full graph.
- **Relations.** Both directions, grouped by verb and read as sentences. Editing
  is staged and saved as *one* revision rather than one per link, and a save that
  meets a moved head replays the delta onto it — adds and removes are set
  operations, so a rebase needs no merge policy and cannot invent a claim neither
  author made. Choosing a target pins an exact revision, defaulting to the issued
  one where there is one, and says which it pinned.
- **Noticed.** Observations, under the relations and phrased from the observer
  outward — "Omar noticed this conflicts with X". Its own section, and one line
  saying that nothing in it governs, is issued, or counts as verification: a note
  that merely *looked* like a relation is a note somebody will read as an order.
  Available on a conflicted document, unlike every revision-writing control,
  because a note competes for no head.
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

Spec prose uses the same user-invisible document schema as Issue descriptions.
Readers and editors operate on the controlled document model; CLI and MCP text
remains semantic plain text. Legacy Markdown heads stay readable and expose an
**Upgrade document** action in the header menu. Upgrading writes an immutable
successor, preserves the head's lifecycle state, and leaves the original
revision in history. Baseline Bodies have no prose field and need no document
migration.
