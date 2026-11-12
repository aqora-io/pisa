//! The physics evaluation sets that need no new model spend, written from
//! the corpus and the store of descriptions: the queries the describer wrote
//! for each declaration, and premise retrieval, a theorem's statement as a
//! goal with the theorems its proof uses as the answers.

use std::collections::HashMap;

use pisa_protocol::search::Query;
use sha2::{Digest, Sha256};

use crate::corpus::{Decl, is_generated, is_syntax, is_tooling};
use crate::eval::{Item, Set, split_of};
use crate::informal_store::Entry;

/// The physics libraries the sets draw from.
pub const LIBRARIES: [&str; 3] = ["Physlib", "QuantumInfo", "PhyslibAlpha"];

/// What search shows without being named exactly, and isn't an instance.
fn shown(d: &Decl) -> bool {
    !is_generated(&d.name)
        && !is_tooling(&d.name)
        && !is_syntax(d)
        && !d.uses_sorry
        && d.deprecated_to.is_none()
        && !d.attrs.iter().any(|a| a == "instance")
}

/// Core logic the elaborator puts in proofs (`congrArg`, `Eq.trans`):
/// nobody searches for these as premises.
const CORE: [&str; 2] = ["Init", "Std"];

/// `per` of `pool`, the same for one seed: those whose names hash lowest.
fn sample(mut pool: Vec<&Decl>, per: usize, seed: u64) -> Vec<&Decl> {
    let key = |d: &Decl| {
        let mut h = Sha256::new();
        h.update(seed.to_le_bytes());
        h.update(d.name.as_bytes());
        h.finalize()
    };
    pool.sort_by_cached_key(|d| (key(d), d.name.clone()));
    pool.truncate(per);
    pool
}

/// The queries the describer wrote for `per` described declarations of each
/// library: its first, in words, and its second, a formula. The third,
/// keywords, is left out, as it often repeats the declaration's name.
pub fn synthetic(decls: &[Decl], entries: &HashMap<String, Entry>, per: usize, seed: u64) -> Set {
    let queries = |d: &Decl| {
        let q = &entries.get(&d.content_hash)?.synthetic_queries;
        let (words, formula) = (q.first()?, q.get(1)?);
        (!words.trim().is_empty() && !formula.trim().is_empty()).then_some([words, formula])
    };
    let mut items = Vec::new();
    for library in LIBRARIES {
        let pool: Vec<&Decl> = (decls.iter())
            .filter(|d| d.library == library && shown(d) && queries(d).is_some())
            .collect();
        for d in sample(pool, per, seed) {
            let Some(asked) = queries(d) else { continue };
            for (style, text) in ["nl", "latex"].into_iter().zip(asked) {
                items.push(Item {
                    id: format!("{}#{style}", d.name),
                    style: format!("{library}/{style}"),
                    split: split_of(&d.name),
                    query: Query {
                        text: Some(text.clone()),
                        ..Default::default()
                    },
                    answers: vec![d.name.clone()],
                    exclude: Vec::new(),
                    source: None,
                });
            }
        }
    }
    Set {
        name: "physics-synthetic".into(),
        about: "The searches Claude Haiku wrote for each declaration when describing it, \
                in words and as a formula, sent as text. The same call wrote the indexed \
                description and saw the name, so these read close to what is indexed: \
                scores run high, and favour the lexical lane."
            .into(),
        items,
    }
}

/// For `per` theorems of each library: the statement as a goal, and the
/// theorems its proof uses as the answers, itself left out.
pub fn premises(decls: &[Decl], per: usize, seed: u64) -> Set {
    let by_name: HashMap<&str, &Decl> = decls.iter().map(|d| (d.name.as_str(), d)).collect();
    let used = |d: &Decl| {
        let mut out: Vec<String> = Vec::new();
        for r in &d.value_refs {
            let Some(p) = by_name.get(r.as_str()) else {
                continue;
            };
            if p.kind == "theorem"
                && shown(p)
                && !CORE.contains(&p.library.as_str())
                && p.name != d.name
                && !out.contains(&p.name)
            {
                out.push(p.name.clone());
            }
        }
        out
    };
    let mut items = Vec::new();
    for library in LIBRARIES {
        let pool: Vec<&Decl> = (decls.iter())
            .filter(|d| d.library == library && d.kind == "theorem" && shown(d))
            .filter(|d| goal_of(&d.signature).is_some() && !used(d).is_empty())
            .collect();
        for d in sample(pool, per, seed) {
            items.push(Item {
                id: d.name.clone(),
                style: library.to_string(),
                split: split_of(&d.name),
                query: Query {
                    goal: goal_of(&d.signature),
                    ..Default::default()
                },
                answers: used(d),
                exclude: vec![d.name.clone()],
                source: None,
            });
        }
    }
    Set {
        name: "physics-premises".into(),
        about: "A theorem's statement as a goal, sent as `goal`; the answers are the \
                theorems its proof uses, and the theorem itself is left out of the hits."
            .into(),
        items,
    }
}

/// A theorem's signature as the goal it states: each binder a hypothesis,
/// an anonymous instance as `inst✝`, then `⊢` and the conclusion.
pub fn goal_of(signature: &str) -> Option<String> {
    let s = signature.trim();
    // The name, with its universes, `.{u_1, u_2}`.
    let mut at = 0;
    loop {
        let rest = &s[at..];
        if rest.starts_with(".{") {
            at = group_end(s, at + 1)?;
            continue;
        }
        match rest.chars().next()? {
            c if c.is_whitespace() => break,
            c => at += c.len_utf8(),
        }
    }
    let mut goal = String::new();
    loop {
        at = s.len() - s[at..].trim_start().len();
        let open = s[at..].chars().next()?;
        if open == ':' {
            goal.push_str("⊢ ");
            goal.push_str(&collapse(&s[at + 1..]));
            return Some(goal);
        }
        closing(open)?;
        let end = group_end(s, at)?;
        let inner = &s[at + open.len_utf8()..end - closing(open)?.len_utf8()];
        let line = match top_level_colon(inner) {
            Some(i) => format!("{} : {}", collapse(&inner[..i]), collapse(&inner[i + 1..])),
            None if open == '[' => format!("inst✝ : {}", collapse(inner)),
            None => collapse(inner),
        };
        goal.push_str(&line);
        goal.push('\n');
        at = end;
    }
}

fn closing(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '{' => Some('}'),
        '[' => Some(']'),
        '⦃' => Some('⦄'),
        _ => None,
    }
}

/// Where the bracket opening at `start` closes, just past it.
fn group_end(s: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s[start..].char_indices() {
        if closing(c).is_some() {
            depth += 1;
        } else if matches!(c, ')' | '}' | ']' | '⦄') {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + i + c.len_utf8());
            }
        }
    }
    None
}

/// The first `:` outside brackets.
fn top_level_colon(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        if closing(c).is_some() {
            depth += 1;
        } else if matches!(c, ')' | '}' | ']' | '⦄') {
            depth = depth.saturating_sub(1);
        } else if c == ':' && depth == 0 {
            return Some(i);
        }
    }
    None
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::search::Informal;

    #[test]
    fn a_signature_becomes_the_goal_it_states() {
        assert_eq!(
            goal_of(
                "Foo.bar.{u_1, u_2} {α : Type u_1} [Fintype α] [inst : Nonempty α] (x y : α)\n  \
                 (h : x = y) :\n  f x = f y"
            )
            .as_deref(),
            Some(
                "α : Type u_1\ninst✝ : Fintype α\ninst : Nonempty α\nx y : α\nh : x = y\n⊢ f x = f y"
            )
        );
        // Colons inside binders and the conclusion stay where they are.
        assert_eq!(
            goal_of("T.pos (f : (a : ℕ) → Fin (a + 1)) ⦃s : Set ℕ⦄ : ∀ x ∈ s, (x : ℤ) ≥ 0")
                .as_deref(),
            Some("f : (a : ℕ) → Fin (a + 1)\ns : Set ℕ\n⊢ ∀ x ∈ s, (x : ℤ) ≥ 0")
        );
        assert_eq!(
            goal_of("Nat.zero_lt_one : 0 < 1").as_deref(),
            Some("⊢ 0 < 1")
        );
        assert_eq!(goal_of("Broken (x : ℕ"), None);
    }

    fn decl(name: &str, library: &str, kind: &str) -> Decl {
        Decl {
            name: name.into(),
            kind: kind.into(),
            library: library.into(),
            module: format!("{library}.M"),
            signature: format!("{name} (x : ℕ) : x = x"),
            content_hash: format!("h-{name}"),
            ..Default::default()
        }
    }

    fn entry(d: &Decl, queries: &[&str]) -> (String, Entry) {
        let e = Entry {
            content_hash: d.content_hash.clone(),
            name: d.name.clone(),
            informal: Informal::default(),
            synthetic_queries: queries.iter().map(|q| q.to_string()).collect(),
            meta: None,
        };
        (d.content_hash.clone(), e)
    }

    #[test]
    fn synthetic_queries_come_from_shown_described_declarations() {
        let three = ["what is a channel", "\\Phi(\\rho)", "channel CPTP map"];
        let a = decl("CPTPMap", "QuantumInfo", "def");
        let one = decl("CPTPMap.one_query", "QuantumInfo", "theorem");
        let generated = decl("CPTPMap.eq_1", "QuantumInfo", "theorem");
        let mut sorried = decl("CPTPMap.sorried", "QuantumInfo", "theorem");
        sorried.uses_sorry = true;
        let undescribed = decl("CPTPMap.undescribed", "QuantumInfo", "theorem");
        let mut instance = decl("CPTPMap.instFoo", "QuantumInfo", "def");
        instance.attrs = vec!["instance".into()];
        let mathlib = decl("Nat.succ", "Mathlib", "def");
        let physlib = decl("Wave.speed", "Physlib", "def");
        let entries: HashMap<String, Entry> = [
            entry(&a, &three),
            entry(&one, &three[..1]),
            entry(&generated, &three),
            entry(&sorried, &three),
            entry(&mathlib, &three),
            entry(&physlib, &three),
            entry(&instance, &three),
        ]
        .into_iter()
        .collect();
        let decls = [
            a,
            one,
            generated,
            sorried,
            undescribed,
            mathlib,
            physlib,
            instance,
        ];
        let set = synthetic(&decls, &entries, 10, 1);
        assert_eq!(set.name, "physics-synthetic");
        let items: Vec<(&str, &str, &str)> = (set.items.iter())
            .map(|i| {
                (
                    i.id.as_str(),
                    i.style.as_str(),
                    i.query.text.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            items,
            [
                ("Wave.speed#nl", "Physlib/nl", "what is a channel"),
                ("Wave.speed#latex", "Physlib/latex", "\\Phi(\\rho)"),
                ("CPTPMap#nl", "QuantumInfo/nl", "what is a channel"),
                ("CPTPMap#latex", "QuantumInfo/latex", "\\Phi(\\rho)"),
            ]
        );
        assert_eq!(set.items[2].answers, ["CPTPMap"]);
        assert_eq!(set.items[2].split, split_of("CPTPMap"));
        assert_eq!(set.items[3].split, split_of("CPTPMap"));
    }

    #[test]
    fn a_sample_is_the_same_for_a_seed() {
        let decls: Vec<Decl> = (0..50)
            .map(|i| decl(&format!("Q.t{i}"), "QuantumInfo", "theorem"))
            .collect();
        let entries: HashMap<String, Entry> = decls.iter().map(|d| entry(d, &["a", "b"])).collect();
        let names = |seed| {
            (synthetic(&decls, &entries, 5, seed).items.iter())
                .map(|i| i.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(1).len(), 10);
        assert_eq!(names(1), names(1));
        assert_ne!(names(1), names(2));
    }

    #[test]
    fn premises_are_the_shown_theorems_a_proof_uses() {
        let mut t = decl("Q.main", "QuantumInfo", "theorem");
        t.signature = "Q.main (ρ : MState d) : ρ.m.trace = 1".into();
        t.value_refs = [
            "Q.lemma",
            "Q.inst",
            "Q.main",
            "Mathlib.Tactic.Ring.add_pf",
            "Q.def",
            "Q.sorried",
            "Q.lemma",
            "Unknown.name",
            "congrArg",
            "Q.instTheorem",
        ]
        .map(String::from)
        .to_vec();
        let mut no_premise = decl("Q.alone", "QuantumInfo", "theorem");
        no_premise.value_refs = vec!["Q.def".into()];
        let lemma = decl("Q.lemma", "QuantumInfo", "theorem");
        let inst = decl("Q.inst", "QuantumInfo", "instance");
        let tactic = decl("Mathlib.Tactic.Ring.add_pf", "Mathlib", "theorem");
        let def = decl("Q.def", "QuantumInfo", "def");
        let mut sorried = decl("Q.sorried", "QuantumInfo", "theorem");
        sorried.uses_sorry = true;
        // Core logic the elaborator puts in every proof, and an instance of
        // a class of propositions, aren't what a search for premises is for.
        let core = decl("congrArg", "Init", "theorem");
        let mut inst_theorem = decl("Q.instTheorem", "QuantumInfo", "theorem");
        inst_theorem.attrs = vec!["instance".into()];
        let decls = [
            t,
            no_premise,
            lemma,
            inst,
            tactic,
            def,
            sorried,
            core,
            inst_theorem,
        ];
        let set = premises(&decls, 10, 1);
        assert_eq!(set.name, "physics-premises");
        let main: Vec<&Item> = set.items.iter().filter(|i| i.id == "Q.main").collect();
        assert_eq!(main.len(), 1);
        let main = main[0];
        assert_eq!(main.style, "QuantumInfo");
        assert_eq!(main.answers, ["Q.lemma"]);
        assert_eq!(main.exclude, ["Q.main"]);
        assert_eq!(
            main.query.goal.as_deref(),
            Some("ρ : MState d\n⊢ ρ.m.trace = 1")
        );
        assert!(!set.items.iter().any(|i| i.id == "Q.alone"));
        // `Q.lemma` itself uses nothing, so it asks for nothing.
        assert!(!set.items.iter().any(|i| i.id == "Q.lemma"));
    }
}
