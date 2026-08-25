use std::{
    fs::File,
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::Duration,
};

use serde::Serialize;

use crate::{
    ActionReceipt, Choreography, Error, Frame, Result, Silent, StoryCue, StoryEvent, StoryFact,
    StoryObserver, StorySurface, StoryTempo, story::CaptureSurface,
};

const DRIVER: &str = "moe.swarm.eguitester.driver/moe.swarm.eguitester.driver.Driver";

/// One ADB-addressable Android device and egui-tester instrumentation endpoint.
#[derive(Clone, Debug)]
pub struct AndroidDevice {
    adb: PathBuf,
    serial: String,
    driver: String,
}

impl AndroidDevice {
    #[must_use]
    pub fn new(serial: impl Into<String>) -> Self {
        Self {
            adb: PathBuf::from("adb"),
            serial: serial.into(),
            driver: DRIVER.to_owned(),
        }
    }

    #[must_use]
    pub fn adb(mut self, executable: impl Into<PathBuf>) -> Self {
        self.adb = executable.into();
        self
    }

    #[must_use]
    pub fn driver(mut self, component: impl Into<String>) -> Self {
        self.driver = component.into();
        self
    }

    #[must_use]
    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn prove_ready(&self) -> Result<()> {
        let output = self.checked(["get-state"])?;
        if String::from_utf8_lossy(&output.stdout).trim() == "device" {
            Ok(())
        } else {
            Err(Error::Verdict {
                detail: format!("Android device `{}` is not ready", self.serial),
            })
        }
    }

    pub fn install_driver(&self, apk: impl AsRef<Path>) -> Result<()> {
        let apk = apk.as_ref();
        let argument = apk.as_os_str().to_owned();
        let _installed = self.checked_os(["install".into(), "-r".into(), "-t".into(), argument])?;
        Ok(())
    }

    pub fn force_stop(&self, package: &str) -> Result<()> {
        let _stopped = self.checked(["shell", "am", "force-stop", package])?;
        Ok(())
    }

    /// Remove one application's private data and cache through Android's
    /// package manager.
    ///
    /// Callers must expose this destructive boundary explicitly in their own
    /// CLI or scenario contract; choreography never clears product state as an
    /// implicit setup step.
    pub fn clear_app_data(&self, package: &str) -> Result<()> {
        let output = self.checked(["shell", "pm", "clear", package])?;
        let report = String::from_utf8_lossy(&output.stdout);
        if report.trim() == "Success" {
            Ok(())
        } else {
            Err(Error::Verdict {
                detail: format!("Android refused to clear `{package}`: {report}"),
            })
        }
    }

    pub fn start_native_activity(&self, package: &str) -> Result<()> {
        let component = format!("{package}/android.app.NativeActivity");
        let _started = self.checked(["shell", "am", "start", "-W", "-n", &component])?;
        Ok(())
    }

    /// Physical display size reported by Android's window manager.
    pub fn screen_size(&self) -> Result<[u32; 2]> {
        let output = self.checked(["shell", "wm", "size"])?;
        let report = String::from_utf8_lossy(&output.stdout);
        let dimensions = report
            .lines()
            .find_map(|line| line.trim().strip_prefix("Physical size: "))
            .ok_or_else(|| Error::Verdict {
                detail: format!("Android window manager reported no physical size:\n{report}"),
            })?;
        let (width, height) = dimensions.split_once('x').ok_or_else(|| Error::Verdict {
            detail: format!("Android physical size is malformed: `{dimensions}`"),
        })?;
        let parse = |axis: &str| {
            axis.parse::<u32>().map_err(|source| Error::Verdict {
                detail: format!("Android physical size is malformed: `{dimensions}`: {source}"),
            })
        };
        Ok([parse(width)?, parse(height)?])
    }

    /// Demand that Android still presents the expected product Activity.
    pub fn require_resumed(&self, package: &str) -> Result<()> {
        let output = self.checked(["shell", "dumpsys", "activity", "activities"])?;
        let report = String::from_utf8_lossy(&output.stdout);
        let resumed = report
            .lines()
            .find(|line| line.trim_start().starts_with("mResumedActivity:"));
        if resumed.is_some_and(|line| line.contains(&format!(" {package}/"))) {
            Ok(())
        } else {
            Err(Error::Verdict {
                detail: format!(
                    "Android choreography left `{package}`; resumed Activity: {}",
                    resumed.map(str::trim).unwrap_or("absent")
                ),
            })
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.adb);
        let _command = command.args(["-s", &self.serial]);
        command
    }

    fn checked<const N: usize>(&self, arguments: [&str; N]) -> Result<Output> {
        self.checked_os(arguments)
    }

    fn checked_os(
        &self,
        arguments: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    ) -> Result<Output> {
        let mut command = self.command();
        let _command = command.args(arguments);
        let printable = format!("{command:?}");
        let output = command.output().map_err(|source| Error::Io {
            operation: "run ADB command",
            path: self.adb.clone(),
            source,
        })?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(Error::Command {
                command: printable,
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }

    fn instrument<const N: usize>(
        &self,
        gesture: &str,
        action: &str,
        parameters: [(&str, String); N],
    ) -> Result<()> {
        let trace_action = action
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || "._-".contains(character) {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let mut arguments: Vec<std::ffi::OsString> = [
            "shell",
            "am",
            "instrument",
            "-w",
            "-r",
            "-e",
            "gesture",
            gesture,
            "-e",
            "action",
            &trace_action,
        ]
        .map(Into::into)
        .to_vec();
        for (name, value) in parameters {
            arguments.extend(["-e".into(), name.into(), value.into()]);
        }
        arguments.push(self.driver.clone().into());
        let output = self.checked_os(arguments)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("INSTRUMENTATION_CODE: -1") {
            Ok(())
        } else {
            Err(Error::Verdict {
                detail: format!("Android instrumentation refused `{action}`:\n{stdout}"),
            })
        }
    }
}

/// Two-finger, centroid-preserving spread or contraction in physical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AndroidPinch {
    pub first: [i32; 2],
    pub second: [i32; 2],
    /// Each finger moves this far away from the initial centroid. Negative
    /// components contract toward it.
    pub spread: [i32; 2],
    pub steps: u16,
    pub duration: Duration,
}

/// Effectful Android projection of the shared causal story stream.
///
/// This is deliberately coordinate-bearing rather than a persisted replay
/// format. Product scenarios remain responsible for deriving coordinates from
/// the current surface or a platform-specific layout contract.
pub struct AndroidChoreography<'device, O = Silent> {
    session: AndroidSession<'device>,
    observer: O,
}

impl<'device> AndroidChoreography<'device, Silent> {
    pub fn bind(device: &'device AndroidDevice) -> Result<Self> {
        device.prove_ready()?;
        Ok(Self {
            session: AndroidSession { device },
            observer: Silent,
        })
    }

    pub fn with_observer<O: StoryObserver>(self, observer: O) -> AndroidChoreography<'device, O> {
        AndroidChoreography {
            session: self.session,
            observer,
        }
    }
}

impl<O: StoryObserver> AndroidChoreography<'_, O> {
    pub fn chapter(&mut self, title: &str) -> Result<()> {
        self.emit(StoryEvent::Cue(StoryCue::Chapter { title }))
    }

    pub fn hold(&mut self, duration: Duration) -> Result<()> {
        self.emit(StoryEvent::Cue(StoryCue::Hold { duration }))
    }

    pub fn tempo(&mut self, tempo: StoryTempo) -> Result<()> {
        self.emit(StoryEvent::Cue(StoryCue::Tempo { tempo }))
    }

    pub fn capture(&self) -> Result<Frame> {
        self.session.capture()
    }

    pub fn tap(&mut self, action: &str, point: [i32; 2]) -> Result<ActionReceipt> {
        self.aim(action, point)?;
        let receipt = ActionReceipt::begin(action).trigger();
        self.session.device.instrument(
            "tap",
            action,
            [("x", point[0].to_string()), ("y", point[1].to_string())],
        )?;
        self.complete(receipt.finish(), Some(point))
    }

    pub fn swipe(
        &mut self,
        action: &str,
        origin: [i32; 2],
        destination: [i32; 2],
        duration: Duration,
    ) -> Result<ActionReceipt> {
        self.aim(action, origin)?;
        let receipt = ActionReceipt::begin(action).trigger();
        self.session.device.instrument(
            "swipe",
            action,
            [
                ("x0", origin[0].to_string()),
                ("y0", origin[1].to_string()),
                ("x1", destination[0].to_string()),
                ("y1", destination[1].to_string()),
                ("steps", "12".to_owned()),
                ("duration_ms", duration.as_millis().max(1).to_string()),
            ],
        )?;
        self.complete(receipt.finish(), Some(destination))
    }

    /// Hold one physical touch in place long enough to cross the platform's
    /// long-touch boundary.
    pub fn long_press(
        &mut self,
        action: &str,
        point: [i32; 2],
        duration: Duration,
    ) -> Result<ActionReceipt> {
        self.aim(action, point)?;
        let receipt = ActionReceipt::begin(action).trigger();
        self.session.device.instrument(
            "hold",
            action,
            [
                ("x", point[0].to_string()),
                ("y", point[1].to_string()),
                ("steps", "12".to_owned()),
                ("duration_ms", duration.as_millis().max(1).to_string()),
            ],
        )?;
        self.complete(receipt.finish(), Some(point))
    }

    pub fn key_code(&mut self, action: &str, code: &str) -> Result<ActionReceipt> {
        let receipt = ActionReceipt::begin(action).trigger();
        self.session
            .device
            .instrument("key", action, [("key", format!("KEYCODE_{code}"))])?;
        self.complete(receipt.finish(), None)
    }

    /// Type ASCII text through Android's virtual keyboard input path.
    pub fn text(&mut self, action: &str, text: &str) -> Result<ActionReceipt> {
        let receipt = ActionReceipt::begin(action).trigger();
        self.session
            .device
            .instrument("text", action, [("text", text.to_owned())])?;
        self.complete(receipt.finish(), None)
    }

    pub fn pinch(&mut self, action: &str, gesture: AndroidPinch) -> Result<ActionReceipt> {
        let center = [
            i32::midpoint(gesture.first[0], gesture.second[0]),
            i32::midpoint(gesture.first[1], gesture.second[1]),
        ];
        self.aim(action, center)?;
        let receipt = ActionReceipt::begin(action).trigger();
        self.session.device.instrument(
            "pinch",
            action,
            [
                ("x0", gesture.first[0].to_string()),
                ("y0", gesture.first[1].to_string()),
                ("x1", gesture.second[0].to_string()),
                ("y1", gesture.second[1].to_string()),
                ("dx", gesture.spread[0].to_string()),
                ("dy", gesture.spread[1].to_string()),
                ("steps", gesture.steps.to_string()),
                (
                    "duration_ms",
                    gesture.duration.as_millis().max(1).to_string(),
                ),
            ],
        )?;
        self.complete(receipt.finish(), Some(center))
    }

    pub fn settle(&self, duration: Duration) {
        thread::sleep(duration);
    }

    pub fn finish(mut self) -> Result<O> {
        let surface = StorySurface::new(&self.session);
        self.observer.finish(surface)?;
        Ok(self.observer)
    }

    fn aim(&mut self, action: &str, point: [i32; 2]) -> Result<()> {
        self.emit(StoryEvent::Fact(StoryFact::PointerAimed {
            target: Some(action),
            pointer: narrow(point),
            anchor: None,
        }))
    }

    fn complete(
        &mut self,
        receipt: ActionReceipt,
        point: Option<[i32; 2]>,
    ) -> Result<ActionReceipt> {
        self.emit(StoryEvent::Fact(StoryFact::ActionDispatched {
            action: receipt.action(),
            target: None,
            pointer: point.map(narrow),
            gesture_started_ns: receipt.gesture_started_ns(),
            triggered_ns: receipt.triggered_ns(),
            completed_ns: receipt.completed_ns(),
        }))?;
        Ok(receipt)
    }

    fn emit(&mut self, event: StoryEvent<'_>) -> Result<()> {
        let surface = StorySurface::new(&self.session);
        self.observer.observe(event, surface)
    }
}

impl<O: StoryObserver> Choreography for AndroidChoreography<'_, O> {
    fn chapter(&mut self, title: &str) -> Result<()> {
        Self::chapter(self, title)
    }

    fn hold(&mut self, duration: Duration) -> Result<()> {
        Self::hold(self, duration)
    }

    fn tempo(&mut self, tempo: StoryTempo) -> Result<()> {
        Self::tempo(self, tempo)
    }
}

struct AndroidSession<'device> {
    device: &'device AndroidDevice,
}

impl CaptureSurface for AndroidSession<'_> {
    fn capture(&self) -> Result<Frame> {
        let output = self.device.checked(["exec-out", "screencap", "-p"])?;
        let mut png = tempfile::NamedTempFile::new().map_err(|source| Error::Io {
            operation: "create Android capture staging file",
            path: PathBuf::from("/tmp"),
            source,
        })?;
        png.write_all(&output.stdout).map_err(|source| Error::Io {
            operation: "stage Android capture",
            path: png.path().to_owned(),
            source,
        })?;
        Frame::load_png(png.path())
    }
}

/// Perfetto capture observer for one Android choreography execution.
pub struct AndroidPerfetto {
    device: AndroidDevice,
    session: Option<String>,
    remote: String,
    output: PathBuf,
    events_path: PathBuf,
    events: BufWriter<File>,
}

impl AndroidPerfetto {
    pub fn begin(
        device: &AndroidDevice,
        config: &str,
        remote: impl Into<String>,
        output: impl Into<PathBuf>,
        events: impl AsRef<Path>,
    ) -> Result<Self> {
        let remote = remote.into();
        if !remote.starts_with("/data/misc/perfetto-traces/") {
            return Err(Error::Verdict {
                detail: "Perfetto output must live beneath /data/misc/perfetto-traces".to_owned(),
            });
        }
        let output = output.into();
        let events_path = events.as_ref();
        if let Some(parent) = events_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                operation: "create Android event-trace directory",
                path: parent.to_owned(),
                source,
            })?;
        }
        let events = File::create(events_path).map_err(|source| Error::Io {
            operation: "create Android event trace",
            path: events_path.to_owned(),
            source,
        })?;
        let session = format!("egui_tester_{}", std::process::id());
        let detach = format!("--detach={session}");
        let mut command = device.command();
        let mut child = command
            .args([
                "shell", "perfetto", "--txt", "-c", "-", "-o", &remote, &detach,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| Error::Io {
                operation: "start Android Perfetto",
                path: device.adb.clone(),
                source,
            })?;
        let mut stdin = child.stdin.take().ok_or_else(|| Error::Verdict {
            detail: "Perfetto child exposed no configuration pipe".to_owned(),
        })?;
        stdin
            .write_all(config.as_bytes())
            .map_err(|source| Error::Io {
                operation: "write Android Perfetto configuration",
                path: device.adb.clone(),
                source,
            })?;
        drop(stdin);
        let detached = child.wait_with_output().map_err(|source| Error::Io {
            operation: "detach Android Perfetto session",
            path: device.adb.clone(),
            source,
        })?;
        if !detached.status.success() {
            return Err(Error::Command {
                command: "adb shell perfetto --detach".to_owned(),
                status: detached.status.to_string(),
                stderr: String::from_utf8_lossy(&detached.stderr).into_owned(),
            });
        }
        Ok(Self {
            device: device.clone(),
            session: Some(session),
            remote,
            output,
            events_path: events_path.to_owned(),
            events: BufWriter::new(events),
        })
    }

    fn seal(&mut self) -> Result<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        let attach = format!("--attach={session}");
        let _stopped = self
            .device
            .checked(["shell", "perfetto", &attach, "--stop"])?;
        let destination = self.output.as_os_str().to_owned();
        let _pulled =
            self.device
                .checked_os(["pull".into(), self.remote.clone().into(), destination])?;
        self.events.flush().map_err(|source| Error::Io {
            operation: "flush Android event trace",
            path: self.events_path.clone(),
            source,
        })
    }
}

impl StoryObserver for AndroidPerfetto {
    fn observe(&mut self, event: StoryEvent<'_>, _surface: StorySurface<'_>) -> Result<()> {
        let record = AndroidEvent {
            host_monotonic_ns: egui_tester_witness::monotonic_ns(),
            event,
        };
        serde_json::to_writer(&mut self.events, &record).map_err(|source| Error::Verdict {
            detail: format!("serialize Android choreography event: {source}"),
        })?;
        self.events.write_all(b"\n").map_err(|source| Error::Io {
            operation: "append Android choreography event",
            path: self.events_path.clone(),
            source,
        })
    }

    fn finish(&mut self, _surface: StorySurface<'_>) -> Result<()> {
        self.seal()
    }

    fn permits_performance_verdicts(&self) -> bool {
        true
    }
}

impl Drop for AndroidPerfetto {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let attach = format!("--attach={session}");
        let _stopped = self
            .device
            .checked(["shell", "perfetto", &attach, "--stop"]);
    }
}

#[derive(Serialize)]
struct AndroidEvent<'a> {
    host_monotonic_ns: u64,
    event: StoryEvent<'a>,
}

fn narrow(point: [i32; 2]) -> [i16; 2] {
    point.map(|axis| i16::try_from(axis).unwrap_or(if axis < 0 { i16::MIN } else { i16::MAX }))
}
