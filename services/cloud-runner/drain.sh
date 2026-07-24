#!/usr/bin/env bash
# Runs ON the bench instance, beside the job (started detached by run.sh or
# redispatch.sh). Tails the run's durable files into CloudWatch Logs every few
# seconds so the runner console (runner.johncarmack.com) can watch the run
# live. The laptop-side pollers (run.sh, watch.sh) stay the collectors of
# record — this is the real-time view, not the archive.
#
# Additive by design: a separate nohup'd process with no shared state with the
# job, and every AWS call is best-effort. Credentials come from the instance
# profile run.sh attaches when the RunnerConsoleStack has been deployed; when
# the profile is missing every put fails and the drain gives up quietly —
# the run never notices either way.
#
# Usage: drain.sh <run-id>
#   env: DRAIN_GROUP (default /planetiler-rs/runs), DRAIN_REGION (default
#        us-east-1 — the group lives with RunnerConsoleStack, boxes write to
#        it cross-region), DRAIN_POLL seconds (default 5)
set -u

RUN_ID=${1:?run id}
GROUP=${DRAIN_GROUP:-/planetiler-rs/runs}
REGION=${DRAIN_REGION:-us-east-1}
POLL=${DRAIN_POLL:-5}
cd "$HOME" || exit 0

# Own deps: aws via snap (the job's apt-get may hold the dpkg lock right now;
# snap stays off it), python3 ships with the ubuntu cloud image.
command -v aws >/dev/null || sudo snap install aws-cli --classic >/dev/null 2>&1
command -v aws >/dev/null || { echo "no aws cli; drain disabled"; exit 0; }
CWL() { aws --region "$REGION" logs "$@"; }

# One stream per durable file. job.log exists from dispatch; the stderr and
# mem logs appear only once the actual planetiler run starts (planet job).
FILES=(job.log planet-rs.stderr planet-mem.log)
SUFFIXES=(job planetiler mem)
CURSORS=(0 0 0)
CREATED=(0 0 0)
# Batch ceiling in lines; build_payload additionally caps the batch near
# CloudWatch's 1 MiB put limit and reports how many lines it consumed.
MAX_BATCH=5000
PAYLOAD=$HOME/.drain-payload.json

SHIPPED=0
FAILS=0

# stdin = raw lines → events JSON at $2 (byte-capped), consumed count on stdout.
build_payload() {
  python3 -c '
import json, sys
ts, out = int(sys.argv[1]), sys.argv[2]
events, consumed, size = [], 0, 0
for line in sys.stdin:
    msg = line.rstrip("\n")[:6000]
    if msg.strip():
        size += len(msg.encode()) + 26
        if size > 900_000 and events:
            break
        events.append({"timestamp": ts, "message": msg})
    consumed += 1
with open(out, "w") as f:
    json.dump(events, f)
print(consumed, len(events))
' "$1" "$2"
}

ship_file() {  # $1 = file index; returns 0 when the file is fully shipped
  local i=$1 file=${FILES[$1]} stream="$RUN_ID/${SUFFIXES[$1]}" total from to counts
  [ -f "$file" ] || return 0
  total=$(wc -l < "$file" 2>/dev/null || echo 0)
  # A redispatch clears the previous run's files; follow the truncation.
  [ "$total" -lt "${CURSORS[i]}" ] && CURSORS[i]=0
  [ "$total" -le "${CURSORS[i]}" ] && return 0
  from=$(( CURSORS[i] + 1 ))
  to=$(( CURSORS[i] + MAX_BATCH )); [ "$to" -gt "$total" ] && to=$total
  counts=$(sed -n "${from},${to}p" "$file" | build_payload "$(date +%s)000" "$PAYLOAD")
  local consumed=${counts% *} events=${counts#* }
  [ "${consumed:-0}" -gt 0 ] || return 0
  if [ "$events" = 0 ]; then  # blank lines only — nothing to put
    CURSORS[i]=$(( CURSORS[i] + consumed ))
    return 1
  fi
  if [ "${CREATED[i]}" = 0 ]; then
    # Already-exists is fine; a real failure surfaces on the put below.
    CWL create-log-stream --log-group-name "$GROUP" --log-stream-name "$stream" 2>/dev/null
    CREATED[i]=1
  fi
  if CWL put-log-events --log-group-name "$GROUP" --log-stream-name "$stream" \
       --log-events "file://$PAYLOAD" >/dev/null 2>&1; then
    CURSORS[i]=$(( CURSORS[i] + consumed ))
    SHIPPED=1; FAILS=0
  else
    FAILS=$(( FAILS + 1 ))
  fi
  [ "${CURSORS[i]}" -ge "$total" ]
}

echo "draining to $GROUP as $RUN_ID (region $REGION, poll ${POLL}s)"
while :; do
  STATUS=$(cat status 2>/dev/null || true)
  CAUGHT_UP=1
  for i in 0 1 2; do ship_file "$i" || CAUGHT_UP=0; done
  # Nothing has ever shipped and puts keep failing: no instance profile or no
  # deployed log group. Stop burning cycles — the run does not need us.
  if [ "$SHIPPED" = 0 ] && [ "$FAILS" -ge 12 ]; then
    echo "giving up: $FAILS consecutive put failures, nothing ever shipped (instance profile missing?)"
    exit 0
  fi
  case "$STATUS" in DONE|FAIL) [ "$CAUGHT_UP" = 1 ] && break ;; esac
  # Skip the sleep while healthily catching up on a backlog; still pace
  # ourselves when the backlog is due to failing puts.
  if [ "$CAUGHT_UP" = 1 ] || [ "$FAILS" -gt 0 ]; then sleep "$POLL"; fi
done
echo "run $STATUS; drain complete"
