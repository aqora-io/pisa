//! The symbol dictionary: notation and LaTeX tokens mapped to the constants
//! they usually mean, as soft hints for the structural lane.
//!
//! ```toml
//! [[symbol]]
//! tokens = ["tr", "trace"]          # normalized query tokens (see `text::tokens`);
//!                                   # "partial trace" needs both words
//! constants = ["Matrix.trace"]
//! context = ["matrix", "density"]   # optional: full weight only with one of these
//! hint = "..."                      # optional: sent back to the caller when the entry fires
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
    #[serde(default)]
    hint: Option<String>,
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
            let has = |t: &String| t.split(' ').all(|w| tokens.iter().any(|q| q == w));
            if !e.tokens.iter().any(has) {
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

    /// The hints of the entries a query's tokens fire, without repeats, in
    /// the dictionary's order.
    pub fn hints(&self, tokens: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.entries {
            let has = |t: &String| t.split(' ').all(|w| tokens.iter().any(|q| q == w));
            let Some(hint) = e.hint.as_ref().filter(|_| e.tokens.iter().any(has)) else {
                continue;
            };
            if !out.contains(hint) {
                out.push(hint.clone());
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

    /// An entry's token of several words matches only when all of them are
    /// in the query.
    #[test]
    fn a_phrase_matches_only_with_all_its_words() {
        let s = Symbols::parse(
            r#"
            [[symbol]]
            tokens = ["partial trace", "ptrace"]
            constants = ["MState.traceLeft"]
            [[symbol]]
            tokens = ["| ⟩"]
            constants = ["Ket"]
            "#,
        )
        .unwrap();
        let means = |text: &str| -> Vec<String> {
            let q = tokens(&latex_to_unicode(text));
            s.constants(&q).into_iter().map(|(c, _)| c).collect()
        };
        assert_eq!(means("the partial trace of ρ"), ["MState.traceLeft"]);
        assert_eq!(means("ptrace"), ["MState.traceLeft"]);
        assert!(means("a partial derivative").is_empty());
        assert!(means("trace").is_empty());
        assert_eq!(means(r"|\psi\rangle"), ["Ket"]);
        assert!(means("⟨a, b⟩").is_empty());
    }

    /// Quantum information's words and notation mean QuantumInfo's
    /// constants; each named here exists in the index.
    #[test]
    fn shipped_quantum_information_vocabulary() {
        let s = Symbols::parse(DEFAULT).unwrap();
        let means = |text: &str| -> Vec<String> {
            let q = tokens(&latex_to_unicode(text));
            s.constants(&q).into_iter().map(|(c, _)| c).collect()
        };
        for (text, constant) in [
            ("partial trace over B", "MState.traceRight"),
            (r"\operatorname{Tr}_A \rho_{AB}", "MState.traceLeft"),
            ("fidelity of two states", "MState.fidelity"),
            ("von Neumann entropy", "Sᵥₙ"),
            ("quantum relative entropy", "qRelativeEnt"),
            ("mutual information", "qMutualInfo"),
            ("Kraus operators", "MatrixMap.of_kraus"),
            ("quantum channel", "CPTPMap"),
            ("CPTP map", "CPTPMap"),
            ("completely positive map", "MatrixMap.IsCompletelyPositive"),
            ("POVM", "POVM"),
            ("qubit", "Qubit"),
            ("Hadamard gate", "Qubit.H"),
            ("CNOT", "Qubit.CNOT"),
            ("⟨ψ|", "Bra"),
            ("density matrix", "MState"),
            ("separable state", "MState.IsSeparable"),
            ("maximally entangled state", "Ket.MES"),
            ("A ⊗ₖ B", "Matrix.kronecker"),
            ("ρ ⊗ᴹ σ", "MState.prod"),
            ("Aᴴ", "Matrix.conjTranspose"),
            ("Hermitian matrix", "HermitianMat"),
        ] {
            assert!(
                means(text).contains(&constant.to_string()),
                "{text}: {:?}",
                means(text)
            );
        }
    }

    /// An entry's hint is reported when the entry fires, once however many
    /// entries share it, and an entry without one reports nothing.
    #[test]
    fn a_fired_entry_reports_its_hint() {
        let s = Symbols::parse(
            r#"
            [[symbol]]
            tokens = ["a b", "ab"]
            constants = ["X"]
            hint = "use X"
            [[symbol]]
            tokens = ["ab"]
            constants = ["Y"]
            hint = "use X"
            [[symbol]]
            tokens = ["c"]
            constants = ["Z"]
            "#,
        )
        .unwrap();
        let hints = |text: &str| s.hints(&tokens(text));
        assert_eq!(hints("a b"), ["use X"]);
        assert_eq!(hints("ab c"), ["use X"]);
        assert!(hints("a").is_empty());
        assert!(hints("c").is_empty());
    }

    /// A logarithm's base is told apart from its argument: the phrase fires
    /// the hint, and plain `log` doesn't.
    #[test]
    fn shipped_logb_base_carries_its_hint() {
        let s = Symbols::parse(DEFAULT).unwrap();
        let q = tokens("logb antitone in its base");
        let hints = s.hints(&q);
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert!(hints[0].contains("Real.log_div_log"), "{hints:?}");
        let consts: Vec<String> = s.constants(&q).into_iter().map(|(c, _)| c).collect();
        assert!(consts.contains(&"Real.logb".to_string()), "{consts:?}");
        assert!(
            consts.contains(&"Real.log_div_log".to_string()),
            "{consts:?}"
        );
        assert!(s.hints(&tokens("logb is monotone")).is_empty());
        assert!(s.hints(&tokens("the log of the base")).is_empty());
        let consts: Vec<String> = s
            .constants(&tokens("log base"))
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert!(consts.contains(&"Real.logb".to_string()), "{consts:?}");
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
        // A bra-ket's bar isn't an absolute value: the two read as a ket.
        assert_eq!(means("|ψ⟩"), ["Ket"]);
        assert_eq!(means("abs"), ["abs"]);
        // Names that exist.
        assert!(means("adjoint").contains(&"Star.star".to_string()));
        assert!(!s.all_constants().contains("star") && !s.all_constants().contains("inner"));
    }
}
