use std::time::Duration;

use eframe as _;
use egui_tester::{AppCommand, Backend, LegacyJsonProbe, TestbedBuilder, WaylandConfig};
use serde as _;
use serde_json as _;
use tempfile as _;

const FIXTURE: &str = env!("CARGO_BIN_EXE_egui-tester-fixture");

#[test]
#[ignore = "requires the optional weston and weston-screenshooter system tools"]
fn launches_and_captures_on_headless_wayland() {
    let testbed = TestbedBuilder::default()
        .backend(Backend::Wayland(WaylandConfig {
            width: 800,
            height: 600,
        }))
        .raise()
        .expect("raise private Weston compositor");
    let probe_path = testbed
        .private_path("probes/fixture.json")
        .expect("resolve probe path");
    let app = testbed
        .launch(
            AppCommand::new(FIXTURE)
                .private_env("EGUI_TESTER_PROBE", "probes/fixture.json")
                .runtime(Duration::from_secs(30)),
        )
        .expect("launch Wayland fixture");
    let mut probe = LegacyJsonProbe::new(probe_path);
    let _frame = probe
        .wait_anchor(&app, "increment", Duration::from_secs(15))
        .expect("observe a rendered Wayland UI frame");
    let pixels = testbed
        .capture_wayland()
        .expect("capture Weston's virtual output");
    assert_eq!((pixels.width(), pixels.height()), (800, 600));
    assert!(
        pixels
            .rgba()
            .chunks_exact(4)
            .any(|pixel| pixel[..3] != [0, 0, 0]),
        "Wayland output capture is entirely black"
    );
    app.terminate().expect("terminate Wayland fixture");
}
