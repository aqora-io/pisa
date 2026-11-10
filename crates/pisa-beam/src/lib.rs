//! Drives Lean Beam's MCP server (`lean-beam-mcp`, shipped in Lean toolchains
//! from v4.36) over stdin/stdout: tactic probes at file positions, chained
//! speculative states, goals and the problems left in a file.
//!
//! Beam reads documents from disk, inside a Lake workspace ([`Workspace`]);
//! its client writes them, then calls `update` or `sync` for a document
//! version that later probes name. Beam is not a sandbox: run it under one.

mod client;
pub mod goal;
pub mod pos;
mod tools;
mod workspace;

pub use client::{Beam, BeamError};
pub use pos::LspPos;
pub use tools::{
    Diagnostic, Diagnostics, Doc, Goal, Handle, Hyp, Mode, ProofState, Range, Readiness,
    RunMessage, RunResult, Save, Sync, Todo, TodoItem, Update,
};
pub use workspace::{Packages, Workspace};
