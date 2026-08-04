---
name: lait
description: File and drive issues in a local-first, peer-to-peer issue tracker via the lait MCP server — create/edit/move/assign/label/comment/close issues, read boards and lists, and follow the activity feed. Use when the user asks this agent to track work, file an issue, update a ticket, or work a board in lait.
---

# Lait: a peer-to-peer issue tracker

You have a `lait` MCP server. It drives a local node that owns the space's
Loro-CRDT issue documents over a durable content-addressed store. lait has no
command surface — do not try to run `lait <verb>` in a shell; these tools *are*
the interface, and the same operations are what the local web app sends.

Every tool returns the same versioned JSON DTO the HTTP head emits.

## You are a member

Your identity is sponsored by a human and holds **write standing** — you file,
comment, start, close, and delete issues under your own key, and the activity
log attributes your work to you and not to your sponsor. Call `whoami` first if
you need to know who you are and what you may do; call `sync` if a board looks
short, since it reports keyring completeness loudly instead of silently showing
fewer issues.

If a write is denied you get a typed refusal naming the next step. Take it at
face value — ask the sponsor or an admin — rather than retrying.

## Refs
An issue `<ref>` is a short `iss_` handle (canonical, collision-free) or a `KEY-n`
alias like `ENG-142`. A project ref is its key (`ENG`) or a `prj_` id. A who-ref
is `@me` or a 64-hex key. If a ref is ambiguous the tool returns a candidate list —
re-issue with a more specific handle.

Product tools are namespaced by the World's mount, so every issue-tracker tool is
`issues_*`. Membership, identity, and transport tools are shell-owned and bare.

## File and drive work
- `issues_project_new {name, key}` / `issues_project_list` — manage projects.
  Create a project before the first issue.
- `issues_new {title, project?, assignees?, priority?, labels?, body?}` — create an
  issue; returns the resolved handle. Priority is none|low|medium|high|urgent.
- `issues_start {reff}` · `issues_done {reff}` · `issues_stop {reff}` — the work
  loop: claim + activate, finish, put down.
- `issues_edit {reff, title?, status?, priority?}` — patch fields; all flags in one
  call is one commit = one activity row.
- `issues_move {reff, project?, position?}` — position is `top`|`bottom`|
  `before:<ref>`|`after:<ref>`. Setting a project changes membership (the truth).
- `issues_assign {reff, who:[…], remove?}` · `issues_label {reff, add:[…],
  remove:[…]}` · `issues_comment {reff, body}` · `issues_delete {reff}`
  (tombstone; stays in history) · `issues_restore {reff}`.

## Read
- `issues_list {project?, mine?, status?, label?, all?}` — rows from the catalog
  cache (fast, no issue-doc loads). `all` includes done/tombstoned.
- `issues_board {project}` — workflow columns × ordered rows.
- `issues_view {reff}` — the full issue: body, comments, metadata.
- `issues_history {reff}` — the issue's derived activity feed.
- `issues_activity {since}` — space-wide recent transitions; pass back `last` to
  follow.
- `issues_inbox` — what is addressed to *you*: assignments, comments on your work,
  mentions. Durable, so it survives restarts.

## Multi-node & E2EE (P2P)
Onboarding across nodes is one step: the host calls `invite_ticket` and shares it;
the other side calls `connect`. Space data is end-to-end encrypted, gated by a
signed membership graph — a joiner sees only ciphertext until an admin admits it:
- `member_add {who, admin?}` — seal the space key to a member (admin-only).
- `member_remove {who}` — revoke + rotate the key (lazy revocation; admin-only).
- `key_rotate` / `members` — rotate the key / list members and roles.
`who` is a presence snapshot; `status` shows the space + issue/project counts;
`doctor` reports the onboarding gates in order when a join is not settling.

Membership authority is the one thing a sponsored identity does not hold: adding
or removing members and rotating the key are refused for you even though writing
content is not.

There is no compare-and-swap: an edit always applies and merges (a CRDT). Read the
current state, act, and let it converge.
