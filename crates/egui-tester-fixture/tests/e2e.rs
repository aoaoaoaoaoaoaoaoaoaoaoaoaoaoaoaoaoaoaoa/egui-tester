use std::{fs, time::Duration};

use eframe as _;
use egui_tester::{AppCommand, Button, JsonProbe, Quiet, Testbed};
use serde as _;
use serde_json as _;

const FIXTURE: &str = env!("CARGO_BIN_EXE_egui-tester-fixture");

#[test]
fn drives_real_input_and_observes_pixels() {
    let testbed = Testbed::raise().expect("raise hermetic X11 testbed");
    let doomed_root = testbed.root().to_owned();
    let probe_path = testbed
        .private_path("probes/fixture.json")
        .expect("resolve private probe");
    let app = testbed
        .launch(
            AppCommand::new(FIXTURE)
                .private_env("EGUI_TESTER_PROBE", "probes/fixture.json")
                .runtime(Duration::from_secs(30)),
        )
        .expect("launch fixture");
    let x11 = testbed.x11().expect("connect to private X server");
    let window = x11
        .wait_window(&app, "egui tester fixture", Duration::from_secs(15))
        .expect("find fixture window");
    x11.focus(&window).expect("focus fixture");

    let mut probe = JsonProbe::new(probe_path);
    let increment = probe
        .wait_anchor(&app, "increment", Duration::from_secs(5))
        .expect("locate increment button");
    let before = x11
        .wait_quiet(&window, Quiet::default())
        .expect("initial pixels settle");
    let (x, y) = increment.center();
    x11.click(&window, x, y, Button::Primary)
        .expect("click increment");
    let _incremented = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "visible count to become one",
            |frame| frame.state["count"] == 1,
        )
        .expect("observe incremented frame");
    let counted = x11
        .wait_quiet(&window, Quiet::default())
        .expect("incremented pixels settle");
    assert!(
        before
            .difference(&counted, 2)
            .expect("compare count pixels")
            > 0.000_01,
        "the product pixels did not reflect the witnessed count change"
    );

    let toggle = probe
        .wait_anchor(&app, "toggle", Duration::from_secs(3))
        .expect("locate color toggle");
    let (x, y) = toggle.center();
    x11.click(&window, x, y, Button::Primary)
        .expect("click color toggle");
    let _violet = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "background to become violet",
            |frame| frame.state["violet"] == true,
        )
        .expect("observe violet frame");
    let violet = x11
        .wait_quiet(&window, Quiet::default())
        .expect("violet pixels settle");
    assert!(
        counted
            .difference(&violet, 2)
            .expect("compare background pixels")
            > 0.5,
        "the rendered background did not change"
    );

    let text = probe
        .wait_anchor(&app, "text", Duration::from_secs(3))
        .expect("locate text field");
    let (x, y) = text.center();
    x11.click(&window, x, y, Button::Primary)
        .expect("focus text field");
    let _focused = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "text field to acquire focus",
            |frame| frame.state["text_focused"] == true,
        )
        .expect("observe text focus");
    x11.type_text("blade").expect("inject keyboard text");
    let _typed = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "text field to contain injected text",
            |frame| frame.state["text"] == "blade",
        )
        .expect("observe keyboard input");
    app.terminate().expect("terminate fixture cgroup");
    drop(app);
    drop(testbed);
    assert!(
        !doomed_root.exists(),
        "testbed left its private filesystem behind"
    );
}

#[test]
fn read_only_borrow_denies_a_real_write() {
    let live = tempfile::tempdir().expect("create simulated live-data directory");
    let sentinel = live.path().join("index");
    fs::write(&sentinel, b"untouched").expect("write sentinel");
    let testbed = Testbed::raise().expect("raise hermetic testbed");
    let doomed_root = testbed.root().to_owned();
    let app = testbed
        .launch(
            AppCommand::new(FIXTURE)
                .args(["--try-write".into(), sentinel.as_os_str().to_owned()])
                .borrow_read_only(live.path())
                .runtime(Duration::from_secs(10)),
        )
        .expect("launch write-denial fixture");
    let exit = app
        .wait(Duration::from_secs(5))
        .expect("wait for denied writer");
    assert_eq!(exit.code, 73, "unexpected sandbox exit: {exit:#?}");
    assert!(
        exit.stderr.contains("would write real file from test"),
        "missing explicit hermeticity diagnostic: {}",
        exit.stderr
    );
    assert_eq!(
        fs::read(&sentinel).expect("read sentinel after attack"),
        b"untouched"
    );
    app.terminate().expect("collect denied writer cgroup");
    drop(app);
    drop(testbed);
    assert!(
        !doomed_root.exists(),
        "testbed left its private filesystem behind"
    );
}

#[test]
fn undeclared_host_data_is_invisible() {
    let live = tempfile::tempdir().expect("create simulated live-data directory");
    let sentinel = live.path().join("secret");
    fs::write(&sentinel, b"unseen").expect("write secret sentinel");
    let testbed = Testbed::raise().expect("raise hermetic testbed");
    let app = testbed
        .launch(
            AppCommand::new(FIXTURE)
                .args(["--try-read".into(), sentinel.as_os_str().to_owned()])
                .runtime(Duration::from_secs(10)),
        )
        .expect("launch read-isolation fixture");
    let exit = app
        .wait(Duration::from_secs(5))
        .expect("wait for denied reader");
    assert_eq!(exit.code, 73, "unexpected sandbox exit: {exit:#?}");
    assert!(
        exit.stderr
            .contains("undeclared real file is invisible to test"),
        "missing isolation diagnostic: {}",
        exit.stderr
    );
    app.terminate().expect("collect denied reader cgroup");
}
