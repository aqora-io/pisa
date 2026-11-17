use super::*;
use delete::Claim;
use object_store::memory::InMemory;
use pisa_protocol::verify::ProofStatus;

/// A worker's snapshots over `db`, with its local cache in `dir`.
fn snapshots(db: Db, dir: &Path) -> Arc<Snapshots> {
    snapshots_on(Arc::new(InMemory::new()), db, dir)
}

/// [`snapshots`] on `store`.
fn snapshots_on(store: Arc<dyn ObjectStore>, db: Db, dir: &Path) -> Arc<Snapshots> {
    std::fs::create_dir_all(dir.join(snapshot::ROOT)).unwrap();
    let policy = Policy {
        allowed_axioms: crate::policy::DEFAULT_AXIOMS
            .split(',')
            .map(String::from)
            .collect(),
        allowed_options: Vec::new(),
        allow_metaprogramming: false,
    };
    let s = Snapshots::new(
        store,
        db,
        "v".into(),
        dir.into(),
        Duration::from_secs(60),
        policy,
    );
    Arc::new(s)
}

/// A new build of module `id`: one `.olean` that holds `content`.
fn built(id: &str, content: &[u8]) -> ModuleMeta {
    let own = Module {
        id: id.into(),
        build: uuid::Uuid::new_v4().simple().to_string(),
        files: vec![ModuleFile {
            ext: "olean".into(),
            sha256: sha256(content),
        }],
    };
    ModuleMeta {
        imports: Vec::new(),
        modules: vec![own],
        header: String::new(),
        code: String::new(),
        status: ProofStatus::Proved,
        verified: false,
        macros: false,
        result: CheckResult::default(),
        created: 0,
    }
}

fn no_labels() -> NewEntry {
    NewEntry {
        labels: Default::default(),
        key: None,
        replaces: None,
        replacement: None,
        holes: Vec::new(),
        fork: false,
        amends: None,
        refutes: None,
        name: None,
        source: None,
        runs: Vec::new(),
    }
}

fn keyed(key: &str) -> NewEntry {
    NewEntry {
        key: Some(key.into()),
        ..no_labels()
    }
}

/// Every row of version `v`, except the change log's, which outlive what
/// they describe, and the catalog's id, which isn't what anything stored.
async fn rows(db: &Db) -> Vec<Vec<String>> {
    let rows = db.rows("v").await.unwrap();
    rows.into_iter().filter(|r| r[0] != "catalogs").collect()
}

async fn upload(s: &Snapshots, meta: &ModuleMeta, content: &[u8]) {
    let compressed = zstd::encode_all(content, 3).unwrap();
    let path = s.blob(&meta.modules[0], "olean");
    s.store.put(&path, compressed.into()).await.unwrap();
}

async fn has_files(s: &Snapshots, meta: &ModuleMeta) -> bool {
    s.store
        .get(&s.blob(&meta.modules[0], "olean"))
        .await
        .is_ok()
}

/// The race a delete has to survive: while it removes a module, a create
/// of the same code on this worker builds the module again, uploads it and
/// stores it. The delete removes only the files of the build it removed,
/// and leaves the new build's local files alone.
#[tokio::test]
async fn a_delete_leaves_a_new_build_of_its_module_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let catalog = Catalog::new(db, "v".into());
    let id = snapshot::id("code");
    let old = built(&id, b"old");
    upload(&s, &old, b"old").await;
    let (entry, _, _) = catalog.commit(&id, &old, None, no_labels()).await.unwrap();

    // The new build is uploaded and installed here, but not stored yet.
    let new = built(&id, b"new");
    upload(&s, &new, b"new").await;
    std::fs::write(s.file(&id, "olean"), b"new").unwrap();
    s.cached
        .lock()
        .unwrap()
        .insert(id.clone(), new.modules[0].clone());
    // A lookup vouched for the old build before.
    s.confirmed
        .lock()
        .unwrap()
        .insert(id.clone(), Instant::now());

    // The build holds the module's lock until it has stored its record,
    // so the delete removes the old record and files, then waits there.
    let lock = s.lock(&id);
    let building = lock.lock().await;
    let deleting = tokio::spawn({
        let s = s.clone();
        async move { s.delete(&s.id(&entry), false).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while has_files(&s, &old).await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    catalog.commit(&id, &new, None, no_labels()).await.unwrap();
    drop(building);

    let deleted = deleting.await.unwrap().unwrap();
    assert_eq!(deleted.removed, [snapshot::module(&id)]);
    assert!(has_files(&s, &new).await, "the new build's files stay");
    assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"new");
    assert_eq!(s.cached.lock().unwrap().get(&id), Some(&new.modules[0]));
    assert_eq!(s.catalog.module(&id).await.unwrap().modules, new.modules);
    // The local copy might have been the old build: the next import
    // looks the module up again.
    assert!(!s.confirmed.lock().unwrap().contains_key(&id));
}

/// A worker that died while deleting C, whose module imports P's, left
/// P's record and files and C's files. The sweep clears them, and the
/// files of K's build that no record names, but not K, nor files newer
/// than its grace.
#[tokio::test]
async fn a_sweep_clears_what_a_stopped_delete_left() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let catalog = Catalog::new(db.clone(), "v".into());
    let own = |m: &ModuleMeta| m.modules.last().unwrap().clone();
    let put = |m: Module| {
        let s = s.clone();
        async move {
            let path = s.blob(&m, "olean");
            s.store.put(&path, b"x".to_vec().into()).await.unwrap();
        }
    };
    let stored = |m: Module| {
        let s = s.clone();
        async move { s.store.get(&s.blob(&m, "olean")).await.is_ok() }
    };
    let (p_id, c_id, k_id) = (snapshot::id("p"), snapshot::id("c"), snapshot::id("k"));
    let p = built(&p_id, b"p");
    let mut c = built(&c_id, b"c");
    c.imports = vec![p_id.clone()];
    c.modules.insert(0, own(&p));
    let k = built(&k_id, b"k");
    for m in [&p, &c, &k] {
        put(own(m)).await;
    }
    let (pe, ..) = catalog.commit(&p_id, &p, None, no_labels()).await.unwrap();
    let (ce, ..) = catalog.commit(&c_id, &c, None, no_labels()).await.unwrap();
    catalog.commit(&k_id, &k, None, no_labels()).await.unwrap();
    catalog.remove_entry(&pe).await.unwrap();
    // The delete that removed C's entry and record went no further.
    catalog.remove_entry(&ce).await.unwrap();
    let stray = own(&built(&k_id, b"an old build"));
    put(stray.clone()).await;
    // As a worker does with its first upload.
    s.mark_store().await.unwrap();

    // Files are kept for their grace, as a build uploads them first.
    let (modules, files) = s.sweep(Duration::from_secs(3600)).await.unwrap();
    assert_eq!((modules, files), (1, FileSweep::Swept(0)));
    assert!(stored(own(&c)).await && stored(stray.clone()).await);
    let (modules, files) = s.sweep(Duration::ZERO).await.unwrap();
    assert_eq!((modules, files), (0, FileSweep::Swept(2)));
    assert!(!stored(own(&p)).await && !stored(own(&c)).await && !stored(stray).await);
    assert!(stored(own(&k)).await);
    assert!(catalog.module(&p_id).await.is_err());
    assert!(catalog.module(&k_id).await.is_ok());
}

/// A build kept with no entry, as a create refused for what it replaces
/// keeps it, is swept only once it is older than the grace.
#[tokio::test]
async fn a_kept_build_outlives_the_sweeps_grace() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    s.mark_store().await.unwrap();
    let id = snapshot::id("kept");
    let meta = ModuleMeta {
        created: build::now(),
        ..built(&id, b"kept")
    };
    upload(&s, &meta, b"kept").await;
    let catalog = Catalog::new(db, "v".into());
    catalog.commit_module(&id, &meta, None).await.unwrap();
    assert_eq!(
        s.sweep(Duration::from_secs(3600)).await.unwrap(),
        (0, FileSweep::Swept(0))
    );
    assert!(has_files(&s, &meta).await);
    assert_eq!(
        s.sweep(Duration::ZERO).await.unwrap(),
        (1, FileSweep::Swept(0))
    );
    assert!(!has_files(&s, &meta).await);
    assert!(catalog.module(&id).await.is_err());
}

/// A worker on another catalog, an empty one here, finds no record of the
/// store's files: it leaves them alone, and the store keeps naming the
/// catalog that wrote them.
#[tokio::test]
async fn a_sweep_leaves_another_catalogs_store_alone() {
    let (dir, other_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let db = Db::memory().await;
    let s = snapshots_on(store.clone(), db.clone(), dir.path());
    let other = snapshots_on(store, Db::memory().await, other_dir.path());
    let id = snapshot::id("m");
    let meta = built(&id, b"m");
    s.mark_store().await.unwrap();
    upload(&s, &meta, b"m").await;
    Catalog::new(db, "v".into())
        .commit(&id, &meta, None, no_labels())
        .await
        .unwrap();

    other.mark_store().await.unwrap();
    let ours = s.catalog.identity().await.unwrap();
    let (_, refused) = other.sweep(Duration::ZERO).await.unwrap();
    let FileSweep::Refused(why) = refused else {
        panic!("{refused:?}")
    };
    assert!(why.contains(&ours), "{why}");
    assert!(has_files(&s, &meta).await);
    assert_eq!(other.store_catalog().await.unwrap(), Some(ours));
    // Its own catalog's worker still sweeps it.
    assert_eq!(
        s.sweep(Duration::ZERO).await.unwrap(),
        (0, FileSweep::Swept(0))
    );
    assert!(has_files(&s, &meta).await);
}

/// A store that names no catalog yet, as one an earlier release wrote,
/// isn't swept until a worker's upload marks it. A worker marks it only if
/// it knows every build the store holds, but for those new enough to be
/// uploads whose records are on their way.
#[tokio::test]
async fn an_unmarked_store_is_claimed_only_by_a_catalog_that_knows_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let kept = built(&snapshot::id("kept"), b"kept");
    upload(&s, &kept, b"kept").await;
    Catalog::new(db, "v".into())
        .commit(&kept.modules[0].id, &kept, None, no_labels())
        .await
        .unwrap();
    let stray = built(&snapshot::id("gone"), b"gone");
    upload(&s, &stray, b"gone").await;
    let (_, refused) = s.sweep(Duration::ZERO).await.unwrap();
    let FileSweep::Refused(why) = refused else {
        panic!("{refused:?}")
    };
    assert!(why.contains("PISA_CLAIM_STORE"), "{why}");
    assert!(has_files(&s, &stray).await);
    // As though the stray were older than the claim's grace.
    let Claim::Refused(why) = s.claim(Duration::ZERO).await.unwrap() else {
        panic!("claimed a store with a file it has no record of")
    };
    assert!(why.contains(&stray.modules[0].id), "{why}");
    assert!(why.contains("PISA_CLAIM_STORE"), "{why}");
    assert_eq!(s.store_catalog().await.unwrap(), None);
    // A new file is an upload on its way: it doesn't stop the claim.
    s.mark_store().await.unwrap();
    let ours = s.catalog.identity().await.unwrap();
    assert_eq!(s.store_catalog().await.unwrap(), Some(ours));
    assert_eq!(
        s.sweep(Duration::ZERO).await.unwrap(),
        (0, FileSweep::Swept(1))
    );
    assert!(!has_files(&s, &stray).await && has_files(&s, &kept).await);
}

/// A new store names no catalog until a worker uploads to it: its sweep has
/// nothing to do, which isn't a refusal.
#[tokio::test]
async fn an_empty_unmarked_store_sweeps_nothing_and_refuses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = snapshots(Db::memory().await, dir.path());
    assert_eq!(
        s.sweep(Duration::ZERO).await.unwrap(),
        (0, FileSweep::Swept(0))
    );
}

/// `PISA_CLAIM_STORE` makes the store's marker name this catalog, whatever
/// it named: a reset database on a kept store.
#[tokio::test]
async fn a_worker_told_to_claims_the_store() {
    let (dir, other_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let old = snapshots_on(store.clone(), Db::memory().await, dir.path());
    old.mark_store().await.unwrap();
    let stray = built(&snapshot::id("old"), b"old");
    upload(&old, &stray, b"old").await;
    let reset = snapshots_on(store, Db::memory().await, other_dir.path());
    let reset = Arc::into_inner(reset).unwrap().claiming_store(true);
    assert_eq!(reset.claim(Duration::ZERO).await.unwrap(), Claim::Ours);
    let ours = reset.catalog.identity().await.unwrap();
    assert_eq!(reset.store_catalog().await.unwrap(), Some(ours));
    // The old catalog's files have no record here, and go.
    assert_eq!(
        reset.sweep(Duration::ZERO).await.unwrap(),
        (0, FileSweep::Swept(1))
    );
}

/// A sweep that would delete most of the store's files stops: a catalog
/// that has lost its records looks like that.
#[tokio::test]
async fn a_sweep_stops_when_most_files_have_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    s.mark_store().await.unwrap();
    let catalog = Catalog::new(db, "v".into());
    let kept = built(&snapshot::id("kept"), b"kept");
    upload(&s, &kept, b"kept").await;
    catalog
        .commit(&kept.modules[0].id, &kept, None, no_labels())
        .await
        .unwrap();
    let strays: Vec<ModuleMeta> = (0..delete::SWEEP_GUARD)
        .map(|i| built(&snapshot::id(&format!("stray {i}")), b"x"))
        .collect();
    for stray in &strays {
        upload(&s, stray, b"x").await;
    }
    let (_, refused) = s.sweep(Duration::ZERO).await.unwrap();
    let FileSweep::Refused(why) = refused else {
        panic!("{refused:?}")
    };
    assert!(why.contains("have no record"), "{why}");
    for stray in &strays {
        assert!(has_files(&s, stray).await);
    }
    // Fewer than the guard go.
    s.store
        .delete(&s.blob(&strays[0].modules[0], "olean"))
        .await
        .unwrap();
    assert_eq!(
        s.sweep(Duration::ZERO).await.unwrap(),
        (0, FileSweep::Swept(delete::SWEEP_GUARD - 1))
    );
    assert!(has_files(&s, &kept).await);
}

/// A re-check that passes on a module's old build. Meanwhile the module
/// was deleted and built again: the new build stays unverified, and so
/// does the old one's record, which no longer exists.
#[tokio::test]
async fn a_verify_of_an_old_build_leaves_the_new_one_unverified() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let catalog = Catalog::new(db, "v".into());
    let id = snapshot::id("code");
    let old = built(&id, b"old");
    let (entry, _, _) = catalog.commit(&id, &old, None, no_labels()).await.unwrap();
    catalog.remove_entry(&entry).await.unwrap();
    let new = built(&id, b"new");
    catalog.commit(&id, &new, None, no_labels()).await.unwrap();

    let passed = Verification {
        ok: true,
        reasons: Vec::new(),
        declarations: Vec::new(),
        changes: Vec::new(),
    };
    let old = &old.modules[0];
    assert_eq!(
        s.before_recheck(old, false).await.unwrap(),
        Some(Recheck::Changed)
    );
    let recorded = s.record_recheck(old, &passed).await.unwrap();
    assert_eq!(recorded, Recheck::Changed);
    assert!(!catalog.module(&id).await.unwrap().verified);

    let new = &new.modules[0];
    let recorded = s.record_recheck(new, &passed).await.unwrap();
    assert_eq!(recorded, Recheck::Passed);
    assert!(catalog.module(&id).await.unwrap().verified);
}

/// The local copy is an old build that a lookup still vouches for, as on
/// a worker that didn't see the module deleted and built again. The
/// re-check of the stored build fetches that build first, since the
/// verifier reads the local files.
#[tokio::test]
async fn a_verify_fetches_the_build_it_checks() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let id = snapshot::id("code");
    let old = built(&id, b"old");
    std::fs::write(s.file(&id, "olean"), b"old").unwrap();
    s.cached
        .lock()
        .unwrap()
        .insert(id.clone(), old.modules[0].clone());
    s.confirmed
        .lock()
        .unwrap()
        .insert(id.clone(), Instant::now());
    let new = built(&id, b"new");
    upload(&s, &new, b"new").await;
    let catalog = Catalog::new(db, "v".into());
    catalog.commit(&id, &new, None, no_labels()).await.unwrap();

    assert_eq!(
        s.before_recheck(&new.modules[0], false).await.unwrap(),
        None
    );
    assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"new");
    assert_eq!(s.cached.lock().unwrap().get(&id), Some(&new.modules[0]));
}

/// A record the store couldn't read back is refused before anything is
/// written, and the files uploaded for it go.
#[tokio::test]
async fn a_record_too_large_is_refused_and_its_files_go() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let id = snapshot::id("code");
    let mut ours = built(&id, b"ours");
    ours.code = "x".repeat(7 * 1024 * 1024);
    let own = ours.modules[0].clone();
    let contents = [b"ours".to_vec()];
    let error = s
        .commit(&own, &contents, &ours, None, keyed("k"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest);
    assert!(
        error.message.contains("snapshot record too large"),
        "{error:?}"
    );
    assert!(rows(&db).await.is_empty());
    assert!(!has_files(&s, &ours).await, "this build's files go");
}

#[tokio::test]
async fn a_listing_takes_at_most_64_filters() {
    let dir = tempfile::tempdir().unwrap();
    let s = snapshots(Db::memory().await, dir.path());
    let filters = |n: usize| -> Vec<(String, String)> {
        (0..n).map(|i| (i.to_string(), String::new())).collect()
    };
    assert!(s.reader().list(&filters(64), 10, None, false).await.is_ok());
    let error = s
        .reader()
        .list(&filters(65), 10, None, false)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
}

/// A snapshot id of another version, which a router routes elsewhere, or
/// a bare entry, is refused here too.
#[tokio::test]
async fn an_id_of_another_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let s = snapshots(Db::memory().await, dir.path());
    let entry = "0".repeat(32);
    let error = s.reader().get(&format!("w~{entry}")).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
    assert!(error.message.contains("belongs to version w"), "{error:?}");
    let error = s.reader().get(&entry).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
    let error = s.reader().get(&s.id(&entry)).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound, "{error:?}");
}

/// Two workers build the same code at once and the other one stores it
/// first: this one adds its entry to their record, deletes the files it
/// uploaded and installs their build instead.
#[tokio::test]
async fn a_commit_that_finds_the_module_stored_keeps_that_build() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let id = snapshot::id("code");
    let theirs = built(&id, b"theirs");
    upload(&s, &theirs, b"theirs").await;
    let catalog = Catalog::new(db, "v".into());
    catalog
        .commit(&id, &theirs, None, no_labels())
        .await
        .unwrap();

    // As `install` leaves this worker's build.
    let ours = built(&id, b"ours");
    std::fs::write(s.file(&id, "olean"), b"ours").unwrap();
    let own = ours.modules[0].clone();
    s.cached.lock().unwrap().insert(id.clone(), own.clone());

    let contents = [b"ours".to_vec()];
    let (stored, (entry, _)) = s
        .commit(&own, &contents, &ours, None, no_labels())
        .await
        .unwrap();
    assert_eq!(stored.modules, theirs.modules);
    assert_eq!(catalog.entry(&entry).await.unwrap().module, id);
    assert!(!has_files(&s, &ours).await, "this build's files go");
    assert!(has_files(&s, &theirs).await);
    assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"theirs");
    assert_eq!(s.cached.lock().unwrap().get(&id), Some(&theirs.modules[0]));
}

/// Another create took the key for other code while this one was
/// building: its transaction writes nothing, not even the module and its
/// child link, and the blobs uploaded for it go. An entry added to a kept
/// module under that key gets the other code's entry back too.
#[tokio::test]
async fn a_commit_whose_key_is_used_by_other_code_keeps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let catalog = Catalog::new(db.clone(), "v".into());
    let (parent_id, other_id, id) = (
        snapshot::id("parent"),
        snapshot::id("other"),
        snapshot::id("code"),
    );
    let parent = built(&parent_id, b"parent");
    let other = built(&other_id, b"other");
    catalog
        .commit(&parent_id, &parent, None, no_labels())
        .await
        .unwrap();
    let (taken, _, _) = catalog
        .commit(&other_id, &other, None, keyed("k"))
        .await
        .unwrap();
    upload(&s, &other, b"other").await;
    let before = rows(&db).await;

    let ours = built(&id, b"ours");
    let own = ours.modules[0].clone();
    upload(&s, &ours, b"ours").await;
    let mut on_parent = ours.clone();
    on_parent.imports = vec![parent_id.clone()];
    on_parent.modules.insert(0, parent.modules[0].clone());
    let (entry, entry_meta, _) = catalog
        .commit(&id, &on_parent, None, keyed("k"))
        .await
        .unwrap();
    assert_eq!((entry, entry_meta.module), (taken.clone(), other_id));
    assert_eq!(rows(&db).await, before);

    let contents = [b"ours".to_vec()];
    let error = s
        .commit(&own, &contents, &on_parent, None, keyed("k"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest);
    assert!(error.message.contains("different code"), "{error:?}");
    assert_eq!(rows(&db).await, before);
    assert!(catalog.module(&id).await.is_err());
    assert!(!has_files(&s, &ours).await, "this build's files go");
    assert!(has_files(&s, &other).await);

    let added = catalog.add_entry(&parent_id, keyed("k")).await.unwrap();
    assert_eq!(added.map(|(e, _)| e), Some(taken));
    assert_eq!(rows(&db).await, before);
}

/// The key is already on an entry of this very module: that entry is
/// returned unchanged with the stored record, and this build's files go.
#[tokio::test]
async fn a_commit_whose_key_is_on_the_same_module_returns_that_entry() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::memory().await;
    let s = snapshots(db.clone(), dir.path());
    let catalog = Catalog::new(db.clone(), "v".into());
    let id = snapshot::id("code");
    let theirs = built(&id, b"theirs");
    upload(&s, &theirs, b"theirs").await;
    let mut labelled = keyed("k");
    labelled.labels.insert("a".into(), "1".into());
    let (taken, _, _) = catalog.commit(&id, &theirs, None, labelled).await.unwrap();
    let before = rows(&db).await;

    let ours = built(&id, b"ours");
    let own = ours.modules[0].clone();
    upload(&s, &ours, b"ours").await;
    std::fs::write(s.file(&id, "olean"), b"ours").unwrap();
    let contents = [b"ours".to_vec()];
    let (stored, (entry, entry_meta)) = s
        .commit(&own, &contents, &ours, None, keyed("k"))
        .await
        .unwrap();
    assert_eq!(entry, taken);
    assert_eq!(entry_meta.labels["a"], "1");
    assert_eq!(stored.modules, theirs.modules);
    assert_eq!(rows(&db).await, before);
    assert!(!has_files(&s, &ours).await, "this build's files go");
    assert!(has_files(&s, &theirs).await);
}

#[tokio::test]
async fn the_store_is_probed_for_reachability() {
    let dir = tempfile::tempdir().unwrap();
    let reachable = snapshots(Db::memory().await, dir.path());
    assert!(reachable.store_reachable().await);

    // Nothing listens on the discard port.
    let down = object_store::aws::AmazonS3Builder::new()
        .with_endpoint("http://127.0.0.1:9")
        .with_allow_http(true)
        .with_bucket_name("b")
        .with_access_key_id("k")
        .with_secret_access_key("s")
        .with_region("r")
        .with_retry(object_store::RetryConfig {
            max_retries: 0,
            ..Default::default()
        })
        .build()
        .unwrap();
    let unreachable = snapshots_on(Arc::new(down), Db::memory().await, dir.path());
    assert!(!unreachable.store_reachable().await);
}
