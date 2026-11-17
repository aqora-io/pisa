//! A pair of a run as a Palomar package (`/snapshots/export?profile=palomar`),
//! made from the run's export as the Erdős campaign's
//! `tools/pisa_to_palomar.py` made one.
//!
//! The pair's challenge and solution take the names the request gives, and
//! each module the pair needs becomes a module-system file, as Palomar
//! requires. The package requires Mathlib alone, at the commit the worker's
//! manifest resolved, and `comparator.json` is the pair's configuration. A
//! pair the package can't hold is refused: one whose spec permits an axiom
//! of its own, whose challenge imports more than Lean and Mathlib, or that
//! imports a module that neither the run, Lean, Mathlib nor what Mathlib
//! builds on has, as the worker's Physlib. What pisa can't decide, the
//! licence, the authors, attribution and fidelity to the source, is left to
//! review in `formalization.yaml` and `REVIEW.md`, and `conversion.json`
//! records what changed.

use std::collections::{BTreeMap, BTreeSet};

use pisa_protocol::api::{Error, ErrorKind};
use pisa_protocol::header::{import_parts, split_header};
use pisa_protocol::snapshot;
use pisa_protocol::wire::{ExportedPair, ExportedRun, PalomarExport};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{NAME, STANDARD_AXIOMS, file, hex, pretty, toml};

/// What a Palomar challenge may import, by root: Lean's modules and Mathlib.
const CHALLENGE_ROOTS: [&str; 3] = ["Mathlib", "Init", "Lean"];

/// The roots of Lean's own modules, which every package has.
const CORE_ROOTS: [&str; 4] = ["Init", "Lean", "Std", "Lake"];

/// The roots of the libraries of the packages Mathlib builds on, which its
/// manifest pins, so that a package that requires Mathlib has them:
/// batteries, aesop, Qq (quote4), ProofWidgets4, plausible, import-graph,
/// LeanSearchClient and lean4-cli. A package Mathlib comes to require must
/// be added here, or a pair that imports it is refused.
const MATHLIB_DEPENDENCY_ROOTS: [&str; 8] = [
    "Batteries",
    "Aesop",
    "Qq",
    "ProofWidgets",
    "Plausible",
    "ImportGraph",
    "LeanSearchClient",
    "Cli",
];

/// The one package a Palomar package requires.
const MATHLIB: &str = "mathlib";

/// The root of Mathlib's modules.
const MATHLIB_ROOT: &str = "Mathlib";

/// What metadata pisa can't decide reads until it is reviewed.
const REVIEW: &str = "REVIEW";

/// What makes a plain file a module-system file, as `conversion.json` lists
/// it.
const HEADER_STEPS: [&str; 3] = [
    "prepend module",
    "make header imports public",
    "add exposed public section",
];

/// Pair `job.pair` of `run`, the export of the run that spec snapshots
/// `against` start with `allow`, as a Palomar package.
pub(super) fn package(
    run: &ExportedRun,
    job: &PalomarExport,
    against: &[String],
    allow: &[String],
) -> Result<ExportedRun, Error> {
    let (n, pair) = pick(&run.pairs, job.pair)?;
    let text = |path: &str| {
        let file = run.files.iter().find(|f| f.path == path);
        file.map_or("", |f| f.text.as_str())
    };
    let config: Value = serde_json::from_str(text(&pair.config)).unwrap_or_default();
    let assumed: Vec<&str> = (config["permitted_axioms"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .filter(|a| !STANDARD_AXIOMS.contains(a))
        .collect();
    if !assumed.is_empty() {
        return Err(bad(format!(
            "pair {n} permits {}, which its spec assumes: a Palomar package permits only {}",
            assumed.join(", "),
            STANDARD_AXIOMS.join(", ")
        )));
    }
    let (challenge, solution) = (&pair.challenge_module, &pair.solution_module);
    if challenge == solution {
        return Err(bad(format!(
            "pair {n}'s spec is its own head: nothing replaces {} yet, so there is no solution \
             to package",
            pair.spec
        )));
    }
    // The export's modules, by name, with their code.
    let modules: BTreeMap<String, &str> = (run.files.iter())
        .filter_map(|f| {
            Some((
                f.path.strip_suffix(".lean")?.replace('/', "."),
                f.text.as_str(),
            ))
        })
        .collect();
    let code = |m: &str| modules.get(m).copied().unwrap_or_default();
    if let Some(m) = (imports(code(challenge)).into_iter()).find(|m| !under(m, &CHALLENGE_ROOTS)) {
        return Err(bad(format!(
            "the challenge, {challenge}, imports {m}: a Palomar challenge imports only Mathlib, \
             Init and Lean"
        )));
    }
    // The challenge, the solution, and what they import of the export.
    let mut needed = BTreeSet::new();
    let mut pending = vec![challenge.clone(), solution.clone()];
    while let Some(m) = pending.pop() {
        if needed.contains(&m) {
            continue;
        }
        for import in imports(code(&m)) {
            if modules.contains_key(&import) {
                pending.push(import);
            } else if !importable(&import) {
                return Err(bad(format!(
                    "{m} imports {import}, which a package that requires Mathlib alone doesn't \
                     have: a Palomar package imports only its own modules, Lean's, Mathlib and \
                     what Mathlib builds on"
                )));
            }
        }
        needed.insert(m);
    }
    let names = BTreeMap::from([
        (challenge.clone(), job.challenge_module.clone()),
        (solution.clone(), job.solution_module.clone()),
    ]);
    let mut kept = needed.iter().filter(|m| !names.contains_key(*m));
    if let Some(taken) = kept.find(|m| names.values().any(|n| n == *m)) {
        return Err(bad(format!(
            "the package has a module {taken} already: name the challenge and the solution \
             otherwise"
        )));
    }
    let named = |m: &str| names.get(m).cloned().unwrap_or_else(|| m.to_string());
    let mut files = Vec::new();
    let mut conversions = Vec::new();
    for m in &needed {
        let (module, steps) = moduleize(code(m));
        let output = snapshot::rename_modules(&module, |word| names.get(&plain(word)).cloned());
        conversions.push(json!({
            "source_module": m,
            "output_module": named(m),
            "header_steps": steps,
            "source_sha256": sha256(code(m)),
            "output_sha256": sha256(&output),
        }));
        files.push(file(
            &format!("{}.lean", named(m).replace('.', "/")),
            output,
        ));
    }
    let run_name = run_name(pair);
    let project = run_name.map_or_else(|| NAME.to_string(), |r| r.replace('.', "_"));
    let worker = serde_json::from_str(text("lake-manifest.json")).unwrap_or_default();
    let (manifest, removed) = manifest(worker, &project)?;
    let mut packages = manifest["packages"].as_array().into_iter().flatten();
    let mathlib = packages.find(|p| p["name"] == MATHLIB);
    let mut targets: Vec<String> = [&job.challenge_module, &job.solution_module]
        .iter()
        .map(|m| root(m).to_string())
        .collect();
    targets.dedup();
    let outputs: Vec<String> = needed.iter().map(|m| named(m)).collect();
    if let Some(toolchain) = run.files.iter().find(|f| f.path == "lean-toolchain") {
        files.push(toolchain.clone());
    }
    files.push(file(
        "lakefile.toml",
        lakefile(&project, &targets, mathlib, &outputs),
    ));
    files.push(file("lake-manifest.json", pretty(&manifest)));
    let mut comparator = serde_json::Map::new();
    for key in [
        "theorem_names",
        "definition_names",
        "permitted_axioms",
        "enable_nanoda",
    ] {
        if let Some(value) = config.get(key) {
            comparator.insert(key.into(), value.clone());
        }
    }
    comparator.insert(
        "challenge_module".into(),
        job.challenge_module.clone().into(),
    );
    comparator.insert("solution_module".into(), job.solution_module.clone().into());
    files.push(file("comparator.json", pretty(&Value::Object(comparator))));
    let theorems = config["theorem_names"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let metadata = formalization(run_name, &theorems, &job.solution_module);
    files.push(file("formalization.yaml", pretty(&metadata)));
    files.push(file("REVIEW.md", review(n, pair, job)));
    let conversion = json!({
        "against": against,
        "allow": allow,
        "pair": n,
        "spec": pair.spec,
        "head": pair.head,
        "conversions": conversions,
        "dependencies_removed": removed,
        "dependency_revisions": "lakefile.toml requires Mathlib at the commit the worker's \
            manifest resolved it to, and each package of lake-manifest.json has its commit as \
            its inputRev",
        "limitations": [
            "The new headers and module names change what Lean builds: build the package, \
             judge it with comparator, and cross-check it in pisa.",
            "The licence, the authors, attribution and fidelity to the source are left to \
             review: see REVIEW.md.",
        ],
    });
    files.push(file("conversion.json", pretty(&conversion)));
    let packaged = ExportedPair {
        challenge_module: job.challenge_module.clone(),
        solution_module: job.solution_module.clone(),
        config: "comparator.json".into(),
        ..pair.clone()
    };
    Ok(ExportedRun {
        files,
        pairs: vec![packaged],
        forks: run.forks.clone(),
    })
}

fn bad(message: String) -> Error {
    Error::new(ErrorKind::BadRequest, message)
}

/// Pair `pair` of `pairs`, from 1 as `comparator/<n>.json` numbers them,
/// with its number; without one, the only pair.
fn pick(pairs: &[ExportedPair], pair: Option<u32>) -> Result<(usize, &ExportedPair), Error> {
    let count = pairs.len();
    match (pair, pairs) {
        (None, [only]) => Ok((1, only)),
        (None, _) => Err(bad(format!(
            "the run has {count} pairs: name the one to package with pair, from 1 as \
             comparator/<n>.json numbers them"
        ))),
        (Some(n), _) => {
            let n = n as usize;
            match n.checked_sub(1).and_then(|i| pairs.get(i)) {
                Some(p) => Ok((n, p)),
                None => Err(bad(format!(
                    "pair is {n}, and the run's pairs are numbered 1 to {count}"
                ))),
            }
        }
    }
}

/// The run that names a pair's modules in the export, if it is named.
fn run_name(pair: &ExportedPair) -> Option<&str> {
    let head =
        (pair.solution_module.strip_prefix(snapshot::RUNS)).and_then(|r| r.strip_prefix('.'));
    head.or_else(|| {
        pair.challenge_module
            .strip_prefix(snapshot::SPECS)?
            .strip_prefix('.')
    })
}

/// The modules `code`'s header imports, as their files are named.
fn imports(code: &str) -> Vec<String> {
    let header = split_header(code).header;
    (header.lines())
        .filter_map(import_parts)
        .flat_map(|(_, modules)| modules.into_iter().map(plain).collect::<Vec<_>>())
        .collect()
}

/// A module name without `«»`.
fn plain(module: &str) -> String {
    module.chars().filter(|c| !matches!(c, '«' | '»')).collect()
}

/// Whether a package that requires Mathlib alone has `module`: Lean's,
/// Mathlib's, or one of a package Mathlib builds on.
fn importable(module: &str) -> bool {
    under(module, &CORE_ROOTS)
        || under(module, &[MATHLIB_ROOT])
        || under(module, &MATHLIB_DEPENDENCY_ROOTS)
}

/// Whether `module` is one of `roots` or under one.
fn under(module: &str, roots: &[&str]) -> bool {
    roots.iter().any(|r| {
        (module.strip_prefix(r)).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    })
}

fn root(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}

fn sha256(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// `code` as a module-system file, and the steps that made it one: a plain
/// file starts with `module`, imports publicly what it imported, and has
/// `@[expose] public section` after its imports, so that what it declares
/// stays visible, definitions' values included. A module-system file stays
/// as it is.
fn moduleize(code: &str) -> (String, Vec<&'static str>) {
    let split = split_header(code);
    if split.header.lines().next() == Some("module") {
        return (code.to_string(), Vec::new());
    }
    let lines: Vec<&str> = code.split('\n').collect();
    let (header, body) = lines.split_at(split.header_lines as usize);
    let mut out = vec!["module".to_string()];
    out.extend(header.iter().map(|line| public(line)));
    out.extend(["".to_string(), "@[expose] public section".to_string()]);
    out.extend(body.iter().map(|line| line.to_string()));
    (out.join("\n"), HEADER_STEPS.to_vec())
}

/// A plain file's header line, an import made public; any other line as it
/// is.
fn public(line: &str) -> String {
    let code = line.split("--").next().unwrap_or(line);
    match import_parts(code) {
        Some((modifiers, _)) if modifiers.starts_with("import") => {
            let at = line.len() - line.trim_start().len();
            format!("{}public {}", &line[..at], &line[at..])
        }
        _ => line.to_string(),
    }
}

/// The worker project's `manifest` as package `name`'s: Mathlib and what it
/// inherits, each with the commit it resolved to as its `inputRev`, and the
/// names of the packages it leaves out. Each must be a GitHub repository's,
/// at a commit, which the worker's project decides, not the request.
fn manifest(mut manifest: Value, name: &str) -> Result<(Value, Vec<String>), Error> {
    manifest["name"] = name.into();
    let packages = manifest["packages"].as_array().cloned().unwrap_or_default();
    let (mut kept, removed): (Vec<Value>, Vec<Value>) =
        (packages.into_iter()).partition(|p| p["inherited"] == true || p["name"] == MATHLIB);
    for p in &mut kept {
        let field = |key: &str| p[key].as_str().unwrap_or_default().to_string();
        let (package, rev, url) = (field("name"), field("rev"), field("url"));
        let commit = rev.len() == 40 && rev.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        let refused = if p["type"] != "git" || !commit {
            Some(format!(
                "the worker's project has {package}, which isn't a git repository at a commit"
            ))
        } else if !github(&url) {
            Some(format!(
                "the worker's project has {package} from {url}, which isn't a GitHub repository"
            ))
        } else {
            None
        };
        if let Some(refused) = refused {
            return Err(Error::new(
                ErrorKind::Internal,
                format!("{refused}: a Palomar package takes each package from one"),
            ));
        }
        p["inputRev"] = rev.into();
    }
    manifest["packages"] = kept.into();
    let removed = (removed.iter())
        .filter_map(|p| p["name"].as_str().map(String::from))
        .collect();
    Ok((manifest, removed))
}

/// Whether `url` is a GitHub repository's, `https://github.com/<owner>/<name>`.
fn github(url: &str) -> bool {
    let parts: Vec<&str> = (url.strip_prefix("https://github.com/").into_iter())
        .flat_map(|path| path.split('/'))
        .collect();
    parts.len() == 2 && parts.iter().all(|p| !p.is_empty())
}

/// The package's lakefile: package `name`, which builds `targets` by
/// default and requires `mathlib`, the manifest's, at its commit, with a
/// library for each root of `modules`, built with `autoImplicit` off as
/// pisa builds them.
fn lakefile(name: &str, targets: &[String], mathlib: Option<&Value>, modules: &[String]) -> String {
    let targets: Vec<String> = targets.iter().map(|t| toml(t)).collect();
    let mut out = format!(
        "name = {}\nversion = \"0.1.0\"\ndefaultTargets = [{}]\n\
         leanOptions = {{ autoImplicit = false }}\n",
        toml(name),
        targets.join(", ")
    );
    if let Some(mathlib) = mathlib {
        let field = |key: &str| toml(mathlib[key].as_str().unwrap_or_default());
        out.push_str(&format!(
            "\n[[require]]\nname = {}\ngit = {}\nrev = {}\n",
            toml(MATHLIB),
            field("url"),
            field("rev")
        ));
    }
    let mut libraries: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for m in modules {
        libraries.entry(root(m)).or_default().push(m);
    }
    for (root, under) in libraries {
        out.push_str(&format!("\n[[lean_lib]]\nname = {}\n", toml(root)));
        // A library is its root module, unless it has others.
        if under != [root] {
            let glob = toml(&format!("{root}.+"));
            out.push_str(&format!("globs = [{glob}]\n"));
        }
    }
    out
}

/// `formalization.yaml`, as JSON, which YAML reads, with every key of the
/// Palomar v0.4 file the Erdős 882 package has: the run's name and the
/// theorems `solution` proves, and in `status` what `comparator.json` makes
/// true of a solution it accepts, no `sorry` and no axiom beyond the
/// standard three. The rest, which pisa can't decide, reads `REVIEW`.
fn formalization(run: Option<&str>, theorems: &[Value], solution: &str) -> Value {
    let statements: Vec<Value> = (theorems.iter())
        .map(|t| {
            json!({"source": REVIEW, "lean": t, "module": solution, "status": REVIEW,
                   "note": REVIEW})
        })
        .collect();
    json!({
        "version": "v0.4",
        "project": {
            "name": run.unwrap_or(REVIEW),
            "description": REVIEW,
            "authors": [REVIEW],
            "responsible_maintainers": [REVIEW],
            "license": REVIEW,
        },
        "classification": {"arxiv": [REVIEW], "msc2020": [REVIEW]},
        "sources": [{
            "title": REVIEW,
            "authors": [REVIEW],
            "type": REVIEW,
            "id": REVIEW,
            "location": REVIEW,
            "relationship": REVIEW,
            "author_endorsement": REVIEW,
            "license": REVIEW,
            "note": REVIEW,
        }],
        "related_formalizations": [{"id": REVIEW, "relationship": REVIEW, "note": REVIEW}],
        "status": {"scope": REVIEW, "sorry_count": 0, "sorry_in_definitions": 0, "axioms": []},
        "automation": {
            "methods": [{
                "method": REVIEW,
                "models": [REVIEW],
                "framework": REVIEW,
                "tool_setup": REVIEW,
                "cost": {"wall_time": REVIEW, "spend_usd": REVIEW, "hardware": REVIEW},
                "prompting_notes": REVIEW,
            }],
            "spend_usd": REVIEW,
            "notes": REVIEW,
        },
        "fidelity": {"divergences": REVIEW},
        "review": {"status": REVIEW, "notes": REVIEW},
        "alignment": {"namespace": REVIEW, "statements": statements},
    })
}

/// `REVIEW.md`: where the package comes from, and what to decide before it
/// is published.
fn review(n: usize, pair: &ExportedPair, job: &PalomarExport) -> String {
    let (challenge, solution) = (&job.challenge_module, &job.solution_module);
    format!(
        "# Before this package is published\n\n\
         pisa made it from pair {n} of a run: spec snapshot `{}` is `{challenge}`, and its head \
         `{}` is `{solution}`. The code is pisa's, with module-system headers and new module \
         names, as `conversion.json` records. What pisa can't decide is left here, and `REVIEW` \
         marks it in `formalization.yaml`:\n\n\
         - **Licence.** `project.license`: Palomar takes Apache-2.0, with its `LICENSE` file.\n\
         - **Authors.** `project.authors` and `project.responsible_maintainers`.\n\
         - **Description, classification and sources.** `project.description`, \
         `classification` and `sources`; `project.name` is the run's name, if it has one.\n\
         - **Attribution.** Statements, definitions and proofs taken from elsewhere: name each \
         source and its licence in `sources` or `related_formalizations`, with a third-party \
         notice and the licence's text where it asks for them.\n\
         - **Source fidelity.** That `{challenge}` states what its source states: each \
         definition and theorem statement against the source's, with any change in \
         `fidelity.divergences`.\n\
         - **The rest of the metadata.** Every other `REVIEW` in `formalization.yaml`: \
         `related_formalizations`, `status.scope`, `automation`, `review`, and in `alignment` \
         its `namespace` and each statement's `source`, `status` and `note`. An entry of a list \
         that has nothing to say goes; a list with none is `[]`. `status`'s counts and axioms \
         are what `comparator.json` makes true of a solution it accepts. Add a `README.md` for \
         the mathematics.\n\
         - **Validation.** The new headers and names change what Lean builds: run \
         `lake exe cache get` and `lake build`, judge `comparator.json` with the Palomar \
         template's comparator, and cross-check the renamed files in pisa with a paranoid \
         acceptance.\n",
        pair.spec, pair.head
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::wire::ExportedFile;

    const HELPER: &str = "PisaSnap.s0123456789abcdef0123456789abcdef";

    /// A plain spec with a copyright comment, a plain head on a helper, and
    /// the helper, a module-system file.
    const SPEC: &str = "/- Copyright -/\nimport Mathlib\n\ndef cost : Nat := sorry\n\
                        theorem t : cost = cost := sorry";
    const HEAD: &str = "import Mathlib\nimport PisaSnap.s0123456789abcdef0123456789abcdef -- help\n\n\
                        def cost : Nat := 3\ntheorem t : cost = cost := rfl";
    const HELPER_CODE: &str =
        "module\n\npublic import Mathlib\n\npublic theorem help : 1 = 1 := rfl";

    /// The 882 worker's: Physlib and Mathlib required, what Mathlib needs
    /// inherited, each resolved to its commit.
    fn worker_manifest() -> Value {
        let git = |name: &str, url: &str, rev: char, input: &str, inherited: bool| {
            json!({"type": "git", "name": name, "url": url, "rev": rev.to_string().repeat(40),
                   "inputRev": input, "inherited": inherited, "subDir": null, "scope": "",
                   "manifestFile": "lake-manifest.json", "configFile": "lakefile.toml"})
        };
        json!({"version": "1.3.0", "packagesDir": ".lake/packages", "name": "pisa_run",
               "lakeDir": ".lake", "fixedToolchain": false, "packages": [
            git("Physlib", "https://github.com/aqora-io/physlib.git", '2', "lean-v4.36.0-rc2", false),
            git("mathlib", "https://github.com/leanprover-community/mathlib4.git", '9', "v4.36.0-rc2", false),
            git("batteries", "https://github.com/leanprover-community/batteries", 'd', "main", true),
        ]})
    }

    fn config(axioms: &[&str]) -> String {
        let config = json!({"challenge_module": "PisaSpec.main", "solution_module": "PisaRun.main",
            "theorem_names": ["t"], "definition_names": ["cost"], "permitted_axioms": axioms,
            "enable_nanoda": true});
        serde_json::to_string_pretty(&config).unwrap()
    }

    fn pair(n: usize, challenge: &str, solution: &str) -> ExportedPair {
        ExportedPair {
            spec: "v~a".into(),
            head: "v~b".into(),
            spec_module: "PisaSnap.sa".into(),
            head_module: "PisaSnap.sb".into(),
            challenge_module: challenge.into(),
            solution_module: solution.into(),
            config: format!("comparator/{n}.json"),
            theorems: 1,
            left_out: Vec::new(),
        }
    }

    /// A named run's export, as `audited_export` makes it: one pair, `main`.
    fn run(spec: &str, head: &str) -> ExportedRun {
        let manifest = serde_json::to_string_pretty(&worker_manifest()).unwrap();
        let files = [
            ("PisaSpec/main.lean", spec.to_string()),
            ("PisaRun/main.lean", head.to_string()),
            (
                &format!("{}.lean", HELPER.replace('.', "/")),
                HELPER_CODE.into(),
            ),
            (
                "comparator/1.json",
                config(&["propext", "Quot.sound", "Classical.choice"]),
            ),
            ("lean-toolchain", "leanprover/lean4:v4.36.0-rc2\n".into()),
            (
                "lakefile.toml",
                "# A pisa run, for `lake comparator`: see README.md.\n".into(),
            ),
            ("lake-manifest.json", manifest),
            ("README.md", "# A pisa run\n".into()),
        ];
        ExportedRun {
            files: (files.into_iter())
                .map(|(path, text)| ExportedFile {
                    path: path.to_string(),
                    text,
                })
                .collect(),
            pairs: vec![pair(1, "PisaSpec.main", "PisaRun.main")],
            forks: Vec::new(),
        }
    }

    fn job(pair: Option<u32>) -> PalomarExport {
        PalomarExport {
            pair,
            challenge_module: "Challenge".into(),
            solution_module: "Solution".into(),
        }
    }

    fn text<'a>(run: &'a ExportedRun, path: &str) -> &'a str {
        let file = run.files.iter().find(|f| f.path == path);
        &file.unwrap_or_else(|| panic!("no {path}")).text
    }

    fn refused(run: &ExportedRun, job: &PalomarExport) -> Error {
        package(run, job, &["v~a".into()], &[]).unwrap_err()
    }

    #[test]
    fn a_plain_file_becomes_a_module_system_file() {
        let (code, steps) = moduleize(SPEC);
        assert_eq!(
            code,
            "module\n/- Copyright -/\npublic import Mathlib\n\n@[expose] public section\n\n\
             def cost : Nat := sorry\ntheorem t : cost = cost := sorry"
        );
        assert_eq!(
            steps,
            [
                "prepend module",
                "make header imports public",
                "add exposed public section"
            ]
        );
        // Its imports' comments and spacing stay; what follows them is the body.
        let (code, _) = moduleize("import A B -- two\n  import C\ntheorem t : True := trivial");
        assert_eq!(
            code,
            "module\npublic import A B -- two\n  public import C\n\n@[expose] public section\n\
             theorem t : True := trivial"
        );
        // A module-system file stays as it is.
        assert_eq!(moduleize(HELPER_CODE), (HELPER_CODE.to_string(), vec![]));
        // So does one with comments before `module`.
        let commented = format!("-- Copyright\n{HELPER_CODE}");
        assert_eq!(moduleize(&commented).0, commented);
    }

    /// Mathlib alone, at the commit the worker's manifest resolved, and what
    /// it inherits, each pinned to its commit: the worker's other packages go.
    #[test]
    fn the_package_requires_mathlib_alone_at_its_commit() {
        let (manifest, removed) = manifest(worker_manifest(), "main").unwrap();
        assert_eq!(removed, ["Physlib"]);
        assert_eq!(manifest["name"], "main");
        let packages = manifest["packages"].as_array().unwrap();
        let names: Vec<&str> = packages
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["mathlib", "batteries"]);
        for p in packages {
            assert_eq!(p["inputRev"], p["rev"], "{p}");
        }
        let mathlib = &packages[0];
        let modules = ["Challenge".to_string(), "Solution".into(), HELPER.into()];
        let targets = ["Challenge".to_string(), "Solution".into()];
        assert_eq!(
            lakefile("main", &targets, Some(mathlib), &modules),
            "name = \"main\"\nversion = \"0.1.0\"\ndefaultTargets = [\"Challenge\", \"Solution\"]\n\
             leanOptions = { autoImplicit = false }\n\n\
             [[require]]\nname = \"mathlib\"\n\
             git = \"https://github.com/leanprover-community/mathlib4.git\"\n\
             rev = \"9999999999999999999999999999999999999999\"\n\n\
             [[lean_lib]]\nname = \"Challenge\"\n\n\
             [[lean_lib]]\nname = \"PisaSnap\"\nglobs = [\"PisaSnap.+\"]\n\n\
             [[lean_lib]]\nname = \"Solution\"\n"
        );
        // Without Mathlib, as a worker without a project has, it requires
        // nothing; a library with modules under its root takes them all.
        let modules = ["Erdos.Challenge".to_string(), "Erdos.Solution".into()];
        let lakefile = lakefile("pisa_run", &["Erdos".into()], None, &modules);
        assert!(!lakefile.contains("[[require]]"), "{lakefile}");
        assert!(
            lakefile.contains("defaultTargets = [\"Erdos\"]\n")
                && lakefile.ends_with("[[lean_lib]]\nname = \"Erdos\"\nglobs = [\"Erdos.+\"]\n"),
            "{lakefile}"
        );
    }

    /// A package pins each package to a commit of a public repository, or
    /// isn't made.
    #[test]
    fn a_package_that_cant_be_pinned_is_refused() {
        let mut unpinned = worker_manifest();
        unpinned["packages"][2]["rev"] = "main".into();
        let mut private = worker_manifest();
        private["packages"][1]["url"] = "git@example.com:mathlib4.git".into();
        let mut local = worker_manifest();
        local["packages"][2] = json!({"type": "path", "name": "local", "dir": "../local",
                                      "inherited": true});
        for (manifest, named) in [
            (unpinned, "batteries"),
            (private, "mathlib"),
            (local, "local"),
        ] {
            let e = super::manifest(manifest, "main").unwrap_err();
            assert_eq!(e.kind, ErrorKind::Internal, "{e:?}");
            assert!(e.message.contains(named), "{e:?}");
        }
    }

    #[test]
    fn a_pair_becomes_a_package() {
        let run = run(SPEC, HEAD);
        let package = package(&run, &job(None), &["v~a".into()], &["g".into()]).unwrap();
        let mut paths: Vec<&str> = package.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        let helper = format!("{}.lean", HELPER.replace('.', "/"));
        assert_eq!(
            paths,
            [
                "Challenge.lean",
                &helper,
                "REVIEW.md",
                "Solution.lean",
                "comparator.json",
                "conversion.json",
                "formalization.yaml",
                "lake-manifest.json",
                "lakefile.toml",
                "lean-toolchain",
            ]
        );
        assert_eq!(text(&package, "Challenge.lean"), moduleize(SPEC).0);
        // The head imports the helper by its name still.
        assert_eq!(text(&package, "Solution.lean"), moduleize(HEAD).0);
        assert_eq!(text(&package, &helper), HELPER_CODE);
        let comparator: Value = serde_json::from_str(text(&package, "comparator.json")).unwrap();
        assert_eq!(
            comparator,
            json!({"challenge_module": "Challenge", "solution_module": "Solution",
                   "theorem_names": ["t"], "definition_names": ["cost"],
                   "permitted_axioms": ["propext", "Quot.sound", "Classical.choice"],
                   "enable_nanoda": true})
        );
        // The run's name names the Lake package.
        let lakefile = text(&package, "lakefile.toml");
        assert!(lakefile.starts_with("name = \"main\"\n"), "{lakefile}");
        assert!(!lakefile.contains("Physlib"), "{lakefile}");
        let manifest: Value = serde_json::from_str(text(&package, "lake-manifest.json")).unwrap();
        assert_eq!(manifest["name"], "main");
        assert!(!manifest.to_string().contains("Physlib"), "{manifest}");
        // What pisa can't decide is left to review.
        let metadata: Value = serde_json::from_str(text(&package, "formalization.yaml")).unwrap();
        assert_eq!(metadata["version"], "v0.4");
        assert_eq!(
            metadata["project"],
            json!({"name": "main", "description": "REVIEW", "authors": ["REVIEW"],
                   "responsible_maintainers": ["REVIEW"], "license": "REVIEW"})
        );
        assert_eq!(
            metadata["classification"],
            json!({"arxiv": ["REVIEW"], "msc2020": ["REVIEW"]})
        );
        assert_eq!(
            metadata["alignment"],
            json!({"namespace": "REVIEW", "statements": [{"source": "REVIEW", "lean": "t",
                   "module": "Solution", "status": "REVIEW", "note": "REVIEW"}]})
        );
        // What comparator.json makes true of a solution it accepts.
        assert_eq!(
            metadata["status"],
            json!({"scope": "REVIEW", "sorry_count": 0, "sorry_in_definitions": 0, "axioms": []})
        );
        let review = text(&package, "REVIEW.md");
        for wanted in [
            "Licence",
            "Attribution",
            "Source fidelity",
            "`v~a`",
            "`v~b`",
        ] {
            assert!(review.contains(wanted), "{wanted}: {review}");
        }
        // Each module's code before and after, by hash.
        let conversion: Value = serde_json::from_str(text(&package, "conversion.json")).unwrap();
        assert_eq!(
            (
                &conversion["pair"],
                &conversion["spec"],
                &conversion["head"]
            ),
            (&json!(1), &json!("v~a"), &json!("v~b"))
        );
        assert_eq!(
            (&conversion["against"], &conversion["allow"]),
            (&json!(["v~a"]), &json!(["g"]))
        );
        assert_eq!(conversion["dependencies_removed"], json!(["Physlib"]));
        // By the module they come from: the head, the helper, the spec.
        let challenge = &conversion["conversions"][2];
        assert_eq!(
            challenge,
            &json!({"source_module": "PisaSpec.main", "output_module": "Challenge",
                    "header_steps": ["prepend module", "make header imports public",
                                     "add exposed public section"],
                    "source_sha256": sha256(SPEC),
                    "output_sha256": sha256(text(&package, "Challenge.lean"))})
        );
        assert_eq!(conversion["conversions"][0]["output_module"], "Solution");
        assert_eq!(conversion["conversions"][1]["header_steps"], json!([]));
        // The package's pair, as the router reads it.
        assert_eq!(
            (
                package.pairs[0].challenge_module.as_str(),
                package.pairs[0].config.as_str()
            ),
            ("Challenge", "comparator.json")
        );
    }

    /// The head's imports of the spec, and of the helper, follow their new
    /// names; an unnamed run's package is `pisa_run`, its title to review.
    #[test]
    fn imports_follow_the_new_names() {
        let head = "module\n\nimport Mathlib\npublic import «PisaSpec».«main»\n\ntheorem u : True := trivial";
        let mut run = run(SPEC, head);
        let job = PalomarExport {
            pair: Some(1),
            challenge_module: "Erdos.Spec".into(),
            solution_module: "Erdos.Proof".into(),
        };
        let package = package(&run, &job, &["v~a".into()], &[]).unwrap();
        assert_eq!(
            text(&package, "Erdos/Proof.lean"),
            "module\n\nimport Mathlib\npublic import Erdos.Spec\n\ntheorem u : True := trivial"
        );
        let lakefile = text(&package, "lakefile.toml");
        assert!(
            lakefile.contains("defaultTargets = [\"Erdos\"]\n")
                && lakefile.contains("globs = [\"Erdos.+\"]"),
            "{lakefile}"
        );
        run.pairs[0] = pair(1, "PisaSnap.sa", "PisaSnap.sb");
        for f in &mut run.files {
            f.path = f
                .path
                .replace("PisaSpec/main", "PisaSnap/sa")
                .replace("PisaRun/main", "PisaSnap/sb");
            f.text = f.text.replace("«PisaSpec».«main»", "PisaSnap.sa");
        }
        let package = super::package(&run, &job, &["v~a".into()], &[]).unwrap();
        assert!(text(&package, "lakefile.toml").starts_with("name = \"pisa_run\"\n"));
        let metadata: Value = serde_json::from_str(text(&package, "formalization.yaml")).unwrap();
        assert_eq!(metadata["project"]["name"], "REVIEW");
    }

    /// What the pair imports outside the run is Lean's, Mathlib's or a
    /// package's Mathlib builds on. Anything else, the worker's other
    /// libraries among it, a package of Mathlib alone can't build.
    #[test]
    fn the_pair_imports_only_lean_mathlib_and_what_mathlib_builds_on() {
        let importing = |import: &str| {
            let head = HEAD.replace(
                "import Mathlib",
                &format!("import Mathlib\nimport {import}"),
            );
            let mut helper = run(SPEC, HEAD);
            helper.files[2].text = HELPER_CODE.replace(
                "public import Mathlib",
                &format!("public import Mathlib\npublic import {import}"),
            );
            [("PisaRun.main", run(SPEC, &head)), (HELPER, helper)]
        };
        for import in ["QuantumInfo.X", "PhyslibAlpha", "PisaProject"] {
            for (by, run) in importing(import) {
                let e = refused(&run, &job(None));
                assert_eq!(e.kind, ErrorKind::BadRequest, "{e:?}");
                let wanted = format!("{by} imports {import}");
                assert!(e.message.contains(&wanted), "{wanted}: {e:?}");
            }
        }
        for import in [
            "Batteries.Data.List",
            "Aesop",
            "Std.Data.HashMap",
            "Lean.Elab",
        ] {
            for (by, run) in importing(import) {
                let made = package(&run, &job(None), &[], &[]);
                assert!(made.is_ok(), "{by} imports {import}: {made:?}");
            }
        }
    }

    /// The shape of the Erdős 882 package's `formalization.yaml`, a Palomar
    /// v0.4 file that passed the template's validator: its keys, each
    /// value's type, and a list's items merged into one.
    const REFERENCE_METADATA: &str = r#"{"version": "", "project": {"name": "", "description": "",
        "authors": [""], "responsible_maintainers": [""], "license": ""},
        "classification": {"arxiv": [""], "msc2020": [""]},
        "sources": [{"title": "", "authors": [""], "type": "", "location": "",
            "relationship": "", "author_endorsement": "", "note": "", "id": "", "license": ""}],
        "related_formalizations": [{"id": "", "relationship": "", "note": ""}],
        "status": {"scope": "", "sorry_count": 0, "sorry_in_definitions": 0, "axioms": []},
        "automation": {"methods": [{"method": "", "models": [""], "framework": "",
            "tool_setup": "", "cost": {"wall_time": "", "spend_usd": "", "hardware": ""},
            "prompting_notes": ""}], "spend_usd": "", "notes": ""},
        "fidelity": {"divergences": ""}, "review": {"status": "", "notes": ""},
        "alignment": {"namespace": "", "statements": [{"source": "", "lean": "", "module": "",
            "status": "", "note": ""}]}}"#;

    /// `value`'s shape: its type, an object's keys each with its value's
    /// shape, and a list's items' shapes merged into one.
    fn shape(value: &Value) -> Value {
        match value {
            Value::Object(map) => (map.iter())
                .map(|(k, v)| (k.clone(), shape(v)))
                .collect::<serde_json::Map<_, _>>()
                .into(),
            Value::Array(items) => {
                let merged = items.iter().map(shape).reduce(merge);
                Value::Array(merged.into_iter().collect())
            }
            Value::String(_) => json!("string"),
            Value::Number(_) => json!("number"),
            Value::Bool(_) => json!("bool"),
            Value::Null => Value::Null,
        }
    }

    fn merge(a: Value, b: Value) -> Value {
        match (a, b) {
            (Value::Object(mut a), Value::Object(b)) => {
                for (k, v) in b {
                    let merged = match a.remove(&k) {
                        Some(old) => merge(old, v),
                        None => v,
                    };
                    a.insert(k, merged);
                }
                Value::Object(a)
            }
            (a, _) => a,
        }
    }

    /// Every key the reference has, with a value of its type, at every level.
    #[test]
    fn the_metadata_has_the_reference_packages_shape() {
        let package = package(&run(SPEC, HEAD), &job(None), &[], &[]).unwrap();
        let metadata: Value = serde_json::from_str(text(&package, "formalization.yaml")).unwrap();
        let reference: Value = serde_json::from_str(REFERENCE_METADATA).unwrap();
        assert_eq!(shape(&metadata), shape(&reference));
    }

    #[test]
    fn what_a_package_cant_hold_is_refused() {
        let refusal = |run: &ExportedRun, job: &PalomarExport, wanted: &[&str]| {
            let e = refused(run, job);
            assert_eq!(e.kind, ErrorKind::BadRequest, "{e:?}");
            for w in wanted {
                assert!(e.message.contains(w), "{w}: {e:?}");
            }
        };
        // A spec's assumption.
        let mut assuming = run(SPEC, HEAD);
        assuming.files[3].text = config(&["propext", "Quot.sound", "Classical.choice", "lit"]);
        refusal(&assuming, &job(Some(1)), &["lit", "propext"]);
        // A challenge that imports more than Mathlib, Init and Lean.
        let spec = SPEC.replace("import Mathlib", "import Mathlib\nimport Std.Data");
        refusal(
            &run(&spec, HEAD),
            &job(None),
            &["PisaSpec.main", "Std.Data"],
        );
        let spec = SPEC.replace("import Mathlib", &format!("import {HELPER}"));
        refusal(&run(&spec, HEAD), &job(None), &[HELPER]);
        // A module outside Lean, Mathlib and what it builds on, anywhere the
        // pair reaches.
        let helper = format!("{HELPER_CODE}\n")
            .replace("public import Mathlib", "public import Physlib.Units");
        let mut physlib = run(SPEC, HEAD);
        physlib.files[2].text = helper;
        refusal(&physlib, &job(None), &[HELPER, "Physlib.Units"]);
        let head = HEAD.replace("import Mathlib", "import Pisa");
        refusal(&run(SPEC, &head), &job(None), &["PisaRun.main", "Pisa"]);
        // A spec that is its own head: nothing to package as the solution.
        let mut unproved = run(SPEC, HEAD);
        unproved.pairs[0] = pair(1, "PisaSpec.main", "PisaSpec.main");
        refusal(&unproved, &job(None), &["v~a", "own head"]);
        // The pair, from 1, which a run of several must name.
        let mut forked = run(SPEC, HEAD);
        forked.pairs.push(pair(2, "PisaSpec.main", HELPER));
        forked.files.push(ExportedFile {
            path: "comparator/2.json".into(),
            text: config(&["propext", "Quot.sound", "Classical.choice"]),
        });
        refusal(&forked, &job(None), &["2 pairs", "pair"]);
        refusal(&forked, &job(Some(3)), &["3", "1 to 2"]);
        assert!(package(&forked, &job(Some(2)), &[], &[]).is_ok());
        // A new name that a module the pair needs has already.
        let mut taken = job(None);
        taken.challenge_module = HELPER.into();
        refusal(&run(SPEC, HEAD), &taken, &[HELPER]);
    }
}
