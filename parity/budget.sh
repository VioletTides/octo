#!/usr/bin/env bash
# WSL's one memory budget for cargo builds and parity stacks, installed as ~/bin/cargo-slot.
# Port agents wrap every cargo build or test in it, and parity/run.sh wraps every parity stack
# it starts in WSL, so builds and stacks together never take more of WSL's 16 GB than it has.
#
#   cargo-slot CMD...              run CMD holding a cargo build's share (the default)
#   cargo-slot --stack CMD...      run CMD holding one parity stack's share (a polite taker, below)
#   cargo-slot --rust-stack CMD... the same for a stack of the Rust build, which takes less
#   cargo-slot --weight N [--polite] CMD...   run CMD holding N units
#   cargo-slot --status            who holds what, and who waits
#   cargo-slot --install           copy this script to ~/bin/cargo-slot
#
# The budget is TOTAL units of 256 MB (see the numbers below). A cargo build takes CARGO
# units, a parity stack STACK units (RUST_STACK for a stack of the Rust build). A command that
# does not fit waits here until enough is free, and says so once on stderr. Builds wait in
# line. A parity stack is a polite taker: it takes its share only while a build's share stays
# free beside it, and waits out of line, so stacks (which hold their share for up to an hour)
# never take the last build's share, and a waiting stack never stands in line ahead of a build.
#
# How it holds a share: each unit is a file in /tmp/octo-budget, and holding a unit is holding
# a flock on it. The units are taken on open files that CMD and everything it starts inherit,
# so the share is held until the last of them exits, and the kernel lets go of it when a holder
# dies, however it dies (a killed holder frees its share; nothing to clean up). Only one taker
# gathers units at a time (the flock on /tmp/octo-budget/gate, which the others wait on in
# line), so two takers never each hold half of what they need and wait for each other.
#
# The budget is sized from measurements on 2026-10-08 (parity/README.md, "WSL's budget"):
# WSL has 15.6 GB; about 1.1 GB goes to the system, dockerd and the shells, and 2 GB stays free
# as headroom. The rest is the budget: 12.5 GB.
set -uo pipefail

DIR="${OCTO_BUDGET_DIR:-/tmp/octo-budget}"
UNIT_MB=256
TOTAL="${OCTO_BUDGET_UNITS:-50}" # 12.5 GB
CARGO=14                         # 3.5 GB: a test rebuild peaks at 2.3 to 3.4 GB, the musl release build at 3.4 GB
STACK=5                          # 1.25 GB: a C# stack peaks at 1.2 GB (0.8 GB of it not page cache)
RUST_STACK=3                     # 0.75 GB: a stack of the Rust build peaks at 0.1 to 0.3 GB (its two Octos are small)
# The flat slots of the old cargo-slot. A build still holding one when this version was put in
# place counts as a cargo build until it ends.
read -ra LEGACY_SLOTS <<< "${OCTO_BUDGET_LEGACY-/tmp/cargo-slot-1 /tmp/cargo-slot-2 /tmp/cargo-slot-3}"

gb() { awk -v u="$1" -v m="$UNIT_MB" 'BEGIN { printf "%.2f GB", u * m / 1024 }'; }

# How many old-style slots are held right now (taking one for a moment to see is harmless: an
# old cargo-slot that finds it busy just tries the next one).
legacy_busy() {
  local f n=0 fd
  for f in "${LEGACY_SLOTS[@]}"; do
    [ -e "$f" ] || continue
    exec {fd}>> "$f" 2> /dev/null || continue
    if ! flock -n "$fd"; then n=$((n + 1)); fi
    exec {fd}>&-
  done
  echo "$n"
}

# Take WANT units, then run the command with them. A polite taker (a parity stack) leaves a
# cargo build's share free besides and does not hold the line while it waits: it looks every
# 5 s and takes its share only when that much is free, so a port agent's build never waits in
# line behind a parity stack, and one build always finds room.
take_and_run() { # want polite cmd...
  local want="$1" polite="$2" gate i fd reserve legacy waited=0 told=0 since need keep=0
  local -a held=() mine=()
  shift 2
  if [ "$polite" = 1 ]; then keep="$CARGO"; fi
  if [ $((want + keep)) -gt "$TOTAL" ]; then
    echo "cargo-slot: $want units is more than the budget has room for ($TOTAL, $keep kept free)" >&2
    exit 64
  fi
  mkdir -p "$DIR" && chmod 1777 "$DIR" 2> /dev/null
  exec {gate}>> "$DIR/gate"
  since="$(date +%s)"
  while :; do
    # A taker gathering free units holds the gate for a moment; only a longer wait is told.
    if ! flock -w 3 "$gate"; then
      if [ "$waited" = 0 ]; then
        echo "cargo-slot: others wait for the budget before this; waiting in line ($want units, $(gb "$want"))" >&2
      fi
      waited=1
      flock "$gate"
    fi
    # First in line: gather units as they come free. Only the holder of the gate gathers, so
    # what it holds is never wanted by another taker.
    while :; do
      legacy="$(legacy_busy)"
      reserve=$((legacy * CARGO))
      need=$((want + keep + reserve))
      for ((i = 1; i <= TOTAL && ${#held[@]} < need; i++)); do
        [ -n "${mine[$i]:-}" ] && continue
        exec {fd}>> "$DIR/unit-$i"
        if flock -n "$fd"; then
          held+=("$fd")
          mine[i]="$fd"
        else
          exec {fd}>&-
        fi
      done
      if [ "${#held[@]}" -ge "$need" ]; then break 2; fi
      if [ "$told" = 0 ]; then
        echo "cargo-slot: the budget is full ($((TOTAL - ${#held[@]})) of $TOTAL units held by others, $legacy old-style builds$([ "$polite" = 1 ] && echo ", $keep kept free for a build")); waiting for $want units ($(gb "$want"))" >&2
        told=1
        waited=1
      fi
      if [ "$polite" = 1 ]; then
        # Let go of everything, the line included, and look again in a while.
        for fd in "${held[@]}"; do exec {fd}>&-; done
        held=()
        mine=()
        flock -u "$gate"
        sleep 5
        continue 2
      fi
      sleep 2
    done
  done
  # Units held only to see that room was left (for old-style builds, or a build beside a
  # stack) go back now.
  while [ "${#held[@]}" -gt "$want" ]; do
    fd="${held[-1]}"
    unset 'held[-1]'
    exec {fd}>&-
  done
  exec {gate}>&-
  if [ "$waited" != 0 ]; then
    echo "cargo-slot: got $want units after $(($(date +%s) - since)) s" >&2
  fi
  exec "$@"
}

ppid_of() {
  local s pp
  s="$(cat "/proc/$1/stat" 2> /dev/null)" || return 1
  read -r _ pp _ <<< "${s##*) }"
  echo "$pp"
}

# Whether an ancestor of PID is in the named map (children share their parent's open files).
under() { # pid map
  local -n m="$2"
  local p
  # A process gone by now (a short-lived child) counts as under its parent: not shown.
  p="$(ppid_of "$1")" || return 0
  while [ -n "$p" ] && [ "$p" -gt 1 ]; do
    if [ -n "${m[$p]:-}" ]; then return 0; fi
    p="$(ppid_of "$p")" || return 1
  done
  return 1
}

cmd_of() { tr '\0' ' ' 2> /dev/null < "/proc/$1/cmdline" | cut -c1-110; }

# Who holds units, who holds the gate (first in line) and who waits behind it.
status() {
  local path pid fd file age
  local -A units=() gate=() gate_locked=()
  while read -r path; do
    pid="${path#/proc/}"
    pid="${pid%%/*}"
    fd="${path##*/}"
    file="$(readlink "$path" 2> /dev/null)" || continue
    case "$file" in
      "$DIR"/unit-*)
        if grep -qs '^lock:.*FLOCK' "/proc/$pid/fdinfo/$fd"; then units[$pid]+=" ${file##*/unit-}"; fi
        ;;
      "$DIR"/gate)
        gate[$pid]=1
        if grep -qs '^lock:.*FLOCK' "/proc/$pid/fdinfo/$fd"; then gate_locked[$pid]=1; fi
        ;;
    esac
  done < <(find /proc/[0-9]*/fd -maxdepth 1 -lname "$DIR/*" 2> /dev/null)
  echo "WSL budget ($DIR): $TOTAL units of $UNIT_MB MB ($(gb "$TOTAL")); a cargo build takes $CARGO ($(gb "$CARGO")), a parity stack $STACK ($(gb "$STACK")), a Rust one $RUST_STACK ($(gb "$RUST_STACK"))"
  local used=0 n
  for pid in "${!units[@]}"; do
    # Only the topmost holder counts; its children hold the same units.
    if under "$pid" units; then continue; fi
    n="$(wc -w <<< "${units[$pid]}")"
    used=$((used + n))
    age="$(ps -o etimes= -p "$pid" 2> /dev/null | tr -d ' ')"
    age="${age:-0}"
    printf '  holds %2d units  %3dm%02ds  pid %-7s %s (in %s)\n' "$n" $((age / 60)) $((age % 60)) "$pid" \
      "$(cmd_of "$pid")" "$(readlink "/proc/$pid/cwd" 2> /dev/null)"
  done
  echo "  in use: $used of $TOTAL units ($(gb "$used")), free: $((TOTAL - used))"
  local legacy
  legacy="$(legacy_busy)"
  if [ "$legacy" -gt 0 ]; then echo "  old-style cargo-slot builds still running: $legacy (each counts as $CARGO units)"; fi
  for pid in "${!gate[@]}"; do
    if under "$pid" gate; then continue; fi
    age="$(ps -o etimes= -p "$pid" 2> /dev/null | tr -d ' ')"
    age="${age:-0}"
    if [ -n "${gate_locked[$pid]:-}" ]; then
      printf '  first in line  %3dm%02ds  pid %-7s %s\n' $((age / 60)) $((age % 60)) "$pid" "$(cmd_of "$pid")"
    else
      printf '  waiting        %3dm%02ds  pid %-7s %s\n' $((age / 60)) $((age % 60)) "$pid" "$(cmd_of "$pid")"
    fi
  done
}

want="$CARGO"
polite=0
case "${1:-}" in
  "") echo "usage: cargo-slot [--stack | --rust-stack | --weight N [--polite]] CMD... (or --status, --install)" >&2; exit 64 ;;
  --status) status; exit 0 ;;
  --install)
    mkdir -p "$HOME/bin"
    install -m 755 "$(readlink -f "$0")" "$HOME/bin/cargo-slot.new"
    mv -f "$HOME/bin/cargo-slot.new" "$HOME/bin/cargo-slot"
    echo "installed $HOME/bin/cargo-slot"
    exit 0
    ;;
  --units) echo "$TOTAL $CARGO $STACK $UNIT_MB $RUST_STACK"; exit 0 ;;
esac
while [ $# -gt 0 ]; do
  case "$1" in
    --stack) want="$STACK"; polite=1 ;;
    --rust-stack) want="$RUST_STACK"; polite=1 ;;
    --cargo) want="$CARGO" ;;
    --polite) polite=1 ;;
    --weight)
      case "${2:-}" in
        "" | *[!0-9]*) echo "cargo-slot: --weight takes a number of units" >&2; exit 64 ;;
      esac
      want="$2"
      shift
      ;;
    --) shift; break ;;
    *) break ;;
  esac
  shift
done
if [ $# = 0 ]; then echo "cargo-slot: no command to run" >&2; exit 64; fi
take_and_run "$want" "$polite" "$@"
