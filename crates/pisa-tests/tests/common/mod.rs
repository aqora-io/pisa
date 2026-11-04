//! A router plus workers running fake-repl, all in-process. Their metadata
//! is in an in-memory SQLite database, or in the PostgreSQL at
//! `PISA_TEST_DATABASE_URL` when that is set.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use object_store::ObjectStore;
use object_store::memory::InMemory;
use object_store::path::Path;
use pisa_router::{RouterConfig, Running};
use pisa_worker::WorkerConfig;
use pisa_worker::config::{Sandbox, StoreKind};
use pisa_worker::db::Db;
use serde_json::Value;
use tokio::task::JoinHandle;

pub struct Cluster {
    pub router: Running,
    pub workers: Vec<JoinHandle<()>>,
    pub client: reqwest::Client,
    /// Shared by all workers, standing in for S3.
    pub store: Arc<dyn ObjectStore>,
    /// Shared by all workers: in memory, or the PostgreSQL at
    /// `PISA_TEST_DATABASE_URL`.
    pub db: Db,
    /// What workers' versions get on PostgreSQL, so that clusters there don't
    /// share rows; see [`Cluster::version`].
    suffix: String,
    /// Workers' scratch space, private to this cluster so that tests running
    /// in parallel with the same worker names don't share files.
    pub dir: tempfile::TempDir,
}

pub fn router_config() -> RouterConfig {
    RouterConfig {
        http: "127.0.0.1:0".parse().unwrap(),
        workers: "127.0.0.1:0".parse().unwrap(),
        max_wait_secs: 10,
        max_retries: 2,
        default_version: None,
        alias: Vec::new(),
    }
}

pub fn worker_config(router: SocketAddr, name: &str, version: &str, slots: usize) -> WorkerConfig {
    WorkerConfig {
        router: router.to_string(),
        worker_id: Some(name.into()),
        version: Some(version.into()),
        slots: Some(slots),
        repl: PathBuf::from(env!("CARGO_BIN_EXE_fake-repl")),
        lean: PathBuf::from(env!("CARGO_BIN_EXE_fake-lean")),
        verifier: PathBuf::from("PisaVerify.lean"),
        project: None,
        max_uses: 100,
        max_rss_mb: 0,
        import_timeout_secs: 10,
        prewarm: Vec::new(),
        sandbox: Sandbox::None,
        work_dir: None,
        cgroup_root: None,
        memory_max_mb: 0,
        pids_max: 0,
        allowed_axioms: split(pisa_worker::policy::DEFAULT_AXIOMS),
        allowed_options: split(pisa_worker::policy::DEFAULT_OPTIONS),
        allow_metaprogramming: false,
        store: StoreKind::Memory,
        database_url: None,
        state_ttl_secs: 0,
        snapshot_confirm_secs: 60,
        drain_timeout_secs: 30,
    }
}

fn split(list: &str) -> Vec<String> {
    list.split(',').map(String::from).collect()
}

/// A new cluster's database, and the suffix for its versions: a random one
/// on PostgreSQL, which every cluster shares.
async fn db_store() -> (Db, String) {
    match std::env::var("PISA_TEST_DATABASE_URL") {
        Ok(url) => {
            let db = Db::connect(Some(&url)).await.unwrap();
            let id = uuid::Uuid::new_v4().simple().to_string();
            (db, format!("-{}", &id[..12]))
        }
        Err(_) => (Db::memory().await, String::new()),
    }
}

impl Cluster {
    pub async fn start(workers: &[(&str, &str, usize)]) -> Cluster {
        Self::start_with(router_config(), workers, |c| c).await
    }

    /// Starts the router and `(name, version, slots)` workers. Requests can
    /// name a version as given here; see [`Cluster::version`].
    pub async fn start_with(
        mut router: RouterConfig,
        workers: &[(&str, &str, usize)],
        tweak: impl Fn(WorkerConfig) -> WorkerConfig,
    ) -> Cluster {
        let (db, suffix) = db_store().await;
        if !suffix.is_empty() {
            for (_, version) in &mut router.alias {
                version.push_str(&suffix);
            }
            for (_, version, _) in workers {
                router
                    .alias
                    .push((version.to_string(), format!("{version}{suffix}")));
            }
        }
        let router = pisa_router::start(router).await.unwrap();
        let mut cluster = Cluster {
            router,
            workers: Vec::new(),
            client: reqwest::Client::new(),
            store: Arc::new(InMemory::new()),
            db,
            suffix,
            dir: tempfile::tempdir().unwrap(),
        };
        for (name, version, slots) in workers {
            cluster.add_worker(tweak(worker_config(
                cluster.router.worker_addr,
                name,
                version,
                *slots,
            )));
        }
        cluster.wait_for_workers(workers.len()).await;
        cluster
    }

    /// The version that workers configured with `version` run as, which
    /// responses, keys and paths have: `version` with this cluster's suffix.
    /// The router maps the versions that the cluster started with.
    pub fn version(&self, version: &str) -> String {
        format!("{version}{}", self.suffix)
    }

    pub fn add_worker(&mut self, cfg: WorkerConfig) {
        self.spawn_worker(cfg, std::future::pending());
    }

    /// Adds a worker that drains and exits when the returned sender fires.
    pub fn add_drainable_worker(&mut self, cfg: WorkerConfig) -> tokio::sync::oneshot::Sender<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.spawn_worker(cfg, async move {
            let _ = rx.await;
        });
        tx
    }

    fn spawn_worker(
        &mut self,
        mut cfg: WorkerConfig,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let name = cfg.worker_id.clone().unwrap_or_default();
        cfg.work_dir
            .get_or_insert_with(|| self.dir.path().join(name));
        cfg.version = cfg.version.map(|v| self.version(&v));
        let store = self.store.clone();
        let db = self.db.clone();
        self.workers.push(tokio::spawn(async move {
            pisa_worker::run_with_stores(cfg, store, db, shutdown)
                .await
                .unwrap();
        }));
    }

    pub async fn wait_for_workers(&self, n: usize) {
        let d = Arc::clone(&self.router.app.dispatcher);
        tokio::time::timeout(Duration::from_secs(10), async {
            while d.worker_count() != n {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("workers connect");
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.router.http_addr)
    }

    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap())
    }

    /// Every object in the store under `snapshots`.
    pub async fn stored_blobs(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut list = self.store.list(Some(&Path::from("snapshots")));
        while let Some(meta) = list.next().await {
            out.push(meta.unwrap().location.to_string());
        }
        out
    }

    /// Every row of `version` (as [`Cluster::version`] maps it), as its
    /// table and key, except the change log's, which outlive what they
    /// describe.
    pub async fn stored_rows(&self, version: &str) -> Vec<Vec<String>> {
        self.db.rows(&self.version(version)).await.unwrap()
    }

    pub async fn metrics(&self) -> String {
        self.client
            .get(self.url("/metrics"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    }
}
