//! The data spine: the record types every pipeline stage speaks.
//!
//! [`SortKey`] is the 64-bit contract that orders encoded features so that all
//! features of one tile are contiguous and, within a tile, ordered by layer
//! then z-order. [`crate::render`] produces [`Entry`] records keyed by
//! `SortKey`; [`crate::collection`] sorts them generically; and
//! [`crate::archive`] consumes them in order. [`Value`] is the typed attribute
//! value used by feature tags and the MVT key/value tables.
//!
//! Keeping the record codec here (not in `render`) is what lets `collection`
//! stay a generic `ExternalMergeSort<Key, Bytes>` without depending on the
//! renderer — the dependency arrow points *into* `model`, never back out.

mod config;
mod entry;
mod sort_key;
mod value;

pub use config::{
    DEFAULT_BUFFER, DEFAULT_MAX_ZOOM, DEFAULT_TILE_EXTENT, LayerConfig, PlanetilerConfig,
};
pub use entry::Entry;
pub use sort_key::{MAX_LAYER, SortKey, ZORDER_MAX, ZORDER_MIN};
pub use value::Value;
