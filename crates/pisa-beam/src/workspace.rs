//! Lake workspaces for Beam's documents. Beam opens only regular `.lean` files
//! inside a Lake workspace, so a client writes its documents into one.

use std::io;
use std::path::{Path, PathBuf};

use crate::tools::Doc;

/// How a workspace built over a project reaches that project's packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packages {
    /// `.lake/packages` is a symlink to the project's.
    Symlink,
    /// `packagesDir` names the project's packages directory by absolute path.
    Absolute,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// A workspace with no dependencies and one library `lib`, on `toolchain`
    /// (e.g. `leanprover/lean4:v4.36.0-rc2`).
    pub fn bare(dir: &Path, toolchain: &str, lib: &str) -> io::Result<Workspace> {
        std::fs::create_dir_all(dir.join(lib))?;
        std::fs::write(dir.join("lean-toolchain"), format!("{toolchain}\n"))?;
        std::fs::write(
            dir.join("lakefile.toml"),
            format!("name = \"beam_docs\"\n\n[[lean_lib]]\nname = \"{lib}\"\n"),
        )?;
        let manifest = serde_json::json!({
            "version": "1.3.0", "packagesDir": ".lake/packages", "packages": [],
            "name": "beam_docs", "lakeDir": ".lake", "fixedToolchain": false
        });
        std::fs::write(dir.join("lake-manifest.json"), manifest.to_string())?;
        Workspace::at(dir)
    }

    /// A workspace with `project`'s configuration, toolchain and packages, plus
    /// a library `lib` for the documents. The project itself may be read-only:
    /// nothing is written to it.
    pub fn over(
        dir: &Path,
        project: &Path,
        lib: &str,
        packages: Packages,
    ) -> io::Result<Workspace> {
        std::fs::create_dir_all(dir.join(lib))?;
        std::fs::copy(project.join("lean-toolchain"), dir.join("lean-toolchain"))?;
        let project_packages = std::path::absolute(project.join(".lake").join("packages"))?;
        let mut lakefile = String::new();
        if packages == Packages::Absolute {
            // A top-level key, so it must come before any table.
            lakefile.push_str(&format!("packagesDir = {:?}\n", project_packages));
        }
        lakefile.push_str(&std::fs::read_to_string(project.join("lakefile.toml"))?);
        lakefile.push_str(&format!("\n[[lean_lib]]\nname = \"{lib}\"\n"));
        std::fs::write(dir.join("lakefile.toml"), lakefile)?;
        let mut manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            project.join("lake-manifest.json"),
        )?)
        .map_err(io::Error::other)?;
        match packages {
            Packages::Absolute => {
                manifest["packagesDir"] = project_packages.to_string_lossy().into_owned().into();
            }
            Packages::Symlink => {
                std::fs::create_dir_all(dir.join(".lake"))?;
                let link = dir.join(".lake").join("packages");
                // `exists` follows the link, and a project without packages
                // (lean-core) leaves it dangling.
                if link.symlink_metadata().is_err() {
                    std::os::unix::fs::symlink(&project_packages, link)?;
                }
            }
        }
        std::fs::write(dir.join("lake-manifest.json"), manifest.to_string())?;
        Workspace::at(dir)
    }

    /// An existing workspace.
    pub fn at(dir: &Path) -> io::Result<Workspace> {
        Ok(Workspace {
            root: std::path::absolute(dir)?,
        })
    }

    /// The absolute root, as Beam's tools want it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes document `rel` (relative to the root), creating its directory.
    pub fn write(&self, rel: &str, text: &str) -> io::Result<()> {
        let path = self.root.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)
    }

    pub fn doc<'a>(&'a self, rel: &'a str) -> Doc<'a> {
        Doc {
            root: &self.root,
            path: rel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(dir: &Path) {
        std::fs::create_dir_all(dir.join(".lake/packages/mathlib")).unwrap();
        std::fs::write(dir.join("lean-toolchain"), "leanprover/lean4:v4.36.0-rc2\n").unwrap();
        std::fs::write(
            dir.join("lakefile.toml"),
            "name = \"pisa_project\"\n\n[[require]]\nname = \"mathlib\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("lake-manifest.json"),
            r#"{"version": "1.3.0", "packagesDir": ".lake/packages", "packages": [], "name": "pisa_project"}"#,
        )
        .unwrap();
    }

    #[test]
    fn a_workspace_over_a_project_reaches_its_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("project");
        project(&proj);
        let packages = std::path::absolute(proj.join(".lake/packages")).unwrap();

        let ws =
            Workspace::over(&tmp.path().join("a"), &proj, "BeamDocs", Packages::Symlink).unwrap();
        let link = std::fs::read_link(ws.root().join(".lake/packages")).unwrap();
        assert_eq!(link, packages);
        let lakefile = std::fs::read_to_string(ws.root().join("lakefile.toml")).unwrap();
        assert!(
            lakefile.starts_with("name = \"pisa_project\""),
            "{lakefile}"
        );
        assert!(
            lakefile.ends_with("[[lean_lib]]\nname = \"BeamDocs\"\n"),
            "{lakefile}"
        );

        let ws =
            Workspace::over(&tmp.path().join("b"), &proj, "BeamDocs", Packages::Absolute).unwrap();
        let lakefile = std::fs::read_to_string(ws.root().join("lakefile.toml")).unwrap();
        assert!(
            lakefile.starts_with(&format!("packagesDir = {:?}\n", packages)),
            "{lakefile}"
        );
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(ws.root().join("lake-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["packagesDir"], packages.to_string_lossy().as_ref());
        assert_eq!(manifest["name"], "pisa_project");

        ws.write("BeamDocs/A.lean", "theorem t : True := trivial\n")
            .unwrap();
        assert!(ws.root().join("BeamDocs/A.lean").is_file());
        assert_eq!(ws.doc("BeamDocs/A.lean").root, ws.root());
    }

    #[test]
    fn a_workspace_over_a_project_without_packages_opens_again() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("project");
        project(&proj);
        std::fs::remove_dir_all(proj.join(".lake")).unwrap();
        let dir = tmp.path().join("ws");
        Workspace::over(&dir, &proj, "BeamDocs", Packages::Symlink).unwrap();
        // The link dangles; a second open, as after a restart, must still work.
        Workspace::over(&dir, &proj, "BeamDocs", Packages::Symlink).unwrap();
    }
}
