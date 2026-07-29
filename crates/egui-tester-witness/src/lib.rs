//! One-way, versioned semantic telemetry for black-box GUI tests.
//!
//! Applications publish immutable observations after presenting a frame. The
//! harness may use them to locate controls and synchronize, never to mutate
//! product state or substitute for an external oracle.

use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

use rustix::time::{ClockId, clock_gettime};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;
pub const PATH_ENV: &str = "EGUI_TESTER_WITNESS";
pub const LAUNCH_ENV: &str = "EGUI_TESTER_LAUNCH";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("witness environment is incomplete: {0}")]
    Environment(&'static str),
    #[error("invalid witness anchor `{name}`: {detail}")]
    Anchor { name: String, detail: &'static str },
    #[error("serialize witness frame: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("{operation} witness `{path}`: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Named hit-test rectangle in physical, window-relative pixels.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Anchor {
    pub name: String,
    pub rect: [f32; 4],
}

impl Anchor {
    pub fn physical(name: impl Into<String>, rect: [f32; 4]) -> Result<Self> {
        let anchor = Self {
            name: name.into(),
            rect,
        };
        anchor.validate()?;
        Ok(anchor)
    }

    pub fn logical(name: impl Into<String>, rect: [f32; 4], pixels_per_point: f32) -> Result<Self> {
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return Err(Error::Anchor {
                name: name.into(),
                detail: "pixels per point must be positive and finite",
            });
        }
        Self::physical(name, rect.map(|coordinate| coordinate * pixels_per_point))
    }

    pub fn validate(&self) -> Result<()> {
        let [x0, y0, x1, y1] = self.rect;
        if self.name.is_empty() {
            return Err(Error::Anchor {
                name: self.name.clone(),
                detail: "name is empty",
            });
        }
        if !self.rect.into_iter().all(f32::is_finite) {
            return Err(Error::Anchor {
                name: self.name.clone(),
                detail: "rectangle is not finite",
            });
        }
        if x0 > x1 || y0 > y1 {
            return Err(Error::Anchor {
                name: self.name.clone(),
                detail: "rectangle is inverted",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn center(&self) -> (i16, i16) {
        let [x0, y0, x1, y1] = self.rect;
        (
            f32::midpoint(x0, x1).round() as i16,
            f32::midpoint(y0, y1).round() as i16,
        )
    }
}

/// Product observation captured before witness serialization.
pub struct PendingFrame<T> {
    frame: u64,
    observed_ns: u64,
    pixels_per_point: f32,
    anchors: Vec<Anchor>,
    state: T,
}

impl<T> PendingFrame<T> {
    pub fn forge(
        frame: u64,
        pixels_per_point: f32,
        anchors: impl IntoIterator<Item = Anchor>,
        state: T,
    ) -> Result<Self> {
        Self::forge_at(
            ProductInstant::now(),
            frame,
            pixels_per_point,
            anchors,
            state,
        )
    }

    /// Forge telemetry against a product timestamp captured before witness work.
    pub fn forge_at(
        observed: ProductInstant,
        frame: u64,
        pixels_per_point: f32,
        anchors: impl IntoIterator<Item = Anchor>,
        state: T,
    ) -> Result<Self> {
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return Err(Error::Anchor {
                name: "<frame>".to_owned(),
                detail: "pixels per point must be positive and finite",
            });
        }
        let anchors = anchors.into_iter().collect::<Vec<_>>();
        let mut names = BTreeSet::new();
        for anchor in &anchors {
            anchor.validate()?;
            if !names.insert(&anchor.name) {
                return Err(Error::Anchor {
                    name: anchor.name.clone(),
                    detail: "name is duplicated in one frame",
                });
            }
        }
        Ok(Self {
            frame,
            observed_ns: observed.0,
            pixels_per_point,
            anchors,
            state,
        })
    }

    #[must_use]
    pub const fn observed_ns(&self) -> u64 {
        self.observed_ns
    }
}

/// Atomic witness sink armed by the standard harness environment.
#[derive(Debug)]
pub struct Publisher {
    path: PathBuf,
    launch: String,
    presentation: u64,
}

impl Publisher {
    pub fn from_env() -> Result<Option<Self>> {
        match (env::var_os(PATH_ENV), env::var_os(LAUNCH_ENV)) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(Error::Environment(
                "EGUI_TESTER_WITNESS is set without EGUI_TESTER_LAUNCH",
            )),
            (None, Some(_)) => Err(Error::Environment(
                "EGUI_TESTER_LAUNCH is set without EGUI_TESTER_WITNESS",
            )),
            (Some(path), Some(launch)) => {
                let launch = launch
                    .into_string()
                    .map_err(|_| Error::Environment("EGUI_TESTER_LAUNCH is not valid Unicode"))?;
                if launch.is_empty() {
                    return Err(Error::Environment("EGUI_TESTER_LAUNCH is empty"));
                }
                Ok(Some(Self {
                    path: PathBuf::from(path),
                    launch,
                    presentation: 0,
                }))
            }
        }
    }

    /// Commit one observation only after its product frame has been presented.
    pub fn present<T: Serialize>(&mut self, pending: PendingFrame<T>) -> Result<u64> {
        self.present_at(pending, ProductInstant::now())
    }

    /// Commit telemetry against a timestamp captured immediately after presentation.
    pub fn present_at<T: Serialize>(
        &mut self,
        pending: PendingFrame<T>,
        presented: ProductInstant,
    ) -> Result<u64> {
        self.presentation = self.presentation.saturating_add(1);
        let frame = WireFrame {
            schema: SCHEMA,
            launch: &self.launch,
            frame: pending.frame,
            observed_ns: pending.observed_ns,
            presented_ns: presented.0,
            presentation: self.presentation,
            ppp: pending.pixels_per_point,
            anchors: &pending.anchors,
            state: &pending.state,
        };
        let bytes = serde_json::to_vec(&frame)?;
        write_atomic(&self.path, &bytes)?;
        Ok(self.presentation)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn launch(&self) -> &str {
        &self.launch
    }
}

#[derive(Serialize)]
struct WireFrame<'a, T> {
    schema: u32,
    launch: &'a str,
    frame: u64,
    observed_ns: u64,
    presented_ns: u64,
    presentation: u64,
    ppp: f32,
    anchors: &'a [Anchor],
    state: &'a T,
}

/// An unforgeable timestamp in the harness's shared monotonic epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductInstant(u64);

impl ProductInstant {
    #[must_use]
    pub fn now() -> Self {
        Self(monotonic_ns())
    }
}

/// Shared `CLOCK_MONOTONIC` epoch used for input-to-observation latency.
#[must_use]
pub fn monotonic_ns() -> u64 {
    let now = clock_gettime(ClockId::Monotonic);
    u64::try_from(now.tv_sec)
        .unwrap_or_default()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(now.tv_nsec).unwrap_or_default())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            operation: "create parent for",
            path: parent.to_owned(),
            source,
        })?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|source| Error::Io {
        operation: "write",
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, path).map_err(|source| Error::Io {
        operation: "replace",
        path: path.to_owned(),
        source,
    })
}

#[cfg(feature = "egui")]
pub mod egui {
    use super::{Anchor, Result};
    use egui::{Context, Id, Rect, Ui};

    const STORE: &str = "egui-tester-witness-anchors";

    #[derive(Clone, Default)]
    struct Anchors(Vec<(String, Rect)>);

    /// Begin one egui pass by discarding the preceding pass's targets.
    pub fn reset(ctx: &Context) {
        ctx.data_mut(|data| {
            let _prior = data.remove_temp::<Anchors>(Id::new(STORE));
        });
    }

    /// Register a semantic target for the current egui pass.
    pub fn record(ui: &Ui, name: impl Into<String>, rect: Rect) {
        record_rect(ui.ctx(), name, rect);
    }

    /// Register a semantic target from painter-only code.
    pub fn record_rect(ctx: &Context, name: impl Into<String>, rect: Rect) {
        ctx.data_mut(|data| {
            data.get_temp_mut_or_default::<Anchors>(Id::new(STORE))
                .0
                .push((name.into(), rect));
        });
    }

    /// Consume the current pass's targets in physical-pixel coordinates.
    pub fn take(ctx: &Context, pixels_per_point: f32) -> Result<Vec<Anchor>> {
        ctx.data_mut(|data| data.remove_temp::<Anchors>(Id::new(STORE)))
            .unwrap_or_default()
            .0
            .into_iter()
            .map(|(name, rect)| {
                Anchor::logical(
                    name,
                    [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                    pixels_per_point,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_rectangles_become_physical() {
        let anchor =
            Anchor::logical("blade", [1.0, 2.0, 3.0, 4.0], 1.5).expect("forge physical anchor");
        assert_eq!(anchor.rect, [1.5, 3.0, 4.5, 6.0]);
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let anchors = [
            Anchor::physical("blade", [0.0, 0.0, 1.0, 1.0]).expect("first anchor"),
            Anchor::physical("blade", [1.0, 1.0, 2.0, 2.0]).expect("second anchor"),
        ];
        assert!(PendingFrame::forge(1, 1.0, anchors, ()).is_err());
    }
}
