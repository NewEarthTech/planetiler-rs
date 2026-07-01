//! Compact node-location store — a first `LongLongMap`.
//!
//! Maps OSM node id → `(lon, lat)` far more compactly than a `HashMap`: a single
//! sorted `Vec<(i64, u64)>` with the coordinate packed as two fixed-point
//! `i32`s at OSM's native 1e-7-degree precision — **16 bytes/node** vs a
//! `HashMap`'s ~48. Built by inserting during the OSM node pass, finalized
//! (sorted), then queried by binary search during the way pass. OSM PBF nodes
//! already arrive id-sorted, so `finish` is usually a no-op. An mmap-backed /
//! `sparsearray` variant for planet scale is future work.

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
}
