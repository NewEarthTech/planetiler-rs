//! File-backed storage for relation member-way geometries.
//!
//! Relations are assembled after the way pass, so every member way's
//! coordinates must be kept somewhere until then. A `HashMap<i64, Vec<(f64,
//! f64)>>` held them on the heap at 16 bytes per coordinate plus per-way `Vec`
//! and map overhead — tens of GB on a continent extract (Planetiler keeps the
//! equivalent state, its `mpGeoms`, in ~1.4 GB there). [`WayGeometryStore`]
//! instead streams coordinates to a temp file as packed 8-byte fixed-point
//! records (the same 1e-7-degree packing as the node stores, so coordinates
//! survive bit-identically) and keeps only a 16-byte `(way id, span)` index
//! entry per way on the heap; lookups memory-map the file.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use memmap2::Mmap;
use tempfile::TempDir;

use super::node_store::{pack, unpack};
use crate::error::Result;

/// Bits of the index value holding the coordinate count. OSM caps ways at
/// 2,000 nodes, so 20 bits (1M) is generous; the remaining 44 bits address
/// 16 TB of 8-byte coordinate records.
const LEN_BITS: u32 = 20;
const MAX_LEN: u64 = (1 << LEN_BITS) - 1;

/// Packed member-way geometry, off the heap.
///
/// `put` appends each way's coordinates to a backing temp file (any way-id
/// order; the index is sorted once at [`finish`](Self::finish)), and `get`
/// unpacks them back out of the memory-mapped file. Write errors are captured
/// and surfaced by `finish`, matching the node stores.
pub struct WayGeometryStore {
    // Held to keep the temp directory (and file) alive for the store's lifetime.
    _dir: TempDir,
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    /// `(way id, start << LEN_BITS | len)` per way, sorted by id at `finish`.
    index: Vec<(i64, u64)>,
    /// Coordinate records written so far.
    total: u64,
    sorted: bool,
    write_err: Option<std::io::Error>,
    mmap: Option<Mmap>,
}

impl WayGeometryStore {
    /// Create an empty store backed by a fresh temp file.
    ///
    /// # Errors
    /// Returns an error if the temp file cannot be created.
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("way-geoms.bin");
        let writer = BufWriter::new(File::create(&path)?);
        Ok(Self {
            _dir: dir,
            path,
            writer: Some(writer),
            index: Vec::new(),
            total: 0,
            sorted: true,
            write_err: None,
            mmap: None,
        })
    }

    /// Append one way's coordinates. Infallible by design; a write error is
    /// captured and surfaced by [`finish`](Self::finish).
    pub fn put(&mut self, way_id: i64, coords: &[(f64, f64)]) {
        if self.write_err.is_some() {
            return;
        }
        let len = coords.len() as u64;
        if len > MAX_LEN {
            // OSM ways hold at most 2,000 nodes; anything past the 20-bit span
            // is malformed input. Skipping it degrades exactly like a member
            // way missing from the extract.
            tracing::warn!(way_id, nodes = len, "member way too long to store");
            return;
        }
        if let Some(&(last, _)) = self.index.last()
            && way_id < last
        {
            self.sorted = false;
        }
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        for &(lon, lat) in coords {
            if let Err(err) = writer.write_all(&pack(lon, lat).to_ne_bytes()) {
                self.write_err = Some(err);
                return;
            }
        }
        self.index.push((way_id, (self.total << LEN_BITS) | len));
        self.total += len;
    }

    /// Finalize for lookups: flush, sort the index if needed, and memory-map
    /// the coordinate file.
    ///
    /// # Errors
    /// Returns any captured write error, or an I/O error flushing or mapping.
    pub fn finish(&mut self) -> Result<()> {
        if let Some(err) = self.write_err.take() {
            return Err(err.into());
        }
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        if !self.sorted {
            self.index.sort_unstable_by_key(|&(id, _)| id);
            self.sorted = true;
        }
        let file = File::open(&self.path)?;
        // SAFETY: the file is fully written and no longer modified (the writer
        // has been dropped), so the mapping is stable for the store's lifetime.
        self.mmap = Some(unsafe { Mmap::map(&file)? });
        Ok(())
    }

    /// The way's coordinates, or `None` if absent. Call
    /// [`finish`](Self::finish) first.
    pub fn get(&self, way_id: i64) -> Option<Vec<(f64, f64)>> {
        let bytes = self.mmap.as_ref()?;
        let slot = self
            .index
            .binary_search_by_key(&way_id, |&(id, _)| id)
            .ok()?;
        let (_, span) = self.index[slot];
        let (start, len) = ((span >> LEN_BITS) as usize, (span & MAX_LEN) as usize);
        Some(
            (start..start + len)
                .map(|i| {
                    unpack(u64::from_ne_bytes(
                        bytes[i * 8..i * 8 + 8].try_into().expect("8-byte record"),
                    ))
                })
                .collect(),
        )
    }

    /// Number of stored ways.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_way_geometries() {
        let mut store = WayGeometryStore::new().unwrap();
        let a = vec![(7.42, 43.73), (7.43, 43.74)];
        let b = vec![(-122.4194, 37.7749), (-122.42, 37.78), (-122.43, 37.79)];
        store.put(10, &a);
        store.put(20, &b);
        store.finish().unwrap();

        assert_eq!(store.len(), 2);
        // Coordinates go through the same 1e-7 fixed-point packing as the node
        // stores; packing what the node store unpacked is exact, so values that
        // are already on the grid round-trip bit-identically.
        let got = store.get(20).unwrap();
        assert_eq!(got.len(), 3);
        for (got, want) in got.iter().zip(&b) {
            assert!((got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6);
        }
        assert_eq!(store.get(15), None);
        assert_eq!(store.get(99), None);
    }

    #[test]
    fn node_store_grid_values_round_trip_exactly() {
        // What actually flows in: coordinates already quantized by the node
        // store's unpack. Those must come back bit-identical.
        let grid = |lon_e7: i32, lat_e7: i32| (f64::from(lon_e7) / 1e7, f64::from(lat_e7) / 1e7);
        let coords = vec![
            grid(74_200_001, 437_300_009),
            grid(-1_224_194_000, 377_749_123),
        ];
        let mut store = WayGeometryStore::new().unwrap();
        store.put(1, &coords);
        store.finish().unwrap();
        let got = store.get(1).unwrap();
        assert_eq!(got[0].0.to_bits(), coords[0].0.to_bits());
        assert_eq!(got[0].1.to_bits(), coords[0].1.to_bits());
        assert_eq!(got[1].0.to_bits(), coords[1].0.to_bits());
        assert_eq!(got[1].1.to_bits(), coords[1].1.to_bits());
    }

    #[test]
    fn sorts_an_unsorted_index_at_finish() {
        let mut store = WayGeometryStore::new().unwrap();
        store.put(30, &[(3.0, 3.0), (3.1, 3.1)]);
        store.put(10, &[(1.0, 1.0), (1.1, 1.1)]);
        store.put(20, &[(2.0, 2.0), (2.1, 2.1)]);
        store.finish().unwrap();
        for (id, lon) in [(10, 1.0), (20, 2.0), (30, 3.0)] {
            let coords = store.get(id).unwrap();
            assert!((coords[0].0 - lon).abs() < 1e-6, "way {id} starts at {lon}");
        }
    }

    #[test]
    fn empty_store_has_no_ways() {
        let mut store = WayGeometryStore::new().unwrap();
        store.finish().unwrap();
        assert!(store.is_empty());
        assert_eq!(store.get(1), None);
    }
}
