#!/usr/bin/env bash
# DEPRECATED — audits the deprecated Flutter client's Dart closure.
#
# Tauri is the canonical interface, and its dependency closure is npm's,
# covered elsewhere. Nothing ships from `apps/astrolabe` any more, so there is
# no Dart closure in a released artifact for this to report on. Kept, unwired,
# only against the Flutter client ever being revived — and it cannot run today
# regardless, because that project's pinned `lit-ui` commit no longer resolves.
# See `apps/astrolabe/DEPRECATED.md`.
#
# The second dependency closure.
#
# `cargo deny` proves the Rust closure carries no copyleft. The Flutter
# interface added a Dart one, and an audit that covers only the first is an
# audit reporting on a closure it has not read.
#
# This resolves the app's package closure, finds each package's licence in the
# pub cache, and fails on the copyleft *class* rather than on one licence that
# happened to be current — which was revision 5's mistake, named in the Plan.
set -euo pipefail

cd "$(dirname "$0")/.."
APP=apps/astrolabe

# The class, not a list of today's offenders. `LGPL` is matched before `GPL`
# would swallow it, and every pattern is anchored loosely because licence files
# spell their own names inconsistently.
DENIED='AGPL|GNU AFFERO|LGPL|LESSER GENERAL PUBLIC|GPL|GNU GENERAL PUBLIC|SSPL|SERVER SIDE PUBLIC|CC-BY-NC|COMMONS CLAUSE'

if ! command -v flutter >/dev/null 2>&1; then
  echo "dart-licences: flutter is not installed." >&2
  exit 1
fi

# Windows puts the cache under LOCALAPPDATA rather than under $HOME, and this
# project's first-class target is Windows.
cache="${PUB_CACHE:-}"
if [ -z "$cache" ]; then
  for candidate in "$HOME/.pub-cache" "${LOCALAPPDATA:-}/Pub/Cache"; do
    if [ -n "$candidate" ] && [ -d "$candidate" ]; then
      cache="$candidate"
      break
    fi
  done
fi
if [ -z "$cache" ] || [ ! -d "$cache" ]; then
  echo "dart-licences: no pub cache at $cache." >&2
  exit 1
fi

# `--style=compact` lists every transitive package as `name version`.
packages=$( (cd "$APP" && flutter pub deps --style=compact --no-dev) \
  | grep -oP '^\s*[|\\+-]*\s*\K[a-z0-9_]+ [0-9][^\s]*' \
  | sort -u || true)

if [ -z "$packages" ]; then
  echo "dart-licences: resolved no packages, which cannot be right." >&2
  exit 1
fi

failed=0
checked=0
while read -r name version; do
  [ -z "$name" ] && continue
  # Path dependencies — covalence and its lints — are ours and are not in the
  # cache. They are covered by this repository's own licence, not by an audit
  # of somebody else's.
  dir=$(find "$cache/hosted" -maxdepth 2 -type d -name "$name-$version" 2>/dev/null | head -1)
  [ -z "$dir" ] && continue
  checked=$((checked + 1))
  licence=$(find "$dir" -maxdepth 1 -iname 'LICENSE*' -o -maxdepth 1 -iname 'COPYING*' | head -1)
  if [ -z "$licence" ]; then
    echo "dart-licences: $name $version carries no licence file." >&2
    failed=1
    continue
  fi
  if grep -qiE "$DENIED" "$licence"; then
    echo "dart-licences: $name $version is copyleft ($licence)." >&2
    failed=1
  fi
done <<<"$packages"

if [ "$failed" -ne 0 ]; then
  echo "dart-licences: the Dart closure would impose terms on top of lait's own offer." >&2
  exit 1
fi

echo "dart-licences: $checked hosted packages, none copyleft."
