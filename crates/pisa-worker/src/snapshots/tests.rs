use super::*;
use object_store::memory::InMemory;
use pisa_protocol::verify::ProofStatus;

/// A worker's snapshots over `db`, with its local cache in `dir`.
fn snapshots(db: Db, dir: &Path) -> Arc<Snapshots> {
    std::fs::create_dir_all(dir.join(snapshot::ROOT)).unwrap();
    let store = Arc::new(InMemory::new());
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
    }
}

fn keyed(key: &str) -> NewEntry {
    NewEntry {
        key: Some(key.into()),
        ..no_labels()
    }
}

/// Every row of version `v`, except the change log's, which outlive what
/// they describe.
async fn rows(db: &Db) -> Vec<Vec<String>> {
    db.rows("v").await.unwrap()
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
    assert_eq!(s.before_recheck(old).await.unwrap(), Some(Recheck::Changed));
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

    assert_eq!(s.before_recheck(&new.modules[0]).await.unwrap(), None);
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
