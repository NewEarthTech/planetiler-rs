//! Map profiles.
//!
//! A [`Profile`] is the user-facing schema: it turns each [`SourceFeature`] into
//! zoom-ranged vector features via a [`FeatureCollector`]. Profiles work purely
//! in lon/lat world geometry and `model` types, with one deliberate exception:
//! [`Profile::post_process_layer`] mirrors upstream's
//! `postProcessLayerFeatures` and operates on a finished tile layer in tile
//! space (merging lines, dissolving polygons — see [`crate::merge`]).

mod collector;

use std::collections::BTreeMap;

pub use collector::{FeatureCollector, VectorFeature};

use crate::model::Value;
use crate::mvt;
use crate::source::SourceFeature;

/// A schema that maps source features to output vector features.
///
/// `Send + Sync` because a profile is shared across worker threads: relation
/// preprocessing runs on OSM decode workers and [`post_process_layer`] runs on
/// the parallel tile-encode workers. Profiles hold configuration, not
/// per-feature state, so this is the natural contract (upstream's `Profile` is
/// shared across threads the same way).
///
/// [`post_process_layer`]: Profile::post_process_layer
pub trait Profile: Send + Sync {
    /// Process one source feature, emitting any output features into `features`.
    fn process_feature(&self, source: &SourceFeature, features: &mut FeatureCollector);

    /// Profile name, used for archive metadata.
    fn name(&self) -> &str {
        "planetiler-rs"
    }

    /// Decide whether to index an OSM relation: return `Some(info)` to have
    /// that info attached (with each member's role) to every member way's
    /// [`SourceFeature`] — the analog of upstream's `preprocessOsmRelation`.
    /// Called for every relation during the OSM node pass, on decode workers.
    /// The default indexes nothing.
    fn preprocess_osm_relation(
        &self,
        relation_id: i64,
        tags: &BTreeMap<String, Value>,
    ) -> Option<BTreeMap<String, Value>> {
        let _ = (relation_id, tags);
        None
    }

    /// Post-process one tile layer's features just before encoding — the hook
    /// for [`crate::merge`] utilities (line merging, polygon dissolve, feature
    /// dropping). Runs once per non-empty layer per tile with the features in
    /// draw order; the default returns them unchanged. This is the one Profile
    /// method that sees tile-space geometry.
    fn post_process_layer(
        &self,
        layer: &str,
        zoom: u8,
        features: Vec<mvt::Feature>,
    ) -> Vec<mvt::Feature> {
        let _ = (layer, zoom);
        features
    }
}

/// A trivial profile that copies every source feature into a single layer,
/// carrying all of its tags. Useful for GeoJSON pass-through and testing.
#[derive(Debug, Clone)]
pub struct PassthroughProfile {
    layer: String,
}

impl PassthroughProfile {
    /// Create a passthrough profile emitting into `layer`.
    pub fn new(layer: impl Into<String>) -> Self {
        Self {
            layer: layer.into(),
        }
    }
}

impl Profile for PassthroughProfile {
    fn process_feature(&self, _source: &SourceFeature, features: &mut FeatureCollector) {
        features.add_all_tags(&self.layer);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use geo_types::{Geometry, Point};

    use super::*;
    use crate::model::Value;

    fn source() -> SourceFeature {
        SourceFeature {
            id: 1,
            geometry: Geometry::Point(Point::new(7.42, 43.73)),
            tags: BTreeMap::from([
                ("name".to_owned(), Value::from("Casino")),
                ("rank".to_owned(), Value::Int(1)),
            ]),
            relations: Vec::new(),
        }
    }

    #[test]
    fn passthrough_copies_geometry_and_tags() {
        let src = source();
        let mut collector = FeatureCollector::new(&src, 0, 14);
        PassthroughProfile::new("poi").process_feature(&src, &mut collector);

        let features = collector.into_features();
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].layer, "poi");
        assert_eq!(features[0].attrs.get("name"), Some(&Value::from("Casino")));
        assert_eq!((features[0].min_zoom, features[0].max_zoom), (0, 14));
    }

    #[test]
    fn custom_profile_can_configure_features() {
        struct Roads;
        impl Profile for Roads {
            fn process_feature(&self, _s: &SourceFeature, f: &mut FeatureCollector) {
                f.add("road")
                    .attr("kind", "primary")
                    .set_zoom_range(8, 14)
                    .set_zorder(5);
            }
        }

        let src = source();
        let mut collector = FeatureCollector::new(&src, 0, 14);
        Roads.process_feature(&src, &mut collector);
        let features = collector.into_features();

        assert_eq!(features[0].layer, "road");
        assert_eq!(features[0].attrs.get("kind"), Some(&Value::from("primary")));
        assert_eq!((features[0].min_zoom, features[0].max_zoom), (8, 14));
        assert_eq!(features[0].zorder, 5);
        assert!(
            !features[0].attrs.contains_key("name"),
            "add() does not copy source tags"
        );
    }
}
