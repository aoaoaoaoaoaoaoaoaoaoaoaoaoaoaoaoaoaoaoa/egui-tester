use std::time::Duration;

use crate::{Error, ProbeFrame, Result};

/// Product endpoint adjudicated by a performance budget.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PerformanceEndpoint {
    /// Native input through completed product-state work, before telemetry.
    #[default]
    Observation,
    /// Native input through presentation of the corresponding product frame.
    Presentation,
}

impl PerformanceEndpoint {
    pub(crate) const fn timestamp<S>(self, frame: &ProbeFrame<S>) -> u64 {
        match self {
            Self::Observation => frame.observed_ns,
            Self::Presentation => frame.presented_ns,
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

/// Production responsiveness contract plus a larger functional deadline.
///
/// Observation budgets end before witness work. Presentation budgets end at
/// the corresponding real product frame. Harness polling and witness I/O
/// cannot consume either. `timeout` only bounds a missing result; it never
/// dilates the production budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerformanceBudget {
    production: Duration,
    timeout: Duration,
    endpoint: PerformanceEndpoint,
}

impl PerformanceBudget {
    #[must_use]
    pub fn new(production: Duration) -> Self {
        let timeout = production.saturating_mul(10).max(Duration::from_secs(2));
        Self {
            production,
            timeout,
            endpoint: PerformanceEndpoint::Observation,
        }
    }

    /// Judge the complete user-visible frame rather than semantic work alone.
    #[must_use]
    pub const fn through_presentation(mut self) -> Self {
        self.endpoint = PerformanceEndpoint::Presentation;
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub const fn production(self) -> Duration {
        self.production
    }

    #[must_use]
    pub const fn functional_timeout(self) -> Duration {
        self.timeout
    }

    #[must_use]
    pub const fn endpoint(self) -> PerformanceEndpoint {
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
        if elapsed > self.production {
            return Err(Error::TooSlow {
                operation,
                budget: self.production,
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
    fn functional_timeout_never_dilates_the_production_budget() {
        let budget =
            PerformanceBudget::new(Duration::from_millis(20)).timeout(Duration::from_secs(30));
        let receipt = ActionReceipt::for_test(1_000_000, 1_000_000, 1_000_000);
        let error = budget
            .adjudicate("repaint", &receipt, 22_000_001, ())
            .expect_err("twenty-one milliseconds must breach twenty");
        assert!(matches!(error, Error::TooSlow { .. }));
    }
}
