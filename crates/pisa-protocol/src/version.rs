//! The label that identifies a pinned Lean project: its toolchain plus a hash
//! of the files that decide what gets built. Worker pools and search indexes
//! use it, so a request names the exact environment it was meant for.

use std::path::Path;

use sha2::{Digest, Sha256};

/// `<toolchain tag>-<8 hex>`, e.g. `v4.35.0-1a2b3c4d`, hashing
/// `lake-manifest.json` and `lakefile.toml` (or `lakefile.lean`).
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
        assert_ne!(project_version(&dir).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
