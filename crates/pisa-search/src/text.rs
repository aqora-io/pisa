//! Text normalization shared by indexing and querying, so both sides produce
//! the same tokens.
//!
//! - Names split on `.`, `_` and camelCase: `Matrix.trace_mul_comm` →
//!   `matrix trace mul comm matrix.trace_mul_comm`.
//! - Math symbols are tokens of their own (`∑`, `⊗`, `†`, `≤`), where a stock
//!   tokenizer would drop them.
//! - ASCII spellings fold to Unicode (`<=` → `≤`, `->` → `→`), and LaTeX turns
//!   into the same Unicode text (`\otimes` → `⊗`), so `\le` and `≤` match.
//!   This is a custom table, not NFKC, which would turn `ℝ` into `R`.

/// Converts LaTeX to Unicode math text: known commands become symbols,
/// `\operatorname{Tr}` and similar become their argument, layout commands
/// and braces disappear. Unknown commands keep their name as a word.
pub fn latex_to_unicode(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j == start {
                    // `\,`, `\;`, `\!`, `\\`, `\{` …: spacing or an escaped brace.
                    if let Some(&next) = chars.get(start) {
                        if next == '{' || next == '}' {
                            out.push(next);
                        } else {
                            out.push(' ');
                        }
                    }
                    i = start + 1;
                    continue;
                }
                let cmd: String = chars[start..j].iter().collect();
                i = j;
                if let Some((sym, end)) = blackboard(&cmd, &chars, j) {
                    // `\mathbb{R}` or `\mathbb R` → `ℝ`, `\mathcal{H}` → `ℋ`.
                    out.push(' ');
                    out.push(sym);
                    out.push(' ');
                    i = end;
                    continue;
                }
                if WRAPPERS.contains(&cmd.as_str()) {
                    // `\operatorname{Tr}` → `Tr`: the braces are dropped below.
                    out.push(' ');
                } else if let Some(sym) = latex_symbol(&cmd) {
                    out.push(' ');
                    out.push_str(sym);
                    out.push(' ');
                } else if !LAYOUT.contains(&cmd.as_str()) {
                    out.push(' ');
                    out.push_str(&cmd);
                    out.push(' ');
                }
                // `i` is past the command's name: what follows is read next.
                continue;
            }
            '{' | '}' | '$' | '&' => out.push(' '),
            '~' => out.push(' '),
            _ => out.push(c),
        }
        i += 1;
    }
    fold_ascii(&out)
}

/// Commands whose argument is the content: `\mathrm{tr}` → `tr`.
const WRAPPERS: &[&str] = &[
    "operatorname",
    "mathrm",
    "text",
    "textrm",
    "mathit",
    "mathbf",
    "mathsf",
    "mathtt",
    "boldsymbol",
    "mathop",
    "mbox",
    "hat",
    "tilde",
    "bar",
    "vec",
    "overline",
    "widetilde",
    "widehat",
    "underline",
];

/// Commands that only affect layout.
const LAYOUT: &[&str] = &[
    "left",
    "right",
    "big",
    "Big",
    "bigg",
    "Bigg",
    "bigl",
    "bigr",
    "Bigl",
    "Bigr",
    "displaystyle",
    "textstyle",
    "quad",
    "qquad",
    "limits",
    "nolimits",
    "middle",
    ",",
    "!",
];

fn latex_symbol(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        // Greek
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ε",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "θ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" => "ρ",
        "varrho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "φ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        // Relations
        "le" | "leq" => "≤",
        "ge" | "geq" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "equiv" => "≡",
        "sim" => "∼",
        "simeq" => "≃",
        "cong" => "≅",
        "propto" => "∝",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "supseteq" => "⊇",
        "in" => "∈",
        "notin" => "∉",
        "ll" => "≪",
        "gg" => "≫",
        "prec" => "≺",
        "preceq" => "≼",
        "succ" => "≻",
        "succeq" => "≽",
        "mid" => "∣",
        "parallel" => "∥",
        "perp" => "⟂",
        // Operators
        "otimes" => "⊗",
        "oplus" => "⊕",
        "times" => "×",
        "cdot" => "•",
        "circ" => "∘",
        "pm" => "±",
        "mp" => "∓",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "\\",
        "wedge" | "land" => "∧",
        "vee" | "lor" => "∨",
        "neg" | "lnot" => "¬",
        "star" => "⋆",
        "ast" => "∗",
        "dagger" => "†",
        "ddagger" => "‡",
        "nabla" => "∇",
        "partial" => "∂",
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "oint" => "∮",
        "bigcup" => "⋃",
        "bigcap" => "⋂",
        "bigotimes" => "⨂",
        "bigoplus" => "⨁",
        "sqrt" => "√",
        "infty" => "∞",
        "emptyset" | "varnothing" => "∅",
        "forall" => "∀",
        "exists" => "∃",
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "Rightarrow" | "implies" => "→",
        "leftrightarrow" | "iff" | "Leftrightarrow" => "↔",
        "mapsto" => "↦",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lvert" | "rvert" | "vert" => "|",
        "lVert" | "rVert" | "Vert" => "‖",
        "ldots" | "cdots" | "dots" => "…",
        "hbar" => "ℏ",
        "ell" => "ℓ",
        "Re" => "re",
        "Im" => "im",
        "top" => "⊤",
        "bot" => "⊥",
        "dag" => "†",
        "prime" => "′",
        // Number systems, as \mathbb{R} etc. are handled through `mathbb` below.
        "R" | "Reals" => "ℝ",
        "C" | "Complex" => "ℂ",
        "N" | "Nat" => "ℕ",
        "Z" | "Integers" => "ℤ",
        "Q" | "Rationals" => "ℚ",
        "mathbb" => "",
        "mathcal" => "",
        "mathfrak" => "",
        "mathscr" => "",
        _ => return None,
    })
}

/// The number system `\mathbb` names after `j`, in braces or not, and where
/// what it names ends.
fn blackboard(cmd: &str, chars: &[char], j: usize) -> Option<(char, usize)> {
    if cmd != "mathbb" && cmd != "mathcal" {
        return None;
    }
    let mut k = j;
    while chars.get(k) == Some(&' ') {
        k += 1;
    }
    let braced = chars.get(k) == Some(&'{');
    let letter = chars.get(k + usize::from(braced))?;
    let end = k + usize::from(braced) + 1;
    if braced && chars.get(end) != Some(&'}') {
        return None;
    }
    let sym = if cmd == "mathcal" {
        calligraphic(*letter)?
    } else {
        match letter {
            'R' => 'ℝ',
            'C' => 'ℂ',
            'N' => 'ℕ',
            'Z' => 'ℤ',
            'Q' => 'ℚ',
            _ => return None,
        }
    };
    Some((sym, end + usize::from(braced)))
}

/// The script capital of `letter`: the Letterlike Symbols block has eight,
/// and Mathematical Alphanumeric Symbols the rest.
fn calligraphic(letter: char) -> Option<char> {
    Some(match letter {
        'B' => 'ℬ',
        'E' => 'ℰ',
        'F' => 'ℱ',
        'H' => 'ℋ',
        'I' => 'ℐ',
        'L' => 'ℒ',
        'M' => 'ℳ',
        'R' => 'ℛ',
        'A'..='Z' => char::from_u32(0x1D49C + (letter as u32 - 'A' as u32))?,
        _ => return None,
    })
}

/// A modifier or subscript letter (`ᴴ`, `ᵀ`, `ᵥ`, `ₙ`, `ₖ`), which marks
/// notation when it ends a word.
fn is_modifier(c: char) -> bool {
    matches!(c as u32, 0x02B0..=0x02FF | 0x1D2C..=0x1D6A | 0x1D9B..=0x1DBF | 0x2090..=0x209C)
}

/// Folds ASCII operator spellings into their Unicode forms and blackboard
/// letters after `\mathbb` (already reduced to a bare letter).
pub fn fold_ascii(s: &str) -> String {
    let mut out = s.to_string();
    for (from, to) in [
        ("<->", "↔"),
        ("->", "→"),
        ("<-", "←"),
        ("<=", "≤"),
        (">=", "≥"),
        ("!=", "≠"),
        ("|-", "⊢"),
        ("=>", "→"),
    ] {
        out = out.replace(from, to);
    }
    out
}

/// Characters that are tokens on their own.
fn is_symbol(c: char) -> bool {
    !c.is_alphanumeric()
        && !c.is_whitespace()
        && !matches!(c, '_' | '.' | '\'' | '?' | '!')
        && !matches!(
            c,
            ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '`' | '@' | '#'
        )
}

/// Splits an identifier on `.`, `_` and lower→upper camelCase boundaries.
pub fn split_name(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for chunk in name.split(['.', '_']) {
        let mut word = String::new();
        let mut prev_lower = false;
        for c in chunk.chars() {
            if c.is_uppercase() && prev_lower && !word.is_empty() {
                parts.push(std::mem::take(&mut word));
            }
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
            word.push(c);
        }
        if !word.is_empty() {
            parts.push(word);
        }
    }
    parts.into_iter().map(|p| p.to_lowercase()).collect()
}

/// Tokens of a piece of text: words lowercased, identifiers both split and
/// whole (lowercased), symbols on their own. Prose, Lean and Unicode math all
/// go through this; LaTeX goes through [`latex_to_unicode`] first.
pub fn tokens(text: &str) -> Vec<String> {
    let text = fold_ascii(text);
    let mut out = Vec::new();
    let mut ident = String::new();
    let flush = |ident: &mut String, out: &mut Vec<String>| {
        let id = ident.trim_matches(|c| c == '.' || c == '\'');
        // `Aᴴ` is `A` and `ᴴ`, as well as the word.
        let base = id.trim_end_matches(is_modifier);
        if !base.is_empty() && base.len() < id.len() {
            out.extend(split_name(base));
            out.extend(id[base.len()..].chars().map(|c| c.to_string()));
            out.push(id.to_lowercase());
        } else if !id.is_empty() {
            let parts = split_name(id);
            let whole = id.to_lowercase();
            let compound = parts.len() > 1;
            out.extend(parts);
            if compound {
                out.push(whole);
            }
        }
        ident.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || matches!(c, '_' | '.' | '\'') {
            ident.push(c);
        } else {
            flush(&mut ident, &mut out);
            if is_symbol(c) {
                out.push(c.to_string());
            }
        }
    }
    flush(&mut ident, &mut out);
    out
}

/// Identifiers that look like Lean names in a piece of Lean text: dotted or
/// capitalized words, for resolving against the declaration names.
pub fn lean_identifiers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut ident = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || matches!(c, '_' | '.' | '\'') {
            ident.push(c);
        } else {
            let id = ident.trim_matches('.');
            if id.len() > 1 && id.chars().next().is_some_and(|c| c.is_alphabetic()) {
                out.push(id.to_string());
            }
            ident.clear();
        }
    }
    out.dedup();
    out
}

/// Edit distance between two strings (by chars).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Undoes LaTeX that was written into JSON unescaped, as in some LeanSearch v2
/// records and a few model outputs: `\times` was read as a tab and `imes`, `\frac` as a form
/// feed, `\rho` as a carriage return, `\beta` as a backspace. A newline
/// followed by a letter inside `$…$` was `\n…` (`\nabla`, `\neq`, `\nu`).
pub fn repair_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_math = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' => {
                in_math = !in_math;
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            '\u{8}' => out.push_str("\\b"),
            '\n' if in_math && chars.peek().is_some_and(|n| n.is_ascii_lowercase()) => {
                out.push_str("\\n")
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_latex_read_as_control_characters() {
        // As the faulty records decode: tab, form feed, carriage return, backspace, newline.
        let broken = "the $ n \times n $ matrix, $\u{c}rac{1}{2}$, $\rho \to \\sigma$, $\u{8}eta$, $\nabla f$\nnext line";
        assert_eq!(
            repair_escapes(broken),
            "the $ n \\times n $ matrix, $\\frac{1}{2}$, $\\rho \\to \\sigma$, $\\beta$, $\\nabla f$\nnext line"
        );
        assert_eq!(repair_escapes("plain $x$"), "plain $x$");
    }

    #[test]
    fn names_split_on_dots_underscores_and_camel_case() {
        assert_eq!(
            split_name("Matrix.trace_mul_comm"),
            ["matrix", "trace", "mul", "comm"]
        );
        assert_eq!(
            split_name("MState.vonNeumannEntropy"),
            ["mstate", "von", "neumann", "entropy"]
        );
        assert_eq!(split_name("CPTPMap"), ["cptpmap"]);
    }

    #[test]
    fn tokens_keep_symbols_and_whole_identifiers() {
        assert_eq!(
            tokens("Matrix.trace A = 1"),
            ["matrix", "trace", "matrix.trace", "a", "=", "1"]
        );
        assert_eq!(tokens("x <= y -> P x"), ["x", "≤", "y", "→", "p", "x"]);
        assert_eq!(tokens("∑ i ∈ s, f i"), ["∑", "i", "∈", "s", "f", "i"]);
        assert_eq!(
            tokens("the trace of a density matrix"),
            ["the", "trace", "of", "a", "density", "matrix"]
        );
    }

    #[test]
    fn latex_becomes_the_same_unicode() {
        // `_` is a subscript in LaTeX, so it separates rather than joins.
        assert_eq!(
            tokens(&latex_to_unicode(r"S(\rho_{AB}) \le S(\rho_A) + S(\rho_B)")),
            ["s", "ρ", "ab", "≤", "s", "ρ", "a", "+", "s", "ρ", "b"]
        );
        assert_eq!(
            tokens(&latex_to_unicode(r"\operatorname{Tr}(\rho) = 1")),
            ["tr", "ρ", "=", "1"]
        );
        assert_eq!(
            tokens(&latex_to_unicode(r"A \otimes B^\dagger")),
            ["a", "⊗", "b", "^", "†"]
        );
        assert_eq!(
            tokens(&latex_to_unicode(r"\left( x \right)\,\mathbb{R}")),
            ["x", "ℝ"]
        );
        assert_eq!(
            tokens(&latex_to_unicode(r"f : \mathbb C \to \mathbb{Z}")),
            ["f", "ℂ", "→", "ℤ"]
        );
    }

    /// What follows a command is read, not skipped: the next command, an
    /// operator, a bracket.
    #[test]
    fn a_command_keeps_what_follows_it() {
        assert_eq!(
            tokens(&latex_to_unicode(r"\alpha\beta+\gamma")),
            ["α", "β", "+", "γ"]
        );
        assert_eq!(tokens(&latex_to_unicode(r"|\psi\rangle")), ["|", "ψ", "⟩"]);
        assert_eq!(tokens(&latex_to_unicode(r"f(\rho)")), ["f", "ρ"]);
    }

    /// A calligraphic letter stays a letter, as `\mathcal{H}` names a space.
    #[test]
    fn calligraphic_letters_survive() {
        assert_eq!(
            tokens(&latex_to_unicode(r"\mathcal{H}_A \otimes \mathcal K")),
            ["ℋ", "a", "⊗", "𝒦"]
        );
    }

    /// Trailing modifier letters, as in `Aᴴ` or `Sᵥₙ`, are their own tokens
    /// too, so notation like `ᴴ` reads as a symbol; the word stays whole.
    #[test]
    fn modifier_letters_split_off_a_word() {
        assert_eq!(tokens("Aᴴ * B"), ["a", "ᴴ", "aᴴ", "*", "b"]);
        assert_eq!(tokens("Sᵥₙ ρ"), ["s", "ᵥ", "ₙ", "sᵥₙ", "ρ"]);
        assert_eq!(tokens("ρ ⊗ₖ σ"), ["ρ", "⊗", "ₖ", "σ"]);
    }

    #[test]
    fn lean_identifiers_and_edit_distance() {
        assert_eq!(
            lean_identifiers("⊢ Matrix.trace ?A = 1 ∧ Real.sqrt x ≤ y"),
            ["Matrix.trace", "Real.sqrt"]
        );
        assert_eq!(levenshtein("sqrt_mul_self", "sqrt_mul_sefl"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
    }
}
