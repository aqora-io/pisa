//! Compares Lean Beam with the Lean REPL for pisa's interactive work: goals
//! at positions, tactic steps, candidate attempts and session starts, both
//! run by the worker's own launcher (sandbox, environment, cgroups). Results
//! go to `--out` as JSON; `bench/beam/` holds the runs and their report.
//!
//! Run it in a fresh `pisa-worker` container (never beside a live worker: the
//! launcher clears its snapshot cache and joins the cgroup root).

mod env;
mod lean;
mod phases;
mod probe;
mod stats;
mod workload;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use pisa_beam::Packages;
use pisa_worker::config::WorkerConfig;
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(about = "Lean Beam against the REPL, inside the worker's sandbox")]
struct Cli {
    #[command(flatten)]
    worker: WorkerConfig,
    /// Directory for the JSON results.
    #[arg(long)]
    out: PathBuf,
    /// `lean-beam-mcp` (default: next to the toolchain's `lean`).
    #[arg(long)]
    beam: Option<PathBuf>,
    /// How workspaces reach the project's packages.
    #[arg(long, value_enum, default_value_t = PackagesArg::Symlink)]
    packages: PackagesArg,
    /// Modules for the workload (default: the infotree bench's eight).
    #[arg(long, value_delimiter = ',')]
    modules: Vec<String>,
    /// Blocks per module whose candidates are tried.
    #[arg(long, default_value_t = 4)]
    candidate_blocks: usize,
    /// Blocks per module checked whole (T2), with two candidates each.
    #[arg(long, default_value_t = 2)]
    t2_blocks: usize,
    /// Probes in flight, for the concurrency phase.
    #[arg(long, value_delimiter = ',', default_values_t = [1, 4, 16, 32])]
    levels: Vec<usize>,
    #[command(subcommand)]
    command: Phase,
}

#[derive(Clone, Copy, ValueEnum)]
enum PackagesArg {
    Symlink,
    Absolute,
}

#[derive(Clone, Copy, Subcommand)]
enum Phase {
    /// Whether and how Beam runs in the sandbox (probe.json).
    Probe,
    /// Build the workload from the modules (workload.json).
    Workload,
    Goals,
    Replay,
    Candidates,
    Concurrency,
    Session,
    /// The workload if missing, then every measured phase.
    All,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.out)?;
    let beam = match &cli.beam {
        Some(b) => b.clone(),
        None => beam_next_to(&cli.worker.lean)?,
    };
    let packages = match cli.packages {
        PackagesArg::Symlink => Packages::Symlink,
        PackagesArg::Absolute => Packages::Absolute,
    };
    let env = env::Env::new(&cli.worker, beam, packages).await?;
    let modules: Vec<String> = if cli.modules.is_empty() {
        workload::MODULES.iter().map(|m| m.to_string()).collect()
    } else {
        cli.modules.clone()
    };
    let workload_path = cli.out.join("workload.json");
    let load = || -> Result<workload::Workload> {
        let text = std::fs::read_to_string(&workload_path).with_context(|| {
            format!("reading {}; run `workload` first", workload_path.display())
        })?;
        Ok(serde_json::from_str(&text)?)
    };
    match cli.command {
        Phase::Probe => write(&cli, &env, "probe", probe::run(&env).await?)?,
        Phase::Workload => {
            let w = workload::generate(&env, &modules).await?;
            std::fs::write(&workload_path, serde_json::to_string_pretty(&w)?)?;
        }
        Phase::Goals => write(&cli, &env, "goals", phases::goals(&env, &load()?).await?)?,
        Phase::Replay => write(&cli, &env, "replay", phases::replay(&env, &load()?).await?)?,
        Phase::Candidates => {
            let r = phases::candidates(&env, &load()?, cli.candidate_blocks, cli.t2_blocks).await?;
            write(&cli, &env, "candidates", r)?
        }
        Phase::Concurrency => {
            let r = phases::concurrency(&env, &load()?, cli.candidate_blocks, &cli.levels).await?;
            write(&cli, &env, "concurrency", r)?
        }
        Phase::Session => write(
            &cli,
            &env,
            "session",
            phases::session(&env, &load()?).await?,
        )?,
        Phase::All => {
            if !workload_path.is_file() {
                let w = workload::generate(&env, &modules).await?;
                std::fs::write(&workload_path, serde_json::to_string_pretty(&w)?)?;
            }
            let w = load()?;
            write(&cli, &env, "session", phases::session(&env, &w).await?)?;
            write(&cli, &env, "goals", phases::goals(&env, &w).await?)?;
            write(&cli, &env, "replay", phases::replay(&env, &w).await?)?;
            let r = phases::candidates(&env, &w, cli.candidate_blocks, cli.t2_blocks).await?;
            write(&cli, &env, "candidates", r)?;
            let r = phases::concurrency(&env, &w, cli.candidate_blocks, &cli.levels).await?;
            write(&cli, &env, "concurrency", r)?;
        }
    }
    Ok(())
}

/// `lean-beam-mcp` in the toolchain of `lean`.
fn beam_next_to(lean: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new(lean)
        .arg("--print-prefix")
        .output()
        .context("running lean --print-prefix")?;
    let prefix = String::from_utf8(out.stdout)?;
    Ok(Path::new(prefix.trim()).join("bin/lean-beam-mcp"))
}

fn write(cli: &Cli, env: &env::Env, name: &str, result: Value) -> Result<()> {
    let meta = json!({
        "version": env.version,
        "toolchain": env.toolchain,
        "packages": format!("{:?}", env.packages),
        "sandbox": format!("{:?}", cli.worker.sandbox),
        "cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "unix_time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
    });
    let path = cli.out.join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({"meta": meta, "result": result}))?,
    )?;
    tracing::info!("wrote {}", path.display());
    Ok(())
}
