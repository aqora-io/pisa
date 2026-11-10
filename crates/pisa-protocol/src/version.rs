//! The label that identifies a pinned Lean project: its toolchain plus a hash
//! of the files that decide what gets built. Worker pools and search indexes
//! use it, so a request names the exact environment it was meant for.

use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};

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
}
