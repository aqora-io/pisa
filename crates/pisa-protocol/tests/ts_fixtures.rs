//! Writes every request and response type, as the server serializes it, into
//! `web/packages/client/test/wire.fixtures.ts`, each checked with `satisfies`
//! against the TypeScript client's types, so that `pnpm typecheck` there
//! catches fields that drifted, and enums that gained or lost variants. Each type is written with every optional field
//! set and with none. The structs are built field by field, so a new field
//! breaks this test until it has a value here.
//!
//! The test fails when the file is out of date; run it with
//! `PISA_UPDATE_FIXTURES=1` to write it.

use std::collections::BTreeMap;
use std::fmt::Write;

use pisa_protocol::api::{
    BatchItem, BatchRequest, BatchResponse, CheckRequest, CheckResult, Deleted, Detail, Error,
    ErrorKind, ErrorResponse, LabelsRequest, Message, Pos, SessionCheckRequest, Severity,
    SnapshotFailure, SnapshotInfo, SnapshotList, SnapshotRequest, SnapshotResponse,
    SnapshotSummary, SnapshotVerified, Sorry, StartRequest, StartResponse, StateInfo, TacticInfo,
    TacticRequest, TacticResponse,
};
use pisa_protocol::search::{
    DeclDetail, DeclResponse, Flags, Hit, Informal, Query, QueryResult, Resolution, ResolveRequest,
    ResolveResponse, SearchRequest, SearchResponse, SnapshotHit, SnapshotScope, Suggestion,
};
use pisa_protocol::verify::Declaration;
use serde::Serialize;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../web/packages/client/test/wire.fixtures.ts"
);

const KINDS: [ErrorKind; 10] = [
    ErrorKind::Timeout,
    ErrorKind::OutOfMemory,
    ErrorKind::Crash,
    ErrorKind::Policy,
    ErrorKind::Lean,
    ErrorKind::Capacity,
    ErrorKind::Unavailable,
    ErrorKind::BadRequest,
    ErrorKind::NotFound,
    ErrorKind::Internal,
];

/// Breaks the build when `ErrorKind` gains a variant `KINDS` lacks.
#[allow(dead_code)]
fn every_kind_is_listed(k: ErrorKind) {
    match k {
        ErrorKind::Timeout
        | ErrorKind::OutOfMemory
        | ErrorKind::Crash
        | ErrorKind::Policy
        | ErrorKind::Lean
        | ErrorKind::Capacity
        | ErrorKind::Unavailable
        | ErrorKind::BadRequest
        | ErrorKind::NotFound
        | ErrorKind::Internal => {}
    }
}

const SEVERITIES: [Severity; 4] = [
    Severity::Trace,
    Severity::Info,
    Severity::Warning,
    Severity::Error,
];

#[allow(dead_code)]
fn every_severity_is_listed(s: Severity) {
    match s {
        Severity::Trace | Severity::Info | Severity::Warning | Severity::Error => {}
    }
}

#[allow(dead_code)]
fn every_detail_is_listed(d: Detail) {
    match d {
        Detail::Tactics => {}
    }
}

#[allow(dead_code)]
fn every_scope_is_listed(s: SnapshotScope) {
    match s {
        SnapshotScope::None | SnapshotScope::All | SnapshotScope::Labels(_) => {}
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

fn labels() -> BTreeMap<String, String> {
    BTreeMap::from([(s("run"), s("7"))])
}

fn pos() -> Pos {
    Pos { line: 1, column: 2 }
}

fn error() -> Error {
    Error {
        kind: ErrorKind::Lean,
        message: s("failed"),
    }
}

fn message(full: bool) -> Message {
    Message {
        severity: Severity::Info,
        pos: pos(),
        end_pos: full.then(pos),
        data: s("Nat : Type"),
    }
}

fn check_result(full: bool) -> CheckResult {
    CheckResult {
        id: full.then(|| s("a")),
        ok: full,
        time: 0.25,
        error: full.then(error),
        messages: vec![message(full)],
        sorries: vec![Sorry {
            goal: s("⊢ True"),
            pos: full.then(pos),
            end_pos: full.then(pos),
        }],
        reasons: vec![s("axiom: t uses sorryAx")],
        declarations: vec![Declaration {
            name: s("t"),
            kind: s("theorem"),
            axioms: vec![s("propext")],
            type_hash: s("ab"),
            value_hash: full.then(|| s("cd")),
        }],
        tactics: if full {
            vec![TacticInfo {
                tactic: s("simp"),
                goals_before: vec![s("⊢ True")],
                goals_after: vec![],
                pos: pos(),
                end_pos: pos(),
            }]
        } else {
            vec![]
        },
    }
}

fn state(full: bool) -> StateInfo {
    StateInfo {
        state: s("v~h~s"),
        goal: s("⊢ True"),
        pos: full.then(pos),
        end_pos: full.then(pos),
    }
}

fn summary(full: bool) -> SnapshotSummary {
    SnapshotSummary {
        entry: s("e"),
        snapshot: s("PisaSnap.s0"),
        imports: vec![],
        header: s("import PisaSnap.s0"),
        labels: if full { labels() } else { BTreeMap::new() },
        dirty: false,
        verified: full,
        created: 1,
    }
}

fn flags(full: bool) -> Flags {
    Flags {
        uses_sorry: full,
        informal: full,
        deprecated_to: full.then(|| s("New")),
        additive_sibling: full.then(|| s("add")),
        imports: full.then(|| s("import Mathlib")),
    }
}

fn hit(full: bool) -> Hit {
    Hit {
        name: s("Nat.add_comm"),
        kind: s("theorem"),
        library: s("Mathlib"),
        module: s("Mathlib.Algebra"),
        signature: s("Nat.add_comm (n m : ℕ) : n + m = m + n"),
        informal_name: full.then(|| s("commutativity")),
        summary: full.then(|| s("Addition commutes.")),
        flags: flags(full),
        score: 1.5,
        source_url: full.then(|| s("https://example.com")),
        text: s("Nat.add_comm …"),
        snapshot: full.then(|| SnapshotHit {
            entries: vec![s("e")],
            dirty: false,
            verified: true,
        }),
    }
}

fn query(full: bool, snapshots: SnapshotScope) -> Query {
    let opt = |v: &str| full.then(|| s(v));
    Query {
        q: opt("q"),
        nl: opt("nl"),
        latex: opt("latex"),
        pattern: opt("pattern"),
        goal: opt("goal"),
        name: opt("name"),
        libraries: if full { vec![s("Mathlib")] } else { vec![] },
        kinds: if full { vec![s("theorem")] } else { vec![] },
        k: 5,
        include_sorried: full,
        verify: full,
        snapshots,
    }
}

/// Every fixture, as `(name, TypeScript type, value)`.
fn fixtures() -> Vec<(String, &'static str, serde_json::Value)> {
    let mut out = Vec::new();
    let mut add = |name: &str, ty: &'static str, value: &dyn erased::Value| {
        out.push((name.to_string(), ty, value.json()));
    };
    for full in [true, false] {
        let n = |base: &str| format!("{base}{}", if full { "Full" } else { "Empty" });
        add(&n("checkResult"), "CheckResult", &check_result(full));
        add(
            &n("batchResponse"),
            "BatchResponse",
            &BatchResponse {
                results: vec![check_result(full)],
            },
        );
        add(
            &n("startResponse"),
            "StartResponse",
            &StartResponse {
                session: s("v~h~id"),
                states: vec![state(full)],
                messages: vec![message(full)],
            },
        );
        add(
            &n("tacticResponse"),
            "TacticResponse",
            &TacticResponse {
                state: s("v~h~s2"),
                goals: vec![],
                complete: full,
                messages: vec![message(full)],
            },
        );
        add(
            &n("snapshotResponse"),
            "SnapshotResponse",
            &SnapshotResponse {
                entry: full.then(|| s("e")),
                snapshot: full.then(|| s("PisaSnap.s0")),
                header: full.then(|| s("import PisaSnap.s0")),
                version: s("v"),
                dirty: !full,
                verified: full,
                labels: if full { labels() } else { BTreeMap::new() },
                result: check_result(full),
            },
        );
        add(
            &n("snapshotInfo"),
            "SnapshotInfo",
            &SnapshotInfo {
                entry: s("e"),
                snapshot: s("PisaSnap.s0"),
                imports: vec![s("PisaSnap.s1")],
                header: s("import PisaSnap.s0"),
                code: s("def x := 1"),
                labels: if full { labels() } else { BTreeMap::new() },
                dirty: false,
                verified: true,
                result: check_result(full),
                created: 1,
            },
        );
        add(
            &n("snapshotList"),
            "SnapshotList",
            &SnapshotList {
                entries: vec![summary(full)],
                next: full.then(|| s("c")),
            },
        );
        add(
            &n("snapshotVerified"),
            "SnapshotVerified",
            &SnapshotVerified {
                entry: s("e"),
                snapshot: s("PisaSnap.s0"),
                verified: !full,
                dirty: false,
                checked: vec![s("PisaSnap.s0")],
                failure: full.then(|| SnapshotFailure {
                    snapshot: s("PisaSnap.s0"),
                    reason: s("kernel"),
                }),
            },
        );
        add(
            &n("searchResponse"),
            "SearchResponse",
            &SearchResponse {
                env_id: s("e"),
                results: vec![QueryResult {
                    hits: vec![hit(full)],
                    verification: full.then(|| s("unavailable")),
                    warnings: if full { vec![s("slow")] } else { vec![] },
                    dense_pending: full,
                }],
            },
        );
        add(
            &n("resolveResponse"),
            "ResolveResponse",
            &ResolveResponse {
                env_id: s("e"),
                results: vec![Resolution {
                    name: s("x"),
                    exists: !full,
                    deprecated_to: full.then(|| s("y")),
                    suggestions: if full {
                        vec![Suggestion {
                            name: s("y"),
                            kind: s("def"),
                            signature: s("y : Nat"),
                        }]
                    } else {
                        vec![]
                    },
                }],
            },
        );
        add(
            &n("declResponse"),
            "DeclResponse",
            &DeclResponse {
                env_id: s("e"),
                decl: DeclDetail {
                    name: s("x"),
                    kind: s("def"),
                    library: s("Mathlib"),
                    module: s("Mathlib.X"),
                    signature: s("x : Nat"),
                    type_full: s("Nat"),
                    docstring: full.then(|| s("The x.")),
                    informal: full.then(|| Informal {
                        informal_name: s("x"),
                        statement_nl: s("x"),
                        statement_latex: Some(s("x")),
                        aliases: vec![s("ex")],
                        source: s("llm"),
                    }),
                    flags: flags(full),
                    source_url: full.then(|| s("https://example.com")),
                    deps: vec![s("Nat")],
                    used_by: vec![],
                },
            },
        );
        add(
            &n("checkRequest"),
            "CheckRequest",
            &CheckRequest {
                code: s("x"),
                version: full.then(|| s("v")),
                timeout: 60,
                detail: if full { vec![Detail::Tactics] } else { vec![] },
            },
        );
        add(
            &n("batchRequest"),
            "BatchRequest",
            &BatchRequest {
                items: vec![BatchItem {
                    id: s("a"),
                    code: s("x"),
                }],
                version: full.then(|| s("v")),
                timeout: 60,
                detail: if full { vec![Detail::Tactics] } else { vec![] },
            },
        );
        add(
            &n("startRequest"),
            "StartRequest",
            &StartRequest {
                code: s("x"),
                version: full.then(|| s("v")),
                timeout: 60,
            },
        );
        add(
            &n("snapshotRequest"),
            "SnapshotRequest",
            &SnapshotRequest {
                code: s("x"),
                version: full.then(|| s("v")),
                timeout: 60,
                verify: full.then_some(true),
                labels: if full { labels() } else { BTreeMap::new() },
                key: full.then(|| s("k")),
            },
        );
        add(
            &n("labelsRequest"),
            "LabelsRequest",
            &LabelsRequest {
                set: if full { labels() } else { BTreeMap::new() },
                remove: if full { vec![s("old")] } else { vec![] },
            },
        );
        add(
            &n("searchRequest"),
            "SearchRequest",
            &SearchRequest {
                queries: vec![
                    query(full, SnapshotScope::None),
                    query(full, SnapshotScope::All),
                    query(full, SnapshotScope::Labels(labels())),
                ],
                env_id: full.then(|| s("e")),
            },
        );
        add(
            &n("resolveRequest"),
            "ResolveRequest",
            &ResolveRequest {
                names: vec![s("x")],
                env_id: full.then(|| s("e")),
            },
        );
    }
    add(
        "tacticRequest",
        "TacticRequest",
        &TacticRequest {
            tactic: s("simp"),
            timeout: 60,
        },
    );
    add(
        "sessionCheckRequest",
        "SessionCheckRequest",
        &SessionCheckRequest {
            code: s("x"),
            timeout: 60,
        },
    );
    add(
        "deleted",
        "Deleted",
        &Deleted {
            entry: s("e"),
            removed: vec![s("PisaSnap.s0")],
        },
    );
    add(
        "errorResponse",
        "ErrorResponse",
        &ErrorResponse { error: error() },
    );
    out
}

mod erased {
    /// A value to serialize, whatever its type.
    pub trait Value {
        fn json(&self) -> serde_json::Value;
    }
    impl<T: serde::Serialize> Value for T {
        fn json(&self) -> serde_json::Value {
            serde_json::to_value(self).expect("fixtures serialize")
        }
    }
}

fn list<T: Serialize>(name: &str, ty: &str, values: &[T]) -> String {
    let json = serde_json::to_string(values).unwrap();
    format!(
        "const {name} = {json} as const satisfies readonly T.{ty}[];\n\
         const {name}Complete: Exclude<T.{ty}, (typeof {name})[number]> extends never ? true : never = true;\n"
    )
}

fn render() -> String {
    let mut out = String::from(
        "// Written by crates/pisa-protocol/tests/ts_fixtures.rs from the Rust types: don't\n\
         // edit. After changing them, run\n\
         // `PISA_UPDATE_FIXTURES=1 cargo test -p pisa-protocol --test ts_fixtures`.\n\
         import type * as T from \"../src/types.ts\";\n\n",
    );
    let mut names = Vec::new();
    for (name, ty, value) in fixtures() {
        let json = serde_json::to_string_pretty(&value).unwrap();
        writeln!(out, "const {name} = {json} satisfies T.{ty};\n").unwrap();
        names.push(name);
    }
    out.push_str(&list("errorKinds", "ErrorKind", &KINDS));
    out.push_str(&list("severities", "Severity", &SEVERITIES));
    names.extend(["errorKindsComplete", "severitiesComplete"].map(String::from));
    // They are only here to be type-checked.
    writeln!(out, "\nvoid [{}];", names.join(", ")).unwrap();
    out
}

#[test]
fn typescript_fixtures_are_up_to_date() {
    let rendered = render();
    if std::env::var_os("PISA_UPDATE_FIXTURES").is_some() {
        std::fs::write(FIXTURES, &rendered).unwrap();
        return;
    }
    let current = std::fs::read_to_string(FIXTURES).unwrap_or_default();
    assert!(
        current == rendered,
        "{FIXTURES} is out of date: run `PISA_UPDATE_FIXTURES=1 cargo test -p pisa-protocol --test ts_fixtures`"
    );
}
