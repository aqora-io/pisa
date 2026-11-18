//! What an accept is doing, for its job to say (see `claims.rs`), and where
//! each pair's time and memory went. It all comes from pisa's own steps and
//! the slot's processes, never from what comparator prints: the kernel
//! running is known by its executable among the processes in the slot's
//! cgroup ([`running_kernel`]), and memory by the cgroup's peak, started
//! again at each phase and each kernel.

use std::future::Future;
use std::time::{Duration, Instant};

use pisa_protocol::api::{AcceptPhase, JobProgress, PairPhase, PairSide};
use tokio::sync::watch;

use super::running_kernel;
use crate::cgroup::Peak;
use crate::check::round6;
use crate::launcher::Launcher;

/// How often a phase looks at the slot: for the kernel running, and to bring
/// its seconds and memory up to date.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// Where an accept says what it is doing, for the job that runs it.
pub type ProgressSender = watch::Sender<Option<JobProgress>>;

/// An accept's phases: the one it is in, said to its [`ProgressSender`], and
/// those of the pair being judged that have ended.
pub struct Progress<'a> {
    launcher: &'a Launcher,
    sender: &'a ProgressSender,
    /// The slot it judges on, once it has one.
    slot: Option<usize>,
    peak: Option<Peak>,
    current: Option<Current>,
    ended: Vec<PairPhase>,
}

struct Current {
    phase: AcceptPhase,
    /// While judging, the kernel that was running when it started.
    kernel: Option<&'static str>,
    started: Instant,
}

impl<'a> Progress<'a> {
    pub fn new(launcher: &'a Launcher, sender: &'a ProgressSender) -> Self {
        Progress {
            launcher,
            sender,
            slot: None,
            peak: None,
            current: None,
            ended: Vec::new(),
        }
    }

    /// From now on it runs on `slot`, whose processes and memory its phases
    /// look at.
    pub fn on_slot(&mut self, slot: usize) {
        self.slot = Some(slot);
        self.peak = self.launcher.peak(slot);
    }

    /// Starts `phase`, ending the one before.
    pub fn begin(&mut self, phase: AcceptPhase) {
        self.end();
        self.start(phase, None);
    }

    /// Ends the pair's last phase, and returns the pair's phases.
    pub fn pair_done(&mut self) -> Vec<PairPhase> {
        self.end();
        std::mem::take(&mut self.ended)
    }

    /// Runs `f`, looking at the slot about each second meanwhile.
    pub async fn during<F: Future>(&mut self, f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let first = tokio::time::Instant::now() + LOOK_EVERY;
        let mut look = tokio::time::interval_at(first, LOOK_EVERY);
        loop {
            tokio::select! {
                out = &mut f => return out,
                _ = look.tick() => self.look(),
            }
        }
    }

    /// Brings the phase up to date: while judging, a kernel that started or
    /// ended starts a phase of its own.
    fn look(&mut self) {
        let Some(current) = &self.current else {
            return;
        };
        if let (AcceptPhase::Judging { .. }, Some(slot)) = (current.phase, self.slot) {
            let kernel = running_kernel(self.launcher, slot);
            if kernel != current.kernel {
                let phase = current.phase;
                self.end();
                self.start(phase, kernel);
                return;
            }
        }
        self.say();
    }

    fn start(&mut self, phase: AcceptPhase, kernel: Option<&'static str>) {
        if let Some(peak) = &self.peak {
            peak.reset();
        }
        self.current = Some(Current {
            phase,
            kernel,
            started: Instant::now(),
        });
        self.say();
    }

    /// Ends the current phase, which the pair keeps if it is one of its own.
    fn end(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        let Some(name) = phase_name(current.phase, current.kernel) else {
            return;
        };
        self.ended.push(PairPhase {
            name: name.to_string(),
            seconds: round6(current.started.elapsed().as_secs_f64()),
            peak_memory_mb: self.peak_mb(),
        });
    }

    fn peak_mb(&self) -> Option<u64> {
        self.peak.as_ref()?.mb()
    }

    /// Says the current phase, as it is now.
    fn say(&self) {
        let Some(current) = &self.current else {
            return;
        };
        self.sender.send_replace(Some(JobProgress {
            phase: current.phase,
            kernel: current.kernel.map(String::from),
            seconds: round6(current.started.elapsed().as_secs_f64()),
            peak_memory_mb: self.peak_mb(),
        }));
    }
}

/// A pair's phase's name, as [`PairPhase::name`] says it: `phase`, or the
/// `kernel` comparator was running in it. `None` for linking, which is no
/// pair's.
fn phase_name(phase: AcceptPhase, kernel: Option<&'static str>) -> Option<&'static str> {
    Some(match (phase, kernel) {
        (AcceptPhase::Linking, _) => return None,
        (AcceptPhase::Exporting { side, .. }, _) => match side {
            PairSide::Challenge => "exporting challenge",
            PairSide::Solution => "exporting solution",
        },
        (AcceptPhase::Judging { .. }, None) => "judging",
        (AcceptPhase::Judging { .. }, Some(kernel)) => kernel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairs_phase_is_named_by_its_step_or_its_kernel() {
        let exporting = |side| AcceptPhase::Exporting { pair: 1, side };
        let judging = AcceptPhase::Judging { pair: 2 };
        assert_eq!(phase_name(AcceptPhase::Linking, None), None);
        assert_eq!(
            phase_name(exporting(PairSide::Challenge), None),
            Some("exporting challenge")
        );
        assert_eq!(
            phase_name(exporting(PairSide::Solution), None),
            Some("exporting solution")
        );
        assert_eq!(phase_name(judging, None), Some("judging"));
        assert_eq!(phase_name(judging, Some("con-ron")), Some("con-ron"));
    }
}
