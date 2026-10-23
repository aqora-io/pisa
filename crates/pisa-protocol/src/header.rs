//! Splitting submitted Lean code into its import header and its body.
//!
//! The header is imported once per REPL process and cached; only the body runs
//! per request. Positions the REPL reports for the body are shifted back down by
//! `header_lines` so they match the submitted code.
//!
//! A header is Lean's: an optional `module` line (for module-system files),
//! then imports, each with optional `public`, `meta` and `all` modifiers.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// Normalised header: one import per line, duplicates removed.
    pub header: String,
    /// Stable cache key for `header`.
    pub header_key: String,
    pub body: String,
    /// Number of lines of the original code that precede the body.
    pub header_lines: u64,
}

/// The modifiers and modules of an import line, e.g. `("public import",
/// ["Mathlib"])` for `public import Mathlib`.
pub fn import_parts(line: &str) -> Option<(String, Vec<&str>)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "import")?;
    if !words[..at].iter().all(|w| matches!(*w, "public" | "meta")) {
        return None;
    }
    let start = at + 1 + usize::from(words.get(at + 1) == Some(&"all"));
    let modules = words[start..].to_vec();
    (!modules.is_empty()).then(|| (words[..start].join(" "), modules))
}

/// A header line (`module` or an import) without its trailing comment and
/// with single spaces, or `None` if the line is not one.
fn header_line(line: &str) -> Option<String> {
    let code = line.split("--").next().unwrap_or(line);
    let line = code.split_whitespace().collect::<Vec<_>>().join(" ");
    (line == "module" || import_parts(&line).is_some()).then_some(line)
}

pub fn header_key(header: &str) -> String {
    hex::encode(&Sha256::digest(header.as_bytes())[..16])
}

/// Splits `code` into its leading run of imports (with any blank lines and
/// comments among them) and the rest.
pub fn split_header(code: &str) -> Split {
    let lines: Vec<&str> = code.split('\n').collect();
    let mut imports: Vec<String> = Vec::new();
    let mut in_block_comment = 0usize;
    let mut header_end = 0;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if in_block_comment > 0 {
            in_block_comment = block_comment_depth(t, in_block_comment);
            header_end = i + 1;
            continue;
        }
        if t.is_empty() || t.starts_with("--") {
            header_end = i + 1;
        } else if t.starts_with("/-") {
            in_block_comment = block_comment_depth(t, 0);
            header_end = i + 1;
        } else if let Some(line) = header_line(t) {
            if !imports.contains(&line) {
                imports.push(line);
            }
            header_end = i + 1;
        } else {
            break;
        }
    }
    // Trailing blank lines and comments after the last import belong to the body,
    // which keeps doc comments attached to the declaration below them.
    let last_import = lines[..header_end]
        .iter()
        .rposition(|l| header_line(l).is_some())
        .map_or(0, |i| i + 1);
    let header = imports.join("\n");
    Split {
        header_key: header_key(&header),
        header,
        body: lines[last_import..].join("\n"),
        header_lines: last_import as u64,
    }
}

/// Returns the block comment nesting depth after scanning `line`.
fn block_comment_depth(line: &str, mut depth: usize) -> usize {
    let b = line.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        match (b[i], b[i + 1]) {
            (b'/', b'-') => {
                depth += 1;
                i += 2;
            }
            (b'-', b'/') if depth > 0 => {
                depth -= 1;
                i += 2;
            }
            _ => i += 1,
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_imports_from_body() {
        let s = split_header("import Mathlib\nimport Physlib\n\ntheorem x : 1 = 1 := rfl\n");
        assert_eq!(s.header, "import Mathlib\nimport Physlib");
        assert_eq!(s.body, "\ntheorem x : 1 = 1 := rfl\n");
        assert_eq!(s.header_lines, 2);
    }

    #[test]
    fn no_imports_means_empty_header() {
        let s = split_header("theorem x : 1 = 1 := rfl");
        assert_eq!(s.header, "");
        assert_eq!(s.header_lines, 0);
        assert_eq!(s.body, "theorem x : 1 = 1 := rfl");
    }

    #[test]
    fn comments_between_imports_are_header_and_dedupe() {
        let code = "-- hi\nimport Mathlib -- all of it\n/- block\n still -/\nimport Mathlib\nimport  Foo.Bar\n/-- doc -/\ntheorem x : True := trivial";
        let s = split_header(code);
        assert_eq!(s.header, "import Mathlib\nimport Foo.Bar");
        assert_eq!(s.header_lines, 6);
        assert_eq!(s.body, "/-- doc -/\ntheorem x : True := trivial");
    }

    #[test]
    fn module_headers_keep_their_modifiers() {
        let code = "/-\nCopyright\n-/\nmodule\n\npublic import Mathlib.Analysis.X\n\
                    import all Physlib.Y -- why\nmeta import  Lean.Elab.Command\n\
                    public meta import Foo\n\n/-! # Doc -/\n\n@[expose] public section";
        let s = split_header(code);
        assert_eq!(
            s.header,
            "module\npublic import Mathlib.Analysis.X\nimport all Physlib.Y\n\
             meta import Lean.Elab.Command\npublic meta import Foo"
        );
        assert_eq!(s.header_lines, 9);
        assert!(s.body.starts_with("\n/-! # Doc -/"), "{:?}", s.body);
        assert_ne!(
            s.header_key,
            split_header("public import Mathlib.Analysis.X").header_key
        );
        // `public` starts the body unless an import follows.
        assert_eq!(
            split_header("module\npublic section").body,
            "public section"
        );
        assert_eq!(
            import_parts("public import all X"),
            Some(("public import all".into(), vec!["X"]))
        );
        assert_eq!(import_parts("private import X"), None);
        assert_eq!(import_parts("import"), None);
    }

    #[test]
    fn key_depends_only_on_normalised_header() {
        let a = split_header("import Mathlib\n\nexample : True := trivial");
        let b = split_header("import   Mathlib\nexample : 1 = 1 := rfl");
        assert_eq!(a.header_key, b.header_key);
        assert_ne!(a.header_key, split_header("import Physlib").header_key);
    }
}
