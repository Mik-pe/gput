use anyhow::Result;
use clap::Parser;
use gput::{
    builtin_router,
    config::{Cli, ServerConfig},
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gput=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let config = ServerConfig::try_from(Cli::parse())?;
    gput::serve(config, builtin_router()).await
}
