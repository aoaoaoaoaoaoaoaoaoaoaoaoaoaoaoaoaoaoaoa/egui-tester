use std::{
    collections::BTreeSet,
    io::ErrorKind,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub use egui_tester_witness::Anchor;
use serde::Deserialize;
use serde_json::Value;

use crate::{ActionReceipt, Application, Error, PerformanceBudget, Result, Timed, error::io};

/// One atomic witness snapshot.
#[derive(Clone, Debug, Deserialize)]
pub struct ProbeFrame {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub launch: String,
    pub frame: u64,
    #[serde(default)]
    pub begun_ns: u64,
    #[serde(default)]
    pub observed_ns: u64,
    #[serde(default)]
    pub presented_ns: u64,
    #[serde(default)]
    pub presentation: u64,
    #[serde(default)]
    pub ppp: Option<f32>,
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub state: Value,
}

impl ProbeFrame {
    #[must_use]
    pub fn anchor(&self, name: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|anchor| anchor.name == name)
    }
}

/// Reader for the standard atomic one-way witness file.
#[derive(Debug)]
pub struct JsonProbe {
    path: PathBuf,
    expected_launch: Option<String>,
    last_frame: u64,
}

struct Stability<T>(Option<(T, Instant)>);

impl<T: PartialEq> Stability<T> {
    const fn new() -> Self {
        Self(None)
    }

    fn observe(&mut self, value: T, now: Instant, quiet: Duration) -> bool {
        match &mut self.0 {
            Some((prior, since)) if prior == &value => now.duration_since(*since) >= quiet,
            slot => {
                *slot = Some((value, now));
                false
            }
        }
    }

    fn break_streak(&mut self) {
        self.0 = None;
    }
}

impl JsonProbe {
    /// Open a legacy or externally configured witness without a launch seal.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            expected_launch: None,
            last_frame: 0,
        }
    }

    pub(crate) fn sealed(path: impl Into<PathBuf>, launch: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            expected_launch: Some(launch.into()),
            last_frame: 0,
        }
    }

    pub fn read(&self) -> Result<ProbeFrame> {
        let bytes = std::fs::read(&self.path).map_err(|err| io("read witness", &self.path, err))?;
        let frame = serde_json::from_slice::<ProbeFrame>(&bytes).map_err(|err| Error::Probe {
            path: self.path.clone(),
            detail: err.to_string(),
        })?;
        self.validate(&frame)?;
        Ok(frame)
    }

    pub fn wait(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
        description: impl Into<String>,
        mut predicate: impl FnMut(&ProbeFrame) -> bool,
    ) -> Result<ProbeFrame> {
        let description = description.into();
        let deadline = Instant::now() + timeout;
        let mut invalid = None;
        loop {
            app.ensure_running(&description)?;
            match self.read() {
                Ok(frame) if predicate(&frame) => {
                    self.last_frame = frame.frame;
                    return Ok(frame);
                }
                Ok(_) => invalid = None,
                Err(Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {}
                Err(err @ Error::Probe { .. }) => invalid = Some(err),
                Err(err) => return Err(err),
            }
            if Instant::now() >= deadline {
                return Err(invalid.unwrap_or(Error::Timeout {
                    waiting: description,
                    timeout,
                }));
            }
            thread::sleep(Duration::from_millis(12));
        }
    }

    pub fn wait_anchor(
        &mut self,
        app: &Application<'_>,
        name: &str,
        timeout: Duration,
    ) -> Result<Anchor> {
        let frame = self.wait(app, timeout, format!("witness anchor `{name}`"), |frame| {
            frame.anchor(name).is_some()
        })?;
        frame.anchor(name).cloned().ok_or_else(|| Error::Probe {
            path: self.path.clone(),
            detail: format!("anchor `{name}` vanished from the matching frame"),
        })
    }

    pub fn wait_fresh(&mut self, app: &Application<'_>, timeout: Duration) -> Result<ProbeFrame> {
        let prior = self.last_frame;
        self.wait(
            app,
            timeout,
            format!("witness frame newer than {prior}"),
            |frame| frame.frame > prior,
        )
    }

    pub fn wait_presented(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
    ) -> Result<ProbeFrame> {
        self.wait(
            app,
            timeout,
            "first product frame to be presented",
            |frame| frame.presentation > 0,
        )
    }

    /// Wait until a semantic projection remains unchanged for `quiet`.
    ///
    /// This fences product kinetics such as inertial scrolling without making
    /// pixel animation, witness polling, or an arbitrary sleep part of the
    /// product contract.
    pub fn wait_stable<T: PartialEq>(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
        quiet: Duration,
        description: impl Into<String>,
        mut project: impl FnMut(&ProbeFrame) -> Option<T>,
    ) -> Result<ProbeFrame> {
        let description = description.into();
        let deadline = Instant::now() + timeout;
        let mut stable = Stability::new();
        let mut invalid = None;
        loop {
            app.ensure_running(&description)?;
            match self.read() {
                Ok(frame) => {
                    invalid = None;
                    let now = Instant::now();
                    if let Some(value) = project(&frame) {
                        if stable.observe(value, now, quiet) {
                            self.last_frame = frame.frame;
                            return Ok(frame);
                        }
                    } else {
                        stable.break_streak();
                    }
                }
                Err(Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {}
                Err(err @ Error::Probe { .. }) => invalid = Some(err),
                Err(err) => return Err(err),
            }
            if Instant::now() >= deadline {
                return Err(invalid.unwrap_or(Error::Timeout {
                    waiting: description,
                    timeout,
                }));
            }
            thread::sleep(Duration::from_millis(12));
        }
    }

    /// Wait for a fresh semantic result and enforce its production latency.
    ///
    /// The end timestamp was captured inside the application before witness
    /// serialization. Reader polling and filesystem latency are excluded.
    pub fn wait_budgeted(
        &mut self,
        app: &Application<'_>,
        receipt: &ActionReceipt,
        budget: PerformanceBudget,
        description: impl Into<String>,
        mut predicate: impl FnMut(&ProbeFrame) -> bool,
    ) -> Result<Timed<ProbeFrame>> {
        let description = description.into();
        let prior = self.last_frame;
        let endpoint = budget.endpoint();
        let frame = self.wait(
            app,
            budget.functional_timeout(),
            description.clone(),
            |frame| {
                frame.frame > prior
                    && frame.begun_ns >= receipt.triggered_ns()
                    && endpoint.timestamp(frame) >= receipt.triggered_ns()
                    && predicate(frame)
            },
        )?;
        budget.adjudicate(description, receipt, endpoint.timestamp(&frame), frame)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn validate(&self, frame: &ProbeFrame) -> Result<()> {
        if let Some(expected) = &self.expected_launch {
            if frame.schema != egui_tester_witness::SCHEMA {
                return self.invalid(format!(
                    "expected schema {}, found {}",
                    egui_tester_witness::SCHEMA,
                    frame.schema
                ));
            }
            if &frame.launch != expected {
                return self.invalid(format!(
                    "launch nonce mismatch: expected `{expected}`, found `{}`",
                    frame.launch
                ));
            }
            if frame.begun_ns == 0
                || frame.observed_ns == 0
                || frame.presented_ns == 0
                || frame.presentation == 0
            {
                return self.invalid(
                    "sealed witness omitted begin, observation, or presentation timestamp"
                        .to_owned(),
                );
            }
            if frame.observed_ns < frame.begun_ns || frame.presented_ns < frame.observed_ns {
                return self.invalid("sealed witness timestamps are not monotonic".to_owned());
            }
        }
        if frame.ppp.is_some_and(|ppp| !ppp.is_finite() || ppp <= 0.0) {
            return self.invalid("pixels per point must be positive and finite".to_owned());
        }
        let mut names = BTreeSet::new();
        for anchor in &frame.anchors {
            anchor.validate().map_err(|err| Error::Probe {
                path: self.path.clone(),
                detail: err.to_string(),
            })?;
            if !names.insert(&anchor.name) {
                return self.invalid(format!("duplicate anchor `{}`", anchor.name));
            }
        }
        Ok(())
    }

    fn invalid<T>(&self, detail: String) -> Result<T> {
        Err(Error::Probe {
            path: self.path.clone(),
            detail,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_anchors_are_not_ambiguous() {
        let probe = JsonProbe::new("/unread");
        let frame = ProbeFrame {
            schema: 0,
            launch: String::new(),
            frame: 1,
            begun_ns: 0,
            observed_ns: 0,
            presented_ns: 0,
            presentation: 0,
            ppp: Some(1.0),
            anchors: vec![
                Anchor::physical("same", [0.0, 0.0, 1.0, 1.0]).expect("first"),
                Anchor::physical("same", [1.0, 1.0, 2.0, 2.0]).expect("second"),
            ],
            state: Value::Null,
        };
        assert!(probe.validate(&frame).is_err());
    }

    #[test]
    fn semantic_stability_restarts_when_the_projection_moves() {
        let epoch = Instant::now();
        let mut stability = Stability::new();
        assert!(!stability.observe(1, epoch, Duration::from_millis(50)));
        assert!(!stability.observe(
            1,
            epoch + Duration::from_millis(40),
            Duration::from_millis(50)
        ));
        assert!(!stability.observe(
            2,
            epoch + Duration::from_millis(60),
            Duration::from_millis(50)
        ));
        assert!(stability.observe(
            2,
            epoch + Duration::from_millis(110),
            Duration::from_millis(50)
        ));
    }
}
