#!/usr/bin/env bash
# Runs ON the planet benchmark instance (launched by run.sh's `planet` job).
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
# Any exit that didn't reach DONE — set -e, explicit exit, signal, OOM kill —
# marks FAIL so the watcher stops instead of polling RUNNING forever.
finalize() { [ "$(cat status 2>/dev/null)" = DONE ] || echo FAIL > status; }
trap finalize EXIT

mkdir -p results
REPORT="results/planet-$(date -u +%Y%m%d-%H%M%S).txt"
say() { echo "$@" | tee -a "$REPORT"; }

echo "== deps"
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
  build-essential protobuf-compiler sqlite3 time git curl openjdk-21-jre-headless >/dev/null
if ! command -v cargo >/dev/null; then
  curl -fsS https://sh.rustup.rs | sh -s -- -y --profile minimal -q
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

echo "== instance-store scratch"
for _ in $(seq 1 90); do mountpoint -q /data && break; sleep 2; done
mountpoint -q /data || { echo "no /data mount" >&2; exit 1; }
mkdir -p /data/tmp /data/out
export TMPDIR=/data/tmp

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

echo "== build $REF"
git clone -q repo.bundle repo
BIN="$HOME/ptrs"
(cd repo && git checkout -q "$REF" && cargo build --release -q \
  && cp target/release/planetiler-rs "$BIN")

TOKEN=$(curl -fsS -X PUT http://169.254.169.254/latest/api/token \
  -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' 2>/dev/null || true)
ITYPE=$(curl -fsS -H "X-aws-ec2-metadata-token: $TOKEN" \
  http://169.254.169.254/latest/meta-data/instance-type 2>/dev/null || echo unknown)

say "# planet  $(date -u +%Y-%m-%dT%H:%M:%SZ)"
say "instance: $ITYPE  cores: $(nproc)  kernel: $(uname -sr)  arch: $(uname -m)"
say "rustc: $(rustc --version)"
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
#   2. A sampler records VmRSS + major-fault count every 10 s to the same
#      durable spot, and a watchdog SIGKILLs the run if RSS crosses a ceiling —
#      so a memory blowup is captured and killed cleanly on one process instead
#      of taking the box down. Peak RSS and wall come from the sampler, so
#      /usr/bin/time (which only sees the wrapper, not the child) is dropped.
RSS_CEILING_KB=$(( 220 * 1024 * 1024 ))   # of the box's ~247 GB usable
RS_STDERR="$HOME/planet-rs.stderr"
MEM_LOG="$HOME/planet-mem.log"

sample_and_guard() {  # $1 = pid to watch
  local pid=$1 rss majflt avail load
  echo "ts vmrss_kb majflt memavail_kb load1" > "$MEM_LOG"
  while kill -0 "$pid" 2>/dev/null; do
    rss=$(awk '/^VmRSS:/{print $2}' "/proc/$pid/status" 2>/dev/null)
    majflt=$(awk '{print $12}' "/proc/$pid/stat" 2>/dev/null)
    avail=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)
    load=$(awk '{print $1}' /proc/loadavg)
    echo "$(date +%s) ${rss:-0} ${majflt:-0} ${avail:-0} ${load:-0}" >> "$MEM_LOG"
    if [ "${rss:-0}" -gt "$RSS_CEILING_KB" ] 2>/dev/null; then
      echo "WATCHDOG $(date -u +%H:%M:%SZ): VmRSS ${rss}KB over ceiling; killing pid $pid" \
        | tee -a "$MEM_LOG" "$REPORT"
      kill -KILL "$pid" 2>/dev/null
      return
    fi
    sleep 10
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

PEAK_KB=$(awk 'NR>1 && $2>m{m=$2} END{print m+0}' "$MEM_LOG")
# The pipeline's own stage timers and the two pre-flight unknowns (run-index
# heap, merge chunk count), pulled from the durable stderr after the fact.
grep -E "sparse node store|k-way merge|chunk|pass 1|pass 2|stream \+ render|merge \+ encode" \
  "$RS_STDERR" | sed -E 's/^[[:space:]]+//' | tee -a "$REPORT" || true
say "rs wall_s: $((RS_END - RS_START))  peak_rss_kb: ${PEAK_KB}  peak_rss_gb: $(( PEAK_KB / 1024 / 1024 ))  rc: $RS_RC"
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

say ""
say "report: $REPORT"
echo DONE > status
