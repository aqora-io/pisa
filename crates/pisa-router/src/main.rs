use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(about = "Pisa router: queues Lean jobs for the workers")]
struct Args {
    #[command(flatten)]
    router: pisa_router::RouterConfig,
    /// The database that holds the job queue, shared with the workers:
    /// PostgreSQL (`postgres://…`), or a SQLite file for a single machine.
    #[arg(long, env = "PISA_DATABASE_URL")]
    database_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();
    let db = pisa_db::db::Db::connect(Some(&args.database_url)).await?;
    pisa_router::start(args.router, db).await?;
    tokio::signal::ctrl_c().await?;
    Ok(())
}
