//! The workload: real proofs from the bench modules, each a tactic block of a
//! theorem split into the steps it is written as. A module's body is also
//! kept with every such block replaced by `sorry`, which is where sessions
//! and candidates start. The modules compile, so every block is a correct
//! proof; whether a backend can replay it step by step is what's measured.
//! The REPL's replay is recorded here.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::lean::{self, Source};

/// The modules of `bench/infotree/2026-11-06`, each on its own imports.
pub const MODULES: &[&str] = &[
    "Physlib.QuantumMechanics.Operators.Unbounded",
    "Physlib.SpaceAndTime.Space.IsDistBounded",
    "Physlib.Mathematics.Distribution.TemperedDistribution",
    "Physlib.SpaceAndTime.Space.Derivatives.Basic",
    "Physlib.Mathematics.SpecialFunctions.PhysHermite",
    "Physlib.Relativity.Tensors.RealTensor.Vector.Basic",
    "Physlib.SpaceAndTime.Space.Basic",
    "Physlib.QuantumMechanics.Operators.Momentum",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workload {
    pub modules: Vec<ModuleWork>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleWork {
    pub source: Source,
    /// The body with every block replaced by `sorry`.
    pub sorried: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// Indentation of the block's steps, in characters.
    pub base: u32,
    pub steps: Vec<Step>,
    /// The line (from 1) of the block's `sorry` in `sorried`.
    pub sorry_line: u32,
    /// The steps, sent one by one to the REPL from the sorry's proof state,
    /// closed the goal.
    pub repl_replays: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repl_failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// The step's text, with the block's indentation removed.
    pub text: String,
    /// Its first line (from 1) in the original body; it starts at column `base`.
    pub line: u32,
}

impl Block {
    /// The whole block as one candidate.
    pub fn whole(&self) -> String {
        self.steps
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl ModuleWork {
    /// The blocks with steps.
    pub fn usable(&self) -> impl Iterator<Item = &Block> {
        self.blocks.iter().filter(|b| !b.steps.is_empty())
    }

    /// `sorried` with `block`'s `sorry` replaced by `text` (one tactic, or
    /// several lines).
    pub fn filled(&self, block: &Block, text: &str) -> String {
        let indent = " ".repeat(block.base as usize);
        let filled: Vec<String> = text.lines().map(|l| format!("{indent}{l}")).collect();
        let mut lines: Vec<String> = self.sorried.split('\n').map(String::from).collect();
        lines[block.sorry_line as usize - 1] = filled.join("\n");
        lines.join("\n")
    }
}

fn indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// The declaration a line at `i` belongs to is a theorem or lemma: the
/// nearest line above at column 0 that isn't blank, a comment or an
/// attribute starts with one, after its modifiers.
fn in_theorem(lines: &[&str], i: usize) -> bool {
    for line in lines[..=i].iter().rev() {
        let t = line.trim_start();
        if t.is_empty() || indent(line) > 0 || t.starts_with("--") || t.starts_with("@[") {
            continue;
        }
        return t
            .split_whitespace()
            .find(|w| {
                !matches!(
                    *w,
                    "private" | "protected" | "noncomputable" | "nonrec" | "public"
                )
            })
            .is_some_and(|w| w == "theorem" || w == "lemma");
    }
    false
}

/// A block found in a body, by its lines `[start, end)` (from 0).
struct Found {
    start: usize,
    end: usize,
    base: usize,
}

/// Tactic blocks of theorems, written as `... := by` followed by indented
/// steps. A line at the block's indentation starts a step, unless it
/// continues one (`<;>`, a match arm `|`); deeper lines continue a step.
fn find_blocks(lines: &[&str]) -> Vec<Found> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_end();
        if !(t.ends_with(":= by") && !t.trim_start().starts_with("--") && in_theorem(lines, i)) {
            i += 1;
            continue;
        }
        let Some(first) = (i + 1..lines.len()).find(|&k| !lines[k].trim().is_empty()) else {
            break;
        };
        let base = indent(lines[first]);
        if base == 0 {
            i += 1;
            continue;
        }
        let mut end = first + 1;
        let mut k = first + 1;
        while k < lines.len() {
            let line = lines[k];
            if line.trim().is_empty() {
                k += 1;
                continue;
            }
            if indent(line) < base {
                break;
            }
            end = k + 1;
            k += 1;
        }
        found.push(Found {
            start: first,
            end,
            base,
        });
        i = end;
    }
    found
}

fn steps(lines: &[&str], f: &Found) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    for (k, line) in lines.iter().enumerate().take(f.end).skip(f.start) {
        let rest: String = line.chars().skip(f.base.min(indent(line))).collect();
        let starts = indent(line) == f.base
            && !rest.starts_with("<;>")
            && !rest.starts_with('|')
            && !line.trim().is_empty();
        match steps.last_mut() {
            Some(last) if !starts => {
                last.text.push('\n');
                last.text.push_str(&rest);
            }
            _ => steps.push(Step {
                text: rest,
                line: k as u32 + 1,
            }),
        }
    }
    for s in &mut steps {
        s.text = s.text.trim_end().to_string();
    }
    steps
}

/// Finds the blocks of `source` and builds its sorried body.
pub fn prepare(source: Source) -> ModuleWork {
    let lines: Vec<&str> = source.body.split('\n').collect();
    let found = find_blocks(&lines);
    let mut sorried: Vec<String> = Vec::new();
    let mut blocks = Vec::new();
    let mut next = 0;
    for f in &found {
        sorried.extend(lines[next..f.start].iter().map(|l| l.to_string()));
        sorried.push(format!("{}sorry", " ".repeat(f.base)));
        blocks.push(Block {
            base: f.base as u32,
            steps: steps(&lines, f),
            sorry_line: sorried.len() as u32,
            repl_replays: false,
            repl_failure: None,
        });
        next = f.end;
    }
    sorried.extend(lines[next..].iter().map(|l| l.to_string()));
    ModuleWork {
        source,
        sorried: sorried.join("\n"),
        blocks,
    }
}

/// Builds the workload: each module's blocks, with whether their steps
/// replay on the REPL from the sorried body's proof states.
pub async fn generate(env: &Env, modules: &[String]) -> Result<Workload> {
    let mut out = Vec::new();
    for module in modules {
        let source = lean::load_module(&env.project, module)?;
        let mut work = prepare(source);
        let slot = env.slot();
        let mut repl = env.repl(slot)?;
        let header = lean::repl_header(&mut repl, &work.source.header).await?;
        let r = lean::repl_body(&mut repl, header, &work.sorried, None).await?;
        let states: HashMap<u64, u64> = r
            .sorries
            .iter()
            .filter_map(|s| Some((s.pos?.line, s.proof_state?)))
            .collect();
        for block in &mut work.blocks {
            let Some(&root) = states.get(&(block.sorry_line as u64)) else {
                block.repl_failure = Some("no proof state at its sorry".into());
                continue;
            };
            let mut state = root;
            let mut failure = None;
            let mut complete = false;
            for step in &block.steps {
                let s = lean::repl_tactic(&mut repl, state, &step.text).await?;
                match (s.ok, s.proof_state) {
                    (true, Some(next)) => {
                        state = next;
                        complete = s.complete;
                    }
                    _ => {
                        failure = Some(s.error.unwrap_or_else(|| "failed".into()));
                        break;
                    }
                }
            }
            if failure.is_none() && !complete {
                failure = Some("goals left after the last step".into());
            }
            block.repl_replays = failure.is_none();
            block.repl_failure = failure.map(|f| f.chars().take(200).collect());
        }
        tracing::info!(
            module,
            blocks = work.blocks.len(),
            repl_replays = work.blocks.iter().filter(|b| b.repl_replays).count(),
            "workload"
        );
        out.push(work);
    }
    Ok(Workload { modules: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "\
namespace X

lemma a (n : Nat) : n + 0 = n := by
  simp

@[simp]
private theorem b (p q : Prop) (hp : p) (hq : q) :
    p ∧ q := by
  constructor
  <;> assumption

def f : Nat := by
  exact 1

theorem c : True ∧ True := by
  refine ⟨?_, ?_⟩
  · trivial
  · exact
      trivial
termination_by 0

theorem d : 1 = 1 := rfl
end X";

    fn source() -> Source {
        Source {
            module: "M".into(),
            header: String::new(),
            body: BODY.into(),
        }
    }

    #[test]
    fn blocks_of_theorems_split_into_their_steps() {
        let work = prepare(source());
        let steps: Vec<Vec<&str>> = work
            .blocks
            .iter()
            .map(|b| b.steps.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(
            steps,
            vec![
                vec!["simp"],
                vec!["constructor\n<;> assumption"],
                vec!["refine ⟨?_, ?_⟩", "· trivial", "· exact\n    trivial"],
            ],
            "the def's block stays"
        );
        assert_eq!(work.blocks[2].steps[1].line, 17);
        assert_eq!(work.blocks[2].base, 2);
    }

    #[test]
    fn the_sorried_body_keeps_everything_else() {
        let work = prepare(source());
        let lines: Vec<&str> = work.sorried.split('\n').collect();
        for b in &work.blocks {
            assert_eq!(lines[b.sorry_line as usize - 1], "  sorry");
        }
        assert!(work.sorried.contains("def f : Nat := by\n  exact 1"));
        assert!(work.sorried.contains("termination_by 0\n\ntheorem d"));
        let filled = work.filled(&work.blocks[0], "omega");
        assert!(filled.contains("n + 0 = n := by\n  omega\n"), "{filled}");
        let whole = work.blocks[2].whole();
        assert_eq!(whole, "refine ⟨?_, ?_⟩\n· trivial\n· exact\n    trivial");
    }
}
