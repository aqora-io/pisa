//! Environment snapshots: accepted code compiled to a Lean module that later
//! code imports. A snapshot's module name comes from a hash of its code, so
//! the same code is the same snapshot on every worker of a version.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::header::{import_parts, split_header};

/// The root of every snapshot module name.
pub const ROOT: &str = "PisaSnap";

/// The module name of snapshot `id`, e.g. `PisaSnap.s1a2b…`.
pub fn module(id: &str) -> String {
    format!("{ROOT}.s{id}")
}

/// The id of the snapshot built from `code`.
pub fn id(code: &str) -> String {
    hex::encode(&Sha256::digest(code.as_bytes())[..16])
}

/// The id in a snapshot module name, `None` for other modules, and an error
/// for a malformed name under [`ROOT`].
pub fn parse_module(name: &str) -> Result<Option<String>, String> {
    let plain: String = name.chars().filter(|c| !matches!(c, '«' | '»')).collect();
    let mut parts = plain.split('.');
    if parts.next() != Some(ROOT) {
        return Ok(None);
    }
    let id = parts.next().and_then(|p| p.strip_prefix('s'));
    match id {
        Some(id)
            if parts.next().is_none()
                && id.len() == 32
                && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            Ok(Some(id.to_string()))
        }
        _ => Err(format!("{name} is not a snapshot module")),
    }
}

/// Ids of the snapshots a normalised header imports, in order.
pub fn imports(header: &str) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for (_, modules) in header.lines().filter_map(import_parts) {
        for m in modules {
            if let Some(id) = parse_module(m)?
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// The header for code on top of snapshot `id`, built from a header with
/// snapshot imports (the header of `id`'s own code): the same lines, with the
/// snapshots replaced by `id`. Code that starts with it reuses the
/// environment the snapshot's creator keeps warm.
///
/// The new import takes the modifiers of the snapshot imports it replaces.
/// Without any, a module-system header gets `public import`, so that chains of
/// snapshots stay transitive, and a plain one `import`.
pub fn next_header(header: &str, id: &str) -> String {
    let mut lines = Vec::new();
    let mut modifiers = None;
    for line in header.lines() {
        let Some((prefix, modules)) = import_parts(line) else {
            lines.push(line.to_string());
            continue;
        };
        let (snapshots, kept): (Vec<&str>, Vec<&str>) = modules
            .into_iter()
            .partition(|m| matches!(parse_module(m), Ok(Some(_)) | Err(_)));
        if snapshots.is_empty() {
            lines.push(line.to_string());
            continue;
        }
        if !kept.is_empty() {
            lines.push(format!("{prefix} {}", kept.join(" ")));
        }
        modifiers.get_or_insert(prefix);
    }
    let modifiers = modifiers.unwrap_or_else(|| {
        let module_system = lines.first().is_some_and(|l| l == "module");
        if module_system {
            "public import"
        } else {
            "import"
        }
        .to_string()
    });
    lines.push(format!("{modifiers} {}", module(id)));
    lines.join("\n")
}

/// `code` with each snapshot its imports name that `map` has (old id → new
/// id) swapped for the new one. Only the import lines change, and an id is
/// always 32 hex digits, so lines and columns stay where they were.
pub fn rewrite_imports(code: &str, map: &BTreeMap<String, String>) -> String {
    let header_lines = split_header(code).header_lines as usize;
    let lines = code.split('\n').enumerate().map(|(i, line)| {
        let import = line.split("--").next().unwrap_or(line);
        let swapped = match import_parts(import) {
            Some((_, modules)) if i < header_lines => modules
                .iter()
                .filter_map(|m| parse_module(m).ok().flatten())
                .filter_map(|id| Some((id.clone(), map.get(&id)?)))
                .collect(),
            _ => Vec::new(),
        };
        swapped
            .into_iter()
            .fold(line.to_string(), |line, (old, new)| {
                line.replace(&format!("s{old}"), &format!("s{new}"))
            })
    });
    lines.collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::split_header;

    const A: &str = "0123456789abcdef0123456789abcdef";
    const B: &str = "fedcba9876543210fedcba9876543210";

    #[test]
    fn module_names_round_trip_and_reject_lookalikes() {
        assert_eq!(parse_module(&module(A)), Ok(Some(A.to_string())));
        assert_eq!(
            parse_module(&format!("«PisaSnap».«s{A}»")),
            Ok(Some(A.into()))
        );
        assert_eq!(parse_module("Mathlib.Data.Real"), Ok(None));
        assert_eq!(parse_module("PisaSnapshot"), Ok(None));
        for bad in [
            "PisaSnap",
            "PisaSnap.sXYZ",
            &format!("PisaSnap.s{A}.Extra"),
            &format!("PisaSnap.t{A}"),
            &format!("PisaSnap.s{}", A.to_uppercase()),
        ] {
            assert!(parse_module(bad).is_err(), "{bad}");
        }
        assert_eq!(id("x"), id("x"));
        assert_ne!(id("x"), id("x "));
        assert_eq!(id("x").len(), 32);
    }

    #[test]
    fn imports_lists_snapshots_once_in_order() {
        let header =
            format!("import Mathlib\nimport PisaSnap.s{B} PisaSnap.s{A}\nimport PisaSnap.s{B}");
        assert_eq!(imports(&header), Ok(vec![B.to_string(), A.to_string()]));
        assert!(imports("import PisaSnap.oops").is_err());
        assert_eq!(imports(""), Ok(vec![]));
    }

    #[test]
    fn next_header_swaps_snapshot_imports_and_matches_split() {
        assert_eq!(
            next_header("import Mathlib", A),
            format!("import Mathlib\nimport PisaSnap.s{A}")
        );
        assert_eq!(next_header("", A), format!("import PisaSnap.s{A}"));
        let header = format!("import Mathlib\nimport PisaSnap.s{B}\nimport Foo PisaSnap.s{B}");
        let next = next_header(&header, A);
        assert_eq!(
            next,
            format!("import Mathlib\nimport Foo\nimport PisaSnap.s{A}")
        );
        // Code that starts with it has exactly that header.
        let split = split_header(&format!("{next}\n\ntheorem t : True := trivial"));
        assert_eq!(split.header, next);
    }

    #[test]
    fn rewrite_imports_swaps_only_the_imports_it_is_given() {
        const C: &str = "00000000000000000000000000000000";
        const N: &str = "11111111111111111111111111111111";
        let code = format!(
            "module\npublic import Mathlib\nimport all PisaSnap.s{A} PisaSnap.s{B} -- s{A}\n\
             import «PisaSnap».«s{C}»\n\n-- PisaSnap.s{A} stays here\ntheorem t : True := trivial"
        );
        let map = BTreeMap::from([
            (A.to_string(), N.to_string()),
            (C.to_string(), B.to_string()),
        ]);
        let rewritten = rewrite_imports(&code, &map);
        assert_eq!(
            rewritten,
            format!(
                "module\npublic import Mathlib\nimport all PisaSnap.s{N} PisaSnap.s{B} -- s{N}\n\
                 import «PisaSnap».«s{B}»\n\n-- PisaSnap.s{A} stays here\ntheorem t : True := trivial"
            )
        );
        let lengths = |c: &str| c.lines().map(str::len).collect::<Vec<_>>();
        assert_eq!(lengths(&rewritten), lengths(&code));
        assert_eq!(
            imports(&split_header(&rewritten).header),
            Ok(vec![N.to_string(), B.to_string()])
        );
        assert_eq!(rewrite_imports(&code, &BTreeMap::new()), code);
    }

    #[test]
    fn module_headers_keep_their_modifiers() {
        let header = format!("module\npublic import Mathlib\nimport all PisaSnap.s{B}");
        assert_eq!(imports(&header), Ok(vec![B.to_string()]));
        assert_eq!(
            next_header(&header, A),
            format!("module\npublic import Mathlib\nimport all PisaSnap.s{A}")
        );
        assert_eq!(
            next_header("module\npublic import Mathlib", A),
            format!("module\npublic import Mathlib\npublic import PisaSnap.s{A}")
        );
        let next = next_header(&format!("module\npublic import PisaSnap.s{B}"), A);
        assert_eq!(next, format!("module\npublic import PisaSnap.s{A}"));
        assert_eq!(
            split_header(&format!("{next}\n\npublic section")).header,
            next
        );
    }
}
