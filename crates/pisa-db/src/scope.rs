//! The scope a request runs in (see [`Scope`]): what it creates is stamped
//! with the scope's labels, and what it reads keeps to them. A server runs
//! each request within its scope, and a worker each job. Work of their own,
//! such as sweeps and the search index, runs in none and sees everything.

use std::collections::BTreeMap;
use std::future::Future;

pub use pisa_protocol::scope::Scope;

tokio::task_local! {
    static SCOPE: Scope;
}

/// Runs `f` within `scope`.
pub async fn within<F: Future>(scope: Scope, f: F) -> F::Output {
    SCOPE.scope(scope, f).await
}

/// The scope the current task runs in: none outside [`within`].
pub fn current() -> Scope {
    SCOPE.try_with(Clone::clone).unwrap_or_default()
}

/// Whether the labels `text` (JSON) gives are in the current scope.
pub(crate) fn in_scope(text: &str) -> bool {
    let scope = current();
    scope.is_empty()
        || serde_json::from_str::<BTreeMap<String, String>>(text).is_ok_and(|l| scope.matches(&l))
}

/// The current scope's labels, as JSON text, for what a request creates.
pub(crate) fn stamp() -> String {
    serde_json::to_string(current().labels()).unwrap_or_default()
}
