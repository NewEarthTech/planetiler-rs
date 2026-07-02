//! Route-overlay example, mirroring upstream's `BikeRouteOverlay`: index
//! `type=route` relations with the relation preprocessor, then emit each
//! member way once per containing route with the route's tags attached.
//!
//! Usage: `cargo run --release --example bike_routes -- <in.osm.pbf> <out.mbtiles> [route-type]`
//! (`route-type` defaults to `bicycle`; try `bus` for city extracts.)

use std::collections::BTreeMap;

use planetiler_rs::error::Result;
use planetiler_rs::model::Value;
use planetiler_rs::pipeline::{GenerateOptions, generate_streaming};
use planetiler_rs::profile::{FeatureCollector, Profile};
use planetiler_rs::source::{NodeStorage, SourceFeature, stream_source};

struct RouteOverlay {
    route_type: String,
}

impl Profile for RouteOverlay {
    fn name(&self) -> &str {
        "route-overlay"
    }

    fn preprocess_osm_relation(
        &self,
        _relation_id: i64,
        tags: &BTreeMap<String, Value>,
    ) -> Option<BTreeMap<String, Value>> {
        let is_route = tags.get("type").and_then(Value::as_str) == Some("route");
        let matches = tags.get("route").and_then(Value::as_str) == Some(self.route_type.as_str());
        (is_route && matches).then(|| {
            let mut info = BTreeMap::new();
            for key in ["name", "ref", "network", "route"] {
                if let Some(value) = tags.get(key) {
                    info.insert(key.to_owned(), value.clone());
                }
            }
            info
        })
    }

    fn process_feature(&self, source: &SourceFeature, features: &mut FeatureCollector) {
        // Emit each member way once per containing route, carrying the
        // route's tags — the overlay is the routes, not the raw ways.
        for membership in &source.relations {
            let feature = features.add(&format!("{}-route", self.route_type));
            for (key, value) in membership.info.iter() {
                feature.attr(key, value.clone());
            }
            feature.attr("role", Value::from(membership.role.as_str()));
        }
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .expect("usage: bike_routes <in.osm.pbf> <out.mbtiles> [route-type]");
    let output = args
        .next()
        .expect("usage: bike_routes <in.osm.pbf> <out.mbtiles> [route-type]");
    let route_type = args.next().unwrap_or_else(|| "bicycle".to_owned());

    let profile = RouteOverlay { route_type };
    let options = GenerateOptions {
        min_zoom: 0,
        max_zoom: 14,
        ..Default::default()
    };
    let stats = generate_streaming(
        |sink| {
            stream_source(
                source.as_ref(),
                sink,
                NodeStorage::Ram,
                Some(&|id, tags: &_| profile.preprocess_osm_relation(id, tags)),
            )
        },
        &profile,
        &options,
        &output,
    )?;
    println!(
        "Wrote {output}: {} tiles from {} source features.",
        stats.tiles, stats.features
    );
    Ok(())
}
