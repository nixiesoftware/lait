#!/usr/bin/env bash
# The coverage manifest: what this workspace tests, written down.
#
# ## Why this exists
#
# Twelve `orbital-*` jobs used to each re-run a named slice of the suite that
# the workspace job had already run minutes earlier. The comment above them was
# candid — "the full workspace suite in `check` subsumes these tests" — and so
# was the reason: naming them as separate required jobs meant a deleted or
# renamed gate turned a green aggregate red instead of silently narrowing
# coverage.
#
# That goal is right. Re-executing the tests is a very expensive way to reach
# it: twelve jobs, ~23 runner-minutes per push, to assert that some tests
# EXIST. And it was strictly weaker than it looked — the mechanism was
# nextest's exit-4 "filter matched nothing", so deleting one test from inside a
# module that still had others left every gate green.
#
# This asserts the same property directly, in about ten seconds, and catches
# the case the gates missed. `cargo nextest list` is authoritative about what
# would run; this records it and fails when the recording stops matching.
#
# ## What is recorded
#
#   [tiers]   how many tests each tier profile selects. A tier that silently
#             stops covering something moves its count.
#   [gates]   how many tests each named release gate selects. A filter that
#             matches nothing fails the generator outright, as before.
#   [tests]   every test id in the workspace, sorted. This is the part with
#             teeth: a deleted test is a deleted line, visible in review, and
#             no count arithmetic can hide it.
#
# ## The manifest is a LINUX artifact
#
# Some tests do not exist on every platform — `#[cfg(unix)]` covers four of
# them here, including the one that checks a Unix socket path fits in
# sun_path. So "every test id in the workspace" is a different set on Windows
# than on Linux, and a manifest regenerated on the wrong OS reports coverage
# loss that is really just cfg.
#
# Linux is the canonical platform because it is where the `pr` tier — the whole
# suite — runs. Regenerating elsewhere is refused rather than silently wrong.
# When CI finds the manifest stale it uploads the corrected file as an
# artifact, so a developer on another OS fixes it by downloading rather than by
# finding a Linux box.
#
# ## Usage
#
#   bash ci/coverage-manifest.sh --check    # CI: regenerate and diff
#   bash ci/coverage-manifest.sh --update   # Linux: accept the new coverage
set -euo pipefail

MANIFEST="ci/coverage-manifest.txt"
MODE="${1:---check}"

case "$(uname -s)" in
  Linux) CANONICAL=1 ;;
  *)     CANONICAL=0 ;;
esac

# The named release gates. Each is a claim the docket makes about the
# substrate, and each must keep selecting tests. Kept here rather than in the
# workflow so the filters and the recording of what they select live together.
GATES=(
  "orbital-boundaries|binary(orbital_boundaries)"
  "orbital-clean-break|binary(orbital_clean_break) + binary(semantic_type_names) + binary(mixed_root_guard)"
  "orbital-authority|binary(authority_history) + binary(world_policy) + (binary(mechanics) & (test(authority_checkpoint_tests) + test(frontier_isolation_tests)))"
  "orbital-formats|binary(product_schema) + binary(coordinates_fixtures) + binary(contact_fixtures) + binary(beacon_presence_fixtures) + binary(transaction_marker_fixtures) + binary(canonical_ids) + binary(algebra_fixtures) + (binary(runtime) & (test(internal_tests::dto_schema) + test(internal_tests::content_host))) + (binary(replica) & (test(manifest_fixture_tests) + test(canonical_store_tests) + test(content_fixture_tests) + test(content_plane_tests) + test(cache_tests))) + (binary(journal) & (test(index_tests) + test(reconciliation_tests)))"
  "core-fabric-foundations|binary(causal_evidence) + binary(history_growth) + binary(commit_cost_baseline) + binary(causal_contract) + binary(batch_atomicity) + binary(store_growth) + (binary(fabric) & (test(algebra_reservation_tests) + test(convergence_laws_tests)))"
  "core-plane-shapes|binary(plane_fixtures) + binary(transport_capabilities) + binary(flows) + (binary(runtime) & (test(internal_tests::budget_fixtures) + test(internal_tests::admission_fixtures) + test(internal_tests::freight_transfer) + test(internal_tests::freight_wire) + test(internal_tests::freight_two_node)))"
  "orbital-faults|binary(orbital_catalog) + binary(concurrent_heads) + (binary(journal) & (test(fault_tests) + test(crash_tests))) + (binary(replica) & test(manifest_atomicity_tests))"
  "orbital-contact-real|binary(contact_iroh) + binary(contact_mem)"
  "orbital-bootstrap-real|binary(orbital_join_iroh) + binary(orbital_admission) + binary(orbital_concurrent_catalog)"
  "orbital-ceremonies|binary(orbital_ceremonies) + (binary(mechanics) & test(sparse_ceremony_tests))"
  "orbital-independent-world|binary(independent_world) + binary(orbital_adoption)"
  "convergence-simulation|binary(runtime) & test(internal_tests::convergence_simulation)"
  "orbital-product-parity|binary(orbital_product_parity) + binary(issues_policy_designer) + binary(lait_daemon) + binary(control_classification) + binary(mcp_parity) + binary(viewer_parity)"
)

TIERS=(pr pr-platform nightly)

COMMON=(--workspace --locked --all-features --message-format json)

# Both helpers write through `sys.stdout.buffer`, which is binary and performs
# no newline translation. Python's `print` would emit CRLF on Windows, and this
# manifest is generated on whatever machine a developer has and then diffed
# byte-for-byte on Linux CI — a Windows regeneration would differ on every line
# at once and report "coverage changed" when nothing had. `.gitattributes`
# pins the committed file too; this is the half that makes `--check` correct
# locally rather than only after a round-trip through git.
emit() {
  python3 -c "$1"
}

# Count the testcases a filterset actually SELECTS. nextest's top-level
# `test-count` counts the whole inventory it walked, mismatches included, so
# reading that number would report the same value for every filter.
count_matches() {
  emit '
import json, sys
d = json.load(sys.stdin)
n = sum(
    1
    for suite in d.get("rust-suites", {}).values()
    for tc in suite.get("testcases", {}).values()
    if tc.get("filter-match", {}).get("status") == "matches"
)
sys.stdout.buffer.write(str(n).encode())'
}

list_test_ids() {
  emit '
import json, sys
d = json.load(sys.stdin)
ids = sorted(
    suite["binary-id"] + " " + name
    for suite in d.get("rust-suites", {}).values()
    for name in suite.get("testcases", {})
)
sys.stdout.buffer.write(("\n".join(ids) + "\n").encode())'
}

generate() {
  echo "# lait test coverage manifest — GENERATED, do not hand-edit."
  echo "#"
  echo "# Regenerate with: bash ci/coverage-manifest.sh --update"
  echo "#"
  echo "# A diff here is a coverage change. That is the point: it should be"
  echo "# reviewed like one, not discovered when a gate goes red on a refactor."
  echo

  # One inventory walk, reused for the master list.
  local inventory
  inventory="$(cargo nextest list "${COMMON[@]}" 2>/dev/null)"

  echo "[tiers]"
  for tier in "${TIERS[@]}"; do
    local n
    n="$(cargo nextest list "${COMMON[@]}" --profile "$tier" 2>/dev/null | count_matches)"
    [ "$n" -gt 0 ] || { echo "::error::tier '$tier' selects no tests" >&2; exit 1; }
    printf '%-14s %s\n' "$tier" "$n"
  done
  echo

  echo "[gates]"
  for entry in "${GATES[@]}"; do
    local name="${entry%%|*}"
    local filter="${entry#*|}"
    local n
    n="$(cargo nextest list "${COMMON[@]}" -E "$filter" 2>/dev/null | count_matches)"
    # A gate that matches nothing is the failure the twelve jobs existed to
    # catch. It stays a hard failure, just a ten-second one.
    [ "$n" -gt 0 ] || { echo "::error::gate '$name' selects no tests — its filter has gone stale" >&2; exit 1; }
    printf '%-28s %s\n' "$name" "$n"
  done
  echo

  echo "[tests]"
  printf '%s\n' "$inventory" | list_test_ids
}

case "$MODE" in
  --update)
    if [ "$CANONICAL" -ne 1 ]; then
      echo "::error::the coverage manifest is generated on Linux — see the header." >&2
      echo "  This machine is $(uname -s), where cfg(unix) tests do not exist, so" >&2
      echo "  regenerating here would record their absence as coverage loss." >&2
      echo "  Push and let CI produce the corrected file: the 'coverage manifest'" >&2
      echo "  job uploads it as an artifact when it finds a mismatch." >&2
      exit 1
    fi
    generate > "$MANIFEST.new"
    mv "$MANIFEST.new" "$MANIFEST"
    echo "wrote $MANIFEST"
    ;;
  --check)
    generate > "$MANIFEST.actual"
    if diff -u "$MANIFEST" "$MANIFEST.actual"; then
      rm -f "$MANIFEST.actual"
      echo "coverage manifest is current"
    else
      if [ "$CANONICAL" -ne 1 ]; then
        echo "::notice::this is $(uname -s); the manifest is generated on Linux, so a" >&2
        echo "  difference of only cfg(unix) tests is expected here and not a failure." >&2
      fi
      # Leave the .actual file in place: the CI job uploads it so the fix is a
      # download rather than a hunt for a Linux machine.
      echo "::error::coverage changed. If intended, run 'bash ci/coverage-manifest.sh --update' on Linux and commit the result, or take the artifact this job uploaded."
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 [--check|--update]" >&2
    exit 2
    ;;
esac
