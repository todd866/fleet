#!/usr/bin/env bash
# heavy-slot — machine-wide limit on concurrent heavy jobs (test suites, builds).
#
# All agents on this Mac (Claude, Codex, Cursor; any project) share one pool of
# slots, so three projects' test runs cannot all start at once and push the
# load average past 100 on 10 cores. Wrap the heavy command:
#
#   heavy-slot -- dotnet test sim.Tests
#   heavy-slot -- npx vitest run
#   heavy-slot status            # who holds which slot
#
# Waits (polling every 5 s) until one of HEAVY_SLOTS slots is free, then runs the
# command holding that slot; the slot is released when the command exits, even
# on a crash (BSD lockf holds an advisory lock on an open file).
# HEAVY_SLOTS defaults to 3. HEAVY_SLOT_LABEL names the holder in `status`.
# Every job is logged to ~/.local/state/heavy-slot/jobs.jsonl (start, wait, duration, exit)
# for machine-report.
# Parsed whole before it runs: an in-place edit can't corrupt a running copy.
{
set -euo pipefail
DIR="${HOME}/.local/state/heavy-slot"
SLOTS="${HEAVY_SLOTS:-3}"
mkdir -p "$DIR"

if [ "${1:-}" = "status" ]; then
  limit="$(cat "$DIR/limit" 2>/dev/null || true)"; [[ "$limit" =~ ^[0-9]+$ ]] || limit="$SLOTS"
  for i in $(seq 1 "$SLOTS"); do
    f="$DIR/slot$i.info"
    if /usr/bin/lockf -t 0 "$DIR/slot$i.lock" true 2>/dev/null; then
      if [ "$i" -gt "$limit" ]; then echo "slot $i: off (limit $limit)"; else echo "slot $i: free"; fi
    else echo "slot $i: $(cat "$f" 2>/dev/null || echo busy)"; fi
  done
  exit 0
fi
[ "${1:-}" = "--" ] && shift
[ $# -gt 0 ] || { echo "usage: heavy-slot -- <command...> | heavy-slot status" >&2; exit 2; }

label="${HEAVY_SLOT_LABEL:-$(basename "$PWD")}"

# While this Mac is in use, on battery, or hot, heavy work leaves (the owner, 1 Nov 2026).
# offload-target names the worker. Agent sessions are refused with the remote command.
# Exempt: HEAVY_SLOT_LOCAL=1
# (the job truly needs this Mac: .git, local hardware, the installed app, signed-in Chrome),
# jobs on a worker (REMOTE_RUN=1), and launchd jobs (XPC_SERVICE_NAME is their label), which
# carry their own power guards (battery_guard, power-pause).
xpc="${XPC_SERVICE_NAME:-0}"
if [ "${HEAVY_SLOT_LOCAL:-}" != 1 ] && [ "${REMOTE_RUN:-}" != 1 ] && { [ "$xpc" = 0 ] || [[ "$xpc" == application.* ]]; }; then
  ot=0; worker="$("$HOME/.local/bin/offload-target" 2>/dev/null)" || ot=$?
  [ "$ot" = 2 ] && gentle_local=1
  if [ "$ot" = 0 ]; then
    {
      echo "heavy-slot: this Mac should stay cool (in use, on battery, or hot) and worker '$worker' can take it — run it there:"
      echo "    remote-run -- $*"
      echo "  (remote-run mirrors the worktree without .git or ignored files; --pull PATH copies results back.)"
      echo "  If it truly needs this Mac (.git, local hardware, the installed app), rerun with HEAVY_SLOT_LOCAL=1."
    } >&2
    exit 75
  fi
fi
# On a shared Mac, worker-governor may lower the slot count and ask for background policy
# while someone else is using the screen (files limit and mode; absent elsewhere).
# Cool-down gate (the owner, 1 Nov 2026: "I'd rather let it cool down so when it's needed it's fast,
# not run it slowly with no chance to recover"). If $DIR/cool holds a chip temperature in °C,
# a new job starts only once Glancebar reports thermal "nominal" and the chip at or below it.
# Waits at most HEAVY_SLOT_COOL_MAX seconds (default 1200), then runs anyway. Unknown readings
# never block. Running jobs are never paused.
cool_ok() {
  local max s chip th
  max="$(awk 'NR==1{print int($1)}' "$DIR/cool" 2>/dev/null)"; [ "${max:-0}" -gt 0 ] || return 0
  for gb in /Applications/Glancebar.app/Contents/MacOS/Glancebar "$HOME/Applications/Glancebar.app/Contents/MacOS/Glancebar"; do
    [ -x "$gb" ] && { s="$("$gb" --sample 2>/dev/null)"; break; }
  done
  chip="$(printf '%s' "${s:-}" | sed -n 's/.*"chipC":\([0-9]*\).*/\1/p')"
  th="$(printf '%s' "${s:-}" | sed -n 's/.*"thermal":"\([a-z]*\)".*/\1/p')"
  cool_reading="${chip:-?} °C, thermal ${th:-?}"
  [ -n "$th" ] && [ "$th" != nominal ] && return 1
  [ -n "$chip" ] && [ "$chip" -gt "$max" ] && return 1
  return 0
}
waited=0; cool_waited=0
while :; do
  if [ "$cool_waited" -lt "${HEAVY_SLOT_COOL_MAX:-1200}" ] && ! cool_ok; then
    [ "$cool_waited" -eq 0 ] && echo "heavy-slot: letting this Mac cool first ($cool_reading; target ≤ $(head -1 "$DIR/cool") °C)" >&2
    sleep 15; cool_waited=$((cool_waited + 15)); waited=$((waited + 15)); continue
  fi
  limit="$(cat "$DIR/limit" 2>/dev/null || true)"; [[ "$limit" =~ ^[0-9]+$ ]] || limit="$SLOTS"
  for i in $(seq 1 "$(( limit < SLOTS ? limit : SLOTS ))"); do
    lock="$DIR/slot$i.lock"
    # -t 0: do not wait on this slot; try the next one.
    if /usr/bin/lockf -t 0 "$lock" true 2>/dev/null; then
      echo "$label · pid $$ · $(date '+%H:%M:%S') · $*" | cut -c1-200 > "$DIR/slot$i.info"
      [ "$waited" -gt 0 ] && echo "heavy-slot: got slot $i after ${waited}s" >&2
      # The job runs under lockf as before; a thin bash records how it ended.
      export HS_LOG="$DIR/jobs.jsonl" HS_LABEL="$label" HS_SLOT="$i" HS_WAITED="$waited" HS_START="$(date +%s)"
      bg=(); { [ "$(cat "$DIR/mode" 2>/dev/null)" = gentle ] || [ "${gentle_local:-0}" = 1 ]; } && bg=(/usr/sbin/taskpolicy -b)
      [ "${gentle_local:-0}" = 1 ] && echo "heavy-slot: no worker free and cool; running here at background priority" >&2
      exec ${bg[@]+"${bg[@]}"} /usr/bin/lockf -k "$lock" /bin/bash -c '
        # Peak memory of the largest process in the job, for sizing a future Mac (the owner, 1 Nov 2026).
        t=$(mktemp -t heavy-slot-mem); /usr/bin/time -l -o "$t" "$@"; rc=$?
        mb=$(awk "/maximum resident set size/{printf \"%d\", \$1/1048576}" "$t" 2>/dev/null); rm -f "$t"
        esc() { local s="${1//\\/\\\\}"; s="${s//\"/\\\"}"; printf "%s" "${s//[$'"'"'\t\n\r'"'"']/ }"; }
        printf "{\"start\":%s,\"end\":%s,\"waited\":%s,\"slot\":%s,\"label\":\"%s\",\"rc\":%s,\"maxrss_mb\":%s,\"cmd\":\"%s\"}\n" \
          "$HS_START" "$(date +%s)" "$HS_WAITED" "$HS_SLOT" "$(esc "$HS_LABEL")" "$rc" "${mb:-null}" "$(esc "$(printf "%s " "$@" | cut -c1-300)")" >> "$HS_LOG"
        exit $rc' heavy-slot "$@"
    fi
  done
  [ "$waited" -eq 0 ] && echo "heavy-slot: all $SLOTS slots busy; waiting (heavy-slot status)" >&2
  sleep 5; waited=$((waited + 5))
done
exit
}
