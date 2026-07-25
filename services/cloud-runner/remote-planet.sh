#!/usr/bin/env bash
# Runs ON the planet benchmark instance (launched by run.sh's `planet` job over
# ssh, or by the run-api dispatch Lambda over SSM). Console dispatch sets
# PTRS_BIN to the CLI lifted prebuilt out of the runner image at requisition:
# no toolchain, no bundle, no build — REF then merely labels the run (the
# image tag/sha). Without PTRS_BIN, laptop behavior is unchanged.
#
# Unlike remote.sh this is not an A/B: it is the first whole-planet run, so the
# question is "does it complete, at what cost" against pinned upstream on the
# same box and input. planetiler-rs goes first and Planetiler runs only if it
# survives — a failed run should not also buy several hours of JVM time.
#
# Settings come from bench/results/planet-preflight.md and are not optional:
# --storage sparse (the `ram` default wants ~152 GB of heap at planet scale),
# --sort-memory-mb 1024 (the default spills ~1200 chunks and the merge opens
# every one of them at once), a raised fd ceiling for the same reason, and
# TMPDIR on the instance store (~350 GB of scratch, versus a 30 GB root disk).
set -euo pipefail

REF=${1:?ref to benchmark}
PLANET_URL=${2:?planet pbf url}
PLANETILER_VERSION=${3:-v0.10.2}

cd "$HOME"
echo RUNNING > status

# Ship results/ + the durable logs to a run-keyed S3 prefix before the box
# dies — the collection path for a console-dispatched box, which has no ssh
# for watch.sh to scp from (#59). Runs on FAIL too: a failed run's stderr and
# mem trajectory are the valuable part (the C/D lesson). Best-effort by
# design: no RUN_ID in the dispatch env (an old-style dispatch) or no
# instance profile and the run's outcome is unchanged — watch.sh still
# collects by scp either way.
ship_results() {
  [ -n "${RUN_ID:-}" ] || return 0
  # aws via snap, same reasoning as drain.sh (apt may not own the box's
  # locks). `|| true` so a failed install can't errexit the EXIT trap.
  command -v aws >/dev/null || sudo snap install aws-cli --classic >/dev/null 2>&1 || true
  command -v aws >/dev/null || { echo "no aws cli; results stay on the box"; return 0; }
  local dest="s3://${RESULTS_BUCKET:-planetiler-rs-run-results}/$RUN_ID"
  local S3=(aws --region "${RESULTS_REGION:-us-east-1}" s3 cp --no-progress)
  # An empty results/ ships nothing and creates no prefix — a cancel during
  # the download ends here — so only claim a ship that put objects somewhere.
  if [ -z "$(ls -A results 2>/dev/null)" ]; then
    echo "no results to ship (results/ is empty; the run ended before the report began)"
  elif "${S3[@]}" --recursive results/ "$dest/results/" >/dev/null 2>&1; then
    echo "results shipped: $dest/results/"
  else
    echo "results ship to $dest failed (no instance profile?)"
  fi
  local f
  for f in planet-rs.stderr planet-mem.log job.log; do
    [ -f "$f" ] && "${S3[@]}" "$f" "$dest/$f" >/dev/null 2>&1 || true
  done
}

# Any exit that didn't reach DONE — set -e, explicit exit, signal, OOM kill —
# marks FAIL so the watcher stops instead of polling RUNNING forever.
finalize() {
  [ "$(cat status 2>/dev/null)" = DONE ] || echo FAIL > status
  ship_results
}
trap finalize EXIT

mkdir -p results
REPORT="results/planet-$(date -u +%Y%m%d-%H%M%S).txt"
say() { echo "$@" | tee -a "$REPORT"; }

echo "== deps"
# PTRS_BIN (console dispatch): the CLI was lifted prebuilt out of the runner
# image at requisition, so the toolchain never installs — only the run/report
# deps do. Without it (laptop dispatch) the ref builds from the shipped
# bundle exactly as before.
sudo apt-get update -qq
if [ -n "${PTRS_BIN:-}" ]; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    sqlite3 time curl openjdk-21-jre-headless >/dev/null
else
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    build-essential protobuf-compiler sqlite3 time git curl openjdk-21-jre-headless >/dev/null
  if ! command -v cargo >/dev/null; then
    curl -fsS https://sh.rustup.rs | sh -s -- -y --profile minimal -q
  fi
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

echo "== instance-store scratch"
for _ in $(seq 1 90); do mountpoint -q /data && break; sleep 2; done
mountpoint -q /data || { echo "no /data mount" >&2; exit 1; }
mkdir -p /data/tmp /data/out
export TMPDIR=/data/tmp
# A watchdog-killed or rebooted run leaves its scratch behind (TempDir::drop
# never fires), and at planet scale that is hundreds of GB of node store and
# sort chunks. Clear stale scratch at startup so a re-dispatch onto the same
# box doesn't stack corpses until the disk fills. Nothing is running yet.
rm -rf /data/tmp/.tmp* /data/out/* 2>/dev/null || true

# The tracing filter defaults to info on its own, but be explicit so an
# inherited RUST_LOG=warn on the box can't silence the stage timers.
export RUST_LOG=${RUST_LOG:-info}

# The k-way merge holds every spilled chunk open simultaneously. Planet
# projects to ~300 chunks at --sort-memory-mb 1024 and ~1200 at the default,
# against a stock soft limit of 1024 — and the open fails only after the whole
# read/render stage has run. Raise it well past either number.
ulimit -n 65536
echo "fd limit: $(ulimit -n)"

echo "== input (pinned planet, length-verified)"
INPUT=/data/planet.osm.pbf
EXPECT=$(curl -fsSIL "$PLANET_URL" | tr -d '\r' \
  | awk 'tolower($1)=="content-length:"{print $2}' | tail -1)
[ -n "$EXPECT" ] || { echo "no content-length from $PLANET_URL" >&2; exit 1; }
if [ -f "$INPUT" ] && [ "$(stat -c %s "$INPUT")" = "$EXPECT" ]; then
  echo "input already present and complete ($EXPECT bytes); skipping download"
else
  # curl exits 0 on a silently truncated transfer, so the size check below is
  # the real gate, not the exit status. This bit us on the first europe fetch.
  curl -fsSL --retry 3 -o "$INPUT" "$PLANET_URL"
fi
ACTUAL=$(stat -c %s "$INPUT")
[ "$ACTUAL" = "$EXPECT" ] || {
  echo "download size mismatch: got $ACTUAL want $EXPECT" >&2; exit 1; }

echo "== pinned upstream jar $PLANETILER_VERSION"
JAR="$HOME/planetiler-$PLANETILER_VERSION.jar"
BASE="https://github.com/onthegomap/planetiler/releases/download/$PLANETILER_VERSION"
if [ ! -f "$JAR" ]; then
  curl -fsSL "$BASE/planetiler.jar" -o "$JAR"
  curl -fsSL "$BASE/planetiler.jar.sha256" -o "$JAR.sha256"
fi
# The published checksum file is `<hash>  planetiler.jar`, so feeding it to
# `sha256sum -c` fails on the filename, not the hash. Compare the hashes
# directly, the same way parity/fetch-upstream.sh does. Verified on every run,
# not just the download, so a half-written cached jar is caught too.
expected=$(cut -d' ' -f1 < "$JAR.sha256")
actual=$(sha256sum "$JAR" | cut -d' ' -f1)
[ "$expected" = "$actual" ] || {
  echo "jar sha256 mismatch: expected $expected got $actual" >&2; exit 1; }

if [ -n "${PTRS_BIN:-}" ]; then
  echo "== prebuilt binary $PTRS_BIN (from the runner image; REF labels the run)"
  [ -x "$PTRS_BIN" ] || { echo "PTRS_BIN=$PTRS_BIN is not executable" >&2; exit 1; }
  BIN=$PTRS_BIN
else
  echo "== build $REF"
  git clone -q repo.bundle repo
  BIN="$HOME/ptrs"
  (cd repo && git checkout -q "$REF" && cargo build --release -q \
    && cp target/release/planetiler-rs "$BIN")
fi

TOKEN=$(curl -fsS -X PUT http://169.254.169.254/latest/api/token \
  -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' 2>/dev/null || true)
ITYPE=$(curl -fsS -H "X-aws-ec2-metadata-token: $TOKEN" \
  http://169.254.169.254/latest/meta-data/instance-type 2>/dev/null || echo unknown)

say "# planet  $(date -u +%Y-%m-%dT%H:%M:%SZ)"
say "instance: $ITYPE  cores: $(nproc)  kernel: $(uname -sr)  arch: $(uname -m)"
if [ -n "${PTRS_BIN:-}" ]; then
  say "binary: prebuilt from runner image ($PTRS_BIN)"
else
  say "rustc: $(rustc --version)"
fi
say "java: $(java -version 2>&1 | head -1)"
say "ref: $REF  upstream: $PLANETILER_VERSION"
say "input: $PLANET_URL"
say "input bytes: $ACTUAL"
say "scratch: $(df -h /data | awk 'NR==2{print $2" total, "$4" free"}')"
say "memory: $(free -g | awk '/^Mem:/{print $2" GB"}')"
say ""

tiles_in() { sqlite3 "$1" 'select count(*) from tiles' 2>/dev/null || echo "?"; }

# The first planet attempt wedged the whole box: RSS climbed until the kernel
# was in permanent page-reclaim and sshd starved, so nothing could be read out
# and the diagnostic log — piped through grep, block-buffered — was lost with
# the reboot. Two fixes, both here:
#
#   1. The run's stderr goes straight to a file on the EBS root ($HOME), which
#      survives a wedge and a reboot, and is not behind a pipe buffer. It is
#      grepped for the report only after the run ends.
#   2. A sampler records VmRSS, anonymous RSS, and the major-fault count to the
#      same durable spot, and a watchdog SIGKILLs the run if memory crosses a
#      ceiling — so a blowup is captured and killed cleanly on one process
#      instead of taking the box down. Peak RSS and wall come from the sampler,
#      so /usr/bin/time (which only sees the wrapper, not the child) is dropped.
#
# The honest kill metric is anonymous RSS, not VmRSS: at planet scale VmRSS
# carries ~85 GB of node/way store that is file-backed and freely evictable, so
# the anon column is what actually OOMs the box (runs C and D died on ~100 GB of
# it climbing ~2.9 GB/s, major faults dead flat). Kill on anon > 200 GB; keep a
# VmRSS backstop below the ~247 GB wall for a file-backed pathology anon misses.
# In the C/D trajectory VmRSS RAM-limited near 246 before anon read 200, so the
# backstop is load-bearing there, not decoration — set with headroom, well above
# any healthy run (~130 GB). Sampling tightens from 10 s to 2 s past a danger
# line because a 10 s gap let VmRSS jump 29 GB between samples in C/D, so both
# the kill and the recorded trajectory overshot.
ANON_CEILING_KB=$(( 200 * 1024 * 1024 ))
RSS_CEILING_KB=$(( 235 * 1024 * 1024 ))
DANGER_KB=$(( 180 * 1024 * 1024 ))
RS_STDERR="$HOME/planet-rs.stderr"
MEM_LOG="$HOME/planet-mem.log"

sample_and_guard() {  # $1 = pid to watch
  local pid=$1 rss anon majflt avail load interval
  echo "ts vmrss_kb anon_kb majflt memavail_kb load1" > "$MEM_LOG"
  while kill -0 "$pid" 2>/dev/null; do
    rss=$(awk '/^VmRSS:/{print $2}' "/proc/$pid/status" 2>/dev/null)
    anon=$(awk '/^Anonymous:/{print $2}' "/proc/$pid/smaps_rollup" 2>/dev/null)
    majflt=$(awk '{print $12}' "/proc/$pid/stat" 2>/dev/null)
    avail=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)
    load=$(awk '{print $1}' /proc/loadavg)
    echo "$(date +%s) ${rss:-0} ${anon:-0} ${majflt:-0} ${avail:-0} ${load:-0}" >> "$MEM_LOG"
    if [ "${anon:-0}" -gt "$ANON_CEILING_KB" ] 2>/dev/null; then
      echo "WATCHDOG $(date -u +%H:%M:%SZ): Anonymous ${anon}KB over anon ceiling; killing pid $pid" \
        | tee -a "$MEM_LOG" "$REPORT"
      kill -KILL "$pid" 2>/dev/null
      return
    fi
    if [ "${rss:-0}" -gt "$RSS_CEILING_KB" ] 2>/dev/null; then
      echo "WATCHDOG $(date -u +%H:%M:%SZ): VmRSS ${rss}KB over backstop ceiling; killing pid $pid" \
        | tee -a "$MEM_LOG" "$REPORT"
      kill -KILL "$pid" 2>/dev/null
      return
    fi
    # 10 s steady, 2 s once memory is high — see the DANGER_KB note above.
    interval=10
    if [ "${rss:-0}" -gt "$DANGER_KB" ] 2>/dev/null || [ "${anon:-0}" -gt "$DANGER_KB" ] 2>/dev/null; then
      interval=2
    fi
    sleep "$interval"
  done
}

say "## planetiler-rs ($REF)"
RS_START=$(date +%s)
# planetiler-rs writes its tracing lines to STDOUT, not stderr, so capture
# stdout (merging stderr in). The first cut sent stdout to /dev/null and kept
# only stderr, which is why the run produced tiles but an empty log.
"$BIN" generate \
  --source "$INPUT" --output /data/out/rs.mbtiles \
  --layer osm --minzoom 12 --maxzoom 12 \
  --storage sparse --sort-memory-mb 1024 \
  --min-size 0 --simplify 0 --buffer 0 --force > "$RS_STDERR" 2>&1 &
RS_PID=$!
sample_and_guard "$RS_PID" &
MON_PID=$!
wait "$RS_PID"; RS_RC=$?
kill "$MON_PID" 2>/dev/null || true
RS_END=$(date +%s)

# Consider only data rows (epoch-timestamp first field), so the header and the
# tee'd "WATCHDOG ..." trailer can't corrupt the max — awk would otherwise
# string-compare a field like "Anonymous" as greater than any number.
PEAK_KB=$(awk '$1 ~ /^[0-9]+$/ && $2>m{m=$2} END{print m+0}' "$MEM_LOG")
PEAK_ANON_KB=$(awk '$1 ~ /^[0-9]+$/ && $3>m{m=$3} END{print m+0}' "$MEM_LOG")
# The pipeline's own stage timers and the two pre-flight unknowns (run-index
# heap, merge chunk count), pulled from the durable stderr after the fact.
grep -E "sparse node store|k-way merge|chunk|pass 1|pass 2|stream \+ render|merge \+ encode" \
  "$RS_STDERR" | sed -E 's/^[[:space:]]+//' | tee -a "$REPORT" || true
say "rs wall_s: $((RS_END - RS_START))  peak_vmrss_gb: $(( PEAK_KB / 1024 / 1024 ))  peak_anon_gb: $(( PEAK_ANON_KB / 1024 / 1024 ))  peak_rss_kb: ${PEAK_KB}  rc: $RS_RC"
say "rs mem trajectory: $MEM_LOG ($(wc -l < "$MEM_LOG") samples)"

if [ "$RS_RC" = 0 ] && [ -f /data/out/rs.mbtiles ]; then
  say "rs tiles: $(tiles_in /data/out/rs.mbtiles)"
  say "rs bytes: $(stat -c %s /data/out/rs.mbtiles)"
else
  say "rs: FAILED (rc=$RS_RC) — skipping the upstream run rather than buying JVM hours on a dead comparison"
  say "last stderr lines:"; tail -5 "$RS_STDERR" | tee -a "$REPORT" || true
  say "scratch at failure: $(du -sh /data/tmp 2>/dev/null | cut -f1)"
  echo DONE > status   # a recorded failure is still a completed job
  exit 0
fi
say ""

# planetiler-rs cleans its own scratch on a clean exit (TempDir drop), but not
# if it was killed; clear it either way so upstream gets the whole disk.
rm -rf /data/tmp/* 2>/dev/null || true

say "## Planetiler $PLANETILER_VERSION (stock defaults; nodemap_type=sparsearray, nodemap_storage=mmap)"
/usr/bin/time -v java -jar "$JAR" osm-qa \
  --osm_path="$INPUT" --output=/data/out/java.mbtiles \
  --tmpdir=/data/tmp --minzoom=12 --maxzoom=12 --force 2>&1 \
  | grep -E "Elapsed \(wall|Maximum resident|finished in|^\s*[0-9:]+ INF - \[" \
  | sed -E 's/^[[:space:]]+//' | tail -40 | tee -a "$REPORT" || true

if [ -f /data/out/java.mbtiles ]; then
  say "java tiles: $(tiles_in /data/out/java.mbtiles)"
  say "java bytes: $(stat -c %s /data/out/java.mbtiles)"
else
  say "java: FAILED"
fi

# Tile-set delta while both archives still exist on the box. The whole-planet
# head-to-head leaves rs a +176,701-tile superset of Java, opposite the sign of
# the country-scale delta; explaining it needs the actual coordinate sets, but a
# planet run's archives die with the instance (only results/ ships home). Distil
# the delta here -- full coord sets, the one-sided sets, per-zoom + size
# histograms -- into results/, which watch.sh collects. Non-fatal: a completed
# head-to-head is not lost to a delta hiccup.
if [ -f /data/out/rs.mbtiles ] && [ -f /data/out/java.mbtiles ]; then
  say ""
  say "## tile-set delta (rs vs java) -- results/tiles-delta.txt + *.coords.gz"
  bash "$HOME/tiles-delta.sh" /data/out/rs.mbtiles /data/out/java.mbtiles results \
    2>&1 | tee -a "$REPORT" || say "tiles-delta.sh failed (non-fatal)"
fi

say ""
say "report: $REPORT"
echo DONE > status
