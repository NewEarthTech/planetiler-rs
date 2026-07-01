//! A sortable, encoded feature record.

use std::cmp::Ordering;

use super::SortKey;

/// One encoded feature ready for the external sort: its [`SortKey`] plus the
/// opaque serialized value (feature id, geometry, tags, group/limit — encoded
/// by [`crate::render`] starting in Phase 2).
///
/// Ordering is by `key` first, then lexicographically by the `value` bytes. The
/// value tiebreak makes the total order **stable and reproducible** and means
/// byte-identical records are true duplicates (used by tile dedup), mirroring
/// Planetiler's `SortableFeature`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Ordering key: tile, layer, and z-order.
    pub key: SortKey,
    /// Opaque encoded feature payload. Concrete format defined in Phase 2.
    pub value: Vec<u8>,
}

impl Entry {
    /// Create an entry from a key and its encoded value.
    pub fn new(key: SortKey, value: Vec<u8>) -> Self {
        Self { key, value }
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .cmp(&other.key)
            .then_with(|| self.value.cmp(&other.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::TileCoord;

    fn key(z: u8, x: u32, y: u32, layer: u8, zorder: i32) -> SortKey {
        SortKey::new(TileCoord::new(z, x, y).unwrap(), layer, zorder).unwrap()
    }

    #[test]
    fn orders_by_key_first() {
        let lo = Entry::new(key(10, 0, 0, 0, 0), vec![255]);
        let hi = Entry::new(key(10, 0, 0, 1, 0), vec![0]);
        assert!(lo < hi, "key dominates the value bytes");
    }

    #[test]
    fn value_bytes_break_ties() {
        let k = key(10, 5, 5, 2, 0);
        let a = Entry::new(k, vec![1, 2, 3]);
        let b = Entry::new(k, vec![1, 2, 4]);
        assert!(a < b);
        assert_eq!(a.cmp(&a.clone()), Ordering::Equal);
    }

    #[test]
    fn total_order_is_reproducible() {
        let mut v = [
            Entry::new(key(10, 0, 0, 1, 0), vec![9]),
            Entry::new(key(10, 0, 0, 0, 0), vec![2]),
            Entry::new(key(10, 0, 0, 0, 0), vec![1]),
            Entry::new(key(9, 0, 0, 0, 0), vec![7]),
        ];
        v.sort();
        let order: Vec<_> = v.iter().map(|e| (e.key, e.value.clone())).collect();
        assert_eq!(
            order,
            vec![
                (key(9, 0, 0, 0, 0), vec![7]),
                (key(10, 0, 0, 0, 0), vec![1]),
                (key(10, 0, 0, 0, 0), vec![2]),
                (key(10, 0, 0, 1, 0), vec![9]),
            ]
        );
    }
}
