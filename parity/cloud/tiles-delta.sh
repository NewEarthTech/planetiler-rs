#!/usr/bin/env bash
# Compute the tile-set delta between a planetiler-rs archive and an upstream
# Planetiler archive built from the SAME input on the SAME box.
#
# The whole-planet head-to-head (bench/results/planet-headtohead.md) leaves rs
# with +176,701 tiles Java does not emit -- an rs SUPERSET, the opposite sign of
# the great-britain delta (there rs is a strict subset and the 16 java-only
# tiles are Berwick Bank + Dover TSS interior fills from closed-way area
# promotion). Explaining the planet delta needs the actual coordinate sets, but
# a planet run's archives die with the box: only what lands in results/ ships
# home. This runs on the box while BOTH archives still exist, computes the delta
# with sqlite set operations, and writes small artifacts into the results dir:
#
#   rs-tiles.coords.gz     every rs (z x y), gzipped         (record / re-diff)
#   java-tiles.coords.gz   every java (z x y), gzipped       (record / re-diff)
#   rs-only.coords.gz      tiles rs emits and java does not  (the +176,701)
#   java-only.coords.gz    tiles java emits and rs does not
#   tiles-delta.txt        counts + per-zoom split + a tile-size histogram of
#                          each one-sided set (interior fills are tiny, a few
#                          hundred bytes -- the size shape is the first
#                          characterization, no tile decode needed)
#
# The coord sets are tiny gzipped (~a few tens of MB for the whole planet, far
# less for the one-sided sets), so shipping them home is cheap. Verifiable on
# any rs/java pair: great-britain reproduces the known java-only=16 result, so
# this is exercised locally before it ever rides a planet run.
set -euo pipefail

RS=${1:?rs mbtiles}
JAVA=${2:?java mbtiles}
OUT=${3:?output dir}
mkdir -p "$OUT"

for db in "$RS" "$JAVA"; do
  [ -f "$db" ] || { echo "no archive at $db" >&2; exit 1; }
  # Both sides must carry Planetiler's compact schema (tiles_shallow keyed
  # zoom/column/row). v0.10.2 osm-qa and planetiler-rs both do; fail loudly
  # rather than silently diff nothing if that ever changes.
  sqlite3 "$db" \
    "select 1 from sqlite_master where type='table' and name='tiles_shallow'" \
    | grep -q 1 || { echo "$db has no tiles_shallow (not compact schema)" >&2; exit 1; }
done

echo "== full coordinate sets"
sqlite3 -noheader -separator ' ' "$RS" \
  'select zoom_level,tile_column,tile_row from tiles_shallow order by 1,2,3' \
  | gzip > "$OUT/rs-tiles.coords.gz"
sqlite3 -noheader -separator ' ' "$JAVA" \
  'select zoom_level,tile_column,tile_row from tiles_shallow order by 1,2,3' \
  | gzip > "$OUT/java-tiles.coords.gz"

echo "== one-sided sets (sqlite EXCEPT is exact)"
# Attach java to the rs connection so the set difference is a single query.
# EXCEPT sorts both inputs internally, so the output is deterministic.
sqlite3 -noheader -separator ' ' "$RS" "
attach '$JAVA' as j;
select zoom_level,tile_column,tile_row from tiles_shallow
except select zoom_level,tile_column,tile_row from j.tiles_shallow
order by 1,2,3;" | gzip > "$OUT/rs-only.coords.gz"
sqlite3 -noheader -separator ' ' "$RS" "
attach '$JAVA' as j;
select zoom_level,tile_column,tile_row from j.tiles_shallow
except select zoom_level,tile_column,tile_row from tiles_shallow
order by 1,2,3;" | gzip > "$OUT/java-only.coords.gz"

echo "== report -> $OUT/tiles-delta.txt"
# One session builds the one-sided sets as temp tables, then reports counts, a
# per-zoom split, and a tile-size histogram (joining each one-sided set back to
# its own `tiles` view for the compressed byte length). The histogram buckets
# resolve the known interior-fill sizes: the great-britain java-only set lands
# as 1x(<50: the empty tile) + 5x(100-199: Dover TSS) + 10x(300-499: Berwick).
sqlite3 "$RS" <<SQL | sed 's/[[:space:]]*$//' > "$OUT/tiles-delta.txt"
.mode list
attach '$JAVA' as j;
create temp table rs_only as
  select zoom_level z, tile_column x, tile_row y from tiles_shallow
  except select zoom_level, tile_column, tile_row from j.tiles_shallow;
create temp table java_only as
  select zoom_level z, tile_column x, tile_row y from j.tiles_shallow
  except select zoom_level, tile_column, tile_row from tiles_shallow;

.print 'tile-set delta: planetiler-rs vs Planetiler'
.print ''
.print '                     rs      java'
select '  total tiles     '
  || printf('%9d', (select count(*) from tiles_shallow))
  || printf('%9d', (select count(*) from j.tiles_shallow));
select '  one-sided       '
  || printf('%9d', (select count(*) from rs_only))
  || printf('%9d', (select count(*) from java_only));
select '  shared          '
  || printf('%9d', (select count(*) from tiles_shallow) - (select count(*) from rs_only));
.print ''
.print 'rs-only per zoom (tiles rs emits, java does not):'
select '  z' || z || '  ' || printf('%9d', count(*)) from rs_only group by z order by z;
.print ''
.print 'java-only per zoom (tiles java emits, rs does not):'
select '  z' || z || '  ' || printf('%9d', count(*)) from java_only group by z order by z;

.print ''
.print 'rs-only tile-size histogram (compressed bytes):'
select '  ' || bucket || '  ' || printf('%9d', n) from (
  select case
    when L < 50 then 'a <50    '
    when L < 100 then 'b 50-99  '
    when L < 200 then 'c 100-199'
    when L < 300 then 'd 200-299'
    when L < 500 then 'e 300-499'
    when L < 1000 then 'f 500-999'
    when L < 5000 then 'g 1k-5k  '
    else 'h 5k+    ' end as bucket, count(*) n
  from (select length(t.tile_data) L
        from rs_only r join tiles t
          on t.zoom_level=r.z and t.tile_column=r.x and t.tile_row=r.y)
  group by bucket order by bucket);

.print ''
.print 'java-only tile-size histogram (compressed bytes):'
select '  ' || bucket || '  ' || printf('%9d', n) from (
  select case
    when L < 50 then 'a <50    '
    when L < 100 then 'b 50-99  '
    when L < 200 then 'c 100-199'
    when L < 300 then 'd 200-299'
    when L < 500 then 'e 300-499'
    when L < 1000 then 'f 500-999'
    when L < 5000 then 'g 1k-5k  '
    else 'h 5k+    ' end as bucket, count(*) n
  from (select length(t.tile_data) L
        from java_only r join j.tiles t
          on t.zoom_level=r.z and t.tile_column=r.x and t.tile_row=r.y)
  group by bucket order by bucket);
SQL

echo "== artifacts in $OUT"
ls -lh "$OUT"/rs-tiles.coords.gz "$OUT"/java-tiles.coords.gz \
       "$OUT"/rs-only.coords.gz "$OUT"/java-only.coords.gz "$OUT"/tiles-delta.txt
echo "== tiles-delta.txt"
cat "$OUT/tiles-delta.txt"
