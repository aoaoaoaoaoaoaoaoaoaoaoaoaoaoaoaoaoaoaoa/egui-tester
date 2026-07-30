use std::{
    collections::{BTreeSet, VecDeque},
    io::ErrorKind,
    marker::PhantomData,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub use egui_tester_witness::Anchor;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{ActionReceipt, Application, Error, PerformanceBudget, Result, Timed, error::io};

/// One atomic witness snapshot.
#[derive(Clone, Debug, Deserialize)]
pub struct ProbeFrame<S = Value> {
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
    pub state: S,
}

impl<S> ProbeFrame<S> {
    #[must_use]
    pub fn anchor(&self, name: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|anchor| anchor.name == name)
    }
}

/// Reader for the standard atomic one-way witness file.
#[derive(Debug)]
pub struct Probe<S = Value> {
    path: PathBuf,
    expected_launch: Option<String>,
    last_frame: u64,
    journal: Option<egui_tester_witness::ObservationJournal>,
    journal_queue: VecDeque<ProbeFrame<S>>,
    state: PhantomData<fn() -> S>,
}

pub type JsonProbe = Probe<Value>;

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

impl Probe<Value> {
    /// Open a legacy or externally configured witness without a launch seal.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            expected_launch: None,
            last_frame: 0,
            journal: None,
            journal_queue: VecDeque::new(),
            state: PhantomData,
        }
    }

    pub(crate) fn sealed(path: impl Into<PathBuf>, launch: impl Into<String>) -> Self {
        let path = path.into();
        let launch = launch.into();
        Self {
            journal: Some(egui_tester_witness::ObservationJournal::sealed(
                &path,
                launch.clone(),
            )),
            path,
            expected_launch: Some(launch),
            last_frame: 0,
            journal_queue: VecDeque::new(),
            state: PhantomData,
        }
    }

    /// Decode the product-owned state into an acceptance-owned observation.
    ///
    /// The observation may deliberately deserialize only the fields consumed
    /// by its stories. This keeps the witness one-way without coupling an
    /// acceptance executable to product internals.
    #[must_use]
    pub fn typed<T>(self) -> Probe<T> {
        Probe {
            path: self.path,
            expected_launch: self.expected_launch,
            last_frame: self.last_frame,
            journal: self.journal,
            journal_queue: VecDeque::new(),
            state: PhantomData,
        }
    }
}

impl<S> Probe<S> {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<S: DeserializeOwned> Probe<S> {
    pub fn read(&self) -> Result<ProbeFrame<S>> {
        let bytes = std::fs::read(&self.path).map_err(|err| io("read witness", &self.path, err))?;
        let frame =
            serde_json::from_slice::<ProbeFrame<S>>(&bytes).map_err(|err| Error::Probe {
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
        mut predicate: impl FnMut(&ProbeFrame<S>) -> bool,
    ) -> Result<ProbeFrame<S>> {
        self.wait_inspecting(app, timeout, description, |frame| {
            predicate(frame).then_some(()).ok_or(None)
        })
    }

    pub fn wait_checked(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
        description: impl Into<String>,
        mut predicate: impl FnMut(&ProbeFrame<S>) -> std::result::Result<(), String>,
    ) -> Result<ProbeFrame<S>> {
        self.wait_inspecting(app, timeout, description, |frame| {
            predicate(frame).map_err(Some)
        })
    }

    fn wait_inspecting(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
        description: impl Into<String>,
        mut inspect: impl FnMut(&ProbeFrame<S>) -> std::result::Result<(), Option<String>>,
    ) -> Result<ProbeFrame<S>> {
        let description = description.into();
        let deadline = Instant::now() + timeout;
        let mut invalid = None;
        let mut last_mismatch = None;
        loop {
            app.ensure_running(&description)?;
            match self.read() {
                Ok(frame) => {
                    invalid = None;
                    match inspect(&frame) {
                        Ok(()) => {
                            self.last_frame = frame.frame;
                            return Ok(frame);
                        }
                        Err(mismatch) => last_mismatch = mismatch,
                    }
                }
                Err(Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {}
                Err(err @ Error::Probe { .. }) => invalid = Some(err),
                Err(err) => return Err(err),
            }
            if Instant::now() >= deadline {
                return Err(invalid.unwrap_or_else(|| {
                    last_mismatch.map_or_else(
                        || Error::Timeout {
                            waiting: description.clone(),
                            timeout,
                        },
                        |last_mismatch| Error::Condition {
                            waiting: description.clone(),
                            timeout,
                            last_mismatch,
                        },
                    )
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

    pub fn wait_fresh(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
    ) -> Result<ProbeFrame<S>> {
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
    ) -> Result<ProbeFrame<S>> {
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
        mut project: impl FnMut(&ProbeFrame<S>) -> Option<T>,
    ) -> Result<ProbeFrame<S>> {
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
        mut predicate: impl FnMut(&ProbeFrame<S>) -> bool,
    ) -> Result<Timed<ProbeFrame<S>>> {
        self.wait_budgeted_checked(app, receipt, budget, description, |frame| {
            predicate(frame)
                .then_some(())
                .ok_or_else(|| "semantic predicate did not match".to_owned())
        })
    }

    pub fn wait_budgeted_checked(
        &mut self,
        app: &Application<'_>,
        receipt: &ActionReceipt,
        budget: PerformanceBudget,
        description: impl Into<String>,
        mut predicate: impl FnMut(&ProbeFrame<S>) -> std::result::Result<(), String>,
    ) -> Result<Timed<ProbeFrame<S>>> {
        let description = description.into();
        if self.journal.is_some() {
            return self.wait_budgeted_journal(app, receipt, budget, description, predicate);
        }
        let prior = self.last_frame;
        let endpoint = budget.endpoint();
        let frame = self.wait_checked(
            app,
            budget.functional_timeout(),
            description.clone(),
            |frame| {
                if frame.frame <= prior {
                    return Err(format!(
                        "frame {} is not newer than prior frame {prior}",
                        frame.frame
                    ));
                }
                if frame.begun_ns < receipt.triggered_ns() {
                    return Err(format!(
                        "frame began at {} before input trigger {}",
                        frame.begun_ns,
                        receipt.triggered_ns()
                    ));
                }
                let timestamp = endpoint.timestamp(frame);
                if timestamp < receipt.triggered_ns() {
                    return Err(format!(
                        "{endpoint:?} timestamp {timestamp} predates input trigger {}",
                        receipt.triggered_ns()
                    ));
                }
                predicate(frame)
            },
        )?;
        budget.adjudicate(description, receipt, endpoint.timestamp(&frame), frame)
    }

    fn wait_budgeted_journal(
        &mut self,
        app: &Application<'_>,
        receipt: &ActionReceipt,
        budget: PerformanceBudget,
        description: String,
        mut predicate: impl FnMut(&ProbeFrame<S>) -> std::result::Result<(), String>,
    ) -> Result<Timed<ProbeFrame<S>>> {
        let prior = self.last_frame;
        let endpoint = budget.endpoint();
        let timeout = budget.functional_timeout();
        let deadline = Instant::now() + timeout;
        let mut last_mismatch = None;
        loop {
            app.ensure_running(&description)?;
            self.refill_journal()?;
            while let Some(frame) = self.journal_queue.pop_front() {
                self.validate(&frame)?;
                if frame.frame <= prior || frame.begun_ns < receipt.triggered_ns() {
                    continue;
                }
                let timestamp = endpoint.timestamp(&frame);
                if timestamp < receipt.triggered_ns() {
                    continue;
                }
                match predicate(&frame) {
                    Ok(()) => {
                        self.last_frame = frame.frame;
                        return budget.adjudicate(description, receipt, timestamp, frame);
                    }
                    Err(mismatch) => last_mismatch = Some(mismatch),
                }
            }
            if Instant::now() >= deadline {
                return Err(last_mismatch.map_or_else(
                    || Error::Timeout {
                        waiting: description.clone(),
                        timeout,
                    },
                    |last_mismatch| Error::Condition {
                        waiting: description.clone(),
                        timeout,
                        last_mismatch,
                    },
                ));
            }
            thread::sleep(Duration::from_millis(8));
        }
    }

    fn refill_journal(&mut self) -> Result<()> {
        let Some(journal) = &mut self.journal else {
            return Ok(());
        };
        match journal.read_new::<ProbeFrame<S>>() {
            Ok(frames) => {
                self.journal_queue.extend(frames);
                Ok(())
            }
            Err(egui_tester_witness::Error::Io { source, .. })
                if source.kind() == ErrorKind::NotFound =>
            {
                Ok(())
            }
            Err(egui_tester_witness::Error::Io {
                source,
                operation,
                path,
            }) => Err(Error::Io {
                operation,
                path,
                source,
            }),
            Err(error) => Err(Error::Probe {
                path: journal.path().to_owned(),
                detail: error.to_string(),
            }),
        }
    }

    fn validate(&self, frame: &ProbeFrame<S>) -> Result<()> {
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
