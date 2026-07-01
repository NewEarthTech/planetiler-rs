//! The tile grid: [`TileCoord`] and zoom math.

use super::mercator;
use crate::error::{Error, Result};

/// Maximum supported zoom level.
///
/// [`crate::model::SortKey`] reserves 15 bits each for tile X and Y, so
/// coordinates must satisfy `x, y < 2^z ≤ 2^15`. Zoom 15 already exceeds
/// Planetiler's usual maximum of 14.
pub const MAX_ZOOM: u8 = 15;

/// A slippy-map tile address `(z, x, y)` in XYZ convention: Y increases
/// southward, with `y = 0` at the north edge.
///
/// Coordinates are validated on construction and kept private so a `TileCoord`
/// is always in range (`z ≤ MAX_ZOOM`, `x, y < 2^z`). MBTiles stores tiles in
/// TMS convention (Y flipped); that conversion happens in the archive writer,
/// not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileCoord {
    z: u8,
    x: u32,
    y: u32,
}

impl TileCoord {
    /// Construct a tile, validating that `z ≤ MAX_ZOOM` and `x, y < 2^z`.
    pub fn new(z: u8, x: u32, y: u32) -> Result<Self> {
        if z > MAX_ZOOM {
            return Err(Error::InvalidTileCoord {
                z,
                x,
                y,
                reason: "zoom exceeds MAX_ZOOM",
            });
        }
        let axis = Self::axis_tiles(z);
        if x >= axis || y >= axis {
            return Err(Error::InvalidTileCoord {
                z,
                x,
                y,
                reason: "x or y outside [0, 2^z)",
            });
        }
        Ok(Self { z, x, y })
    }

    /// Zoom level.
    #[inline]
    pub fn z(&self) -> u8 {
        self.z
    }

    /// Tile column (0 at the west edge).
    #[inline]
    pub fn x(&self) -> u32 {
        self.x
    }

    /// Tile row (0 at the north edge).
    #[inline]
    pub fn y(&self) -> u32 {
        self.y
    }

    /// Number of tiles along each axis at zoom `z`, i.e. `2^z`.
    #[inline]
    pub fn axis_tiles(z: u8) -> u32 {
        1u32 << z
    }

    /// The tile containing a world coordinate (`[0, 1]²`) at zoom `z`.
    ///
    /// Coordinates on the eastern/southern edge (`= 1.0`) clamp to the last
    /// tile rather than overflowing; out-of-range inputs clamp to the grid.
    pub fn from_world(z: u8, world_x: f64, world_y: f64) -> Result<Self> {
        if z > MAX_ZOOM {
            return Err(Error::InvalidTileCoord {
                z,
                x: 0,
                y: 0,
                reason: "zoom exceeds MAX_ZOOM",
            });
        }
        let axis = Self::axis_tiles(z);
        let to_index = |v: f64| -> u32 {
            let t = (v * axis as f64).floor();
            if t < 0.0 {
                0
            } else if t >= axis as f64 {
                axis - 1
            } else {
                t as u32
            }
        };
        Self::new(z, to_index(world_x), to_index(world_y))
    }

    /// The tile containing a lng/lat position at zoom `z`.
    pub fn from_lng_lat(z: u8, lng: f64, lat: f64) -> Result<Self> {
        let (wx, wy) = mercator::lng_lat_to_world(lng, lat);
        Self::from_world(z, wx, wy)
    }

    /// World-coordinate bounds of this tile as `(min_x, min_y, max_x, max_y)`
    /// in `[0, 1]²` — `min` is the north-west corner, `max` the south-east.
    pub fn world_bounds(&self) -> (f64, f64, f64, f64) {
        let n = Self::axis_tiles(self.z) as f64;
        let min_x = self.x as f64 / n;
        let min_y = self.y as f64 / n;
        (min_x, min_y, min_x + 1.0 / n, min_y + 1.0 / n)
    }

    /// Convert a world coordinate to this tile's local grid at `extent` (e.g.
    /// 4096). Points inside the tile map to `[0, extent]`; points outside are
    /// returned unclamped (rounding and clipping are the caller's job, since
    /// the tile buffer legitimately places coordinates beyond the edges).
    pub fn world_to_tile_extent(&self, world_x: f64, world_y: f64, extent: u32) -> (f64, f64) {
        let n = Self::axis_tiles(self.z) as f64;
        let e = extent as f64;
        (
            (world_x * n - self.x as f64) * e,
            (world_y * n - self.y as f64) * e,
        )
    }

    /// The parent tile one zoom out, or `None` at zoom 0.
    pub fn parent(&self) -> Option<TileCoord> {
        if self.z == 0 {
            return None;
        }
        Some(TileCoord {
            z: self.z - 1,
            x: self.x >> 1,
            y: self.y >> 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range() {
        assert!(TileCoord::new(0, 1, 0).is_err()); // only tile 0/0/0 exists at z0
        assert!(TileCoord::new(1, 2, 0).is_err()); // z1 has x,y in {0,1}
        assert!(TileCoord::new(MAX_ZOOM + 1, 0, 0).is_err());
        assert!(TileCoord::new(1, 1, 1).is_ok());
    }

    #[test]
    fn accessors_round_trip() {
        let t = TileCoord::new(14, 8191, 5000).unwrap();
        assert_eq!((t.z(), t.x(), t.y()), (14, 8191, 5000));
    }

    #[test]
    fn from_world_center_and_edges() {
        // Center of the world at z1 is the boundary of the four tiles; floor
        // puts it in tile (1, 1).
        assert_eq!(
            TileCoord::from_world(1, 0.5, 0.5).unwrap(),
            TileCoord::new(1, 1, 1).unwrap()
        );
        // North-west corner.
        assert_eq!(
            TileCoord::from_world(1, 0.0, 0.0).unwrap(),
            TileCoord::new(1, 0, 0).unwrap()
        );
        // South-east corner clamps into the last tile instead of overflowing.
        assert_eq!(
            TileCoord::from_world(1, 1.0, 1.0).unwrap(),
            TileCoord::new(1, 1, 1).unwrap()
        );
    }

    #[test]
    fn from_lng_lat_places_monaco() {
        // Monaco (~7.42E, 43.73N) — upstream's default example area.
        let t = TileCoord::from_lng_lat(14, 7.42, 43.73).unwrap();
        assert_eq!(t.z(), 14);
        // Round-trips back into the same tile via the tile center.
        let (min_x, min_y, max_x, max_y) = t.world_bounds();
        let (cx, cy) = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
        let (lng, lat) = mercator::world_to_lng_lat(cx, cy);
        assert_eq!(TileCoord::from_lng_lat(14, lng, lat).unwrap(), t);
    }

    #[test]
    fn world_bounds_partition_the_tile() {
        let t = TileCoord::new(3, 5, 2).unwrap();
        let (min_x, min_y, max_x, max_y) = t.world_bounds();
        assert_eq!(TileCoord::from_world(3, min_x, min_y).unwrap(), t);
        // The lower-right corner belongs to the next tile (half-open bounds).
        let just_inside = TileCoord::from_world(3, max_x - 1e-9, max_y - 1e-9).unwrap();
        assert_eq!(just_inside, t);
    }

    #[test]
    fn extent_maps_tile_corners_to_grid() {
        let t = TileCoord::new(3, 5, 2).unwrap();
        let (min_x, min_y, _, _) = t.world_bounds();
        let (px, py) = t.world_to_tile_extent(min_x, min_y, 4096);
        assert!(
            px.abs() < 1e-6 && py.abs() < 1e-6,
            "nw corner should be (0,0): ({px},{py})"
        );
        let n = TileCoord::axis_tiles(3) as f64;
        let (px2, py2) = t.world_to_tile_extent(min_x + 1.0 / n, min_y + 1.0 / n, 4096);
        assert!((px2 - 4096.0).abs() < 1e-6 && (py2 - 4096.0).abs() < 1e-6);
    }

    #[test]
    fn parent_walks_up() {
        let t = TileCoord::new(3, 5, 2).unwrap();
        assert_eq!(t.parent().unwrap(), TileCoord::new(2, 2, 1).unwrap());
        assert_eq!(TileCoord::new(0, 0, 0).unwrap().parent(), None);
    }
}
