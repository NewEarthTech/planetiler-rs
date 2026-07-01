//! Binary entry point. A thin wrapper that delegates to [`planetiler_rs::cli`].

fn main() -> anyhow::Result<()> {
    planetiler_rs::cli::run()
}
