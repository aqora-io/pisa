//! The label that identifies a pinned Lean project: its toolchain plus a hash
//! of the files that decide what gets built. Worker pools and search indexes
//! use it, so a request names the exact environment it was meant for.

use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::api::PoolPackage;

/// How a request's version resolves: the default for a request that names
/// none, and aliases, e.g. `physlib` for `v4.35.0-1a2b3c4d`.
#[derive(Debug, Clone, Default)]
pub struct Versions {
    pub default: Option<String>,
    pub aliases: HashMap<String, String>,
}

impl Versions {
    /// The pool label for a requested version (or the default), following aliases.
    pub fn resolve(&self, requested: Option<&str>) -> Option<String> {
        let v = requested
            .map(str::to_string)
            .or_else(|| self.default.clone())?;
        Some(self.aliases.get(&v).cloned().unwrap_or(v))
    }
}

/// `<toolchain tag>-<8 hex>`, e.g. `v4.35.0-1a2b3c4d`, hashing
/// `lake-manifest.json`, `lakefile.toml` (or `lakefile.lean`) and the
/// project's own `.lean` files, whose imports decide what gets built.
pub fn project_version(project: &Path) -> std::io::Result<String> {
    let toolchain = std::fs::read_to_string(project.join("lean-toolchain"))?;
    let toolchain = toolchain.trim();
    let tag = toolchain.rsplit(':').next().unwrap_or(toolchain);
    let mut hasher = Sha256::new();
    for file in ["lake-manifest.json", "lakefile.toml", "lakefile.lean"] {
        if let Ok(bytes) = std::fs::read(project.join(file)) {
            hasher.update(file.as_bytes());
            hasher.update(&bytes);
        }
    }
    let mut sources: Vec<String> = std::fs::read_dir(project)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".lean") && n != "lakefile.lean")
        .collect();
    sources.sort();
    for file in sources {
        hasher.update(file.as_bytes());
        hasher.update(std::fs::read(project.join(&file))?);
    }
    Ok(format!("{tag}-{}", hex::encode(&hasher.finalize()[..4])))
}

/// The toolchain `project` pins (its `lean-toolchain`, e.g.
/// `leanprover/lean4:v4.36.0-rc2`) and the packages its `lake-manifest.json`
/// pins: each empty if its file is missing or unreadable.
pub fn project_pins(project: &Path) -> (String, Vec<PoolPackage>) {
    #[derive(serde::Deserialize)]
    struct Manifest {
        #[serde(default)]
        packages: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        rev: String,
        #[serde(default, rename = "inputRev")]
        input_rev: Option<String>,
    }
    let toolchain = std::fs::read_to_string(project.join("lean-toolchain"))
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    let manifest = std::fs::read(project.join("lake-manifest.json")).ok();
    let packages = manifest
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
        .map(|m| m.packages)
        .unwrap_or_default();
    let packages = packages
        .into_iter()
        .map(|p| PoolPackage {
            name: p.name,
            url: p.url,
            rev: p.rev,
            input_rev: p.input_rev,
        })
        .collect();
    (toolchain, packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_from_project_files() {
        let dir = std::env::temp_dir().join(format!("pisa-v-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lean-toolchain"), "leanprover/lean4:v4.35.0\n").unwrap();
        std::fs::write(dir.join("lake-manifest.json"), "{}").unwrap();
        let before = project_version(&dir).unwrap();
        assert!(before.starts_with("v4.35.0-"), "{before}");
        assert_eq!(before.len(), "v4.35.0-".len() + 8);
        std::fs::write(dir.join("lakefile.toml"), "name = \"x\"").unwrap();
        let with_lakefile = project_version(&dir).unwrap();
        assert_ne!(with_lakefile, before);
        std::fs::write(dir.join("X.lean"), "import Mathlib\n").unwrap();
        let with_root = project_version(&dir).unwrap();
        assert_ne!(
            with_root, with_lakefile,
            "a new import is a new environment"
        );
        std::fs::write(dir.join("notes.md"), "not Lean").unwrap();
        assert_eq!(project_version(&dir).unwrap(), with_root);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pins_are_the_toolchain_and_the_manifests_packages() {
        let dir = std::env::temp_dir().join(format!("pisa-pins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(project_pins(&dir), (String::new(), Vec::new()));
        std::fs::write(dir.join("lean-toolchain"), "leanprover/lean4:v4.35.0\n").unwrap();
        let manifest = r#"{"version": "1.1.0", "packages": [
            {"type": "git", "name": "cslib", "url": "https://github.com/leanprover/cslib",
             "rev": "abc", "inputRev": "main", "inherited": false},
            {"type": "path", "name": "local", "dir": "./local", "inherited": false}]}"#;
        std::fs::write(dir.join("lake-manifest.json"), manifest).unwrap();
        let (toolchain, packages) = project_pins(&dir);
        assert_eq!(toolchain, "leanprover/lean4:v4.35.0");
        assert_eq!(
            packages,
            [
                PoolPackage {
                    name: "cslib".into(),
                    url: Some("https://github.com/leanprover/cslib".into()),
                    rev: "abc".into(),
                    input_rev: Some("main".into()),
                },
                PoolPackage {
                    name: "local".into(),
                    ..PoolPackage::default()
                },
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
