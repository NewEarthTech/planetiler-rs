//! The job catalog: what a box can be asked to run, with the per-job launch
//! defaults `services/cloud-runner/run.sh` established (region, box size,
//! failsafe hours). v1 dispatch is single-ref only — the A/B jobs compare a
//! candidate branch whose image CI has not built (#59's scoping note), so
//! they stay on the laptop path until image.yml grows branch builds.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Planet,
    GbAb,
    EuropeAb,
}

impl Job {
    pub fn parse(name: &str) -> Option<Job> {
        match name {
            "planet" => Some(Job::Planet),
            "gb-ab" => Some(Job::GbAb),
            "europe-ab" => Some(Job::EuropeAb),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Job::Planet => "planet",
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
            _ => "r7gd.4xlarge",
        }
    }

    /// Boot failsafe (`shutdown -h`), so a wedged run can't idle for a
    /// weekend; shutdown behavior is terminate.
    pub fn default_max_hours(self) -> u32 {
        match self {
            Job::Planet => 14,
            _ => 4,
        }
    }

    /// Whether the console can run this job today. A/B jobs need a candidate
    /// image that only branch builds would produce.
    pub fn dispatchable(self) -> bool {
        matches!(self, Job::Planet)
    }
}
