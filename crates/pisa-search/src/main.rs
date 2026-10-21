use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use pisa_search::corpus::{self, Corpus};
use pisa_search::engine::Engine;
use pisa_search::index::{self, Bundle};
use pisa_search::symbols::{self, Symbols};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(about = "Declaration search over Mathlib and Physlib")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Turn exporter output into a corpus with content hashes.
    Ingest {
        /// Output of search/exporter/PisaExport.lean and ToAdditive.lean
        /// (.ndjson or .ndjson.zst); repeat for several files.
        #[arg(long, required = true)]
        export: Vec<PathBuf>,
        /// The pinned Lake project; gives the env_id and source links.
        #[arg(long, default_value = "docker/lean-project")]
        project: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Report what changed against an earlier corpus.
        #[arg(long)]
        previous: Option<PathBuf>,
    },
    /// Build an index bundle from a corpus and stored descriptions.
    Build {
        #[arg(long)]
        corpus: PathBuf,
        /// Description store (a directory or s3:// URL), if any.
        #[arg(long)]
        informal: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Store LeanSearch v2's Mathlib descriptions for matching declarations.
    ImportLsv2 {
        #[arg(long)]
        corpus: PathBuf,
        /// Where the LeanSearch v2 JSONL is (downloaded and verified if absent).
        #[arg(long)]
        file: PathBuf,
        /// Description store (a directory or s3:// URL).
        #[arg(long)]
        informal: String,
    },
    /// Serve the HTTP API for a bundle.
    Serve {
        #[arg(long, env = "PISA_SEARCH_BUNDLE")]
        bundle: PathBuf,
        #[arg(long, env = "PISA_SEARCH_HTTP", default_value = "0.0.0.0:8100")]
        http: std::net::SocketAddr,
        /// Symbol dictionary; defaults to the shipped search/symbols.toml.
        #[arg(long, env = "PISA_SEARCH_SYMBOLS")]
        symbols: Option<PathBuf>,
    },
    /// Measure retrieval quality on MathlibQR.
    Eval {
        #[arg(long)]
        bundle: PathBuf,
        /// MathlibQR.json (downloaded from the pinned commit if absent).
        #[arg(long)]
        data: PathBuf,
        /// Weights to use instead of the defaults, as JSON (fields of `Weights`).
        #[arg(long)]
        weights: Option<String>,
        /// Only the fused result, not each lane alone.
        #[arg(long)]
        fused_only: bool,
        /// Write the report here as well as to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run one search against a bundle and print the hits.
    Query {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Free text: natural language, LaTeX, Lean or a name.
        text: String,
    },
}

fn load_engine(
    bundle: &std::path::Path,
    symbols_path: Option<&std::path::Path>,
) -> anyhow::Result<Engine> {
    let symbols = match symbols_path {
        Some(p) => Symbols::parse(&std::fs::read_to_string(p)?)?,
        None => Symbols::parse(symbols::DEFAULT)?,
    };
    Engine::new(Bundle::open(bundle)?, symbols)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().command {
        Command::Ingest {
            export,
            project,
            out,
            previous,
        } => {
            let env_id = pisa_protocol::version::project_version(&project)?;
            let manifest = project.join("lake-manifest.json");
            let exports: Vec<&std::path::Path> = export.iter().map(PathBuf::as_path).collect();
            let c = corpus::ingest(&exports, env_id, Some(&manifest))?;
            c.write(&out)?;
            println!("{}", serde_json::to_string_pretty(&c.manifest)?);
            if let Some(prev) = previous {
                let old = Corpus::read(&prev)?;
                println!("{:?}", corpus::diff(&old.decls, &c.decls));
            }
        }
        Command::Build {
            corpus,
            informal,
            out,
        } => {
            let c = Corpus::read(&corpus)?;
            let descriptions = match informal {
                Some(url) => {
                    let all = pisa_search::informal_store::Store::open(&url)?
                        .load_all()
                        .await?;
                    c.decls
                        .iter()
                        .filter_map(|d| {
                            all.get(&d.content_hash)
                                .map(|e| (d.name.clone(), e.informal.clone()))
                        })
                        .collect()
                }
                None => Default::default(),
            };
            let started = std::time::Instant::now();
            let m = tokio::task::spawn_blocking(move || index::build(&c, &descriptions, &out))
                .await??;
            println!("{}", serde_json::to_string_pretty(&m)?);
            eprintln!("built in {:.1}s", started.elapsed().as_secs_f64());
        }
        Command::ImportLsv2 {
            corpus,
            file,
            informal,
        } => {
            pisa_search::lsv2::fetch(&file).await?;
            let c = Corpus::read(&corpus)?;
            let (entries, report) = pisa_search::lsv2::import(&file, &c.decls)?;
            let store = pisa_search::informal_store::Store::open(&informal)?;
            // Sorts before any later run's batches, so those win.
            store
                .write_batch(&format!("0000-{}", pisa_search::lsv2::SOURCE), &entries)
                .await?;
            println!("{report:#?}");
        }
        Command::Serve {
            bundle,
            http,
            symbols,
        } => {
            let engine = Arc::new(load_engine(&bundle, symbols.as_deref())?);
            tracing::info!(env_id = engine.env_id(), decls = engine.bundle.decls.len(), %http, "serving");
            let listener = tokio::net::TcpListener::bind(http).await?;
            axum::serve(listener, pisa_search::server::router(engine)).await?;
        }
        Command::Eval {
            bundle,
            data,
            weights,
            fused_only,
            out,
        } => {
            use pisa_search::engine::Lanes;
            let mut engine = load_engine(&bundle, None)?;
            if let Some(w) = weights {
                engine.weights = serde_json::from_str(&w)?;
            }
            let items = pisa_search::eval::load(&data).await?;
            let none = Lanes {
                lexical: false,
                name: false,
                structural: false,
                dense: false,
            };
            let mut configs = vec![("fused", Lanes::ALL)];
            if !fused_only {
                configs.push((
                    "lexical",
                    Lanes {
                        lexical: true,
                        ..none
                    },
                ));
                configs.push((
                    "structural",
                    Lanes {
                        structural: true,
                        ..none
                    },
                ));
                configs.push(("name", Lanes { name: true, ..none }));
                if engine.dense.is_some() {
                    configs.push((
                        "dense",
                        Lanes {
                            dense: true,
                            ..none
                        },
                    ));
                }
            }
            let report = pisa_search::eval::run(&engine, &items, &configs);
            let md = report.markdown(engine.env_id());
            print!("{md}");
            if let Some(out) = out {
                std::fs::write(out, md)?;
            }
        }
        Command::Query { bundle, k, text } => {
            let engine = load_engine(&bundle, None)?;
            let q = pisa_protocol::search::Query {
                q: Some(text),
                k,
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let r = engine.search(&q);
            for hit in &r.hits {
                println!("{}  [{}]\n", hit.text, hit.lanes.join(","));
            }
            eprintln!("{:.1} ms", started.elapsed().as_secs_f64() * 1e3);
        }
    }
    Ok(())
}
