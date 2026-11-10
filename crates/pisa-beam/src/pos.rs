//! Positions. Beam speaks LSP: zero-based lines, and columns in UTF-16 code
//! units. pisa and its REPL number a body's lines from 1 and count columns
//! in characters, and Physlib's notation has characters outside the Basic
//! Multilingual Plane (`𝓝` is two UTF-16 units), so the two differ.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct LspPos {
    pub line: u32,
    pub character: u32,
}

/// The UTF-16 length of the first `chars` characters of `line`.
pub fn utf16_of_chars(line: &str, chars: usize) -> u32 {
    line.chars().take(chars).map(|c| c.len_utf16() as u32).sum()
}

/// How many characters of `line` its first `units` UTF-16 units cover.
pub fn chars_of_utf16(line: &str, units: u32) -> usize {
    let mut seen = 0;
    line.chars()
        .take_while(|c| {
            seen += c.len_utf16() as u32;
            seen <= units
        })
        .count()
}

/// `text` followed by an empty `section`/`end` pair, and the position of
/// `section`. A command probe there runs after all of `text`'s declarations.
/// The end of the file won't do: the lines after a `by` block still belong
/// to it, so a probe there parses as a tactic.
pub fn anchored(text: &str) -> (String, LspPos) {
    let mut out = text.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    let line = out.matches('\n').count() as u32;
    out.push_str("section\nend\n");
    (out, LspPos { line, character: 0 })
}

/// A document made of a header of `offset` lines followed by a body, with
/// positions in the body as pisa gives them: lines from 1, columns in
/// characters.
pub struct BodyMap<'a> {
    lines: Vec<&'a str>,
    offset: u32,
}

impl<'a> BodyMap<'a> {
    pub fn new(document: &'a str, offset: u32) -> Self {
        BodyMap {
            lines: document.split('\n').collect(),
            offset,
        }
    }

    /// The document position of body line `line` (from 1), column `column`
    /// (in characters).
    pub fn to_lsp(&self, line: u32, column: u32) -> LspPos {
        let doc_line = self.offset + line.saturating_sub(1);
        let text = self
            .lines
            .get(doc_line as usize)
            .copied()
            .unwrap_or_default();
        LspPos {
            line: doc_line,
            character: utf16_of_chars(text, column as usize),
        }
    }

    /// The body position (line from 1, column in characters) of `pos`, or
    /// `None` inside the header.
    pub fn to_body(&self, pos: LspPos) -> Option<(u32, u32)> {
        let line = pos.line.checked_sub(self.offset)? + 1;
        let text = self
            .lines
            .get(pos.line as usize)
            .copied()
            .unwrap_or_default();
        Some((line, chars_of_utf16(text, pos.character) as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_utf16_units_outside_the_bmp() {
        let line = "  𝓝 x = ⟪a, b⟫";
        assert_eq!(utf16_of_chars(line, 2), 2);
        // `𝓝` is one character and two units; `⟪` is one of each.
        assert_eq!(utf16_of_chars(line, 3), 4);
        assert_eq!(utf16_of_chars(line, 9), 10);
        assert_eq!(chars_of_utf16(line, 4), 3);
        assert_eq!(chars_of_utf16(line, 3), 2, "half of `𝓝` is not a character");
        assert_eq!(chars_of_utf16(line, 99), line.chars().count());
    }

    #[test]
    fn the_anchor_follows_the_text() {
        let (doc, at) = anchored("theorem t : True := by\n  trivial");
        assert_eq!(doc, "theorem t : True := by\n  trivial\n\nsection\nend\n");
        assert_eq!(
            at,
            LspPos {
                line: 3,
                character: 0
            }
        );
        assert_eq!(doc.split('\n').nth(at.line as usize), Some("section"));
        let (doc, at) = anchored("def x := 1\n");
        assert_eq!(doc.split('\n').nth(at.line as usize), Some("section"));
    }

    #[test]
    fn body_positions_sit_below_the_header() {
        let doc = "import Pisa\nset_option autoImplicit false\ntheorem t : 𝓝 = 𝓝 := by\n  rfl";
        let map = BodyMap::new(doc, 2);
        assert_eq!(
            map.to_lsp(1, 0),
            LspPos {
                line: 2,
                character: 0
            }
        );
        // The first 15 characters hold one `𝓝`: 16 units.
        assert_eq!(
            map.to_lsp(1, 15),
            LspPos {
                line: 2,
                character: 16
            }
        );
        assert_eq!(
            map.to_lsp(2, 2),
            LspPos {
                line: 3,
                character: 2
            }
        );
        assert_eq!(
            map.to_body(LspPos {
                line: 2,
                character: 16
            }),
            Some((1, 15))
        );
        assert_eq!(
            map.to_body(LspPos {
                line: 1,
                character: 0
            }),
            None
        );
    }
}
