//! MVT geometry command encoding (spec section 4.3).
//!
//! Geometry is a flat `u32` stream of *command integers* and zigzag-encoded
//! parameter pairs, relative to a cursor that starts at `(0, 0)`. The commands
//! are `MoveTo` (id 1), `LineTo` (id 2), and `ClosePath` (id 7); a command
//! integer packs `(id & 0x7) | (count << 3)`.
//!
//! This layer is fixed by the MVT specification and independent of any
//! Planetiler-specific choices — ring winding and ring nesting are decided by
//! the polygon-assembly layer, not here. Coordinates are tile-grid integers in
//! `[0, extent]` (default extent 4096), Y pointing down.

use crate::error::{Error, Result};

const CMD_MOVE_TO: u32 = 1;
const CMD_LINE_TO: u32 = 2;
const CMD_CLOSE_PATH: u32 = 7;

/// Zigzag-encode a signed delta into an unsigned parameter (spec 4.3.2).
#[inline]
pub fn zigzag(n: i32) -> u32 {
    (n.wrapping_shl(1) ^ (n >> 31)) as u32
}

/// Inverse of [`zigzag`].
#[inline]
pub fn unzigzag(u: u32) -> i32 {
    ((u >> 1) as i32) ^ -((u & 1) as i32)
}

/// Pack a command id and repeat count into a command integer (spec 4.3.3).
#[inline]
fn command_integer(id: u32, count: u32) -> u32 {
    (id & 0x7) | (count << 3)
}

#[inline]
fn command_id(cmd: u32) -> u32 {
    cmd & 0x7
}

#[inline]
fn command_count(cmd: u32) -> u32 {
    cmd >> 3
}

/// Incrementally builds an MVT geometry command stream, tracking the cursor.
#[derive(Debug, Default)]
pub struct CommandEncoder {
    out: Vec<u32>,
    cx: i32,
    cy: i32,
}

impl CommandEncoder {
    /// Start a fresh encoder with the cursor at the tile origin `(0, 0)`.
    pub fn new() -> Self {
        Self::default()
    }

    fn vertices(&mut self, id: u32, points: &[(i32, i32)]) {
        if points.is_empty() {
            return;
        }
        self.out.push(command_integer(id, points.len() as u32));
        for &(x, y) in points {
            self.out.push(zigzag(x - self.cx));
            self.out.push(zigzag(y - self.cy));
            self.cx = x;
            self.cy = y;
        }
    }

    /// Emit a `MoveTo` covering every point (multi-point uses `count > 1`).
    pub fn move_to(&mut self, points: &[(i32, i32)]) {
        self.vertices(CMD_MOVE_TO, points);
    }

    /// Emit a `LineTo` over the given vertices.
    pub fn line_to(&mut self, points: &[(i32, i32)]) {
        self.vertices(CMD_LINE_TO, points);
    }

    /// Emit a `ClosePath` (count 1, no parameters).
    pub fn close_path(&mut self) {
        self.out.push(command_integer(CMD_CLOSE_PATH, 1));
    }

    /// Consume the encoder and return the command stream.
    pub fn finish(self) -> Vec<u32> {
        self.out
    }
}

/// Encode a (multi)point geometry as a single `MoveTo` over all points.
pub fn encode_points(points: &[(i32, i32)]) -> Vec<u32> {
    let mut e = CommandEncoder::new();
    e.move_to(points);
    e.finish()
}

/// Encode a (multi)line geometry. Each line needs at least two vertices;
/// shorter inputs are skipped (they cannot form a valid MVT linestring).
pub fn encode_lines(lines: &[Vec<(i32, i32)>]) -> Vec<u32> {
    let mut e = CommandEncoder::new();
    for line in lines {
        if line.len() < 2 {
            continue;
        }
        e.move_to(&line[..1]);
        e.line_to(&line[1..]);
    }
    e.finish()
}

/// Encode a (multi)polygon from rings given as distinct vertices (no repeated
/// closing point — `ClosePath` implies it). Rings with fewer than three
/// vertices are skipped. Winding and ring nesting are the caller's job.
pub fn encode_polygons(rings: &[Vec<(i32, i32)>]) -> Vec<u32> {
    let mut e = CommandEncoder::new();
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        e.move_to(&ring[..1]);
        e.line_to(&ring[1..]);
        e.close_path();
    }
    e.finish()
}

/// Walks a command stream, tracking the cursor and decoding vertices.
struct Reader<'a> {
    geom: &'a [u32],
    i: usize,
    cx: i32,
    cy: i32,
}

impl<'a> Reader<'a> {
    fn new(geom: &'a [u32]) -> Self {
        Self {
            geom,
            i: 0,
            cx: 0,
            cy: 0,
        }
    }

    fn next_command(&mut self) -> Option<(u32, u32)> {
        let cmd = *self.geom.get(self.i)?;
        self.i += 1;
        Some((command_id(cmd), command_count(cmd)))
    }

    fn read_vertex(&mut self) -> Result<(i32, i32)> {
        let dx = unzigzag(
            *self
                .geom
                .get(self.i)
                .ok_or(Error::MvtGeometry("truncated vertex"))?,
        );
        let dy = unzigzag(
            *self
                .geom
                .get(self.i + 1)
                .ok_or(Error::MvtGeometry("truncated vertex"))?,
        );
        self.i += 2;
        self.cx += dx;
        self.cy += dy;
        Ok((self.cx, self.cy))
    }
}

/// Decode a (multi)point command stream into absolute tile coordinates.
pub fn decode_points(geom: &[u32]) -> Result<Vec<(i32, i32)>> {
    let mut r = Reader::new(geom);
    let mut pts = Vec::new();
    while let Some((id, count)) = r.next_command() {
        if id != CMD_MOVE_TO {
            return Err(Error::MvtGeometry("unexpected command in point geometry"));
        }
        for _ in 0..count {
            pts.push(r.read_vertex()?);
        }
    }
    Ok(pts)
}

/// Decode a (multi)line command stream into lines of absolute coordinates.
pub fn decode_lines(geom: &[u32]) -> Result<Vec<Vec<(i32, i32)>>> {
    let mut r = Reader::new(geom);
    let mut lines: Vec<Vec<(i32, i32)>> = Vec::new();
    while let Some((id, count)) = r.next_command() {
        match id {
            CMD_MOVE_TO => {
                if count != 1 {
                    return Err(Error::MvtGeometry(
                        "MoveTo count must be 1 in line geometry",
                    ));
                }
                let v = r.read_vertex()?;
                lines.push(vec![v]);
            }
            CMD_LINE_TO => {
                let line = lines
                    .last_mut()
                    .ok_or(Error::MvtGeometry("LineTo before MoveTo"))?;
                for _ in 0..count {
                    line.push(r.read_vertex()?);
                }
            }
            _ => return Err(Error::MvtGeometry("unexpected command in line geometry")),
        }
    }
    Ok(lines)
}

/// Decode a (multi)polygon command stream into rings of absolute coordinates
/// (without the repeated closing vertex). Rings are returned in stream order.
pub fn decode_polygons(geom: &[u32]) -> Result<Vec<Vec<(i32, i32)>>> {
    let mut r = Reader::new(geom);
    let mut rings: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut current: Option<Vec<(i32, i32)>> = None;
    while let Some((id, count)) = r.next_command() {
        match id {
            CMD_MOVE_TO => {
                if count != 1 {
                    return Err(Error::MvtGeometry(
                        "MoveTo count must be 1 in polygon geometry",
                    ));
                }
                if current.is_some() {
                    return Err(Error::MvtGeometry("MoveTo without preceding ClosePath"));
                }
                current = Some(vec![r.read_vertex()?]);
            }
            CMD_LINE_TO => {
                let ring = current
                    .as_mut()
                    .ok_or(Error::MvtGeometry("LineTo before MoveTo"))?;
                for _ in 0..count {
                    ring.push(r.read_vertex()?);
                }
            }
            CMD_CLOSE_PATH => {
                let ring = current
                    .take()
                    .ok_or(Error::MvtGeometry("ClosePath before MoveTo"))?;
                rings.push(ring);
            }
            _ => return Err(Error::MvtGeometry("unexpected command in polygon geometry")),
        }
    }
    if current.is_some() {
        return Err(Error::MvtGeometry("polygon ring not closed"));
    }
    Ok(rings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_round_trips_including_extremes() {
        for n in [
            0,
            1,
            -1,
            2,
            -2,
            25,
            17,
            123_456,
            -123_456,
            i32::MAX,
            i32::MIN,
        ] {
            assert_eq!(unzigzag(zigzag(n)), n, "n={n}");
        }
        // Spec-listed values.
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(-1), 1);
        assert_eq!(zigzag(1), 2);
        assert_eq!(zigzag(25), 50);
        assert_eq!(zigzag(17), 34);
    }

    #[test]
    fn command_integer_packs_per_spec() {
        assert_eq!(command_integer(CMD_MOVE_TO, 1), 9);
        assert_eq!(command_integer(CMD_LINE_TO, 2), 18);
        assert_eq!(command_integer(CMD_CLOSE_PATH, 1), 15);
        assert_eq!(command_id(9), CMD_MOVE_TO);
        assert_eq!(command_count(9), 1);
        assert_eq!(command_count(18), 2);
    }

    // The three canonical examples from MVT spec 2.1, section 4.3.5.

    #[test]
    fn encodes_spec_point_example() {
        assert_eq!(encode_points(&[(25, 17)]), vec![9, 50, 34]);
    }

    #[test]
    fn encodes_spec_line_example() {
        let line = vec![(2, 2), (2, 10), (10, 10)];
        assert_eq!(encode_lines(&[line]), vec![9, 4, 4, 18, 0, 16, 16, 0]);
    }

    #[test]
    fn encodes_spec_polygon_example() {
        let ring = vec![(3, 6), (8, 12), (20, 34)];
        assert_eq!(
            encode_polygons(&[ring]),
            vec![9, 6, 12, 18, 10, 12, 24, 44, 15]
        );
    }

    #[test]
    fn point_round_trip_multi() {
        let pts = vec![(25, 17), (30, 20), (0, 0)];
        assert_eq!(decode_points(&encode_points(&pts)).unwrap(), pts);
    }

    #[test]
    fn line_round_trip_multi() {
        let lines = vec![vec![(2, 2), (2, 10), (10, 10)], vec![(1, 1), (5, 5)]];
        assert_eq!(decode_lines(&encode_lines(&lines)).unwrap(), lines);
    }

    #[test]
    fn polygon_round_trip_multi() {
        let rings = vec![
            vec![(3, 6), (8, 12), (20, 34)],
            vec![(0, 0), (10, 0), (10, 10), (0, 10)],
        ];
        assert_eq!(decode_polygons(&encode_polygons(&rings)).unwrap(), rings);
    }

    #[test]
    fn decode_rejects_truncated_and_misordered() {
        assert!(decode_points(&[9, 50]).is_err()); // missing dy parameter
        assert!(decode_lines(&[18, 0, 0]).is_err()); // LineTo before any MoveTo
        // Polygon example missing its trailing ClosePath -> unclosed ring.
        assert!(decode_polygons(&[9, 6, 12, 18, 10, 12, 24, 44]).is_err());
    }
}
