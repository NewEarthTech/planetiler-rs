//! Compact node-location store — a first `LongLongMap`.
//!
//! Maps OSM node id → `(lon, lat)` far more compactly than a `HashMap`: a single
//! sorted `Vec<(i64, u64)>` with the coordinate packed as two fixed-point
//! `i32`s at OSM's native 1e-7-degree precision — **16 bytes/node** vs a
//! `HashMap`'s ~48. Built by inserting during the OSM node pass, finalized
//! (sorted), then queried by binary search during the way pass. OSM PBF nodes
//! already arrive id-sorted, so `finish` is usually a no-op.
//!
//! [`MmapNodeStore`] is the off-heap variant for large / planet-scale extracts:
//! it streams the same 16-byte records to a temp file (kept off the heap) and
//! memory-maps them for lookup, so peak RSS no longer grows with node count.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use memmap2::Mmap;
use tempfile::TempDir;

use crate::error::{Error, Result};

/// Bytes per stored record: an 8-byte id followed by an 8-byte packed location.
const RECORD: usize = 16;

/// Coordinate scale: OSM stores lon/lat at 1e-7 degrees (~1 cm), which fits in
/// an `i32` (`180 * 1e7 < i32::MAX`).
const SCALE: f64 = 1e7;

fn pack(lon: f64, lat: f64) -> u64 {
    let lon_e7 = (lon * SCALE).round() as i32 as u32;
    let lat_e7 = (lat * SCALE).round() as i32 as u32;
    (u64::from(lon_e7) << 32) | u64::from(lat_e7)
}

fn unpack(value: u64) -> (f64, f64) {
    let lon_e7 = (value >> 32) as u32 as i32;
    let lat_e7 = (value & 0xFFFF_FFFF) as u32 as i32;
    (f64::from(lon_e7) / SCALE, f64::from(lat_e7) / SCALE)
}

/// A compact id → location store.
#[derive(Default)]
pub struct NodeLocations {
    entries: Vec<(i64, u64)>,
    sorted: bool,
}

impl NodeLocations {
    /// A store preallocated for `capacity` nodes.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            sorted: true,
        }
    }

    /// Record a node's location. Cheap; ordering is fixed up in [`finish`].
    pub fn put(&mut self, id: i64, lon: f64, lat: f64) {
        if let Some(&(last, _)) = self.entries.last()
            && id < last
        {
            self.sorted = false;
        }
        self.entries.push((id, pack(lon, lat)));
    }

    /// Finalize the store for lookups (sorts by id if the input wasn't sorted).
    pub fn finish(&mut self) {
        if !self.sorted {
            self.entries.sort_unstable_by_key(|&(id, _)| id);
            self.sorted = true;
        }
    }

    /// Look up a node's `(lon, lat)`, or `None` if absent. Call [`finish`] first.
    pub fn get(&self, id: i64) -> Option<(f64, f64)> {
        self.entries
            .binary_search_by_key(&id, |&(k, _)| k)
            .ok()
            .map(|i| unpack(self.entries[i].1))
    }

    /// Number of stored nodes.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// An mmap-backed node store for large / planet-scale extracts.
///
/// Records stream to a temp file during the node pass (so they never sit on the
/// heap); at [`finish`](Self::finish) the file is memory-mapped and lookups
/// binary-search it. OSM PBF nodes arrive id-sorted, so the file is written
/// already sorted; an unsorted input is sorted once at `finish` (the only path
/// that briefly loads the records into RAM).
pub struct MmapNodeStore {
    // Held to keep the temp directory (and file) alive for the store's lifetime.
    _dir: TempDir,
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    len: usize,
    last_id: Option<i64>,
    sorted: bool,
    write_err: Option<std::io::Error>,
    mmap: Option<Mmap>,
}

impl MmapNodeStore {
    /// Create an empty store backed by a fresh temp file.
    ///
    /// # Errors
    /// Returns an error if the temp file cannot be created.
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nodes.bin");
        let writer = BufWriter::new(File::create(&path)?);
        Ok(Self {
            _dir: dir,
            path,
            writer: Some(writer),
            len: 0,
            last_id: None,
            sorted: true,
            write_err: None,
            mmap: None,
        })
    }

    /// Record a node's location by appending it to the backing file. Infallible
    /// by design (matches the RAM store); a write error is captured and surfaced
    /// by [`finish`](Self::finish).
    pub fn put(&mut self, id: i64, lon: f64, lat: f64) {
        if self.write_err.is_some() {
            return;
        }
        if let Some(last) = self.last_id
            && id < last
        {
            self.sorted = false;
        }
        self.last_id = Some(id);
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let mut record = [0u8; RECORD];
        record[..8].copy_from_slice(&id.to_ne_bytes());
        record[8..].copy_from_slice(&pack(lon, lat).to_ne_bytes());
        match writer.write_all(&record) {
            Ok(()) => self.len += 1,
            Err(err) => self.write_err = Some(err),
        }
    }

    /// Finalize for lookups: flush, sort if the input wasn't sorted, and mmap.
    ///
    /// # Errors
    /// Returns any I/O error from writing, sorting, or mapping the file.
    pub fn finish(&mut self) -> Result<()> {
        if let Some(err) = self.write_err.take() {
            return Err(err.into());
        }
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        if !self.sorted {
            self.sort_file()?;
        }
        let file = File::open(&self.path)?;
        // SAFETY: the file is fully written and no longer modified (the writer
        // has been dropped), so the mapping is stable for the store's lifetime.
        self.mmap = Some(unsafe { Mmap::map(&file)? });
        Ok(())
    }

    /// Look up a node's `(lon, lat)`, or `None` if absent. Call [`finish`](Self::finish) first.
    pub fn get(&self, id: i64) -> Option<(f64, f64)> {
        let bytes = self.mmap.as_ref()?;
        let (mut lo, mut hi) = (0usize, self.len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_id = read_i64(bytes, mid * RECORD);
            if mid_id < id {
                lo = mid + 1;
            } else if mid_id > id {
                hi = mid;
            } else {
                return Some(unpack(read_u64(bytes, mid * RECORD + 8)));
            }
        }
        None
    }

    /// Number of stored nodes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Rare path: load the records, sort by id, and rewrite the file.
    fn sort_file(&mut self) -> Result<()> {
        let bytes = std::fs::read(&self.path)?;
        let mut records: Vec<(i64, u64)> = (0..self.len)
            .map(|i| {
                (
                    read_i64(&bytes, i * RECORD),
                    read_u64(&bytes, i * RECORD + 8),
                )
            })
            .collect();
        records.sort_unstable_by_key(|&(id, _)| id);
        let mut writer = BufWriter::new(File::create(&self.path)?);
        for (id, value) in records {
            writer.write_all(&id.to_ne_bytes())?;
            writer.write_all(&value.to_ne_bytes())?;
        }
        writer.flush()?;
        self.sorted = true;
        Ok(())
    }
}

/// A run-compressed sparse node store — the compact, planet-scale layout.
///
/// Node locations stream to a temp file as 8-byte values in id-ascending order
/// (no keys stored inline); a small in-RAM *run index* records each maximal run
/// of consecutive ids as `(start_id, first_value_index)`. Lookup binary-searches
/// the runs, then indexes straight into the memory-mapped values. This needs
/// **ascending ids** (OSM pbf delivers them) and shrinks from 16 toward 8
/// B/node as runs lengthen — reaching ~8 B/node at planet scale, where ids are
/// dense (measured on rhode-island: ~11 B/node, avg run length 4.9).
pub struct SparseNodeStore {
    _dir: TempDir,
    path: PathBuf,
    /// `(start_id, first_value_index)` per run, appended in ascending `start_id`.
    runs: Vec<(i64, u64)>,
    writer: Option<BufWriter<File>>,
    len: u64,
    last_id: Option<i64>,
    ascending: bool,
    write_err: Option<std::io::Error>,
    mmap: Option<Mmap>,
}

impl SparseNodeStore {
    /// Bytes per stored value (packed lon/lat).
    const VALUE: usize = 8;

    /// Create an empty store backed by a fresh temp file.
    ///
    /// # Errors
    /// Returns an error if the temp file cannot be created.
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("sparse-nodes.bin");
        let writer = BufWriter::new(File::create(&path)?);
        Ok(Self {
            _dir: dir,
            path,
            runs: Vec::new(),
            writer: Some(writer),
            len: 0,
            last_id: None,
            ascending: true,
            write_err: None,
            mmap: None,
        })
    }

    /// Append a node's location. Requires strictly ascending ids (enforced in
    /// [`finish`](Self::finish)); a write error is captured and surfaced there.
    pub fn put(&mut self, id: i64, lon: f64, lat: f64) {
        if self.write_err.is_some() || !self.ascending {
            return;
        }
        match self.last_id {
            Some(last) if id <= last => {
                self.ascending = false;
                return;
            }
            Some(last) if id == last + 1 => {} // extends the current run
            _ => self.runs.push((id, self.len)), // first node, or a gap: new run
        }
        self.last_id = Some(id);
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        match writer.write_all(&pack(lon, lat).to_ne_bytes()) {
            Ok(()) => self.len += 1,
            Err(err) => self.write_err = Some(err),
        }
    }

    /// Finalize for lookups: flush and memory-map the values.
    ///
    /// # Errors
    /// Returns an error on a captured write failure, non-ascending ids, or an
    /// I/O failure flushing or mapping the file.
    pub fn finish(&mut self) -> Result<()> {
        if let Some(err) = self.write_err.take() {
            return Err(err.into());
        }
        if !self.ascending {
            return Err(Error::Source(
                "sparse node store requires ascending node ids; use --storage ram or mmap",
            ));
        }
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        let file = File::open(&self.path)?;
        // SAFETY: fully written and no longer modified once the writer is dropped.
        self.mmap = Some(unsafe { Mmap::map(&file)? });
        Ok(())
    }

    /// Look up a node's `(lon, lat)`, or `None` if absent. Call [`finish`](Self::finish) first.
    pub fn get(&self, id: i64) -> Option<(f64, f64)> {
        let bytes = self.mmap.as_ref()?;
        // The run that could contain `id` is the last one with `start_id <= id`.
        let after = self.runs.partition_point(|&(start, _)| start <= id);
        let &(start_id, start_index) = self.runs.get(after.checked_sub(1)?)?;
        let run_end = self.runs.get(after).map_or(self.len, |&(_, next)| next);
        let offset = (id - start_id) as u64;
        if offset < run_end - start_index {
            Some(unpack(read_u64(
                bytes,
                (start_index + offset) as usize * Self::VALUE,
            )))
        } else {
            None // id falls in a gap between runs
        }
    }

    /// Number of stored nodes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// A node id → location store, so the OSM reader can pick a backing strategy at
/// runtime: [`NodeLocations`] (in-RAM, fast, the default) or [`MmapNodeStore`]
/// (off-heap, for extracts whose node store would not fit in memory).
///
/// `Send + Sync` so the way pass can share the finished store across decode
/// workers (`get` is `&self`).
pub trait NodeStore: Send + Sync {
    /// Record a node's location.
    fn put(&mut self, id: i64, lon: f64, lat: f64);
    /// Finalize for lookups.
    ///
    /// # Errors
    /// Returns any I/O error from an on-disk store.
    fn finish(&mut self) -> Result<()>;
    /// Look up a node's `(lon, lat)`.
    fn get(&self, id: i64) -> Option<(f64, f64)>;
}

impl NodeStore for NodeLocations {
    fn put(&mut self, id: i64, lon: f64, lat: f64) {
        NodeLocations::put(self, id, lon, lat);
    }
    fn finish(&mut self) -> Result<()> {
        NodeLocations::finish(self);
        Ok(())
    }
    fn get(&self, id: i64) -> Option<(f64, f64)> {
        NodeLocations::get(self, id)
    }
}

impl NodeStore for MmapNodeStore {
    fn put(&mut self, id: i64, lon: f64, lat: f64) {
        MmapNodeStore::put(self, id, lon, lat);
    }
    fn finish(&mut self) -> Result<()> {
        MmapNodeStore::finish(self)
    }
    fn get(&self, id: i64) -> Option<(f64, f64)> {
        MmapNodeStore::get(self, id)
    }
}

impl NodeStore for SparseNodeStore {
    fn put(&mut self, id: i64, lon: f64, lat: f64) {
        SparseNodeStore::put(self, id, lon, lat);
    }
    fn finish(&mut self) -> Result<()> {
        SparseNodeStore::finish(self)
    }
    fn get(&self, id: i64) -> Option<(f64, f64)> {
        SparseNodeStore::get(self, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn round_trips_locations() {
        let mut store = NodeLocations::default();
        store.put(1, 7.42, 43.73);
        store.put(2, -122.4194, 37.7749);
        store.put(3, 180.0, -85.0);
        store.finish();
        assert!(approx(store.get(1).unwrap(), (7.42, 43.73)));
        assert!(approx(store.get(2).unwrap(), (-122.4194, 37.7749)));
        assert!(approx(store.get(3).unwrap(), (180.0, -85.0)));
        assert_eq!(store.get(99), None);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn sorts_unsorted_input() {
        let mut store = NodeLocations::default();
        store.put(30, 3.0, 3.0);
        store.put(10, 1.0, 1.0);
        store.put(20, 2.0, 2.0);
        store.finish();
        assert!(approx(store.get(10).unwrap(), (1.0, 1.0)));
        assert!(approx(store.get(20).unwrap(), (2.0, 2.0)));
        assert!(approx(store.get(30).unwrap(), (3.0, 3.0)));
    }

    #[test]
    fn mmap_round_trips_locations() {
        let mut store = MmapNodeStore::new().unwrap();
        store.put(1, 7.42, 43.73);
        store.put(2, -122.4194, 37.7749);
        store.put(3, 180.0, -85.0);
        store.finish().unwrap();
        assert!(approx(store.get(1).unwrap(), (7.42, 43.73)));
        assert!(approx(store.get(2).unwrap(), (-122.4194, 37.7749)));
        assert!(approx(store.get(3).unwrap(), (180.0, -85.0)));
        assert_eq!(store.get(99), None);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn mmap_sorts_unsorted_input() {
        let mut store = MmapNodeStore::new().unwrap();
        store.put(30, 3.0, 3.0);
        store.put(10, 1.0, 1.0);
        store.put(20, 2.0, 2.0);
        store.finish().unwrap();
        assert!(approx(store.get(10).unwrap(), (1.0, 1.0)));
        assert!(approx(store.get(20).unwrap(), (2.0, 2.0)));
        assert!(approx(store.get(30).unwrap(), (3.0, 3.0)));
        assert_eq!(store.get(15), None);
    }

    #[test]
    fn mmap_empty_store_has_no_locations() {
        let mut store = MmapNodeStore::new().unwrap();
        store.finish().unwrap();
        assert!(store.is_empty());
        assert_eq!(store.get(1), None);
    }

    #[test]
    fn sparse_round_trips_runs_and_gaps() {
        let mut store = SparseNodeStore::new().unwrap();
        // Two runs of consecutive ids with a gap between them.
        for (id, c) in [(10, 1.0), (11, 1.1), (12, 1.2), (20, 2.0), (21, 2.1)] {
            store.put(id, c, c);
        }
        store.finish().unwrap();
        assert!(approx(store.get(10).unwrap(), (1.0, 1.0)));
        assert!(approx(store.get(12).unwrap(), (1.2, 1.2)));
        assert!(approx(store.get(20).unwrap(), (2.0, 2.0)));
        assert!(approx(store.get(21).unwrap(), (2.1, 2.1)));
        assert_eq!(store.get(13), None, "gap just after a run");
        assert_eq!(store.get(17), None, "gap between runs");
        assert_eq!(store.get(5), None, "before the first id");
        assert_eq!(store.get(99), None, "after the last id");
        assert_eq!(store.len(), 5);
    }

    #[test]
    fn sparse_rejects_non_ascending_ids() {
        let mut store = SparseNodeStore::new().unwrap();
        store.put(10, 1.0, 1.0);
        store.put(9, 2.0, 2.0); // out of order
        assert!(store.finish().is_err());
    }
}
