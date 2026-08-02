use std::{fs, time::Duration};

use eframe as _;
use egui_tester::{
    AppCommand, Button, Drag, Key, LegacyJsonProbe, Modifiers, Testbed, WindowQuery,
};
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
    let session = testbed
        .x11_session(
            &app,
            WindowQuery::title_exact("egui tester fixture"),
            Duration::from_secs(15),
        )
        .expect("find fixture window");
    session.focus().expect("focus fixture");

    let mut probe = LegacyJsonProbe::new(probe_path);
    let increment = probe
        .wait_anchor(&app, "increment", Duration::from_secs(5))
        .expect("locate increment button");
    let before = session.capture().expect("capture initial pixels");
    let (x, y) = increment.center();
    let _receipt = session
        .click(x, y, Button::Primary)
        .expect("click increment");
    let _incremented = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "visible count to become one",
            |frame| frame.state["count"] == 1,
        )
        .expect("observe incremented frame");
    let counted = session
        .wait_changed(&before, 0.000_01, 2, Duration::from_secs(3))
        .expect("count reaches product pixels");
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
    let _receipt = session
        .click(x, y, Button::Primary)
        .expect("click color toggle");
    let _violet = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "background to become violet",
            |frame| frame.state["violet"] == true,
        )
        .expect("observe violet frame");
    let violet = session
        .wait_changed(&counted, 0.5, 2, Duration::from_secs(3))
        .expect("violet background reaches product pixels");
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
    let _receipt = session
        .click(x, y, Button::Primary)
        .expect("focus text field");
    let _focused = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "text field to acquire focus",
            |frame| frame.state["text_focused"] == true,
        )
        .expect("observe text focus");
    let _typed_text = session.type_text("blade").expect("inject keyboard text");
    let _typed = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "text field to contain injected text",
            |frame| frame.state["text"] == "blade",
        )
        .expect("observe keyboard input");
    let _select = session
        .chord(Modifiers::CTRL, Key::Character('a'))
        .expect("select all text");
    let _replacement_text = session.type_text("steel").expect("replace keyboard text");
    let _replaced = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "text field replacement",
            |frame| frame.state["text"] == "steel",
        )
        .expect("observe modified keyboard input");

    let _f2 = session.key(Key::Function(2)).expect("inject F2");
    let _function_key = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "F2 count to become one",
            |frame| frame.state["f2_count"] == 1,
        )
        .expect("observe function key");

    let drag = probe
        .wait_anchor(&app, "drag", Duration::from_secs(3))
        .expect("locate draggable slider");
    let [x0, y0, x1, y1] = drag.rect;
    let _dragged = session
        .drag(
            (
                (x0 + 8.0).round() as i16,
                f32::midpoint(y0, y1).round() as i16,
            ),
            (
                (x1 - 8.0).round() as i16,
                f32::midpoint(y0, y1).round() as i16,
            ),
            Drag {
                duration: Duration::from_millis(40),
                ..Drag::default()
            },
        )
        .expect("drag real slider");
    let _drag_value = probe
        .wait(
            &app,
            Duration::from_secs(3),
            "dragged slider to reach its upper range",
            |frame| {
                frame.state["drag_value"]
                    .as_f64()
                    .is_some_and(|value| value > 80.0)
            },
        )
        .expect("observe pointer drag");
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

#[test]
fn result_failures_retain_registered_artifacts() {
    let sink = tempfile::tempdir().expect("create artifact sink");
    let verdict = Testbed::builder()
        .failure_artifacts(sink.path())
        .run(|testbed| {
            let _oracle = testbed.write_private("project/oracle.txt", b"retained")?;
            testbed.retain_on_failure("project/oracle.txt")?;
            Err::<(), _>(egui_tester::Error::Containment {
                layer: "fixture verdict",
                detail: "deliberate failure".to_owned(),
            })
        });
    assert!(verdict.is_err(), "deliberate scenario unexpectedly passed");
    let sessions = fs::read_dir(sink.path())
        .expect("read artifact sink")
        .collect::<Result<Vec<_>, _>>()
        .expect("read artifact entries");
    let [session] = sessions.as_slice() else {
        panic!("expected one retained session, found {}", sessions.len());
    };
    assert_eq!(
        fs::read(session.path().join("project/oracle.txt")).expect("read retained oracle"),
        b"retained"
    );
}

#[test]
fn private_oracles_refuse_symlink_escape() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().expect("create outside tree");
    let secret = outside.path().join("secret");
    fs::write(&secret, b"sealed").expect("seed outside file");
    let testbed = Testbed::raise().expect("raise hermetic testbed");
    let project = testbed
        .create_private_dir("project")
        .expect("create private project");
    symlink(&secret, project.join("escape")).expect("forge hostile file symlink");
    symlink(outside.path(), project.join("outside")).expect("forge hostile directory symlink");

    assert!(
        testbed.read_private("project/escape").is_err(),
        "private oracle followed an app-controlled file symlink"
    );
    assert!(
        testbed
            .write_private("project/outside/pwned", b"breach")
            .is_err(),
        "private writer followed an app-controlled directory symlink"
    );
    assert_eq!(
        fs::read(&secret).expect("read outside sentinel"),
        b"sealed",
        "confined oracle mutated outside state"
    );
    assert!(
        !outside.path().join("pwned").exists(),
        "confined writer escaped through a private symlink"
    );
}
