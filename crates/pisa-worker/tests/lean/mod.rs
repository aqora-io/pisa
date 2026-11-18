//! What the real-Lean tests share.

use std::path::{Path, PathBuf};

/// The repository's `docker/`, with the `pisa` tool's project
/// (`pisa-lean`) and the core-Lean project (`lean-core`).
pub fn docker() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker")
}
