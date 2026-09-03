#!/usr/bin/env bash
# Package the Roku receiver against a daemon and sideload it — and leave the
# tree exactly as git has it.
#
# Three files under receivers/roku are generated for a deployment and must
# never be committed in that state: `receiver-bootstrap.json` (the ship state
# trusts Web PKI at nixiesoftware.com; a private build pins the daemon's own
# certificate), `source/Build.brs` (the build tag the channel prints on
# launch) and `manifest` (its build_version). The pinned bootstrap was hand
# edited once and nearly committed. This script derives it from the daemon's
# own TLS file, so the bytes it pins are the bytes the daemon serves, and
# restores all three from git on every exit — success, failure or ^C.
#
# Usage:
#   ci/roku-deploy.sh [--roku IP] [--user rokudev] [--pass abcd] [--pin|--web]
#                     [--origin-ip IP] [--identity DIR] [--no-install]
#                     [--force] [--keep]
#
#   --pin         bootstrap pinned to the daemon's certificate (default when
#                 --roku is given); the origin is https://<lan-ip>:7443
#   --web         the ship bootstrap (default when --roku is not given)
#   --origin-ip   the LAN address receivers reach the daemon at; default is
#                 `ipconfig getifaddr en0`, then the default route's interface
#   --identity    the daemon's identity directory holding
#                 daemon/display/tls/coordinator-tls.json
#   --no-install  package only; no sideload, no console
#   --force       proceed although the generated files carry uncommitted
#                 changes (they are backed up, then overwritten and restored)
#   --keep        leave the generated files in place afterwards
#
# The Roku console on port 8085 is a single-client telnet stream and is not
# reliable; a build tag that does not show up within 30 s is reported as
# `unobserved`, which is not a failure.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
ROKU_DIR="$REPO/receivers/roku"
GENERATED=(receiver-bootstrap.json source/Build.brs manifest)
GUARDED=(receiver-bootstrap.json source/Build.brs)

ROKU=""
USER_NAME="rokudev"
PASS="abcd"
MODE=""
ORIGIN_IP=""
IDENTITY=""
INSTALL=1
FORCE=0
KEEP=0
STATE_DIR="${TMPDIR:-/tmp}"
STATE_DIR="${STATE_DIR%/}"

die() { echo "roku-deploy: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --roku) ROKU="$2"; shift 2 ;;
    --roku=*) ROKU="${1#--roku=}"; shift ;;
    --user) USER_NAME="$2"; shift 2 ;;
    --user=*) USER_NAME="${1#--user=}"; shift ;;
    --pass) PASS="$2"; shift 2 ;;
    --pass=*) PASS="${1#--pass=}"; shift ;;
    --pin) MODE=pin; shift ;;
    --web) MODE=web; shift ;;
    --origin-ip) ORIGIN_IP="$2"; shift 2 ;;
    --origin-ip=*) ORIGIN_IP="${1#--origin-ip=}"; shift ;;
    --identity) IDENTITY="$2"; shift 2 ;;
    --identity=*) IDENTITY="${1#--identity=}"; shift ;;
    --no-install) INSTALL=0; shift ;;
    --force) FORCE=1; shift ;;
    --keep) KEEP=1; shift ;;
    -h|--help) sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown flag $1" ;;
  esac
done

if [ -z "$MODE" ]; then
  if [ -n "$ROKU" ]; then MODE=pin; else MODE=web; fi
fi
if [ "$INSTALL" = 1 ] && [ -z "$ROKU" ]; then
  die "installing needs --roku IP (or pass --no-install to package only)"
fi
if [ -z "$IDENTITY" ]; then
  case "$(uname -s)" in
    Darwin) IDENTITY="$HOME/Library/Application Support/dev.nixi.lait" ;;
    Linux) IDENTITY="${XDG_CONFIG_HOME:-$HOME/.config}/lait" ;;
    *) IDENTITY="" ;;
  esac
fi
TLS_FILE="$IDENTITY/daemon/display/tls/coordinator-tls.json"

cd "$ROKU_DIR"

# ------------------------------------------------------------- (a) the guard

# Which generated files git tracks. An untracked one (Build.brs before the
# stamp script lands in git) cannot be restored with `git checkout`; if this
# run creates it, the restore deletes it, and if it was already there it is
# left alone and named.
TRACKED=()
UNTRACKED_BEFORE=()
for file in "${GENERATED[@]}"; do
  if git ls-files --error-unmatch -- "$file" >/dev/null 2>&1; then
    TRACKED+=("$file")
  elif [ -e "$file" ]; then
    UNTRACKED_BEFORE+=("$file")
  fi
done

dirty=()
for file in "${GUARDED[@]}"; do
  if git ls-files --error-unmatch -- "$file" >/dev/null 2>&1 \
    && [ -n "$(git status --porcelain -- "$file")" ]; then
    dirty+=("$file")
  fi
done
if [ "${#dirty[@]}" -gt 0 ]; then
  if [ "$FORCE" = 0 ]; then
    echo "roku-deploy: uncommitted changes in generated files:" >&2
    for file in "${dirty[@]}"; do echo "  receivers/roku/$file" >&2; done
    echo "these are written by this script and restored from git afterwards, which would discard the edit;" >&2
    echo "commit or 'git checkout --' them, or pass --force (a backup is taken)" >&2
    exit 1
  fi
  backup="$STATE_DIR/lait-roku-deploy.backup.$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$backup"
  for file in "${dirty[@]}"; do
    mkdir -p "$backup/$(dirname "$file")"
    cp "$file" "$backup/$file"
  done
  echo "--force: uncommitted ${dirty[*]} backed up under $backup; the restore will put git's version back"
fi

# --------------------------------------------------------- (f) the restore

RESTORED=0
restore() {
  local status=$?
  trap - EXIT INT TERM
  if [ -n "${CONSOLE_PID:-}" ] && kill -0 "$CONSOLE_PID" 2>/dev/null; then
    kill "$CONSOLE_PID" 2>/dev/null || true
  fi
  if [ "$RESTORED" = 1 ]; then exit "$status"; fi
  RESTORED=1
  if [ "$KEEP" = 1 ]; then
    echo "--keep: generated files left in place: ${GENERATED[*]}"
    exit "$status"
  fi
  if [ "${#TRACKED[@]}" -gt 0 ]; then
    git checkout -- "${TRACKED[@]}"
  fi
  for file in "${GENERATED[@]}"; do
    case " ${TRACKED[*]} ${UNTRACKED_BEFORE[*]} " in
      *" $file "*) continue ;;
    esac
    if [ -e "$file" ]; then
      rm -f "$file"
      echo "removed generated, untracked $file"
    fi
  done
  # Build.brs that was already there untracked (before the stamp script is
  # committed) is put back to its default by the stamp script's own restore,
  # which is what git would have done for a tracked one.
  case " ${UNTRACKED_BEFORE[*]} " in
    *" source/Build.brs "*)
      if [ -f scripts/stamp.mjs ]; then
        node scripts/stamp.mjs --restore >/dev/null && echo "restored source/Build.brs to its default"
      fi
      ;;
  esac
  local left=""
  for file in "${GENERATED[@]}"; do
    if [ -n "$(git status --porcelain -- "$file")" ]; then
      left="$left receivers/roku/$file"
    fi
  done
  if [ -z "$left" ]; then
    echo "restored from git: ${TRACKED[*]} — the tree is clean of generated files"
  else
    echo "generated files still differ from git:$left"
    if [ "${#UNTRACKED_BEFORE[@]}" -gt 0 ]; then
      echo "(untracked before this run and left alone: ${UNTRACKED_BEFORE[*]})"
    fi
  fi
  exit "$status"
}
trap restore EXIT INT TERM

# ------------------------------------------------------- (b) the bootstrap

lan_ip() {
  local ip=""
  if [ -n "$ORIGIN_IP" ]; then
    echo "$ORIGIN_IP"
    return
  fi
  if command -v ipconfig >/dev/null 2>&1; then
    ip="$(ipconfig getifaddr en0 2>/dev/null || true)"
    if [ -z "$ip" ]; then
      local iface
      iface="$(route -n get default 2>/dev/null | awk '/interface:/ { print $2 }' || true)"
      if [ -n "$iface" ]; then
        ip="$(ipconfig getifaddr "$iface" 2>/dev/null || true)"
      fi
    fi
  elif command -v ip >/dev/null 2>&1; then
    ip="$(ip route get 1.1.1.1 2>/dev/null | awk '/src/ { for (i = 1; i <= NF; i++) if ($i == "src") print $(i + 1) }' | head -n 1 || true)"
  fi
  echo "$ip"
}

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    sha256sum "$1" | cut -d ' ' -f 1
  fi
}

write_pinned_bootstrap() {
  command -v jq >/dev/null 2>&1 || die "jq is required to read the daemon's TLS file"
  [ -f "$TLS_FILE" ] || die "no daemon TLS file at $TLS_FILE — has a daemon with the display coordinator run under this identity?"
  local work der pem lan origin recorded sha
  work="$(mktemp -d)"
  der="$work/coordinator.der"
  pem="$work/coordinator.pem"
  # Only the certificate leaves the file. It also holds the private key, which
  # nothing here reads, prints or copies.
  jq -e '.certificate_der | type == "array" and length > 0' "$TLS_FILE" >/dev/null \
    || die "$TLS_FILE has no certificate_der byte array"
  # Byte-exact: a shell loop printing octal escapes drops every zero byte
  # (the first deploy shipped a 336-byte certificate from a 378-byte one
  # and the receiver's curl refused the file). Python writes the bytes as
  # they are, and openssl proves they parse as a certificate.
  python3 - "$TLS_FILE" "$der" <<'PYEOF'
import json, sys
tls = json.load(open(sys.argv[1]))
open(sys.argv[2], "wb").write(bytes(tls["certificate_der"]))
PYEOF
  openssl x509 -inform der -in "$der" -noout 2>/dev/null \
    || die "the daemon's certificate_der did not parse as a certificate"
  {
    echo "-----BEGIN CERTIFICATE-----"
    # `fold` leaves the last line without a newline; without this the END
    # marker lands on the same line as the last base64 and curl refuses the
    # file (its PEM loader wants the marker on a line of its own).
    openssl base64 -A -in "$der" | fold -w 64
    printf '\n'
    echo "-----END CERTIFICATE-----"
  } >"$pem"
  sha="$(sha256_of "$der")"
  local der_bytes
  der_bytes="$(wc -c <"$der" | tr -d ' ')"
  lan="$(lan_ip)"
  [ -n "$lan" ] || die "could not detect a LAN address; pass --origin-ip"
  origin="https://$lan:7443"
  recorded="$(jq -r '.origin // "absent"' "$TLS_FILE")"
  jq -n --indent 2 \
    --arg origin "$origin" \
    --arg sha "$sha" \
    --rawfile pem "$pem" \
    '{protocol_major: 1,
      trust: {kind: "pinned_certificate", origin: $origin, sha256: $sha},
      certificate_pem: $pem,
      rendezvous: null}' >receiver-bootstrap.json
  rm -rf "$work"
  echo "bootstrap: pinned_certificate"
  echo "  origin:      $origin"
  echo "  sha256(DER): $sha ($der_bytes bytes)"
  echo "  from:        $TLS_FILE"
  if [ "$recorded" != "$origin" ]; then
    echo "  note: the daemon's TLS file records its own origin as $recorded; the bootstrap says $origin"
  fi
}

write_web_bootstrap() {
  cat >receiver-bootstrap.json <<'EOF'
{
  "protocol_major": 1,
  "trust": {
    "kind": "web_pki_origin",
    "origin": "https://nixiesoftware.com"
  },
  "certificate_pem": null,
  "rendezvous": null
}
EOF
  echo "bootstrap: web_pki_origin https://nixiesoftware.com (the ship state)"
}

case "$MODE" in
  pin) write_pinned_bootstrap ;;
  web) write_web_bootstrap ;;
esac

# ------------------------------------------------------------ (c) the stamp

if [ -f scripts/stamp.mjs ]; then
  echo "stamp: node scripts/stamp.mjs"
  node scripts/stamp.mjs
  if [ -f source/Build.brs ]; then
    echo "  source/Build.brs: $(grep -m 1 -i 'build' source/Build.brs | sed 's/^[[:space:]]*//' || echo 'written')"
  fi
  echo "  manifest build_version: $(grep -m 1 '^build_version=' manifest | cut -d= -f2 || echo absent)"
else
  echo "stamp: receivers/roku/scripts/stamp.mjs is absent — skipped; the build tag is whatever the tree already says"
fi

# ---------------------------------------------------------- (d) the package

[ -x node_modules/.bin/bsc ] || die "node_modules/.bin/bsc is missing — run npm ci in receivers/roku first"
echo "package: npm run package:roku"
npm run --silent package:roku
ZIP="$ROKU_DIR/dist/astrolabe-roku.zip"
[ -f "$ZIP" ] || die "npm run package:roku produced no $ZIP"
echo "  zip:  $ZIP"
echo "  size: $(wc -c <"$ZIP" | tr -d ' ') bytes"

# ---------------------------------------------------------- (e) the install

if [ "$INSTALL" = 0 ]; then
  echo "install: skipped (--no-install)"
  exit 0
fi

CONSOLE_LOG="$STATE_DIR/lait-roku-console.$$.log"
CONSOLE_PID=""
if command -v nc >/dev/null 2>&1; then
  # Attached before the install so the launch that follows it is on the
  # stream. `-w 45` is an idle timeout; the reader is killed on exit anyway.
  : >"$CONSOLE_LOG"
  nc -w 45 "$ROKU" 8085 >"$CONSOLE_LOG" 2>/dev/null </dev/null &
  CONSOLE_PID=$!
else
  echo "console: nc is absent; the build tag will be unobserved"
fi

RESPONSE="$STATE_DIR/lait-roku-install.$$.html"
echo "install: POST http://$ROKU/plugin_install as $USER_NAME"
http_code="$(curl --silent --show-error --digest -u "$USER_NAME:$PASS" \
  -F mysubmit=Install -F "archive=@$ZIP" \
  -o "$RESPONSE" -w '%{http_code}' \
  "http://$ROKU/plugin_install" || echo "000")"
install_text="$(sed -e 's/<[^>]*>/ /g' "$RESPONSE" 2>/dev/null | tr -s ' \n\r\t' ' ' || true)"
if grep -q "Install Success" "$RESPONSE" 2>/dev/null; then
  echo "  result: Install Success (HTTP $http_code)"
elif grep -q "Identical to previous version" "$RESPONSE" 2>/dev/null; then
  echo "  result: Identical to previous version — the Roku kept what it had (HTTP $http_code)"
else
  messages="$(grep -o 'Roku\.Message\.Add[^;]*' "$RESPONSE" 2>/dev/null | sed 's/Roku\.Message\.Add//' || true)"
  echo "  result: failed (HTTP $http_code)"
  if [ -n "$messages" ]; then
    echo "$messages" | sed 's/^/    /'
  else
    echo "    ${install_text:0:400}"
  fi
  rm -f "$RESPONSE"
  exit 1
fi
rm -f "$RESPONSE"

# The console: an AppLaunchInitiate beacon, then the channel's own build line.
launch="unobserved"
tag="unobserved"
if [ -n "$CONSOLE_PID" ]; then
  waited=0
  while [ "$waited" -lt 300 ]; do
    if [ "$launch" = unobserved ] && grep -q "AppLaunchInitiate" "$CONSOLE_LOG" 2>/dev/null; then
      launch="observed after $((waited / 10)).$((waited % 10)) s"
    fi
    if [ "$launch" != unobserved ]; then
      line="$(sed -n '/AppLaunchInitiate/,$p' "$CONSOLE_LOG" | grep -m 1 '^\[astrolabe\] build ' || true)"
      if [ -n "$line" ]; then
        tag="${line#\[astrolabe\] build }"
        break
      fi
    fi
    if ! kill -0 "$CONSOLE_PID" 2>/dev/null; then
      break
    fi
    sleep 0.1
    waited=$((waited + 1))
  done
  kill "$CONSOLE_PID" 2>/dev/null || true
  CONSOLE_PID=""
fi
echo "console:"
echo "  AppLaunchInitiate: $launch"
echo "  build tag:         $tag"
if [ "$tag" = unobserved ] && [ -s "${CONSOLE_LOG:-/nonexistent}" ]; then
  echo "  last console lines ($CONSOLE_LOG):"
  tail -n 5 "$CONSOLE_LOG" | sed 's/^/    | /'
fi
rm -f "$CONSOLE_LOG"
