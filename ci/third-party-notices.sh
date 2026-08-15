#!/usr/bin/env bash
# The third-party notices: every crate whose code goes into a lait artifact,
# with the licence it is offered under, generated from the lockfile.
#
# ## Why this exists
#
# lait is offered as `MIT OR Apache-2.0`, and that offer is only meaningful if
# the closure under it is permissive too. `cargo deny check licenses` already
# fails the build on a denied licence — that is the gate. This is the *record*:
# the list a person who receives a lait binary is owed, naming what is in it and
# under what terms.
#
# The Astrolabe Plan (revision 6) commissions it in as many words: "third-party
# notices are generated from the lockfile, in tree, and drift fails the build".
# Generated rather than hand-maintained because a hand-maintained list is a list
# that silently stops matching; in tree rather than produced at release time
# because a notice file nobody can read before a release is a notice file nobody
# reviews.
#
# ## What is recorded
#
# Every package reachable from a workspace member through *normal* and *build*
# dependencies, for every target platform, excluding the workspace's own crates.
#
# - Dev-dependencies are excluded. Their code does not reach an artifact, and
#   including them would overstate what a person is receiving.
# - Build-dependencies are included. Their code does not ship either, but it
#   runs to produce what does, and a notice list that omitted a proc macro would
#   be making a distinction its readers did not ask for.
# - Every platform, not this one. `cargo metadata` resolves the whole graph
#   unless told otherwise, which is what makes this file identical on Linux,
#   macOS and Windows — unlike `ci/coverage-manifest.txt`, which cannot be
#   regenerated off a unix host. Regenerate this one wherever you are.
#
# ## Usage
#
#   bash ci/third-party-notices.sh --check    # CI: regenerate and diff
#   bash ci/third-party-notices.sh --update   # accept the new closure
set -euo pipefail

NOTICES="THIRD-PARTY-NOTICES.md"
MODE="${1:---check}"

# Written through `sys.stdout.buffer`, which is binary and performs no newline
# translation. Python's `print` emits CRLF on Windows, and this file is diffed
# byte-for-byte on Linux CI — a Windows regeneration would differ on every line
# at once and report drift where there is none.
generate() {
  cargo metadata --format-version 1 --all-features --locked | python3 -c '
import json, sys

meta = json.load(sys.stdin)
packages = {p["id"]: p for p in meta["packages"]}
members = set(meta["workspace_members"])
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

# Breadth-first from the workspace, over the edges whose code reaches an
# artifact. A `dep_kind` of null is an ordinary dependency; "build" runs to
# produce the artifact; "dev" does neither and is skipped.
seen = set()
frontier = list(members)
while frontier:
    current = frontier.pop()
    node = nodes.get(current)
    if node is None:
        continue
    for edge in node["deps"]:
        kinds = {k.get("kind") for k in edge.get("dep_kinds", [])}
        if not kinds - {"dev"}:
            continue
        if edge["pkg"] in seen:
            continue
        seen.add(edge["pkg"])
        frontier.append(edge["pkg"])

third_party = sorted(
    (packages[i] for i in seen if i not in members),
    key=lambda p: (p["name"].lower(), p["version"]),
)

licences = {}
for package in third_party:
    licence = package.get("license") or "(see the crate source)"
    licences[licence] = licences.get(licence, 0) + 1

out = []
out.append("# Third-party notices")
out.append("")
out.append(
    "lait is offered under `MIT OR Apache-2.0`. It is built from the crates below, "
    "each offered under its own terms. This file is generated from `Cargo.lock` by "
    "`ci/third-party-notices.sh` and CI fails when it stops matching — do not edit it "
    "by hand."
)
out.append("")
out.append(
    "Listed here is every crate reachable from this workspace through normal and "
    "build dependencies, on every target platform. Dev-dependencies are excluded: "
    "their code does not reach an artifact. The full text of each crate’s licence "
    "is distributed with that crate’s source at the version recorded below, and is "
    "reachable at the repository recorded beside it."
)
out.append("")
out.append(f"{len(third_party)} crates, under {len(licences)} distinct licence expressions.")
out.append("")
out.append("## Licence expressions in this closure")
out.append("")
out.append("| Licence | Crates |")
out.append("| --- | ---: |")
for licence, count in sorted(licences.items(), key=lambda kv: (-kv[1], kv[0])):
    out.append(f"| `{licence}` | {count} |")
out.append("")
out.append("## Crates")
out.append("")
out.append("| Crate | Version | Licence | Source |")
out.append("| --- | --- | --- | --- |")
for package in third_party:
    licence = package.get("license") or "(see the crate source)"
    repository = package.get("repository") or ""
    source = "<" + repository + ">" if repository else "—"
    # Percent formatting rather than an f-string: an f-string carrying a quoted
    # subscript needs Python 3.12, and this runs on whatever python3 a runner
    # or a developer happens to have.
    out.append(
        "| %s | %s | `%s` | %s |" % (package["name"], package["version"], licence, source)
    )
out.append("")

sys.stdout.buffer.write("\n".join(out).encode("utf-8"))
'
}

case "$MODE" in
  --update)
    generate >"$NOTICES"
    echo "wrote $NOTICES"
    ;;
  --check)
    actual="$(mktemp)"
    trap 'rm -f "$actual"' EXIT
    generate >"$actual"
    if diff -u "$NOTICES" "$actual" >/dev/null 2>&1; then
      echo "$NOTICES is current"
      exit 0
    fi
    echo "::error::$NOTICES is stale. Run: bash ci/third-party-notices.sh --update"
    diff -u "$NOTICES" "$actual" || true
    # Uploaded by the workflow, so a developer fixes this by taking the file
    # rather than by working out which dependency moved.
    cp "$actual" "$NOTICES.actual"
    exit 1
    ;;
  *)
    echo "usage: bash ci/third-party-notices.sh [--check|--update]" >&2
    exit 2
    ;;
esac
