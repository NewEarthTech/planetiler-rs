#!/usr/bin/env bash
# Watch a detached planet bench box, stream its log, ship the durable
# diagnostics home *continuously*, and only terminate once they are provably
# collected.
#
# run.sh's gb-ab/europe-ab jobs poll inline because they finish inside an hour.
# The planet job detaches instead: it runs for hours, and a laptop-side ssh
# session held open that long has flaked repeatedly (sleep, wifi, VPN). This
# script is the collector, and it is safe to kill and re-run at any point — the
# job is nohup'd on the box and does not care whether anyone is watching.
#
# Hard lesson from planet runs C and D: they walled ~100 min in and cost real
# money, but *no planetiler-rs log line ever came home*. The old collector only
# scp'd at the very end, by which point the box was already in page-reclaim and
# the transfer got zero bytes — so every diagnosis was inference from RSS shape,
# not from a log. This collector fixes that three ways:
#   1. It scps the durable stderr + mem log home every poll, not just at the
#      end, so the last good snapshot survives even if the box later wedges.
#   2. It aborts the run cheap if nothing ever ships in the first ~5 min — a
#      broken ssh/scp path is a minute-5 discovery, not a minute-100 one.
#   3. On DONE/FAIL it verifies the fetched bytes against the box BEFORE
#      terminating, and keeps the box on any fetch failure (or on FAIL, so a
#      walled probe can be re-dispatched onto the same warm box).
#
# Usage: services/cloud-runner/watch.sh <ip> [max-hours] [poll-seconds]
#   KEEP=1 in the environment leaves the box up even on a clean DONE.
set -euo pipefail

IP=${1:?instance ip}
MAX_HOURS=${2:-14}
POLL=${3:-80}
KEEP=${KEEP:-0}
REGION=${AWS_REGION:-us-west-2}
NAME=planetiler-bench
KEY_FILE=$HOME/.ssh/$NAME.pem
HERE=$(cd "$(dirname "$0")" && pwd)
OUT_DIR="$HERE/results"
AWS() { aws --region "$REGION" "$@"; }
SSH_OPTS=(-i "$KEY_FILE" -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=30)
# The values we interpolate into remote commands (line numbers, the two literal
# artifact filenames) are all local and trusted, so client-side expansion is
# exactly what we want here.
# shellcheck disable=SC2029
SSH() { ssh "${SSH_OPTS[@]}" "ubuntu@$IP" "$@"; }

mkdir -p "$OUT_DIR"
STAMP=$(date +%Y%m%d-%H%M%S)
# One set of files per watch invocation, refreshed in place every poll: the
# file's final state IS the record, so there is no separate copy step and a
# later wedge cannot erase what already came home.
JOB_LOCAL="$OUT_DIR/planet-job-$STAMP.log"
STDERR_LOCAL="$OUT_DIR/planet-rs-$STAMP.stderr"
MEM_LOCAL="$OUT_DIR/planet-mem-$STAMP.log"

# Portable byte size (this runs on the laptop; darwin stat differs from GNU).
fsize() { [ -f "$1" ] && wc -c < "$1" | tr -d ' ' || echo 0; }

# scp one remote file home atomically: land it at a temp path and only swap it
# over the kept snapshot if it arrived non-empty, so a truncated or failed
# transfer never clobbers the last good copy. Echoes the resulting local size.
fetch_file() {  # $1 = remote name, $2 = local dest
  local remote=$1 dest=$2 tmp="$2.part"
  if scp -q "${SSH_OPTS[@]}" "ubuntu@$IP:$remote" "$tmp" 2>/dev/null && [ -s "$tmp" ]; then
    mv -f "$tmp" "$dest"
  else
    rm -f "$tmp"
  fi
  fsize "$dest"
}
remote_size() { SSH "stat -c %s '$1' 2>/dev/null" || true; }  # bytes, or empty if unreachable

IID=$(AWS ec2 describe-instances \
  --filters "Name=ip-address,Values=$IP" "Name=instance-state-name,Values=running" \
  --query 'Reservations[0].Instances[0].InstanceId' --output text)
[ -n "$IID" ] && [ "$IID" != None ] || { echo "no running instance at $IP" >&2; exit 1; }
echo "watching $IID at $IP (region $REGION, poll ${POLL}s)"
echo "live artifacts (refreshed every poll):"
echo "  $STDERR_LOCAL"
echo "  $MEM_LOCAL"

LAST_LINES=0
STATUS=unknown
RUNNING_POLLS=0
SHIP_CONFIRMED=0
# ~5 min of RUNNING with not one byte shipped = the ship path is broken.
GRACE_POLLS=$(( 300 / POLL + 1 ))

for _ in $(seq 1 $((MAX_HOURS * 3600 / POLL))); do
  # An unreachable box is not a failed job — the instance may be busy or the
  # local network may have blinked. Keep polling; only status says otherwise.
  STATUS=$(SSH 'cat status 2>/dev/null' || echo unreachable)
  TOTAL=$(SSH 'wc -l < job.log 2>/dev/null' || echo 0)
  if [ "$TOTAL" -gt "$LAST_LINES" ] 2>/dev/null; then
    SSH "sed -n '$((LAST_LINES + 1)),${TOTAL}p' job.log" || true
    LAST_LINES=$TOTAL
  fi

  # Ship the durable diagnostics home every poll. job.log is the early canary
  # (it exists from the first deps line); planet-rs.stderr and planet-mem.log
  # appear once the actual run starts, after the release build.
  joblog_sz=$(fetch_file job.log "$JOB_LOCAL")
  stderr_sz=$(fetch_file planet-rs.stderr "$STDERR_LOCAL")
  mem_sz=$(fetch_file planet-mem.log "$MEM_LOCAL")
  if [ "$SHIP_CONFIRMED" = 0 ] && [ "${stderr_sz:-0}" -gt 0 ] 2>/dev/null; then
    echo "== run capture confirmed live: planet-rs.stderr ${stderr_sz}B home"
    SHIP_CONFIRMED=1
  fi

  if [ "$STATUS" = RUNNING ]; then RUNNING_POLLS=$((RUNNING_POLLS + 1)); fi
  # Cheap abort: box has said RUNNING for ~5 min and job.log has never reached
  # us — ssh works (we just read status) but scp does not, so no diagnostic can
  # ever come home. Stop paying for a run we could not learn anything from.
  if [ "$RUNNING_POLLS" -ge "$GRACE_POLLS" ] && [ "${joblog_sz:-0}" -eq 0 ] 2>/dev/null; then
    echo "ABORT: RUNNING ~$((RUNNING_POLLS * POLL))s but 0 bytes ever shipped home — ship path broken; terminating cheap" >&2
    AWS ec2 terminate-instances --instance-ids "$IID" >/dev/null || true
    exit 1
  fi

  case "$STATUS" in DONE|FAIL) break ;; esac
  sleep "$POLL"
done
echo "status: $STATUS"

echo "== final fetch + verify"
scp -q -r "${SSH_OPTS[@]}" "ubuntu@$IP:results/*" "$OUT_DIR/" 2>/dev/null || true
joblog_sz=$(fetch_file job.log "$JOB_LOCAL")
stderr_sz=$(fetch_file planet-rs.stderr "$STDERR_LOCAL")
mem_sz=$(fetch_file planet-mem.log "$MEM_LOCAL")

# "Provably home": compare what we hold against what the box still has. A short
# read (box wedged mid-scp) or an empty file means the diagnostics did NOT come
# home — keep the box so nothing is lost. If the box is unreachable the remote
# size is empty and we fall back to trusting the last good periodic snapshot.
rmt_stderr=$(remote_size planet-rs.stderr)
rmt_mem=$(remote_size planet-mem.log)
echo "stderr: local ${stderr_sz}B / remote ${rmt_stderr:-unreachable}B"
echo "mem:    local ${mem_sz}B / remote ${rmt_mem:-unreachable}B"

collected=1
[ "${stderr_sz:-0}" -gt 0 ] 2>/dev/null || collected=0
[ "${mem_sz:-0}" -gt 0 ] 2>/dev/null || collected=0
if [ -n "$rmt_stderr" ] && [ "${stderr_sz:-0}" -lt "$rmt_stderr" ] 2>/dev/null; then collected=0; fi
if [ -n "$rmt_mem" ] && [ "${mem_sz:-0}" -lt "$rmt_mem" ] 2>/dev/null; then collected=0; fi

# The tile-set delta (tiles-delta.txt + the *.coords.gz sets) only exists on a
# completed head-to-head, and it is the one artifact that cannot be regenerated
# once the archives die with the box. If the box produced it but we do not hold
# a full copy, keep the box — same rule as the stderr/mem diagnostics. Silent
# when there is no delta (a failed or non-planet run never made the file).
rmt_delta=$(remote_size results/tiles-delta.txt)
delta_sz=$(fsize "$OUT_DIR/tiles-delta.txt")
echo "delta:  local ${delta_sz}B / remote ${rmt_delta:-none}B"
if [ -n "$rmt_delta" ] && [ "${rmt_delta:-0}" -gt 0 ] 2>/dev/null \
   && [ "${delta_sz:-0}" -lt "$rmt_delta" ] 2>/dev/null; then collected=0; fi

keep_reason=""
if [ "$collected" = 0 ]; then
  keep_reason="diagnostics NOT fully collected"
elif [ "$STATUS" != DONE ]; then
  keep_reason="status=$STATUS — kept for inspection / redispatch"
elif [ "$KEEP" = 1 ]; then
  keep_reason="KEEP=1"
fi

if [ -z "$keep_reason" ]; then
  echo "diagnostics verified home; terminating $IID"
  AWS ec2 terminate-instances --instance-ids "$IID" >/dev/null || true
  echo "results in $OUT_DIR"
else
  echo "leaving $IID up — $keep_reason (boot failsafe still armed)" >&2
  echo "  ssh -i $KEY_FILE ubuntu@$IP" >&2
  echo "  iterate a fix:  $HERE/redispatch.sh $IP planet <ref>" >&2
  echo "  kill it now:    aws --region $REGION ec2 terminate-instances --instance-ids $IID" >&2
  if [ "$collected" = 1 ]; then exit 0; else exit 1; fi
fi
