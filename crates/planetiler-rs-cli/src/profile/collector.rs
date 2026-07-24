//! The feature collector a [`Profile`](super::Profile) emits into.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use geo_types::Geometry;

use crate::model::Value;
use crate::source::SourceFeature;

/// One shared empty attribute map, so `add()` (which emits no source tags) costs
/// a refcount bump rather than an allocation. `attr()` clones it on first write
/// via `Arc::make_mut`, and cloning an empty `BTreeMap` allocates nothing.
static NO_ATTRS: LazyLock<Arc<BTreeMap<String, Value>>> =
    LazyLock::new(|| Arc::new(BTreeMap::new()));

/// A vector feature emitted by a profile: geometry still in lon/lat, plus the
/// layer, zoom range, attributes, and z-order that the renderer needs. The
/// renderer (not the profile) projects this into tile space.
#[derive(Debug, Clone)]
pub struct VectorFeature {
    /// Feature id (carried to the MVT feature); defaults to the source id.
    pub id: u64,
    /// Target MVT layer name.
    pub layer: String,
    /// Geometry in lon/lat (WGS84), shared with the [`SourceFeature`] it came
    /// from — emitting a feature is a refcount bump, not a coordinate copy.
    pub geometry: Arc<Geometry<f64>>,
    /// Output attributes, shared with the source feature's tags on the
    /// passthrough path. [`attr`](Self::attr) copies on write.
    pub attrs: Arc<BTreeMap<String, Value>>,
    /// Lowest zoom to render at (inclusive).
    pub min_zoom: u8,
    /// Highest zoom to render at (inclusive).
    pub max_zoom: u8,
    /// Draw order within a tile+layer (encoded into the sort key).
    pub zorder: i32,
}

impl VectorFeature {
    fn new(id: u64, layer: &str, geometry: Arc<Geometry<f64>>, min_zoom: u8, max_zoom: u8) -> Self {
        Self {
            id,
            layer: layer.to_owned(),
            geometry,
            attrs: Arc::clone(&NO_ATTRS),
            min_zoom,
            max_zoom,
            zorder: 0,
        }
    }

    /// Set the inclusive zoom range.
    pub fn set_zoom_range(&mut self, min_zoom: u8, max_zoom: u8) -> &mut Self {
        self.min_zoom = min_zoom;
        self.max_zoom = max_zoom;
        self
    }

    /// Set the minimum zoom.
    pub fn set_min_zoom(&mut self, min_zoom: u8) -> &mut Self {
        self.min_zoom = min_zoom;
        self
    }

    /// Set the maximum zoom.
    pub fn set_max_zoom(&mut self, max_zoom: u8) -> &mut Self {
        self.max_zoom = max_zoom;
        self
    }

    /// Set one output attribute.
    ///
    /// Copy-on-write: attributes are shared with the source feature's tags until
    /// something actually changes them, so a profile that only reads them never
    /// pays for a copy, and one that writes pays exactly once.
    pub fn attr(&mut self, key: &str, value: impl Into<Value>) -> &mut Self {
        Arc::make_mut(&mut self.attrs).insert(key.to_owned(), value.into());
        self
    }

    /// Set the z-order (draw order within a tile+layer).
    pub fn set_zorder(&mut self, zorder: i32) -> &mut Self {
        self.zorder = zorder;
        self
    }
}

/// Collects the [`VectorFeature`]s a profile emits for one source feature. Bound
/// to that source feature so `add` reuses its geometry.
///
/// Emitted features are appended straight to a caller-owned output `Vec` (the
/// pipeline's render batch) rather than to a per-source-feature `Vec` the caller
/// then drains. A collector is constructed and dropped once per source feature
/// on the serial consume path, so owning a `Vec` cost an allocate/free pair plus
/// a move of every emitted feature, per feature, for a buffer that almost always
/// held exactly one element. `first()` is where a source feature's own features
/// start in that shared buffer.
pub struct FeatureCollector<'s, 'o> {
    source: &'s SourceFeature,
    out: &'o mut Vec<VectorFeature>,
    first: usize,
    default_min_zoom: u8,
    default_max_zoom: u8,
}

impl<'s, 'o> FeatureCollector<'s, 'o> {
    /// Create a collector for one source feature with the default zoom range,
    /// appending everything it emits to `out`.
    pub fn new(
        source: &'s SourceFeature,
        out: &'o mut Vec<VectorFeature>,
        default_min_zoom: u8,
        default_max_zoom: u8,
    ) -> Self {
        let first = out.len();
        Self {
            source,
            out,
            first,
            default_min_zoom,
            default_max_zoom,
        }
    }

    /// Index in the output `Vec` where this source feature's emitted features
    /// begin — the caller's handle on just what this collector produced.
    pub fn first(&self) -> usize {
        self.first
    }

    /// Emit a feature into `layer` reusing the source geometry, with no
    /// attributes. Returns a handle for further configuration.
    pub fn add(&mut self, layer: &str) -> &mut VectorFeature {
        self.out.push(VectorFeature::new(
            self.source.id,
            layer,
            Arc::clone(&self.source.geometry),
            self.default_min_zoom,
            self.default_max_zoom,
        ));
        self.out.last_mut().expect("just pushed")
    }

    /// Emit a feature into `layer` reusing the source geometry and carrying all
    /// of the source's tags (the passthrough behaviour). The tags are shared,
    /// not copied; [`attr`](VectorFeature::attr) copies on write.
    pub fn add_all_tags(&mut self, layer: &str) -> &mut VectorFeature {
        let mut feature = VectorFeature::new(
            self.source.id,
            layer,
            Arc::clone(&self.source.geometry),
            self.default_min_zoom,
            self.default_max_zoom,
        );
        feature.attrs = Arc::clone(&self.source.tags);
        self.out.push(feature);
        self.out.last_mut().expect("just pushed")
    }
}
