//! The symbol dictionary: notation and LaTeX tokens mapped to the constants
//! they usually mean, as soft hints for the structural lane.
//!
//! ```toml
//! [[symbol]]
//! tokens = ["tr", "trace"]          # normalized query tokens (see `text::tokens`)
//! constants = ["Matrix.trace"]
//! context = ["matrix", "density"]   # optional: full weight only with one of these
//! ```

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Symbols {
    #[serde(default, rename = "symbol")]
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Deserialize)]
struct Entry {
    tokens: Vec<String>,
    constants: Vec<String>,
    #[serde(default)]
    context: Vec<String>,
}

/// The dictionary shipped in `search/symbols.toml`.
pub const DEFAULT: &str = include_str!("../../../search/symbols.toml");

impl Symbols {
    pub fn parse(toml_text: &str) -> anyhow::Result<Symbols> {
        Ok(toml::from_str(toml_text)?)
    }

    /// Every constant the dictionary names.
    pub fn all_constants(&self) -> std::collections::BTreeSet<String> {
        self.entries
            .iter()
            .flat_map(|e| e.constants.iter().cloned())
            .collect()
    }

    /// Drops constants the index doesn't have, logging them.
    pub fn retain_known(mut self, known: impl Fn(&str) -> bool) -> Symbols {
        for e in &mut self.entries {
            e.constants.retain(|c| {
                let ok = known(c);
                if !ok {
                    tracing::debug!(constant = %c, "symbol dictionary names an unknown constant");
                }
                ok
            });
        }
        self.entries.retain(|e| !e.constants.is_empty());
        self
    }

    /// Candidate constants for a query's tokens, with weights.
    pub fn constants(&self, tokens: &[String]) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = Vec::new();
        for e in &self.entries {
            if !e.tokens.iter().any(|t| tokens.contains(t)) {
                continue;
            }
            let weight = if e.context.is_empty() || e.context.iter().any(|c| tokens.contains(c)) {
                1.0
            } else {
                0.5
            };
            for c in &e.constants {
                match out.iter_mut().find(|(name, _)| name == c) {
                    Some(existing) => existing.1 = existing.1.max(weight),
                    None => out.push((c.clone(), weight)),
                }
            }
        }
        out
    }

    pub fn constant_names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .flat_map(|e| e.constants.iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{latex_to_unicode, tokens};

    #[test]
    fn maps_latex_to_weighted_constants() {
        let s = Symbols::parse(
            r#"
            [[symbol]]
            tokens = ["tr"]
            constants = ["Matrix.trace", "LinearMap.trace"]
            context = ["matrix", "density"]
            [[symbol]]
            tokens = ["⊗"]
            constants = ["TensorProduct"]
            "#,
        )
        .unwrap();
        let q = tokens(&latex_to_unicode(r"\operatorname{Tr}(\rho \otimes \sigma)"));
        assert_eq!(
            s.constants(&q),
            [
                ("Matrix.trace".to_string(), 0.5),
                ("LinearMap.trace".to_string(), 0.5),
                ("TensorProduct".to_string(), 1.0)
            ]
        );
        let s = s.retain_known(|c| c != "LinearMap.trace");
        assert_eq!(s.constant_names().count(), 2);
    }

    #[test]
    fn shipped_dictionary_parses() {
        assert!(Symbols::parse(DEFAULT).unwrap().constant_names().count() > 20);
    }

    #[test]
    fn shipped_brackets_and_bars_mean_what_they_say() {
        let s = Symbols::parse(DEFAULT).unwrap();
        let means = |text: &str| -> Vec<String> {
            let q = tokens(&latex_to_unicode(text));
            s.constants(&q).into_iter().map(|(c, _)| c).collect()
        };
        // The inner product's brackets, not the anonymous constructor's.
        assert_eq!(means("⟪x, y⟫"), ["Inner.inner"]);
        assert_eq!(means("inner"), ["Inner.inner"]);
        assert_eq!(means("⟨a, b⟩"), Vec::<String>::new());
        // A bra-ket's bar isn't an absolute value.
        assert_eq!(means("|ψ⟩"), Vec::<String>::new());
        assert_eq!(means("abs"), ["abs"]);
        // Names that exist.
        assert!(means("adjoint").contains(&"Star.star".to_string()));
        assert!(!s.all_constants().contains("star") && !s.all_constants().contains("inner"));
    }
}
