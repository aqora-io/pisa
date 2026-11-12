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
    /// The line of the code, from 0, that each line of `header` comes from:
    /// the first with that import.
    pub header_at: Vec<u64>,
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
    let mut header_at: Vec<u64> = Vec::new();
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
                header_at.push(i as u64);
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
        header_at,
    }
}

/// Where a position Lean reports in the normalised header of `split` (lines
/// from 1, columns from 0 in code points) is in `submitted`: on the line its
/// import comes from, at the same word. `submitted` is the split code, or the
/// code as sent before imports by run name were resolved, on the same lines.
/// `None` for a line the header doesn't have.
pub fn header_to_code(
    split: &Split,
    submitted: &str,
    line: u64,
    column: u64,
) -> Option<(u64, u64)> {
    let at = *split
        .header_at
        .get(usize::try_from(line).ok()?.checked_sub(1)?)?;
    let from = split.header.split('\n').nth(line as usize - 1)?;
    let to = submitted.split('\n').nth(at as usize)?;
    Some((at + 1, align_column(from, to, column)))
}

/// Places columns of `code` on `submitted`, the code as sent, whose header
/// lines name runs where `code`'s import their heads: a function of a
/// position's line (from 1) and column that gives its column there. Columns
/// of other lines stay as they are.
pub fn resolved_to_submitted<'a>(
    code: &'a str,
    submitted: &'a str,
) -> impl Fn(u64, u64) -> u64 + 'a {
    let header_lines = split_header(code).header_lines as usize;
    let differ: Vec<Option<(&str, &str)>> = code
        .split('\n')
        .zip(submitted.split('\n'))
        .take(header_lines)
        .map(|(from, to)| (from != to).then_some((from, to)))
        .collect();
    move |line, column| {
        let at = line.checked_sub(1).and_then(|i| differ.get(i as usize));
        match at {
            Some(Some((from, to))) => align_column(from, to, column),
            _ => column,
        }
    }
}

/// Column `column` (in code points) of line `from` placed on line `to`,
/// whose words are those of `from`, some of other lengths, and maybe more:
/// at the same word, as far into it as `to`'s has, or as far past its end.
fn align_column(from: &str, to: &str, column: u64) -> u64 {
    // Each word's first column and its length.
    let words = |line: &str| -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut start = None;
        let mut len = 0;
        for (i, c) in line.chars().enumerate() {
            let i = i as u64;
            match (c.is_whitespace(), start) {
                (false, None) => start = Some(i),
                (true, Some(s)) => {
                    out.push((s, i - s));
                    start = None;
                }
                _ => {}
            }
            len = i + 1;
        }
        if let Some(s) = start {
            out.push((s, len - s));
        }
        out
    };
    let (from_words, to_words) = (words(from), words(to));
    let line_end = to.chars().count() as u64;
    let Some(k) = from_words.iter().rposition(|(s, _)| *s <= column) else {
        return column.min(to_words.first().map_or(line_end, |w| w.0));
    };
    let (Some(&(from_start, from_len)), Some(&(to_start, to_len))) =
        (from_words.get(k), to_words.get(k))
    else {
        return line_end;
    };
    let offset = column - from_start;
    let next = to_words.get(k + 1).map_or(line_end, |w| w.0);
    match offset < from_len {
        true => to_start + offset.min(to_len),
        false => (to_start + to_len + offset - from_len).min(next),
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
    fn header_positions_are_placed_on_the_lines_they_come_from() {
        let code = "-- hi\nimport  Foo -- foo\n\nimport Foo\n  import Missing\ntheorem x : True := trivial";
        let s = split_header(code);
        assert_eq!(s.header, "import Foo\nimport Missing");
        assert_eq!(s.header_at, [1, 4]);
        // `Missing`, at 7 in the header's second line.
        assert_eq!(header_to_code(&s, code, 2, 7), Some((5, 9)));
        assert_eq!(header_to_code(&s, code, 2, 14), Some((5, 16)));
        assert_eq!(header_to_code(&s, code, 1, 0), Some((2, 0)));
        assert_eq!(header_to_code(&s, code, 3, 0), None);
        assert_eq!(header_to_code(&s, code, 0, 0), None);
        // Placed on the code as sent, whose import names a run.
        let sent = "-- hi\nimport  Foo -- foo\n\nimport Foo\n  import PisaRun.m\ntheorem x : True := trivial";
        assert_eq!(header_to_code(&s, sent, 2, 14), Some((5, 18)));
    }

    #[test]
    fn columns_after_a_resolved_import_move_back() {
        let module = "PisaSnap.s0123456789abcdef0123456789abcdef";
        let code = format!("import {module} Foo -- x\n\ntheorem t : True := trivial");
        let sent = "import PisaRun.a.b Foo -- x\n\ntheorem t : True := trivial";
        let foo = 8 + module.len() as u64;
        let place = resolved_to_submitted(&code, sent);
        assert_eq!(place(1, foo), 19);
        assert_eq!(place(1, foo + 3), 22);
        // Into the resolved module: as far into the name as it goes.
        assert_eq!(place(1, 7), 7);
        assert_eq!(place(1, 30), 18);
        assert_eq!(place(1, 0), 0);
        // The body's lines are the same.
        assert_eq!(place(3, 12), 12);
    }

    #[test]
    fn key_depends_only_on_normalised_header() {
        let a = split_header("import Mathlib\n\nexample : True := trivial");
        let b = split_header("import   Mathlib\nexample : 1 = 1 := rfl");
        assert_eq!(a.header_key, b.header_key);
        assert_ne!(a.header_key, split_header("import Physlib").header_key);
    }
}
