#!/usr/bin/env bash
# The display dev loop: which daemon holds 7443, and swapping it for the one
# you just built.
#
# The display coordinator binds one fixed port, 7443, and whichever daemon got
# there first is the one every receiver on the LAN is talking to. That daemon is
# not always the one you think: Astrolabe starts its own from a device image
# under `~/.local/share/Astrolabe/devices/images/<id>/lait`, and a debug build
# started by hand loses the port to it (or the other way round) without either
# saying so. A night was lost to a receiver faithfully polling a binary nobody
# was editing. So this script prints facts about the holder — pid, executable,
# the version *that executable* reports, its mtime — and replaces it on request.
#
# Usage:
#   ci/display-dev.sh status
#   ci/display-dev.sh up   <binary> [--log PATH] [--dump DIR] [--wait-receiver]
#   ci/display-dev.sh down
#   ci/display-dev.sh swap <binary> [--log PATH] [--dump DIR] [--wait-receiver]
#
# `up` stops whatever holds the port (politely — it never `kill -9`s, and says
# so when that would be needed), records what it replaced, and starts
# `<binary> daemon` detached with display logging on. `down` stops the daemon
# this script started and tells you what was there before and how to get it
# back. `swap` is the two in one command. Nothing here relaunches Astrolabe.
#
# State lives in ${TMPDIR:-/tmp}:
#   lait-display-dev.pid       the daemon this script started
#   lait-display-dev.previous  the executable that held the port before `up`
#   lait-display-dev.log       the daemon's stdout+stderr (override: --log)
#
# Every line is a measurement. Where one cannot be taken it says `absent`,
# never `0` or a guess.

set -euo pipefail

PORT=7443
HOME_DIR=""
STATE_DIR="${TMPDIR:-/tmp}"
STATE_DIR="${STATE_DIR%/}"
PID_FILE="$STATE_DIR/lait-display-dev.pid"
PREV_FILE="$STATE_DIR/lait-display-dev.previous"
LOG_FILE="$STATE_DIR/lait-display-dev.log"
DUMP_DIR=""
WAIT_RECEIVER=0
ASTROLABE_IMAGES="$HOME/.local/share/Astrolabe/devices/images"

die() { echo "display-dev: $*" >&2; exit 1; }

usage() {
  sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

# ---------------------------------------------------------------- measurements

# Pids listening on the port, one per line. Empty when nobody is.
holder_pids() {
  lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null | sort -u || true
}

# The absolute executable of a pid, from the kernel's text mapping rather than
# argv[0] — `ps -o comm=` on macOS repeats what was typed, which for a daemon
# started as `target/debug/lait daemon` is a relative path that names nothing
# from any other directory.
exe_of() {
  local pid="$1" exe
  exe="$(lsof -p "$pid" -a -d txt -Fn 2>/dev/null | sed -n 's/^n//p' | head -n 1 || true)"
  if [ -z "$exe" ]; then
    exe="$(ps -o comm= -p "$pid" 2>/dev/null || true)"
  fi
  if [ -z "$exe" ]; then
    echo absent
  else
    echo "$exe"
  fi
}

command_of() {
  local cmd
  cmd="$(ps -o command= -p "$1" 2>/dev/null || true)"
  echo "${cmd:-absent}"
}

ppid_of() {
  local ppid
  ppid="$(ps -o ppid= -p "$1" 2>/dev/null | tr -d ' ' || true)"
  echo "${ppid:-absent}"
}

version_of() {
  local exe="$1" version
  if [ "$exe" = absent ] || [ ! -x "$exe" ]; then
    echo absent
    return
  fi
  version="$("$exe" --version 2>/dev/null | head -n 1 || true)"
  echo "${version:-absent}"
}

mtime_of() {
  local path="$1"
  if [ "$path" = absent ] || [ ! -e "$path" ]; then
    echo absent
    return
  fi
  if stat -f '%Sm' -t '%Y-%m-%d %H:%M:%S' "$path" 2>/dev/null; then
    return
  fi
  stat -c '%y' "$path" 2>/dev/null || echo absent
}

is_alive() { kill -0 "$1" 2>/dev/null; }

# Every `lait daemon` and `lait --world …` process, as "pid ppid".
lait_process_pids() {
  ps -axo pid=,ppid=,command= 2>/dev/null \
    | awk '$3 ~ /(^|\/)lait(\.exe)?$/ && ($4 == "daemon" || $4 == "--world") { print $1, $2 }' \
    || true
}

describe_process() {
  local pid="$1" exe
  exe="$(exe_of "$pid")"
  echo "  pid $pid  ppid $(ppid_of "$pid")"
  echo "    executable: $exe"
  echo "    command:    $(command_of "$pid")"
  echo "    version:    $(version_of "$exe")"
  echo "    mtime:      $(mtime_of "$exe")"
  case "$exe" in
    "$ASTROLABE_IMAGES"/*) echo "    origin:     Astrolabe device image" ;;
  esac
}

port_is_free() { [ -z "$(holder_pids)" ]; }

# ------------------------------------------------------------------ commands

cmd_status() {
  local pids started_by_us="absent"
  pids="$(holder_pids)"
  if [ -z "$pids" ]; then
    echo "port $PORT: free (no listener)"
  else
    for pid in $pids; do
      echo "port $PORT: held by pid $pid"
      describe_process "$pid"
      if [ -f "$PID_FILE" ] && [ "$(cat "$PID_FILE")" = "$pid" ]; then
        started_by_us="yes"
      else
        started_by_us="no"
      fi
      echo "    started by this script: $started_by_us"
    done
  fi
  if [ -f "$PREV_FILE" ]; then
    echo "previous holder (recorded by up): $(cat "$PREV_FILE")"
  else
    echo "previous holder (recorded by up): absent"
  fi
  if [ -f "$PID_FILE" ]; then
    local recorded
    recorded="$(cat "$PID_FILE")"
    if is_alive "$recorded"; then
      echo "pid file: $PID_FILE -> $recorded (alive)"
    else
      echo "pid file: $PID_FILE -> $recorded (not running; stale)"
    fi
  else
    echo "pid file: absent"
  fi
  echo "lait daemon / --world processes:"
  local found=0
  while read -r pid ppid; do
    [ -n "${pid:-}" ] || continue
    found=1
    describe_process "$pid"
  done <<<"$(lait_process_pids)"
  [ "$found" = 1 ] || echo "  absent"
  if pgrep -x astrolabe >/dev/null 2>&1 || pgrep -x Astrolabe >/dev/null 2>&1; then
    echo "Astrolabe client: running (pid $(pgrep -x astrolabe 2>/dev/null || pgrep -x Astrolabe))"
  else
    echo "Astrolabe client: not running"
  fi
}

# Stop a pid with TERM and wait for it to go. Returns 1 if it is still there.
stop_pid() {
  local pid="$1" what="$2" waited=0
  if ! is_alive "$pid"; then
    echo "$what (pid $pid) is not running"
    return 0
  fi
  echo "stopping $what (pid $pid) with SIGTERM"
  kill "$pid" 2>/dev/null || true
  while is_alive "$pid" && [ "$waited" -lt 100 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  if is_alive "$pid"; then
    echo "pid $pid is still running after 10 s"
    return 1
  fi
  echo "pid $pid stopped"
}

# Stop whatever holds the port, and any `lait --world` heads that came from the
# same executable (Astrolabe's heads are children of its image, and a head
# outliving its daemon is a head that answers for nothing).
stop_holder() {
  local pids pid exe
  pids="$(holder_pids)"
  [ -n "$pids" ] || return 0
  for pid in $pids; do
    exe="$(exe_of "$pid")"
    echo "port $PORT is held:"
    describe_process "$pid"
    echo "$exe" >"$PREV_FILE"
    echo "recorded previous holder in $PREV_FILE"
    if ! stop_pid "$pid" "the daemon"; then
      die "refusing to kill -9 pid $pid; stop it yourself (kill -9 $pid) and run again"
    fi
    while read -r hpid hppid; do
      [ -n "${hpid:-}" ] || continue
      [ "$hpid" != "$pid" ] || continue
      local hcmd hexe
      hcmd="$(command_of "$hpid")"
      hexe="$(exe_of "$hpid")"
      case "$hcmd" in
        *--world*) ;;
        *) continue ;;
      esac
      if [ "$hexe" = "$exe" ] || [ "$hppid" = "$pid" ]; then
        stop_pid "$hpid" "head ($hcmd)" \
          || echo "head pid $hpid did not stop; not forcing it"
      fi
    done <<<"$(lait_process_pids)"
  done
  local waited=0
  while ! port_is_free && [ "$waited" -lt 100 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  if ! port_is_free; then
    die "port $PORT is still held by pid(s) $(holder_pids | tr '\n' ' ')after 10 s; refusing to kill -9"
  fi
  echo "port $PORT is free"
  if [ -f "$PID_FILE" ] && [ "$(cat "$PID_FILE")" = "$pid" ]; then
    rm -f "$PID_FILE"
  fi
}

cmd_up() {
  local binary="${1:-}"
  [ -n "$binary" ] || die "up needs a binary: ci/display-dev.sh up <binary>"
  shift
  parse_up_flags "$@"
  [ -e "$binary" ] || die "$binary does not exist"
  [ -x "$binary" ] || die "$binary is not executable; refusing"
  [ -f "$binary" ] || die "$binary is not a file"
  binary="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"

  if ! port_is_free; then
    stop_holder
  else
    echo "port $PORT is free"
  fi

  local rust_log="info,lait::display=debug"
  if [ -n "${RUST_LOG:-}" ]; then
    rust_log="$rust_log,$RUST_LOG"
  fi
  : >"$LOG_FILE"
  echo "starting: $binary daemon"
  echo "  RUST_LOG=$rust_log"
  if [ -n "$DUMP_DIR" ]; then
    mkdir -p "$DUMP_DIR"
    echo "  LAIT_DISPLAY_DUMP_DIR=$DUMP_DIR"
    RUST_LOG="$rust_log" LAIT_DISPLAY_PORT="$PORT" LAIT_DISPLAY_DUMP_DIR="$DUMP_DIR" \
      nohup "$binary" daemon ${HOME_DIR:+--home "$HOME_DIR"} >"$LOG_FILE" 2>&1 </dev/null &
  else
    RUST_LOG="$rust_log" LAIT_DISPLAY_PORT="$PORT" \
      nohup "$binary" daemon ${HOME_DIR:+--home "$HOME_DIR"} >"$LOG_FILE" 2>&1 </dev/null &
  fi
  local pid=$!
  disown "$pid" 2>/dev/null || true
  echo "$pid" >"$PID_FILE"

  local waited=0 holder=""
  while [ "$waited" -lt 200 ]; do
    if ! is_alive "$pid"; then
      echo "the daemon (pid $pid) exited before listening; last lines of $LOG_FILE:"
      tail -n 20 "$LOG_FILE" | sed 's/^/  | /'
      rm -f "$PID_FILE"
      exit 1
    fi
    holder="$(holder_pids)"
    if [ -n "$holder" ]; then
      break
    fi
    sleep 0.1
    waited=$((waited + 1))
  done
  if [ -z "$holder" ]; then
    echo "pid $pid is running but nothing is listening on $PORT after 20 s; last lines of $LOG_FILE:"
    tail -n 20 "$LOG_FILE" | sed 's/^/  | /'
    exit 1
  fi
  if ! echo "$holder" | grep -qx "$pid"; then
    echo "warning: port $PORT is listened on by pid(s) $(echo "$holder" | tr '\n' ' '), not the daemon this script started ($pid)"
  fi
  echo "up:"
  echo "  pid:        $pid"
  echo "  executable: $binary"
  echo "  version:    $(version_of "$binary")"
  echo "  mtime:      $(mtime_of "$binary")"
  echo "  log:        $LOG_FILE"
  echo "  listening after: $((waited / 10)).$((waited % 10)) s"

  if [ "$WAIT_RECEIVER" = 1 ]; then
    wait_receiver
  fi
}

wait_receiver() {
  local waited=0
  echo "waiting for a receiver poll (a 'compiling display program' line in the log), up to 60 s"
  while [ "$waited" -lt 600 ]; do
    if grep -q "compiling display program" "$LOG_FILE" 2>/dev/null; then
      echo "receiver polled after $((waited / 10)).$((waited % 10)) s:"
      grep "compiling display program" "$LOG_FILE" | head -n 1 | sed 's/^/  | /'
      return 0
    fi
    sleep 0.1
    waited=$((waited + 1))
  done
  echo "no receiver poll observed in 60 s (unobserved, not necessarily absent)"
  return 0
}

cmd_down() {
  if [ ! -f "$PID_FILE" ]; then
    echo "no daemon started by this script (no $PID_FILE)"
  else
    local pid cmd
    pid="$(cat "$PID_FILE")"
    if ! is_alive "$pid"; then
      echo "the daemon this script started (pid $pid) is no longer running"
    else
      cmd="$(command_of "$pid")"
      case "$cmd" in
        *lait*daemon*)
          describe_process "$pid"
          stop_pid "$pid" "the daemon this script started" \
            || die "refusing to kill -9 pid $pid; stop it yourself (kill -9 $pid)"
          ;;
        *)
          echo "pid $pid is not a lait daemon any more ($cmd); not touching it"
          ;;
      esac
    fi
    rm -f "$PID_FILE"
  fi
  if [ -f "$PREV_FILE" ]; then
    local previous
    previous="$(cat "$PREV_FILE")"
    echo "before up, port $PORT was held by: $previous"
    case "$previous" in
      "$ASTROLABE_IMAGES"/*)
        echo "to get it back: relaunch Astrolabe, which starts its daemon from that device image (this script will not)"
        ;;
      *)
        echo "to get it back: start it again yourself ($previous daemon), or relaunch Astrolabe for its own daemon"
        ;;
    esac
  else
    echo "previous holder: absent (nothing recorded)"
  fi
  if port_is_free; then
    echo "port $PORT is free"
  else
    echo "port $PORT is still held by pid(s) $(holder_pids | tr '\n' ' ')"
  fi
}

parse_up_flags() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --log) [ $# -ge 2 ] || die "--log needs a path"; LOG_FILE="$2"; shift 2 ;;
      --port) [ $# -ge 2 ] || die "--port needs a number"; PORT="$2"; shift 2 ;;
      --home) [ $# -ge 2 ] || die "--home needs a directory"; HOME_DIR="$2"; shift 2 ;;
      --log=*) LOG_FILE="${1#--log=}"; shift ;;
      --dump) [ $# -ge 2 ] || die "--dump needs a directory"; DUMP_DIR="$2"; shift 2 ;;
      --dump=*) DUMP_DIR="${1#--dump=}"; shift ;;
      --wait-receiver) WAIT_RECEIVER=1; shift ;;
      -h|--help) usage 0 ;;
      *) die "unknown flag $1" ;;
    esac
  done
}

main() {
  local cmd="${1:-}"
  [ -n "$cmd" ] || usage 1
  shift
  command -v lsof >/dev/null 2>&1 || die "lsof is required"
  case "$cmd" in
    status) cmd_status ;;
    up) cmd_up "$@" ;;
    down) cmd_down ;;
    swap)
      echo "== down =="
      cmd_down
      echo "== up =="
      cmd_up "$@"
      ;;
    -h|--help|help) usage 0 ;;
    *) die "unknown command $cmd (status | up <binary> | down | swap <binary>)" ;;
  esac
}

main "$@"
