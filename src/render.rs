//! Feature rendering: geometry → tiles.
//!
//! *(Planned — Phase 2.)* `TiledGeometry` projects collected features into tile
//! space, simplifies in tile units, clips (hand-rolled geojson-vt-style rect
//! clip), slices into every covering tile, repairs snapped polygons, and emits
//! `model::Entry` records. The hardest correctness code in the pipeline.
