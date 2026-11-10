//! A run as a Lake project, for `lake comparator` to judge apart from pisa's
//! own audit (`/snapshots/export`).
//!
//! The project holds every module the run's spec snapshots and heads need,
//! as their code was stored, and a comparator configuration for each spec
//! snapshot and head it reaches, the pairs the audit compares: the spec's
//! module is the challenge and the head's the solution. Its Lake
//! configuration is the worker's project's, so the modules build on what they
//! were built on.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use pisa_protocol::api::{Error, ErrorKind};
use pisa_protocol::snapshot;
use pisa_protocol::verify::Declaration;
use pisa_protocol::wire::{ExportedFile, ExportedPair, ExportedRun};
use serde_json::{Value, json};

use crate::catalog::ModuleMeta;
use crate::snapshots::Snapshots;

/// The project's Lake package.
const NAME: &str = "pisa_run";

/// The axioms `#print axioms` counts a proof clean with.
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];

/// The run that spec snapshots `against` start, with `allow` naming what its
/// heads may change besides the spec's holes, as a Lake project. `project`
/// is the worker's, whose toolchain and packages it takes.
pub async fn export(
    snapshots: &Snapshots,
    project: Option<&Path>,
    against: &[String],
    allow: &[String],
) -> Result<ExportedRun, Error> {
    let audit = snapshots.audit_against(against, allow).await?;
    let catalog = snapshots.catalog();
    let mut metas: HashMap<String, ModuleMeta> = HashMap::new();
    let mut record = async |m: &str| -> Result<ModuleMeta, Error> {
        if let Some(meta) = metas.get(m) {
            return Ok(meta.clone());
        }
        let meta = catalog.module(m).await?;
        metas.insert(m.to_string(), meta.clone());
        Ok(meta)
    };
    let mut needed = BTreeSet::new();
    let mut files = Vec::new();
    let mut pairs = Vec::new();
    for (n, compared) in audit.comparisons.iter().enumerate() {
        let spec = catalog.entry(&snapshots.parse(&compared.spec)?).await?;
        let head = catalog.entry(&snapshots.parse(&compared.head)?).await?;
        let spec_meta = record(&spec.module).await?;
        let head_meta = record(&head.module).await?;
        needed.extend(spec_meta.modules.iter().map(|m| m.id.clone()));
        needed.extend(head_meta.modules.iter().map(|m| m.id.clone()));
        // What the spec permits: the axioms its module, and what it builds
        // on, declare.
        let mut assumptions = Vec::new();
        for m in &spec_meta.modules {
            let meta = record(&m.id).await?;
            let axioms = meta
                .result
                .declarations
                .iter()
                .filter(|d| d.kind == "axiom");
            assumptions.extend(axioms.map(|d| d.name.clone()));
        }
        let holes: Vec<String> = spec.holes.iter().chain(allow).cloned().collect();
        let (config, skipped) = config(
            &spec.module,
            &head.module,
            &spec_meta.result.declarations,
            &holes,
            &assumptions,
        );
        let path = format!("comparator/{}.json", n + 1);
        files.push(file(&path, config));
        pairs.push(ExportedPair {
            spec: compared.spec.clone(),
            head: compared.head.clone(),
            config: path,
            skipped,
        });
    }
    for m in &needed {
        let code = record(m).await?.code;
        files.push(file(&format!("{}/s{m}.lean", snapshot::ROOT), code));
    }
    files.extend(project_files(project)?);
    files.push(file("README.md", readme(&pairs)));
    Ok(ExportedRun {
        files,
        pairs,
        forks: audit.forks,
    })
}

fn file(path: &str, text: String) -> ExportedFile {
    ExportedFile {
        path: path.to_string(),
        text,
    }
}

/// The Lake configuration: the project's toolchain and manifest, and a
/// lakefile with its requirements.
fn project_files(project: Option<&Path>) -> Result<Vec<ExportedFile>, Error> {
    let Some(project) = project else {
        // As in tests, which build nothing.
        let manifest = json!({"version": "1.3.0", "packagesDir": ".lake/packages",
                              "packages": [], "name": NAME, "lakeDir": ".lake",
                              "fixedToolchain": false});
        return Ok(vec![
            file("lake-manifest.json", pretty(&manifest)),
            file("lakefile.toml", lakefile(&manifest)),
        ]);
    };
    let read = |name: &str| {
        std::fs::read_to_string(project.join(name)).map_err(|e| {
            Error::new(
                ErrorKind::Internal,
                format!("reading the project's {name}: {e}"),
            )
        })
    };
    let mut manifest: Value = serde_json::from_str(&read("lake-manifest.json")?).map_err(|e| {
        Error::new(
            ErrorKind::Internal,
            format!("reading the project's lake-manifest.json: {e}"),
        )
    })?;
    manifest["name"] = NAME.into();
    Ok(vec![
        file("lean-toolchain", read("lean-toolchain")?),
        file("lakefile.toml", lakefile(&manifest)),
        file("lake-manifest.json", pretty(&manifest)),
    ])
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// A TOML basic string, which JSON's escapes are valid in.
fn toml(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// A lakefile that requires what `manifest` requires directly, at the
/// revisions it names, with the snapshot modules as a library built with
/// `autoImplicit` off, as the worker builds them.
fn lakefile(manifest: &Value) -> String {
    let mut out = format!(
        "# A pisa run, for `lake comparator`: see README.md.\nname = {}\n\
         defaultTargets = [{}]\n\n[leanOptions]\nautoImplicit = false\n",
        toml(NAME),
        toml(snapshot::ROOT)
    );
    let packages = manifest["packages"].as_array().into_iter().flatten();
    for p in packages.filter(|p| p["inherited"] == false) {
        let field = |key: &str| p[key].as_str().filter(|s| !s.is_empty());
        let Some(name) = field("name") else { continue };
        out.push_str(&format!("\n[[require]]\nname = {}\n", toml(name)));
        match (field("type"), field("url"), field("dir")) {
            (Some("git"), Some(url), _) => {
                out.push_str(&format!("git = {}\n", toml(url)));
                if let Some(rev) = field("inputRev").or(field("rev")) {
                    out.push_str(&format!("rev = {}\n", toml(rev)));
                }
                if let Some(sub) = field("subDir") {
                    out.push_str(&format!("subDir = {}\n", toml(sub)));
                }
            }
            (_, _, Some(dir)) => out.push_str(&format!("path = {}\n", toml(dir))),
            _ => {}
        }
    }
    out.push_str(&format!(
        "\n[[lean_lib]]\nname = {}\nglobs = [{}]\n",
        toml(snapshot::ROOT),
        toml(&format!("{}.+", snapshot::ROOT))
    ));
    out
}

/// Comparator's configuration for spec module `spec` (the challenge) and
/// head module `head` (the solution): the spec's theorems among
/// `declarations`, `holes` as its definition holes, and the standard axioms
/// and `assumptions` as permitted. Private names, which differ between the
/// two modules, are left out and returned.
fn config(
    spec: &str,
    head: &str,
    declarations: &[Declaration],
    holes: &[String],
    assumptions: &[String],
) -> (String, Vec<String>) {
    let mut skipped = Vec::new();
    let mut public = |names: &mut dyn Iterator<Item = &String>| -> Vec<String> {
        let mut kept: Vec<String> = Vec::new();
        for name in names {
            if name.starts_with("_private.") {
                if !skipped.contains(name) {
                    skipped.push(name.clone());
                }
            } else if !kept.contains(name) {
                kept.push(name.clone());
            }
        }
        kept
    };
    let theorems = public(
        &mut declarations
            .iter()
            .filter(|d| d.kind == "theorem" && !d.generated && d.parent.is_none())
            .map(|d| &d.name),
    );
    let holes = public(&mut holes.iter());
    let standard: Vec<String> = STANDARD_AXIOMS.iter().map(|a| a.to_string()).collect();
    let axioms = public(&mut standard.iter().chain(assumptions));
    let config = json!({
        "challenge_module": snapshot::module(spec),
        "solution_module": snapshot::module(head),
        "theorem_names": theorems,
        "definition_names": holes,
        "permitted_axioms": axioms,
    });
    (pretty(&config), skipped)
}

fn readme(pairs: &[ExportedPair]) -> String {
    let mut out = String::from(
        "# A pisa run\n\n\
         The run's spec snapshots and the heads that replace them, as a Lake project for \
         `lake comparator`. Each configuration in `comparator/` pairs a spec snapshot's module, \
         the challenge, with a head's, the solution:\n\n\
         | Configuration | Spec snapshot | Head |\n| --- | --- | --- |\n",
    );
    for p in pairs {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` |\n",
            p.config, p.spec, p.head
        ));
    }
    out.push_str(
        "\nJudge each with `lake comparator --paranoid --config comparator/<n>.json`, which \
         needs `bwrap`. It accepts a pair when each theorem of the spec is proved in the head \
         with the same statement, the definitions those statements use are the same but for \
         the spec's holes (`definition_names`), and the proofs use no axioms but the standard \
         ones and the assumptions the spec declares (`permitted_axioms`).\n\n\
         What it doesn't see:\n\n\
         - Private declarations, which can't be named across two modules. Each pair below \
         lists those it leaves out.\n\
         - Definitions that no theorem's statement uses, which pisa's audit compares too.\n\
         - It refuses some heads that keep the spec's statements, such as code that opens \
         `Classical` in its header, or whose auxiliary declarations differ.\n",
    );
    for p in pairs.iter().filter(|p| !p.skipped.is_empty()) {
        out.push_str(&format!(
            "\n`{}` leaves out: {}\n",
            p.config,
            p.skipped
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::verify::ProofStatus;

    fn decl(name: &str, kind: &str) -> Declaration {
        Declaration {
            name: name.into(),
            kind: kind.into(),
            axioms: Vec::new(),
            assumes: Vec::new(),
            status: ProofStatus::Sorry,
            type_hash: "t".into(),
            value_hash: None,
            type_refs: Vec::new(),
            value_refs: Vec::new(),
            generated: false,
            parent: None,
        }
    }

    #[test]
    fn the_lakefile_requires_what_the_project_requires() {
        let manifest = json!({"packages": [
            {"type": "git", "name": "Physlib", "url": "https://example.com/physlib.git",
             "rev": "20d6", "inputRev": "lean-v4.36.0-rc2", "subDir": null, "inherited": false},
            {"type": "git", "name": "mathlib", "url": "https://example.com/mathlib4.git",
             "rev": "9d58", "inputRev": "v4.36.0-rc2", "inherited": true},
            {"type": "path", "name": "local", "dir": "../local", "inherited": false},
        ]});
        assert_eq!(
            lakefile(&manifest),
            "# A pisa run, for `lake comparator`: see README.md.\nname = \"pisa_run\"\n\
             defaultTargets = [\"PisaSnap\"]\n\n[leanOptions]\nautoImplicit = false\n\n\
             [[require]]\nname = \"Physlib\"\ngit = \"https://example.com/physlib.git\"\n\
             rev = \"lean-v4.36.0-rc2\"\n\n\
             [[require]]\nname = \"local\"\npath = \"../local\"\n\n\
             [[lean_lib]]\nname = \"PisaSnap\"\nglobs = [\"PisaSnap.+\"]\n"
        );
    }

    #[test]
    fn a_pair_names_the_specs_public_theorems_holes_and_assumptions() {
        let mut generated = decl("t.eq_1", "theorem");
        generated.generated = true;
        let mut derived = decl("S.mk.injEq", "theorem");
        derived.parent = Some("S".into());
        let declarations = [
            decl("cost", "def"),
            decl("t", "theorem"),
            generated,
            derived,
            decl("_private.PisaSnap.sa.0.lemma", "theorem"),
            decl("t_nonvacuous", "theorem"),
            decl("lit", "axiom"),
        ];
        let holes = ["cost".to_string(), "_private.h".into(), "cost".into()];
        let assumptions = ["lit".to_string(), "_private.PisaSnap.sa.0.a".into()];
        let (config, skipped) = config("a", "b", &declarations, &holes, &assumptions);
        let config: Value = serde_json::from_str(&config).unwrap();
        assert_eq!(
            config,
            json!({
                "challenge_module": "PisaSnap.sa",
                "solution_module": "PisaSnap.sb",
                "theorem_names": ["t", "t_nonvacuous"],
                "definition_names": ["cost"],
                "permitted_axioms": ["propext", "Quot.sound", "Classical.choice", "lit"],
            })
        );
        assert_eq!(
            skipped,
            [
                "_private.PisaSnap.sa.0.lemma",
                "_private.h",
                "_private.PisaSnap.sa.0.a"
            ]
        );
    }

    #[test]
    fn without_a_project_the_lakefile_requires_nothing() {
        let files = project_files(None).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["lake-manifest.json", "lakefile.toml"]);
        assert!(!files[1].text.contains("[[require]]"), "{}", files[1].text);
    }
}
