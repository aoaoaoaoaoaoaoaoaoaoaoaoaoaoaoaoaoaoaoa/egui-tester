//! Pointer gestures that take deliberate rests. Every rest passes through a
//! caller's [`Rest`], so a story observer keeps sampling the surface
//! through input pacing; an unpaced caller simply sleeps.

use std::{
    thread,
    time::{Duration, Instant},
};

use x11rb::protocol::xproto::{BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT};

use crate::{ActionReceipt, Drag, Error, Modifiers, Motion, Result, Stroke, Wheel};

use super::{MODIFIER_GUARD, POINTER_DELIVERY_GUARD, Window, X11Controller, X11Session};

/// One deliberate rest of the given span within a gesture.
pub(crate) type Rest<'a> = &'a mut dyn FnMut(Duration) -> Result<()>;

/// The rest of a gesture no observer paces.
#[expect(
    clippy::unnecessary_wraps,
    reason = "an unpaced rest shares the fallible shape a story observer needs"
)]
fn sleep(duration: Duration) -> Result<()> {
    thread::sleep(duration);
    Ok(())
}

impl X11Controller {
    pub fn motion(
        &self,
        window: &Window,
        from: (i16, i16),
        to: (i16, i16),
        policy: Motion,
    ) -> Result<ActionReceipt> {
        self.motion_paced(window, from, to, policy, &mut sleep)
    }

    pub(crate) fn motion_paced(
        &self,
        window: &Window,
        from: (i16, i16),
        to: (i16, i16),
        policy: Motion,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        if policy.steps == 0 {
            return Err(Error::X11 {
                operation: "transport pointer",
                detail: "pointer motion requires at least one step".to_owned(),
            });
        }
        self.move_to(window, from.0, from.1)?;
        let receipt = ActionReceipt::begin(format!(
            "pointer motion ({}, {}) → ({}, {})",
            from.0, from.1, to.0, to.1
        ));
        self.transport(window, from, to, policy.steps, policy.duration, rest)?;
        Ok(receipt.trigger().finish())
    }

    pub fn scroll(
        &self,
        window: &Window,
        x: i16,
        y: i16,
        vertical_ticks: i32,
    ) -> Result<ActionReceipt> {
        let policy = Wheel {
            tick_duration: Duration::ZERO,
        };
        self.wheel(window, x, y, vertical_ticks, policy)
    }

    pub fn wheel(
        &self,
        window: &Window,
        x: i16,
        y: i16,
        vertical_ticks: i32,
        policy: Wheel,
    ) -> Result<ActionReceipt> {
        let modifiers = Modifiers::empty();
        self.wheel_paced(window, x, y, vertical_ticks, policy, modifiers, &mut sleep)
    }

    pub fn modified_wheel(
        &self,
        window: &Window,
        x: i16,
        y: i16,
        vertical_ticks: i32,
        policy: Wheel,
        modifiers: Modifiers,
    ) -> Result<ActionReceipt> {
        self.wheel_paced(window, x, y, vertical_ticks, policy, modifiers, &mut sleep)
    }

    /// Turn the wheel `vertical_ticks` notches at one point, holding
    /// `modifiers` throughout.
    pub(crate) fn wheel_paced(
        &self,
        window: &Window,
        x: i16,
        y: i16,
        vertical_ticks: i32,
        policy: Wheel,
        modifiers: Modifiers,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        self.move_to(window, x, y)?;
        let held = self.hold(modifiers)?;
        if let Err(err) = self.flush("acquire wheel modifiers") {
            self.release_keycodes(&held);
            let _flushed = self.flush("recover wheel modifiers");
            return Err(err);
        }
        let turned = (|| -> Result<ActionReceipt> {
            if !held.is_empty() {
                rest(MODIFIER_GUARD)?;
            }
            let label = format!("scroll {vertical_ticks} ticks at ({x}, {y})");
            let mut receipt = ActionReceipt::begin(if modifiers.is_empty() {
                label
            } else {
                format!("{modifiers:?} {label}")
            });
            let detail = if vertical_ticks < 0 { 4 } else { 5 };
            let count = vertical_ticks.unsigned_abs();
            for tick in 0..count {
                if tick + 1 == count {
                    receipt = receipt.trigger();
                }
                self.fake(BUTTON_PRESS_EVENT, detail, 0, 0)?;
                self.fake(BUTTON_RELEASE_EVENT, detail, 0, 0)?;
                self.flush("scroll pointer")?;
                if tick + 1 < count && !policy.tick_duration.is_zero() {
                    rest(policy.tick_duration)?;
                }
            }
            let receipt = receipt.finish();
            if count > 0 {
                rest(POINTER_DELIVERY_GUARD)?;
            }
            Ok(receipt)
        })();
        self.release_keycodes(&held);
        let released = self.flush("release wheel modifiers");
        let receipt = turned?;
        released.map(|()| receipt)
    }

    pub fn drag(
        &self,
        window: &Window,
        from: (i16, i16),
        to: (i16, i16),
        policy: Drag,
    ) -> Result<ActionReceipt> {
        self.drag_paced(window, from, to, policy, &mut sleep)
    }

    pub(crate) fn drag_paced(
        &self,
        window: &Window,
        from: (i16, i16),
        to: (i16, i16),
        policy: Drag,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        let stroke = Stroke {
            button: policy.button,
            press_duration: policy.press_duration,
            steps_per_leg: policy.steps,
            leg_duration: policy.duration,
            knot_dwell: Duration::ZERO,
        };
        self.stroke_paced(window, &[from, to], stroke, rest)
    }

    pub fn stroke(
        &self,
        window: &Window,
        knots: &[(i16, i16)],
        policy: Stroke,
    ) -> Result<ActionReceipt> {
        self.stroke_paced(window, knots, policy, &mut sleep)
    }

    pub(crate) fn stroke_paced(
        &self,
        window: &Window,
        knots: &[(i16, i16)],
        policy: Stroke,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        let &[from, .., to] = knots else {
            return Err(stroke_fault("pointer stroke requires at least two knots"));
        };
        if policy.steps_per_leg == 0 {
            return Err(stroke_fault(
                "pointer stroke requires at least one step per leg",
            ));
        }
        self.move_to(window, from.0, from.1)?;
        let receipt = ActionReceipt::begin(format!(
            "{:?} stroke {} knot(s) ({}, {}) → ({}, {})",
            policy.button,
            knots.len(),
            from.0,
            from.1,
            to.0,
            to.1
        ));
        self.fake(BUTTON_PRESS_EVENT, policy.button as u8, 0, 0)?;
        self.flush("begin pointer stroke")?;
        let carried = (|| -> Result<()> {
            if !policy.press_duration.is_zero() {
                rest(policy.press_duration)?;
            }
            for leg in knots.windows(2) {
                let [from, to] = leg else {
                    continue;
                };
                let (steps, duration) = (policy.steps_per_leg, policy.leg_duration);
                self.transport(window, *from, *to, steps, duration, &mut *rest)?;
                if !policy.knot_dwell.is_zero() {
                    rest(policy.knot_dwell)?;
                }
            }
            Ok(())
        })();
        if let Err(err) = carried {
            let _released = self.fake(BUTTON_RELEASE_EVENT, policy.button as u8, 0, 0);
            let _flushed = self.flush("abort pointer stroke");
            return Err(err);
        }
        let receipt = receipt.trigger();
        if let Err(err) = self.fake(BUTTON_RELEASE_EVENT, policy.button as u8, 0, 0) {
            let _released = self.fake(BUTTON_RELEASE_EVENT, policy.button as u8, 0, 0);
            let _flushed = self.flush("recover pointer stroke release");
            return Err(err);
        }
        self.flush("finish pointer stroke")?;
        Ok(receipt.finish())
    }

    /// Carry the pointer from `from` to `to` in `steps` equal moves, each due
    /// at its share of `duration` from the first, so slow rests never stretch
    /// the transport.
    fn transport(
        &self,
        window: &Window,
        from: (i16, i16),
        to: (i16, i16),
        steps: u16,
        duration: Duration,
        rest: Rest<'_>,
    ) -> Result<()> {
        let begun = Instant::now();
        for step in 1..=steps {
            let fraction = f64::from(step) / f64::from(steps);
            let x = f64::from(from.0)
                .mul_add(1.0 - fraction, f64::from(to.0) * fraction)
                .round() as i16;
            let y = f64::from(from.1)
                .mul_add(1.0 - fraction, f64::from(to.1) * fraction)
                .round() as i16;
            self.move_to(window, x, y)?;
            if step < steps && !duration.is_zero() {
                let due = begun + duration * u32::from(step) / u32::from(steps);
                rest(due.saturating_duration_since(Instant::now()))?;
            }
        }
        Ok(())
    }
}

impl X11Session<'_, '_> {
    pub fn motion(&self, to: (i16, i16), policy: Motion) -> Result<ActionReceipt> {
        self.motion_paced(to, policy, &mut sleep)
    }

    pub(crate) fn motion_paced(
        &self,
        to: (i16, i16),
        policy: Motion,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer transport")?;
        let from = self.controller.pointer(&self.window)?;
        let receipt = self
            .controller
            .motion_paced(&self.window, from, to, policy, rest)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn drag(&self, from: (i16, i16), to: (i16, i16), policy: Drag) -> Result<ActionReceipt> {
        self.drag_paced(from, to, policy, &mut sleep)
    }

    pub(crate) fn drag_paced(
        &self,
        from: (i16, i16),
        to: (i16, i16),
        policy: Drag,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer drag")?;
        let receipt = self
            .controller
            .drag_paced(&self.window, from, to, policy, rest)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn stroke(&self, knots: &[(i16, i16)], policy: Stroke) -> Result<ActionReceipt> {
        self.stroke_paced(knots, policy, &mut sleep)
    }

    pub(crate) fn stroke_paced(
        &self,
        knots: &[(i16, i16)],
        policy: Stroke,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer stroke")?;
        let receipt = self
            .controller
            .stroke_paced(&self.window, knots, policy, rest)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn scroll(&self, x: i16, y: i16, ticks: i32) -> Result<ActionReceipt> {
        let policy = Wheel {
            tick_duration: Duration::ZERO,
        };
        self.wheel(x, y, ticks, policy)
    }

    pub fn wheel(&self, x: i16, y: i16, ticks: i32, policy: Wheel) -> Result<ActionReceipt> {
        self.wheel_paced(x, y, ticks, policy, Modifiers::empty(), &mut sleep)
    }

    pub fn modified_wheel(
        &self,
        x: i16,
        y: i16,
        ticks: i32,
        policy: Wheel,
        modifiers: Modifiers,
    ) -> Result<ActionReceipt> {
        self.wheel_paced(x, y, ticks, policy, modifiers, &mut sleep)
    }

    pub(crate) fn wheel_paced(
        &self,
        x: i16,
        y: i16,
        ticks: i32,
        policy: Wheel,
        modifiers: Modifiers,
        rest: Rest<'_>,
    ) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer wheel gesture")?;
        let receipt =
            self.controller
                .wheel_paced(&self.window, x, y, ticks, policy, modifiers, rest)?;
        self.record(&receipt)?;
        Ok(receipt)
    }
}

fn stroke_fault(detail: &str) -> Error {
    Error::X11 {
        operation: "stroke pointer",
        detail: detail.to_owned(),
    }
}
