//! PMTiles v3 writer.
//!
//! Single-file archive: a 127-byte header, a gzip'd root directory (kept
//! within the spec's 16 KB header+root budget by splitting entries into gzip'd
//! leaf directories when needed), gzip'd JSON metadata, then the tile data.
//! Tiles arrive in pipeline order and are staged in a temp file; `finish`
//! re-orders them by Hilbert tile id so the archive is **clustered** (data
//! offsets non-decreasing in tile-id order), which is what readers optimize
//! for.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{TileArchiveMetadata, TileCompression};
use crate::error::{Error, Result};
use crate::geom::TileCoord;

const HEADER_LEN: usize = 127;
/// Header plus root directory must fit in the first 16 KB (spec recommendation).
const MAX_ROOT_BYTES: usize = 16_384 - HEADER_LEN;

/// Streaming PMTiles v3 writer with the same surface as `MbtilesWriter`.
pub struct PmtilesWriter {
    path: PathBuf,
    compression: TileCompression,
    staging: BufWriter<File>,
    staging_len: u64,
    /// `(tile_id, staging offset, length)` per written tile.
    entries: Vec<(u64, u64, u32)>,
    metadata: Option<TileArchiveMetadata>,
}

impl PmtilesWriter {
    /// Create a writer targeting `path` (written at [`finish`](Self::finish)).
    ///
    /// # Errors
    /// Returns an error if the staging temp file cannot be created.
    pub fn create(path: impl AsRef<Path>, compression: TileCompression) -> Result<Self> {
        Ok(Self {
            path: path.as_ref().to_owned(),
            compression,
            staging: BufWriter::new(tempfile::tempfile()?),
            staging_len: 0,
            entries: Vec::new(),
            metadata: None,
        })
    }

    /// Record the archive metadata (header fields + the JSON blob).
    ///
    /// # Errors
    /// Infallible today; `Result` for parity with the MBTiles writer.
    pub fn write_metadata(&mut self, metadata: &TileArchiveMetadata) -> Result<()> {
        self.metadata = Some(metadata.clone());
        Ok(())
    }

    /// Append one already-compressed tile.
    ///
    /// # Errors
    /// Returns an error on staging I/O failure or an out-of-range coordinate.
    pub fn write_tile_compressed(&mut self, coord: TileCoord, compressed: &[u8]) -> Result<()> {
        let id = zxy_to_tile_id(coord.z(), coord.x(), coord.y());
        self.staging.write_all(compressed)?;
        let len =
            u32::try_from(compressed.len()).map_err(|_| Error::Config("tile larger than 4 GiB"))?;
        self.entries.push((id, self.staging_len, len));
        self.staging_len += u64::from(len);
        Ok(())
    }

    /// Assemble and write the final archive.
    ///
    /// # Errors
    /// Returns an error on I/O failure or if no metadata was written.
    pub fn finish(&mut self) -> Result<()> {
        let metadata = self
            .metadata
            .take()
            .ok_or(Error::Config("pmtiles: metadata must be written"))?;

        // Cluster: entries sorted by tile id, data re-ordered to match.
        self.entries.sort_unstable_by_key(|&(id, _, _)| id);
        self.staging.flush()?;
        let mut staging = self.staging.get_ref();

        let mut data = Vec::with_capacity(self.staging_len as usize);
        let mut directory = Vec::with_capacity(self.entries.len());
        for &(id, offset, len) in &self.entries {
            let start = data.len() as u64;
            staging.seek(SeekFrom::Start(offset))?;
            let mut buf = vec![0u8; len as usize];
            staging.read_exact(&mut buf)?;
            data.extend_from_slice(&buf);
            directory.push(DirEntry {
                tile_id: id,
                offset: start,
                length: len,
                run_length: 1,
            });
        }

        let (root, leaves) = build_directories(&directory)?;
        let metadata_json = gzip(metadata_json(&metadata).as_bytes())?;

        let root_offset = HEADER_LEN as u64;
        let metadata_offset = root_offset + root.len() as u64;
        let leaves_offset = metadata_offset + metadata_json.len() as u64;
        let data_offset = leaves_offset + leaves.len() as u64;

        let header = Header {
            root_offset,
            root_len: root.len() as u64,
            metadata_offset,
            metadata_len: metadata_json.len() as u64,
            leaves_offset,
            leaves_len: leaves.len() as u64,
            data_offset,
            data_len: data.len() as u64,
            addressed_tiles: directory.len() as u64,
            tile_entries: directory.len() as u64,
            tile_contents: directory.len() as u64,
            clustered: true,
            tile_compression: self.compression,
            min_zoom: metadata.min_zoom,
            max_zoom: metadata.max_zoom,
            bounds: metadata.bounds,
            center: metadata.center,
        };

        let mut out = BufWriter::new(File::create(&self.path)?);
        out.write_all(&header.encode())?;
        out.write_all(&root)?;
        out.write_all(&metadata_json)?;
        out.write_all(&leaves)?;
        out.write_all(&data)?;
        out.flush()?;
        Ok(())
    }
}

/// One directory entry, per the v3 spec.
#[derive(Debug, Clone, Copy)]
struct DirEntry {
    tile_id: u64,
    offset: u64,
    length: u32,
    /// `>= 1` for tile entries; `0` marks a leaf-directory pointer.
    run_length: u32,
}

struct Header {
    root_offset: u64,
    root_len: u64,
    metadata_offset: u64,
    metadata_len: u64,
    leaves_offset: u64,
    leaves_len: u64,
    data_offset: u64,
    data_len: u64,
    addressed_tiles: u64,
    tile_entries: u64,
    tile_contents: u64,
    clustered: bool,
    tile_compression: TileCompression,
    min_zoom: u8,
    max_zoom: u8,
    bounds: (f64, f64, f64, f64),
    center: (f64, f64, u8),
}

impl Header {
    fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..7].copy_from_slice(b"PMTiles");
        b[7] = 3; // spec version
        let mut put_u64 = |at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        put_u64(8, self.root_offset);
        put_u64(16, self.root_len);
        put_u64(24, self.metadata_offset);
        put_u64(32, self.metadata_len);
        put_u64(40, self.leaves_offset);
        put_u64(48, self.leaves_len);
        put_u64(56, self.data_offset);
        put_u64(64, self.data_len);
        put_u64(72, self.addressed_tiles);
        put_u64(80, self.tile_entries);
        put_u64(88, self.tile_contents);
        b[96] = u8::from(self.clustered);
        b[97] = 2; // internal compression: gzip
        b[98] = match self.tile_compression {
            TileCompression::None => 1,
            TileCompression::Gzip => 2,
        };
        b[99] = 1; // tile type: MVT
        b[100] = self.min_zoom;
        b[101] = self.max_zoom;
        let e7 = |v: f64| ((v * 1e7) as i32).to_le_bytes();
        b[102..106].copy_from_slice(&e7(self.bounds.0)); // min lon
        b[106..110].copy_from_slice(&e7(self.bounds.1)); // min lat
        b[110..114].copy_from_slice(&e7(self.bounds.2)); // max lon
        b[114..118].copy_from_slice(&e7(self.bounds.3)); // max lat
        b[118] = self.center.2;
        b[119..123].copy_from_slice(&e7(self.center.0));
        b[123..127].copy_from_slice(&e7(self.center.1));
        b
    }
}

/// Serialize + gzip the root directory, splitting entries into leaf
/// directories (doubling the per-leaf entry count) until the root fits the
/// spec's 16 KB header+root budget.
fn build_directories(entries: &[DirEntry]) -> Result<(Vec<u8>, Vec<u8>)> {
    let root_only = gzip(&serialize_directory(entries))?;
    if root_only.len() <= MAX_ROOT_BYTES {
        return Ok((root_only, Vec::new()));
    }

    let mut per_leaf = 4096usize;
    loop {
        let mut root_entries = Vec::new();
        let mut leaves = Vec::new();
        for chunk in entries.chunks(per_leaf) {
            let leaf = gzip(&serialize_directory(chunk))?;
            root_entries.push(DirEntry {
                tile_id: chunk[0].tile_id,
                offset: leaves.len() as u64,
                length: u32::try_from(leaf.len())
                    .map_err(|_| Error::Config("pmtiles leaf directory too large"))?,
                run_length: 0, // leaf pointer
            });
            leaves.extend_from_slice(&leaf);
        }
        let root = gzip(&serialize_directory(&root_entries))?;
        if root.len() <= MAX_ROOT_BYTES {
            return Ok((root, leaves));
        }
        per_leaf *= 2;
    }
}

/// Spec layout: entry count, delta tile ids, run lengths, lengths, then
/// offsets (`0` = previous offset + previous length, else `offset + 1`).
fn serialize_directory(entries: &[DirEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(entries.len() * 8 + 8);
    varint(&mut out, entries.len() as u64);
    let mut last_id = 0u64;
    for e in entries {
        varint(&mut out, e.tile_id - last_id);
        last_id = e.tile_id;
    }
    for e in entries {
        varint(&mut out, u64::from(e.run_length));
    }
    for e in entries {
        varint(&mut out, u64::from(e.length));
    }
    let mut prev_end = 0u64;
    for (i, e) in entries.iter().enumerate() {
        // Spec: 0 = "previous offset + previous length", but only ever for
        // entries after the first (readers treat a leading 0 as offset+1 form).
        if i > 0 && e.offset == prev_end {
            varint(&mut out, 0);
        } else {
            varint(&mut out, e.offset + 1);
        }
        prev_end = e.offset + u64::from(e.length);
    }
    out
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

/// The PMTiles metadata JSON object (name/description/attribution/version,
/// archive type, and MBTiles-style `vector_layers`).
fn metadata_json(metadata: &TileArchiveMetadata) -> String {
    let mut object = serde_json::Map::new();
    if let Some(name) = &metadata.name {
        object.insert("name".into(), name.clone().into());
    }
    if let Some(description) = &metadata.description {
        object.insert("description".into(), description.clone().into());
    }
    if let Some(attribution) = &metadata.attribution {
        object.insert("attribution".into(), attribution.clone().into());
    }
    if let Some(version) = &metadata.version {
        object.insert("version".into(), version.clone().into());
    }
    object.insert("type".into(), metadata.layer_type.clone().into());
    object.insert(
        "vector_layers".into(),
        serde_json::Value::Array(
            metadata
                .vector_layers
                .iter()
                .map(|layer| {
                    let mut l = serde_json::Map::new();
                    l.insert("id".into(), layer.id.clone().into());
                    l.insert(
                        "fields".into(),
                        layer
                            .fields
                            .iter()
                            .map(|(k, t)| (k.clone(), t.as_str().into()))
                            .collect::<serde_json::Map<_, _>>()
                            .into(),
                    );
                    if let Some(z) = layer.min_zoom {
                        l.insert("minzoom".into(), z.into());
                    }
                    if let Some(z) = layer.max_zoom {
                        l.insert("maxzoom".into(), z.into());
                    }
                    if let Some(d) = &layer.description {
                        l.insert("description".into(), d.clone().into());
                    }
                    serde_json::Value::Object(l)
                })
                .collect(),
        ),
    );
    serde_json::Value::Object(object).to_string()
}

/// PMTiles tile id: tiles of all zooms below `z`, plus the Hilbert-curve index
/// of `(x, y)` within zoom `z`.
pub fn zxy_to_tile_id(z: u8, x: u32, y: u32) -> u64 {
    let base: u64 = ((1u64 << (2 * u64::from(z))) - 1) / 3;
    let n: i64 = 1i64 << z;
    let (mut tx, mut ty) = (i64::from(x), i64::from(y));
    let mut d: i64 = 0;
    let mut s: i64 = n / 2;
    while s > 0 {
        let rx = i64::from((tx & s) > 0);
        let ry = i64::from((ty & s) > 0);
        d += s * s * ((3 * rx) ^ ry);
        // Rotate the quadrant (canonical Hilbert xy→d step).
        if ry == 0 {
            if rx == 1 {
                tx = n - 1 - tx;
                ty = n - 1 - ty;
            }
            std::mem::swap(&mut tx, &mut ty);
        }
        s /= 2;
    }
    base + d as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_ids_match_spec_vectors() {
        assert_eq!(zxy_to_tile_id(0, 0, 0), 0);
        assert_eq!(zxy_to_tile_id(1, 0, 0), 1);
        assert_eq!(zxy_to_tile_id(1, 0, 1), 2);
        assert_eq!(zxy_to_tile_id(1, 1, 1), 3);
        assert_eq!(zxy_to_tile_id(1, 1, 0), 4);
        assert_eq!(zxy_to_tile_id(2, 0, 0), 5);
    }

    #[test]
    fn tile_ids_walk_a_hilbert_curve() {
        // Every id at z3 is unique, covers the range, and consecutive ids are
        // grid-adjacent (the defining Hilbert property).
        let z = 3u8;
        let n = 1u32 << z;
        let base = zxy_to_tile_id(z, 0, 0);
        let mut coords_by_index = vec![None; (n * n) as usize];
        for x in 0..n {
            for y in 0..n {
                let d = (zxy_to_tile_id(z, x, y) - base) as usize;
                assert!(coords_by_index[d].is_none(), "duplicate id");
                coords_by_index[d] = Some((x as i32, y as i32));
            }
        }
        for pair in coords_by_index.windows(2) {
            let (ax, ay) = pair[0].unwrap();
            let (bx, by) = pair[1].unwrap();
            assert_eq!((ax - bx).abs() + (ay - by).abs(), 1, "curve steps by 1");
        }
    }

    #[test]
    fn varint_encodes_like_leb128() {
        let mut out = Vec::new();
        varint(&mut out, 0);
        varint(&mut out, 127);
        varint(&mut out, 128);
        varint(&mut out, 300);
        assert_eq!(out, vec![0x00, 0x7f, 0x80, 0x01, 0xac, 0x02]);
    }

    #[test]
    fn directory_serialization_matches_spec_layout() {
        let entries = [
            DirEntry {
                tile_id: 5,
                offset: 0,
                length: 10,
                run_length: 1,
            },
            DirEntry {
                tile_id: 7,
                offset: 10,
                length: 4,
                run_length: 1,
            },
        ];
        let bytes = serialize_directory(&entries);
        // count=2 | deltas 5,2 | run lengths 1,1 | lengths 10,4 | offsets 1,0
        // (first offset written literally as offset+1, second contiguous -> 0)
        assert_eq!(bytes, vec![2, 5, 2, 1, 1, 10, 4, 1, 0]);
    }
}
