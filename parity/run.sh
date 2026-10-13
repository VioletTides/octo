#!/usr/bin/env bash
# The one command for parity runs. It runs the parity harness against the Rust build of the
# commit you have checked out and prints the diffs per backlog group, on one of two lanes:
#
#   wsl  WSL's own Docker, beside the port agents' builds. As many stacks at once as WSL's
#        memory budget allows (~/bin/cargo-slot, parity/budget.sh), so runs of several branches,
#        and the shards of one run, go side by side.
#   box  The test box, LXC 131, one run at a time (parity/lxc131.sh, unchanged).
#
#   parity/run.sh                      # compile here, record, compare with C# 2026.10.05
#   parity/run.sh --only '13-*'        # some corpus files (00-setup always runs; C# recorded again)
#   parity/run.sh --no-build           # same image as last time (corpus or stub changes only)
#   parity/run.sh --bytes              # also the bytes-mode diff
#   parity/run.sh --rust-twice         # also Rust against Rust (the determinism gate)
#   parity/run.sh --csharp-twice       # also C# against C# (after a corpus change)
#   parity/run.sh --csharp-only --csharp-twice   # corpus work: no Rust build or run
#   parity/run.sh --attach             # follow this branch's run again and fetch its reports
#   parity/run.sh --queue              # both lanes: the WSL budget and runs, and the box's queue
#   parity/run.sh --selftest [--sleep S]   # a dry run of the lane's own machinery (no build, no Docker)
#   parity/run.sh --clean              # remove this branch's image and folder (on the lane picked)
#   parity/run.sh --lane wsl|box ...   # pick the lane yourself
#   parity/run.sh --stop-run NAME      # stop the WSL run NAME and its stacks, as its watchdog would
#
# Without --lane it picks WSL when WSL has Docker, and the box when WSL has none, when you ask
# for a box build (--box-build, --low-memory), or when the WSL budget is full with stacks
# already waiting and the box is free. The box lane is parity/lxc131.sh with the same options.
#
# The WSL lane, like the box: commit first (the binary is built from the working tree, the run
# uses HEAD). It compiles octo here as a static musl binary (through cargo-slot), checks HEAD
# out into its own folder (~/.octo-parity/runs/<name>/src, so you can go on working, or switch
# branches, while it runs), copies this script beside it (runs/<name>/run.sh: the run's
# machinery is the script you started, the corpus and parity.py are the commit's) and starts
# the run there detached (setsid nohup): it outlives this script, and this script follows its
# log until it ends, then copies the reports to parity/.run/wsl-<name>/ and exits with the
# run's exit status (0: the run went through, diffs or not; 143: it was stopped). If this
# script is stopped, --attach follows the run again.
#
# The run: the image octo-rust-lane:<name> from Dockerfile's runtime stage around the
# binary; the corpus against the C# image (kept per corpus version and shard plan in
# ~/.octo-parity/cs-cache/, recorded again only when they change) and against the Rust image;
# then the diffs. Every recording is cut into the shards of parity/shards.json (parity.py run
# --shard; one shard, the whole corpus, while there is no such file or parity.py cannot) and
# every shard is a stack of its own, so the shards and the recordings (Rust, Rust again, C#
# again) all go at once as far as the budget allows. Each stack takes a stack's share of the
# budget for its life (a smaller one for the Rust build), and the shards are then joined
# (parity.py merge) into the one recording the diff reads. When parity.py run has
# --waits-from, the Rust recording of a shard waits for the C# one and adapts its waits to it.
#
# Each stack has its own compose project (octo-lane-<name>-<run id>-<recording>-<shard>), its
# own volumes, its own block of host ports from 21000 to 22999, taken for the stack's life
# under a lock (~/.octo-parity/ports/), and its own two networks on subnets that go with that
# block (10.213.<block>.0/24, made before the stack, since Docker's own pools hold only about 30
# networks in all), so stacks never meet. It never touches the box's projects (octo-parity-*),
# other compose projects or networks, or other images.
#
# A memory watchdog runs beside each WSL run (detached too): when WSL's MemAvailable falls
# under 1.5 GB it stops that run, its processes and its stacks, and nothing else. When several
# runs are going, the newest one is stopped first.
#
# Settings (environment): PARITY_NAME (default: the branch name), and for the box lane those
# of parity/lxc131.sh. Every WSL run keeps its things in ~/.octo-parity.
set -euo pipefail

ROOT="$HOME/.octo-parity"
CS_IMAGE=ghcr.io/winters27/octo:2026.10.05
SLOT="${PARITY_CARGO_SLOT:-$HOME/bin/cargo-slot}"
# This script, wherever it was started from; a run starts its own copy (runs/<name>/run.sh).
SELF="$(readlink -f "$0")"
# The reports a run writes into its folder (each with .summary and .groups.json beside it).
REPORTS=(report.txt report-bytes.txt cs-cs.txt cs-cs-bytes.txt rs-rs.txt rs-rs-bytes.txt)
# Host ports: blocks of 10 from 21000; a stack uses the first four (Octo, the second Octo's
# two, Navidrome).
PORT_BASE=21000
PORT_BLOCKS=200
WATCH_MB=1536
# Each stack's two networks get their subnets from 10.213.0.0/16 by its port block (block B:
# 10.213.B.0/25 for the sandbox, 10.213.B.128/25 for the edge), made before the stack and not
# from Docker's own address pools, which hold only about 30 networks for everyone in WSL.
LANE_NET=10.213
MIN_FREE_GB=8

check_name() {
  case "$1" in
    "" | .* | *[!a-z0-9._-]*) echo "bad run name '$1'" >&2; exit 64 ;;
  esac
}
# Seconds as 1h05m.
ago() { printf '%dh%02dm' $(($1 / 3600)) $(($1 % 3600 / 60)); }
say() { printf '\n== %s\n' "$*"; }
# A compose project name may not have dots.
safe() { printf '%s' "$1" | tr -c 'a-z0-9_-' '_'; }
# Whether the process is the supervisor of WSL run ID.
supervises() { # pid id
  local argv=()
  mapfile -d '' -t argv 2> /dev/null < "/proc/$1/cmdline" || return 1
  [ "${#argv[@]}" -ge 4 ] && [[ "${argv[1]}" == */run.sh ]] && [ "${argv[2]}" = --supervise ] && [ "${argv[3]}" = "$2" ]
}
elapsed() { # pid: seconds since it started
  local s
  s="$(ps -o etimes= -p "$1" 2> /dev/null | tr -d ' ')"
  echo "${s:-0}"
}

# NAME's latest WSL run, one line: STATE ID PID CODE ("-" for what is not known).
#   none     no WSL run of NAME
#   running  its supervisor is running (PID)
#   done     ended; CODE is its exit status
#   gone     its supervisor is not running and it wrote no done marker (killed)
run_status() { # name
  local run="$ROOT/runs/$1" id="" pid="" code="" state=none
  if [ -f "$run/current" ]; then
    id="$(cat "$run/current")"
    pid="$(sed -n 's/^pid=//p' "$run/run-$id.info" 2> /dev/null || true)"
    if [ -f "$run/run-$id.done" ]; then
      state=done
      code="$(cat "$run/run-$id.done")"
    elif [ -n "$pid" ] && supervises "$pid" "$id"; then
      state=running
    elif [ ! -f "$run/run-$id.info" ] && [ $(($(date +%s) - $(stat -c %Y "$run/current"))) -lt 20 ]; then
      state=running # just starting
    else
      state=gone
    fi
  fi
  echo "$state ${id:--} ${pid:--} ${code:--}"
}

# Every WSL run going now: NAME ID PID, oldest first.
live_runs() {
  local cur name id pid st
  for cur in "$ROOT"/runs/*/current; do
    [ -f "$cur" ] || continue
    name="$(basename "$(dirname "$cur")")"
    read -r st id pid _ <<< "$(run_status "$name")"
    if [ "$st" = running ]; then echo "$id $name $pid"; fi
  done | sort | awk '{print $2, $1, $3}'
}

# The WSL side of --queue.
show_wsl_queue() {
  local now name id pid st f tag what since code n=0
  now="$(date +%s)"
  echo "WSL lane ($ROOT, $(date -u '+%F %H:%M') UTC):"
  if budget_installed; then "$SLOT" --status | sed 's/^/  /'; else echo "  $SLOT is not the weighted budget: bash parity/budget.sh --install"; fi
  awk '/MemAvailable/ {printf "  WSL MemAvailable: %d MB (the watchdog stops a run under %d MB)\n", $2 / 1024, '"$WATCH_MB"'}' /proc/meminfo
  while read -r name id pid; do
    [ -n "$name" ] || continue
    n=$((n + 1))
    echo "  run $name $id (supervisor pid $pid), going for $(ago "$(elapsed "$pid")"):"
    for f in "$ROOT/runs/$name/out/jobs/"*.state; do
      [ -f "$f" ] || continue
      tag="$(basename "$f" .state)"
      read -r what since code < "$f" || continue
      case "$what" in
        after-cs) echo "    $tag: waiting for the C# recording of its shard for $(ago $((now - since)))" ;;
        waiting) echo "    $tag: waiting for the budget for $(ago $((now - since)))" ;;
        recording) echo "    $tag: recording for $(ago $((now - since))) (project $code)" ;;
        done) echo "    $tag: recorded (exit $since)" ;;
        selftest) echo "    $tag: selftest stack for $(ago $((now - since)))" ;;
      esac
    done
  done < <(live_runs)
  if [ "$n" = 0 ]; then echo "  no WSL runs going"; fi
}

# ---- Stop a WSL run: everything in its session but the supervisor (which then writes the done
# marker), and its stacks. For the watchdog; also by hand.
stop_run() { # name [--why TEXT]
  local name="$1" why="stopped by hand" st id pid p projects
  shift
  while [ $# -gt 0 ]; do
    case "$1" in
      --why) why="$2"; shift ;;
      *) echo "unknown option $1" >&2; exit 64 ;;
    esac
    shift
  done
  check_name "$name"
  read -r st id pid _ <<< "$(run_status "$name")"
  # A run whose supervisor was killed ("gone") may still have its processes and stacks.
  if { [ "$st" != running ] && [ "$st" != gone ]; } || [ "$pid" = - ]; then
    echo "no WSL run of $name is going; nothing stopped"
    return 0
  fi
  local line="STOP: $why; stopping the WSL run $name $id (supervisor pid $pid) and its stacks"
  echo "$line"
  printf '\n%s\n' "$line" >> "$ROOT/runs/$name/run-$id.log"
  # The supervisor started the run with setsid: its session is the run. Everything in it but
  # the supervisor, looked up again every second, since a process can start another between
  # the look and the kill: TERM for up to 10 s, then KILL.
  session_rest() { ps -e -o pid=,sid= | awk -v s="$pid" '$2 == s && $1 != s {print $1}'; }
  local targets=() round sig=TERM
  for round in $(seq 15); do
    mapfile -t targets < <(session_rest)
    [ "${#targets[@]}" = 0 ] && break
    if [ "$round" -gt 10 ]; then sig=KILL; fi
    kill "-$sig" "${targets[@]}" 2> /dev/null || true
    sleep 1
  done
  down_projects "octo-lane-$(safe "$name")-$id-"
  echo "stopped"
}

# The lane's stacks that have anything left in Docker, one name a line: from containers, compose
# networks and volumes (a stack whose up failed half way may have only a network), and the
# networks the lane made itself (make_networks).
lane_stacks() {
  {
    docker ps -a --format '{{.Label "com.docker.compose.project"}}'
    docker network ls --format '{{.Label "com.docker.compose.project"}}'
    docker volume ls --format '{{.Label "com.docker.compose.project"}}'
    docker network ls --filter label=octo-lane.stack --format '{{.Label "octo-lane.stack"}}'
  } 2> /dev/null | grep '^octo-lane-' | sort -u
}

# Take a stack down: its containers, networks and volumes (never images), and the networks the
# lane made for it.
down_project() { # project
  local f net
  docker compose -p "$1" down -v --remove-orphans > /dev/null 2>&1 || true
  docker volume ls -q --filter "label=com.docker.compose.project=$1" 2> /dev/null | xargs -r docker volume rm > /dev/null 2>&1 || true
  for f in "com.docker.compose.project=$1" "octo-lane.stack=$1"; do
    for net in $(docker network ls -q --filter "label=$f" 2> /dev/null); do
      docker network rm "$net" > /dev/null 2>&1 || true
    done
  done
}

# Take down every stack whose name starts with PREFIX.
down_projects() { # prefix
  local p
  for p in $(lane_stacks | grep -F -- "$1"); do
    [[ "$p" == "$1"* ]] || continue
    down_project "$p"
    echo "  took down stack $p"
  done
}

# The networks of the stack PROJECT on port block B. Compose takes them as the project's own
# (by name and the com.docker.compose.network label) but does not remove them (they carry no
# project label), so parity.py's own down before up leaves them; the job removes them after.
make_networks() { # project block
  local p="$1" b="$2" try net
  for try in 1 2; do
    if docker network create --internal --subnet "$LANE_NET.$b.0/25" --label "octo-lane.stack=$p" \
      --label com.docker.compose.network=sandbox "${p}_sandbox" > /dev/null &&
      docker network create --subnet "$LANE_NET.$b.128/25" --label "octo-lane.stack=$p" \
        --label com.docker.compose.network=edge "${p}_edge" > /dev/null; then
      return 0
    fi
    docker network rm "${p}_sandbox" "${p}_edge" > /dev/null 2>&1 || true
    # This job holds block B, so a lane network on its subnets is a killed run's: remove it.
    for net in $(docker network ls -q --filter label=octo-lane.stack); do
      if docker network inspect -f '{{range .IPAM.Config}}{{.Subnet}} {{end}}' "$net" 2> /dev/null | grep -q "^$LANE_NET\.$b\.\| $LANE_NET\.$b\."; then
        docker network rm "$net" > /dev/null 2>&1 || true
      fi
    done
  done
  return 1
}

# Stacks of WSL runs that are not going any more (their supervisor was killed, or their up
# failed half way): down.
reap_stacks() {
  local p live n id
  for p in $(lane_stacks); do
    live=0
    while read -r n id _; do
      [ -n "$n" ] || continue
      case "$p" in "octo-lane-$(safe "$n")-$id-"*) live=1 ;; esac
    done < <(live_runs)
    if [ "$live" = 0 ]; then
      echo "a stack left by a stopped run: $p; taking it down"
      down_project "$p"
    fi
  done
}

# ---- The WSL memory watchdog of run ID: started detached by the supervisor.
watchdog() { # name id [--selftest]
  local name="$1" id="$2" mode="${3:-}" limit="$WATCH_MB" free run
  run="$ROOT/runs/$name"
  if [ "$mode" = --selftest ]; then limit="${PARITY_WATCHDOG_MB:-$WATCH_MB}"; fi
  wsay() { printf '%s wsl watchdog %s %s: %s\n' "$(date '+%F %T')" "$name" "$id" "$*"; }
  free_mb() { awk '/MemAvailable/ {print int($2 / 1024)}' /proc/meminfo; }
  wsay "watching (pid $$): this run is stopped when WSL has under $limit MB available; now $(free_mb) MB"
  while :; do
    # Every 10 s, the first time 10 s after the start (what WSL had then is not this run's doing).
    sleep 10
    if [ -e "$run/run-$id.done" ]; then
      wsay "the run wrote its done marker; exiting"
      return 0
    fi
    local st rid
    read -r st rid _ <<< "$(run_status "$name")"
    if [ "$rid" != "$id" ] || [ "$st" != running ]; then
      sleep 2
      [ -e "$run/run-$id.done" ] || wsay "the run has gone without a done marker; exiting"
      return 0
    fi
    free="$(free_mb)"
    if [ "$free" -lt "$limit" ]; then
      # The newest run goes first: when a run started after this one is going, give its own
      # watchdog the time to stop it, and look again.
      local newer=0 n i _p
      while read -r n i _p; do
        if [[ "$i" > "$id" ]]; then newer=1; fi
      done < <(live_runs)
      if [ "$newer" = 1 ]; then
        wsay "WSL is down to $free MB available; a newer run goes first"
        sleep 25
        free="$(free_mb)"
      fi
      if [ "$free" -lt "$limit" ]; then
        wsay "WSL is down to $free MB available; stopping this run"
        bash "$SELF" --stop-run "$name" --why "WSL was down to $free MB available" 2>&1 | sed 's/^/  /'
        sleep 5
        continue
      fi
    fi
  done
}

# ---- The detached run's supervisor: run the body, then write the done marker.
supervise() { # id name options...
  local id="$1" name="$2" run status=0 start wflag=""
  shift
  run="$ROOT/runs/$name"
  start="$(date +%s)"
  printf 'pid=%s\nqueued=%s\nargs=%s\nlane=wsl\n' "$$" "$start" "$*" > "$run/run-$id.info"
  echo "wsl: run $name $id, detached (supervisor pid $$), log $run/run-$id.log"
  for a in "$@"; do
    if [ "$a" = --selftest ]; then wflag=--selftest; fi
  done
  setsid -f nohup bash "$SELF" --watchdog "$name" "$id" $wflag >> "$run/watchdog.log" 2>&1 < /dev/null
  echo "wsl watchdog: started, log $run/watchdog.log"
  PARITY_RUN_ID="$id" bash "$SELF" --body "$@" || status=$?
  printf '\n== run.sh ended with exit %s, %s after it started\n' "$status" "$(ago $(($(date +%s) - start)))"
  echo "$status" > "$run/run-$id.done.tmp"
  mv -f "$run/run-$id.done.tmp" "$run/run-$id.done"
}

# ---- One stack: record one shard of one recording. Runs under cargo-slot --stack, in the run's
# checkout. The shard comes in PARITY_SHARD (a shard of parity/shards.json; none: the whole
# corpus, or PARITY_ONLY of it), the reference recording for adaptive waits in
# PARITY_WAITS_FROM.
job() { # name id recording shard image out
  local name="$1" id="$2" rec="$3" shard="$4" image="$5" out="$6"
  local run="$ROOT/runs/$1" state k pfd base block project st=0 i
  state="$run/out/jobs/$rec-$shard.state"
  mkdir -p "$ROOT/ports"
  # A block of host ports nobody holds and nothing listens on, held for the stack's life.
  i=$((RANDOM % PORT_BLOCKS))
  base=""
  for ((k = 0; k < PORT_BLOCKS; k++)); do
    local b=$(((i + k) % PORT_BLOCKS))
    exec {pfd}>> "$ROOT/ports/block-$b"
    if flock -n "$pfd"; then
      if python3 - "$((PORT_BASE + b * 10))" << 'EOF'; then
import socket, sys
p = int(sys.argv[1])
for port in range(p, p + 4):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", port))
    finally:
        s.close()
EOF
        base=$((PORT_BASE + b * 10))
        block="$b"
        break
      fi
    fi
    exec {pfd}>&-
  done
  if [ -z "$base" ]; then
    echo "no free block of host ports from $PORT_BASE" >&2
    echo "done 1 -" > "$state"
    exit 1
  fi
  project="octo-lane-$(safe "$name")-$id-$rec-$(safe "$shard")"
  echo "recording $(date +%s) $project" > "$state"
  if [ "${PARITY_SELFTEST:-0}" = 1 ]; then
    echo "selftest $(date +%s) $project" > "$state"
    echo "selftest stack $project on ports $base-$((base + 3)) for ${PARITY_SLEEP:-60} s"
    for ((i = 0; i < ${PARITY_SLEEP:-60}; i += 5)); do
      echo "selftest $rec $shard: $i s"
      sleep 5
    done
    mkdir -p "$out"
    echo "{}" > "$out/meta.json"
    echo "done 0 $(date +%s)" > "$state"
    exit 0
  fi
  local args=(--project "$project" --port "$base" --nd-port "$((base + 3))" --image "$image" --out "$out")
  if [ -n "${PARITY_SHARD:-}" ]; then
    args+=(--shard "$PARITY_SHARD")
  elif [ -n "${PARITY_ONLY:-}" ]; then
    args+=(--only "$PARITY_ONLY")
  fi
  if [ -n "${PARITY_WAITS_FROM:-}" ]; then args+=(--waits-from "$PARITY_WAITS_FROM"); fi
  echo "stack $project on ports $base-$((base + 3)) and $LANE_NET.$block.0/24, image $image"
  rm -rf "$out"
  mkdir -p "$(dirname "$out")"
  if ! make_networks "$project" "$block"; then
    echo "could not make the networks of $project on $LANE_NET.$block.0/24" >&2
    echo "done 1 -" > "$state"
    exit 1
  fi
  trap 'docker network rm "${project}_sandbox" "${project}_edge" > /dev/null 2>&1 || true' EXIT
  python3 parity/parity.py run "${args[@]}" > "$out.log" 2>&1 || st=$?
  echo "done $st $(date +%s)" > "$state"
  # 2 = some requests got no answer at all (the diff shows them); anything else is a failure.
  if [ "$st" != 0 ] && [ "$st" != 2 ]; then
    tail -n 30 "$out.log"
    echo "recording $rec $shard with $image failed (exit $st); log $out.log" >&2
  fi
  exit "$st"
}

# ---- The run itself, in the run's checkout.
body() { # name options...
  local name="$1"
  shift
  check_name "$name"
  local build=2 only="" bytes=0 cs_twice=0 rs_twice=0 rust=1 selftest=0 sleep_s=60
  while [ $# -gt 0 ]; do
    case "$1" in
      --no-build) build=0 ;;
      --prebuilt) build=2 ;;
      --csharp-only) build=0; rust=0 ;;
      --only) only="$2"; shift ;;
      --bytes) bytes=1 ;;
      --csharp-twice) cs_twice=1 ;;
      --rust-twice) rs_twice=1 ;;
      --selftest) selftest=1 ;;
      --sleep) sleep_s="$2"; shift ;;
      *) echo "unknown option $1" >&2; exit 64 ;;
    esac
    shift
  done
  RUN="$ROOT/runs/$name"
  SRC="$RUN/src"
  OUT="$RUN/out"
  ID="${PARITY_RUN_ID:?}"
  RS_IMAGE="octo-rust-lane:$name"
  NAME="$name"
  ONLY="$only"
  cd "$SRC"
  rm -rf "$OUT"
  mkdir -p "$OUT/jobs" "$OUT/cs-ready" "$ROOT/cs-cache"
  local t0
  t0="$(date +%s)"

  # ---- The shard plan: parity/shards.json when there is one, parity.py can record one shard
  # of it (run --shard) and join shards (merge), and there is no --only; else one shard, the
  # whole corpus (or the --only part of it).
  SHARDS=()
  local plan_id=whole
  if [ -z "$only" ] && [ -f parity/shards.json ]; then
    if python3 parity/parity.py merge --help > /dev/null 2>&1 &&
      python3 parity/parity.py run --help 2> /dev/null | grep -qE -- '--shard( |\]|$)'; then
      mapfile -t SHARDS < <(python3 -c '
import json
plan = json.load(open("parity/shards.json", encoding="utf-8"))
for s in plan["shards"] if isinstance(plan, dict) else plan:
    print(s["name"])')
      plan_id="$(python3 parity/parity.py shards digest 2> /dev/null || sha256sum parity/shards.json)"
      plan_id="${plan_id:0:12}"
    else
      say "parity/shards.json is here, but this parity.py cannot record or join shards; one shard"
    fi
  fi
  if [ "${#SHARDS[@]}" -le 1 ]; then
    SHARDS=(all)
    plan_id=whole
  fi
  # Adaptive waits: the Rust recording of a shard waits for the C# one, and reads its waits.
  ADAPTIVE=0
  if python3 parity/parity.py run --help 2> /dev/null | grep -q -- --waits-from; then ADAPTIVE=1; fi
  say "plan: ${#SHARDS[@]} shard(s): ${SHARDS[*]}$([ "$ADAPTIVE" = 1 ] && echo '; Rust waits adapt to C#' || true)"

  if [ "$selftest" = 1 ]; then
    export PARITY_SELFTEST=1 PARITY_SLEEP="$sleep_s"
    say "selftest: one stack per shard, $sleep_s s each, holding 1 unit of the budget each (no Docker)"
    STACK_FLAG=(--weight 1 --polite)
    RUST_FLAG=(--weight 1 --polite)
    record_set st-a "-" "$OUT/st-a" 0 &
    local pa=$!
    record_set st-b "-" "$OUT/st-b" 0 &
    local pb=$!
    local sa=0 sb=0
    wait "$pa" || sa=$?
    wait "$pb" || sb=$?
    echo "selftest report for $name $ID: shards ${SHARDS[*]}, statuses $sa $sb" > "$RUN/report.txt"
    say "selftest: done in $(($(date +%s) - t0)) s"
    return $((sa + sb))
  fi
  STACK_FLAG=(--stack)
  # A stack of the Rust build takes less (budget.sh); an older cargo-slot has no such share.
  RUST_FLAG=(--stack)
  if "$SLOT" --units 2> /dev/null | awk 'NF >= 5 {ok = 1} END {exit !ok}'; then RUST_FLAG=(--rust-stack); fi

  # ---- Disk: at least 8 GB free.
  local free_gb
  free_gb="$(df -BG --output=avail "$ROOT" | tail -n 1 | tr -dc '0-9')"
  if [ "$free_gb" -lt "$MIN_FREE_GB" ]; then
    echo "only $free_gb GB free in WSL (need $MIN_FREE_GB)" >&2
    exit 1
  fi
  reap_stacks

  # ---- The Rust image.
  if [ "$build" = 2 ] && [ "$rust" = 1 ]; then
    [ -x .parity-bin/octo ] || { echo "no .parity-bin/octo in the checkout; run.sh sends it" >&2; exit 1; }
    local old_id new_id start
    old_id="$(docker image inspect -f '{{.Id}}' "$RS_IMAGE" 2> /dev/null || true)"
    say "building $RS_IMAGE around the binary built for $(git rev-parse --short HEAD)"
    {
      echo "FROM scratch AS build"
      echo "COPY .parity-bin/octo /out/octo"
      sed -n '/^# ---- Runtime ----/,$p' Dockerfile
    } > .parity-bin/Dockerfile
    start=$(date +%s)
    docker build -q -f .parity-bin/Dockerfile -t "$RS_IMAGE" . > "$RUN/build.log" 2>&1 || {
      tail -n 40 "$RUN/build.log"
      echo "the Rust image did not build; the whole log is $RUN/build.log" >&2
      exit 1
    }
    echo "built in $(($(date +%s) - start)) s"
    new_id="$(docker image inspect -f '{{.Id}}' "$RS_IMAGE")"
    if [ -n "$old_id" ] && [ "$old_id" != "$new_id" ]; then
      docker rmi "$old_id" > /dev/null 2>&1 || true
    fi
  elif [ "$rust" = 1 ] && ! docker image inspect "$RS_IMAGE" > /dev/null 2>&1; then
    echo "no $RS_IMAGE in WSL yet; run without --no-build" >&2
    exit 1
  fi
  docker image inspect "$CS_IMAGE" > /dev/null 2>&1 || docker pull -q "$CS_IMAGE" > /dev/null

  # ---- The recordings. C#: from the cache when it has this corpus and plan, else recorded (and
  # kept); with --only, recorded for this run alone. Rust, Rust again and C# again beside it.
  local digest cs_dir
  digest="$(python3 parity/parity.py digest)"
  if [ -n "$only" ]; then
    cs_dir="$OUT/cs"
  else
    # A C# recording made by a parity.py that keeps wait times is kept apart from an older one.
    cs_dir="$ROOT/cs-cache/${digest:0:16}-$plan_id$([ "$ADAPTIVE" = 1 ] && echo -waits || true)"
  fi
  CS_DIR="$cs_dir"
  local pids=() what=() p s fail=0
  (
    st=0
    cs_runner "$cs_dir" "${digest:0:16}" || st=$?
    # A shard the C# recording never got to is failed, so no Rust recording waits for it.
    for s in "${SHARDS[@]}"; do
      [ -f "$OUT/cs-ready/$s" ] || echo failed > "$OUT/cs-ready/$s"
    done
    exit "$st"
  ) &
  pids+=($!)
  what+=("C# 2026.10.05")
  if [ "$rust" = 1 ]; then
    record_set rs "$RS_IMAGE" "$OUT/rs" "$ADAPTIVE" &
    pids+=($!)
    what+=("the Rust build")
    if [ "$rs_twice" = 1 ]; then
      record_set rs2 "$RS_IMAGE" "$OUT/rs-again" "$ADAPTIVE" &
      pids+=($!)
      what+=("the Rust build again")
    fi
  fi
  if [ "$cs_twice" = 1 ]; then
    record_set cs2 "$CS_IMAGE" "$OUT/cs-again" 0 &
    pids+=($!)
    what+=("C# again")
  fi
  for i in "${!pids[@]}"; do
    s=0
    wait "${pids[$i]}" || s=$?
    if [ "$s" != 0 ]; then
      echo "recording ${what[$i]} failed (exit $s)" >&2
      fail=1
    fi
  done
  if [ "$fail" = 1 ]; then exit 1; fi
  say "recorded in $(ago $(($(date +%s) - t0))) ($(($(date +%s) - t0)) s) since the run started"

  diff_run() { # label a b report [bytes]
    local mode=structural
    [ "${5:-}" = bytes ] && mode=bytes
    python3 parity/parity.py diff --mode "$mode" "$2" "$3" --report "$4" --groups-out "${4%.txt}.groups.json" \
      ${only:+--only "$only"} --summary > "$4.summary" || true
    say "$1 ($mode)"
    cat "$4.summary"
  }
  local cs_all="$cs_dir/all"
  if [ "$cs_twice" = 1 ]; then
    diff_run "C# against C#" "$cs_all" "$OUT/cs-again/all" "$RUN/cs-cs.txt"
    diff_run "C# against C#" "$cs_all" "$OUT/cs-again/all" "$RUN/cs-cs-bytes.txt" bytes
  fi
  if [ "$rs_twice" = 1 ] && [ "$rust" = 1 ]; then
    diff_run "Rust against Rust" "$OUT/rs/all" "$OUT/rs-again/all" "$RUN/rs-rs.txt"
    diff_run "Rust against Rust" "$OUT/rs/all" "$OUT/rs-again/all" "$RUN/rs-rs-bytes.txt" bytes
  fi
  if [ "$rust" = 1 ]; then
    diff_run "C# against Rust" "$cs_all" "$OUT/rs/all" "$RUN/report.txt"
  fi
  if [ "$bytes" = 1 ] && [ "$rust" = 1 ]; then
    diff_run "C# against Rust" "$cs_all" "$OUT/rs/all" "$RUN/report-bytes.txt" bytes
  fi
  say "wsl: $(df -BG --output=avail "$ROOT" | tail -n 1 | tr -dc '0-9') GB free; reports in $RUN; C# recording $cs_all; $(ago $(($(date +%s) - t0))) in all"
}

# The C# recording: the cache's, or recorded now. Writes cs-ready/<shard> ("ok DIR" or
# "failed") for each shard as it is there, which the Rust recordings of that shard wait for.
cs_runner() { # dir digest
  local dir="$1" lk st=0 s
  if [ "$dir" = "$OUT/cs" ]; then
    say "recording C# 2026.10.05 (corpus $2, --only: for this run alone)"
    record_set cs "$CS_IMAGE" "$dir" 0 || st=$?
    return "$st"
  fi
  # One run records a corpus version at a time; another that needs it waits and uses it.
  exec {lk}>> "$dir.lock"
  if ! flock -n "$lk"; then
    say "another run is recording C# for corpus $2; waiting for it"
    flock "$lk"
  fi
  if [ -f "$dir/complete" ]; then
    say "C# 2026.10.05 recording for corpus $2 is cached ($dir)"
    for s in "${SHARDS[@]}"; do echo "ok $(shard_dir "$dir" "$s")" > "$OUT/cs-ready/$s"; done
    return 0
  fi
  say "recording C# 2026.10.05 (corpus $2) into $dir"
  rm -rf -- "$dir"
  mkdir -p "$dir"
  record_set cs "$CS_IMAGE" "$dir" 0 || st=$?
  if [ "$st" = 0 ]; then date -u +%FT%TZ > "$dir/complete"; fi
  exec {lk}>&-
  return "$st"
}

# Where shard S of the recording in DIR goes: DIR/all itself when there is one shard.
shard_dir() { # dir shard
  if [ "${#SHARDS[@]}" = 1 ]; then echo "$1/all"; else echo "$1/shards/$2"; fi
}

# One recording, all its shards at once (each a stack under the budget), then joined into
# DIR/all. With WAIT_CS=1 each shard first waits for the C# recording of that shard.
record_set() { # recording image dir wait_cs
  local rec="$1" image="$2" dir="$3" wait_cs="$4" i s pids=() st=0 r
  for i in "${!SHARDS[@]}"; do
    s="${SHARDS[$i]}"
    (
      local ref="" state="$OUT/jobs/$rec-$s.state" out
      out="$(shard_dir "$dir" "$s")"
      if [ "$wait_cs" = 1 ]; then
        echo "after-cs $(date +%s)" > "$state"
        while [ ! -f "$OUT/cs-ready/$s" ]; do sleep 5; done
        read -r r ref < "$OUT/cs-ready/$s"
        if [ "$r" != ok ]; then
          echo "$rec $s: the C# recording of this shard failed; not recorded" >&2
          exit 1
        fi
        # Only a C# recording with wait times can lend them.
        if ! grep -qs '"waitMs"' "$ref"/*.json; then ref=""; fi
      fi
      echo "waiting $(date +%s)" > "$state"
      local t=$(date +%s) js=0
      local shard_arg="$s" flag=("${STACK_FLAG[@]}")
      if [ "$image" = "$RS_IMAGE" ]; then flag=("${RUST_FLAG[@]}"); fi
      if [ "${#SHARDS[@]}" = 1 ]; then shard_arg=""; fi
      PARITY_SHARD="$shard_arg" PARITY_ONLY="$ONLY" PARITY_WAITS_FROM="$ref" \
        "$SLOT" "${flag[@]}" bash "$SELF" --job "$NAME" "$ID" "$rec" "$s" "$image" "$out" \
        > "$OUT/jobs/$rec-$s.log" 2>&1 || js=$?
      echo "  $rec $s: exit $js in $(($(date +%s) - t)) s$( [ -n "$ref" ] && echo ', waits from C#')"
      grep -E 'transport errors|WAIT|SKIP' "$out.log" 2> /dev/null | sed "s/^/    $rec $s: /" | tail -n 8 || true
      if [ "$rec" = cs ]; then
        if [ "$js" = 0 ] || [ "$js" = 2 ]; then echo "ok $out" > "$OUT/cs-ready/$s"; else echo "failed" > "$OUT/cs-ready/$s"; fi
      fi
      if [ "$js" != 0 ] && [ "$js" != 2 ]; then
        tail -n 20 "$OUT/jobs/$rec-$s.log" | sed "s/^/    $rec $s: /"
        exit 1
      fi
      exit 0
    ) &
    pids+=($!)
  done
  for i in "${!pids[@]}"; do
    wait "${pids[$i]}" || st=1
  done
  if [ "$st" != 0 ]; then return 1; fi
  if [ "${#SHARDS[@]}" -gt 1 ] && [ "${PARITY_SELFTEST:-0}" != 1 ]; then
    local parts=()
    for s in "${SHARDS[@]}"; do parts+=("$dir/shards/$s"); done
    python3 parity/parity.py merge "$dir/all" "${parts[@]}" > "$dir/merge.log" 2>&1 || {
      cat "$dir/merge.log"
      echo "joining the shards of $rec failed" >&2
      return 1
    }
    echo "  $rec: ${#SHARDS[@]} shards joined into $dir/all"
  fi
  return 0
}

# ---- The front: in your checkout.
front() {
  local repo branch name lane=auto mode=run clean=0 prebuilt=1 selftest=0 boxonly=0
  local args=() pass=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --lane) lane="${2:-}"; shift ;;
      --lane=*) lane="${1#--lane=}" ;;
      *)
        pass+=("$1")
        case "$1" in
          --clean) clean=1 ;;
          --attach) mode=attach ;;
          --queue) mode=queue ;;
          --selftest) selftest=1; prebuilt=0; args+=("$1") ;;
          --sleep | --only) args+=("$1" "${2:-}"); pass+=("${2:-}"); shift ;;
          --box-build | --low-memory) boxonly=1 ;;
          --no-build | --csharp-only) prebuilt=0; args+=("$1") ;;
          --bytes | --rust-twice | --csharp-twice) args+=("$1") ;;
          *) echo "unknown option $1 (parity/run.sh takes those of parity/lxc131.sh, and --lane wsl|box)" >&2; exit 64 ;;
        esac
        ;;
    esac
    shift
  done
  case "$lane" in auto | wsl | box) ;; *) echo "--lane is wsl or box" >&2; exit 64 ;; esac
  if [ "$lane" = wsl ] && [ "$boxonly" = 1 ]; then
    echo "--box-build and --low-memory are for the box lane" >&2
    exit 64
  fi
  local wsl_docker=0
  if [ "$(uname -s)" = Linux ] && command -v docker > /dev/null && docker info > /dev/null 2>&1; then wsl_docker=1; fi

  if [ "$mode" = queue ]; then
    if [ "$lane" != box ]; then
      if [ "$wsl_docker" = 1 ]; then show_wsl_queue; else echo "WSL lane: no Docker here"; fi
      echo
    fi
    if [ "$lane" != wsl ]; then
      echo "Box lane (LXC 131):"
      # From the checkout this script is in: --queue needs no checkout of yours.
      (cd "$(dirname "$SELF")" && bash ./lxc131.sh --queue 2>&1) | sed 's/^/  /' || echo "  the box could not be asked"
    fi
    exit 0
  fi

  repo="$(git rev-parse --show-toplevel)"
  cd "$repo"
  branch="$(git rev-parse --abbrev-ref HEAD)"
  name="${PARITY_NAME:-$branch}"
  name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9._-' '-' | tr 'A-Z' 'a-z')"
  if [ "$selftest" = 1 ]; then name="$name-selftest"; fi

  if [ "$lane" = auto ]; then
    lane=wsl
    if [ "$wsl_docker" = 0 ] || [ "$boxonly" = 1 ]; then
      lane=box
    elif [ "$mode" = attach ]; then
      read -r st _ <<< "$(run_status "$name")"
      if [ "$st" = none ]; then lane=box; fi
    elif [ "$mode" = run ] && [ "$clean" = 0 ] && [ "$selftest" = 0 ] && wsl_crowded; then
      if bash "$repo/parity/lxc131.sh" --queue 2> /dev/null | grep -q 'free: nothing holds it'; then
        echo "the WSL budget is full and stacks wait for it; the box is free: taking the box lane"
        lane=box
      fi
    fi
  fi
  if [ "$lane" = box ]; then
    exec bash "$repo/parity/lxc131.sh" "${pass[@]+"${pass[@]}"}"
  fi
  if [ "$wsl_docker" = 0 ]; then
    echo "no Docker here (the WSL lane runs in WSL's own Docker); use --lane box" >&2
    exit 1
  fi
  wsl_lane "$repo" "$name" "$mode" "$clean" "$prebuilt" "${args[@]+"${args[@]}"}"
}

# Whether ~/bin/cargo-slot is the weighted budget (parity/budget.sh), not the old flat slots.
budget_installed() { [ -x "$SLOT" ] && grep -q octo-budget "$SLOT"; }

# Whether the WSL budget has no room for a stack and stacks already wait for it.
wsl_crowded() {
  budget_installed || return 1
  local status
  status="$("$SLOT" --status 2> /dev/null)" || return 1
  local free stack
  free="$(sed -n 's/.*free: \([0-9]*\).*/\1/p' <<< "$status")"
  stack="$("$SLOT" --units 2> /dev/null | awk '{print $3}')"
  [ -n "$free" ] && [ -n "$stack" ] && [ "$free" -lt "$stack" ] && grep -q -- '--job' <<< "$(grep -E 'waiting|first in line' <<< "$status")"
}

wsl_lane() { # repo name mode clean prebuilt options...
  local repo="$1" name="$2" mode="$3" clean="$4" prebuilt="$5"
  shift 5
  local run="$ROOT/runs/$name" work="$repo/parity/.run/wsl-$name" st id spid code
  check_name "$name"
  mkdir -p "$run" "$work"
  read -r st id spid _ <<< "$(run_status "$name")"

  if [ "$mode" = attach ]; then
    case "$st" in
      none) echo "no WSL run of $name to attach to" >&2; exit 1 ;;
    esac
    echo "attaching to the WSL run of $name $id ($st)"
    follow_and_finish "$name" "$id" "$work"
  fi
  if [ "$st" = running ]; then
    echo "a WSL run of $name is still going; parity/run.sh --attach follows it" >&2
    exit 75
  fi
  if [ "$clean" = 1 ]; then
    docker rmi "octo-rust-lane:$name" 2> /dev/null || true
    rm -rf -- "${ROOT:?}/runs/$name"
    df -h "$ROOT" | tail -n 1
    exit 0
  fi

  if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    if [ "$prebuilt" = 1 ]; then
      echo "commit first: the binary is built from the working tree and the run uses HEAD" >&2
      exit 1
    fi
    echo "note: uncommitted changes are not used; the run uses $(git rev-parse --short HEAD)" >&2
  fi
  if ! budget_installed; then
    echo "~/bin/cargo-slot is not the weighted budget; install it: bash parity/budget.sh --install" >&2
    exit 1
  fi
  local sha binary musl_target=x86_64-unknown-linux-musl
  sha="$(git rev-parse HEAD)"
  binary="$repo/target/$musl_target/release/octo"
  if [ "$prebuilt" = 1 ]; then
    echo "building octo for $musl_target here"
    # The harness compares with C# 2026.10.05, so the binary claims that release (build.rs reads
    # OCTO_VERSION first). A VERSION ahead of it, as on a release branch, would otherwise differ in
    # every answer that names the release. PARITY_OCTO_VERSION picks another.
    OCTO_VERSION="${PARITY_OCTO_VERSION:-2026.10.05}" \
      "$SLOT" cargo build --release --locked --package octo --bin octo --target "$musl_target" --quiet
    echo "built $(du -h "$binary" | cut -f1) binary from $(git rev-parse --short HEAD)"
  fi

  # HEAD, checked out in the run's own folder. It reads this repository's objects (a --shared
  # clone), so any commit here can be checked out there with nothing copied.
  local src="$run/src" common
  common="$(cd "$repo" && cd "$(git rev-parse --git-common-dir)" && pwd)"
  if ! git -C "$src" rev-parse --git-dir > /dev/null 2>&1 ||
    [ "$(cat "$src/.git/objects/info/alternates" 2> /dev/null)" != "$common/objects" ]; then
    rm -rf -- "$src"
    git clone -q --shared --no-checkout "$common" "$src"
  fi
  git -C "$src" checkout -q -f --detach "$sha"
  git -C "$src" clean -q -fdx
  echo "wsl: $name at $(git -C "$src" log --oneline -1)"
  if [ "$prebuilt" = 1 ]; then
    mkdir -p "$src/.parity-bin"
    install -m 755 "$binary" "$src/.parity-bin/octo"
    set -- --prebuilt "$@"
  fi

  # This run's reports only: nothing left from an earlier run is fetched as this one's.
  local b
  for b in "${REPORTS[@]}"; do
    rm -f -- "$run/$b" "$run/$b.summary" "$run/${b%.txt}.groups.json"
  done
  id="$(date -u +%Y%m%d-%H%M%S)"
  if [ -e "$run/run-$id.log" ]; then
    sleep 1
    id="$(date -u +%Y%m%d-%H%M%S)"
  fi
  printf '%s\n' "$id" > "$run/current"
  # The run's machinery is this script; the corpus, the stubs and parity.py are the checkout's.
  install -m 755 "$SELF" "$run/run.sh"
  setsid -f nohup bash "$run/run.sh" --supervise "$id" "$name" "$@" >> "$run/run-$id.log" 2>&1 < /dev/null
  for _ in $(seq 50); do
    [ -f "$run/run-$id.info" ] && break
    sleep 0.2
  done
  if [ ! -f "$run/run-$id.info" ]; then
    echo "the run did not start; see $run/run-$id.log" >&2
    exit 1
  fi
  echo "wsl: run $name $id started, detached. If this script stops, the run goes on:"
  echo "  parity/run.sh --attach follows it again; parity/run.sh --queue shows both lanes"
  follow_and_finish "$name" "$id" "$work"
}

# Follow WSL run ID's log until it ends, copy its reports and exit with its status.
follow_and_finish() { # name id work
  local name="$1" id="$2" work="$3" run="$ROOT/runs/$1" st rid spid c code=1 f
  trap 'echo; echo "stopped following; the run goes on in WSL: parity/run.sh --attach picks it up" >&2; exit 130' INT TERM
  read -r st rid spid c <<< "$(run_status "$name")"
  if [ "$st" = running ] && [ "$spid" != - ]; then
    tail -c +1 -f --pid="$spid" "$run/run-$id.log" | tee "$work/run-$id.log" || true
  else
    tee "$work/run-$id.log" < "$run/run-$id.log" || true
  fi
  trap - INT TERM
  read -r st rid spid c <<< "$(run_status "$name")"
  if [ "$st" = done ] && [[ "$c" =~ ^[0-9]+$ ]]; then
    code="$c"
  else
    echo "the WSL run stopped without its done marker (its supervisor was killed)" >&2
  fi
  for f in "${REPORTS[@]}" report.groups.json; do
    rm -f "$work/$f"
    if [ -f "$run/$f" ]; then cp "$run/$f" "$work/$f"; fi
  done
  echo
  if [ -f "$work/report.txt" ]; then
    echo "full report: parity/.run/wsl-$name/report.txt"
  else
    echo "no report.txt from this run; the log is parity/.run/wsl-$name/run-$id.log"
  fi
  if [ "$code" != 0 ]; then echo "run.sh exit status: $code" >&2; fi
  exit "$code"
}

case "${1:-}" in
  --supervise) shift; supervise "$@"; exit 0 ;;
  --body) shift; body "$@"; exit $? ;;
  --job) shift; job "$@" ;;
  --watchdog) shift; watchdog "$@"; exit 0 ;;
  --stop-run) shift; stop_run "$@"; exit 0 ;;
  --status) check_name "${2:-}"; run_status "$2"; exit 0 ;;
  *) front "$@" ;;
esac
