//! egui integration: final-pass target collection and the pass's surface size.

use super::{Anchor, Result};
use egui::{Context, Id, Rect, Response, Ui, plugin::Plugin};

const STORE: &str = "egui-tester-witness-anchors";

#[derive(Clone, Default)]
struct Anchors(Vec<(String, Rect, bool)>);

#[derive(Default)]
struct AnchorPass;

impl Plugin for AnchorPass {
    fn debug_name(&self) -> &'static str {
        "egui-tester witness anchors"
    }

    fn on_begin_pass(&mut self, ui: &mut Ui) {
        reset(ui.ctx());
    }
}

/// Install final-pass anchor collection into an egui context.
///
/// Installation is idempotent. Targets from passes invalidated by
/// [`Context::request_discard`] are erased before the replacement pass.
pub fn install(ctx: &Context) {
    ctx.add_plugin(AnchorPass);
}

fn reset(ctx: &Context) {
    ctx.data_mut(|data| {
        let _prior = data.remove_temp::<Anchors>(Id::new(STORE));
    });
}

/// Register a semantic target for the current egui pass. `name` must be a
/// [`TargetName`](crate::TargetName); [`take`] fails the pass otherwise.
pub fn record(ui: &Ui, name: impl Into<String>, rect: Rect) {
    record_rect(ui.ctx(), name, rect);
}

/// Register one egui response, including whether it owns keyboard focus.
pub fn record_response(ui: &Ui, name: impl Into<String>, response: &Response) {
    record_target(ui.ctx(), name, response.rect, response.has_focus());
}

/// Register a semantic target from painter-only code.
pub fn record_rect(ctx: &Context, name: impl Into<String>, rect: Rect) {
    record_target(ctx, name, rect, false);
}

fn record_target(ctx: &Context, name: impl Into<String>, rect: Rect, focused: bool) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Anchors>(Id::new(STORE))
            .0
            .push((name.into(), rect, focused));
    });
}

/// Consume the current pass's targets in physical-pixel coordinates,
/// refusing the pass if any recorded name is not a canonical
/// [`TargetName`](crate::TargetName).
pub fn take(ctx: &Context, pixels_per_point: f32) -> Result<Vec<Anchor>> {
    ctx.data_mut(|data| data.remove_temp::<Anchors>(Id::new(STORE)))
        .unwrap_or_default()
        .0
        .into_iter()
        .map(|(name, rect, focused)| {
            Anchor::logical(
                name,
                [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                pixels_per_point,
            )
            .map(|anchor| anchor.with_focus(focused))
        })
        .collect()
}

/// The physical size of the surface the current pass lays out for: egui's
/// viewport at `pixels_per_point`. Declare it with
/// [`PendingFrame::surface_size`](crate::PendingFrame::surface_size).
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a rounded viewport extent is a nonnegative pixel count"
)]
pub fn surface_size(ctx: &Context, pixels_per_point: f32) -> [u32; 2] {
    let size = ctx.viewport_rect().size() * pixels_per_point;
    [size.x, size.y].map(|extent| extent.round().max(0.0) as u32)
}
