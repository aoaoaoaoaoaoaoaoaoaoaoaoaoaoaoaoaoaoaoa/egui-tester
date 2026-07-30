use std::time::Duration;

use crate::{Error, ProbeFrame, Result};

/// Product timestamp used to complete one reaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReactionEndpoint {
    /// Native input through completed product-state work, before telemetry.
    #[default]
    Observation,
    /// Native input through `wgpu` surface present submission.
    ///
    /// This is not a compositor scanout or physical-display completion proof.
    SurfacePresent,
}

impl ReactionEndpoint {
    pub(crate) const fn timestamp<S>(self, frame: &ProbeFrame<S>) -> u64 {
        match self {
            Self::Observation => frame.observed_ns,
            Self::SurfacePresent => frame.surface_presented_ns,
        }
    }
}

/// Monotonic bounds and reaction trigger for one native input gesture.
#[must_use = "an action receipt may be used to enforce a performance budget"]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionReceipt {
    gesture_started_ns: u64,
    triggered_ns: u64,
    completed_ns: u64,
    action: String,
}

impl ActionReceipt {
    pub(crate) fn begin(action: impl Into<String>) -> Self {
        let gesture_started_ns = egui_tester_witness::monotonic_ns();
        Self {
            gesture_started_ns,
            triggered_ns: gesture_started_ns,
            completed_ns: gesture_started_ns,
            action: action.into(),
        }
    }

    pub(crate) fn trigger(mut self) -> Self {
        self.triggered_ns = egui_tester_witness::monotonic_ns();
        self
    }

    pub(crate) fn finish(mut self) -> Self {
        self.completed_ns = egui_tester_witness::monotonic_ns();
        self
    }

    #[must_use]
    pub const fn gesture_started_ns(&self) -> u64 {
        self.gesture_started_ns
    }

    #[must_use]
    pub const fn triggered_ns(&self) -> u64 {
        self.triggered_ns
    }

    #[must_use]
    pub const fn completed_ns(&self) -> u64 {
        self.completed_ns
    }

    #[must_use]
    pub fn action(&self) -> &str {
        &self.action
    }

    #[cfg(test)]
    pub(crate) fn for_test(gesture_started_ns: u64, triggered_ns: u64, completed_ns: u64) -> Self {
        Self {
            gesture_started_ns,
            triggered_ns,
            completed_ns,
            action: "test action".to_owned(),
        }
    }
}

/// Deadline for one reaction, optionally carrying a production latency limit.
///
/// A functional budget bounds a missing cue without making a performance
/// claim. A performance budget additionally rejects reactions whose product
/// timestamp breaches `production`. Observation ends before witness work;
/// surface-present ends after the corresponding `wgpu` present call. Harness
/// polling and witness I/O consume neither.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReactionBudget {
    production: Option<Duration>,
    timeout: Duration,
    endpoint: ReactionEndpoint,
}

impl ReactionBudget {
    /// Bound functional progress without adjudicating production latency.
    #[must_use]
    pub const fn functional(timeout: Duration) -> Self {
        Self {
            production: None,
            timeout,
            endpoint: ReactionEndpoint::Observation,
        }
    }

    /// Adjudicate production latency, with a larger missing-cue deadline.
    #[must_use]
    pub fn performance(production: Duration) -> Self {
        let timeout = production.saturating_mul(10).max(Duration::from_secs(2));
        Self {
            production: Some(production),
            timeout,
            endpoint: ReactionEndpoint::Observation,
        }
    }

    /// Judge through `wgpu` surface present submission.
    #[must_use]
    pub const fn through_surface_present(mut self) -> Self {
        self.endpoint = ReactionEndpoint::SurfacePresent;
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub const fn production(self) -> Option<Duration> {
        self.production
    }

    #[must_use]
    pub const fn functional_timeout(self) -> Duration {
        self.timeout
    }

    #[must_use]
    pub const fn endpoint(self) -> ReactionEndpoint {
        self.endpoint
    }

    pub(crate) fn adjudicate<T>(
        self,
        operation: impl Into<String>,
        receipt: &ActionReceipt,
        observed_ns: u64,
        value: T,
    ) -> Result<Timed<T>> {
        let operation = operation.into();
        let elapsed_ns = observed_ns
            .checked_sub(receipt.triggered_ns)
            .ok_or_else(|| Error::Timing {
                operation: operation.clone(),
                detail: format!(
                    "application observation {observed_ns} predates input trigger {}",
                    receipt.triggered_ns
                ),
            })?;
        let elapsed = Duration::from_nanos(elapsed_ns);
        if let Some(production) = self.production
            && elapsed > production
        {
            return Err(Error::TooSlow {
                operation,
                budget: production,
                elapsed,
            });
        }
        Ok(Timed { value, elapsed })
    }
}

/// Value accepted within its production budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Timed<T> {
    value: T,
    elapsed: Duration,
}

impl<T> Timed<T> {
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    #[must_use]
    pub fn value(&self) -> &T {
        &self.value
    }

    #[must_use]
    pub fn into_value(self) -> T {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functional_deadline_does_not_adjudicate_latency() {
        let budget = ReactionBudget::functional(Duration::from_secs(30));
        let receipt = ActionReceipt::for_test(1_000_000, 1_000_000, 1_000_000);
        let reaction = budget
            .adjudicate("repaint", &receipt, 22_000_001, ())
            .expect("functional deadline must not claim a production regression");
        assert_eq!(reaction.elapsed(), Duration::from_nanos(21_000_001));
    }

    #[test]
    fn functional_timeout_never_dilates_the_production_budget() {
        let budget =
            ReactionBudget::performance(Duration::from_millis(20)).timeout(Duration::from_secs(30));
        let receipt = ActionReceipt::for_test(1_000_000, 1_000_000, 1_000_000);
        let error = budget
            .adjudicate("repaint", &receipt, 22_000_001, ())
            .expect_err("twenty-one milliseconds must breach twenty");
        assert!(matches!(error, Error::TooSlow { .. }));
    }
}
