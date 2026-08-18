#!/usr/bin/env bash
# Error-context ratchet for the convergence path.
#
# `map_err(|_| SomeVariant)` at a seam is how four separate layers of this path
# each came to discard the one string an operator needed: mechanics named which
# of fourteen receipt fields failed to bind, and by the time the answer crossed
# `verify_transaction` -> `AuthorityUnverified` -> `Invalid::Binding` ->
# `Failure::Convergence`, it was one word. Three releases each peeled one layer
# before the pattern itself was named.
#
# This is a RATCHET, not a ban. Discarding is sometimes right — a timeout's
# `()` carries nothing, and an entropy failure needs no elaboration — so each
# file below has a budget equal to its audited count. Adding a new discard site
# pushes a file over budget and fails here, which is the moment to ask: does
# the error being dropped say something the surface above will want? If it
# genuinely does not, raise the budget in this file — in the same diff, where
# review can see it. If you CONVERT sites, lower the budget so the gain locks.
#
# The contact `Failure` enum itself is guarded harder than this script can:
# its diagnostic variants carry `String`, so a new bare construction does not
# compile. This ratchet covers the files feeding that enum, where the compiler
# has no such opinion.
set -euo pipefail

declare -A BUDGET=(
  [crates/runtime/src/contact_driver.rs]=33
  [crates/runtime/src/lifecycle.rs]=10
  [crates/replica/src/replica.rs]=93
  [crates/replica/src/transaction.rs]=6
  [crates/replica/src/manifest.rs]=10
  [src/orbital/mechanics.rs]=4
  [src/orbital/hosting.rs]=7
)

fail=0
for file in "${!BUDGET[@]}"; do
  count=$(grep -c 'map_err(|_|' "$file" || true)
  budget=${BUDGET[$file]}
  if (( count > budget )); then
    echo "::error::$file has $count 'map_err(|_| …)' sites (budget $budget)." \
         "A new one on the convergence path is how a diagnosable refusal" \
         "becomes one word. Carry the cause, or raise the budget in" \
         "ci/error-context-guard.sh where review can see it."
    fail=1
  elif (( count < budget )); then
    echo "::notice::$file is under its error-context budget ($count < $budget)" \
         "— lower the budget in ci/error-context-guard.sh to lock the gain."
  fi
done
exit $fail
