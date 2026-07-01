//! Mapbox Vector Tile encode/decode.
//!
//! Targets MVT spec 2.1 over the vendored `proto/vector_tile.proto` (compiled by
//! `build.rs`). The [`geometry`] layer implements the spec-fixed command stream
//! (MoveTo/LineTo/ClosePath, zigzag-delta vertices, extent 4096, clockwise
//! Y-down ring winding). Tile assembly — feature/key/value ordering and the
//! attribute-type mapping — mirrors upstream Planetiler exactly for drop-in
//! parity (built out as the encoder lands).

pub mod decode;
pub mod geometry;

pub use decode::{
    DecodedFeature, DecodedGeometry, DecodedLayer, DecodedTile, decode_tile, semantically_equal,
};

/// Generated protobuf wire types for the MVT format (Mapbox spec 2.1),
/// compiled from `proto/vector_tile.proto`.
pub mod proto {
    // Prost-generated code; exempt from the lints we enforce on hand-written
    // code and from doc requirements.
    #![allow(clippy::all)]
    #![allow(missing_docs)]
    include!(concat!(env!("OUT_DIR"), "/vector_tile.rs"));
}
