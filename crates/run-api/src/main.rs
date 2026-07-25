use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use run_api::cloud::AwsCloud;
use run_api::config::Config;
use run_api::server::{AppState, router};

/// Pinned rather than `planet-latest`, same reproducibility rule as run.sh:
/// a benchmark input has to be reproducible. Bump deliberately, re-record.
const PLANET_URL: &str = "https://osm-planet-us-west-2.s3.dualstack.us-west-2.amazonaws.com/planet/pbf/2026/planet-260713.osm.pbf";

/// The runner console's dispatch API (#59): requisition, dispatch, cancel,
/// destroy, and list bench boxes. On Lambda the API Gateway JWT authorizer
/// is the auth layer; run locally it listens with NO auth — dev only.
#[derive(Parser)]
struct Args {
    #[arg(long, env = "RUN_API_LISTEN", default_value = "127.0.0.1:8081")]
    listen: String,
    /// ECR repo URI the boxes pull, e.g.
    /// `<acct>.dkr.ecr.us-east-1.amazonaws.com/planetiler-rs-bench` (carries
    /// the account id, so it can't be a baked-in default).
    #[arg(long, env = "RUN_API_IMAGE_REPO")]
    image_repo: String,
    /// runner-live's base URL, for the events links the API hands out.
    #[arg(
        long,
        env = "RUN_API_LIVE_BASE",
        default_value = "https://live.runner.planetiler.rs"
    )]
    live_base: String,
    #[arg(
        long,
        env = "RUN_API_DRAIN_GROUP",
        default_value = "/planetiler-rs/runs"
    )]
    drain_group: String,
    #[arg(long, env = "RUN_API_DRAIN_REGION", default_value = "us-east-1")]
    drain_region: String,
    /// Fixed-name instance profile from RunnerConsoleStack.
    #[arg(
        long,
        env = "RUN_API_INSTANCE_PROFILE",
        default_value = "planetiler-rs-bench-box"
    )]
    instance_profile: String,
    #[arg(long, env = "RUN_API_PLANET_URL", default_value = PLANET_URL)]
    planet_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_ansi(false)
        .init();
    let args = Args::parse();
    let config = Config::new(
        args.image_repo,
        args.live_base,
        args.drain_group,
        args.drain_region,
        args.instance_profile,
        args.planet_url,
    )?;
    let state = AppState {
        cloud: Arc::new(AwsCloud::new().await),
        config: Arc::new(config),
    };
    let app = router(state);

    // One binary, two harnesses: under the Lambda runtime the events come
    // from API Gateway (already JWT-vetted); anywhere else it's a plain
    // local listener for poking at with curl.
    if std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok() {
        lambda_http::run(app)
            .await
            .map_err(|err| anyhow::anyhow!("lambda runtime: {err}"))
    } else {
        let listener = tokio::net::TcpListener::bind(&args.listen)
            .await
            .with_context(|| format!("binding {}", args.listen))?;
        tracing::info!(listen = %args.listen, "run-api up (local, NO auth — dev only)");
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    }
}
