#!/usr/bin/env bash
# A run on the machine you are sitting at. Reached through `just run <job>`
# (run-job.sh picks this when it isn't on a bench box), never called directly
# in normal use.
#
# It is the local twin of remote-planet.sh, and deliberately writes the same
# three durable files under one run-keyed directory:
#
#   ~/.planetiler-rs/runs/<run-id>/
#     job.log         this script's narration        -> console stream <run>/job
#     planetiler.log  the CLI's stdout+stderr        -> console stream <run>/planetiler
#     mem.log         the memory sampler, same columns as the box's
#     status          RUNNING | DONE | FAIL
#     results/        the run report
#     out/            the archive
#
# That layout is the whole trick behind watching a local run in the console:
# runner-live's `local` source tails these files exactly the way its Live Tail
# source consumes the box's CloudWatch streams, so the same parser, progress
# model, and log window light up with no cloud in the picture. Because the
# files are the record -- not a socket -- the console can attach at any point,
# including long after the run started, and still see it from line one.
#
# What it does NOT do, on purpose:
#   - no AWS credentials are touched unless you ask for --save-s3
#   - no CloudWatch drain, no instance profile, no S3 by default
#   - no memory watchdog by default: the box's ceilings are sized for a 256 GB
#     r7gd and killing a laptop run at 200 GB anon is not a service. Set
#     PTRS_MEM_CEILING_GB to arm one.
#
# Usage: local-run.sh <job> [--ref <label>] [--run-id <id>] [--save-s3 [uri]] [--no-build]
#   env: PTRS_RUNS_DIR (default ~/.planetiler-rs/runs)
#        PTRS_INPUT_DIR (default ~/.planetiler-rs/inputs) -- the input cache
#        PTRS_INPUT      an existing .osm.pbf to use instead of downloading
#        PTRS_BIN        an existing planetiler-rs binary (skips the build)
#        PTRS_OUT        archive path (default <run-dir>/out/<job>.mbtiles)
#        PTRS_TMPDIR     scratch for the external sort (default: system TMPDIR)
#        PTRS_MEM_CEILING_GB  kill the run above this RSS (default: unarmed)
#        PTRS_SAVE_S3 / PTRS_RESULTS_S3  see --save-s3
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=services/cloud-runner/jobs.sh
. "$HERE/jobs.sh"

JOB=${1:?job name}
shift
job_spec "$JOB"

REF=""
RUN_ID=${RUN_ID:-}
BUILD=1
SAVE_S3=${PTRS_SAVE_S3:-0}
S3_DEST=${PTRS_RESULTS_S3:-}
while [ $# -gt 0 ]; do
  case "$1" in
    --ref) REF=${2:?--ref needs a label}; shift 2 ;;
    --run-id) RUN_ID=${2:?--run-id needs an id}; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    --save-s3)
      SAVE_S3=1
      # Optional destination: `--save-s3 s3://my-bucket` overrides the default.
      if [ "${2:-}" != "" ] && [ "${2#s3://}" != "${2:-}" ]; then S3_DEST=$2; shift; fi
      shift ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

RUNS_DIR=${PTRS_RUNS_DIR:-$HOME/.planetiler-rs/runs}
INPUT_DIR=${PTRS_INPUT_DIR:-$HOME/.planetiler-rs/inputs}
# Same id shape as the cloud (`<job>-<utc stamp>`), so one run is one id
# everywhere: the run directory, the console's picker, the S3 prefix.
RUN_ID=${RUN_ID:-$JOB-$(date -u +%Y%m%d-%H%M%S)}
RUN_DIR=$RUNS_DIR/$RUN_ID
mkdir -p "$RUN_DIR/results" "$RUN_DIR/out" "$INPUT_DIR"

JOB_LOG=$RUN_DIR/job.log
PTRS_LOG=$RUN_DIR/planetiler.log
MEM_LOG=$RUN_DIR/mem.log
REPORT="$RUN_DIR/results/$JOB-$(date -u +%Y%m%d-%H%M%S).txt"
OUT=${PTRS_OUT:-$RUN_DIR/out/$JOB.mbtiles}

# Everything this script says goes to job.log as well as the terminal -- the
# box's dispatch does this with a shell redirect, we do it with tee, and the
# console sees the same stream either way.
exec > >(tee -a "$JOB_LOG") 2>&1

echo RUNNING > "$RUN_DIR/status"

ship_results() {
  [ "$SAVE_S3" = 1 ] || return 0
  local dest=${S3_DEST:-s3://${RESULTS_BUCKET:-planetiler-rs-run-results}}/$RUN_ID
  if ! command -v aws >/dev/null; then
    echo "--save-s3 asked for, but no aws cli on PATH; results stay in $RUN_DIR"
    return 0
  fi
  local S3=(aws --region "${RESULTS_REGION:-us-east-1}" s3 cp --no-progress)
  if "${S3[@]}" --recursive "$RUN_DIR/results/" "$dest/results/" >/dev/null 2>&1; then
    echo "results shipped: $dest/results/"
  else
    echo "results ship to $dest failed (credentials? bucket?) -- they are still in $RUN_DIR"
    return 0
  fi
  local f
  for f in "$PTRS_LOG" "$MEM_LOG" "$JOB_LOG"; do
    [ -f "$f" ] && "${S3[@]}" "$f" "$dest/$(basename "$f")" >/dev/null 2>&1 || true
  done
}

# Any exit that didn't reach DONE marks FAIL, same contract as the box: the
# console's status chip and any watcher read this file, not the exit code.
finalize() {
  [ "$(cat "$RUN_DIR/status" 2>/dev/null)" = DONE ] || echo FAIL > "$RUN_DIR/status"
  ship_results
}
trap finalize EXIT

# --- host portability -------------------------------------------------------
# The box is always Ubuntu; a laptop is as often macOS, and /proc is the first
# thing that isn't there. Everything host-shaped is behind these four.
OS=$(uname -s)
cpus() { getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 1; }
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
           else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
fsize() { wc -c < "$1" | tr -d ' '; }
mem_total_gb() {
  if [ "$OS" = Darwin ]; then echo $(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 ))
  else awk '/^MemTotal:/{print int($2/1024/1024)}' /proc/meminfo; fi
}

# One sample row, same six columns as remote-planet.sh's sampler so the
# console's mem readout parses both without knowing which it is:
#   ts vmrss_kb anon_kb majflt memavail_kb load1
# On macOS anon RSS and major faults aren't cheaply available per-process, so
# anon repeats RSS and majflt is 0 -- the columns stay honest about shape, and
# the number that matters (RSS) is real.
sample_row() {
  local pid=$1 rss anon majflt avail load
  if [ "$OS" = Darwin ]; then
    rss=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')
    anon=$rss
    majflt=0
    avail=$(vm_stat 2>/dev/null | awk '/page size of/{p=$8} /Pages free/{f=$3} /Pages inactive/{i=$3}
      END{gsub(/\./,"",f); gsub(/\./,"",i); print int((f+i)*p/1024)}')
    load=$(uptime | sed -E 's/.*load averages?: ([0-9.]+).*/\1/')
  else
    rss=$(awk '/^VmRSS:/{print $2}' "/proc/$pid/status" 2>/dev/null)
    anon=$(awk '/^Anonymous:/{print $2}' "/proc/$pid/smaps_rollup" 2>/dev/null)
    majflt=$(awk '{print $12}' "/proc/$pid/stat" 2>/dev/null)
    avail=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)
    load=$(awk '{print $1}' /proc/loadavg)
  fi
  echo "$(date +%s) ${rss:-0} ${anon:-0} ${majflt:-0} ${avail:-0} ${load:-0}"
}

sample_and_guard() {  # $1 = pid to watch
  local pid=$1 row rss
  echo "ts vmrss_kb anon_kb majflt memavail_kb load1" > "$MEM_LOG"
  while kill -0 "$pid" 2>/dev/null; do
    row=$(sample_row "$pid")
    echo "$row" >> "$MEM_LOG"
    if [ -n "${PTRS_MEM_CEILING_GB:-}" ]; then
      rss=$(echo "$row" | awk '{print $2}')
      if [ "${rss:-0}" -gt $(( PTRS_MEM_CEILING_GB * 1024 * 1024 )) ] 2>/dev/null; then
        echo "WATCHDOG $(date -u +%H:%M:%SZ): RSS ${rss}KB over ${PTRS_MEM_CEILING_GB}GB ceiling; killing pid $pid" \
          | tee -a "$MEM_LOG"
        kill -KILL "$pid" 2>/dev/null
        return
      fi
    fi
    # 2 s: a local run is minutes, not hours, and the console's readout should
    # move while you watch it.
    sleep 2
  done
}
# ---------------------------------------------------------------------------

say() { echo "$@" | tee -a "$REPORT"; }

echo "== run $RUN_ID ($JOB, local)"
echo "   dir:     $RUN_DIR"
echo "   console: http://localhost:5173  (just ui)"

# The binary. PTRS_BIN skips the build entirely (the runner image's CLI takes
# this path on a box); otherwise build the working tree, because the point of a
# local run is usually to measure what you just changed.
if [ -n "${PTRS_BIN:-}" ]; then
  BIN=$PTRS_BIN
  [ -x "$BIN" ] || { echo "PTRS_BIN=$BIN is not executable" >&2; exit 1; }
elif [ "$BUILD" = 1 ]; then
  echo "== build (cargo build --release)"
  (cd "$REPO_ROOT" && cargo build --release --locked --bin planetiler-rs)
  BIN="$REPO_ROOT/target/release/planetiler-rs"
else
  BIN="$REPO_ROOT/target/release/planetiler-rs"
  [ -x "$BIN" ] || { echo "--no-build, but $BIN isn't there" >&2; exit 1; }
fi
[ -n "$REF" ] || REF=$(git -C "$REPO_ROOT" describe --always --dirty 2>/dev/null || echo working-tree)

# The input, cached across runs and length-verified exactly the way the box
# does it: curl exits 0 on a silently truncated transfer, so the size check is
# the real gate, not the exit status.
INPUT=${PTRS_INPUT:-$INPUT_DIR/$JOB_INPUT_FILE}
if [ -n "${PTRS_INPUT:-}" ]; then
  [ -f "$INPUT" ] || { echo "PTRS_INPUT=$INPUT does not exist" >&2; exit 1; }
  echo "== input (supplied): $INPUT"
else
  echo "== input (cached, length-verified): $JOB_INPUT_URL"
  EXPECT=$(curl -fsSIL "$JOB_INPUT_URL" | tr -d '\r' \
    | awk 'tolower($1)=="content-length:"{print $2}' | tail -1)
  [ -n "$EXPECT" ] || { echo "no content-length from $JOB_INPUT_URL" >&2; exit 1; }
  if [ -f "$INPUT" ] && [ "$(fsize "$INPUT")" = "$EXPECT" ]; then
    echo "already staged ($EXPECT bytes); skipping download"
  else
    curl -fSL --retry 3 -o "$INPUT" "$JOB_INPUT_URL"
  fi
  ACTUAL=$(fsize "$INPUT")
  [ "$ACTUAL" = "$EXPECT" ] || { echo "download size mismatch: got $ACTUAL want $EXPECT" >&2; exit 1; }
fi

if [ -n "${PTRS_TMPDIR:-}" ]; then
  mkdir -p "$PTRS_TMPDIR"
  export TMPDIR=$PTRS_TMPDIR
fi
# The k-way merge holds every spilled chunk open at once; the stock 256 soft
# limit on macOS is nowhere near enough at anything past city scale.
ulimit -n 8192 2>/dev/null || true
export RUST_LOG=${RUST_LOG:-info}

say "# $JOB  $(date -u +%FT%TZ)  (local)"
say "host: $(uname -sr)  arch: $(uname -m)  cores: $(cpus)  memory: $(mem_total_gb) GB"
say "ref: $REF"
say "input: ${PTRS_INPUT:+supplied }$INPUT"
say "input bytes: $(fsize "$INPUT")  sha256: $(sha256 "$INPUT" | cut -c1-16)"
say "flags: $(job_flags_line)"
say "fd limit: $(ulimit -n)"
say ""

rm -f "$OUT"
say "## planetiler-rs ($REF)"
START=$(date +%s)
"$BIN" generate --source "$INPUT" --output "$OUT" "${JOB_FLAGS[@]}" > "$PTRS_LOG" 2>&1 &
RS_PID=$!
sample_and_guard "$RS_PID" &
MON_PID=$!
set +e
wait "$RS_PID"; RS_RC=$?
set -e
kill "$MON_PID" 2>/dev/null || true
END=$(date +%s)

# Data rows only ($1 an epoch stamp), so the header and any watchdog trailer
# can't corrupt the max -- awk would string-compare "WATCHDOG" as the largest.
PEAK_KB=$(awk '$1 ~ /^[0-9]+$/ && $2>m{m=$2} END{print m+0}' "$MEM_LOG")
grep -E "sparse node store|k-way merge|chunk|pass 1|pass 2|stream \+ render|merge \+ encode" \
  "$PTRS_LOG" | sed -E 's/^[[:space:]]+//' | tee -a "$REPORT" || true
say "rs wall_s: $((END - START))  peak_rss_mb: $(( PEAK_KB / 1024 ))  rc: $RS_RC"
say "rs mem trajectory: $MEM_LOG ($(wc -l < "$MEM_LOG" | tr -d ' ') samples)"

if [ "$RS_RC" != 0 ] || [ ! -f "$OUT" ]; then
  say "rs: FAILED (rc=$RS_RC)"
  say "last log lines:"; tail -5 "$PTRS_LOG" | tee -a "$REPORT" || true
  echo DONE > "$RUN_DIR/status"   # a recorded failure is still a completed run
  say ""
  say "report: $REPORT"
  exit 0
fi

if command -v sqlite3 >/dev/null; then
  say "rs tiles: $(sqlite3 "$OUT" 'select count(*) from tiles' 2>/dev/null || echo '?')"
fi
say "rs bytes: $(fsize "$OUT")"
say "archive: $OUT"
say ""
say "report: $REPORT"
echo DONE > "$RUN_DIR/status"
