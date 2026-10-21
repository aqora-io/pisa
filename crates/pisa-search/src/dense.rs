//! The dense lane: embedding similarity. Filled in with the embedding step.

/// Vectors for the declarations that have them, and a way to embed queries.
pub struct Dense;

impl Dense {
    /// Declaration ids nearest to `text`, or `None` if the query can't be embedded.
    pub fn search(&self, _text: &str, _n: usize) -> Option<Vec<u32>> {
        None
    }
}
