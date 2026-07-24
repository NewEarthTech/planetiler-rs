//! Worker pipeline.
//!
//! *(Planned — Phase 3.)* Backpressured, bounded-queue stages
//! (read → process → render → sort-write) over crossbeam channels and rayon,
//! with ordered single-threaded sinks so all cores stay busy under bounded
//! memory.
