//! Mapbox Vector Tile encode/decode.
//!
//! *(Planned — Phase 1.)* Encodes `model` features into MVT over a
//! prost-generated `vector_tile.proto`: zigzag/delta geometry commands,
//! clockwise (Y-down) ring winding, and the per-tile key/value tables. A
//! decoder supports the semantic-equality checks the benchmark harness uses.
