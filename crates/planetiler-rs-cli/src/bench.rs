//! Benchmark & parity harness utilities.
//!
//! The one justification for this port is beating upstream Planetiler on
//! wall-clock time and peak memory, so the measurement plumbing is stood up in
//! Phase 0 — before planetiler-rs can produce a single tile — and every later
//! capability lands with a head-to-head number.
//!
//! This module captures **wall time** and **OS-level peak resident set size**
//! for an arbitrary child process by running it under `/usr/bin/time` and
//! parsing the report (macOS `-l` reports bytes; Linux `-v` reports kbytes).
//! The macro runner in `src/bin/bench.rs` drives it; the Rust-vs-Java table and
//! the semantic-MVT diff (which needs the decoder) arrive in Phase 1.
//!
//! Peak-RSS caveat: with mmap node stores, resident bytes understate true
//! footprint, so the harness must report the storage mode alongside the number
//! — see the fairness rules in `ARCHITECTURE.md`.

use std::fmt;
use std::process::Command;
use std::time::{Duration, Instant};

/// The result of timing one child process.
#[derive(Debug, Clone)]
pub struct RunMeasurement {
    /// Wall-clock time for the whole child invocation.
    pub wall: Duration,
    /// Peak resident set size in bytes, if it could be parsed from the report.
    pub peak_rss_bytes: Option<u64>,
    /// Whether the child exited successfully.
    pub exit_ok: bool,
}

impl fmt::Display for RunMeasurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wall={:.3}s", self.wall.as_secs_f64())?;
        match self.peak_rss_bytes {
            Some(b) => write!(f, " peak_rss={:.1}MB", b as f64 / (1024.0 * 1024.0))?,
            None => write!(f, " peak_rss=?")?,
        }
        write!(f, " exit={}", if self.exit_ok { "ok" } else { "fail" })
    }
}

/// Run `program args...` under `/usr/bin/time` and capture wall time + peak RSS.
///
/// The child's own stdout/stderr are captured (and discarded) so the timing
/// report can be parsed; this is a benchmark runner, not a pass-through shell.
///
/// # Errors
/// Returns an error if `/usr/bin/time` (or the child) cannot be spawned.
pub fn measure_command(program: &str, args: &[String]) -> anyhow::Result<RunMeasurement> {
    // macOS `time -l` prints "maximum resident set size" in bytes; GNU
    // `time -v` prints "Maximum resident set size (kbytes)".
    let flag = if cfg!(target_os = "macos") {
        "-l"
    } else {
        "-v"
    };

    let start = Instant::now();
    let output = Command::new("/usr/bin/time")
        .arg(flag)
        .arg(program)
        .args(args)
        .output()?;
    let wall = start.elapsed();

    // `/usr/bin/time` writes its report to stderr, after (or interleaved with)
    // the child's own stderr.
    let report = String::from_utf8_lossy(&output.stderr);
    Ok(RunMeasurement {
        wall,
        peak_rss_bytes: parse_peak_rss_bytes(&report),
        exit_ok: output.status.success(),
    })
}

/// Parse the peak resident set size (in bytes) out of a `/usr/bin/time` report.
///
/// Handles both the macOS form (`<bytes>  maximum resident set size`) and the
/// GNU form (`Maximum resident set size (kbytes): <kb>`), returning bytes in
/// both cases. Returns `None` if no such line is present.
pub fn parse_peak_rss_bytes(report: &str) -> Option<u64> {
    for line in report.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("resident set size") {
            let value: u64 = line
                .split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())?
                .parse()
                .ok()?;
            return Some(if lower.contains("kbyte") {
                value * 1024
            } else {
                value
            });
        }
    }
    None
}

/// A semantic parity report between two tile archives (e.g. ours vs upstream).
#[derive(Debug, Default)]
pub struct ArchiveComparison {
    /// Tiles present only in the first archive.
    pub only_a: usize,
    /// Tiles present only in the second archive.
    pub only_b: usize,
    /// Tiles present in both archives.
    pub shared: usize,
    /// Shared tiles that are semantically identical (decode-equal, order and all).
    pub identical: usize,
    /// Shared tiles with the same feature content but a different within-layer
    /// order (content parity, not draw-order parity). Excludes `identical`.
    pub content_equal: usize,
    /// Shared tiles whose content differs (feature selection/geometry/attrs).
    pub differing: usize,
    /// Total decoded features in the first archive.
    pub features_a: usize,
    /// Total decoded features in the second archive.
    pub features_b: usize,
    /// Layer names seen in the first archive.
    pub layers_a: std::collections::BTreeSet<String>,
    /// Layer names seen in the second archive.
    pub layers_b: std::collections::BTreeSet<String>,
    /// A dump of the first feature of the first differing shared tile from each
    /// side, to eyeball what differs (ids, geometry, attributes).
    pub sample: Option<String>,
}

impl std::fmt::Display for ArchiveComparison {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "tiles:   shared={} only_a={} only_b={}",
            self.shared, self.only_a, self.only_b
        )?;
        writeln!(
            f,
            "shared:  identical={} content_equal={} differing={}",
            self.identical, self.content_equal, self.differing
        )?;
        writeln!(f, "features: a={} b={}", self.features_a, self.features_b)?;
        writeln!(f, "layers_a ({}): {:?}", self.layers_a.len(), self.layers_a)?;
        writeln!(f, "layers_b ({}): {:?}", self.layers_b.len(), self.layers_b)?;
        if let Some(sample) = &self.sample {
            write!(f, "{sample}")?;
        }
        Ok(())
    }
}

/// For the first differing shared tile, find a feature id present in *both* and
/// show how the two versions differ (geometry/attrs); failing that, report the
/// id-set difference so we can see whether ids or selection are the problem.
fn sample_first_feature(
    coord: crate::geom::TileCoord,
    a: &crate::mvt::Tile,
    b: &crate::mvt::Tile,
) -> String {
    use std::collections::BTreeMap;
    // id -> (rotation-canonical form, raw Debug for display)
    let index = |tile: &crate::mvt::Tile| -> BTreeMap<u64, (String, String)> {
        tile.layers
            .first()
            .map(|l| {
                l.features
                    .iter()
                    .map(|f| (f.id, (canonical_feature(f), format!("{f:?}"))))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (ia, ib) = (index(a), index(b));
    let header = format!(
        "first differing tile z{}/{}/{}",
        coord.z(),
        coord.x(),
        coord.y()
    );

    for (id, (canon_a, raw_a)) in &ia {
        if let Some((canon_b, raw_b)) = ib.get(id)
            && canon_a != canon_b
        {
            return format!(
                "{header}\n  shared feature id={id} differs (beyond ring rotation):\n  A: {raw_a}\n  B: {raw_b}"
            );
        }
    }
    let only_a: Vec<u64> = ia
        .keys()
        .filter(|k| !ib.contains_key(k))
        .take(6)
        .copied()
        .collect();
    let only_b: Vec<u64> = ib
        .keys()
        .filter(|k| !ia.contains_key(k))
        .take(6)
        .copied()
        .collect();
    format!(
        "{header}\n  no shared feature id differs; |A|={} |B|={}\n  ids only in A (sample): {only_a:?}\n  ids only in B (sample): {only_b:?}",
        ia.len(),
        ib.len(),
    )
}

/// Compare two MBTiles archives semantically: decode every tile in each and
/// report tile-set overlap, per-tile decode-equality, and feature/layer totals.
/// This is the parity oracle — time and memory only matter once output matches.
pub fn compare_archives(
    a: &std::path::Path,
    b: &std::path::Path,
) -> anyhow::Result<ArchiveComparison> {
    let tiles_a = read_archive(a)?;
    let tiles_b = read_archive(b)?;
    let mut report = ArchiveComparison::default();

    for (coord, tile) in &tiles_a {
        report.features_a += tile.layers.iter().map(|l| l.features.len()).sum::<usize>();
        report
            .layers_a
            .extend(tile.layers.iter().map(|l| l.name.clone()));
        match tiles_b.get(coord) {
            None => report.only_a += 1,
            Some(other) => {
                report.shared += 1;
                if crate::mvt::semantically_equal(tile, other) {
                    report.identical += 1;
                } else {
                    if tile_content_equal(tile, other) {
                        report.content_equal += 1;
                    } else {
                        report.differing += 1;
                    }
                    if report.sample.is_none() {
                        report.sample = Some(sample_first_feature(*coord, tile, other));
                    }
                }
            }
        }
    }
    for (coord, tile) in &tiles_b {
        report.features_b += tile.layers.iter().map(|l| l.features.len()).sum::<usize>();
        report
            .layers_b
            .extend(tile.layers.iter().map(|l| l.name.clone()));
        if !tiles_a.contains_key(coord) {
            report.only_b += 1;
        }
    }
    Ok(report)
}

/// Whether two tiles have the same feature content ignoring within-layer order:
/// per layer (by name), the multiset of features must match. Features are keyed
/// by their rotation-canonical form ([`canonical_feature`]) so a polygon ring
/// that merely starts at a different vertex counts as equal — same polygon.
fn tile_content_equal(a: &crate::mvt::Tile, b: &crate::mvt::Tile) -> bool {
    let index = |tile: &crate::mvt::Tile| -> std::collections::BTreeMap<String, Vec<String>> {
        tile.layers
            .iter()
            .map(|layer| {
                let mut features: Vec<String> =
                    layer.features.iter().map(canonical_feature).collect();
                features.sort();
                (layer.name.clone(), features)
            })
            .collect()
    };
    index(a) == index(b)
}

/// A canonical string form of a feature (id + geometry + attributes) with each
/// polygon ring rotated to start at its minimum vertex. Ring start-vertex is not
/// semantically meaningful in MVT, so this makes the comparison rotation-blind.
fn canonical_feature(feature: &crate::mvt::Feature) -> String {
    let geometry = match &feature.geometry {
        crate::mvt::Geometry::Polygons(rings) => {
            // Rotate each ring to a canonical start, then sort the rings so a
            // multipolygon that lists its rings in a different order still matches.
            let mut canon: Vec<Vec<(i32, i32)>> = rings.iter().map(|r| rotate_to_min(r)).collect();
            canon.sort();
            crate::mvt::Geometry::Polygons(canon)
        }
        other => other.clone(),
    };
    format!("id={} {:?} {:?}", feature.id, geometry, feature.attributes)
}

/// Rotate a ring so it starts at its lexicographically smallest vertex.
fn rotate_to_min(ring: &[(i32, i32)]) -> Vec<(i32, i32)> {
    match (0..ring.len()).min_by_key(|&i| ring[i]) {
        Some(k) => ring[k..].iter().chain(&ring[..k]).copied().collect(),
        None => Vec::new(),
    }
}

/// Read an MBTiles archive into decoded tiles (handles the compact `tiles` view,
/// the TMS→XYZ flip, and gzip). Loads everything into memory — fine at
/// city/region scale.
fn read_archive(
    path: &std::path::Path,
) -> anyhow::Result<std::collections::BTreeMap<crate::geom::TileCoord, crate::mvt::Tile>> {
    use std::io::Read;
    let conn = rusqlite::Connection::open(path)?;
    let mut stmt =
        conn.prepare("SELECT zoom_level, tile_column, tile_row, tile_data FROM tiles")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, u8>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, u32>(2)?,
            r.get::<_, Vec<u8>>(3)?,
        ))
    })?;

    let mut out = std::collections::BTreeMap::new();
    for row in rows {
        let (z, x, tms_y, blob) = row?;
        let y = (1u32 << z) - 1 - tms_y; // TMS → XYZ
        let coord = crate::geom::TileCoord::new(z, x, y).map_err(|e| anyhow::anyhow!("{e}"))?;
        let raw = if blob.starts_with(&[0x1f, 0x8b]) {
            let mut decoder = flate2::read::GzDecoder::new(blob.as_slice());
            let mut buf = Vec::new();
            decoder.read_to_end(&mut buf)?;
            buf
        } else {
            blob
        };
        let tile = crate::mvt::decode_tile(&raw).map_err(|e| anyhow::anyhow!("{e}"))?;
        out.insert(coord, tile);
    }
    Ok(out)
}

/// Render one tile from each archive into a single overlaid SVG for visual
/// parity checks: the first archive draws blue, the second red, so agreement
/// blends to purple and any divergence shows as pure blue or red. MVT tile space
/// (y-down, extent 4096) maps straight to SVG user space.
pub fn render_tiles_svg(
    archives: &[std::path::PathBuf],
    z: u8,
    x: u32,
    y: u32,
) -> anyhow::Result<String> {
    // First archive draws as a translucent blue fill; a second archive draws as
    // a red outline on top, so agreement is a red edge hugging every blue shape
    // and any divergence shows as blue with no red (or red away from blue).
    const STYLES: [(&str, &str); 2] = [
        ("rgba(30,90,220,0.35)", "rgb(20,70,200)"),
        ("none", "rgba(220,25,20,0.9)"),
    ];
    let mut body = String::new();
    for (i, path) in archives.iter().enumerate() {
        let (fill, stroke) = STYLES[i.min(STYLES.len() - 1)];
        match read_tile(path, z, x, y)? {
            Some(tile) => body.push_str(&tile_group_svg(&tile, fill, stroke)),
            None => body.push_str(&format!(
                "<!-- no tile {z}/{x}/{y} in {} -->",
                path.display()
            )),
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"-128 -128 4352 4352\" width=\"768\" height=\"768\">\
         <rect x=\"-128\" y=\"-128\" width=\"4352\" height=\"4352\" fill=\"white\"/>\
         <rect x=\"0\" y=\"0\" width=\"4096\" height=\"4096\" fill=\"none\" stroke=\"#ccc\" stroke-width=\"2\"/>\
         {body}</svg>"
    ))
}

/// An SVG `<g>` drawing one tile's geometry with the given fill/stroke.
fn tile_group_svg(tile: &crate::mvt::Tile, fill: &str, stroke: &str) -> String {
    use crate::mvt::Geometry;
    let mut s = format!(
        "<g fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"4\" stroke-linejoin=\"round\">"
    );
    for layer in &tile.layers {
        for feature in &layer.features {
            match &feature.geometry {
                Geometry::Polygons(rings) => {
                    let mut d = String::new();
                    for ring in rings {
                        if let Some((x0, y0)) = ring.first() {
                            d.push_str(&format!("M{x0} {y0}"));
                            for (px, py) in &ring[1..] {
                                d.push_str(&format!("L{px} {py}"));
                            }
                            d.push('Z');
                        }
                    }
                    s.push_str(&format!("<path d=\"{d}\" fill-rule=\"evenodd\"/>"));
                }
                Geometry::Lines(lines) => {
                    for line in lines {
                        let pts: String =
                            line.iter().map(|(px, py)| format!("{px},{py} ")).collect();
                        s.push_str(&format!("<polyline points=\"{pts}\" fill=\"none\"/>"));
                    }
                }
                Geometry::Points(points) => {
                    for (px, py) in points {
                        s.push_str(&format!(
                            "<circle cx=\"{px}\" cy=\"{py}\" r=\"10\" stroke=\"none\"/>"
                        ));
                    }
                }
                Geometry::Unknown => {}
            }
        }
    }
    s.push_str("</g>");
    s
}

/// Read and decode a single tile (`z`/`x`/`y` in XYZ) from an archive.
fn read_tile(
    path: &std::path::Path,
    z: u8,
    x: u32,
    y: u32,
) -> anyhow::Result<Option<crate::mvt::Tile>> {
    use rusqlite::OptionalExtension;
    use std::io::Read;
    let conn = rusqlite::Connection::open(path)?;
    let tms_y = (1u32 << z) - 1 - y;
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT tile_data FROM tiles WHERE zoom_level=?1 AND tile_column=?2 AND tile_row=?3",
            rusqlite::params![z, x, tms_y],
            |r| r.get(0),
        )
        .optional()?;
    let Some(blob) = blob else { return Ok(None) };
    let raw = if blob.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(blob.as_slice());
        let mut buf = Vec::new();
        decoder.read_to_end(&mut buf)?;
        buf
    } else {
        blob
    };
    Ok(Some(
        crate::mvt::decode_tile(&raw).map_err(|e| anyhow::anyhow!("{e}"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_macos_report() {
        let report = "        0.01 real         0.00 user\n         1212416  maximum resident set size\n              917720  peak memory footprint\n";
        assert_eq!(parse_peak_rss_bytes(report), Some(1_212_416));
    }

    #[test]
    fn parses_gnu_report() {
        let report = "\tUser time (seconds): 0.00\n\tMaximum resident set size (kbytes): 20480\n";
        assert_eq!(parse_peak_rss_bytes(report), Some(20_480 * 1024));
    }

    #[test]
    fn returns_none_when_absent() {
        assert_eq!(parse_peak_rss_bytes("no rusage here\n"), None);
    }

    #[test]
    fn measures_a_trivial_command() {
        // `true` exists on every unix; it should succeed with some wall time.
        let m = measure_command("true", &[]).expect("spawn true");
        assert!(m.exit_ok);
        // Peak RSS may or may not parse depending on the platform's time(1),
        // but the call must not error and wall time must be non-negative.
        let _ = m.peak_rss_bytes;
        assert!(m.wall.as_secs_f64() >= 0.0);
    }
}
