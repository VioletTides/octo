#!/usr/bin/env bash
# The box side of parity/lxc131.sh: build the Rust image from this checkout, record the corpus
# against it and against the C# release, and print the diffs per backlog group. Runs on the
# test box (LXC 131) from /opt/octo-parity/runs/<name>/src; lxc131.sh sends the code and calls
# it. Everything it makes is its own: the folder /opt/octo-parity, compose projects named
# octo-parity-*, the image octo-rust-parity:<name> and the buildx builder octo-parity. It never
# touches the starter rig (/opt/octo, project octo) or other images.
#
#   box-run.sh NAME [--prebuilt | --no-build] [--low-memory] [--csharp-only] [--only GLOB]
#              [--bytes] [--csharp-twice] [--rust-twice] [--selftest [--sleep S]]
#   box-run.sh --start NAME [options]    the same, detached from the caller (below)
#   box-run.sh --status NAME             NAME's latest detached run, one line, for lxc131.sh
#   box-run.sh --queue                   what holds the box lock and what waits for it
#   box-run.sh --stop-holder [--selftest] [--for NAME] [--why TEXT]
#                                        stop the run that holds the lock (the host watchdog)
#
# --prebuilt takes the binary lxc131.sh compiled (a static musl build, .parity-bin/octo in this
# checkout) and builds the image from Dockerfile's runtime stage around it: no compile on
# the box, only Debian packages (cached after the first time) and a copy. lxc131.sh does this
# by default. Without it the whole Dockerfile builds here, compile included.
#
# --low-memory builds with one job and without LTO: the same program, but no 3 GB link, for
# when the Proxmox host has too little memory to spare for the usual build (the host watchdog
# stops a run when the host falls under 1.5 GB). Slower, and its own cache.
#
# One run at a time (a lock), since a stack of two Octos, Navidrome and the stubs takes about
# 2 GB and the box has 6. The C# recording is kept per corpus version (parity.py digest), so it
# is made again only when the corpus, stubs, fixtures or compose file change.
#
# Detached runs. lxc131.sh starts a run with --start, which starts this script again under
# setsid nohup (--supervise) and returns at once, so the run, its wait for the lock included, no
# longer dies with the ssh session that started it. Each run has an id (when it started, UTC)
# and its files in /opt/octo-parity/runs/<name>/: run-<id>.log (all it prints), run-<id>.info
# (the supervisor's pid, when it was queued, its options), run-<id>.locked (when it got the
# lock) and run-<id>.done (its exit status, written last: the done marker). `current` holds the
# latest id. lxc131.sh follows the log until the done marker appears, and lxc131.sh --attach
# does the same for a run whose lxc131.sh was stopped.
#
# --selftest is a dry run of that machinery: it takes its own lock
# (/opt/octo-parity/selftest.lock, so it never queues behind a real run), prints a line every
# 5 s for --sleep seconds (default 60), writes a one-line report.txt and ends. No Docker.
set -euo pipefail

ROOT=/opt/octo-parity
LOCK="$ROOT/run.lock"
SELFTEST_LOCK="$ROOT/selftest.lock"
# The reports a run writes into its folder (each with .summary and .groups.json beside it).
REPORTS=(report.txt report-bytes.txt cs-cs.txt cs-cs-bytes.txt rs-rs.txt rs-rs-bytes.txt)

check_name() {
  case "$1" in
    "" | .* | *[!a-z0-9._-]*) echo "bad run name '$1'" >&2; exit 64 ;;
  esac
}
# Seconds as 1h05m.
ago() { printf '%dh%02dm' $(($1 / 3600)) $(($1 % 3600 / 60)); }
# When a process started, in epoch seconds. ps's own elapsed time is wrong in this container
# (lxcfs gives the container's uptime, the start time counts from the host's boot); its start
# date is right.
started() {
  local s
  s="$(ps -o lstart= -p "$1" 2> /dev/null)" || true
  if [ -n "$s" ]; then date -d "$s" +%s; else echo 0; fi
}
ppid_of() {
  local s pp
  s="$(cat "/proc/$1/stat" 2> /dev/null)" || return 1
  read -r _ pp _ <<< "${s##*) }"
  echo "$pp"
}
# The kernel's start time of a process, to tell a pid from a reused one.
ticks_of() {
  local s f=()
  s="$(cat "/proc/$1/stat" 2> /dev/null)" || return 1
  read -ra f <<< "${s##*) }"
  echo "${f[19]}"
}
# The run name when the process is a run of this script (bash .../box-run.sh NAME ...), not one
# of its modes; fails otherwise.
run_name() {
  local argv=()
  mapfile -d '' -t argv 2> /dev/null < "/proc/$1/cmdline" || return 1
  [ "${#argv[@]}" -ge 3 ] && [[ "${argv[1]}" == */box-run.sh ]] && [[ "${argv[2]}" != -* ]] || return 1
  printf '%s\n' "${argv[2]}"
}
# Whether the process is the supervisor of detached run ID.
supervises() { # pid id
  local argv=()
  mapfile -d '' -t argv 2> /dev/null < "/proc/$1/cmdline" || return 1
  [ "${#argv[@]}" -ge 4 ] && [[ "${argv[1]}" == */box-run.sh ]] && [ "${argv[2]}" = --supervise ] && [ "${argv[3]}" = "$2" ]
}
descendants() {
  local c
  for c in $(ps -o pid= --ppid "$1" 2> /dev/null); do
    echo "$c"
    descendants "$c"
  done
}

# The processes that have LOCK open, sorted out, one line each:
#   holding NAME PID   a run that has the lock (PID is its box-run.sh)
#   waiting NAME PID   a run waiting for it
#   stray held PID     a process that has the lock with no run above it (its run was killed)
#   stray waiting PID  one that waits with no run above it: the `flock 9` of a run killed
#                      while it waited, which lets go the moment it gets the lock
# A process has the lock when its fdinfo for the file lists the flock: the kernel shows a lock
# on the open file that took it, which the run's children share. A waiter has the file open and
# no lock. A run's own subshells look like the run; only the topmost counts.
lock_users() { # lock
  local lock="$1" path pid name p under
  local -A fd_of=() run_of=()
  while read -r path; do
    pid="${path#/proc/}"
    pid="${pid%%/*}"
    fd_of[$pid]="${path##*/}"
  done < <(find /proc/[0-9]*/fd -maxdepth 1 -lname "$lock" 2> /dev/null)
  for pid in "${!fd_of[@]}"; do
    name="$(run_name "$pid")" || continue
    p="$(ppid_of "$pid")" || p=0
    if [ "$(run_name "$p" 2> /dev/null || true)" = "$name" ]; then continue; fi
    run_of[$pid]="$name"
  done
  for pid in "${!fd_of[@]}"; do
    [ -d "/proc/$pid" ] || continue
    if [ -n "${run_of[$pid]:-}" ]; then
      if grep -qs '^lock:.*FLOCK' "/proc/$pid/fdinfo/${fd_of[$pid]}"; then
        echo "holding ${run_of[$pid]} $pid"
      else
        echo "waiting ${run_of[$pid]} $pid"
      fi
      continue
    fi
    under=0
    p="$pid"
    while p="$(ppid_of "$p")" && [ -n "$p" ] && [ "$p" -gt 1 ]; do
      if run_name "$p" > /dev/null; then
        under=1
        break
      fi
    done
    [ "$under" = 1 ] && continue
    if grep -qs '^lock:.*FLOCK' "/proc/$pid/fdinfo/${fd_of[$pid]}"; then
      echo "stray held $pid"
    else
      echo "stray waiting $pid"
    fi
  done
}

# NAME's latest detached run, one line: STATE ID PID CODE ("-" for what is not known).
#   none        no detached run of NAME, and none in the foreground either
#   queued      waiting for the lock (PID is the supervisor's)
#   running     has the lock
#   done        ended; CODE is box-run.sh's exit status
#   gone        its supervisor is not running and it wrote no done marker (killed)
#   foreground  no detached run going, but a run of NAME from an older lxc131.sh is
run_status() { # name
  local name="$1" run="$ROOT/runs/$1" id="" pid="" code="" state=none p pp
  if [ -f "$run/current" ]; then
    id="$(cat "$run/current")"
    pid="$(sed -n 's/^pid=//p' "$run/run-$id.info" 2> /dev/null || true)"
    if [ -f "$run/run-$id.done" ]; then
      state=done
      code="$(cat "$run/run-$id.done")"
    elif [ -n "$pid" ] && supervises "$pid" "$id"; then
      if [ -f "$run/run-$id.locked" ]; then state=running; else state=queued; fi
    else
      state=gone
    fi
  fi
  if [ "$state" != queued ] && [ "$state" != running ]; then
    for p in $(pgrep -f -- "box-run\.sh ${name//./\\.}( |\$)" || true); do
      [ "$(run_name "$p" 2> /dev/null || true)" = "$name" ] || continue
      pp="$(ppid_of "$p")" || pp=0
      if ! supervises "$pp" "${id:-x}"; then
        state=foreground
        pid="$p"
        break
      fi
    done
  fi
  echo "$state ${id:--} ${pid:--} ${code:--}"
}

# When run NAME (box-run.sh PID) got the lock, if it recorded it (a detached run), else 0.
locked_at() { # name pid
  local id at p
  id="$(cat "$ROOT/runs/$1/current" 2> /dev/null)" || { echo 0; return; }
  read -r at p 2> /dev/null < "$ROOT/runs/$1/run-$id.locked" || { echo 0; return; }
  if [ "$p" = "$2" ]; then echo "$at"; else echo 0; fi
}

show_queue() {
  local now lock users kind name pid holder q at how line pp id
  local runs=() strays=()
  now="$(date +%s)"
  for lock in "$LOCK" "$SELFTEST_LOCK"; do
    users="$(lock_users "$lock")"
    if [ "$lock" = "$SELFTEST_LOCK" ] && [ -z "$users" ]; then continue; fi
    echo "$lock ($(date -u '+%F %H:%M') UTC):"
    if [ -z "$users" ]; then
      echo "  free: nothing holds it and nothing waits"
      continue
    fi
    holder="$(awk '$1 == "holding" {print $2}' <<< "$users" | paste -sd, -)"
    runs=()
    strays=()
    while read -r kind name pid; do
      q="$(started "$pid")"
      if [ "$kind" = stray ]; then
        line="stray    pid $pid ($(tr '\0' ' ' 2> /dev/null < "/proc/$pid/cmdline" | cut -c1-60)) in $(readlink "/proc/$pid/cwd" 2> /dev/null), for $(ago $((now - q)))"
        if [ "$name" = held ]; then
          strays+=("$line: has the lock with no run above it (its run was killed)")
        else
          strays+=("$line: a killed run's wait; it lets go the moment it gets the lock")
        fi
        continue
      fi
      pp="$(ppid_of "$pid")" || pp=0
      id="$(cat "$ROOT/runs/$name/current" 2> /dev/null)" || id=x
      if supervises "$pp" "$id"; then how="detached"; else how="in the foreground of an older lxc131.sh"; fi
      if [ "$kind" = holding ]; then
        at="$(locked_at "$name" "$pid")"
        if [ "$at" -gt 0 ]; then
          line="holding  $name  for $(ago $((now - at))) (queued $(ago $((now - q))) ago), pid $pid, $how"
        else
          line="holding  $name  queued $(ago $((now - q))) ago (when it got the lock is not recorded), pid $pid, $how"
        fi
        runs+=("0 $q $line")
      else
        runs+=("1 $q waiting  $name  for $(ago $((now - q))), behind ${holder:-a stray holder}, pid $pid, $how")
      fi
    done <<< "$users"
    if [ "${#runs[@]}" -gt 0 ]; then
      printf '%s\n' "${runs[@]}" | sort -k1,1n -k2,2n | cut -d' ' -f3- | sed 's/^/  /'
    fi
    if [ "${#strays[@]}" -gt 0 ]; then printf '  %s\n' "${strays[@]}"; fi
  done
  echo "Waiters get the lock in whatever order the kernel wakes them, not strictly first come first served."
}

# Stop the run that holds the lock, for the host watchdog: TERM to its box-run.sh and everything
# under it (then KILL to what is left after 10 s), and the octo-parity-cs and -rs stacks down.
# Only the holder: waiting runs, other containers and the starter rig are left alone. With
# --selftest, the selftest lock's holder, and no stacks (a selftest has none).
stop_holder() {
  local lock="$LOCK" selftest=0 for_name="" why="the host watchdog asked" users kind name pid
  local roots=() names=() targets=() alive=() p t id src line
  local -A tick=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --selftest) selftest=1; lock="$SELFTEST_LOCK" ;;
      --for) for_name="$2"; shift ;;
      --why) why="$2"; shift ;;
      *) echo "unknown option $1" >&2; exit 64 ;;
    esac
    shift
  done
  if [ "$selftest" = 0 ] && [[ "$for_name" == *-selftest ]]; then
    echo "a selftest's watchdog never stops a real run" >&2
    exit 64
  fi
  users="$(lock_users "$lock")"
  while read -r kind name pid; do
    if [ "$kind" = holding ]; then
      roots+=("$pid")
      names+=("$name")
    elif [ "$kind $name" = "stray held" ]; then
      roots+=("$pid")
    fi
  done <<< "$users"
  if [ "${#roots[@]}" = 0 ]; then
    echo "nothing holds $lock; nothing stopped"
    return 0
  fi
  for p in "${roots[@]}"; do
    targets+=("$p")
    while read -r t; do
      if [ -n "$t" ]; then targets+=("$t"); fi
    done < <(descendants "$p")
  done
  for p in "${targets[@]}"; do tick[$p]="$(ticks_of "$p" || echo gone)"; done
  line="WATCHDOG: $why; stopping ${names[*]:-a stray lock holder} (pid ${roots[*]} and what runs under it)"
  echo "$line"
  # Into the logs of the run stopped (when detached) and of the run that asked.
  for name in "${names[@]}" "$for_name"; do
    [ -n "$name" ] || continue
    id="$(cat "$ROOT/runs/$name/current" 2> /dev/null)" || continue
    if [ -f "$ROOT/runs/$name/run-$id.log" ] && [ ! -f "$ROOT/runs/$name/run-$id.done" ]; then
      printf '\n%s\n' "$line" >> "$ROOT/runs/$name/run-$id.log"
    fi
  done
  kill -TERM "${targets[@]}" 2> /dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    alive=()
    for p in "${targets[@]}"; do
      if [ "${tick[$p]}" != gone ] && [ "$(ticks_of "$p" 2> /dev/null || echo gone)" = "${tick[$p]}" ]; then
        alive+=("$p")
      fi
    done
    [ "${#alive[@]}" = 0 ] && break
    sleep 1
  done
  if [ "${#alive[@]}" -gt 0 ]; then kill -KILL "${alive[@]}" 2> /dev/null || true; fi
  if [ "$selftest" = 0 ]; then
    src="$ROOT/runs/${names[0]:-x}/src"
    [ -f "$src/parity/docker-compose.yml" ] || src="$(cd "$(dirname "$0")/.." && pwd)"
    for p in octo-parity-cs octo-parity-rs; do
      docker compose -p "$p" -f "$src/parity/docker-compose.yml" down -v --remove-orphans > /dev/null 2>&1 || true
    done
    docker buildx stop octo-parity > /dev/null 2>&1 || true
    echo "parity stacks stopped"
  fi
}

# Start a run detached (see the top) and print its id.
start_detached() { # name options...
  local name="$1" run id st b
  check_name "$name"
  run="$ROOT/runs/$name"
  read -r st _ <<< "$(run_status "$name")"
  case "$st" in
    queued | running | foreground)
      echo "a run of $name is still going on the box ($st); parity/lxc131.sh --attach follows it" >&2
      exit 75
      ;;
  esac
  id="$(date -u +%Y%m%d-%H%M%S)"
  if [ -e "$run/run-$id.log" ]; then
    sleep 1
    id="$(date -u +%Y%m%d-%H%M%S)"
  fi
  # This run's reports only: nothing left from an earlier run is fetched as this one's.
  for b in "${REPORTS[@]}"; do
    rm -f -- "$run/$b" "$run/$b.summary" "$run/${b%.txt}.groups.json"
  done
  printf '%s\n' "$id" > "$run/current"
  setsid -f nohup bash "$0" --supervise "$id" "$@" >> "$run/run-$id.log" 2>&1 < /dev/null
  for _ in $(seq 50); do
    [ -f "$run/run-$id.info" ] && break
    sleep 0.2
  done
  if [ ! -f "$run/run-$id.info" ]; then
    echo "the run did not start; see $run/run-$id.log on the box" >&2
    exit 1
  fi
  echo "$id"
}

# The detached run's supervisor: run box-run.sh, then write the done marker.
supervise() { # id name options...
  local id="$1" name="$2" run status=0 start
  shift
  run="$ROOT/runs/$name"
  start="$(date +%s)"
  printf 'pid=%s\nqueued=%s\nargs=%s\n' "$$" "$start" "$*" > "$run/run-$id.info"
  echo "box: run $name $id, detached (supervisor pid $$), log $run/run-$id.log"
  PARITY_RUN_ID="$id" bash "$0" "$@" || status=$?
  printf '\n== box-run.sh ended with exit %s, %s after it was queued\n' "$status" "$(ago $(($(date +%s) - start)))"
  echo "$status" > "$run/run-$id.done.tmp"
  mv -f "$run/run-$id.done.tmp" "$run/run-$id.done"
}

case "${1:-}" in
  --start) shift; start_detached "$@"; exit 0 ;;
  --supervise) shift; supervise "$@"; exit 0 ;;
  --status) check_name "${2:-}"; run_status "$2"; exit 0 ;;
  --queue) show_queue; exit 0 ;;
  --stop-holder) shift; stop_holder "$@"; exit 0 ;;
  "" | -*) echo "usage: box-run.sh NAME [options] (or --start, --status, --queue, --stop-holder)" >&2; exit 64 ;;
esac

name="$1"
shift
check_name "$name"
build=1 only="" bytes=0 cs_twice=0 rs_twice=0 rust=1 selftest=0 sleep_s=60
build_args=(--build-arg CARGO_BUILD_JOBS=3)
while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) build=0 ;;
    --prebuilt) build=2 ;;
    --low-memory) build_args=(--build-arg CARGO_BUILD_JOBS=1 --build-arg CARGO_PROFILE_RELEASE_LTO=off) ;;
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
CS_IMAGE=ghcr.io/winters27/octo:2026.10.05
RS_IMAGE="octo-rust-parity:$name"
BUILDER=octo-parity
MIN_FREE_GB=8

cd "$SRC"
lock="$LOCK"
if [ "$selftest" = 1 ]; then lock="$SELFTEST_LOCK"; fi
exec 9> "$lock"
if ! flock -n 9; then
  echo "another parity run is going on the box ($(lock_users "$lock" | awk '$1 == "holding" {print $2}' | paste -sd, -)); waiting for it"
  waited="$(date +%s)"
  flock 9
  echo "got the box lock after $(ago $(($(date +%s) - waited)))"
fi
if [ -n "${PARITY_RUN_ID:-}" ]; then echo "$(date +%s) $$" > "$RUN/run-$PARITY_RUN_ID.locked"; fi

if [ "$selftest" = 1 ]; then
  printf '\n== selftest: holding %s for %s s\n' "$lock" "$sleep_s"
  for ((i = 0; i < sleep_s; i += 5)); do
    echo "selftest: $i s"
    sleep 5
  done
  echo "selftest report for $name ${PARITY_RUN_ID:-}" > "$RUN/report.txt"
  printf '\n== selftest: done\n'
  exit 0
fi

free_gb() { df -BG --output=avail / | tail -n 1 | tr -dc '0-9'; }
say() { printf '\n== %s\n' "$*"; }

# ---- Disk: at least 8 GB free before anything is built or pulled.
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
  say "only $(free_gb) GB free: trimming the parity builder's cache"
  docker buildx prune --builder "$BUILDER" -f --keep-storage 3gb > /dev/null 2>&1 || true
fi
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
  echo "only $(free_gb) GB free on the box (need $MIN_FREE_GB); remove old parity images first:" >&2
  docker images --format '{{.Repository}}:{{.Tag}} {{.Size}}' | grep '^octo-rust-parity:' >&2 || true
  exit 1
fi

# ---- The Rust image, built from this checkout.
if [ "$build" = 2 ]; then
  # Dockerfile's runtime stage with the build stage swapped for the binary sent along.
  [ -x .parity-bin/octo ] || { echo "no .parity-bin/octo in the checkout; lxc131.sh sends it" >&2; exit 1; }
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
    echo "the Rust image did not build; the whole log is $RUN/build.log on the box" >&2
    exit 1
  }
  echo "built in $(($(date +%s) - start)) s"
  new_id="$(docker image inspect -f '{{.Id}}' "$RS_IMAGE")"
  if [ -n "$old_id" ] && [ "$old_id" != "$new_id" ]; then
    docker rmi "$old_id" > /dev/null 2>&1 || true
  fi
elif [ "$build" = 1 ]; then
  if ! docker buildx inspect "$BUILDER" > /dev/null 2>&1; then
    # Its own builder, capped below the box's memory (the LTO link takes about 3 GB), with its
    # own cache, so a rebuild after a change in the Octo crates skips the dependencies.
    docker buildx create --name "$BUILDER" --driver docker-container \
      --driver-opt memory=3500m --driver-opt memory-swap=3500m > /dev/null
  fi
  old_id="$(docker image inspect -f '{{.Id}}' "$RS_IMAGE" 2> /dev/null || true)"
  say "building $RS_IMAGE from $(git rev-parse --short HEAD)"
  start=$(date +%s)
  # The harness compares with C# 2026.10.05, so the binary claims that release (build.rs reads
  # OCTO_VERSION first). A VERSION ahead of it, as on a release branch, would otherwise differ in
  # every answer that names the release. PARITY_OCTO_VERSION picks another.
  docker buildx build --builder "$BUILDER" --progress plain -f Dockerfile \
    --build-arg "OCTO_VERSION=${PARITY_OCTO_VERSION:-2026.10.05}" \
    "${build_args[@]}" -t "$RS_IMAGE" --load . > "$RUN/build.log" 2>&1 || {
    tail -n 40 "$RUN/build.log"
    echo "the Rust image did not build; the whole log is $RUN/build.log on the box" >&2
    exit 1
  }
  echo "built in $(($(date +%s) - start)) s"
  new_id="$(docker image inspect -f '{{.Id}}' "$RS_IMAGE")"
  if [ -n "$old_id" ] && [ "$old_id" != "$new_id" ]; then
    docker rmi "$old_id" > /dev/null 2>&1 || true
  fi
  # Keep the builder's cache from growing without end (dependencies stay; old Octo layers go),
  # and stop the builder: its daemon holds memory the stacks below need.
  docker buildx prune --builder "$BUILDER" -f --keep-storage 6gb > /dev/null 2>&1 || true
  docker buildx stop "$BUILDER" > /dev/null 2>&1 || true
elif [ "$rust" = 1 ] && ! docker image inspect "$RS_IMAGE" > /dev/null 2>&1; then
  echo "no $RS_IMAGE on the box yet; run without --no-build" >&2
  exit 1
fi

docker image inspect "$CS_IMAGE" > /dev/null 2>&1 || docker pull -q "$CS_IMAGE" > /dev/null

record() { # project port nd-port image out
  rm -rf "$5"
  local status=0
  python3 parity/parity.py run --project "$1" --port "$2" --nd-port "$3" --image "$4" \
    --out "$5" ${only:+--only "$only"} > "$5.log" 2>&1 || status=$?
  # 2 = some requests got no answer at all (the diff shows them); anything else is a failure.
  if [ "$status" != 0 ] && [ "$status" != 2 ]; then
    tail -n 30 "$5.log"
    echo "recording with $4 failed (exit $status); log $5.log on the box" >&2
    exit 1
  fi
  grep -E 'transport errors|WAIT|SKIP' "$5.log" | sed 's/^/  /' | tail -n 15 || true
}

mkdir -p "$OUT" "$ROOT/cs-cache"
digest="$(python3 parity/parity.py digest)"
cs_dir="$ROOT/cs-cache/${digest:0:16}${only:+-only}"
if [ -n "$only" ] || [ ! -f "$cs_dir/meta.json" ]; then
  say "recording C# 2026.10.05 (corpus ${digest:0:16})"
  record octo-parity-cs 18480 18453 "$CS_IMAGE" "$cs_dir"
else
  say "C# 2026.10.05 recording for corpus ${digest:0:16} is cached"
fi

if [ "$rust" = 1 ]; then
  say "recording the Rust build"
  record octo-parity-rs 18580 18553 "$RS_IMAGE" "$OUT/rs"
fi

diff_run() { # label a b report
  local mode=structural
  [ "${5:-}" = bytes ] && mode=bytes
  python3 parity/parity.py diff --mode "$mode" "$2" "$3" --report "$4" --groups-out "${4%.txt}.groups.json" \
    ${only:+--only "$only"} --summary > "$4.summary" || true
  say "$1 ($mode)"
  cat "$4.summary"
}

if [ "$cs_twice" = 1 ]; then
  record octo-parity-cs 18480 18453 "$CS_IMAGE" "$OUT/cs-again"
  diff_run "C# against C#" "$cs_dir" "$OUT/cs-again" "$RUN/cs-cs.txt"
  diff_run "C# against C#" "$cs_dir" "$OUT/cs-again" "$RUN/cs-cs-bytes.txt" bytes
fi
if [ "$rs_twice" = 1 ] && [ "$rust" = 1 ]; then
  record octo-parity-rs 18580 18553 "$RS_IMAGE" "$OUT/rs-again"
  diff_run "Rust against Rust" "$OUT/rs" "$OUT/rs-again" "$RUN/rs-rs.txt"
  diff_run "Rust against Rust" "$OUT/rs" "$OUT/rs-again" "$RUN/rs-rs-bytes.txt" bytes
fi
if [ "$rust" = 1 ]; then
  diff_run "C# against Rust" "$cs_dir" "$OUT/rs" "$RUN/report.txt"
fi
if [ "$bytes" = 1 ] && [ "$rust" = 1 ]; then
  diff_run "C# against Rust" "$cs_dir" "$OUT/rs" "$RUN/report-bytes.txt" bytes
fi

say "box: $(free_gb) GB free; reports in $RUN; C# recording $cs_dir"
