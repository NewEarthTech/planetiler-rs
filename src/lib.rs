//! planetiler-rs — a Rust port of [Planetiler](https://github.com/onthegomap/planetiler).
//!
//! Planetiler turns OpenStreetMap (and other geodata) into planet-scale Mapbox
//! Vector Tile archives through a parallel read → external-sort-on-disk →
//! render pipeline. This crate is a from-scratch Rust reimplementation whose
//! sole justification is beating the Java original on wall-clock time and peak
//! memory, so every capability lands alongside a head-to-head benchmark (see
//! [`bench`], `src/bin/bench.rs`, and `docs/ARCHITECTURE.md`).
//!
//! # Module map
//!
//! The crate is a single library with the pipeline split into modules. The data
//! spine ([`model`]) is the contract every stage speaks: [`render`] produces
//! [`model::Entry`] records, [`collection`] sorts them generically, and
//! [`archive`] consumes them in tile order. Modules whose docs say *(planned)*
//! are declared for orientation but not yet implemented — see the phase they
//! belong to in `docs/ARCHITECTURE.md`.

pub mod bench;
pub mod error;
pub mod geom;
pub mod model;

// --- Pipeline stages (implemented incrementally; see docs/ARCHITECTURE.md) ---
pub mod archive;
pub mod cli;
pub mod collection;
pub mod mvt;
pub mod pipeline;
pub mod profile;
pub mod render;
pub mod source;
pub mod worker;

pub use error::{Error, Result};
