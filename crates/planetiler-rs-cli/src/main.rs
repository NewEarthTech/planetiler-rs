//! Binary entry point. A thin wrapper that delegates to [`planetiler_rs::cli`].

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    planetiler_rs::cli::run()
}
