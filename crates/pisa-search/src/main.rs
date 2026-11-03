use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use pisa_protocol::search::Informal;
use pisa_search::corpus::{self, Corpus};
use pisa_search::dense::{self, Embedder};
use pisa_search::engine::{self, Weights};
use pisa_search::index::{self, Bundle};
use pisa_search::indexer;
use pisa_search::informal_store::Store;
use pisa_search::informalize;
use pisa_search::llm::Llm;
use pisa_search::qdrant::Qdrant;
use pisa_search::symbols::{self, Symbols};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(about = "Declaration search over Mathlib and Physlib")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct QdrantArgs {
    /// Qdrant's REST URL.
    #[arg(long = "qdrant", env = "PISA_SEARCH_QDRANT")]
    url: String,
    /// The environment variable holding Qdrant's API key, if it needs one.
    #[arg(long = "qdrant-key-env", env = "PISA_SEARCH_QDRANT_KEY_ENV")]
    key_env: Option<String>,
}

impl QdrantArgs {
    fn client(&self) -> anyhow::Result<Qdrant> {
        let key = match &self.key_env {
            Some(var) => Some(std::env::var(var).map_err(|_| anyhow::anyhow!("{var} is not set"))?),
            None => None,
        };
        Qdrant::new(&self.url, key)
    }
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
        /// `ENV=HEADER`: declarations from exporter environment ENV are
        /// loaded by HEADER rather than the default imports, e.g.
        /// `physlib-quantuminfo=import Physlib; import QuantumInfo`.
        #[arg(long)]
        env_imports: Vec<String>,
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
    /// Embed document views through an embedding server, caching the vectors.
    Embed {
        #[arg(long)]
        corpus: PathBuf,
        /// Description and vector store (a directory or s3:// URL).
        #[arg(long)]
        informal: String,
        /// A TEI URL, or `openai+<base url>?model=<model>[&key_env=<VAR>]`.
        #[arg(
            long,
            alias = "tei",
            env = "PISA_SEARCH_TEI",
            default_value = "http://localhost:8090"
        )]
        embedder: String,
        /// Cache the vectors under this name instead of the model's, e.g. to
        /// keep one model's vectors from two backends apart.
        #[arg(long)]
        label: Option<String>,
        /// Embed exactly the declarations named in this file (one per line)
        /// instead of `--libraries`.
        #[arg(long)]
        names: Option<PathBuf>,
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
        /// Requests in flight at once (for remote endpoints).
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        /// Keep running alongside `informalize`: embed declarations as their
        /// descriptions arrive, skipping those still waiting for one, until
        /// this file exists; then embed everything left and stop.
        #[arg(long)]
        follow: Option<PathBuf>,
    },
    /// Describe declarations in natural language with Claude or an OpenAI
    /// model, in dependency order, caching the descriptions. Needs
    /// ANTHROPIC_API_KEY or OPENAI_API_KEY.
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
        /// Describe exactly the declarations named in this file (one per line).
        #[arg(long, conflicts_with = "sample")]
        names: Option<PathBuf>,
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
        /// Models that judge the discrimination test (default: `--model`).
        #[arg(long)]
        judge: Vec<String>,
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
        /// Include cached document vectors of this embedding model (or label).
        #[arg(long)]
        embedding_model: Option<String>,
        /// Include vectors only for the declarations named in this file.
        #[arg(long)]
        vector_names: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Load a bundle into Qdrant, as the declarations of its `env_id`.
    Load {
        #[arg(long)]
        bundle: PathBuf,
        #[command(flatten)]
        qdrant: QdrantArgs,
        /// Copies of each collection's shard.
        #[arg(long, default_value_t = 1)]
        replication: u32,
        /// Refuse a bundle whose vectors cover less than this share of its declarations.
        #[arg(long, default_value_t = 0.99)]
        min_coverage: f64,
    },
    /// Serve the HTTP API for the bundle loaded for an environment. Any
    /// number of replicas can serve one environment.
    Serve {
        #[command(flatten)]
        qdrant: QdrantArgs,
        /// The environment whose loaded bundle to serve.
        #[arg(long, env = "PISA_SEARCH_ENV_ID")]
        env_id: String,
        #[arg(long, env = "PISA_SEARCH_HTTP", default_value = "0.0.0.0:8100")]
        http: std::net::SocketAddr,
        /// Symbol dictionary; defaults to the shipped search/symbols.toml.
        #[arg(long, env = "PISA_SEARCH_SYMBOLS")]
        symbols: Option<PathBuf>,
        /// Embedding server for queries (enables the dense lane); see `embed`.
        /// It also embeds snapshot declarations, so it must serve the model
        /// that embedded the bundle.
        #[arg(long, alias = "tei", env = "PISA_SEARCH_TEI")]
        embedder: Option<String>,
        /// PD endpoints of the TiKV holding the workers' snapshot catalog,
        /// comma-separated; with them, queries can search snapshots, and the
        /// replica that holds the lease indexes them.
        #[arg(long, env = "PISA_TIKV_PD", value_delimiter = ',')]
        tikv_pd: Vec<String>,
        /// The worker version whose snapshots to search (default: the env_id).
        #[arg(long, env = "PISA_SEARCH_SNAPSHOT_VERSION")]
        snapshot_version: Option<String>,
        /// Claude or OpenAI model that describes every snapshot declaration
        /// (needs ANTHROPIC_API_KEY or OPENAI_API_KEY); without one they are
        /// searched by name, signature and docstring.
        #[arg(long, env = "PISA_SEARCH_DESCRIBE_MODEL")]
        describe_model: Option<String>,
        /// LLM and embedding requests in flight for snapshots.
        #[arg(long, env = "PISA_SEARCH_DESCRIBE_CONCURRENCY", default_value_t = 8)]
        describe_concurrency: usize,
    },
    /// Measure retrieval quality on MathlibQR.
    Eval {
        #[command(flatten)]
        qdrant: QdrantArgs,
        /// The environment whose loaded bundle to search.
        #[arg(long)]
        env_id: String,
        /// MathlibQR.json (downloaded from the pinned commit if absent).
        #[arg(long)]
        data: PathBuf,
        /// Weights to use instead of the defaults, as JSON (fields of
        /// `Weights`); several are evaluated in turn, embedding queries once.
        #[arg(long)]
        weights: Vec<String>,
        /// Only the fused result, not each lane alone.
        #[arg(long)]
        fused_only: bool,
        /// Embedding server for queries (enables the dense lane); see `embed`.
        #[arg(long, alias = "tei", env = "PISA_SEARCH_TEI")]
        embedder: Option<String>,
        /// Write the report here as well as to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run one search against a loaded bundle and print the hits.
    Query {
        #[command(flatten)]
        qdrant: QdrantArgs,
        #[arg(long)]
        env_id: String,
        #[arg(long, alias = "tei", env = "PISA_SEARCH_TEI")]
        embedder: Option<String>,
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Free text: natural language, LaTeX, Lean or a name.
        text: String,
    },
    /// Repair stored descriptions whose LaTeX was decoded as control
    /// characters (`\times` read as a tab), writing the fixed ones as a new batch.
    RepairDescriptions {
        #[arg(long)]
        informal: String,
    },
    /// Compare two cached sets of vectors for the same document views.
    CompareVectors {
        #[arg(long)]
        corpus: PathBuf,
        #[arg(long)]
        informal: String,
        /// Declarations to compare, one name per line.
        #[arg(long)]
        names: PathBuf,
        /// Model names or labels of the two caches.
        #[arg(long)]
        a: String,
        #[arg(long)]
        b: String,
    },
}

fn read_names(path: &Path) -> anyhow::Result<std::collections::HashSet<String>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// The Qdrant engine for `env_id`, embedding queries with `embedder`.
async fn open_engine(
    qdrant: &QdrantArgs,
    env_id: &str,
    embedder: Option<String>,
) -> anyhow::Result<engine::Engine> {
    // The embedder's blocking HTTP client is made off the async threads.
    let embedder = match embedder {
        Some(spec) => Some(tokio::task::spawn_blocking(move || Embedder::parse(&spec)).await??),
        None => None,
    };
    let symbols = Symbols::parse(symbols::DEFAULT)?;
    engine::Engine::open(qdrant.client()?, env_id, symbols, embedder).await
}

/// Drops `value`, which may hold a blocking HTTP client, off the async threads.
async fn drop_blocking<T: Send + 'static>(value: T) {
    let _ = tokio::task::spawn_blocking(move || drop(value)).await;
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
            env_imports,
        } => {
            let env_id = pisa_protocol::version::project_version(&project)?;
            let manifest = project.join("lake-manifest.json");
            let exports: Vec<&Path> = export.iter().map(PathBuf::as_path).collect();
            let env_imports = env_imports
                .iter()
                .map(|e| {
                    e.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .ok_or_else(|| anyhow::anyhow!("--env-imports {e}: expected ENV=HEADER"))
                })
                .collect::<anyhow::Result<_>>()?;
            let c = corpus::ingest(&exports, env_id, Some(&manifest), &env_imports)?;
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
            embedder,
            label,
            names,
            libraries,
            with_deps,
            limit,
            batch,
            concurrency,
            follow,
        } => {
            let c = Corpus::read(&corpus)?;
            let store = Store::open(&informal)?;
            let embedder = Embedder::parse(&embedder)?;
            let probe = embedder.clone();
            let model = match label {
                Some(l) => l,
                None => tokio::task::spawn_blocking(move || probe.model()).await??,
            };
            let selected: Vec<&corpus::Decl> = match &names {
                Some(path) => {
                    let names = read_names(path)?;
                    c.decls.iter().filter(|d| names.contains(&d.name)).collect()
                }
                None => dense::select(&c.decls, &libraries, with_deps),
            };
            let mut known = dense::cached_hashes(&store, &model).await?;
            let started = std::time::Instant::now();
            let mut total = 0;
            loop {
                let last = follow.as_ref().is_none_or(|f| f.exists());
                let described = descriptions(&c, &store).await?;
                // While following, a declaration that will get a description
                // waits for it, so its view is embedded once.
                let waiting = |d: &corpus::Decl| {
                    !last
                        && (d.informal || informalize::needs_description(d))
                        && !described.contains_key(&d.name)
                };
                let mut docs: Vec<String> = selected
                    .iter()
                    .filter(|d| !waiting(d))
                    .map(|d| dense::document_view(d, described.get(&d.name)))
                    .collect();
                if let Some(n) = limit {
                    docs.truncate(n);
                }
                eprintln!(
                    "{} of {} declarations ready; model {model}",
                    docs.len(),
                    selected.len()
                );
                let n = dense::embed_missing(
                    &store,
                    &embedder,
                    &model,
                    docs,
                    &mut known,
                    batch,
                    concurrency,
                )
                .await?;
                total += n;
                if last {
                    break;
                }
                if n == 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            }
            eprintln!(
                "embedded {total} in {:.0}s",
                started.elapsed().as_secs_f64()
            );
        }
        Command::Informalize {
            corpus,
            informal,
            libraries,
            sample,
            names,
            seed,
            model,
            effort,
            max_tokens,
            concurrency,
            force,
            report,
            bundle,
            judge,
            dry_run,
        } => {
            let c = Corpus::read(&corpus)?;
            let store = Store::open(&informal)?;
            let stored = store.load_all().await?;
            let described = descriptions(&c, &store).await?;
            let undescribed = informalize::undescribed(&c, &stored);
            let names = names.as_deref().map(read_names).transpose()?;
            let mut targets = match (sample, &names) {
                (_, Some(names)) => c.decls.iter().filter(|d| names.contains(&d.name)).collect(),
                (Some(n), None) => informalize::pilot_sample(&c.decls, &libraries, n, seed),
                (None, None) => c
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
            let sample_text = match (sample, &names) {
                (_, Some(_)) => format!("{} named declarations", targets.len()),
                (Some(n), None) => format!(
                    "{} declarations from {} {} modules (about {n} requested, whole modules stratified by area, seed {seed})",
                    targets.len(),
                    modules.len(),
                    libraries.join("/")
                ),
                (None, None) => format!("all of {}", libraries.join(", ")),
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
            let client = Llm::for_model(&cfg.model)?;
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
                let mut discrimination = Vec::new();
                if let Some(b) = bundle {
                    let bundle = Bundle::open(&b)?;
                    let mut entries: Vec<_> =
                        outcomes.iter().filter_map(|o| o.entry.as_ref()).collect();
                    entries.sort_by_key(|e| informalize::rank(seed + 1, &e.name));
                    entries.truncate(20);
                    let judges = if judge.is_empty() {
                        vec![cfg.model.clone()]
                    } else {
                        judge
                    };
                    for model in judges {
                        let judge_cfg = informalize::Config {
                            model,
                            ..cfg.clone()
                        };
                        let client = Llm::for_model(&judge_cfg.model)?;
                        discrimination.push(
                            informalize::discriminate(&client, &judge_cfg, &bundle, &entries, seed)
                                .await?,
                        );
                    }
                }
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
            vector_names,
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
                    let mut v = dense::vectors_by_name(&c.decls, &described, model, &cache);
                    if let Some(path) = &vector_names {
                        let names = read_names(path)?;
                        v.retain(|name, _| names.contains(name));
                    }
                    Some(v)
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
        Command::Load {
            bundle,
            qdrant,
            replication,
            min_coverage,
        } => {
            let started = std::time::Instant::now();
            let q = qdrant.client()?;
            let loaded = pisa_search::load::load(&bundle, &q, replication, min_coverage).await?;
            tracing::info!(
                env_id = loaded.env_id,
                points = loaded.points,
                removed = loaded.removed,
                elapsed = format!("{:.0}s", started.elapsed().as_secs_f64()),
                "loaded the bundle"
            );
        }
        Command::Serve {
            qdrant,
            env_id,
            http,
            symbols,
            embedder,
            tikv_pd,
            snapshot_version,
            describe_model,
            describe_concurrency,
        } => {
            // Compose passes unset variables as empty ones.
            let tikv_pd: Vec<String> = tikv_pd.into_iter().filter(|p| !p.is_empty()).collect();
            let describe_model = describe_model.filter(|m| !m.is_empty());
            let symbols = match symbols {
                Some(p) => Symbols::parse(&std::fs::read_to_string(p)?)?,
                None => Symbols::parse(symbols::DEFAULT)?,
            };
            let embedder = match embedder.clone() {
                Some(spec) => {
                    Some(tokio::task::spawn_blocking(move || Embedder::parse(&spec)).await??)
                }
                None => None,
            };
            let mut engine = pisa_search::engine::Engine::open(
                qdrant.client()?,
                &env_id,
                symbols,
                embedder.clone(),
            )
            .await?;
            engine.snapshots = !tikv_pd.is_empty();
            let engine = Arc::new(engine);
            if !tikv_pd.is_empty() {
                let kv = pisa_kv::kv::Kv::tikv(&tikv_pd)
                    .await
                    .map_err(|e| anyhow::anyhow!("connecting to TiKV: {e}"))?;
                let version = snapshot_version.unwrap_or_else(|| env_id.clone());
                tracing::info!(%version, "searching snapshots");
                let llm = match describe_model {
                    Some(model) => Some((
                        Llm::for_model(&model)?,
                        informalize::Config {
                            model,
                            effort: "low".into(),
                            max_tokens: 4096,
                            concurrency: describe_concurrency,
                        },
                    )),
                    None => None,
                };
                // Snapshot vectors join the bundle's, so without those there are none.
                let embedder = embedder.filter(|_| engine.manifest.embedding_model.is_some());
                let how = indexer::Describe { llm, embedder };
                let indexer = indexer::Indexer::new(
                    engine.clone(),
                    kv,
                    version,
                    how,
                    describe_concurrency,
                    indexer::LEASE_TTL,
                );
                tokio::spawn(indexer.run());
            }
            tracing::info!(
                env_id = engine.env_id(),
                decls = engine.manifest.decls,
                snapshots = engine.snapshots,
                %http,
                "serving"
            );
            let listener = tokio::net::TcpListener::bind(http).await?;
            axum::serve(listener, pisa_search::server::router(engine)).await?;
        }
        Command::Eval {
            qdrant,
            env_id,
            data,
            weights,
            fused_only,
            embedder,
            out,
        } => {
            let mut engine = open_engine(&qdrant, &env_id, embedder).await?;
            engine.cache = Some(Default::default());
            let weights: Vec<Weights> = if weights.is_empty() {
                vec![Weights::default()]
            } else {
                weights
                    .iter()
                    .map(|w| serde_json::from_str(w))
                    .collect::<Result<_, _>>()?
            };
            let items = pisa_search::eval::load(&data).await?;
            let labels = engine::weight_labels(weights.len());
            let mut configs = Vec::new();
            for (w, suffix) in weights.iter().zip(&labels) {
                for (label, lanes) in engine::lane_configs(fused_only) {
                    configs.push((format!("{label}{suffix}"), lanes, w.clone()));
                }
            }
            let report = pisa_search::eval::run(&mut engine, &items, &configs).await?;
            let report = report.markdown(engine.env_id());
            print!("{report}");
            if let Some(out) = out {
                std::fs::write(out, report)?;
            }
            drop_blocking(engine).await;
        }
        Command::RepairDescriptions { informal } => {
            let store = Store::open(&informal)?;
            let fix = pisa_search::text::repair_escapes;
            let repaired: Vec<_> = store
                .load_all()
                .await?
                .into_values()
                .filter_map(|mut e| {
                    let i = &mut e.informal;
                    let before = (
                        i.informal_name.clone(),
                        i.statement_nl.clone(),
                        i.statement_latex.clone(),
                    );
                    i.informal_name = fix(&i.informal_name);
                    i.statement_nl = fix(&i.statement_nl);
                    i.statement_latex = i.statement_latex.as_deref().map(fix);
                    (before
                        != (
                            i.informal_name.clone(),
                            i.statement_nl.clone(),
                            i.statement_latex.clone(),
                        ))
                        .then_some(e)
                })
                .collect();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
            if !repaired.is_empty() {
                store
                    .write_batch(&format!("{}-repair", now.as_millis()), &repaired)
                    .await?;
            }
            println!("repaired {} descriptions", repaired.len());
        }
        Command::CompareVectors {
            corpus,
            informal,
            names,
            a,
            b,
        } => {
            let c = Corpus::read(&corpus)?;
            let store = Store::open(&informal)?;
            let described = descriptions(&c, &store).await?;
            let names = read_names(&names)?;
            let (cache_a, cache_b) = (
                dense::load_cache(&store, &a).await?,
                dense::load_cache(&store, &b).await?,
            );
            let mut cosines: Vec<f32> = c
                .decls
                .iter()
                .filter(|d| names.contains(&d.name))
                .filter_map(|d| {
                    let text = dense::document_view(d, described.get(&d.name));
                    let va = cache_a.get(&dense::text_hash(&a, &text))?;
                    let vb = cache_b.get(&dense::text_hash(&b, &text))?;
                    Some(va.iter().zip(vb).map(|(x, y)| x * y).sum())
                })
                .collect();
            anyhow::ensure!(
                !cosines.is_empty(),
                "no document has vectors in both caches"
            );
            cosines.sort_by(f32::total_cmp);
            let mean = cosines.iter().sum::<f32>() / cosines.len() as f32;
            println!(
                "{} documents: mean cosine {mean:.4}, min {:.4}, 5th percentile {:.4}",
                cosines.len(),
                cosines[0],
                cosines[cosines.len() / 20]
            );
        }
        Command::Query {
            qdrant,
            env_id,
            embedder,
            k,
            text,
        } => {
            let engine = open_engine(&qdrant, &env_id, embedder).await?;
            let q = pisa_protocol::search::Query {
                q: Some(text),
                k,
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let r = engine.search(&q).await?;
            for hit in &r.hits {
                println!("{}  ({:.4})\n", hit.text, hit.score);
            }
            eprintln!("{:.1} ms", started.elapsed().as_secs_f64() * 1e3);
            drop_blocking(engine).await;
        }
    }
    Ok(())
}
