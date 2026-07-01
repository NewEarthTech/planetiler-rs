//! Source readers.
//!
//! *(Planned — Phase 2+.)* Decode input formats (GeoJSON first, then the
//! two-pass OSM `.osm.pbf` reader, later shapefile/Natural Earth/geoparquet)
//! into a common `SourceFeature` carrying lon/lat geometry and tags.
