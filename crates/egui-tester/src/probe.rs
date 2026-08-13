use std::{
    collections::{BTreeSet, VecDeque},
    io::ErrorKind,
    marker::PhantomData,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub use egui_tester_witness::Anchor;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{ActionReceipt, Application, Error, ReactionBudget, Result, Timed, error::io};

/// One sealed observation from the standard semantic journal.
#[derive(Clone, Debug, Deserialize)]
pub struct ProbeFrame<S = Value> {
    pub schema: u32,
    pub launch: String,
    pub frame: u64,
    pub begun_ns: u64,
    pub observed_ns: u64,
    pub surface_presented_ns: u64,
    pub surface_sequence: u64,
    pub ppp: f32,
    pub anchors: Vec<Anchor>,
    pub state: S,
}

impl<S> ProbeFrame<S> {
    #[must_use]
    pub fn anchor(&self, name: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|anchor| anchor.name == name)
    }

    /// Find a named target only when it owns keyboard focus.
    #[must_use]
    pub fn focused_anchor(&self, name: &str) -> Option<&Anchor> {
        self.anchor(name).filter(|anchor| anchor.focused)
    }
}

/// Incremental reader for the standard sealed semantic journal.
///
/// Every complete record is consumed exactly once and the newest record is
/// retained as the current frame. There is no competing snapshot surface.
#[derive(Debug)]
pub struct Probe<S = Value> {
    path: PathBuf,
    launch: String,
    cursor: ProbeCursor,
    journal: egui_tester_witness::ObservationJournal,
    pending: VecDeque<Vec<u8>>,
    current: Option<Vec<u8>>,
    state: PhantomData<fn() -> S>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ProbeCursor {
    frame: u64,
    surface_sequence: u64,
    begun_ns: u64,
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

impl Probe<Value> {
    pub(crate) fn sealed(path: impl Into<PathBuf>, launch: impl Into<String>) -> Self {
        let path = path.into();
        let launch = launch.into();
        Self {
            journal: egui_tester_witness::ObservationJournal::sealed(&path, launch.clone()),
            path,
            launch,
            cursor: ProbeCursor::default(),
            pending: VecDeque::new(),
            current: None,
            state: PhantomData,
        }
    }

    /// Decode product state into an acceptance-owned observation.
    ///
    /// The observation may deserialize only fields consumed by its stories.
    #[must_use]
    pub fn typed<T>(self) -> Probe<T> {
        Probe {
            path: self.path,
            launch: self.launch,
            cursor: self.cursor,
            journal: self.journal,
            pending: self.pending,
            current: self.current,
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
    /// Read the newest complete journal record, never regressing.
    pub fn read(&mut self) -> Result<ProbeFrame<S>> {
        self.refill()?;
        let mut newest = None;
        while let Some(frame) = self.take_next()? {
            newest = Some(frame);
        }
        match newest {
            Some(frame) => Ok(frame),
            None => self.current()?.ok_or_else(|| Error::Probe {
                path: self.path.clone(),
                detail: "semantic journal contains no complete observation".to_owned(),
            }),
        }
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
        let mut last_mismatch = None;
        loop {
            app.ensure_running(&description)?;
            self.refill()?;
            let mut consumed = false;
            while let Some(frame) = self.take_next()? {
                consumed = true;
                match inspect(&frame) {
                    Ok(()) => return Ok(frame),
                    Err(mismatch) => last_mismatch = mismatch,
                }
            }
            if !consumed && let Some(frame) = self.current()? {
                match inspect(&frame) {
                    Ok(()) => return Ok(frame),
                    Err(mismatch) => last_mismatch = mismatch,
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

    /// Wait until a named target owns keyboard focus in a presented frame.
    pub fn wait_focus(
        &mut self,
        app: &Application<'_>,
        name: &str,
        timeout: Duration,
    ) -> Result<Anchor> {
        let frame = self.wait_checked(
            app,
            timeout,
            format!("focus on witness anchor `{name}`"),
            |frame| {
                if frame.focused_anchor(name).is_some() {
                    return Ok(());
                }
                let target = if frame.anchor(name).is_some() {
                    "target is present but unfocused"
                } else {
                    "target is absent"
                };
                let focused = frame
                    .anchors
                    .iter()
                    .filter(|anchor| anchor.focused)
                    .map(|anchor| anchor.name.as_str())
                    .collect::<Vec<_>>();
                Err(format!("{target}; focused anchors: {focused:?}"))
            },
        )?;
        frame
            .focused_anchor(name)
            .cloned()
            .ok_or_else(|| Error::Probe {
                path: self.path.clone(),
                detail: format!("focused anchor `{name}` vanished from the matching frame"),
            })
    }

    pub fn wait_fresh(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
    ) -> Result<ProbeFrame<S>> {
        let prior = self.cursor.surface_sequence;
        self.wait(
            app,
            timeout,
            format!("surface-presented observation newer than {prior}"),
            |frame| frame.surface_sequence > prior,
        )
    }

    pub fn wait_surface_presented(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
    ) -> Result<ProbeFrame<S>> {
        self.wait(
            app,
            timeout,
            "first product frame to reach surface present",
            |frame| frame.surface_sequence > 0,
        )
    }

    /// Wait until a semantic projection remains unchanged for `quiet`.
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
        loop {
            app.ensure_running(&description)?;
            self.refill()?;
            let mut newest = None;
            while let Some(frame) = self.take_next()? {
                if let Some(value) = project(&frame) {
                    let _stable = stable.observe(value, Instant::now(), quiet);
                } else {
                    stable.break_streak();
                }
                newest = Some(frame);
            }
            let frame = match newest {
                Some(frame) => Some(frame),
                None => self.current()?,
            };
            if let Some(frame) = frame {
                if let Some(value) = project(&frame) {
                    if stable.observe(value, Instant::now(), quiet) {
                        return Ok(frame);
                    }
                } else {
                    stable.break_streak();
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: description,
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(8));
        }
    }

    /// Await a post-trigger semantic cue and enforce its product timestamp.
    ///
    /// Eligibility proves temporal ordering, not causation. The witness is a
    /// synchronization surface; a user-valued story claim still needs an
    /// external rendered or durable oracle.
    pub fn wait_budgeted(
        &mut self,
        app: &Application<'_>,
        receipt: &ActionReceipt,
        budget: ReactionBudget,
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
        budget: ReactionBudget,
        description: impl Into<String>,
        mut predicate: impl FnMut(&ProbeFrame<S>) -> std::result::Result<(), String>,
    ) -> Result<Timed<ProbeFrame<S>>> {
        let description = description.into();
        let prior = self.cursor.surface_sequence;
        let endpoint = budget.endpoint();
        let timeout = budget.functional_timeout();
        let deadline = Instant::now() + timeout;
        let mut last_mismatch = None;
        loop {
            app.ensure_running(&description)?;
            self.refill()?;
            while let Some(frame) = self.take_next()? {
                if frame.surface_sequence <= prior || frame.begun_ns < receipt.triggered_ns() {
                    continue;
                }
                let timestamp = endpoint.timestamp(&frame);
                if timestamp < receipt.triggered_ns() {
                    continue;
                }
                match predicate(&frame) {
                    Ok(()) => {
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

    fn refill(&mut self) -> Result<()> {
        match self.journal.read_new_bytes() {
            Ok(records) => {
                self.pending.extend(records);
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
                path: self.path.clone(),
                detail: error.to_string(),
            }),
        }
    }

    fn take_next(&mut self) -> Result<Option<ProbeFrame<S>>> {
        let Some(bytes) = self.pending.pop_front() else {
            return Ok(None);
        };
        let frame = self.decode(&bytes)?;
        if self.current.is_some()
            && (frame.surface_sequence <= self.cursor.surface_sequence
                || frame.frame < self.cursor.frame
                || frame.begun_ns < self.cursor.begun_ns)
        {
            return self.invalid("sealed witness observation order regressed".to_owned());
        }
        self.cursor = ProbeCursor {
            frame: frame.frame,
            surface_sequence: frame.surface_sequence,
            begun_ns: frame.begun_ns,
        };
        self.current = Some(bytes);
        Ok(Some(frame))
    }

    fn current(&self) -> Result<Option<ProbeFrame<S>>> {
        self.current
            .as_deref()
            .map(|bytes| self.decode(bytes))
            .transpose()
    }

    fn decode(&self, bytes: &[u8]) -> Result<ProbeFrame<S>> {
        let frame = serde_json::from_slice(bytes).map_err(|error| Error::Probe {
            path: self.path.clone(),
            detail: error.to_string(),
        })?;
        self.validate(&frame)?;
        Ok(frame)
    }

    fn validate(&self, frame: &ProbeFrame<S>) -> Result<()> {
        if frame.schema != egui_tester_witness::SCHEMA {
            return self.invalid(format!(
                "expected schema {}, found {}",
                egui_tester_witness::SCHEMA,
                frame.schema
            ));
        }
        if frame.launch != self.launch {
            return self.invalid(format!(
                "launch nonce mismatch: expected `{}`, found `{}`",
                self.launch, frame.launch
            ));
        }
        if frame.frame == 0
            || frame.begun_ns == 0
            || frame.observed_ns == 0
            || frame.surface_presented_ns == 0
            || frame.surface_sequence == 0
        {
            return self.invalid("sealed witness omitted a required frame field".to_owned());
        }
        if frame.observed_ns < frame.begun_ns || frame.surface_presented_ns < frame.observed_ns {
            return self.invalid("sealed witness timestamps are not monotonic".to_owned());
        }
        if !frame.ppp.is_finite() || frame.ppp <= 0.0 {
            return self.invalid("pixels per point must be positive and finite".to_owned());
        }
        validate_anchors(&self.path, &frame.anchors)
    }

    fn invalid<T>(&self, detail: String) -> Result<T> {
        Err(Error::Probe {
            path: self.path.clone(),
            detail,
        })
    }
}

/// One weak, product-specific atomic JSON snapshot.
///
/// This exists only to migrate applications that predate the sealed protocol.
#[derive(Clone, Debug, Deserialize)]
pub struct LegacyProbeFrame<S = Value> {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub launch: String,
    pub frame: u64,
    #[serde(default)]
    pub ppp: Option<f32>,
    pub anchors: Vec<Anchor>,
    pub state: S,
}

impl<S> LegacyProbeFrame<S> {
    #[must_use]
    pub fn anchor(&self, name: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|anchor| anchor.name == name)
    }
}

/// Explicit reader for an unsealed product-owned atomic JSON snapshot.
#[derive(Debug)]
pub struct LegacyProbe<S = Value> {
    path: PathBuf,
    last_frame: u64,
    state: PhantomData<fn() -> S>,
}

pub type LegacyJsonProbe = LegacyProbe<Value>;

impl LegacyProbe<Value> {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_frame: 0,
            state: PhantomData,
        }
    }

    #[must_use]
    pub fn typed<T>(self) -> LegacyProbe<T> {
        LegacyProbe {
            path: self.path,
            last_frame: self.last_frame,
            state: PhantomData,
        }
    }
}

impl<S: DeserializeOwned> LegacyProbe<S> {
    pub fn read(&self) -> Result<LegacyProbeFrame<S>> {
        let bytes =
            std::fs::read(&self.path).map_err(|error| io("read witness", &self.path, error))?;
        let frame = serde_json::from_slice::<LegacyProbeFrame<S>>(&bytes).map_err(|error| {
            Error::Probe {
                path: self.path.clone(),
                detail: error.to_string(),
            }
        })?;
        if frame.ppp.is_some_and(|ppp| !ppp.is_finite() || ppp <= 0.0) {
            return Err(Error::Probe {
                path: self.path.clone(),
                detail: "pixels per point must be positive and finite".to_owned(),
            });
        }
        validate_anchors(&self.path, &frame.anchors)?;
        Ok(frame)
    }

    pub fn wait(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
        description: impl Into<String>,
        mut predicate: impl FnMut(&LegacyProbeFrame<S>) -> bool,
    ) -> Result<LegacyProbeFrame<S>> {
        let description = description.into();
        let deadline = Instant::now() + timeout;
        let mut invalid = None;
        loop {
            app.ensure_running(&description)?;
            match self.read() {
                Ok(frame) => {
                    invalid = None;
                    if predicate(&frame) {
                        self.last_frame = frame.frame;
                        return Ok(frame);
                    }
                }
                Err(Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {}
                Err(error @ Error::Probe { .. }) => invalid = Some(error),
                Err(error) => return Err(error),
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
        let frame = self.wait(app, timeout, format!("legacy anchor `{name}`"), |frame| {
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
    ) -> Result<LegacyProbeFrame<S>> {
        let prior = self.last_frame;
        self.wait(
            app,
            timeout,
            format!("legacy witness frame newer than {prior}"),
            |frame| frame.frame > prior,
        )
    }
}

fn validate_anchors(path: &Path, anchors: &[Anchor]) -> Result<()> {
    let mut names = BTreeSet::new();
    for anchor in anchors {
        anchor.validate().map_err(|error| Error::Probe {
            path: path.to_owned(),
            detail: error.to_string(),
        })?;
        if !names.insert(&anchor.name) {
            return Err(Error::Probe {
                path: path.to_owned(),
                detail: format!("duplicate anchor `{}`", anchor.name),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicate_anchors_are_not_ambiguous() {
        let probe = Probe::sealed("/unread", "launch");
        let frame = ProbeFrame {
            schema: egui_tester_witness::SCHEMA,
            launch: "launch".to_owned(),
            frame: 1,
            begun_ns: 1,
            observed_ns: 2,
            surface_presented_ns: 3,
            surface_sequence: 1,
            ppp: 1.0,
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

    #[test]
    fn sealed_observations_are_ordered_by_surface_sequence() {
        let mut probe: Probe<Value> = Probe::sealed("/unread", "launch");
        probe.pending.extend([
            wire_frame(7, 1, 10),
            wire_frame(7, 2, 20),
            wire_frame(8, 1, 30),
        ]);
        assert_eq!(
            probe
                .take_next()
                .expect("first observation")
                .expect("first frame")
                .surface_sequence,
            1
        );
        assert_eq!(
            probe
                .take_next()
                .expect("same product frame may be presented again")
                .expect("second frame")
                .surface_sequence,
            2
        );
        assert!(
            probe.take_next().is_err(),
            "surface identity regression was admitted"
        );
    }

    fn wire_frame(frame: u64, surface_sequence: u64, begun_ns: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": egui_tester_witness::SCHEMA,
            "launch": "launch",
            "frame": frame,
            "begun_ns": begun_ns,
            "observed_ns": begun_ns + 1,
            "surface_presented_ns": begun_ns + 2,
            "surface_sequence": surface_sequence,
            "ppp": 1.0,
            "anchors": [],
            "state": null
        }))
        .expect("encode witness fixture")
    }
}
