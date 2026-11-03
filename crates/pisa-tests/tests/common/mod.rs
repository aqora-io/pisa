//! A router plus workers running fake-repl, all in-process.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use object_store::ObjectStore;
use object_store::memory::InMemory;
use pisa_router::{RouterConfig, Running};
use pisa_worker::WorkerConfig;
use pisa_worker::config::{MetadataKind, Sandbox, StoreKind};
use pisa_worker::kv::Kv;
use serde_json::Value;
use tokio::task::JoinHandle;

pub struct Cluster {
    pub router: Running,
    pub workers: Vec<JoinHandle<()>>,
    pub client: reqwest::Client,
    /// Shared by all workers, standing in for S3.
    pub store: Arc<dyn ObjectStore>,
    /// Shared by all workers, standing in for TiKV.
    pub kv: Kv,
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
        metadata: MetadataKind::Memory,
        tikv_pd: vec![],
        state_ttl_secs: 0,
        snapshot_confirm_secs: 60,
        drain_timeout_secs: 30,
    }
}

fn split(list: &str) -> Vec<String> {
    list.split(',').map(String::from).collect()
}

impl Cluster {
    pub async fn start(workers: &[(&str, &str, usize)]) -> Cluster {
        Self::start_with(router_config(), workers, |c| c).await
    }

    pub async fn start_with(
        router: RouterConfig,
        workers: &[(&str, &str, usize)],
        tweak: impl Fn(WorkerConfig) -> WorkerConfig,
    ) -> Cluster {
        let router = pisa_router::start(router).await.unwrap();
        let mut cluster = Cluster {
            router,
            workers: Vec::new(),
            client: reqwest::Client::new(),
            store: Arc::new(InMemory::new()),
            kv: Kv::memory(),
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
        let store = self.store.clone();
        let kv = self.kv.clone();
        self.workers.push(tokio::spawn(async move {
            pisa_worker::run_with_stores(cfg, store, kv, shutdown)
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
