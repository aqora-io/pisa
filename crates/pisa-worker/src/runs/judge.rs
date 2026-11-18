//! Judging a run's pairs with `lake comparator`, each in turn, in one
//! directory, so that Lake builds each module once.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pisa_protocol::api::{Error, ErrorKind, KernelVerdict, PairError, PairErrorKind};
use pisa_protocol::wire::ExportedFile;

use super::{
    OUTPUT_LINES, accepted_kernels, classify, comparator_args, external_config, failed_kernels,
    needs_external, tail, with_cap, write,
};
use crate::launcher::Launcher;

/// The modules a run's pairs are judged on.
pub struct ModulesToJudge {
    /// The directory the pairs are judged in, as a Lake project.
    pub root: PathBuf,
    /// The project's files but the pairs' configurations: written into
    /// `root`, unless it holds them already, when only its README is written
    /// again, so that Lake finds what it built.
    pub sources: Vec<ExportedFile>,
}

/// A pair to judge.
pub struct PairToJudge {
    /// The spec snapshot's module, `PisaSnap.s<id>`.
    pub spec_module: String,
    /// The head's module.
    pub head_module: String,
    /// Comparator's configuration for the pair, as JSON.
    pub configuration: String,
    /// How many of the spec's theorems the configuration names.
    pub theorems: u64,
}

/// What comparator made of a pair.
pub struct Judgment {
    /// Why it wasn't accepted; `None` if it was.
    pub error: Option<PairError>,
    /// Comparator's exit code; `None` if it didn't run, or ran out of time.
    pub exit_code: Option<i32>,
    /// The end of what comparator printed.
    pub output: String,
    pub kernels: Vec<KernelVerdict>,
}

pub struct JudgeOptions {
    /// Whether every bundled kernel checks the solution, not only Lean's.
    pub paranoid: bool,
    /// How long comparator has for a pair.
    pub timeout: Duration,
    /// What the first pair comparator runs has besides, as it builds the
    /// modules that later pairs replay.
    pub build_timeout: Duration,
}

/// Judges `pairs` with `lake comparator`, each in turn, in `modules.root`,
/// on `slot`, whose memory should be free. Each pair's configuration is
/// `comparator/<n>.json` there, `n` counting from 1. `project` is the
/// worker's, whose packages the run builds on.
pub async fn judge(
    launcher: &Launcher,
    slot: usize,
    modules: &ModulesToJudge,
    project: Option<&Path>,
    pairs: &[PairToJudge],
    options: &JudgeOptions,
) -> Result<Vec<Judgment>, Error> {
    let dir = &modules.root;
    let configs: Vec<ExportedFile> = (pairs.iter().enumerate())
        .map(|(n, pair)| ExportedFile {
            path: format!("comparator/{}.json", n + 1),
            text: pair.configuration.clone(),
        })
        .collect();
    let files: Vec<&ExportedFile> = modules.sources.iter().chain(&configs).collect();
    write(dir, &files, project).map_err(|e| {
        Error::new(
            ErrorKind::Internal,
            format!("writing the run's project: {e}"),
        )
    })?;
    // The first pair comparator runs builds the run's modules, which later
    // pairs replay, so it has the build's time too.
    let mut building = options.build_timeout;
    let mut judgments = Vec::new();
    // The toolchain's `bin`, found when a pair needs its kernels.
    let mut bin: Option<PathBuf> = None;
    for (pair, config) in pairs.iter().zip(&configs) {
        // Nothing for comparator to judge, which it would refuse to start.
        if pair.theorems == 0 {
            judgments.push(Judgment {
                error: Some(PairError {
                    kind: PairErrorKind::NothingToJudge,
                    message: "the spec names no theorem comparator can judge".into(),
                }),
                exit_code: None,
                output: String::new(),
                kernels: Vec::new(),
            });
            continue;
        }
        let external = needs_external(&pair.configuration, options.paranoid);
        if external {
            if bin.is_none() {
                bin = Some(launcher.toolchain_bin(dir).await.map_err(|e| {
                    Error::new(
                        ErrorKind::Internal,
                        format!("finding the toolchain's kernels: {e}"),
                    )
                })?);
            }
            let text =
                external_config(&pair.configuration, bin.as_deref().unwrap_or(Path::new("")));
            std::fs::write(dir.join(&config.path), text).map_err(|e| {
                Error::new(
                    ErrorKind::Internal,
                    format!("writing the configuration: {e}"),
                )
            })?;
        }
        let args = comparator_args(&config.path, options.paranoid, external);
        let oom_before = launcher.oom_kills(slot);
        let limit = options.timeout + std::mem::take(&mut building);
        let ran = launcher.lake(slot, &args, dir, limit).await.map_err(|e| {
            Error::new(ErrorKind::Internal, format!("running lake comparator: {e}"))
        })?;
        let oom = launcher.oom_kills(slot) > oom_before;
        // From the whole output, before it is cut to its end.
        let kernels = match ran.code {
            Some(0) => accepted_kernels(options.paranoid, external),
            _ => failed_kernels(&ran.output, external),
        };
        judgments.push(Judgment {
            error: classify(&ran, oom).map(|e| with_cap(e, launcher.memory_max_mb(slot))),
            exit_code: ran.code,
            output: tail(&ran.output, OUTPUT_LINES),
            kernels,
        });
    }
    Ok(judgments)
}
