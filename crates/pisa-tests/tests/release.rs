//! What a release names agrees: every package has the workspace's version,
//! which the changelog has a section for, and Compose and the chart default
//! to the environments the repository builds.

use std::path::{Path, PathBuf};

use pisa_protocol::version::project_version;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap()
}

/// The value of the first line of `lines` that starts with `key`, unquoted.
fn value<'a>(mut lines: impl Iterator<Item = &'a str>, key: &str) -> &'a str {
    let found = lines.find_map(|l| l.strip_prefix(key));
    found
        .unwrap_or_else(|| panic!("no {key}"))
        .trim()
        .trim_matches('"')
}

#[test]
fn every_package_has_the_workspaces_version() {
    let release = env!("CARGO_PKG_VERSION");
    for package in ["web/packages/client", "web/packages/react"] {
        let json = read(&format!("{package}/package.json"));
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["version"], release, "{package}");
    }
    let chart = read("deploy/helm/pisa/Chart.yaml");
    assert_eq!(value(chart.lines(), "version:"), release);
    assert_eq!(value(chart.lines(), "appVersion:"), release);
    // The release workflow's notes are the changelog's section.
    let changelog = read("CHANGELOG.md");
    let heading = format!("## [{release}]");
    assert!(
        changelog.lines().any(|l| l.starts_with(&heading)),
        "{heading}"
    );
}

#[test]
fn compose_and_the_chart_default_to_the_projects_environments() {
    let full = project_version(&root().join("docker/lean-project")).unwrap();
    let core = project_version(&root().join("docker/lean-core")).unwrap();

    let compose = read("docker-compose.yml");
    let search = value(compose.lines().map(str::trim), "PISA_SEARCH_VERSION:");
    assert_eq!(search, format!("${{PISA_SEARCH_VERSION:-{full}}}"));
    assert!(compose.contains("PROJECT: ${PROJECT:-lean-project}"));

    let values = read("deploy/helm/pisa/values.yaml");
    let worker = values.lines().skip_while(|l| *l != "worker:");
    assert_eq!(value(worker, "  version:"), full);
    // The values name lean-core's, for a smaller image.
    assert!(values.contains(&format!("is {core}.")), "{core}");
    // Minikube runs a lean-core image.
    let minikube = read("deploy/minikube/values.yaml");
    let worker = minikube.lines().skip_while(|l| *l != "worker:");
    assert_eq!(value(worker, "  version:"), core);
}
