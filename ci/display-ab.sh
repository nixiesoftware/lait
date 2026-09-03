#!/usr/bin/env bash
# A/B the display coordinator: one commit against the binary you have.
#
# "Was it like this before?" took twenty minutes by hand — a worktree, a build,
# two swaps on port 7443, two probe runs, and a person holding the numbers in
# their head across all of it. This is that, as one command:
#
#   ci/display-ab.sh <commit-ish> [--seconds N] [--binary-b PATH]
#                    [--identity DIR] [--origin URL] [--keep-worktree] [--dump]
#
# A is `<commit-ish>`, built in a git worktree under ${TMPDIR:-/tmp}/lait-ab/<sha>
# (reused when it is already there). B is `--binary-b`, default the tree's own
# `target/debug/lait`. For each: `ci/display-dev.sh swap`, wait for a receiver
# to poll, run the headless probe (`tools/display-probe/probe.mjs`) for N
# seconds, and at the end print the two reports side by side. It finishes with
# B running, so the daemon left on the port is the one you are working on.
#
# When the probe is not in the tree yet the swaps still happen, each held for
# N seconds, so the comparison can be made by eye against a real receiver.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
STATE_DIR="${TMPDIR:-/tmp}"
STATE_DIR="${STATE_DIR%/}"
AB_ROOT="$STATE_DIR/lait-ab"
PROBE="$REPO/tools/display-probe/probe.mjs"
DEV="$REPO/ci/display-dev.sh"

COMMITISH=""
SECONDS_N=60
BINARY_B="$REPO/target/debug/lait"
IDENTITY=""
ORIGIN=""
KEEP_WORKTREE=0
DUMP=0

die() { echo "display-ab: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --seconds) SECONDS_N="$2"; shift 2 ;;
    --seconds=*) SECONDS_N="${1#--seconds=}"; shift ;;
    --binary-b) BINARY_B="$2"; shift 2 ;;
    --binary-b=*) BINARY_B="${1#--binary-b=}"; shift ;;
    --identity) IDENTITY="$2"; shift 2 ;;
    --identity=*) IDENTITY="${1#--identity=}"; shift ;;
    --origin) ORIGIN="$2"; shift 2 ;;
    --origin=*) ORIGIN="${1#--origin=}"; shift ;;
    --keep-worktree) KEEP_WORKTREE=1; shift ;;
    --dump) DUMP=1; shift ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown flag $1" ;;
    *)
      [ -z "$COMMITISH" ] || die "one commit-ish only (got $COMMITISH and $1)"
      COMMITISH="$1"; shift ;;
  esac
done
[ -n "$COMMITISH" ] || die "usage: ci/display-ab.sh <commit-ish> [--seconds N] [--binary-b PATH]"
case "$SECONDS_N" in
  ''|*[!0-9]*) die "--seconds needs a whole number, got '$SECONDS_N'" ;;
esac
[ -x "$DEV" ] || die "$DEV is missing or not executable"

if [ -z "$IDENTITY" ]; then
  case "$(uname -s)" in
    Darwin) IDENTITY="$HOME/Library/Application Support/dev.nixi.lait" ;;
    Linux) IDENTITY="${XDG_CONFIG_HOME:-$HOME/.config}/lait" ;;
    *) IDENTITY="" ;;
  esac
fi
SOCKET="$IDENTITY/daemon/control.sock"

if [ -z "$ORIGIN" ]; then
  lan=""
  if command -v ipconfig >/dev/null 2>&1; then
    lan="$(ipconfig getifaddr en0 2>/dev/null || true)"
  elif command -v ip >/dev/null 2>&1; then
    lan="$(ip route get 1.1.1.1 2>/dev/null | awk '{ for (i = 1; i <= NF; i++) if ($i == "src") print $(i + 1) }' | head -n 1 || true)"
  fi
  ORIGIN="https://${lan:-127.0.0.1}:7443"
fi

# ------------------------------------------------------------- A: the build

cd "$REPO"
SHA="$(git rev-parse --verify --quiet "${COMMITISH}^{commit}")" \
  || die "'$COMMITISH' does not name a commit"
SHORT="$(git rev-parse --short "$SHA")"
WORKTREE="$AB_ROOT/$SHA"
mkdir -p "$AB_ROOT"

if [ -d "$WORKTREE" ]; then
  if git worktree list --porcelain | grep -qx "worktree $WORKTREE"; then
    echo "worktree: reusing $WORKTREE"
  else
    die "$WORKTREE exists but is not a registered worktree; remove it or 'git worktree prune' and run again"
  fi
else
  git worktree prune
  echo "worktree: git worktree add --detach $WORKTREE $SHORT"
  git worktree add --detach "$WORKTREE" "$SHA"
fi
echo "A: building lait at $SHORT ($(git log -1 --format='%s' "$SHA"))"
(cd "$WORKTREE" && cargo build -p lait --bin lait --locked)
BINARY_A="$WORKTREE/target/debug/lait"
[ -x "$BINARY_A" ] || die "the build produced no $BINARY_A"

[ -x "$BINARY_B" ] || die "B binary $BINARY_B is missing or not executable"
BINARY_B="$(cd "$(dirname "$BINARY_B")" && pwd)/$(basename "$BINARY_B")"

# ------------------------------------------------------------- the two runs

STAMP="$(date +%Y%m%d-%H%M%S)"
REPORTS="$AB_ROOT/reports/$SHORT-$STAMP"
mkdir -p "$REPORTS"
echo "reports: $REPORTS"
echo "A: $BINARY_A ($("$BINARY_A" --version 2>/dev/null | head -n 1 || echo absent))"
echo "B: $BINARY_B ($("$BINARY_B" --version 2>/dev/null | head -n 1 || echo absent))"
if [ -f "$PROBE" ]; then
  echo "probe: node $PROBE --socket $SOCKET --origin $ORIGIN --seconds $SECONDS_N"
else
  echo "probe: $PROBE does not exist — swapping and holding each side for $SECONDS_N s so a person can compare by eye"
fi

run_side() {
  local side="$1" binary="$2"
  local dump_flag=()
  if [ "$DUMP" = 1 ]; then
    dump_flag=(--dump "$REPORTS/dump-$side")
  fi
  echo
  echo "===== $side: $binary ====="
  "$DEV" swap "$binary" --log "$REPORTS/daemon-$side.log" --wait-receiver ${dump_flag[@]+"${dump_flag[@]}"}
  if [ -f "$PROBE" ]; then
    if node "$PROBE" --socket "$SOCKET" --origin "$ORIGIN" \
      --seconds "$SECONDS_N" --out "$REPORTS/report-$side.json"; then
      echo "$side: report at $REPORTS/report-$side.json"
    else
      echo "$side: the probe exited non-zero; report-$side.json may be absent"
    fi
  else
    echo "$side: holding for $SECONDS_N s"
    sleep "$SECONDS_N"
  fi
}

run_side A "$BINARY_A"
run_side B "$BINARY_B"

# ------------------------------------------------------------ the comparison

echo
echo "===== A ($SHORT) vs B ====="
if [ -f "$REPORTS/report-A.json" ] || [ -f "$REPORTS/report-B.json" ]; then
  node - "$REPORTS/report-A.json" "$REPORTS/report-B.json" <<'EOF'
const fs = require("node:fs");
const [a, b] = process.argv.slice(2).map((p) => {
  try { return JSON.parse(fs.readFileSync(p, "utf8")); } catch { return null; }
});
// The probe's report shape belongs to the probe; these are the names asked
// for, each looked up along a few plausible paths, and `absent` when none
// of them is there. A key nobody wrote is not a zero.
const pick = (o, paths) => {
  if (!o) return "absent";
  for (const path of paths) {
    let v = o;
    for (const k of path.split(".")) v = v == null ? undefined : v[k];
    if (v === undefined || v === null) continue;
    if (Array.isArray(v)) return String(v.length);
    if (typeof v === "object") continue;
    return String(v);
  }
  return "absent";
};
const rows = [
  ["runway min",     ["runway.min", "runway_min", "runway.min_ms", "runwayMin"]],
  ["runway median",  ["runway.median", "runway_median", "runway.median_ms", "runway.p50", "runwayMedian"]],
  ["stalls",         ["stalls", "stalls.count", "stall_count", "stallCount"]],
  ["violations",     ["violations", "violations.count", "violation_count", "violationCount"]],
  ["health refused", ["health.refused", "health_refused", "healthRefused", "refused"]],
];
const w = 16;
console.log(`${"".padEnd(w)}${"A".padEnd(18)}B`);
for (const [label, paths] of rows) {
  console.log(`${label.padEnd(w)}${pick(a, paths).padEnd(18)}${pick(b, paths)}`);
}
for (const [name, o] of [["A", a], ["B", b]]) {
  console.log(`${name} top-level keys: ${o ? Object.keys(o).join(", ") : "absent (no report)"}`);
}
EOF
else
  echo "no reports (the probe did not run); compare the receiver by eye, logs are in $REPORTS"
fi
echo "daemon logs: $REPORTS/daemon-A.log, $REPORTS/daemon-B.log"
echo "running now: B ($BINARY_B)"

if [ "$KEEP_WORKTREE" = 1 ]; then
  echo "worktree kept: $WORKTREE"
else
  git worktree remove --force "$WORKTREE"
  echo "worktree removed: $WORKTREE (pass --keep-worktree to keep the build for the next run)"
fi
