use std::time::Duration;

use clap::Parser;
use pisa_server::ServerConfig;
use tracing_subscriber::EnvFilter;

/// How long a server keeps taking requests after it is told to stop: its
/// Service stops sending it new ones a moment after the signal.
const STOP_GRACE: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cfg = ServerConfig::parse();
    let drain = Duration::from_secs(cfg.drain_timeout_secs);
    let server = pisa_server::start(cfg).await?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down: draining");
    tokio::time::sleep(STOP_GRACE).await;
    server.running.shutdown(drain).await;
    Ok(())
}
