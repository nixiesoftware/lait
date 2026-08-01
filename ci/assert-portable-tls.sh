#!/usr/bin/env bash
# Defense-in-depth beyond the cargo-deny ban: fail if the aws-lc-* rustls
# backend is actually LINKED on THIS platform (a backend swap can be per-target).
#
# We assert on real linkage, not mere Cargo.lock presence: `cargo tree -i
# <crate>` prints the reverse-dependency tree to STDOUT only when the crate is
# genuinely in the build graph. A crate that is only pinned in Cargo.lock as an
# inactive optional dependency (e.g. rustls records aws-lc-rs's version even
# when the `ring` provider is the one selected) yields empty stdout — its
# "nothing to print" note goes to stderr — and must pass, because nothing C is
# compiled or linked.
set -euo pipefail

bad=0
for crate in aws-lc-rs aws-lc-sys aws-lc-fips-sys; do
  if [ -n "$(cargo tree --locked -e no-dev -i "$crate" 2>/dev/null)" ]; then
    echo "::error::$crate is linked into the build — the build must use the portable 'ring' rustls backend."
    bad=1
  fi
done
exit "$bad"
