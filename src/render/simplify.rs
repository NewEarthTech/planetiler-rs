//! Douglas–Peucker line simplification in tile space.

/// Simplify a polyline with Douglas–Peucker: drop points whose perpendicular
/// distance from the retained baseline is within `tolerance`. Endpoints are kept.
/// `tolerance <= 0` or fewer than three points returns the input unchanged.
pub fn simplify(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if points.len() <= 2 || tolerance <= 0.0 {
        return points.to_vec();
    }
    let tol_sq = tolerance * tolerance;
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    douglas_peucker(points, 0, points.len() - 1, tol_sq, &mut keep);
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

fn douglas_peucker(
    points: &[(f64, f64)],
    first: usize,
    last: usize,
    tol_sq: f64,
    keep: &mut [bool],
) {
    if last <= first + 1 {
        return;
    }
    let a = points[first];
    let b = points[last];
    let mut max_dist = 0.0;
    let mut split = first;
    for (offset, &p) in points[(first + 1)..last].iter().enumerate() {
        let d = perp_dist_sq(p, a, b);
        if d > max_dist {
            max_dist = d;
            split = first + 1 + offset;
        }
    }
    if max_dist > tol_sq {
        keep[split] = true;
        douglas_peucker(points, first, split, tol_sq, keep);
        douglas_peucker(points, split, last, tol_sq, keep);
    }
}

/// Squared perpendicular distance from `p` to the infinite line through `a`–`b`.
fn perp_dist_sq(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx * dx + dy * dy;
    if len_sq == 0.0 {
        let (ex, ey) = (p.0 - a.0, p.1 - a.1);
        return ex * ex + ey * ey;
    }
    let cross = dx * (a.1 - p.1) - dy * (a.0 - p.0);
    cross * cross / len_sq
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
        let simplified = simplify(&line, 1.0);
        assert_eq!(simplified, vec![(0.0, 0.0), (20.0, 0.0)]);
    }

    #[test]
    fn keeps_significant_vertices() {
        // A clear spike is preserved.
        let line = [(0.0, 0.0), (5.0, 10.0), (10.0, 0.0)];
        assert_eq!(simplify(&line, 1.0), line.to_vec());
    }

    #[test]
    fn passthrough_when_disabled_or_short() {
        let line = [(0.0, 0.0), (1.0, 5.0), (2.0, 0.0)];
        assert_eq!(simplify(&line, 0.0), line.to_vec());
        assert_eq!(
            simplify(&[(0.0, 0.0), (1.0, 1.0)], 5.0),
            vec![(0.0, 0.0), (1.0, 1.0)]
        );
    }
}
