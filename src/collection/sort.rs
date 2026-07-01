//! Generic external merge sort over [`Entry`] records.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::error::Result;
use crate::model::{Entry, SortKey};

/// Rough per-entry RAM overhead beyond the value bytes, used to decide when to
/// spill. Not exact — just keeps the buffer estimate honest.
const ENTRY_OVERHEAD: usize = std::mem::size_of::<Entry>() + 16;

/// Sorts [`Entry`] records that may exceed RAM by spilling sorted runs to disk
/// and k-way merging them. Entries are ordered by [`Entry`]'s total order (sort
/// key, then value bytes), so the output is deterministic.
pub struct ExternalMergeSort {
    buffer: Vec<Entry>,
    buffer_bytes: usize,
    max_buffer_bytes: usize,
    chunks: Vec<PathBuf>,
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
        write_chunk(&path, &self.buffer)?;
        self.chunks.push(path);
        self.buffer.clear();
        self.buffer_bytes = 0;
        Ok(())
    }

    /// Consume the sorter and stream entries in sorted order.
    ///
    /// If nothing spilled, this is an in-memory sort; otherwise it spills the
    /// remaining buffer and k-way merges all chunks.
    pub fn into_sorted(mut self) -> Result<SortedEntries> {
        if self.chunks.is_empty() {
            self.buffer.sort_unstable();
            let entries = std::mem::take(&mut self.buffer);
            return Ok(SortedEntries {
                inner: Inner::Memory(entries.into_iter()),
                _dir: self.dir,
            });
        }
        self.spill()?;
        let mut heap = BinaryHeap::with_capacity(self.chunks.len());
        let mut readers = Vec::with_capacity(self.chunks.len());
        for (index, path) in self.chunks.iter().enumerate() {
            let mut reader = ChunkReader::open(path)?;
            if let Some(entry) = reader.read_entry()? {
                heap.push(Reverse(HeapItem {
                    entry,
                    source: index,
                }));
            }
            readers.push(reader);
        }
        Ok(SortedEntries {
            inner: Inner::Merge(MergeState { readers, heap }),
            _dir: self.dir,
        })
    }
}

/// A streaming iterator over entries in sorted order. Yields `Result<Entry>`
/// because merging reads from disk. Holds the temp directory alive until dropped.
pub struct SortedEntries {
    inner: Inner,
    _dir: TempDir,
}

enum Inner {
    Memory(std::vec::IntoIter<Entry>),
    Merge(MergeState),
}

struct MergeState {
    readers: Vec<ChunkReader>,
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

fn write_chunk(path: &Path, entries: &[Entry]) -> Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    for entry in entries {
        writer.write_all(&entry.key.to_bits().to_le_bytes())?;
        let len = u32::try_from(entry.value.len()).expect("feature value fits in u32");
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&entry.value)?;
    }
    writer.flush()?;
    Ok(())
}

struct ChunkReader {
    reader: BufReader<File>,
}

impl ChunkReader {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            reader: BufReader::new(File::open(path)?),
        })
    }

    /// Read the next entry, or `None` at end of file.
    fn read_entry(&mut self) -> Result<Option<Entry>> {
        let mut key_bytes = [0u8; 8];
        match self.reader.read_exact(&mut key_bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let mut len_bytes = [0u8; 4];
        self.reader.read_exact(&mut len_bytes)?;
        let len = u32::from_le_bytes(len_bytes) as usize;
        let mut value = vec![0u8; len];
        self.reader.read_exact(&mut value)?;
        Ok(Some(Entry {
            key: SortKey::from_bits(u64::from_le_bytes(key_bytes)),
            value,
        }))
    }
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
}
