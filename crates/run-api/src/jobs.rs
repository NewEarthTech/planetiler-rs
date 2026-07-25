//! The job catalog: what a box can be asked to run, with the per-job launch
//! defaults `services/cloud-runner/run.sh` established (region, box size,
//! failsafe hours). v1 dispatch is single-ref only — the A/B jobs compare a
//! candidate branch whose image CI has not built (#59's scoping note), so
//! they stay on the laptop path until image.yml grows branch builds.

/// The smoke input: small enough that the whole planet pipeline — download,
/// build, upstream comparison, tile delta, results ship — completes in a few
/// minutes on the cheapest NVMe box, so the console loop can be proven end to
/// end for cents before anyone commits to a planet run. `-latest` rather than
/// a pin on purpose: this job proves the machinery, it records no numbers.
const SMOKE_INPUT_URL: &str = "https://download.geofabrik.de/europe/monaco-latest.osm.pbf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Planet,
    /// The planet job's pipeline on a monaco-sized input: an end-to-end
    /// rehearsal of every console leg, not a benchmark.
    PlanetSmoke,
    GbAb,
    EuropeAb,
}

impl Job {
    pub fn parse(name: &str) -> Option<Job> {
        match name {
            "planet" => Some(Job::Planet),
            "planet-smoke" => Some(Job::PlanetSmoke),
            "gb-ab" => Some(Job::GbAb),
            "europe-ab" => Some(Job::EuropeAb),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Job::Planet => "planet",
            Job::PlanetSmoke => "planet-smoke",
            Job::GbAb => "gb-ab",
            Job::EuropeAb => "europe-ab",
        }
    }

    /// planet launches where the OSM planet mirror lives, so the 94 GB input
    /// fetch is in-region (free and fast); everything else runs in us-east-1
    /// next to the log group.
    pub fn region(self) -> &'static str {
        match self {
            Job::Planet => "us-west-2",
            _ => "us-east-1",
        }
    }

    pub fn default_instance_type(self) -> &'static str {
        match self {
            Job::Planet => "r7gd.8xlarge",
            // The smallest NVMe Graviton box: the smoke run is about the
            // legs, not the numbers, and it still needs an instance store
            // for the /data mount the requisition user-data formats.
            Job::PlanetSmoke => "r7gd.large",
            _ => "r7gd.4xlarge",
        }
    }

    /// Boot failsafe (`shutdown -h`), so a wedged run can't idle for a
    /// weekend; shutdown behavior is terminate.
    pub fn default_max_hours(self) -> u32 {
        match self {
            Job::Planet => 14,
            Job::PlanetSmoke => 1,
            _ => 4,
        }
    }

    /// Whether the console can run this job today. A/B jobs stay on the
    /// laptop path: their dispatch wiring (a two-binary prebuilt mode for
    /// remote.sh and its dispatch script) doesn't exist yet — image.yml
    /// builds their candidate `:sha` images since #82.
    pub fn dispatchable(self) -> bool {
        matches!(self, Job::Planet | Job::PlanetSmoke)
    }

    /// The input this job runs when the dispatch names none. None = the
    /// deployment's pinned planet (the reproducibility pin behind every
    /// number in bench/results/).
    pub fn default_input_url(self) -> Option<&'static str> {
        match self {
            Job::PlanetSmoke => Some(SMOKE_INPUT_URL),
            _ => None,
        }
    }
}
