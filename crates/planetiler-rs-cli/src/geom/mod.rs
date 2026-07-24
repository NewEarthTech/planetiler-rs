//! Geometry primitives: web-mercator projection and the tile grid.
//!
//! Pure, dependency-light math shared by every stage. Projection uses the
//! standard "world" coordinate space where the whole mercator-projected globe
//! is the unit square `[0, 1] × [0, 1]`, with Y pointing **down** (north at
//! `y = 0`). Multiplying a world coordinate by `2^z` gives tile-space
//! coordinates at zoom `z`; the fractional part times the tile extent (4096)
//! gives the position within a tile.
//!
//! Real geometry types (`LineString`/`Polygon` via `geo-types`) enter with the
//! renderer in Phase 2; Phase 0 only needs scalar coordinate math.

mod mercator;
mod tile;

pub use mercator::{WEB_MERCATOR_LAT_LIMIT, lng_lat_to_world, world_to_lng_lat};
pub use tile::{MAX_ZOOM, TileCoord};
