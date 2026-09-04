#!/usr/bin/env bash
#
# Publish the foundation.pub/i join surface — the three same-origin files a
# shared `foundation.pub/i#join=<ticket>` link loads (see
# packaging/foundation/README.md, "The join surface"). This exists because the
# base path is a trap: the surface is served UNDER `/i`, so the bundle must be
# built `--base=/i/` or every asset reference points at the apex root and a
# real browser at foundation.pub loads a blank page (the worker never boots).
# A default `npm run build` — what the e2e stack and CI use, correctly, because
# they serve at root — produces exactly that broken bundle for `/i`. This
# script builds with the right base so the step cannot be gotten wrong by hand,
# then uploads and invalidates.
#
#   ci/publish-join-surface.sh                 # build + upload + invalidate
#   ci/publish-join-surface.sh --build-only    # just produce the /i bundle
#
# The committed products/issues-app/assets/web stays DEFAULT-base (the e2e
# stack serves it at root); this build is ephemeral and is left in place only
# for the upload, so run `(cd viewer && npm run build)` afterward if you need
# the default-base tree back for a local stack.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

BUCKET="gs://the-foundation-join/i"
BUCKET_HTTPS="https://storage.googleapis.com/the-foundation-join/i"
URL_MAP="url-map"
PROJECT="the-foundation-498604"
BUILD_ONLY=""
[ "${1:-}" = "--build-only" ] && BUILD_ONLY=1

echo "== building the viewer bundle with --base=/i/"
( cd "$root/viewer" && npm run build -- --base=/i/ )

# The engine wasm is produced by wasm-pack (its own workspace); the runner wasm
# is a release build. Both are gitignored build outputs — produced at publish
# time, not committed — so a publisher rebuilds them if absent.
engine="$root/crates/porthole/pkg/porthole_bg.wasm"
runner="$root/target/wasm32-unknown-unknown/release/lait_issues_runner.wasm"
[ -f "$engine" ] || { echo "missing $engine — run wasm-pack build --target web in crates/porthole"; exit 1; }
[ -f "$runner" ] || { echo "missing $runner — build the Issues runner for wasm32"; exit 1; }

if [ -n "$BUILD_ONLY" ]; then
    echo "built; --build-only, not uploading"
    exit 0
fi

echo "== uploading the bundle (short cache) and the immutable wasms (long cache)"
gcloud storage rsync "$root/products/issues-app/assets/web" "$BUCKET" \
    --cache-control="public, max-age=300"
gcloud storage cp "$engine" "$BUCKET/porthole_bg.wasm" \
    --cache-control="public, max-age=86400" --content-type=application/wasm
gcloud storage cp "$runner" "$BUCKET/lait_issues_runner.wasm" \
    --cache-control="public, max-age=86400" --content-type=application/wasm

echo "== invalidating the CDN for /i/*"
gcloud compute url-maps invalidate-cdn-cache "$URL_MAP" --path "/i/*" --project "$PROJECT" --async

echo "published. verify: curl -s https://foundation.pub/i/index.html | grep 'src=\"/i/app.js\"'"
echo "note: the committed assets tree is now --base=/i/; rebuild default-base for a local stack:"
echo "      (cd viewer && npm run build)"
