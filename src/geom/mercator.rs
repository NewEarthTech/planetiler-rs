//! Spherical web-mercator (EPSG:3857) projection to and from the unit square.

use std::f64::consts::PI;

/// Maximum latitude representable in web mercator, in degrees.
///
/// Beyond `±85.05112878°` the projection runs to infinity, so inputs are
/// clamped to this range (matching every slippy-map implementation).
pub const WEB_MERCATOR_LAT_LIMIT: f64 = 85.051_128_779_806_59;

/// Project longitude/latitude (degrees) to world coordinates in `[0, 1]²`.
///
/// `x` runs west→east (`lng = -180°` → `0.0`, `+180°` → `1.0`); `y` runs
/// north→south (`lat = +85.05…°` → `0.0`, `-85.05…°` → `1.0`), i.e. Y is
/// **down**, matching tile and screen space. Latitude is clamped to
/// [`WEB_MERCATOR_LAT_LIMIT`]; longitude is not wrapped.
pub fn lng_lat_to_world(lng: f64, lat: f64) -> (f64, f64) {
    let lat = lat.clamp(-WEB_MERCATOR_LAT_LIMIT, WEB_MERCATOR_LAT_LIMIT);
    let x = (lng + 180.0) / 360.0;
    let sin_lat = (lat * PI / 180.0).sin();
    let y = 0.5 - ((1.0 + sin_lat) / (1.0 - sin_lat)).ln() / (4.0 * PI);
    (x, y)
}

/// Inverse of [`lng_lat_to_world`]: world coordinates in `[0, 1]²` back to
/// longitude/latitude in degrees.
pub fn world_to_lng_lat(x: f64, y: f64) -> (f64, f64) {
    let lng = x * 360.0 - 180.0;
    let n = PI * (1.0 - 2.0 * y);
    let lat = n.sinh().atan() * 180.0 / PI;
    (lng, lat)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn origin_is_center_of_world() {
        let (x, y) = lng_lat_to_world(0.0, 0.0);
        assert!(approx(x, 0.5, 1e-12), "x = {x}");
        assert!(approx(y, 0.5, 1e-12), "y = {y}");
    }

    #[test]
    fn corners_map_to_unit_square() {
        let (x0, y0) = lng_lat_to_world(-180.0, WEB_MERCATOR_LAT_LIMIT);
        assert!(approx(x0, 0.0, 1e-9), "x0 = {x0}");
        assert!(approx(y0, 0.0, 1e-6), "y0 = {y0}");
        let (x1, y1) = lng_lat_to_world(180.0, -WEB_MERCATOR_LAT_LIMIT);
        assert!(approx(x1, 1.0, 1e-9), "x1 = {x1}");
        assert!(approx(y1, 1.0, 1e-6), "y1 = {y1}");
    }

    #[test]
    fn north_is_smaller_y_than_south() {
        let (_, y_north) = lng_lat_to_world(0.0, 45.0);
        let (_, y_south) = lng_lat_to_world(0.0, -45.0);
        assert!(
            y_north < y_south,
            "expected north above south: {y_north} < {y_south}"
        );
    }

    #[test]
    fn round_trips() {
        for &(lng, lat) in &[
            (0.0, 0.0),
            (7.42, 43.73),
            (-122.4, 37.8),
            (139.7, 35.7),
            (-0.12, 51.5),
        ] {
            let (x, y) = lng_lat_to_world(lng, lat);
            let (lng2, lat2) = world_to_lng_lat(x, y);
            assert!(approx(lng, lng2, 1e-9), "lng {lng} != {lng2}");
            assert!(approx(lat, lat2, 1e-9), "lat {lat} != {lat2}");
        }
    }

    #[test]
    fn latitude_is_clamped() {
        let (_, y_pole) = lng_lat_to_world(0.0, 89.9);
        let (_, y_limit) = lng_lat_to_world(0.0, WEB_MERCATOR_LAT_LIMIT);
        assert!(approx(y_pole, y_limit, 1e-12));
    }
}
