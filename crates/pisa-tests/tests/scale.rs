//! A library at the size agent-built ones reach, without Lean: a chain of
//! 1,500 snapshot modules, each importing the one before it, committed
//! through the catalog. It checks that a module's record reads its whole
//! closure in order and that the reads and a rebuild plan over the chain
//! answer, and prints how long each took. Ignored by default, as it takes a
//! while; run it with
//! `cargo test -p pisa-tests --test scale -- --ignored --nocapture`.

mod common;

use std::time::Instant;

use common::Cluster;
use pisa_protocol::api::{CheckResult, ProofStatus};
use pisa_protocol::snapshot;
use pisa_worker::catalog::{Catalog, Module, ModuleMeta, NewEntry};
use serde_json::{Value, json};

const CHAIN: usize = 1500;

async fn create(c: &Cluster, code: &str) -> Value {
    let (status, r) = c.post("/snapshots", json!({"code": code})).await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    r
}

async fn get(c: &Cluster, path: &str) -> (u16, Value) {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

fn timed<T>(what: &str, start: Instant, out: T) -> T {
    println!("{what}: {:.2}s", start.elapsed().as_secs_f64());
    out
}

#[tokio::test]
#[ignore = "commits a 1,500-module chain; run with --ignored"]
async fn a_chain_of_1500_modules_reads_and_plans() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let from = create(&c, "import Foo\n\ntheorem b : 1 = 1 := rfl").await;
    let to = create(&c, "import Foo\n\ntheorem b : 1 = 1 := by rfl").await;
    let base = from["snapshot"]["module"].as_str().unwrap()["PisaSnap.s".len()..].to_string();
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let mut modules = catalog.module(&base).await.unwrap().modules;
    let built_on = modules.len();

    // Each module imports the one before it.
    let start = Instant::now();
    let mut below = base.clone();
    let mut chain = Vec::new();
    for i in 0..CHAIN {
        let import = format!("import PisaSnap.s{below}");
        let code = format!("{import}\n\ntheorem d{i} : {i} = {i} := rfl");
        let id = snapshot::id(&code);
        modules.push(Module {
            id: id.clone(),
            build: "b".into(),
            files: Vec::new(),
        });
        let meta = ModuleMeta {
            imports: vec![below.clone()],
            modules: modules.clone(),
            header: snapshot::next_header(&import, &id),
            code,
            status: ProofStatus::Proved,
            verified: false,
            macros: false,
            result: CheckResult::default(),
            created: 0,
        };
        let entry = NewEntry {
            labels: [("p".to_string(), "1".to_string())].into(),
            key: None,
            replaces: None,
            replacement: None,
            holes: Vec::new(),
            fork: false,
            amends: None,
            refutes: None,
            name: None,
            source: None,
        };
        catalog.commit(&id, &meta, None, entry).await.unwrap();
        chain.push(id.clone());
        below = id;
    }
    timed(&format!("committing {CHAIN} modules"), start, ());

    // The top module's record holds its whole closure, imports first.
    let start = Instant::now();
    let top = timed(
        "the top module's record",
        start,
        catalog.module(&below).await.unwrap(),
    );
    let read: Vec<&str> = top.modules.iter().map(|m| m.id.as_str()).collect();
    let made: Vec<&str> = modules.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(read, made);
    assert_eq!(read.len(), built_on + CHAIN);

    let version = c.version("v");
    let start = Instant::now();
    let (status, r) = get(
        &c,
        &format!("/snapshots?label=p:1&limit=1000&version={version}"),
    )
    .await;
    timed("a listing page of 1000", start, ());
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["snapshots"].as_array().map(Vec::len), Some(1000));

    let bottom = from["snapshot"]["id"].as_str().unwrap();
    let start = Instant::now();
    let (status, r) = get(&c, &format!("/snapshots/{bottom}/dependents")).await;
    timed("the dependents of the bottom", start, ());
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["snapshots"].as_array().map(Vec::len), Some(CHAIN));

    let start = Instant::now();
    let (status, r) = get(&c, &format!("/snapshots/audit?label=p:1&version={version}")).await;
    timed("a label audit", start, ());
    assert_eq!(status, 200, "{r}");

    let body =
        json!({"from": bottom, "to": to["snapshot"]["id"], "labels": {"p": "1"}, "dry_run": true});
    let start = Instant::now();
    let (status, r) = c.post("/snapshots/rebuild", body).await;
    timed("a rebuild plan of the chain", start, ());
    assert_eq!((status, &r["ok"]), (200, &json!(true)), "{r}");
    assert_eq!(r["plan"].as_array().map(Vec::len), Some(CHAIN));
}
