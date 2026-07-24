//! Douglas–Peucker line simplification in tile space, ported to match
//! Planetiler's `DouglasPeuckerSimplifier` exactly (itself adapted from JTS):
//! distances are measured to the *segment* (not the infinite line), endpoints
//! are always kept, and closed rings force-keep their two furthest interior
//! points even when below tolerance so small rings never collapse — the
//! subsequent area filter removes tiny rings more accurately than collapse
//! would. The forced-point budget propagates asymmetrically (left `n-1`,
//! right `n-2`) through the recursion, mirroring upstream so vertex selection
//! is identical.

/// Simplify a polyline or closed ring with Douglas–Peucker.
///
/// `points` for a ring must be the *closed* form (first == last); pass
/// `is_ring = true` so at least 4 points (3 distinct) survive. A negative
/// `tolerance` returns the input unchanged; `0` drops only exactly-collinear
/// points, matching upstream.
pub fn simplify(points: &[(f64, f64)], tolerance: f64, is_ring: bool) -> Vec<(f64, f64)> {
    let min_points = if is_ring { 4 } else { 2 };
    if points.len() <= min_points || tolerance < 0.0 {
        return points.to_vec();
    }
    let sq_tolerance = tolerance * tolerance.abs();
    let mut out = Vec::with_capacity(points.len());
    out.push(points[0]);
    subsimplify(
        points,
        &mut out,
        0,
        points.len() - 1,
        min_points as i32 - 2,
        sq_tolerance,
    );
    out.push(points[points.len() - 1]);
    out
}

/// In-order recursive step. `num_forced_points > 0` keeps the furthest point
/// even when it is below tolerance (ring anti-collapse).
fn subsimplify(
    points: &[(f64, f64)],
    out: &mut Vec<(f64, f64)>,
    first: usize,
    last: usize,
    num_forced_points: i32,
    sq_tolerance: f64,
) {
    let force = num_forced_points > 0;
    let mut max_sq_dist = if force { -1.0 } else { sq_tolerance };
    let mut index = usize::MAX;
    let (p1x, p1y) = points[first];
    let (p2x, p2y) = points[last];

    for (offset, &(px, py)) in points[(first + 1)..last].iter().enumerate() {
        let sq_dist = sq_seg_dist(px, py, p1x, p1y, p2x, p2y);
        if sq_dist > max_sq_dist {
            index = first + 1 + offset;
            max_sq_dist = sq_dist;
        }
    }

    if force || max_sq_dist > sq_tolerance {
        if index - first > 1 {
            subsimplify(
                points,
                out,
                first,
                index,
                num_forced_points - 1,
                sq_tolerance,
            );
        }
        out.push(points[index]);
        if last - index > 1 {
            subsimplify(
                points,
                out,
                index,
                last,
                num_forced_points - 2,
                sq_tolerance,
            );
        }
    }
}

/// Squared distance from `(px, py)` to the *segment* `(p1x, p1y)-(p2x, p2y)`
/// (endpoint distance beyond the segment ends), matching upstream's
/// `getSqSegDist`.
fn sq_seg_dist(px: f64, py: f64, p1x: f64, p1y: f64, p2x: f64, p2y: f64) -> f64 {
    let mut x = p1x;
    let mut y = p1y;
    let mut dx = p2x - x;
    let mut dy = p2y - y;

    if dx != 0.0 || dy != 0.0 {
        let t = ((px - x) * dx + (py - y) * dy) / (dx * dx + dy * dy);
        if t > 1.0 {
            x = p2x;
            y = p2y;
        } else if t > 0.0 {
            x += dx * t;
            y += dy * t;
        }
    }

    dx = px - x;
    dy = py - y;
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_collinear_and_within_tolerance_points() {
        // A straight line with a tiny wobble; tolerance removes the middles.
        let line = [
            (0.0, 0.0),
            (5.0, 0.1),
            (10.0, 0.0),
            (15.0, 0.05),
            (20.0, 0.0),
        ];
        let simplified = simplify(&line, 1.0, false);
        assert_eq!(simplified, vec![(0.0, 0.0), (20.0, 0.0)]);
    }

    #[test]
    fn keeps_significant_vertices() {
        // A clear spike is preserved.
        let line = [(0.0, 0.0), (5.0, 10.0), (10.0, 0.0)];
        assert_eq!(simplify(&line, 1.0, false), line.to_vec());
    }

    #[test]
    fn negative_tolerance_passes_through() {
        let line = [(0.0, 0.0), (1.0, 5.0), (2.0, 0.0)];
        assert_eq!(simplify(&line, -1.0, false), line.to_vec());
        assert_eq!(
            simplify(&[(0.0, 0.0), (1.0, 1.0)], 5.0, false),
            vec![(0.0, 0.0), (1.0, 1.0)]
        );
    }

    #[test]
    fn zero_tolerance_drops_exactly_collinear_points_only() {
        // (5,0) is exactly on the (0,0)-(10,0) sub-baseline and goes; (10,0)
        // stays because its baseline becomes (0,0)-(15,0.1) once the spike is
        // kept — hand-traced against upstream's recursion.
        let line = [
            (0.0, 0.0),
            (5.0, 0.0),
            (10.0, 0.0),
            (15.0, 0.1),
            (20.0, 0.0),
        ];
        assert_eq!(
            simplify(&line, 0.0, false),
            vec![(0.0, 0.0), (10.0, 0.0), (15.0, 0.1), (20.0, 0.0)]
        );
    }

    #[test]
    fn ring_below_tolerance_keeps_two_forced_interior_points() {
        // A tiny closed square, far below tolerance: upstream keeps the two
        // furthest interior points instead of collapsing the ring.
        let ring = [(0.0, 0.0), (0.1, 0.0), (0.1, 0.1), (0.0, 0.1), (0.0, 0.0)];
        let simplified = simplify(&ring, 10.0, true);
        assert_eq!(simplified.len(), 4, "endpoints + 2 forced interior points");
        assert_eq!(simplified.first(), simplified.last());
    }

    #[test]
    fn distance_is_to_segment_not_infinite_line() {
        // The middle point is beyond the segment end: its distance to the
        // *segment* is large even though the infinite line passes near it.
        let line = [(0.0, 0.0), (30.0, 0.5), (10.0, 0.0)];
        assert_eq!(simplify(&line, 1.0, false), line.to_vec());
    }
}
