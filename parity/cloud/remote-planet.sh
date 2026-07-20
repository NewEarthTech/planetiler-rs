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
  echo "$(cat "$JAR.sha256")  $JAR" | sha256sum -c - >/dev/null \
    || { echo "jar sha256 mismatch" >&2; exit 1; }
fi

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

# Timed runs capture the pipeline's own stage timers alongside wall/RSS. The
# node-store and merge lines are the pre-flight unknowns (run-index heap and
# chunk count) finally being measured on a real planet.
say "## planetiler-rs ($REF)"
RS_OK=1
/usr/bin/time -v "$BIN" generate \
  --source "$INPUT" --output /data/out/rs.mbtiles \
  --layer osm --minzoom 12 --maxzoom 12 \
  --storage sparse --sort-memory-mb 1024 \
  --min-size 0 --simplify 0 --buffer 0 --force 2>&1 \
  | grep -E "sparse node store|k-way merge|chunk count|pass 1|pass 2|stream \+ render|merge \+ encode|Elapsed \(wall|Maximum resident" \
  | sed -E 's/^[[:space:]]+//' | tee -a "$REPORT" || RS_OK=0

if [ "$RS_OK" = 1 ] && [ -f /data/out/rs.mbtiles ]; then
  say "rs tiles: $(tiles_in /data/out/rs.mbtiles)"
  say "rs bytes: $(stat -c %s /data/out/rs.mbtiles)"
else
  say "rs: FAILED — skipping the upstream run rather than buying JVM hours on a dead comparison"
  say ""
  say "scratch after failure: $(du -sh /data/tmp 2>/dev/null | cut -f1)"
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
