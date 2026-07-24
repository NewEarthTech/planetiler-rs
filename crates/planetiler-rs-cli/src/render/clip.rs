//! Rectangle clipping of tile-local geometry (f64 coordinates).
//!
//! Points are filtered, lines are clipped with Liang–Barsky (a polyline that
//! leaves and re-enters the rectangle splits into several polylines), and rings
//! are clipped with Sutherland–Hodgman against the four axis-aligned edges. The
//! clip rectangle is the tile box grown by the buffer.

/// An axis-aligned clip rectangle (inclusive bounds).
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Rect {
    /// A rectangle from its inclusive bounds.
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    /// Whether the point is inside (inclusive).
    pub fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

fn coord((x, y): (f64, f64), axis: Axis) -> f64 {
    match axis {
        Axis::X => x,
        Axis::Y => y,
    }
}

fn lerp(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1))
}

/// Keep only the points inside the rectangle.
pub fn clip_points(points: &[(f64, f64)], rect: &Rect) -> Vec<(f64, f64)> {
    points
        .iter()
        .copied()
        .filter(|&p| rect.contains(p))
        .collect()
}

/// Liang–Barsky: the visible parameter sub-range `[t0, t1]` of the segment
/// `p0 -> p1` inside `rect`, or `None` if the segment misses the rectangle.
fn segment_range(p0: (f64, f64), p1: (f64, f64), rect: &Rect) -> Option<(f64, f64)> {
    let dx = p1.0 - p0.0;
    let dy = p1.1 - p0.1;
    let p = [-dx, dx, -dy, dy];
    let q = [
        p0.0 - rect.min_x,
        rect.max_x - p0.0,
        p0.1 - rect.min_y,
        rect.max_y - p0.1,
    ];

    let mut t0 = 0.0;
    let mut t1 = 1.0;
    for i in 0..4 {
        if p[i] == 0.0 {
            // Parallel to this edge: reject only if wholly outside it.
            if q[i] < 0.0 {
                return None;
            }
        } else {
            let r = q[i] / p[i];
            if p[i] < 0.0 {
                if r > t1 {
                    return None;
                }
                if r > t0 {
                    t0 = r;
                }
            } else {
                if r < t0 {
                    return None;
                }
                if r < t1 {
                    t1 = r;
                }
            }
        }
    }
    Some((t0, t1))
}

/// Clip a polyline into zero or more polylines (each with at least two points).
pub fn clip_line(points: &[(f64, f64)], rect: &Rect) -> Vec<Vec<(f64, f64)>> {
    let mut result = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();

    for window in points.windows(2) {
        let (p0, p1) = (window[0], window[1]);
        match segment_range(p0, p1, rect) {
            None => flush(&mut current, &mut result),
            Some((t0, t1)) => {
                let a = lerp(p0, p1, t0);
                let b = lerp(p0, p1, t1);
                if current.is_empty() {
                    current.push(a);
                } else if t0 > 0.0 {
                    // The line re-enters the rectangle: start a new polyline.
                    flush(&mut current, &mut result);
                    current.push(a);
                }
                current.push(b);
                if t1 < 1.0 {
                    // The line exits the rectangle here.
                    flush(&mut current, &mut result);
                }
            }
        }
    }
    flush(&mut current, &mut result);
    result
}

fn flush(current: &mut Vec<(f64, f64)>, result: &mut Vec<Vec<(f64, f64)>>) {
    if current.len() >= 2 {
        result.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

/// Clip a ring (given without a repeated closing vertex) against the rectangle
/// with Sutherland–Hodgman. The result is likewise without a closing vertex and
/// may be empty (ring wholly outside).
pub fn clip_ring(ring: &[(f64, f64)], rect: &Rect) -> Vec<(f64, f64)> {
    let mut out = clip_halfplane(ring, Axis::X, rect.min_x, true);
    out = clip_halfplane(&out, Axis::X, rect.max_x, false);
    out = clip_halfplane(&out, Axis::Y, rect.min_y, true);
    clip_halfplane(&out, Axis::Y, rect.max_y, false)
}

/// One Sutherland–Hodgman pass against a single axis-aligned half-plane:
/// `keep_ge` keeps points with `coord >= value`, otherwise `coord <= value`.
fn clip_halfplane(input: &[(f64, f64)], axis: Axis, value: f64, keep_ge: bool) -> Vec<(f64, f64)> {
    if input.is_empty() {
        return Vec::new();
    }
    let inside = |p: (f64, f64)| {
        let c = coord(p, axis);
        if keep_ge { c >= value } else { c <= value }
    };
    let intersect = |a: (f64, f64), b: (f64, f64)| {
        let t = (value - coord(a, axis)) / (coord(b, axis) - coord(a, axis));
        lerp(a, b, t)
    };

    let mut out = Vec::new();
    let n = input.len();
    for i in 0..n {
        let curr = input[i];
        let next = input[(i + 1) % n];
        let (ci, ni) = (inside(curr), inside(next));
        if ci {
            out.push(curr);
            if !ni {
                out.push(intersect(curr, next));
            }
        } else if ni {
            out.push(intersect(curr, next));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 100.0, 100.0)
    }

    #[test]
    fn clips_points() {
        let pts = [(10.0, 10.0), (-5.0, 50.0), (50.0, 200.0), (100.0, 100.0)];
        assert_eq!(
            clip_points(&pts, &rect()),
            vec![(10.0, 10.0), (100.0, 100.0)]
        );
    }

    #[test]
    fn clips_line_crossing_one_edge() {
        // Starts inside, exits the right edge.
        let line = [(50.0, 50.0), (150.0, 50.0)];
        let out = clip_line(&line, &rect());
        assert_eq!(out, vec![vec![(50.0, 50.0), (100.0, 50.0)]]);
    }

    #[test]
    fn splits_line_that_exits_and_reenters() {
        // Goes in -> out the right -> back in -> across: two visible pieces.
        let line = [
            (-10.0, 50.0),
            (50.0, 50.0),
            (150.0, 50.0),
            (250.0, 50.0),
            (90.0, 50.0),
        ];
        let out = clip_line(&line, &rect());
        assert_eq!(out.len(), 2, "line splits into two polylines: {out:?}");
        assert_eq!(out[0], vec![(0.0, 50.0), (50.0, 50.0), (100.0, 50.0)]);
        assert_eq!(out[1].first(), Some(&(100.0, 50.0)));
        assert_eq!(out[1].last(), Some(&(90.0, 50.0)));
    }

    #[test]
    fn ring_inside_is_unchanged() {
        let ring = [(10.0, 10.0), (90.0, 10.0), (90.0, 90.0), (10.0, 90.0)];
        assert_eq!(clip_ring(&ring, &rect()), ring.to_vec());
    }

    #[test]
    fn ring_larger_than_rect_becomes_the_rect() {
        // A big CW (Y-down) square fully covering the rect clips to its corners.
        let ring = [
            (-50.0, -50.0),
            (150.0, -50.0),
            (150.0, 150.0),
            (-50.0, 150.0),
        ];
        let out = clip_ring(&ring, &rect());
        assert_eq!(out.len(), 4);
        for p in &out {
            assert!(rect().contains(*p), "corner {p:?} inside rect");
        }
    }

    #[test]
    fn ring_outside_is_empty() {
        let ring = [(200.0, 200.0), (300.0, 200.0), (300.0, 300.0)];
        assert!(clip_ring(&ring, &rect()).is_empty());
    }
}
