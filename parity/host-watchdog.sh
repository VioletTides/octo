#!/usr/bin/env bash
# The parity run's memory watchdog, on the Proxmox host itself. parity/lxc131.sh copies it to
# the host and starts it detached (setsid nohup), one per run, so it lives as long as the run
# on the box and not as long as the script that started it.
#
#   host-watchdog.sh CT NAME ID [--selftest]
#
# Every 15 s it reads the host's MemAvailable. Under 1.5 GB it has the box stop the run that
# holds the box lock (box-run.sh --stop-holder: that run's processes and the octo-parity-cs and
# -rs stacks, nothing in any other container and no waiting run), since the holder is the run
# using the memory, whether it is this one or one this run waits behind. It exits when this run
# writes its done marker (/opt/octo-parity/runs/NAME/run-ID.done on the box), when the run has
# gone without one, or when the container stops. It reads the box's files through
# /proc/<the container's init>/root, so it does not start a pct exec every 15 s.
#
# Its pid is in <this file without .sh>.pid while it runs; it removes that and itself when it
# ends. lxc131.sh appends its output to /root/octo-parity-watchdog-NAME.log.
#
# With --selftest (lxc131.sh --selftest) it watches the selftest lock instead, and
# PARITY_WATCHDOG_MB may set the limit, to see it trip. A real run's limit is always 1.5 GB.
set -uo pipefail

ct="$1" name="$2" id="$3" mode="${4:-}"
limit=1536
if [ "$mode" = --selftest ]; then limit="${PARITY_WATCHDOG_MB:-1536}"; fi
run="/opt/octo-parity/runs/$name"
me="$(readlink -f "$0")"
pidfile="${me%.sh}.pid"
echo "$$" > "$pidfile"
trap 'rm -f "$pidfile" "$me"' EXIT

say() { printf '%s watchdog %s %s: %s\n' "$(date '+%F %T')" "$name" "$id" "$*"; }
free_mb() { awk '/MemAvailable/ {print int($2 / 1024)}' /proc/meminfo; }
# The container's root as the host sees it, or nothing when it is not running.
box_root() {
  local p
  p="$(lxc-info -n "$ct" -p -H 2> /dev/null)" || return 1
  [ -n "$p" ] && [ -d "/proc/$p/root$run" ] && echo "/proc/$p/root"
}
# Whether the run's supervisor (its pid in run-ID.info) is still running.
run_alive() { # root
  local spid
  spid="$(sed -n 's/^pid=//p' "$1$run/run-$id.info" 2> /dev/null)"
  [ -n "$spid" ] && tr '\0' ' ' 2> /dev/null < "$1/proc/$spid/cmdline" | grep -q -- "--supervise $id "
}

say "watching (pid $$): the box lock's holder is stopped when the host has under $limit MB free; now $(free_mb) MB"
missing=0
while :; do
  root="$(box_root)" || root=""
  if [ -z "$root" ]; then
    missing=$((missing + 1))
    if [ "$missing" -ge 4 ]; then
      say "container $ct or $run is not there for a minute; exiting"
      exit 0
    fi
  else
    missing=0
    if [ -e "$root$run/run-$id.done" ]; then
      say "the run wrote its done marker (exit $(cat "$root$run/run-$id.done" 2> /dev/null)); exiting"
      exit 0
    fi
    if ! run_alive "$root"; then
      sleep 2
      if [ ! -e "$root$run/run-$id.done" ]; then
        say "the run has gone without a done marker; exiting"
        exit 0
      fi
      continue
    fi
  fi
  free="$(free_mb)"
  if [ "$free" -gt 0 ] && [ "$free" -lt "$limit" ]; then
    say "the host is down to $free MB free; stopping the run that holds the box lock"
    pct exec "$ct" -- bash "$run/src/parity/box-run.sh" --stop-holder ${mode:+"$mode"} --for "$name" \
      --why "the Proxmox host was down to $free MB free" 2>&1 | sed 's/^/  /'
    # Give the memory time to come back before reading it again.
    sleep 30
  fi
  sleep 15
done
