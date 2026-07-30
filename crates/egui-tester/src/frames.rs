use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub use egui_tester_witness::FrameSample;

use crate::{ActionReceipt, Application, Error, Result};

/// Reader for the lossless product frame-timing journal.
#[derive(Debug)]
pub struct FrameProbe {
    path: PathBuf,
    launch: String,
}

impl FrameProbe {
    pub(crate) fn sealed(path: impl Into<PathBuf>, launch: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            launch: launch.into(),
        }
    }

    pub fn read(&self) -> Result<FrameTrace> {
        match egui_tester_witness::read_frame_journal(&self.path, &self.launch) {
            Ok(samples) => Ok(FrameTrace::new(samples)),
            Err(egui_tester_witness::Error::Io {
                operation,
                path,
                source,
            }) => Err(Error::Io {
                operation,
                path,
                source,
            }),
            Err(error) => Err(Error::FrameJournal {
                path: self.path.clone(),
                detail: error.to_string(),
            }),
        }
    }

    /// Wait until the journal spans an input gesture, then isolate its frames.
    pub fn trace(
        &self,
        app: &Application<'_>,
        action: &ActionReceipt,
        timeout: Duration,
    ) -> Result<FrameTrace> {
        let deadline = Instant::now() + timeout;
        loop {
            app.ensure_running(format!("frame trace for {}", action.action()))?;
            match self.read() {
                Ok(trace)
                    if trace
                        .samples
                        .last()
                        .is_some_and(|sample| sample.begun_ns >= action.completed_ns()) =>
                {
                    return Ok(trace.during(action));
                }
                Ok(_) | Err(Error::Io { .. }) => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: format!("frame trace through {}", action.action()),
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(8));
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Ordered frame samples covering a launch or one input gesture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameTrace {
    samples: Vec<FrameSample>,
}

impl FrameTrace {
    fn new(samples: Vec<FrameSample>) -> Self {
        Self { samples }
    }

    #[must_use]
    pub fn samples(&self) -> &[FrameSample] {
        &self.samples
    }

    #[must_use]
    pub fn during(&self, action: &ActionReceipt) -> Self {
        Self::new(
            self.samples
                .iter()
                .copied()
                .filter(|sample| {
                    sample.begun_ns >= action.gesture_started_ns()
                        && sample.begun_ns <= action.completed_ns()
                })
                .collect(),
        )
    }

    pub fn adjudicate(
        &self,
        operation: impl Into<String>,
        budget: CadenceBudget,
    ) -> Result<CadenceReport> {
        let operation = operation.into();
        let report = CadenceReport::forge(&self.samples).ok_or_else(|| Error::Timing {
            operation: operation.clone(),
            detail: "frame trace contains fewer than two samples".to_owned(),
        })?;
        if report.frames < budget.minimum_frames {
            return Err(Error::Timing {
                operation,
                detail: format!(
                    "observed {} frames, requires at least {}",
                    report.frames, budget.minimum_frames
                ),
            });
        }
        for (name, observed, limit) in [
            ("median cadence", report.p50, budget.p50),
            ("p95 cadence", report.p95, budget.p95),
            ("worst cadence", report.worst, budget.worst),
            ("p95 frame work", report.paint_p95, budget.paint_p95),
        ] {
            if let Some(limit) = limit
                && observed > limit
            {
                return Err(Error::TooSlow {
                    operation: format!("{operation} {name}"),
                    budget: limit,
                    elapsed: observed,
                });
            }
        }
        Ok(report)
    }
}

/// Distributional responsiveness contract for sustained interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CadenceBudget {
    minimum_frames: usize,
    p50: Option<Duration>,
    p95: Option<Duration>,
    worst: Option<Duration>,
    paint_p95: Option<Duration>,
}

impl Default for CadenceBudget {
    fn default() -> Self {
        Self {
            minimum_frames: 2,
            p50: None,
            p95: None,
            worst: None,
            paint_p95: None,
        }
    }
}

impl CadenceBudget {
    #[must_use]
    pub const fn minimum_frames(mut self, minimum: usize) -> Self {
        self.minimum_frames = minimum;
        self
    }

    #[must_use]
    pub const fn p50(mut self, limit: Duration) -> Self {
        self.p50 = Some(limit);
        self
    }

    #[must_use]
    pub const fn p95(mut self, limit: Duration) -> Self {
        self.p95 = Some(limit);
        self
    }

    #[must_use]
    pub const fn worst(mut self, limit: Duration) -> Self {
        self.worst = Some(limit);
        self
    }

    #[must_use]
    pub const fn paint_p95(mut self, limit: Duration) -> Self {
        self.paint_p95 = Some(limit);
        self
    }
}

/// Raw product frame statistics from timestamps captured on the UI thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CadenceReport {
    pub frames: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub worst: Duration,
    pub paint_p95: Duration,
}

impl CadenceReport {
    fn forge(samples: &[FrameSample]) -> Option<Self> {
        if samples.len() < 2 {
            return None;
        }
        let mut cadence = samples
            .windows(2)
            .map(|pair| Duration::from_nanos(pair[1].begun_ns.saturating_sub(pair[0].begun_ns)))
            .collect::<Vec<_>>();
        let mut paint = samples
            .iter()
            .map(|sample| {
                Duration::from_nanos(sample.surface_presented_ns.saturating_sub(sample.begun_ns))
            })
            .collect::<Vec<_>>();
        cadence.sort_unstable();
        paint.sort_unstable();
        Some(Self {
            frames: samples.len(),
            p50: percentile(&cadence, 50),
            p95: percentile(&cadence, 95),
            worst: *cadence.last()?,
            paint_p95: percentile(&paint, 95),
        })
    }
}

fn percentile(sorted: &[Duration], percentage: usize) -> Duration {
    let rank = percentage
        .saturating_mul(sorted.len())
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted[rank]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_uses_unmodified_ui_thread_timestamps() {
        let samples = vec![sample(0, 0, 10), sample(1, 20, 30), sample(2, 40, 50)];
        let report = CadenceReport::forge(&samples).expect("cadence");
        assert_eq!(report.p50, Duration::from_nanos(20));
        assert_eq!(report.paint_p95, Duration::from_nanos(10));
    }

    #[test]
    fn gesture_trace_excludes_an_in_flight_predecessor() {
        let trace = FrameTrace::new(vec![sample(0, 0, 10), sample(1, 20, 30), sample(2, 40, 50)]);
        let action = ActionReceipt::for_test(15, 35, 45);
        assert_eq!(
            trace
                .during(&action)
                .samples()
                .iter()
                .map(|sample| sample.frame)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    fn sample(frame: u64, begun_ns: u64, surface_presented_ns: u64) -> FrameSample {
        FrameSample {
            frame,
            surface_sequence: frame + 1,
            begun_ns,
            observed_ns: begun_ns + 5,
            surface_presented_ns,
        }
    }
}
