//! On-disk collections.
//!
//! [`ExternalMergeSort`] sorts more [`model::Entry`](crate::model::Entry)
//! records than fit in RAM: it buffers entries, spills sorted chunks to a temp
//! directory when the buffer fills, and streams a k-way merge back in order.
//! This is what bounds the pipeline's memory once rendering stops holding every
//! tile at once. The node-location `LongLongMap` store joins this module in
//! Phase 4.

mod node_store;
mod sort;
mod way_store;

pub use node_store::{MmapNodeStore, NodeLocations, NodeStore, SparseNodeStore};
pub use sort::{ExternalMergeSort, SortedEntries, SortedPartition};
pub use way_store::WayGeometryStore;
