use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use egui_tester::{
    AppCommand, Backend, Button, Error, LegacyJsonProbe, Result, Testbed, TestbedBuilder, X11Config,
};

const APP: &str = "adequate_booru_viewer";
const TITLE: &str = "adequate booru viewer";

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let binary = required(&mut args, "ABV binary")?;
    let booru_root = required(&mut args, "booru checkout")?;
    let artifacts = args.next().map(PathBuf::from);
    let mut builder = TestbedBuilder::default().backend(Backend::X11(X11Config::default()));
    if let Some(path) = &artifacts {
        builder = builder.failure_artifacts(path);
    }
    let testbed = builder.raise()?;
    seed_demo(&testbed, Path::new(&booru_root))?;
    let probe_path = testbed.private_path("probes/booru.json")?;
    let command = AppCommand::new(binary)
        .private_env("ABV_ANCHOR_PROBE", "probes/booru.json")
        .private_env("ADEQUATE_BOORU_VIEWER_STARTUP_PROBE", "probes/booru-ready")
        .runtime(Duration::from_mins(1));

    let app = testbed.launch(command.clone())?;
    let x11 = testbed.x11()?;
    let window = x11.wait_window(&app, TITLE, Duration::from_secs(20))?;
    x11.focus(&window)?;
    let mut probe = LegacyJsonProbe::new(&probe_path);
    let recess = probe.wait_anchor(&app, "recess:ui", Duration::from_secs(10))?;
    let closed = x11.capture(&window)?;
    let (x, y) = recess.center();
    let _receipt = x11.click(&window, x, y, Button::Primary)?;
    let dry = probe.wait_anchor(&app, "water:dry", Duration::from_secs(5))?;
    let open = x11.wait_changed(&app, &window, &closed, 0.001, 2, Duration::from_secs(5))?;
    demand(
        closed.difference(&open, 2)? > 0.001,
        "opening the UI recess did not alter rendered pixels",
    )?;

    let wet = probe.wait_anchor(&app, "water:wet", Duration::from_secs(3))?;
    let (x, y) = wet.center();
    let _receipt = x11.click(&window, x, y, Button::Primary)?;
    let _wet_frame = probe.wait(
        &app,
        Duration::from_secs(3),
        "booru water mode to become wet",
        |frame| state_is(frame, "water", "wet"),
    )?;
    let animated = x11.wait_changed(&app, &window, &open, 0.001, 2, Duration::from_secs(5))?;
    demand(
        open.difference(&animated, 2)? > 0.001,
        "wet mode changed the witness but not the product pixels",
    )?;
    if let Some(path) = &artifacts {
        closed.save_png(path.join("booru-closed.png"))?;
        open.save_png(path.join("booru-open.png"))?;
        animated.save_png(path.join("booru-wet.png"))?;
    }

    app.wait_until(
        Duration::from_secs(5),
        "wet mode to reach the persisted slate",
        || {
            Ok(testbed
                .read_private_to_string(format!("xdg/state/{APP}/slate.toml"))
                .is_ok_and(|text| text.contains("water = \"wet\"")))
        },
    )?;
    app.terminate()?;
    drop(app);
    let _removed = fs::remove_file(&probe_path);

    let restarted = testbed.launch(command)?;
    let window = x11.wait_window(&restarted, TITLE, Duration::from_secs(20))?;
    x11.focus(&window)?;
    let mut probe = LegacyJsonProbe::new(&probe_path);
    let restored = probe.wait(
        &restarted,
        Duration::from_secs(10),
        "restarted booru to restore wet mode",
        |frame| state_is(frame, "water", "wet"),
    )?;
    let dry = restored.anchor(&dry.name).ok_or_else(|| Error::Probe {
        path: probe_path.clone(),
        detail: "persisted open UI lost its dry-mode control".to_owned(),
    })?;
    let (x, y) = dry.center();
    let _receipt = x11.click(&window, x, y, Button::Primary)?;
    let _dry_frame = probe.wait(
        &restarted,
        Duration::from_secs(3),
        "booru water mode to return to dry",
        |frame| state_is(frame, "water", "dry"),
    )?;
    restarted.terminate()?;
    println!("booru smoke passed under {}", testbed.id());
    Ok(())
}

fn seed_demo(testbed: &Testbed, booru_root: &Path) -> Result<()> {
    let demo = booru_root.join("demo/wet");
    let _config = testbed.copy_private(
        format!("xdg/config/{APP}/config.toml"),
        demo.join("config.toml"),
    )?;
    let _slate = testbed.copy_private(
        format!("xdg/state/{APP}/slate.toml"),
        demo.join("slate.toml"),
    )?;
    Ok(())
}

fn state_is(frame: &egui_tester::LegacyProbeFrame, key: &str, expected: &str) -> bool {
    frame.state[key]
        .as_str()
        .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
}

fn demand(condition: bool, detail: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::X11 {
            operation: "adjudicate booru pixels",
            detail: detail.to_owned(),
        })
    }
}

fn required(args: &mut impl Iterator<Item = OsString>, name: &'static str) -> Result<OsString> {
    args.next().ok_or_else(|| Error::Containment {
        layer: "booru acceptance CLI",
        detail: format!("missing {name}; usage: booru-acceptance <abv> <booru-root> [artifacts]"),
    })
}
