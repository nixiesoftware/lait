#!/usr/bin/env bash
# Emit the cargo target flags that build exactly the OS-seam tier.
#
# `[profile.pr-platform]` in .config/nextest.toml is the authoritative
# definition of that tier, and a developer can reproduce CI with a bare
# `cargo nextest run --profile pr-platform`. But a filterset only decides what
# RUNS; cargo still builds every test target in the workspace, and on a Windows
# runner linking eighty test binaries against iroh + loro + frost is most of
# the job's wall clock — 6.6 min of a 19 min job, measured.
#
# So the workflow additionally passes cargo target flags to restrict the BUILD
# to the seam (twenty binaries, not eighty). Those flags must name the same
# binaries the profile selects, and the obvious way to arrange that — writing
# the list out twice — is the failure this repository has already paid for
# once: a selector list that drifted from the tests it named, discovered when
# five gates went red on a refactor.
#
# So the list is written once, in nextest.toml, and this derives the flags from
# it. If the profile gains a binary, the build follows automatically.
#
# Usage:  cargo nextest run --profile pr-platform $(bash ci/platform-seam-targets.sh)
set -euo pipefail

CONFIG="${1:-.config/nextest.toml}"
[ -f "$CONFIG" ] || { echo "::error::no nextest config at $CONFIG" >&2; exit 1; }

# Slice out the [profile.pr-platform] block: from its header to the next
# top-level [table], exclusive. Everything this reads is inside that block, so
# a `binary(...)` in a neighbouring profile cannot leak into the flags.
block="$(awk '
  /^\[profile\.pr-platform\]/ { inblock = 1; next }
  inblock && /^\[/            { exit }
  inblock                     { print }
' "$CONFIG")"

[ -n "$block" ] || { echo "::error::[profile.pr-platform] not found in $CONFIG" >&2; exit 1; }

# `kind(lib)` in the filterset means every crate's unit tests, which is `--lib`.
# Anything else it names by binary is an integration target, which is `--test`.
flags=""
if printf '%s' "$block" | grep -q 'kind(lib)'; then
  flags="--lib"
fi

names="$(printf '%s' "$block" | grep -o 'binary([a-z0-9_]*)' | sed 's/binary(\(.*\))/\1/' | sort -u)"
for name in $names; do
  # `binary(mechanics)` and friends name a LIB target (the crate's own unit
  # tests), already covered by --lib; `--test mechanics` would not resolve.
  # Integration targets are the ones that are not also package names.
  case "$name" in
    comms|fabric|journal|mechanics|replica|runtime|lait|world-interface) continue ;;
  esac
  flags="$flags --test $name"
done

[ -n "$flags" ] || { echo "::error::derived no target flags from [profile.pr-platform]" >&2; exit 1; }
printf '%s\n' "$flags"
