//! Hermetic, black-box end-to-end control for native GUI applications.
//!
//! The harness injects real display-server input and judges rendered pixels or
//! external product effects. Optional witnesses may locate controls and
//! synchronize frames, but they are deliberately incapable of mutating the
//! application.

mod error;
mod frames;
mod pixels;
mod probe;
mod service;
mod testbed;
mod timing;
mod x11;

pub use error::{Error, Result};
pub use frames::{CadenceBudget, CadenceReport, FrameProbe, FrameSample, FrameTrace};
pub use pixels::{Frame, Quiet};
pub use probe::{Anchor, JsonProbe, ProbeFrame};
pub use service::{AppCommand, Application, Graphics, Network};
pub use testbed::{Backend, Testbed, TestbedBuilder, WaylandConfig, X11Config};
pub use timing::{ActionReceipt, PerformanceBudget, PerformanceEndpoint, Timed};
pub use x11::{
    Button, Drag, Key, Modifiers, Stroke, Wheel, Window, WindowQuery, X11Controller, X11Session,
};
