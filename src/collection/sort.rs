//! Generic external merge sort over [`Entry`] records.
//!
//! Spilled chunks record a sparse `(key, offset)` sample index as they are
//! written, so the sorted stream can be split into disjoint, tile-aligned
//! partitions ([`ExternalMergeSort::into_sorted_partitions`]) that merge
//! independently — in parallel — while their concatenation stays exactly the
//! single k-way merge order. Partitions read their byte ranges with positioned
//! reads on shared per-chunk file handles: file-descriptor count stays one per
//! chunk however many partitions are in flight, and chunk bytes pass through
//! the page cache instead of accumulating in process RSS the way a mapping
//! would.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::slice::ParallelSliceMut;
use tempfile::TempDir;

use crate::error::{Error, Result};
use crate::model::{Entry, SortKey};

/// Rough per-entry RAM overhead beyond the value bytes, used to decide when to
/// spill. Not exact — just keeps the buffer estimate honest.
const ENTRY_OVERHEAD: usize = std::mem::size_of::<Entry>() + 16;

/// Bytes of a chunk record before its value: an 8-byte key + a 4-byte length.
const RECORD_HEADER: usize = 12;

/// Every `SAMPLE_STRIDE` entries a spilled chunk records `(key, byte offset)`,
/// so a partition boundary is located by scanning forward at most one stride
/// from the nearest sample instead of rescanning the chunk.
const SAMPLE_STRIDE: usize = 1024;

/// Sorts [`Entry`] records that may exceed RAM by spilling sorted runs to disk
/// and k-way merging them. Entries are ordered by [`Entry`]'s total order (sort
/// key, then value bytes), so the output is deterministic.
pub struct ExternalMergeSort {
    buffer: Vec<Entry>,
    buffer_bytes: usize,
    max_buffer_bytes: usize,
    chunks: Vec<PathBuf>,
    samples: Vec<Vec<(u64, u64)>>,
    dir: TempDir,
    len: u64,
}

impl ExternalMergeSort {
    /// Create a sorter that keeps at most about `max_buffer_bytes` of entries in
    /// RAM before spilling a sorted chunk to a temp directory.
    ///
    /// # Errors
    /// Returns an error if the temp directory cannot be created.
    pub fn new(max_buffer_bytes: usize) -> Result<Self> {
        Ok(Self {
            buffer: Vec::new(),
            buffer_bytes: 0,
            max_buffer_bytes: max_buffer_bytes.max(1),
            chunks: Vec::new(),
            samples: Vec::new(),
            dir: tempfile::tempdir()?,
            len: 0,
        })
    }

    /// Number of entries added so far.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether no entries have been added.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Add one entry, spilling a sorted chunk if the buffer is full.
    pub fn add(&mut self, entry: Entry) -> Result<()> {
        self.buffer_bytes += entry.value.len() + ENTRY_OVERHEAD;
        self.buffer.push(entry);
        self.len += 1;
        if self.buffer_bytes >= self.max_buffer_bytes {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer.sort_unstable();
        let path = self
            .dir
            .path()
            .join(format!("chunk-{:05}", self.chunks.len()));
        let samples = write_chunk(&path, &self.buffer)?;
        self.chunks.push(path);
        self.samples.push(samples);
        self.buffer.clear();
        self.buffer_bytes = 0;
        Ok(())
    }

    /// Consume the sorter and stream entries in sorted order.
    ///
    /// If nothing spilled, this is an in-memory sort; otherwise it spills the
    /// remaining buffer and k-way merges all chunks.
    pub fn into_sorted(self) -> Result<SortedEntries> {
        let mut partitions = self.into_sorted_partitions(1)?;
        partitions
            .pop()
            .expect("a sorter yields at least one partition")
            .open()
    }

    /// Consume the sorter and split the sorted stream into at most `target`
    /// disjoint partitions, each internally sorted, whose concatenation in
    /// order is exactly the [`into_sorted`](Self::into_sorted) stream. Every
    /// boundary is a [tile floor](SortKey::tile_floor), so a tile's entries
    /// always land in one partition — callers can merge partitions on separate
    /// threads and still group whole tiles.
    ///
    /// Fewer than `target` partitions come back when the data doesn't divide
    /// (few distinct tiles, or one mega-tile dominating a quantile).
    ///
    /// # Errors
    /// Returns an error if spilling the final buffer or mapping a chunk fails.
    pub fn into_sorted_partitions(mut self, target: usize) -> Result<Vec<SortedPartition>> {
        let target = target.max(1);
        if !self.chunks.is_empty() {
            self.spill()?; // flush the tail so every entry is on disk
        }
        let Self {
            buffer,
            chunks,
            samples,
            dir,
            ..
        } = self;
        let dir = Arc::new(dir);

        if chunks.is_empty() {
            return Ok(split_memory(buffer, target)
                .into_iter()
                .map(|entries| SortedPartition {
                    inner: PartitionInner::Memory(entries),
                    dir: Arc::clone(&dir),
                })
                .collect());
        }

        // Open every chunk once; partitions share the handles via positioned
        // reads, so file-descriptor cost stays per-chunk no matter how many
        // partitions merge concurrently.
        let mut files = Vec::with_capacity(chunks.len());
        for path in &chunks {
            let file = File::open(path)?;
            let len = file.metadata()?.len();
            files.push((Arc::new(file), len));
        }

        // Boundary keys: quantiles of the spill-time samples (uniform in entry
        // count), floored to tile starts so no tile straddles a boundary.
        let mut keys: Vec<u64> = samples.iter().flatten().map(|&(key, _)| key).collect();
        keys.sort_unstable();
        let mut boundaries: Vec<u64> = (1..target)
            .map(|i| {
                SortKey::from_bits(keys[i * keys.len() / target])
                    .tile_floor()
                    .to_bits()
            })
            .collect();
        boundaries.sort_unstable();
        boundaries.dedup();

        // Byte offset of each boundary in each chunk.
        let offsets: Vec<Vec<u64>> = files
            .iter()
            .zip(&samples)
            .map(|((file, len), samples)| boundary_offsets(file, *len, samples, &boundaries))
            .collect::<Result<_>>()?;

        let mut partitions = Vec::with_capacity(boundaries.len() + 1);
        for p in 0..=boundaries.len() {
            let mut slices = Vec::new();
            for (c, (file, len)) in files.iter().enumerate() {
                let start = if p == 0 { 0 } else { offsets[c][p - 1] };
                let end = if p == boundaries.len() {
                    *len
                } else {
                    offsets[c][p]
                };
                if start < end {
                    slices.push(ChunkSlice {
                        file: Arc::clone(file),
                        start,
                        end,
                    });
                }
            }
            if !slices.is_empty() {
                partitions.push(SortedPartition {
                    inner: PartitionInner::Chunks(slices),
                    dir: Arc::clone(&dir),
                });
            }
        }
        debug_assert!(!partitions.is_empty(), "chunks exist, so entries exist");
        Ok(partitions)
    }
}

/// Sort the in-memory buffer and split it at tile floors nearest the equal
/// entry-count quantiles. Always yields at least one (possibly empty) segment.
fn split_memory(mut buffer: Vec<Entry>, target: usize) -> Vec<Vec<Entry>> {
    buffer.par_sort_unstable();
    if target <= 1 || buffer.is_empty() {
        return vec![buffer];
    }
    let len = buffer.len();
    let mut boundaries: Vec<u64> = (1..target)
        .map(|i| buffer[i * len / target].key.tile_floor().to_bits())
        .collect();
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut parts = Vec::with_capacity(boundaries.len() + 1);
    for boundary in boundaries.into_iter().rev() {
        let at = buffer.partition_point(|e| e.key.to_bits() < boundary);
        let tail = buffer.split_off(at);
        if !tail.is_empty() {
            parts.push(tail);
        }
    }
    if !buffer.is_empty() || parts.is_empty() {
        parts.push(buffer);
    }
    parts.reverse();
    parts
}

/// Byte offset in the chunk of the first record whose key is `>= boundary`,
/// for each ascending boundary. Each scan starts at the latest spill-time
/// sample below the boundary (or wherever the previous boundary landed), so it
/// walks at most one sample stride of record headers, skipping over values.
fn boundary_offsets(
    file: &Arc<File>,
    len: u64,
    samples: &[(u64, u64)],
    boundaries: &[u64],
) -> Result<Vec<u64>> {
    let mut cursor = ChunkCursor::new(Arc::clone(file), 0, len);
    let mut out = Vec::with_capacity(boundaries.len());
    for &boundary in boundaries {
        let i = samples.partition_point(|&(key, _)| key < boundary);
        let hint = if i == 0 { 0 } else { samples[i - 1].1 };
        if hint > cursor.pos {
            cursor.seek(hint);
        }
        while cursor.pos < len {
            let record = cursor.pos;
            let (key, value_len) = cursor.read_header()?;
            if key >= boundary {
                cursor.seek(record); // leave the record for its partition
                break;
            }
            cursor.seek(record + (RECORD_HEADER + value_len) as u64);
        }
        out.push(cursor.pos);
    }
    Ok(out)
}

/// One disjoint slice of the sorted stream, produced by
/// [`ExternalMergeSort::into_sorted_partitions`]. Cheap to hold and `Send`;
/// [`open`](Self::open) builds the merge state, so a worker thread can claim a
/// partition and merge it locally.
pub struct SortedPartition {
    inner: PartitionInner,
    dir: Arc<TempDir>,
}

enum PartitionInner {
    Memory(Vec<Entry>),
    Chunks(Vec<ChunkSlice>),
}

/// A byte range of one chunk file, read via positioned reads on the shared
/// handle.
struct ChunkSlice {
    file: Arc<File>,
    start: u64,
    end: u64,
}

impl SortedPartition {
    /// Turn the partition into a streaming iterator over its entries in sorted
    /// order.
    ///
    /// # Errors
    /// Returns an error if a chunk record is malformed.
    pub fn open(self) -> Result<SortedEntries> {
        let inner = match self.inner {
            PartitionInner::Memory(entries) => Inner::Memory(entries.into_iter()),
            PartitionInner::Chunks(slices) => {
                let mut heap = BinaryHeap::with_capacity(slices.len());
                let mut readers = Vec::with_capacity(slices.len());
                for (index, slice) in slices.into_iter().enumerate() {
                    let mut cursor = ChunkCursor::new(slice.file, slice.start, slice.end);
                    if let Some(entry) = cursor.read_entry()? {
                        heap.push(Reverse(HeapItem {
                            entry,
                            source: index,
                        }));
                    }
                    readers.push(cursor);
                }
                Inner::Merge(MergeState { readers, heap })
            }
        };
        Ok(SortedEntries {
            inner,
            _dir: self.dir,
        })
    }
}

/// A streaming iterator over entries in sorted order. Yields `Result<Entry>`
/// because merging reads from disk. Holds the temp directory alive until dropped.
pub struct SortedEntries {
    inner: Inner,
    _dir: Arc<TempDir>,
}

enum Inner {
    Memory(std::vec::IntoIter<Entry>),
    Merge(MergeState),
}

struct MergeState {
    readers: Vec<ChunkCursor>,
    heap: BinaryHeap<Reverse<HeapItem>>,
}

impl Iterator for SortedEntries {
    type Item = Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            Inner::Memory(iter) => iter.next().map(Ok),
            Inner::Merge(state) => {
                let Reverse(HeapItem { entry, source }) = state.heap.pop()?;
                match state.readers[source].read_entry() {
                    Ok(Some(next)) => state.heap.push(Reverse(HeapItem {
                        entry: next,
                        source,
                    })),
                    Ok(None) => {}
                    Err(e) => return Some(Err(e)),
                }
                Some(Ok(entry))
            }
        }
    }
}

/// Heap element ordered by the entry's total order, then source for determinism.
struct HeapItem {
    entry: Entry,
    source: usize,
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.entry == other.entry && self.source == other.source
    }
}
impl Eq for HeapItem {}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.entry
            .cmp(&other.entry)
            .then(self.source.cmp(&other.source))
    }
}

/// Write a sorted chunk and return its sparse `(key, byte offset)` samples
/// (every [`SAMPLE_STRIDE`] entries, always including the first).
fn write_chunk(path: &Path, entries: &[Entry]) -> Result<Vec<(u64, u64)>> {
    let mut writer = BufWriter::new(File::create(path)?);
    let mut samples = Vec::with_capacity(entries.len() / SAMPLE_STRIDE + 1);
    let mut offset = 0u64;
    for (index, entry) in entries.iter().enumerate() {
        if index % SAMPLE_STRIDE == 0 {
            samples.push((entry.key.to_bits(), offset));
        }
        writer.write_all(&entry.key.to_bits().to_le_bytes())?;
        let len = u32::try_from(entry.value.len()).expect("feature value fits in u32");
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&entry.value)?;
        offset += (RECORD_HEADER + entry.value.len()) as u64;
    }
    writer.flush()?;
    Ok(samples)
}

/// Read-ahead window per cursor. Small enough that many cursors stay cheap,
/// large enough to amortize positioned-read syscalls over hundreds of records.
const CURSOR_BUF: usize = 64 << 10;

/// Reads records sequentially from a byte range of a chunk file through a
/// small read-ahead buffer over positioned reads, so concurrent cursors can
/// share one handle without seeking under each other.
struct ChunkCursor {
    file: Arc<File>,
    /// File offset of the next unread byte.
    pos: u64,
    /// Exclusive end of this cursor's range (a record boundary).
    end: u64,
    buf: Vec<u8>,
    /// File offset of `buf[0]`.
    buf_start: u64,
}

impl ChunkCursor {
    fn new(file: Arc<File>, start: u64, end: u64) -> Self {
        Self {
            file,
            pos: start,
            end,
            buf: Vec::new(),
            buf_start: start,
        }
    }

    /// Move to an absolute file offset (record boundaries only).
    fn seek(&mut self, pos: u64) {
        self.pos = pos;
    }

    /// Return `n` bytes at `pos` and advance past them, refilling the buffer
    /// from the file when the range isn't already buffered.
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.pos < self.buf_start || self.pos + n as u64 > self.buf_start + self.buf.len() as u64
        {
            let want = (self.end - self.pos).min(CURSOR_BUF.max(n) as u64) as usize;
            if want < n {
                return Err(Error::Codec("truncated sort-chunk record"));
            }
            self.buf.resize(want, 0);
            read_exact_at(&self.file, &mut self.buf, self.pos)?;
            self.buf_start = self.pos;
        }
        let at = (self.pos - self.buf_start) as usize;
        self.pos += n as u64;
        Ok(&self.buf[at..at + n])
    }

    /// Read a record header `(key, value length)` and advance past it.
    fn read_header(&mut self) -> Result<(u64, usize)> {
        let header = self.take(RECORD_HEADER)?;
        let key = u64::from_le_bytes(header[..8].try_into().expect("8-byte key"));
        let len = u32::from_le_bytes(header[8..].try_into().expect("4-byte len")) as usize;
        Ok((key, len))
    }

    /// Read the next entry, or `None` at the end of the range.
    fn read_entry(&mut self) -> Result<Option<Entry>> {
        if self.pos >= self.end {
            return Ok(None);
        }
        let (key, len) = self.read_header()?;
        let value = self.take(len)?.to_vec();
        Ok(Some(Entry {
            key: SortKey::from_bits(key),
            value,
        }))
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "chunk file ended early",
                ));
            }
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::TileCoord;

    fn entry(z: u8, x: u32, y: u32, layer: u8, zorder: i32, value: &[u8]) -> Entry {
        let key = SortKey::new(TileCoord::new(z, x, y).unwrap(), layer, zorder).unwrap();
        Entry::new(key, value.to_vec())
    }

    fn collect(sorter: ExternalMergeSort) -> Vec<Entry> {
        sorter
            .into_sorted()
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect()
    }

    /// A deterministic spread of entries across many tiles (several per tile),
    /// inserted in a scrambled order.
    fn scrambled_entries(n: u32) -> Vec<Entry> {
        (0..n)
            .map(|i| {
                let scrambled = i.wrapping_mul(2_654_435_761) % n;
                let tile = scrambled / 4; // ~4 entries per tile
                let x = tile % 64;
                let y = tile / 64;
                entry(14, x, y, 0, 0, &scrambled.to_le_bytes())
            })
            .collect()
    }

    #[test]
    fn sorts_in_memory_when_small() {
        let mut sorter = ExternalMergeSort::new(1 << 20).unwrap();
        sorter.add(entry(10, 5, 5, 1, 0, b"c")).unwrap();
        sorter.add(entry(9, 0, 0, 0, 0, b"a")).unwrap();
        sorter.add(entry(10, 5, 5, 0, 0, b"b")).unwrap();
        let sorted = collect(sorter);
        let values: Vec<_> = sorted.iter().map(|e| e.value.clone()).collect();
        assert_eq!(values, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn value_bytes_break_ties_for_total_order() {
        let mut sorter = ExternalMergeSort::new(1 << 20).unwrap();
        // Same key, different values.
        sorter.add(entry(10, 5, 5, 0, 0, b"y")).unwrap();
        sorter.add(entry(10, 5, 5, 0, 0, b"x")).unwrap();
        let sorted = collect(sorter);
        assert_eq!(sorted[0].value, b"x");
        assert_eq!(sorted[1].value, b"y");
    }

    #[test]
    fn merges_many_spilled_chunks_correctly() {
        // Tiny buffer forces frequent spills.
        let mut sorter = ExternalMergeSort::new(64).unwrap();
        let n = 2000u32;
        // Insert in reverse tile order with a per-entry payload.
        for i in (0..n).rev() {
            let x = i % 128;
            let y = i / 128;
            sorter.add(entry(14, x, y, 0, 0, &i.to_le_bytes())).unwrap();
        }
        assert_eq!(sorter.len(), u64::from(n));
        let sorted = collect(sorter);
        assert_eq!(sorted.len(), n as usize);

        // Output must be non-decreasing by the full entry order and contain
        // every input exactly once.
        for pair in sorted.windows(2) {
            assert!(pair[0] <= pair[1], "sorted order violated");
        }
        let mut seen: Vec<u32> = sorted
            .iter()
            .map(|e| u32::from_le_bytes(e.value[..4].try_into().unwrap()))
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..n).collect::<Vec<_>>());
    }

    #[test]
    fn empty_sorter_yields_nothing() {
        let sorter = ExternalMergeSort::new(1 << 20).unwrap();
        assert!(sorter.is_empty());
        assert_eq!(collect(sorter).len(), 0);
    }

    #[test]
    fn empty_sorter_yields_one_empty_partition() {
        let sorter = ExternalMergeSort::new(1 << 20).unwrap();
        let partitions = sorter.into_sorted_partitions(8).unwrap();
        assert_eq!(partitions.len(), 1);
        assert_eq!(
            partitions
                .into_iter()
                .next()
                .unwrap()
                .open()
                .unwrap()
                .count(),
            0
        );
    }

    /// Partitioning must be transparent: concatenating the partitions (for any
    /// target) reproduces the exact single-merge stream.
    fn assert_partitions_concatenate(buffer_bytes: usize, n: u32, target: usize) {
        let mut a = ExternalMergeSort::new(buffer_bytes).unwrap();
        let mut b = ExternalMergeSort::new(buffer_bytes).unwrap();
        for e in scrambled_entries(n) {
            a.add(e.clone()).unwrap();
            b.add(e).unwrap();
        }
        let expected = collect(a);

        let partitions = b.into_sorted_partitions(target).unwrap();
        assert!(!partitions.is_empty() && partitions.len() <= target);
        let mut concatenated = Vec::new();
        let mut tiles_per_partition: Vec<Vec<TileCoord>> = Vec::new();
        for partition in partitions {
            let entries: Vec<Entry> = partition
                .open()
                .unwrap()
                .map(std::result::Result::unwrap)
                .collect();
            let mut tiles: Vec<TileCoord> = entries.iter().map(|e| e.key.tile()).collect();
            tiles.dedup();
            tiles_per_partition.push(tiles);
            concatenated.extend(entries);
        }
        assert_eq!(
            concatenated, expected,
            "partitions must concatenate to the full sort"
        );

        // No tile may appear in more than one partition.
        let all_tiles: Vec<TileCoord> = tiles_per_partition.iter().flatten().copied().collect();
        let distinct = {
            let mut t = all_tiles.clone();
            t.sort_unstable_by_key(|t| (t.z(), t.x(), t.y()));
            t.dedup();
            t.len()
        };
        assert_eq!(
            all_tiles.len(),
            distinct,
            "a tile straddled a partition boundary"
        );
    }

    #[test]
    fn spilled_partitions_concatenate_to_the_full_sorted_stream() {
        // Tiny buffer → many chunks; more partitions than a laptop has cores.
        assert_partitions_concatenate(256, 4000, 7);
    }

    #[test]
    fn in_memory_partitions_concatenate_to_the_full_sorted_stream() {
        // Large buffer → no spill; the memory path must split identically.
        assert_partitions_concatenate(1 << 24, 4000, 5);
    }

    #[test]
    fn single_partition_matches_into_sorted() {
        assert_partitions_concatenate(512, 1000, 1);
    }

    #[test]
    fn one_mega_tile_cannot_be_split() {
        let mut sorter = ExternalMergeSort::new(128).unwrap();
        for i in 0..500u32 {
            sorter.add(entry(3, 1, 2, 0, 0, &i.to_le_bytes())).unwrap();
        }
        let partitions = sorter.into_sorted_partitions(6).unwrap();
        assert_eq!(partitions.len(), 1, "a single tile is indivisible");
        let entries: Vec<Entry> = partitions
            .into_iter()
            .next()
            .unwrap()
            .open()
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert_eq!(entries.len(), 500);
    }
}
