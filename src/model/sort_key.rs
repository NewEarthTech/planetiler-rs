//! The 64-bit feature sort key.
//!
//! # Layout (most- to least-significant bit)
//!
//! | bits | field | meaning |
//! |-----:|-------|---------|
//! | 4  | `z`        | zoom level, `0..=15` |
//! | 15 | `x`        | tile column, `0..2^z` |
//! | 15 | `y`        | tile row, `0..2^z` |
//! | 6  | `layer`    | layer index, `0..64` |
//! | 20 | `zorder`   | draw order, offset-binary encoded signed |
//! | 4  | `reserved` | must be zero (headroom) |
//!
//! Sorting the raw `u64` ascending groups every feature of a tile together
//! (ordered by `z`, then `x`, then `y`) and, within a tile, orders by `layer`
//! then `zorder`. Ties beyond `zorder` are broken by the feature value bytes at
//! the [`Entry`](super::Entry) level, giving a total, reproducible order.
//!
//! `zorder` uses **offset-binary** (add `2^19`), *not* zigzag: offset-binary
//! preserves signed ordering in unsigned space, which zigzag does not. (Zigzag
//! appears later, in MVT geometry-command encoding, where compactness — not
//! order — is what matters.)
//!
//! The tile bits are packed row-major `(z, x, y)` today, which is the right
//! grouping for MBTiles. PMTiles wants Hilbert clustering; because callers only
//! ever go through [`SortKey::new`] and the accessors, the internal packing can
//! switch to a Hilbert index in Phase 5 without touching them.

use crate::error::{Error, Result};
use crate::geom::TileCoord;

const Z_BITS: u32 = 4;
const X_BITS: u32 = 15;
const Y_BITS: u32 = 15;
const LAYER_BITS: u32 = 6;
const ZORDER_BITS: u32 = 20;
const RESERVED_BITS: u32 = 4;

const _: () =
    assert!(Z_BITS + X_BITS + Y_BITS + LAYER_BITS + ZORDER_BITS + RESERVED_BITS == u64::BITS);

const ZORDER_SHIFT: u32 = RESERVED_BITS; // 4
const LAYER_SHIFT: u32 = ZORDER_SHIFT + ZORDER_BITS; // 24
const Y_SHIFT: u32 = LAYER_SHIFT + LAYER_BITS; // 30
const X_SHIFT: u32 = Y_SHIFT + Y_BITS; // 45
const Z_SHIFT: u32 = X_SHIFT + X_BITS; // 60

const Z_MASK: u64 = (1 << Z_BITS) - 1;
const COORD_MASK: u64 = (1 << X_BITS) - 1; // X and Y share the 15-bit width
const LAYER_MASK: u64 = (1 << LAYER_BITS) - 1;
const ZORDER_MASK: u64 = (1 << ZORDER_BITS) - 1;
const ZORDER_BIAS: i64 = 1 << (ZORDER_BITS - 1); // 2^19

/// Maximum layer index (the field holds `0..=MAX_LAYER`, i.e. 64 layers).
pub const MAX_LAYER: u8 = (1 << LAYER_BITS) - 1; // 63
/// Minimum representable z-order.
pub const ZORDER_MIN: i32 = -(1 << (ZORDER_BITS - 1)); // -524288
/// Maximum representable z-order.
pub const ZORDER_MAX: i32 = (1 << (ZORDER_BITS - 1)) - 1; // 524287

/// A packed 64-bit feature sort key. See the [module docs](self) for the bit
/// layout and ordering guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SortKey(u64);

impl SortKey {
    /// Build a sort key from a tile, layer index, and z-order.
    ///
    /// # Errors
    /// Returns [`Error::SortKeyRange`] if `layer > MAX_LAYER` or `zorder` is
    /// outside `[ZORDER_MIN, ZORDER_MAX]`. Tile coordinates are already
    /// range-checked by [`TileCoord`].
    pub fn new(tile: TileCoord, layer: u8, zorder: i32) -> Result<Self> {
        if layer > MAX_LAYER {
            return Err(Error::SortKeyRange("layer index exceeds MAX_LAYER"));
        }
        if !(ZORDER_MIN..=ZORDER_MAX).contains(&zorder) {
            return Err(Error::SortKeyRange("zorder out of range"));
        }
        let zorder_ob = (zorder as i64 + ZORDER_BIAS) as u64; // offset-binary in [0, 2^20)
        let bits = ((tile.z() as u64) << Z_SHIFT)
            | ((tile.x() as u64) << X_SHIFT)
            | ((tile.y() as u64) << Y_SHIFT)
            | ((layer as u64) << LAYER_SHIFT)
            | (zorder_ob << ZORDER_SHIFT);
        Ok(Self(bits))
    }

    /// The raw packed value (for writing to disk).
    #[inline]
    pub fn to_bits(self) -> u64 {
        self.0
    }

    /// Wrap a raw packed value read back from disk.
    ///
    /// No validation is performed; [`SortKey::tile`] will panic if `bits` was
    /// not produced by [`SortKey::new`] (e.g. it encodes `x ≥ 2^z`).
    #[inline]
    pub fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Zoom level.
    #[inline]
    pub fn z(self) -> u8 {
        ((self.0 >> Z_SHIFT) & Z_MASK) as u8
    }

    /// Tile column.
    #[inline]
    pub fn x(self) -> u32 {
        ((self.0 >> X_SHIFT) & COORD_MASK) as u32
    }

    /// Tile row.
    #[inline]
    pub fn y(self) -> u32 {
        ((self.0 >> Y_SHIFT) & COORD_MASK) as u32
    }

    /// Layer index.
    #[inline]
    pub fn layer(self) -> u8 {
        ((self.0 >> LAYER_SHIFT) & LAYER_MASK) as u8
    }

    /// Signed z-order (draw order within a tile+layer).
    #[inline]
    pub fn zorder(self) -> i32 {
        let ob = ((self.0 >> ZORDER_SHIFT) & ZORDER_MASK) as i64;
        (ob - ZORDER_BIAS) as i32
    }

    /// Reconstruct the [`TileCoord`] this key was built from.
    ///
    /// # Panics
    /// If the key was produced by [`SortKey::from_bits`] with a value that does
    /// not encode a valid tile.
    pub fn tile(self) -> TileCoord {
        TileCoord::new(self.z(), self.x(), self.y())
            .expect("sort key encodes a valid tile coordinate")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(z: u8, x: u32, y: u32) -> TileCoord {
        TileCoord::new(z, x, y).unwrap()
    }

    #[test]
    fn round_trips_all_fields() {
        let cases = [
            (tile(0, 0, 0), 0u8, 0i32),
            (tile(14, 8191, 5000), 12, 250),
            (tile(15, 32767, 32767), MAX_LAYER, ZORDER_MAX),
            (tile(15, 0, 0), 0, ZORDER_MIN),
            (tile(7, 63, 100), 5, -42),
        ];
        for (t, layer, zorder) in cases {
            let k = SortKey::new(t, layer, zorder).unwrap();
            assert_eq!(k.tile(), t, "tile");
            assert_eq!(k.z(), t.z(), "z");
            assert_eq!(k.x(), t.x(), "x");
            assert_eq!(k.y(), t.y(), "y");
            assert_eq!(k.layer(), layer, "layer");
            assert_eq!(k.zorder(), zorder, "zorder");
            assert_eq!(SortKey::from_bits(k.to_bits()), k, "bits round-trip");
        }
    }

    #[test]
    fn rejects_out_of_range_fields() {
        assert!(SortKey::new(tile(0, 0, 0), MAX_LAYER + 1, 0).is_err());
        assert!(SortKey::new(tile(0, 0, 0), 0, ZORDER_MAX + 1).is_err());
        assert!(SortKey::new(tile(0, 0, 0), 0, ZORDER_MIN - 1).is_err());
    }

    #[test]
    fn orders_by_tile_then_layer_then_zorder() {
        let base = tile(10, 100, 200);
        // Same tile: layer is more significant than zorder.
        let a = SortKey::new(base, 1, 500).unwrap();
        let b = SortKey::new(base, 2, -500).unwrap();
        assert!(a < b, "lower layer sorts first regardless of zorder");

        // Same tile and layer: zorder orders, including across zero.
        let neg = SortKey::new(base, 1, -5).unwrap();
        let zero = SortKey::new(base, 1, 0).unwrap();
        let pos = SortKey::new(base, 1, 5).unwrap();
        assert!(
            neg < zero && zero < pos,
            "offset-binary keeps signed order: {neg:?} {zero:?} {pos:?}"
        );
    }

    #[test]
    fn groups_features_by_tile() {
        // Lower zoom sorts before higher; within a zoom, row-major (x then y).
        let z_low = SortKey::new(tile(9, 0, 0), MAX_LAYER, ZORDER_MAX).unwrap();
        let z_high = SortKey::new(tile(10, 0, 0), 0, ZORDER_MIN).unwrap();
        assert!(z_low < z_high, "zoom dominates");

        let x0 = SortKey::new(tile(10, 0, 999), 0, 0).unwrap();
        let x1 = SortKey::new(tile(10, 1, 0), 0, 0).unwrap();
        assert!(x0 < x1, "x dominates y within a zoom");

        let y0 = SortKey::new(tile(10, 5, 0), 0, 0).unwrap();
        let y1 = SortKey::new(tile(10, 5, 1), 0, 0).unwrap();
        assert!(y0 < y1, "y orders within a column");
    }

    #[test]
    fn reserved_bits_are_zero() {
        let k = SortKey::new(tile(15, 32767, 32767), MAX_LAYER, ZORDER_MAX).unwrap();
        assert_eq!(k.to_bits() & ((1 << RESERVED_BITS) - 1), 0);
    }
}
