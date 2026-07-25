//! Runtime configuration: the fixed names the estate already leans on (repo,
//! log group, instance profile) plus the two values that vary by deployment
//! (the ECR repo URI, which carries the account id, and the live backend's
//! base URL for the events links the API hands out).

use anyhow::{Context, bail};

pub struct Config {
    /// ECR repo URI without a tag, e.g. `<acct>.dkr.ecr.us-east-1.amazonaws.com/planetiler-rs-bench`.
    pub image_repo: String,
    /// Registry host (`docker login` target), parsed from `image_repo`.
    pub registry: String,
    /// Region the registry lives in, parsed from `image_repo`.
    pub repo_region: String,
    /// Base URL of runner-live; `{live_base}/runs/{run_id}/events` streams a run.
    pub live_base: String,
    pub drain_group: String,
    pub drain_region: String,
    /// Fixed-name instance profile from RunnerConsoleStack.
    pub instance_profile: String,
    /// Pinned planet PBF (same reproducibility rule as run.sh: bump deliberately).
    pub planet_url: String,
}

impl Config {
    pub fn new(
        image_repo: String,
        live_base: String,
        drain_group: String,
        drain_region: String,
        instance_profile: String,
        planet_url: String,
    ) -> anyhow::Result<Config> {
        let registry = image_repo
            .split('/')
            .next()
            .context("image repo URI has no host")?
            .to_string();
        // <account>.dkr.ecr.<region>.amazonaws.com — the region rides in the
        // host, so ECR auth needs no extra configuration.
        let mut host = registry.split('.');
        let repo_region = match (host.next(), host.next(), host.next(), host.next()) {
            (Some(_), Some("dkr"), Some("ecr"), Some(region)) => region.to_string(),
            _ => bail!("image repo {image_repo} is not an ECR repo URI"),
        };
        Ok(Config {
            image_repo,
            registry,
            repo_region,
            live_base: live_base.trim_end_matches('/').to_string(),
            drain_group,
            drain_region,
            instance_profile,
            planet_url,
        })
    }

    pub fn image_uri(&self, tag: &str) -> String {
        format!("{}:{tag}", self.image_repo)
    }

    pub fn events_url(&self, run_id: &str) -> String {
        format!("{}/runs/{run_id}/events", self.live_base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::new(
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/planetiler-rs-bench".into(),
            "https://live.runner.example/".into(),
            "/planetiler-rs/runs".into(),
            "us-east-1".into(),
            "planetiler-rs-bench-box".into(),
            "https://example.com/planet.osm.pbf".into(),
        )
        .unwrap()
    }

    #[test]
    fn parses_registry_and_region_from_the_repo_uri() {
        let config = config();
        assert_eq!(
            config.registry,
            "123456789012.dkr.ecr.us-east-1.amazonaws.com"
        );
        assert_eq!(config.repo_region, "us-east-1");
        assert_eq!(
            config.image_uri("main"),
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/planetiler-rs-bench:main"
        );
        assert_eq!(
            config.events_url("planet-20260724-120000"),
            "https://live.runner.example/runs/planet-20260724-120000/events"
        );
    }

    #[test]
    fn rejects_a_non_ecr_repo() {
        assert!(
            Config::new(
                "docker.io/library/debian".into(),
                "https://live".into(),
                "g".into(),
                "us-east-1".into(),
                "p".into(),
                "https://x".into(),
            )
            .is_err()
        );
    }
}
