use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use pisa_db::description_flags::DescriptionFlags;
use pisa_protocol::search::Informal;
use pisa_search::corpus::{self, Corpus};
use pisa_search::dense::{self, Embedder};
use pisa_search::engine::{self, Weights};
use pisa_search::index::{self, Bundle};
use pisa_search::informal_store::{Store, description_hash};
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
        Qdrant::with_key_env(&self.url, self.key_env.as_deref())
    }
}

#[derive(Subcommand)]
enum Command {
    /// Turn exporter output into a corpus with content hashes.
    Ingest(IngestArgs),
    /// Store LeanSearch v2's Mathlib descriptions for matching declarations.
    ImportLsv2(ImportLsv2Args),
    /// Carry stored descriptions over to a rebuilt corpus, for declarations
    /// unchanged but for docstring whitespace (which Lean releases reformat).
    Carry(CarryArgs),
    /// Embed document views through an embedding server, caching the vectors.
    Embed(EmbedArgs),
    /// Describe declarations in natural language with Claude or an OpenAI
    /// model, in dependency order, caching the descriptions. Needs
    /// ANTHROPIC_API_KEY or OPENAI_API_KEY.
    Informalize(InformalizeArgs),
    /// Build an index bundle from a corpus and stored descriptions.
    Build(BuildArgs),
    /// Load a bundle into Qdrant, as the declarations of its `env_id`.
    Load(LoadArgs),
    /// Measure retrieval quality on MathlibQR and evaluation sets.
    Eval(EvalArgs),
    /// Write the physics evaluation sets that need no model spend: the
    /// describer's queries, and premise retrieval from proofs.
    EvalSets(EvalSetsArgs),
    /// Run one search against a loaded bundle and print the hits.
    Query(QueryArgs),
    /// Ask a model which hypotheses stored descriptions leave out, of
    /// declarations with anything like a hypothesis; describe each that
    /// leaves some out again, once, told what, and write those the model then
    /// passes as a new batch. Prints a line per description judged. Needs
    /// ANTHROPIC_API_KEY or OPENAI_API_KEY.
    ValidateDescriptions(ValidateDescriptionsArgs),
}

#[derive(clap::Args)]
struct IngestArgs {
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
    /// `LIBRARY=MODULE,MODULE,…`: the library's declarations are loaded
    /// by importing these modules rather than the default imports, e.g.
    /// `QuantumInfo=Mathlib,Physlib,QuantumInfo`.
    #[arg(long)]
    library_imports: Vec<String>,
}

#[derive(clap::Args)]
struct ImportLsv2Args {
    #[arg(long)]
    corpus: PathBuf,
    /// Where the LeanSearch v2 JSONL is (downloaded and verified if absent).
    #[arg(long)]
    file: PathBuf,
    /// Description and vector store (a directory or s3:// URL).
    #[arg(long)]
    informal: String,
}

#[derive(clap::Args)]
struct CarryArgs {
    /// The corpus the stored descriptions were made for.
    #[arg(long)]
    from: PathBuf,
    #[arg(long)]
    corpus: PathBuf,
    /// Description and vector store (a directory or s3:// URL).
    #[arg(long)]
    informal: String,
}

#[derive(clap::Args)]
struct EmbedArgs {
    #[arg(long)]
    corpus: PathBuf,
    /// Description and vector store (a directory or s3:// URL).
    #[arg(long)]
    informal: String,
    /// A TEI URL, or `openai+<base url>?model=<model>[&key_env=<VAR>]`.
    #[arg(
        long,
        env = "PISA_SEARCH_EMBEDDER",
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
}

#[derive(clap::Args)]
struct InformalizeArgs {
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
}

#[derive(clap::Args)]
struct BuildArgs {
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
}

#[derive(clap::Args)]
struct LoadArgs {
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
}

#[derive(clap::Args)]
struct EvalArgs {
    #[command(flatten)]
    qdrant: QdrantArgs,
    /// The version whose loaded bundle to search, the bundle's `env_id`.
    #[arg(long)]
    version: String,
    /// MathlibQR.json (downloaded from the pinned commit if absent).
    #[arg(long)]
    data: Option<PathBuf>,
    /// An evaluation set (`search/eval/sets/*.json`); repeat for several.
    #[arg(long)]
    set: Vec<PathBuf>,
    /// Only this half of each set: `dev` to tune on, `test` to report on.
    #[arg(long, value_enum)]
    split: Option<pisa_search::eval::Split>,
    /// Weights to use instead of the defaults, as JSON (fields of
    /// `Weights`); several are evaluated in turn, embedding queries once.
    #[arg(long)]
    weights: Vec<String>,
    /// Only the fused result, not each lane alone.
    #[arg(long)]
    fused_only: bool,
    /// Embedding server for queries (enables the dense lane); see `embed`.
    #[arg(long, env = "PISA_SEARCH_EMBEDDER")]
    embedder: Option<String>,
    /// Write the report here as well as to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args)]
struct EvalSetsArgs {
    #[arg(long)]
    corpus: PathBuf,
    /// The description store whose synthetic queries to draw on.
    #[arg(long)]
    informal: String,
    /// Directory to write `physics-synthetic.json` and `physics-premises.json` to.
    #[arg(long)]
    out: PathBuf,
    /// Declarations drawn from each physics library.
    #[arg(long, default_value_t = 300)]
    per_library: usize,
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

#[derive(clap::Args)]
struct QueryArgs {
    #[command(flatten)]
    qdrant: QdrantArgs,
    /// The version whose loaded bundle to search.
    #[arg(long)]
    version: String,
    #[arg(long, env = "PISA_SEARCH_EMBEDDER")]
    embedder: Option<String>,
    #[arg(long, default_value_t = 5)]
    k: usize,
    /// Free text: natural language, LaTeX, Lean or a name.
    text: String,
}

#[derive(clap::Args)]
struct ValidateDescriptionsArgs {
    #[arg(long)]
    corpus: PathBuf,
    /// Description and vector store (a directory or s3:// URL).
    #[arg(long)]
    informal: String,
    /// Only descriptions from this source, e.g. `lsv2-v4.28.0-rc1`.
    #[arg(long)]
    source: Option<String>,
    /// Only descriptions that readers flagged, in the database.
    #[arg(long)]
    flagged: bool,
    /// The database that keeps the flags, for `--flagged`.
    #[arg(long, env = "PISA_DATABASE_URL")]
    database_url: Option<String>,
    /// Only these declarations: names, or files of one name per line,
    /// separated by commas.
    #[arg(long, value_delimiter = ',')]
    names: Vec<String>,
    /// The model that judges and describes, as the server's describer.
    #[arg(
        long,
        env = "PISA_SEARCH_DESCRIBE_MODEL",
        default_value = "claude-haiku-5-5"
    )]
    model: String,
    #[arg(long, default_value = "low")]
    effort: String,
    #[arg(long, default_value_t = 4096)]
    max_tokens: u32,
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    /// Print what would be judged instead of calling the API.
    #[arg(long)]
    dry_run: bool,
}

/// Names given as names, or as files of one name per line.
fn names_or_files(args: &[String]) -> anyhow::Result<std::collections::HashSet<String>> {
    let mut names = std::collections::HashSet::new();
    for a in args.iter().map(|a| a.trim()).filter(|a| !a.is_empty()) {
        if Path::new(a).is_file() {
            names.extend(read_names(Path::new(a))?);
        } else {
            names.insert(a.to_string());
        }
    }
    Ok(names)
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
        Command::Ingest(args) => ingest(args).await,
        Command::ImportLsv2(args) => import_lsv2(args).await,
        Command::Carry(args) => carry(args).await,
        Command::Embed(args) => embed(args).await,
        Command::Informalize(args) => informalize(args).await,
        Command::Build(args) => build(args).await,
        Command::Load(args) => load(args).await,
        Command::Eval(args) => eval(args).await,
        Command::EvalSets(args) => eval_sets(args).await,
        Command::ValidateDescriptions(args) => validate_descriptions(args).await,
        Command::Query(args) => query(args).await,
    }
}

async fn ingest(
    IngestArgs {
        export,
        project,
        out,
        previous,
        library_imports,
    }: IngestArgs,
) -> anyhow::Result<()> {
    let env_id = pisa_protocol::version::project_version(&project)?;
    let manifest = project.join("lake-manifest.json");
    let exports: Vec<&Path> = export.iter().map(PathBuf::as_path).collect();
    let library_imports = corpus::library_imports(&library_imports)?;
    let c = corpus::ingest(&exports, env_id, Some(&manifest), &library_imports)?;
    c.write(&out)?;
    println!("{}", serde_json::to_string_pretty(&c.manifest)?);
    if let Some(prev) = previous {
        let old = Corpus::read(&prev)?;
        println!("{:?}", corpus::diff(&old.decls, &c.decls));
    }
    Ok(())
}

async fn import_lsv2(
    ImportLsv2Args {
        corpus,
        file,
        informal,
    }: ImportLsv2Args,
) -> anyhow::Result<()> {
    pisa_search::lsv2::fetch(&file).await?;
    let c = Corpus::read(&corpus)?;
    let (entries, report) = pisa_search::lsv2::import(&file, &c.decls)?;
    let store = Store::open(&informal)?;
    // Sorts before any later run's batches, so those win.
    store
        .write_batch(&format!("0000-{}", pisa_search::lsv2::SOURCE), &entries)
        .await?;
    println!("{report:#?}");
    Ok(())
}

async fn carry(
    CarryArgs {
        from,
        corpus,
        informal,
    }: CarryArgs,
) -> anyhow::Result<()> {
    let old = Corpus::read(&from)?;
    let c = Corpus::read(&corpus)?;
    let store = Store::open(&informal)?;
    let (entries, report) =
        pisa_search::carry::carry(&old.decls, &c.decls, &store.load_all().await?);
    if !entries.is_empty() {
        // Timestamped like informalize's batches, so later runs win.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
        let batch = format!("{}-carry-{}", now.as_millis(), c.manifest.env_id);
        store.write_batch(&batch, &entries).await?;
    }
    println!("{report:#?}");
    Ok(())
}

async fn embed(
    EmbedArgs {
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
    }: EmbedArgs,
) -> anyhow::Result<()> {
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
    Ok(())
}

async fn informalize(
    InformalizeArgs {
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
    }: InformalizeArgs,
) -> anyhow::Result<()> {
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
                libraries.contains(&d.library) && (d.informal || informalize::needs_description(d))
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
    let outcomes = informalize::run(&client, &cfg, &store, &c, &targets, described, &batch).await?;
    let elapsed_secs = started.elapsed().as_secs_f64();
    let cost: f64 = outcomes.iter().filter_map(|o| o.cost).sum();
    let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
    let judge_calls: u32 = outcomes.iter().map(|o| o.judge_calls).sum();
    let redescribed = outcomes.iter().filter(|o| o.redescribed).count();
    eprintln!(
        "described {} of {} in {elapsed_secs:.0}s for ${cost:.3}, with {judge_calls} \
         judge calls; {redescribed} described again for leaving out hypotheses",
        outcomes.len() - failed,
        outcomes.len()
    );
    if let Some(path) = report {
        let mut discrimination = Vec::new();
        if let Some(b) = bundle {
            let bundle = Bundle::open(&b)?;
            let mut entries: Vec<_> = outcomes.iter().filter_map(|o| o.entry.as_ref()).collect();
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
                    informalize::discriminate(&client, &judge_cfg, &bundle, &entries, seed).await?,
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
    Ok(())
}

async fn build(
    BuildArgs {
        corpus,
        informal,
        embedding_model,
        vector_names,
        out,
    }: BuildArgs,
) -> anyhow::Result<()> {
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
    Ok(())
}

async fn load(
    LoadArgs {
        bundle,
        qdrant,
        replication,
        min_coverage,
    }: LoadArgs,
) -> anyhow::Result<()> {
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
    Ok(())
}

async fn eval(
    EvalArgs {
        qdrant,
        version,
        data,
        set,
        split,
        weights,
        fused_only,
        embedder,
        out,
    }: EvalArgs,
) -> anyhow::Result<()> {
    let mut engine = open_engine(&qdrant, &version, embedder).await?;
    engine.cache = std::sync::Arc::new(std::sync::Mutex::new(lru::LruCache::unbounded()));
    engine.embed_wait = engine::EMBED_TIMEOUT + std::time::Duration::from_secs(1);
    let weights: Vec<Weights> = if weights.is_empty() {
        vec![Weights::default()]
    } else {
        weights
            .iter()
            .map(|w| serde_json::from_str(w))
            .collect::<Result<_, _>>()?
    };
    let mut sets = Vec::new();
    if let Some(data) = &data {
        sets.push(pisa_search::eval::mathlib_qr(data).await?);
    }
    for path in &set {
        sets.push(pisa_search::eval::read_set(path)?);
    }
    anyhow::ensure!(
        !sets.is_empty(),
        "name MathlibQR with --data or a set with --set"
    );
    let labels = engine::weight_labels(weights.len());
    let mut configs = Vec::new();
    for (w, suffix) in weights.iter().zip(&labels) {
        for (label, lanes) in engine::lane_configs(fused_only) {
            configs.push((format!("{label}{suffix}"), lanes, w.clone()));
        }
    }
    let mut report = String::new();
    for s in &sets {
        let r = pisa_search::eval::run(&mut engine, s, split, &configs).await?;
        if !report.is_empty() {
            report.push('\n');
        }
        report.push_str(&r.markdown(engine.env_id()));
    }
    print!("{report}");
    if let Some(out) = out {
        std::fs::write(out, report)?;
    }
    drop_blocking(engine).await;
    Ok(())
}

async fn eval_sets(
    EvalSetsArgs {
        corpus,
        informal,
        out,
        per_library,
        seed,
    }: EvalSetsArgs,
) -> anyhow::Result<()> {
    let c = Corpus::read(&corpus)?;
    let entries = Store::open(&informal)?.load_all().await?;
    std::fs::create_dir_all(&out)?;
    use pisa_search::eval_sets;
    for set in [
        eval_sets::synthetic(&c.decls, &entries, per_library, seed),
        eval_sets::premises(&c.decls, per_library, seed),
    ] {
        let path = out.join(format!("{}.json", set.name));
        std::fs::write(&path, pisa_search::eval::write_set(&set)?)?;
        eprintln!("wrote {} ({} items)", path.display(), set.items.len());
    }
    Ok(())
}

async fn validate_descriptions(
    ValidateDescriptionsArgs {
        corpus,
        informal,
        source,
        flagged,
        database_url,
        names,
        model,
        effort,
        max_tokens,
        concurrency,
        dry_run,
    }: ValidateDescriptionsArgs,
) -> anyhow::Result<()> {
    let c = Corpus::read(&corpus)?;
    let store = Store::open(&informal)?;
    let stored = store.load_all().await?;
    let names = (!names.is_empty())
        .then(|| names_or_files(&names))
        .transpose()?;
    // The flags on each declaration's current description.
    let flags = match (flagged, database_url) {
        (false, _) => None,
        (true, Some(url)) => {
            let db = pisa_db::db::Db::connect(Some(&url)).await?;
            let mut flags: HashMap<(String, String), usize> = HashMap::new();
            for f in DescriptionFlags::new(db).all().await? {
                *flags.entry((f.name, f.description_hash)).or_default() += 1;
            }
            Some(flags)
        }
        (true, None) => anyhow::bail!("--flagged needs --database-url"),
    };
    let flagged_times = |d: &corpus::Decl, i: &Informal| {
        let hash = description_hash(
            &i.informal_name,
            &i.statement_nl,
            i.statement_latex.as_deref(),
        );
        let key = (d.name.clone(), hash);
        flags.as_ref().and_then(|f| f.get(&key)).copied()
    };
    let (mut targets, mut skipped) = (Vec::new(), 0);
    for d in &c.decls {
        let Some(e) = stored.get(&d.content_hash) else {
            continue;
        };
        let wanted = source.as_ref().is_none_or(|s| *s == e.informal.source)
            && names.as_ref().is_none_or(|n| n.contains(&d.name))
            && (!flagged || flagged_times(d, &e.informal).is_some())
            && informalize::needs_description(d);
        if !wanted {
            continue;
        }
        if informalize::has_hypotheses(d) {
            targets.push(d);
        } else {
            skipped += 1;
        }
    }
    if let Some(names) = &names {
        let described: std::collections::HashSet<&str> = (c.decls.iter())
            .filter(|d| stored.contains_key(&d.content_hash))
            .map(|d| d.name.as_str())
            .collect();
        let mut missing: Vec<&String> = (names.iter())
            .filter(|n| !described.contains(n.as_str()))
            .collect();
        missing.sort();
        for n in missing {
            eprintln!("{n}: not in the corpus, or no description stored");
        }
    }
    eprintln!(
        "{} descriptions to judge ({skipped} more have nothing like a hypothesis)",
        targets.len()
    );
    let cfg = informalize::Config {
        model,
        effort,
        max_tokens,
        concurrency,
    };
    let source_of = |d: &corpus::Decl| stored[&d.content_hash].informal.source.clone();
    if dry_run {
        for d in &targets {
            println!("{} ({})", d.name, source_of(d));
        }
        return Ok(());
    }
    let client = Llm::for_model(&cfg.model)?;
    let described = descriptions(&c, &store).await?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let batch = format!("{}-validate-{}", now.as_millis(), cfg.model);
    let judged =
        informalize::validate(&client, &cfg, &store, &c, &targets, described, &batch).await?;
    let by_name: HashMap<&str, &corpus::Decl> =
        targets.iter().map(|d| (d.name.as_str(), *d)).collect();
    for (name, j) in &judged {
        let d = by_name[name.as_str()];
        let flagged = match flagged_times(d, &stored[&d.content_hash].informal) {
            Some(1) => ", flagged once".to_string(),
            Some(n) => format!(", flagged {n} times"),
            None => String::new(),
        };
        println!("{name} ({}{flagged}): {}", source_of(d), j.verdict());
    }
    let count =
        |f: &dyn Fn(&informalize::Judged) -> bool| judged.iter().filter(|(_, j)| f(j)).count();
    let leaving_out = count(&|j| j.missing.as_ref().is_ok_and(|m| !m.is_empty()));
    let replaced = count(&|j| j.replaced);
    let judge_calls: u32 = judged.iter().map(|(_, j)| j.judge_calls).sum();
    let cost: f64 = judged.iter().filter_map(|(_, j)| j.cost).sum();
    let written = match replaced {
        0 => String::new(),
        _ => format!(" in batches {batch}-*"),
    };
    eprintln!(
        "judged {} descriptions with {judge_calls} judge calls for ${cost:.3}: {leaving_out} \
         leave out hypotheses, {replaced} replaced{written}, {} kept, {} not judged",
        judged.len(),
        leaving_out - replaced,
        count(&|j| j.missing.is_err())
    );
    Ok(())
}

async fn query(
    QueryArgs {
        qdrant,
        version,
        embedder,
        k,
        text,
    }: QueryArgs,
) -> anyhow::Result<()> {
    let mut engine = open_engine(&qdrant, &version, embedder).await?;
    // Answering without the dense lane would skew what it shows.
    engine.embed_wait = engine::EMBED_TIMEOUT + std::time::Duration::from_secs(1);
    let q = pisa_protocol::search::Query {
        text: Some(text),
        limit: k,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let r = engine.search(&q).await?;
    for hit in &r.hits {
        println!("{}  ({:.4})\n", hit.text, hit.score);
    }
    eprintln!("{:.1} ms", started.elapsed().as_secs_f64() * 1e3);
    drop_blocking(engine).await;
    Ok(())
}
