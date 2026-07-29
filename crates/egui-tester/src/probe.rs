use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::Value;

use crate::{Application, Error, Result, error::io};

/// Named hit-test rectangle in physical, window-relative pixels.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Anchor {
    pub name: String,
    pub rect: [f32; 4],
}

impl Anchor {
    #[must_use]
    pub fn center(&self) -> (i16, i16) {
        let [x0, y0, x1, y1] = self.rect;
        (
            f32::midpoint(x0, x1).round() as i16,
            f32::midpoint(y0, y1).round() as i16,
        )
    }
}

/// One atomic witness snapshot.
#[derive(Clone, Debug, Deserialize)]
pub struct ProbeFrame {
    pub frame: u64,
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

/// Adapter for an atomic JSON witness file.
///
/// This is a synchronization and target-location plane. Tests should make
/// verdicts from pixels or externally visible product effects.
#[derive(Debug)]
pub struct JsonProbe {
    path: PathBuf,
    last_frame: u64,
}

impl JsonProbe {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_frame: 0,
        }
    }

    pub fn read(&self) -> Result<ProbeFrame> {
        let bytes = std::fs::read(&self.path).map_err(|err| io("read probe", &self.path, err))?;
        serde_json::from_slice(&bytes).map_err(|err| Error::Probe {
            path: self.path.clone(),
            detail: err.to_string(),
        })
    }

    pub fn wait(
        &mut self,
        app: &Application<'_>,
        timeout: Duration,
        description: impl Into<String>,
        predicate: impl Fn(&ProbeFrame) -> bool,
    ) -> Result<ProbeFrame> {
        let description = description.into();
        let deadline = Instant::now() + timeout;
        loop {
            app.ensure_running(&description)?;
            match self.read() {
                Ok(frame) if predicate(&frame) => {
                    self.last_frame = frame.frame;
                    return Ok(frame);
                }
                Ok(_) | Err(Error::Io { .. } | Error::Probe { .. }) => {}
                Err(err) => return Err(err),
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: description,
                    timeout,
                });
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
        let frame = self.wait(app, timeout, format!("probe anchor `{name}`"), |frame| {
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
            format!("probe frame newer than {prior}"),
            |frame| frame.frame > prior,
        )
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
