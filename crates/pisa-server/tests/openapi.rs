//! The committed `openapi.json` is the server's OpenAPI document.

const FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../openapi.json");

#[test]
fn the_committed_spec_is_current() {
    let spec = serde_json::to_string_pretty(&pisa_server::openapi()).unwrap() + "\n";
    if std::env::var_os("PISA_UPDATE_OPENAPI").is_some() {
        std::fs::write(FILE, &spec).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(FILE).unwrap_or_default();
    assert!(
        committed == spec,
        "openapi.json is out of date: run PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi"
    );
}
