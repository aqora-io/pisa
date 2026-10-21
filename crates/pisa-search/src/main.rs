use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use pisa_protocol::search::Informal;
use pisa_search::anthropic::Client;
use pisa_search::corpus::{self, Corpus};
use pisa_search::dense::{self, Tei};
use pisa_search::engine::{Engine, Lanes};
use pisa_search::index::{self, Bundle};
use pisa_search::informal_store::Store;
use pisa_search::informalize;
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
    /// Store LeanSearch v2's Mathlib descriptions for matching declarations.
    ImportLsv2 {
        #[arg(long)]
        corpus: PathBuf,
        /// Where the LeanSearch v2 JSONL is (downloaded and verified if absent).
        #[arg(long)]
        file: PathBuf,
        /// Description and vector store (a directory or s3:// URL).
        #[arg(long)]
        informal: String,
    },
    /// Embed document views through a TEI server, caching the vectors.
    Embed {
        #[arg(long)]
        corpus: PathBuf,
        /// Description and vector store (a directory or s3:// URL).
        #[arg(long)]
        informal: String,
        #[arg(long, env = "PISA_SEARCH_TEI", default_value = "http://localhost:8090")]
        tei: String,
        /// Libraries to embed.
        #[arg(long, value_delimiter = ',', default_value = "Physlib")]
        libraries: Vec<String>,
        /// Also embed what those declarations use directly.
        #[arg(long)]
        with_deps: bool,
        /// Embed at most this many (for measuring throughput).
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 16)]
        batch: usize,
    },
    /// Describe declarations in natural language with Claude, in dependency
    /// order, caching the descriptions. Needs ANTHROPIC_API_KEY.
    Informalize {
        #[arg(long)]
        corpus: PathBuf,
        /// Description and vector store (a directory or s3:// URL).
        #[arg(long)]
        informal: String,
        #[arg(long, value_delimiter = ',', default_value = "Physlib")]
        libraries: Vec<String>,
        /// Describe a pilot sample of about this many declarations, whole
        /// modules stratified by area, instead of all of them.
        #[arg(long)]
        sample: Option<usize>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, default_value = "claude-haiku-5-5")]
        model: String,
        #[arg(long, default_value = "low")]
        effort: String,
        #[arg(long, default_value_t = 4096)]
        max_tokens: u32,
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
        /// Describe again even if a description is stored.
        #[arg(long)]
        force: bool,
        /// Write a report (cost, checks, samples) to this file.
        #[arg(long)]
        report: Option<PathBuf>,
        /// A bundle with vectors, for the report's discrimination test.
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// Print the plan and the first prompts instead of calling the API.
        #[arg(long)]
        dry_run: bool,
    },
    /// Build an index bundle from a corpus and stored descriptions.
    Build {
        #[arg(long)]
        corpus: PathBuf,
        /// Description and vector store (a directory or s3:// URL), if any.
        #[arg(long)]
        informal: Option<String>,
        /// Include cached document vectors of this embedding model.
        #[arg(long)]
        embedding_model: Option<String>,
        #[arg(long)]
        out: PathBuf,
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
        /// TEI server for embedding queries (enables the dense lane).
        #[arg(long, env = "PISA_SEARCH_TEI")]
        tei: Option<String>,
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
        /// TEI server for embedding queries (enables the dense lane).
        #[arg(long, env = "PISA_SEARCH_TEI")]
        tei: Option<String>,
        /// Write the report here as well as to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run one search against a bundle and print the hits.
    Query {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long, env = "PISA_SEARCH_TEI")]
        tei: Option<String>,
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Free text: natural language, LaTeX, Lean or a name.
        text: String,
    },
}

fn load_engine(
    bundle: &Path,
    symbols_path: Option<&Path>,
    tei: Option<&str>,
) -> anyhow::Result<Engine> {
    let symbols = match symbols_path {
        Some(p) => Symbols::parse(&std::fs::read_to_string(p)?)?,
        None => Symbols::parse(symbols::DEFAULT)?,
    };
    let mut engine = Engine::new(Bundle::open(bundle)?, symbols)?;
    if let (Some(d), Some(url)) = (engine.dense.take(), tei) {
        engine.dense = Some(d.with_tei(Tei::new(url)));
    }
    Ok(engine)
}

/// Stored descriptions for a corpus, by declaration name.
async fn descriptions(c: &Corpus, store: &Store) -> anyhow::Result<HashMap<String, Informal>> {
    let all = store.load_all().await?;
    Ok(c.decls
        .iter()
        .filter_map(|d| {
            all.get(&d.content_hash)
                .map(|e| (d.name.clone(), e.informal.clone()))
        })
        .collect())
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
            let exports: Vec<&Path> = export.iter().map(PathBuf::as_path).collect();
            let c = corpus::ingest(&exports, env_id, Some(&manifest))?;
            c.write(&out)?;
            println!("{}", serde_json::to_string_pretty(&c.manifest)?);
            if let Some(prev) = previous {
                let old = Corpus::read(&prev)?;
                println!("{:?}", corpus::diff(&old.decls, &c.decls));
            }
        }
        Command::ImportLsv2 {
            corpus,
            file,
            informal,
        } => {
            pisa_search::lsv2::fetch(&file).await?;
            let c = Corpus::read(&corpus)?;
            let (entries, report) = pisa_search::lsv2::import(&file, &c.decls)?;
            let store = Store::open(&informal)?;
            // Sorts before any later run's batches, so those win.
            store
                .write_batch(&format!("0000-{}", pisa_search::lsv2::SOURCE), &entries)
                .await?;
            println!("{report:#?}");
        }
        Command::Embed {
            corpus,
            informal,
            tei,
            libraries,
            with_deps,
            limit,
            batch,
        } => {
            let c = Corpus::read(&corpus)?;
            let store = Store::open(&informal)?;
            let described = descriptions(&c, &store).await?;
            let tei = Tei::new(&tei);
            let probe = tei.clone();
            let model = tokio::task::spawn_blocking(move || probe.model()).await??;
            let mut docs: Vec<String> = dense::select(&c.decls, &libraries, with_deps)
                .into_iter()
                .map(|d| dense::document_view(d, described.get(&d.name)))
                .collect();
            if let Some(n) = limit {
                docs.truncate(n);
            }
            eprintln!("{} declarations selected; model {model}", docs.len());
            let started = std::time::Instant::now();
            let n = dense::embed_missing(&store, &tei, &model, docs, batch, 1000).await?;
            eprintln!("embedded {n} in {:.0}s", started.elapsed().as_secs_f64());
        }
        Command::Informalize {
            corpus,
            informal,
            libraries,
            sample,
            seed,
            model,
            effort,
            max_tokens,
            concurrency,
            force,
            report,
            bundle,
            dry_run,
        } => {
            let c = Corpus::read(&corpus)?;
            let store = Store::open(&informal)?;
            let stored = store.load_all().await?;
            let described = descriptions(&c, &store).await?;
            let undescribed = informalize::undescribed(&c, &stored);
            let mut targets = match sample {
                Some(n) => informalize::pilot_sample(&c.decls, &libraries, n, seed),
                None => c
                    .decls
                    .iter()
                    .filter(|d| {
                        libraries.contains(&d.library)
                            && (d.informal || informalize::needs_description(d))
                    })
                    .collect(),
            };
            let modules: std::collections::HashSet<&str> =
                targets.iter().map(|d| d.module.as_str()).collect();
            let sample_text = match sample {
                Some(n) => format!(
                    "{} declarations from {} {} modules (about {n} requested, whole modules stratified by area, seed {seed})",
                    targets.len(),
                    modules.len(),
                    libraries.join("/")
                ),
                None => format!("all of {}", libraries.join(", ")),
            };
            if !force {
                targets.retain(|d| !stored.contains_key(&d.content_hash));
            }
            let docstring_entries = targets.iter().filter(|d| d.informal).count();
            eprintln!(
                "{} declarations to describe ({docstring_entries} from docstrings)",
                targets.len()
            );
            let cfg = informalize::Config {
                model,
                effort,
                max_tokens,
                concurrency,
            };
            if dry_run {
                let llm: Vec<_> = targets
                    .iter()
                    .copied()
                    .filter(|d| informalize::needs_description(d))
                    .collect();
                let ctx = informalize::Context::new(&c, described);
                eprintln!("{} levels; {sample_text}", informalize::levels(&llm).len());
                println!(
                    "{}",
                    serde_json::to_string_pretty(&informalize::system(&[
                        informalize::GLOSSARY_MATHLIB,
                        informalize::GLOSSARY_PHYSLIB
                    ])?)?
                );
                for d in llm.iter().take(3) {
                    println!("---\n{}", ctx.prompt(d));
                }
                return Ok(());
            }
            let client = Client::from_env()?;
            let started = std::time::Instant::now();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
            let batch = format!("{}-{}", now.as_millis(), cfg.model);
            let outcomes =
                informalize::run(&client, &cfg, &store, &c, &targets, described, &batch).await?;
            let elapsed_secs = started.elapsed().as_secs_f64();
            let cost: f64 = outcomes.iter().filter_map(|o| o.cost).sum();
            let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
            eprintln!(
                "described {} of {} in {elapsed_secs:.0}s for ${cost:.3}",
                outcomes.len() - failed,
                outcomes.len()
            );
            if let Some(path) = report {
                let discrimination = match bundle {
                    Some(b) => {
                        let bundle = Bundle::open(&b)?;
                        let mut entries: Vec<_> =
                            outcomes.iter().filter_map(|o| o.entry.as_ref()).collect();
                        entries.sort_by_key(|e| informalize::rank(seed + 1, &e.name));
                        entries.truncate(20);
                        Some(
                            informalize::discriminate(&client, &cfg, &bundle, &entries, seed)
                                .await?,
                        )
                    }
                    None => None,
                };
                let input = informalize::ReportInput {
                    date: path
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    env_id: &c.manifest.env_id,
                    cfg: &cfg,
                    sample: sample_text,
                    docstring_entries,
                    elapsed_secs,
                    undescribed,
                    discrimination,
                    seed,
                };
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, informalize::report(&input, &outcomes, &c))?;
                eprintln!("wrote {}", path.display());
            }
        }
        Command::Build {
            corpus,
            informal,
            embedding_model,
            out,
        } => {
            let c = Corpus::read(&corpus)?;
            let store = informal.as_deref().map(Store::open).transpose()?;
            let described = match &store {
                Some(store) => descriptions(&c, store).await?,
                None => HashMap::new(),
            };
            let vectors = match (&store, &embedding_model) {
                (Some(store), Some(model)) => {
                    let cache = dense::load_cache(store, model).await?;
                    Some(dense::vectors_by_name(&c.decls, &described, model, &cache))
                }
                (None, Some(_)) => anyhow::bail!("--embedding-model needs --informal"),
                _ => None,
            };
            let started = std::time::Instant::now();
            let m = tokio::task::spawn_blocking(move || {
                let v = embedding_model.as_deref().zip(vectors.as_ref());
                index::build(&c, &described, v, &out)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&m)?);
            eprintln!("built in {:.1}s", started.elapsed().as_secs_f64());
        }
        Command::Serve {
            bundle,
            http,
            symbols,
            tei,
        } => {
            let engine = Arc::new(load_engine(&bundle, symbols.as_deref(), tei.as_deref())?);
            tracing::info!(
                env_id = engine.env_id(),
                decls = engine.bundle.decls.len(),
                vectors = engine.dense.as_ref().map_or(0, |d| d.len()),
                %http,
                "serving"
            );
            let listener = tokio::net::TcpListener::bind(http).await?;
            axum::serve(listener, pisa_search::server::router(engine)).await?;
        }
        Command::Eval {
            bundle,
            data,
            weights,
            fused_only,
            tei,
            out,
        } => {
            let mut engine = load_engine(&bundle, None, tei.as_deref())?;
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
            let engine = Arc::new(engine);
            let report = tokio::task::spawn_blocking(move || {
                let r = pisa_search::eval::run(&engine, &items, &configs);
                r.markdown(engine.env_id())
            })
            .await?;
            print!("{report}");
            if let Some(out) = out {
                std::fs::write(out, report)?;
            }
        }
        Command::Query {
            bundle,
            tei,
            k,
            text,
        } => {
            let engine = load_engine(&bundle, None, tei.as_deref())?;
            let q = pisa_protocol::search::Query {
                q: Some(text),
                k,
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let r = tokio::task::spawn_blocking(move || engine.search(&q)).await?;
            for hit in &r.hits {
                println!("{}  [{}]\n", hit.text, hit.lanes.join(","));
            }
            eprintln!("{:.1} ms", started.elapsed().as_secs_f64() * 1e3);
        }
    }
    Ok(())
}
