//! Hermetic, black-box end-to-end control for native GUI applications.
//!
//! The harness injects real display-server input and judges rendered pixels or
//! external product effects. Optional witnesses may locate controls and
//! synchronize frames, but they are deliberately incapable of mutating the
//! application.

mod condition;
mod error;
mod frames;
mod pixels;
mod probe;
mod service;
mod story;
mod testbed;
mod timing;
mod x11;

pub use condition::{Condition, Field, field};
pub use error::{Error, Result};
pub use frames::{CadenceBudget, CadenceReport, FrameProbe, FrameSample, FrameTrace};
pub use pixels::{Frame, PixelRegion, Quiet};
pub use probe::{Anchor, LegacyJsonProbe, LegacyProbe, LegacyProbeFrame, Probe, ProbeFrame};
pub use service::{AppCommand, Application, Graphics, Network};
pub use story::{Reaction, Story, demand};
pub use testbed::{Backend, Testbed, TestbedBuilder, WaylandConfig, X11Config};
pub use timing::{ActionReceipt, ReactionBudget, ReactionEndpoint, Timed};
pub use x11::{
    Button, Drag, Key, Modifiers, Stroke, Wheel, Window, WindowQuery, X11Controller, X11Session,
};
