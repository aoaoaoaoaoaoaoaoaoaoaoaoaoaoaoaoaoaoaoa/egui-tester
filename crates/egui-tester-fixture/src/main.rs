use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32};
use serde::Serialize;

#[cfg(test)]
use {egui_tester as _, tempfile as _};

const TITLE: &str = "egui tester fixture";

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        Some(flag)
            if flag == std::ffi::OsStr::new("--try-read")
                || flag == std::ffi::OsStr::new("--try-write") =>
        {
            let Some(path) = args.next() else {
                eprintln!("{} requires a path", flag.to_string_lossy());
                std::process::exit(64);
            };
            if flag == std::ffi::OsStr::new("--try-read") {
                audit_hidden(Path::new(&path));
            }
            audit_denial(Path::new(&path));
        }
        Some(flag) => {
            eprintln!("unknown fixture flag `{}`", flag.to_string_lossy());
            std::process::exit(64);
        }
        None => {}
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(|context| Ok(Box::new(Fixture::new(context)))),
    )
}

fn audit_hidden(path: &Path) -> ! {
    match std::fs::read(path) {
        Ok(_) => {
            eprintln!(
                "HERMETICITY BREACH: isolated application read `{}`",
                path.display()
            );
            std::process::exit(70);
        }
        Err(err) => {
            eprintln!(
                "undeclared real file is invisible to test: `{}` denied: {err}",
                path.display()
            );
            std::process::exit(73);
        }
    }
}

fn audit_denial(path: &Path) -> ! {
    match std::fs::write(path, b"breach") {
        Ok(()) => {
            eprintln!(
                "HERMETICITY BREACH: test application wrote `{}`",
                path.display()
            );
            std::process::exit(70);
        }
        Err(err) => {
            eprintln!(
                "would write real file from test: `{}` denied: {err}",
                path.display()
            );
            std::process::exit(73);
        }
    }
}

struct Fixture {
    probe: Option<PathBuf>,
    count: u64,
    violet: bool,
    text: String,
}

impl Fixture {
    fn new(_context: &eframe::CreationContext<'_>) -> Self {
        Self {
            probe: std::env::var_os("EGUI_TESTER_PROBE").map(PathBuf::from),
            count: 0,
            violet: false,
            text: String::new(),
        }
    }
}

impl eframe::App for Fixture {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let fill = if self.violet {
            Color32::from_rgb(74, 28, 126)
        } else {
            Color32::from_rgb(14, 62, 72)
        };
        let mut anchors = Vec::new();
        let mut text_focused = false;
        let _background = ui.painter().rect_filled(ui.max_rect(), 0, fill);
        let _panel = egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            let _heading = ui.heading("Black-box fixture");
            let _count = ui.label(format!("Count: {}", self.count));
            let increment = ui.button("Increment");
            anchors.push(anchor("increment", increment.rect));
            if increment.clicked() {
                self.count += 1;
            }
            let toggle = ui.button("Toggle color");
            anchors.push(anchor("toggle", toggle.rect));
            if toggle.clicked() {
                self.violet = !self.violet;
            }
            let text = ui.text_edit_singleline(&mut self.text);
            anchors.push(anchor("text", text.rect));
            text_focused = text.has_focus();
        });
        if let Some(path) = &self.probe {
            let probe = Probe {
                frame: ui.ctx().cumulative_frame_nr(),
                anchors,
                state: State {
                    count: self.count,
                    violet: self.violet,
                    text: &self.text,
                    text_focused,
                },
            };
            if let Ok(bytes) = serde_json::to_vec(&probe) {
                write_atomic(path, &bytes);
            }
        }
    }
}

#[derive(Serialize)]
struct Probe<'a> {
    frame: u64,
    anchors: Vec<Anchor>,
    state: State<'a>,
}

#[derive(Serialize)]
struct Anchor {
    name: &'static str,
    rect: [f32; 4],
}

#[derive(Serialize)]
struct State<'a> {
    count: u64,
    violet: bool,
    text: &'a str,
    text_focused: bool,
}

fn anchor(name: &'static str, rect: egui::Rect) -> Anchor {
    Anchor {
        name,
        rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, bytes).is_ok() {
        let _ignored = std::fs::rename(temporary, path);
    }
}
