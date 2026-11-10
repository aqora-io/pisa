use super::audit::Permitted;
use super::*;
use object_store::memory::InMemory;

fn decl(name: &str, ty: &str, value: Option<&str>, axioms: &[&str]) -> Declaration {
    let reported = ReportedDeclaration {
        name: name.into(),
        kind: if value.is_some() { "def" } else { "theorem" }.into(),
        axioms: axioms.iter().map(|a| a.to_string()).collect(),
        type_hash: ty.into(),
        value_hash: value.map(Into::into),
        ..Default::default()
    };
    reported.with_status(ProofStatus::Proved)
}

/// A `kind` declaration whose type uses `type_refs` and value
/// `value_refs`.
fn declared(name: &str, kind: &str, type_refs: &[&str], value_refs: &[&str]) -> Declaration {
    let d = Declaration {
        kind: kind.into(),
        ..decl(name, "", None, &[])
    };
    using(d, type_refs, value_refs)
}

/// An `allow` that names `names`.
fn allow(names: &[&str]) -> Option<Allow> {
    Some(Allow {
        definitions: names.iter().map(|n| n.to_string()).collect(),
    })
}

/// [`super::compare`], its refusals as the reasons they read as.
fn compare(
    replacing: &Replacing,
    now: &[Declaration],
    swapped: &Swapped,
) -> Result<Replacement, Vec<String>> {
    super::compare(replacing, now, swapped)
        .map_err(|changes| changes.iter().map(ToString::to_string).collect())
}

fn replacing(old: Vec<Declaration>, allow: Option<Allow>) -> Replacing {
    Replacing {
        replaced: Replaces {
            snapshot_id: "v~e".into(),
            allow,
        },
        entry: "e".into(),
        module: "m".into(),
        declarations: old,
        needs: vec!["a".into()],
        holes: Vec::new(),
    }
}

#[test]
fn a_replacement_keeps_statements_and_says_what_it_filled() {
    let old = vec![
        decl("t", "1", None, &["sorryAx"]),
        decl("u", "2", None, &["sorryAx"]),
        decl("f", "3", Some("4"), &[]),
    ];
    let now = [
        decl("t", "1", None, &["propext"]),
        decl("u", "2", None, &["sorryAx"]),
        decl("f", "3", Some("4"), &[]),
        decl("helper", "9", None, &[]),
    ];
    // It may build on more snapshots than the replaced module does.
    let needs = Swapped::default();
    let r = compare(&replacing(old.clone(), None), &now, &needs).unwrap();
    assert_eq!(r.filled, ["t"]);
    assert!(r.changed_definitions.is_empty() && r.affected.is_empty());

    let retyped = [decl("t", "X", None, &[]), now[1].clone(), now[2].clone()];
    assert_eq!(
        compare(&replacing(old.clone(), None), &retyped, &needs),
        Err(vec!["statement: t has a different type".to_string()])
    );
    // A definition's value changes only when that is allowed, and never
    // its type.
    let redefined = [
        now[0].clone(),
        now[1].clone(),
        decl("f", "3", Some("5"), &[]),
    ];
    assert_eq!(
        compare(&replacing(old.clone(), None), &redefined, &needs),
        Err(vec!["statement: f has a different value".to_string()])
    );
    let allowed = replacing(old.clone(), allow(&["f"]));
    let r = compare(&allowed, &redefined, &needs).unwrap();
    assert_eq!(r.changed_definitions, ["f"]);
    assert_eq!(compare(&allowed, &retyped, &needs).unwrap_err().len(), 1);
}

fn using(d: Declaration, type_refs: &[&str], value_refs: &[&str]) -> Declaration {
    let refs = |r: &[&str]| r.iter().map(|r| r.to_string()).collect();
    Declaration {
        type_refs: refs(type_refs),
        value_refs: refs(value_refs),
        ..d
    }
}

/// A module on snapshot `a`, whose `thm3` states something about `a`'s
/// `foo`, which uses `bar`; and a replacement on `a2` instead, which
/// declares `foo` and `bar` again as `foo2` and `bar2` say.
fn swap(foo2: Option<Declaration>, bar2: Declaration) -> (Vec<Declaration>, Swapped) {
    let old = vec![using(decl("thm3", "9", None, &[]), &["foo"], &[])];
    let foo = using(decl("foo", "1", Some("2"), &[]), &[], &["bar"]);
    let bar = using(decl("bar", "3", Some("4"), &[]), &[], &[]);
    let lemma = using(decl("lemma", "7", None, &[]), &[], &[]);
    let gained = foo2.into_iter().chain([bar2, lemma]).collect();
    let swapped = Swapped {
        lost: vec![("a".into(), vec![foo, bar])],
        gained: vec![("a2".into(), gained)],
    };
    (old, swapped)
}

#[test]
fn only_the_named_definitions_may_change_their_value() {
    let old = vec![
        decl("f", "1", Some("2"), &[]),
        decl("g", "1", Some("2"), &[]),
    ];
    let now = [
        decl("f", "1", Some("3"), &[]),
        decl("g", "1", Some("3"), &[]),
    ];
    let none = Swapped::default();
    assert_eq!(
        compare(&replacing(old.clone(), allow(&["f"])), &now, &none),
        Err(vec!["statement: g has a different value".to_string()])
    );
    let r = compare(&replacing(old, allow(&["f", "g"])), &now, &none).unwrap();
    assert_eq!(r.changed_definitions, ["f", "g"]);
}

#[test]
fn allow_names_definitions_of_the_replaced_module_or_a_lost_snapshot() {
    let old = vec![decl("f", "1", Some("2"), &[]), decl("t", "3", None, &[])];
    let swapped = Swapped {
        lost: vec![(
            "a".into(),
            vec![
                decl("foo", "1", Some("2"), &[]),
                decl("_private.PisaSnap.sa.0.h", "1", Some("2"), &[]),
            ],
        )],
        gained: vec![("b".into(), vec![decl("bar", "1", Some("2"), &[])])],
    };
    assert!(allow_error(&replacing(old.clone(), None), &swapped).is_none());
    for names in [&["f"][..], &["foo"], &["f", "foo"], &["_private.h"]] {
        let allowed = replacing(old.clone(), allow(names));
        assert!(allow_error(&allowed, &swapped).is_none(), "{names:?}");
    }
    // A theorem, a definition of a snapshot the code builds on now, and
    // a private name as written, which the error says how to name.
    for (name, hint) in [
        ("t", ""),
        ("bar", ""),
        ("nope", ""),
        ("h", " (for a private definition, write _private.h)"),
    ] {
        let allowed = replacing(old.clone(), allow(&["f", name]));
        let error = allow_error(&allowed, &swapped).unwrap();
        assert_eq!(error.kind, ErrorKind::BadRequest);
        let message = format!(
            "allow names {name}, which isn't a definition or inductive type of the replaced snapshot or of a \
             snapshot the code no longer builds on{hint}"
        );
        assert_eq!(error.message, message);
    }
}

#[test]
fn a_private_definition_is_allowed_by_its_name_without_its_module() {
    let h = |m: &str, value: &str| {
        decl(
            &format!("_private.PisaSnap.s{m}.0.h"),
            "1",
            Some(value),
            &[],
        )
    };
    let none = Swapped::default();
    // The replaced module's own: as the comparison names it, or as a
    // refusal does.
    let old = vec![h("a", "2")];
    let now = [h("b", "3")];
    for name in ["_private.h", "_private.PisaSnap.sa.0.h"] {
        let allowed = replacing(old.clone(), allow(&[name]));
        assert!(allow_error(&allowed, &none).is_none(), "{name}");
        let r = compare(&allowed, &now, &none).unwrap();
        assert_eq!(r.changed_definitions, ["_private.PisaSnap.sa.0.h"]);
    }
    let bare = replacing(old.clone(), allow(&["h"]));
    assert!(allow_error(&bare, &none).is_some());
    assert_eq!(
        compare(&bare, &now, &none),
        Err(vec![
            "statement: _private.PisaSnap.sa.0.h has a different value".to_string()
        ])
    );

    // One the comparison reaches in a snapshot the code no longer builds
    // on.
    let thm = vec![using(
        decl("thm3", "9", None, &[]),
        &["_private.PisaSnap.sa.0.h"],
        &[],
    )];
    let swapped = Swapped {
        lost: vec![("a".into(), vec![h("a", "2")])],
        gained: vec![("b".into(), vec![h("b", "3")])],
    };
    let allowed = replacing(thm.clone(), allow(&["_private.h"]));
    assert!(allow_error(&allowed, &swapped).is_none());
    let r = compare(&allowed, &thm, &swapped).unwrap();
    assert_eq!(r.changed_definitions, ["_private.PisaSnap.sa.0.h"]);
    assert_eq!(r.affected, ["thm3"]);
}

#[test]
fn a_swapped_snapshot_passes_when_nothing_the_statements_use_changed() {
    let foo = || using(decl("foo", "1", Some("2"), &[]), &[], &["bar"]);
    let bar = |value: &str| using(decl("bar", "3", Some(value), &[]), &[], &[]);
    let (old, swapped) = swap(Some(foo()), bar("4"));
    assert!(compare(&replacing(old.clone(), None), &old, &swapped).is_ok());

    // `bar` changed under `foo`, which `thm3` names.
    let (old, swapped) = swap(Some(foo()), bar("5"));
    let reason = format!(
        "statement: bar, from {}, has a different value",
        snapshot::module("a")
    );
    assert_eq!(
        compare(&replacing(old.clone(), None), &old, &swapped),
        Err(vec![reason])
    );
    let allowed = replacing(old.clone(), allow(&["bar"]));
    let r = compare(&allowed, &old, &swapped).unwrap();
    assert_eq!(
        (r.changed_definitions, r.affected),
        (vec!["bar".into()], vec!["thm3".into()])
    );

    let (old, swapped) = swap(None, bar("4"));
    let reason = format!("statement: foo, from {}, is missing", snapshot::module("a"));
    assert_eq!(
        compare(&replacing(old.clone(), None), &old, &swapped),
        Err(vec![reason])
    );
}

#[test]
fn a_replacement_cant_assume_what_it_should_prove() {
    let axiom = |name: &str, ty: &str| Declaration {
        kind: "axiom".into(),
        ..using(decl(name, ty, None, &[]), &[], &[])
    };
    let old = vec![using(decl("lem2", "1", None, &["sorryAx"]), &[], &[])];
    let skeleton = || replacing(old.clone(), None);
    let none = Swapped::default();
    // The theorem itself, assumed.
    let err = compare(&skeleton(), &[axiom("lem2", "1")], &none).unwrap_err();
    assert_eq!(
        err,
        [
            "statement: lem2 is a different kind of declaration",
            "statement: the code adds the assumption lem2"
        ]
    );
    // An assumption of its own, or of a snapshot it now builds on.
    let proof = using(decl("lem2", "1", None, &["helper"]), &[], &[]);
    let err = compare(&skeleton(), &[axiom("helper", "1"), proof.clone()], &none);
    assert_eq!(
        err.unwrap_err(),
        ["statement: the code adds the assumption helper"]
    );
    let gained = Swapped {
        lost: Vec::new(),
        gained: vec![("x".into(), vec![axiom("helper", "1")])],
    };
    let err = compare(&skeleton(), std::slice::from_ref(&proof), &gained).unwrap_err();
    assert_eq!(err, ["statement: the code adds the assumption helper"]);

    // Assumptions it already had are kept, with their statements.
    let assumed = vec![axiom("kunneth", "5"), old[0].clone()];
    let with = replacing(assumed.clone(), None);
    let kept = [
        axiom("kunneth", "5"),
        using(decl("lem2", "1", None, &["kunneth"]), &[], &[]),
    ];
    assert!(compare(&with, &kept, &none).is_ok());
    let moved = Swapped {
        lost: vec![("a".into(), vec![axiom("other", "6")])],
        gained: vec![("b".into(), vec![axiom("other", "7")])],
    };
    let err = compare(&with, &kept, &moved).unwrap_err();
    assert!(
        err.contains(&"statement: the assumption other has a different statement".to_string()),
        "{err:?}"
    );
    // What Lean declares, such as `native_decide`'s axiom, isn't one.
    let native = [
        axiom("lem2._native.native_decide.ax_1_1", "8"),
        old[0].clone(),
    ];
    assert!(compare(&skeleton(), &native, &none).is_ok());
}

#[test]
fn another_modules_private_assumption_isnt_the_specs_own() {
    let axiom = |name: &str, ty: &str| Declaration {
        kind: "axiom".into(),
        ..using(decl(name, ty, None, &[]), &[], &[])
    };
    // The spec assumes its own private `a : True`; the head keeps it, but
    // proves `t` from a snapshot it newly builds on whose private `a` is
    // `False`: a private name of another module isn't the spec's.
    let spec = vec![
        axiom("_private.PisaSnap.sS.0.a", "true"),
        using(decl("t", "1", None, &["sorryAx"]), &[], &[]),
    ];
    let now = [
        axiom("_private.PisaSnap.sH.0.a", "true"),
        using(
            decl("t", "1", None, &["_private.PisaSnap.sG.0.a"]),
            &[],
            &[],
        ),
    ];
    let gained = Swapped {
        lost: Vec::new(),
        gained: vec![("G".into(), vec![axiom("_private.PisaSnap.sG.0.a", "false")])],
    };
    assert_eq!(
        compare(&replacing(spec.clone(), None), &now, &gained).unwrap_err(),
        ["statement: the code adds the assumption _private.PisaSnap.sG.0.a"]
    );
    // Its own private `a` is the spec's.
    assert!(compare(&replacing(spec, None), &now, &Swapped::default()).is_ok());
}

#[test]
fn a_spec_permits_its_own_private_assumptions_on_a_heads_own_module() {
    let mut permitted = Permitted::default();
    // The spec module's own, and one of a module it builds on.
    permitted.add("_private.PisaSnap.sS.0.a", true);
    permitted.add("kun", true);
    permitted.add("_private.PisaSnap.sB.0.b", false);
    assert!(permitted.permits("_private.PisaSnap.sH.0.a", "H"));
    assert!(permitted.permits("kun", "H"));
    assert!(permitted.permits("_private.PisaSnap.sB.0.b", "H"));
    // Another module's private `a` isn't the spec's own.
    assert!(!permitted.permits("_private.PisaSnap.sG.0.a", "H"));
    // Nor is the private `b` of what the spec builds on a head's own.
    assert!(!permitted.permits("_private.PisaSnap.sH.0.b", "H"));
}

#[test]
fn a_swap_that_cant_be_followed_is_refused() {
    // A private name the new side has twice.
    let private = |m: &str| {
        using(
            decl(&format!("_private.PisaSnap.s{m}.0.h"), "5", None, &[]),
            &[],
            &[],
        )
    };
    let old = vec![using(
        decl("thm3", "9", None, &[]),
        &["_private.PisaSnap.sa.0.h"],
        &[],
    )];
    let swapped = Swapped {
        lost: vec![("a".into(), vec![private("a")])],
        gained: vec![
            ("b".into(), vec![private("b")]),
            ("c".into(), vec![private("c")]),
        ],
    };
    let err = compare(&replacing(old.clone(), None), &old, &swapped);
    assert!(err.unwrap_err()[0].contains("more than one"));
    // Once, it is found under its new module.
    let swapped = Swapped {
        gained: vec![("b".into(), vec![private("b")])],
        ..swapped
    };
    assert!(compare(&replacing(old.clone(), None), &old, &swapped).is_ok());
}

#[test]
fn a_level_is_one_more_than_the_highest_import() {
    // b under c and d, e on c and d, and f on b and e.
    let above = BTreeMap::from(
        [
            ("c", vec!["b"]),
            ("d", vec!["b"]),
            ("e", vec!["c", "d"]),
            ("f", vec!["b", "e"]),
        ]
        .map(|(m, i)| (m.to_string(), i.into_iter().map(String::from).collect())),
    );
    let levels = levels(&above, "b");
    let level = |m: &str| levels[m];
    assert_eq!(
        [level("b"), level("c"), level("d"), level("e"), level("f")],
        [0, 1, 1, 2, 3]
    );
}

#[test]
fn an_item_comes_after_what_it_imports() {
    // 0 and 1 on `from`'s module; 2 on 0's module, and 3 on 1's and 2's.
    let imports = [vec![], vec![], vec![0], vec![1, 2]];
    assert_eq!(waves(&imports), [1, 1, 2, 3]);
}

#[test]
fn a_rebuild_needs_what_has_the_labels_and_what_links_it_to_the_root() {
    // b under c and d, e on c, f on d; x on b links y to it.
    let above = BTreeMap::from(
        [
            ("c", vec!["b"]),
            ("d", vec!["b"]),
            ("e", vec!["c"]),
            ("f", vec!["d"]),
            ("x", vec!["b"]),
            ("y", vec!["x"]),
        ]
        .map(|(m, i)| (m.to_string(), i.into_iter().map(String::from).collect())),
    );
    let needed = |matching: &[&str]| -> Vec<String> {
        needed(&above, matching.iter().map(|m| m.to_string()))
            .into_iter()
            .collect()
    };
    assert_eq!(needed(&["e"]), ["c", "e"]);
    assert_eq!(needed(&["e", "y"]), ["c", "e", "x", "y"]);
    assert!(
        needed(&["b", "nothing"]).is_empty(),
        "the root isn't rebuilt"
    );
}

#[test]
fn uses_say_what_comes_to_use_a_name() {
    let declared = [
        declared("g", "def", &["Nat"], &["f"]),
        declared("h", "def", &["Nat"], &["g"]),
        declared("t", "theorem", &["h"], &["f"]),
        declared("u", "theorem", &["Nat"], &["f"]),
    ];
    let used = HashSet::from(["f".to_string()]);
    let (users, theorems) = uses(&used, &declared);
    assert_eq!(
        (users, theorems),
        (vec!["g".into(), "h".into()], vec!["t".into()])
    );
}

#[test]
fn affected_theorems_use_a_changed_definition_directly_or_through_others() {
    let declared = [
        declared("f", "def", &["Nat"], &["Nat.add"]),
        declared("g", "def", &["Nat"], &["f"]),
        declared("h", "def", &["Nat"], &["Nat.mul"]),
        declared("direct", "theorem", &["f", "Eq"], &[]),
        declared("through", "theorem", &["g"], &[]),
        declared("unrelated", "theorem", &["h"], &["f"]),
    ];
    let affected = |changed: &str| uses(&HashSet::from([changed.to_string()]), &declared).1;
    assert_eq!(affected("f"), ["direct", "through"]);
    assert!(affected("nothing").is_empty());
}

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
    assert!(s.list(&filters(64), 10, None, false).await.is_ok());
    let error = s.list(&filters(65), 10, None, false).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
}

/// A snapshot id of another version, which a router routes elsewhere, or
/// a bare entry, is refused here too.
#[tokio::test]
async fn an_id_of_another_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let s = snapshots(Db::memory().await, dir.path());
    let entry = "0".repeat(32);
    let error = s.get(&format!("w~{entry}")).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
    assert!(error.message.contains("belongs to version w"), "{error:?}");
    let error = s.get(&entry).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
    let error = s.get(&s.id(&entry)).await.unwrap_err();
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

/// A spec snapshot's module and its head's, as an audit judges them,
/// with one record or the other gone, as when a snapshot is deleted
/// during the audit: what can't be judged isn't accepted.
#[tokio::test]
async fn an_audit_target_without_its_records_is_not_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let s = snapshots(Db::memory().await, dir.path());
    let entry = |module: &str| EntryMeta {
        module: module.into(),
        labels: Default::default(),
        key: None,
        created_micros: 0,
        replaces: None,
        replacement: None,
        holes: Vec::new(),
    };
    // Each module declares `t`, proved and verified.
    let module = |m: &str| ModuleMeta {
        verified: true,
        result: CheckResult {
            declarations: vec![decl("t", "1", None, &[])],
            ..Default::default()
        },
        ..built(m, b"")
    };
    let specs = [("spec".to_string(), entry("s"))];
    let reached = [vec!["head".to_string()]];
    let heads = [("head".to_string(), "h".to_string())];
    // Their comparison found the statements kept.
    let compared = [AuditComparison {
        spec: s.id("spec"),
        head: s.id("head"),
        reasons: Vec::new(),
        changes: Vec::new(),
        replacement: Some(Replacement::default()),
    }];
    let judged = |modules: &[&str]| {
        let metas = modules.iter().map(|m| (m.to_string(), module(m))).collect();
        s.targets(
            &specs,
            &reached,
            &heads,
            &metas,
            &Permitted::default(),
            &compared,
        )
    };
    let (targets, accepted) = judged(&["s", "h"]);
    assert_eq!((targets.len(), accepted), (1, true));
    for left in ["s", "h"] {
        assert_eq!(judged(&[left]), (Vec::new(), false), "only {left}");
    }
}
