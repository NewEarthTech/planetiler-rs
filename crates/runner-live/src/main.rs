use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;

use runner_live::auth::{self, AuthArgs};
use runner_live::event::StreamKind;
use runner_live::hub::RunHub;
use runner_live::server::{AppState, router};
use runner_live::source::LogSource;
use runner_live::source::livetail::LiveTailSource;
use runner_live::source::replay::ReplaySource;

/// The runner console's consumer backend (#59): ingests run logs and serves
/// them as live SSE streams behind the console's Cognito auth. Caddy owns
/// TLS in front; this listens on localhost.
#[derive(Parser)]
struct Args {
    #[arg(long, env = "RUNNER_LIVE_LISTEN", default_value = "127.0.0.1:8080")]
    listen: String,
    #[arg(long, value_enum, default_value = "livetail")]
    source: SourceKind,
    /// ARN of the run log group (fixed name `/planetiler-rs/runs`; the ARN
    /// carries the account id, so it can't be a baked-in default).
    #[arg(long, env = "RUNNER_LIVE_LOG_GROUP_ARN")]
    log_group_arn: Option<String>,
    /// Region the log group lives in — us-east-1 with RunnerConsoleStack.
    #[arg(long, env = "RUNNER_LIVE_AWS_REGION", default_value = "us-east-1")]
    aws_region: String,
    /// Recorded log file to replay (`--source replay`).
    #[arg(long)]
    replay_file: Option<std::path::PathBuf>,
    #[arg(long, default_value = "replay")]
    replay_run: String,
    /// 0 = replay as fast as possible; ~20000 approximates a planet run.
    #[arg(long, default_value_t = 0)]
    replay_delay_ms: u64,
    /// Where the SPA's auth config is published; the backend reads the same
    /// file rather than carrying its own copy of the pool ids.
    #[arg(
        long,
        env = "RUNNER_LIVE_CONFIG_URL",
        default_value = "https://runner.planetiler.rs/config.json"
    )]
    config_url: String,
    #[arg(long, env = "RUNNER_LIVE_USER_POOL_ID")]
    user_pool_id: Option<String>,
    #[arg(long, env = "RUNNER_LIVE_CLIENT_ID")]
    client_id: Option<String>,
    #[arg(long, env = "RUNNER_LIVE_AUTH_REGION")]
    auth_region: Option<String>,
    /// Browser origins allowed by CORS: the console's origin plus the Vite
    /// dev server (mirrors the localhost callback on the Cognito client).
    #[arg(
        long,
        env = "RUNNER_LIVE_ALLOWED_ORIGINS",
        default_value = "https://runner.planetiler.rs,http://localhost:5173",
        value_delimiter = ','
    )]
    allowed_origins: Vec<String>,
    /// Serve without auth — local dev only.
    #[arg(long)]
    no_auth: bool,
    /// Ring-buffer cap per run, in lines (~150 B each).
    #[arg(long, default_value_t = 100_000)]
    buffer_lines: usize,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceKind {
    Livetail,
    Replay,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();

    let hub = RunHub::new(args.buffer_lines);
    let auth = auth::resolve(AuthArgs {
        no_auth: args.no_auth,
        user_pool_id: args.user_pool_id,
        client_id: args.client_id,
        auth_region: args.auth_region,
        config_url: args.config_url,
    })
    .await?;

    match args.source {
        SourceKind::Livetail => {
            let arn = args.log_group_arn.context(
                "--log-group-arn (or RUNNER_LIVE_LOG_GROUP_ARN) is required for --source livetail",
            )?;
            let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(aws_config::Region::new(args.aws_region.clone()))
                .load()
                .await;
            spawn_source(
                LiveTailSource {
                    client: aws_sdk_cloudwatchlogs::Client::new(&config),
                    log_group_arn: arn,
                },
                hub.clone(),
            );
        }
        SourceKind::Replay => {
            let path = args
                .replay_file
                .context("--replay-file is required for --source replay")?;
            spawn_source(
                ReplaySource {
                    path,
                    run: args.replay_run,
                    stream: StreamKind::Planetiler,
                    line_delay: Duration::from_millis(args.replay_delay_ms),
                },
                hub.clone(),
            );
        }
    }

    let allowed_origins = args
        .allowed_origins
        .iter()
        .map(|origin| origin.parse())
        .collect::<Result<Vec<_>, _>>()
        .context("--allowed-origins: not a valid header value")?;
    let state = AppState {
        hub,
        auth: Arc::new(auth),
        allowed_origins,
    };
    let listener = tokio::net::TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    tracing::info!(listen = %args.listen, "runner-live up");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

fn spawn_source<S: LogSource + Send + 'static>(source: S, hub: Arc<RunHub>) {
    tokio::spawn(async move {
        if let Err(err) = source.run(hub).await {
            tracing::error!(error = %format!("{err:#}"), "log source exited");
        }
    });
}
