#!/usr/bin/env bash
# Run the parity harness on the test box (LXC 131) against the Rust build of the commit you have
# checked out, and print the diffs per backlog group. For port agents; one command:
#
#   parity/lxc131.sh                      # build here, record, compare with C# 2026.10.05
#   parity/lxc131.sh --only '13-*'        # just some corpus files (the C# side is recorded again)
#   parity/lxc131.sh --no-build           # same image as last time (corpus or stub changes only)
#   parity/lxc131.sh --box-build          # compile on the box instead (Dockerfile whole)
#   parity/lxc131.sh --box-build --low-memory   # that, with one job and no LTO
#   parity/lxc131.sh --bytes              # also the bytes-mode diff
#   parity/lxc131.sh --rust-twice         # also Rust against Rust (the determinism gate)
#   parity/lxc131.sh --csharp-twice       # also C# against C# (after a corpus change)
#   parity/lxc131.sh --csharp-only --csharp-twice   # corpus work: no Rust build or run
#   parity/lxc131.sh --attach             # follow this name's run on the box again and fetch
#                                         # its reports (after this script was stopped)
#   parity/lxc131.sh --queue              # what holds the box lock and what waits for it
#   parity/lxc131.sh --selftest [--sleep S]     # a dry run of the harness's own machinery
#   parity/lxc131.sh --clean              # remove this name's image and folder on the box
#
# What it does: compiles octo here as a static musl binary (cargo build --release --target
# x86_64-unknown-linux-musl; `rustup target add x86_64-unknown-linux-musl` and musl-tools once),
# bundles HEAD (only the commits the box lacks), copies the bundle and the binary to the box
# through the Proxmox host, checks it out in /opt/octo-parity/runs/<name>/src, and starts
# parity/box-run.sh there (the image from Dockerfile's runtime stage around that binary,
# the corpus against both builds, the diff).
#
# Compiling here is the default because the box's compile is what hurts the shared Proxmox
# host: the LTO link takes about 3 GB, and on 2026-10-07 a box build tripped the watchdog with
# the host at 1 GB and its swap full. The runtime stage is only Debian packages and a copy.
# A static musl binary runs on bookworm-slim whatever glibc the machine that built it has
# (a WSL glibc build needs GLIBC_2.38 and does not start there). --box-build is the old way.
# The full report comes back to parity/.run/lxc131-<name>/. Commit first: only commits travel.
# Nothing is pushed anywhere and nothing on the box outside /opt/octo-parity, the
# octo-parity-* compose projects, the octo-rust-parity:<name> image and the octo-parity buildx
# builder is touched.
#
# The run on the box is detached (box-run.sh --start): it waits for the box lock, runs and
# writes its log and done marker there whether this script is still running or not. This
# script follows that log (a copy goes to parity/.run/lxc131-<name>/run-<id>.log), and when the
# done marker appears fetches the reports and exits with box-run.sh's exit status (0: the run
# went through, diffs or not). If it is stopped, the run goes on: --attach follows it again.
# One run per name at a time; a second start of a name whose run is still going is refused.
#
# Settings (environment): PARITY_NAME (default: the branch name), PARITY_PROXMOX (ssh host of
# the Proxmox node, default pmx), PARITY_CT (container id, default 131), PARITY_MIN_HOST_MB (the
# Proxmox host's free memory needed to start, default 3500). From WSL, where `pmx` is only in
# the Windows ssh config, the Windows ssh.exe/scp.exe are used automatically.
#
# The Proxmox host is shared (LXC 111 runs Brandon's own Octo there) and has little memory to
# spare: on 2026-10-07 a run with the host already short pushed it into swap until nothing
# answered. So the run starts only with PARITY_MIN_HOST_MB free on the host, and a watchdog on
# the host itself (parity/host-watchdog.sh, started detached for the life of the run) stops the
# run that holds the box lock, and takes its stacks down, when the host's free memory drops
# under 1.5 GB.
set -euo pipefail

repo="$(git rev-parse --show-toplevel)"
cd "$repo"
host="${PARITY_PROXMOX:-pmx}"
ct="${PARITY_CT:-131}"
branch="$(git rev-parse --abbrev-ref HEAD)"
name="${PARITY_NAME:-$branch}"
name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9._-' '-' | tr 'A-Z' 'a-z')"
args=()
mode=run
clean=0
prebuilt=1
selftest=0
for a in "$@"; do
  case "$a" in
    --clean) clean=1 ;;
    --attach) mode=attach ;;
    --queue) mode=queue ;;
    --selftest) selftest=1; prebuilt=0; args+=("$a") ;;
    --box-build) prebuilt=0 ;;
    --no-build | --csharp-only) prebuilt=0; args+=("$a") ;;
    *) args+=("$a") ;;
  esac
done
if [ "$prebuilt" = 1 ]; then
  for a in "${args[@]+"${args[@]}"}"; do
    if [ "$a" = --low-memory ]; then
      echo "--low-memory is for a box build; add --box-build" >&2
      exit 64
    fi
  done
fi
# A selftest has its own name, so it never meets this branch's real run.
if [ "$selftest" = 1 ]; then name="$name-selftest"; fi

# ssh/scp that know the host: the plain ones, or Windows' from WSL.
SSH=ssh SCP=scp
if ! ssh -o BatchMode=yes -o ConnectTimeout=5 "$host" true 2> /dev/null; then
  if command -v ssh.exe > /dev/null && ssh.exe -o BatchMode=yes -o ConnectTimeout=5 "$host" true 2> /dev/null; then
    SSH=ssh.exe SCP=scp.exe
  else
    echo "cannot reach $host over ssh (set PARITY_PROXMOX)" >&2
    exit 1
  fi
fi
on_box() { "$SSH" "$host" "pct exec $ct -- bash -c $(printf '%q' "$1")"; }
# This commit's box-run.sh, run on the box from a temporary copy: the modes that need no
# checkout there (--status NAME, --queue).
box_tool() {
  git -C "$repo" show HEAD:parity/box-run.sh |
    "$SSH" "$host" "pct exec $ct -- bash -c 'f=\$(mktemp) && cat > \"\$f\" && bash \"\$f\" $*; s=\$?; rm -f \"\$f\"; exit \$s'"
}

work="$repo/parity/.run/lxc131-$name"
mkdir -p "$work"
boxrun="/opt/octo-parity/runs/$name"

if [ "$mode" = queue ]; then
  box_tool --queue
  exit 0
fi

# The host's memory watchdog for run ID: parity/host-watchdog.sh, copied to the Proxmox host
# and started there detached, so it outlives this script. One per run; --attach starts it
# again when it is not running.
start_watchdog() { # id
  local id="$1" base="/root/octo-parity-watchdog-$name-$1" env="" flag=""
  if "$SSH" "$host" "test -s $base.pid && grep -qa octo-parity-watchdog /proc/\$(cat $base.pid)/cmdline 2> /dev/null"; then
    echo "host watchdog: already running for this run"
    return 0
  fi
  if [ "$selftest" = 1 ]; then
    # A selftest may set its own limit, to see the watchdog trip; a real run never can.
    flag=" --selftest"
    case "${PARITY_WATCHDOG_MB:-}" in
      "") ;;
      *[!0-9]*) echo "PARITY_WATCHDOG_MB must be a number of MB" >&2; exit 64 ;;
      *) env="PARITY_WATCHDOG_MB=$PARITY_WATCHDOG_MB " ;;
    esac
  fi
  git -C "$repo" show HEAD:parity/host-watchdog.sh |
    "$SSH" "$host" "cat > $base.sh && ${env}setsid -f nohup bash $base.sh $ct $name $id$flag >> /root/octo-parity-watchdog-$name.log 2>&1 < /dev/null && sleep 1 && echo \"host watchdog: pid \$(cat $base.pid 2> /dev/null), log /root/octo-parity-watchdog-$name.log on the Proxmox host\""
}

# Follow run ID's log on the box until its done marker is there, a copy into
# parity/.run/lxc131-<name>/run-<id>.log. Sets code to box-run.sh's exit status (75 when the box
# could not be reached for a while, and the run goes on there; then no reports are fetched).
follow_run() { # id
  local id="$1" log="$work/run-$1.log" st rid spid c size last=-1 stalls=0
  : > "$log"
  code=1
  while :; do
    read -r st rid spid c <<< "$(box_tool --status "$name" 2> /dev/null || echo "unknown - - -")"
    if [ "$st" != unknown ] && [ "$rid" != "$id" ]; then
      echo "the box's latest run of $name is $rid ($st), not $id; its reports are not fetched" >&2
      code=75
      return 0
    fi
    size="$(wc -c < "$log" | tr -d ' ')"
    case "$st" in
      done | gone)
        # What is left of the log, then the end.
        "$SSH" -n "$host" "pct exec $ct -- tail -c +$((size + 1)) $boxrun/run-$id.log" | tee -a "$log" || true
        if [ "$st" = done ] && [[ "$c" =~ ^[0-9]+$ ]]; then
          code="$c"
        else
          echo "the run on the box stopped without its done marker (its supervisor was killed)" >&2
          code=1
        fi
        return 0
        ;;
      queued | running)
        # Until the supervisor ends; tail checks it every second.
        "$SSH" -n -o ServerAliveInterval=30 -o ServerAliveCountMax=4 "$host" \
          "pct exec $ct -- tail -c +$((size + 1)) -f --pid=$spid $boxrun/run-$id.log" | tee -a "$log" || true
        ;;
    esac
    # Back here with the run not done: the connection dropped. Again, unless nothing comes.
    size="$(wc -c < "$log" | tr -d ' ')"
    if [ "$size" = "$last" ]; then stalls=$((stalls + 1)); else stalls=0; fi
    last="$size"
    if [ "$stalls" -ge 12 ]; then
      echo "lost the box for a minute; the run goes on there: parity/lxc131.sh --attach" >&2
      code=75
      return 0
    fi
    sleep 5
  done
}

# The full reports, for reading the diffs themselves.
pull_reports() {
  local f there
  # pct pull of a file that is not there makes an empty one; only what the run wrote.
  there="$("$SSH" "$host" "pct exec $ct -- ls -1 $boxrun" 2> /dev/null | tr -d '\r' || true)"
  for f in report.txt report-bytes.txt cs-cs.txt cs-cs-bytes.txt rs-rs.txt rs-rs-bytes.txt report.groups.json; do
    rm -f "$work/$f"
    grep -qx -- "$f" <<< "$there" || continue
    if "$SSH" "$host" "pct pull $ct $boxrun/$f /root/parity-$name-$f" 2> /dev/null; then
      (cd "$work" && "$SCP" -q "$host:/root/parity-$name-$f" "$f")
      "$SSH" "$host" "rm -f /root/parity-$name-$f"
    fi
  done
  echo
  if [ -f "$work/report.txt" ]; then
    echo "full report: parity/.run/lxc131-$name/report.txt"
  else
    echo "no report.txt from this run; the log is parity/.run/lxc131-$name/run-$1.log"
  fi
}

# Follow run ID to its end, fetch the reports and exit with its status.
follow_and_finish() { # id
  trap 'echo; echo "stopped following; the run goes on on the box: parity/lxc131.sh --attach picks it up" >&2; exit 130' INT TERM
  follow_run "$1"
  trap - INT TERM
  if [ "$code" != 75 ]; then pull_reports "$1"; fi
  if [ "$code" != 0 ]; then echo "box-run.sh exit status: $code" >&2; fi
  exit "$code"
}

status_line="$(box_tool --status "$name")"
read -r st id spid _ <<< "$status_line"

if [ "$mode" = attach ]; then
  case "$st" in
    none) echo "no run of $name on the box to attach to" >&2; exit 1 ;;
    foreground)
      echo "the run of $name on the box (pid $spid) was started by an lxc131.sh from before --attach;" \
        "it cannot be followed. parity/lxc131.sh --queue shows it." >&2
      exit 1
      ;;
  esac
  echo "attaching to the run of $name $id ($st)"
  if [ "$st" = queued ] || [ "$st" = running ]; then start_watchdog "$id"; fi
  follow_and_finish "$id"
fi

case "$st" in
  queued | running | foreground)
    echo "a run of $name is still going on the box ($st); parity/lxc131.sh --attach follows it" >&2
    exit 75
    ;;
esac

if [ "$clean" = 1 ]; then
  on_box "docker rmi octo-rust-parity:$name 2> /dev/null; rm -rf /opt/octo-parity/runs/$name; git -C /opt/octo-parity/repo.git update-ref -d refs/heads/$name 2> /dev/null; df -h / | tail -n 1"
  exit 0
fi

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  if [ "$prebuilt" = 1 ]; then
    # The binary is built from the working tree; the box checks out HEAD. They must agree.
    echo "commit first: the binary is built from the working tree and the box runs HEAD" >&2
    exit 1
  fi
  echo "note: uncommitted changes are not sent; the box runs $(git rev-parse --short HEAD)" >&2
fi
sha="$(git rev-parse HEAD)"

# The binary, compiled here (see above).
musl_target=x86_64-unknown-linux-musl
binary="$repo/target/$musl_target/release/octo"
if [ "$prebuilt" = 1 ]; then
  echo "building octo for $musl_target here"
  # One of WSL's three build slots (~/bin/cargo-slot), when it is there, so a parity build
  # never makes a fourth compile beside the port agents' own.
  slot=()
  if [ -x "$HOME/bin/cargo-slot" ]; then slot=("$HOME/bin/cargo-slot"); fi
  # The harness compares with C# 2026.10.05, so the binary claims that release (build.rs reads
  # OCTO_VERSION first). A VERSION ahead of it, as on a release branch, would otherwise differ in
  # every answer that names the release. PARITY_OCTO_VERSION picks another.
  OCTO_VERSION="${PARITY_OCTO_VERSION:-2026.10.05}" \
    "${slot[@]}" cargo build --release --locked --package octo --bin octo --target "$musl_target" --quiet
  echo "built $(du -h "$binary" | cut -f1) binary from $(git rev-parse --short HEAD)"
fi

# Only the commits the box does not have yet.
on_box "mkdir -p /opt/octo-parity/incoming && { [ -d /opt/octo-parity/repo.git ] || git init -q --bare /opt/octo-parity/repo.git; }"
known="$(on_box "git -C /opt/octo-parity/repo.git for-each-ref --format='%(objectname)'" || true)"
nots=()
for k in $known; do
  if git cat-file -e "$k^{commit}" 2> /dev/null; then nots+=("^$k"); fi
done
bundle="parity-$name.bundle"
rm -f "$work/$bundle"
if git bundle create "$work/$bundle" HEAD "${nots[@]}" > /dev/null 2>&1; then
  echo "sending $(du -h "$work/$bundle" | cut -f1) of commits to the box"
  (cd "$work" && "$SCP" -q "$bundle" "$host:/root/$bundle")
  "$SSH" "$host" "pct push $ct /root/$bundle /opt/octo-parity/incoming/$bundle && rm -f /root/$bundle"
  fetch="git -C /opt/octo-parity/repo.git fetch -q /opt/octo-parity/incoming/$bundle HEAD:refs/heads/$name --force && rm -f /opt/octo-parity/incoming/$bundle"
else
  # The box has HEAD already.
  fetch="git -C /opt/octo-parity/repo.git update-ref refs/heads/$name $sha"
fi
rm -f "$work/$bundle"

checkout="set -e; $fetch
run=/opt/octo-parity/runs/$name
mkdir -p \$run
if [ ! -d \$run/src/.git ]; then git clone -q /opt/octo-parity/repo.git \$run/src; fi
cd \$run/src
git fetch -q origin $name
git checkout -q -f $sha
git clean -q -fdx
echo \"box: $name at \$(git log --oneline -1)\""
on_box "$checkout"

if [ "$prebuilt" = 1 ]; then
  # Into the checkout, after its git clean; box-run.sh --prebuilt wraps it in the runtime stage.
  "$SCP" -q "$(command -v wslpath > /dev/null && [ "$SCP" = scp.exe ] && wslpath -w "$binary" || echo "$binary")" "$host:/root/parity-$name.octo"
  "$SSH" "$host" "pct exec $ct -- mkdir -p /opt/octo-parity/runs/$name/src/.parity-bin && pct push $ct /root/parity-$name.octo /opt/octo-parity/runs/$name/src/.parity-bin/octo --perms 755 && rm -f /root/parity-$name.octo"
  args=(--prebuilt "${args[@]+"${args[@]}"}")
fi

host_free_mb() { "$SSH" -o ConnectTimeout=20 "$host" "awk '/MemAvailable/ {print int(\$2 / 1024)}' /proc/meminfo" 2> /dev/null || echo 0; }
need="${PARITY_MIN_HOST_MB:-3500}"
free="$(host_free_mb)"
if [ "$selftest" = 0 ] && [ "$free" -lt "$need" ]; then
  echo "the Proxmox host has $free MB free (need $need); not starting. Try again later." >&2
  exit 1
fi

# The run itself, detached on the box. One run at a time on the box: every branch's run uses
# the same stack names, and two at once would push the host into swap. box-run.sh takes
# /opt/octo-parity/run.lock itself, and a second run waits there for the first to finish
# (--queue shows who holds it and who waits).
quoted=""
for a in "${args[@]+"${args[@]}"}"; do quoted+=" $(printf '%q' "$a")"; done
id="$(on_box "bash $boxrun/src/parity/box-run.sh --start $name$quoted" | tail -n 1)"
if ! [[ "$id" =~ ^[0-9]{8}-[0-9]{6}$ ]]; then
  echo "box-run.sh --start did not give a run id (got '$id')" >&2
  exit 1
fi
echo "box: run $name $id started, detached. If this script stops, the run goes on:"
echo "  parity/lxc131.sh --attach follows it again; parity/lxc131.sh --queue shows the box's queue"
start_watchdog "$id"
follow_and_finish "$id"
