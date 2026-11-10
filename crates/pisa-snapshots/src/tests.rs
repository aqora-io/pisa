use super::*;
use crate::audit::Permitted;
use pisa_protocol::api::CheckResult;
use pisa_protocol::verify::ReportedDeclaration;

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

/// A module's record with nothing but a build.
fn module_meta(id: &str) -> ModuleMeta {
    ModuleMeta {
        imports: Vec::new(),
        modules: vec![Module {
            id: id.into(),
            build: "b".into(),
            files: Vec::new(),
        }],
        header: String::new(),
        code: String::new(),
        status: ProofStatus::Proved,
        verified: false,
        macros: false,
        result: CheckResult::default(),
        created: 0,
    }
}

/// A spec snapshot's module and its head's, as an audit judges them,
/// with one record or the other gone, as when a snapshot is deleted
/// during the audit: what can't be judged isn't accepted.
#[tokio::test]
async fn an_audit_target_without_its_records_is_not_accepted() {
    let s = Reader::new(Db::memory().await, "v".into(), Vec::new());
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
        ..module_meta(m)
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
