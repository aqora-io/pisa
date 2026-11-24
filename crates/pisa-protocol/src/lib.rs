//! Shared protocol types for pisa: the client HTTP API, the Lean REPL
//! protocol, and the router↔worker wire protocol.
//!
//! The API's types derive their OpenAPI schemas, which pisa-server serves at
//! `/openapi.json`. For a schema to say what serde sends:
//! - a field skipped when empty (`skip_serializing_if`) is optional;
//! - a field always sent, but decoded leniently (`#[serde(default)]`), is
//!   marked `#[schema(required = true)]`;
//! - an optional field is absent rather than `null`, so pisa-server drops
//!   `null` from optional fields; one that can be `null` is always sent, and
//!   marked required.

pub mod api;
pub mod header;
pub mod labels;
pub mod repl;
pub mod revision;
pub mod scope;
pub mod search;
pub mod snapshot;
pub mod verify;
pub mod version;
pub mod wire;

/// Microseconds since the Unix epoch, as the database and the queue keep
/// times.
pub fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}
