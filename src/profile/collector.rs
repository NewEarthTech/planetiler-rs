//! The feature collector a [`Profile`](super::Profile) emits into.

use std::collections::BTreeMap;

use geo_types::Geometry;

use crate::model::Value;
use crate::source::SourceFeature;

/// A vector feature emitted by a profile: geometry still in lon/lat, plus the
/// layer, zoom range, attributes, and z-order that the renderer needs. The
/// renderer (not the profile) projects this into tile space.
#[derive(Debug, Clone)]
pub struct VectorFeature {
    /// Target MVT layer name.
    pub layer: String,
    /// Geometry in lon/lat (WGS84).
    pub geometry: Geometry<f64>,
    /// Output attributes.
    pub attrs: BTreeMap<String, Value>,
    /// Lowest zoom to render at (inclusive).
    pub min_zoom: u8,
    /// Highest zoom to render at (inclusive).
    pub max_zoom: u8,
    /// Draw order within a tile+layer (encoded into the sort key).
    pub zorder: i32,
}

impl VectorFeature {
    fn new(layer: &str, geometry: Geometry<f64>, min_zoom: u8, max_zoom: u8) -> Self {
        Self {
            layer: layer.to_owned(),
            geometry,
            attrs: BTreeMap::new(),
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
    pub fn attr(&mut self, key: &str, value: impl Into<Value>) -> &mut Self {
        self.attrs.insert(key.to_owned(), value.into());
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
pub struct FeatureCollector<'a> {
    source: &'a SourceFeature,
    features: Vec<VectorFeature>,
    default_min_zoom: u8,
    default_max_zoom: u8,
}

impl<'a> FeatureCollector<'a> {
    /// Create a collector for one source feature with the default zoom range.
    pub fn new(source: &'a SourceFeature, default_min_zoom: u8, default_max_zoom: u8) -> Self {
        Self {
            source,
            features: Vec::new(),
            default_min_zoom,
            default_max_zoom,
        }
    }

    /// Emit a feature into `layer` reusing the source geometry, with no
    /// attributes. Returns a handle for further configuration.
    pub fn add(&mut self, layer: &str) -> &mut VectorFeature {
        self.features.push(VectorFeature::new(
            layer,
            self.source.geometry.clone(),
            self.default_min_zoom,
            self.default_max_zoom,
        ));
        self.features.last_mut().expect("just pushed")
    }

    /// Emit a feature into `layer` reusing the source geometry and copying all
    /// of the source's tags (the passthrough behaviour).
    pub fn add_all_tags(&mut self, layer: &str) -> &mut VectorFeature {
        let mut feature = VectorFeature::new(
            layer,
            self.source.geometry.clone(),
            self.default_min_zoom,
            self.default_max_zoom,
        );
        feature.attrs = self.source.tags.clone();
        self.features.push(feature);
        self.features.last_mut().expect("just pushed")
    }

    /// Consume the collector, yielding the emitted features.
    pub fn into_features(self) -> Vec<VectorFeature> {
        self.features
    }
}
