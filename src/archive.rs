//! Tile archive writers.
//!
//! *(Planned — Phase 1.)* A `TileArchive` trait plus an MBTiles writer
//! (rusqlite, TMS Y-flip, per-tile gzip, tilejson `vector_layers` metadata,
//! whole-tile dedup); a PMTiles writer follows in Phase 5. Consumes
//! `model::Entry` records in tile order.
