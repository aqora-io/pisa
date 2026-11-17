//! The commit this binary was built from.

/// The commit this binary was built from, as `PISA_REVISION` named it at
/// build time (`unknown` if it didn't).
pub const REVISION: &str = match option_env!("PISA_REVISION") {
    Some(revision) => revision,
    None => "unknown",
};
