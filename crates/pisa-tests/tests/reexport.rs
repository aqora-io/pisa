//! Module-system snapshots whose export is from before builds exported
//! their non-public declarations are exported again by a worker.

mod common;

use std::time::Duration;

use common::{Cluster, worker_config};
use pisa_worker::catalog::{Catalog, EXPORT_FORMAT, SnapshotDecls};
use serde_json::json;

const MODULE: &str = "module\n\npublic theorem t : 1 = 1 := rfl\n\ndef hidden := 7";
const PLAIN: &str = "theorem p : 2 = 2 := rfl\n\ndef seven := 7";

/// The module of a create of `code`, with its own build.
async fn created(c: &Cluster, catalog: &Catalog, code: &str) -> (String, String) {
    let (status, r) = c.post("/snapshots", json!({"code": code})).await;
    assert_eq!(status, 200, "{r}");
    let name = r["snapshot"]["module"].as_str().unwrap();
    let m = name.strip_prefix("PisaSnap.s").unwrap().to_string();
    let build = catalog
        .module(&m)
        .await
        .unwrap()
        .modules
        .pop()
        .unwrap()
        .build;
    (m, build)
}

/// Stores module `m`'s export as an older build made it: in no format, and
/// without its declarations other than `kept`.
async fn make_old(catalog: &Catalog, m: &str, build: &str, kept: &str) {
    let mut decls = catalog.decls(m).await.unwrap().unwrap();
    decls.decls.retain(|d| d.name == kept);
    let old = SnapshotDecls { format: 0, ..decls };
    assert!(catalog.replace_decls(m, build, &old).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_exports_an_old_module_system_snapshot_again() {
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let (m, build) = created(&c, &catalog, MODULE).await;
    let (plain, plain_build) = created(&c, &catalog, PLAIN).await;
    let names = |decls: &SnapshotDecls| -> Vec<String> {
        let mut names: Vec<String> = decls.decls.iter().map(|d| d.name.clone()).collect();
        names.sort();
        names
    };
    assert_eq!(
        names(&catalog.decls(&m).await.unwrap().unwrap()),
        ["hidden", "t"]
    );
    make_old(&catalog, &m, &build, "t").await;
    make_old(&catalog, &plain, &plain_build, "p").await;
    let before = catalog.changes(0, None, 1000).await.unwrap().len();

    // A worker looks for old exports as it starts.
    c.add_worker(worker_config("w1", "v", 1));
    let decls = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let decls = catalog.decls(&m).await.unwrap().unwrap();
            if decls.format == EXPORT_FORMAT {
                return decls;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the export is made again");
    assert_eq!(names(&decls), ["hidden", "t"]);
    let hidden = decls.decls.iter().find(|d| d.name == "hidden").unwrap();
    assert_eq!(hidden.value.as_deref(), Some("7"));
    // Search hears of it.
    let changes = catalog.changes(0, None, 1000).await.unwrap();
    let logged = changes[before..]
        .iter()
        .filter(|l| l.change.module.as_deref() == Some(&m));
    assert_eq!(logged.count(), 1);
    // Code outside the module system exported all it declares already.
    let plain = catalog.decls(&plain).await.unwrap().unwrap();
    assert_eq!((names(&plain), plain.format), (vec!["p".to_string()], 0));
}
