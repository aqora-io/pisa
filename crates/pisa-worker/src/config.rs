use std::path::PathBuf;

use anyhow::Context;

use crate::db::Db;

#[derive(Debug, Clone, clap::Parser)]
#[command(about = "Pisa worker: runs Lean REPLs for the servers' jobs")]
pub struct WorkerConfig {
    #[arg(long, env = "PISA_WORKER_ID")]
    pub worker_id: Option<String>,
    /// Pool label. Defaults to the project's toolchain plus a hash of its lake-manifest.json.
    #[arg(long, env = "PISA_VERSION")]
    pub version: Option<String>,
    /// The version this worker has to turn out to serve, e.g. the one its
    /// pool is deployed and scaled as: with another, it doesn't start.
    #[arg(long, env = "PISA_EXPECT_VERSION")]
    pub expect_version: Option<String>,
    /// Number of Lean processes. Defaults to the CPU count minus one.
    #[arg(long, env = "PISA_SLOTS")]
    pub slots: Option<usize>,
    /// The REPL executable.
    #[arg(long, env = "PISA_REPL", default_value = "repl")]
    pub repl: PathBuf,
    /// The `lean` executable, used to explain imports the REPL fails to load.
    #[arg(long, env = "PISA_LEAN", default_value = "lean")]
    pub lean: PathBuf,
    /// `lake`, which runs `lake comparator` on the exports of a run's
    /// modules. Elan's picks the toolchain of the project's `lean-toolchain`.
    #[arg(long, env = "PISA_LAKE", default_value = "lake")]
    pub lake: PathBuf,
    /// Lean's bundled `leanexport`, which exports the snapshot modules
    /// comparator judges. Defaults to the one in the toolchain of `--lean`,
    /// as the project resolves it.
    #[arg(long, env = "PISA_LEAN_EXPORT")]
    pub lean_export: Option<PathBuf>,
    /// The `pisa` tool (`docker/pisa-lean`), which builds snapshot modules and
    /// re-verifies them through the kernel.
    #[arg(
        long,
        env = "PISA_TOOL",
        default_value = "/opt/pisa/.lake/build/bin/pisa"
    )]
    pub tool: PathBuf,
    /// The built `Pisa` library (`docker/pisa-lean`), whose report every
    /// header imports: its `.olean` root.
    #[arg(
        long,
        env = "PISA_LEAN_LIB",
        default_value = "/opt/pisa/.lake/build/lib/lean"
    )]
    pub lean_lib: PathBuf,
    /// Lake project whose dependencies the REPL imports (run through `lake env`).
    #[arg(long, env = "PISA_PROJECT")]
    pub project: Option<PathBuf>,
    /// The git repository the project's own code comes from, with
    /// `--project-rev`, for a project that is itself a library code imports,
    /// rather than a wrapper that requires one: an export of a run whose
    /// modules import it requires the project from there, and without them
    /// is refused.
    #[arg(long, env = "PISA_PROJECT_GIT", requires = "project_rev")]
    pub project_git: Option<String>,
    /// The revision of `--project-git` the project is at.
    #[arg(long, env = "PISA_PROJECT_REV", requires = "project_git")]
    pub project_rev: Option<String>,
    /// Replace a Lean process after this many jobs. The REPL never frees environments.
    #[arg(long, env = "PISA_MAX_USES", default_value_t = 200)]
    pub max_uses: u64,
    /// Replace a Lean process once its resident memory exceeds this (0 disables).
    #[arg(long, env = "PISA_MAX_RSS_MB", default_value_t = 0)]
    pub max_rss_mb: u64,
    /// Timeout for importing a header into a fresh process.
    #[arg(long, env = "PISA_IMPORT_TIMEOUT_SECS", default_value_t = 600)]
    pub import_timeout_secs: u64,
    /// Headers to import at startup, e.g. "import Mathlib". Separate several with ';'.
    #[arg(long, env = "PISA_PREWARM", value_delimiter = ';')]
    pub prewarm: Vec<String>,
    /// How to isolate each Lean process.
    #[arg(long, env = "PISA_SANDBOX", value_enum, default_value_t = Sandbox::None)]
    pub sandbox: Sandbox,
    /// Scratch space for per-slot files. Defaults to a directory under $TMPDIR.
    #[arg(long, env = "PISA_WORK_DIR")]
    pub work_dir: Option<PathBuf>,
    /// A delegated cgroup v2 directory. Each Lean process gets its own child
    /// cgroup with the limits below. Without it, only RSS polling applies.
    /// Set, the worker doesn't start if it can't set the cgroups up.
    #[arg(long, env = "PISA_CGROUP_ROOT")]
    pub cgroup_root: Option<PathBuf>,
    /// memory.max per Lean process, in MiB (0 for no limit).
    #[arg(long, env = "PISA_MEMORY_MAX_MB", default_value_t = 0)]
    pub memory_max_mb: u64,
    /// pids.max per Lean process (0 for no limit).
    #[arg(long, env = "PISA_PIDS_MAX", default_value_t = 256)]
    pub pids_max: u64,
    /// Axioms a checked declaration may depend on.
    #[arg(long, env = "PISA_ALLOWED_AXIOMS", value_delimiter = ',',
          default_value = crate::policy::DEFAULT_AXIOMS)]
    pub allowed_axioms: Vec<String>,
    /// Options `set_option` may change; a trailing `*` matches a prefix.
    #[arg(long, env = "PISA_ALLOWED_OPTIONS", value_delimiter = ',',
          default_value = crate::policy::DEFAULT_OPTIONS)]
    pub allowed_options: Vec<String>,
    /// Allow #eval, macros, elaborators and other metaprogramming in submitted code.
    #[arg(long, env = "PISA_ALLOW_METAPROGRAMMING")]
    pub allow_metaprogramming: bool,
    /// Where snapshot modules live. `s3` reads the AWS_* environment
    /// variables (AWS_ENDPOINT, AWS_BUCKET, AWS_REGION, credentials,
    /// AWS_ALLOW_HTTP). All workers of a version must share one store.
    #[arg(long, env = "PISA_STORE", value_enum, default_value_t = StoreKind::Memory)]
    pub store: StoreKind,
    /// The database that holds the job queue, shared with the servers, and
    /// the document, session, state and snapshot records, shared by all
    /// workers of a version: PostgreSQL (`postgres://…`), or a SQLite file
    /// for a single machine. Snapshot modules go to `--store`.
    #[arg(long, env = "PISA_DATABASE_URL", required = true)]
    pub database_url: Option<String>,
    /// The job queue to take jobs from, as the servers name it.
    #[arg(long, env = "PISA_QUEUE", default_value = "pisa")]
    pub queue: String,
    /// How long a claimed job stays this worker's without a heartbeat. A job
    /// whose worker died runs elsewhere once it lapses.
    #[arg(long, env = "PISA_LEASE_SECS", default_value_t = 10)]
    pub lease_secs: u64,
    /// Delete documents, sessions and their states older than this (0 keeps
    /// them).
    #[arg(long, env = "PISA_STATE_TTL_SECS", default_value_t = 0)]
    pub state_ttl_secs: u64,
    /// How long a lookup in the store vouches for a snapshot this worker has
    /// cached. A deleted snapshot keeps working here for at most this long.
    #[arg(long, env = "PISA_SNAPSHOT_CONFIRM_SECS", default_value_t = 60)]
    pub snapshot_confirm_secs: u64,
    /// Make the store's marker name this database's catalog as the worker
    /// starts, whatever it names, so that this catalog's workers sweep the
    /// store: for a database reset on a store that is kept. Unset it again
    /// once a worker has started with it.
    #[arg(long, env = "PISA_CLAIM_STORE")]
    pub claim_store: bool,
    /// On shutdown, how long to wait for in-flight jobs before exiting anyway.
    #[arg(long, env = "PISA_DRAIN_TIMEOUT_SECS", default_value_t = 600)]
    pub drain_timeout_secs: u64,
    /// Lean Beam processes for open documents (`/documents`) and tactic
    /// sessions, besides the REPL slots. 0 serves neither.
    #[arg(long, env = "PISA_BEAM_SLOTS", default_value_t = 1)]
    pub beam_slots: usize,
    /// Documents each Beam process keeps open. To open another, it closes the
    /// least recently used one that no job is using.
    #[arg(long, env = "PISA_BEAM_DOCUMENTS", default_value_t = 4)]
    pub beam_documents: usize,
    /// `lean-beam-mcp`. Defaults to the one in the toolchain of `--lean`.
    #[arg(long, env = "PISA_BEAM_BIN")]
    pub beam_bin: Option<PathBuf>,
    /// memory.max per Beam process, in MiB (0 for no limit).
    #[arg(long, env = "PISA_BEAM_MEMORY_MAX_MB", default_value_t = 0)]
    pub beam_memory_max_mb: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StoreKind {
    /// In this process only; snapshots can't move between workers.
    Memory,
    S3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Sandbox {
    None,
    /// bubblewrap: no network, private /proc and /tmp, read-only filesystem.
    Bwrap,
}

impl WorkerConfig {
    pub fn slots(&self) -> usize {
        self.slots.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1))
        })
    }

    pub fn worker_id(&self) -> String {
        self.worker_id.clone().unwrap_or_else(|| {
            let host = std::env::var("HOSTNAME")
                .ok()
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
                .map(|h| h.trim().to_string())
                .unwrap_or_else(|| "worker".into());
            format!("{host}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
        })
    }

    pub fn policy(&self) -> crate::policy::Policy {
        crate::policy::Policy {
            allowed_axioms: self.allowed_axioms.clone(),
            allowed_options: self.allowed_options.clone(),
            allow_metaprogramming: self.allow_metaprogramming,
        }
    }

    pub fn object_store(&self) -> anyhow::Result<std::sync::Arc<dyn object_store::ObjectStore>> {
        Ok(match self.store {
            StoreKind::Memory => std::sync::Arc::new(object_store::memory::InMemory::new()),
            StoreKind::S3 => std::sync::Arc::new(
                object_store::aws::AmazonS3Builder::from_env()
                    .build()
                    .context("configuring the S3 store")?,
            ),
        })
    }

    pub async fn db(&self) -> anyhow::Result<Db> {
        Db::connect(self.database_url.as_deref())
            .await
            .context("connecting to the database")
    }

    pub fn version(&self) -> anyhow::Result<String> {
        let version = match (&self.version, &self.project) {
            (Some(v), _) => v.clone(),
            (None, Some(project)) => pisa_protocol::version::project_version(project)
                .with_context(|| format!("reading the project at {}", project.display()))?,
            (None, None) => "dev".into(),
        };
        match &self.expect_version {
            Some(expected) if *expected != version => anyhow::bail!(
                "this worker serves {version}, and was expected to serve {expected} \
                 (PISA_EXPECT_VERSION): its pool names another version than its image's project has"
            ),
            _ => Ok(version),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::WorkerConfig;

    #[test]
    fn the_projects_source_takes_both_its_repository_and_revision() {
        let parse = |args: &[&str]| {
            let base = ["pisa-worker", "--database-url", "sqlite::memory:"];
            WorkerConfig::try_parse_from([&base[..], args].concat())
        };
        let both = parse(&[
            "--project-git",
            "https://example.com/l.git",
            "--project-rev",
            "33c9",
        ]);
        let both = both.unwrap();
        assert_eq!(
            both.project_git.as_deref(),
            Some("https://example.com/l.git")
        );
        assert_eq!(both.project_rev.as_deref(), Some("33c9"));
        assert!(parse(&["--project-git", "https://example.com/l.git"]).is_err());
        assert!(parse(&["--project-rev", "33c9"]).is_err());
    }

    #[test]
    fn a_worker_of_another_version_than_expected_has_none() {
        let parse = |args: &[&str]| {
            let base = ["pisa-worker", "--database-url", "sqlite::memory:"];
            WorkerConfig::try_parse_from([&base[..], args].concat()).unwrap()
        };
        let version = |args: &[&str]| parse(args).version().map_err(|e| e.to_string());
        assert_eq!(version(&["--version", "v1"]), Ok("v1".to_string()));
        let same = ["--version", "v1", "--expect-version", "v1"];
        assert_eq!(version(&same), Ok("v1".to_string()));
        let other = version(&["--version", "v1", "--expect-version", "v2"]).unwrap_err();
        assert!(
            other.contains("serves v1") && other.contains("v2"),
            "{other}"
        );
    }
}
