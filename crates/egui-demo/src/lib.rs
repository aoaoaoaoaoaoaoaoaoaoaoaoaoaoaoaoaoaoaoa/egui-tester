//! Optional film and trace observer for [`egui_tester::Story`].
//!
//! Ordinary acceptance stories remain the source of truth. Attaching a
//! [`Recorder`] turns the same causal event stream into an H.264 film and a
//! JSONL execution trace without granting any new product authority.

use std::{
    fs::{self, File},
    io::{BufWriter, ErrorKind, Write as _},
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use egui_tester::{
    Anchor, Error, Frame, Result, StoryCue, StoryEvent, StoryFact, StoryObserver, StorySurface,
    demand,
};
use font8x8::{BASIC_FONTS, UnicodeFonts as _};
use serde::Serialize;

const TRACE_SCHEMA: &str = "egui-demo.trace/2";
const TARGET_FLIGHT: Duration = Duration::from_millis(220);
const TARGET_REST: Duration = Duration::from_millis(100);
const ACTION_REST: Duration = Duration::from_millis(160);
const OBSERVATION_REST: Duration = Duration::from_millis(220);
const CHAPTER_REST: Duration = Duration::from_millis(1_500);

/// Immutable encoding policy for one story film.
#[derive(Clone, Debug)]
pub struct RecorderConfig {
    output: PathBuf,
    frames_per_second: NonZeroU32,
    encoding_profile: EncodingProfile,
}

/// H.264 cost and fidelity policy, independent of temporal sampling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EncodingProfile {
    /// Fast artifact suitable for exercising recording in an acceptance rail.
    #[default]
    Proof,
    /// Presentation artifact with expensive, animation-tuned compression.
    Showpiece,
}

impl EncodingProfile {
    const fn x264(self) -> (&'static str, &'static str, Option<&'static str>) {
        match self {
            Self::Proof => ("veryfast", "18", None),
            Self::Showpiece => ("slow", "12", Some("animation")),
        }
    }

    const fn stages_lossless_capture(self) -> bool {
        matches!(self, Self::Showpiece)
    }
}

impl RecorderConfig {
    #[must_use]
    pub fn new(output: impl Into<PathBuf>) -> Self {
        Self {
            output: output.into(),
            frames_per_second: NonZeroU32::new(60).unwrap_or(NonZeroU32::MIN),
            encoding_profile: EncodingProfile::Proof,
        }
    }

    #[must_use]
    pub const fn frames_per_second(mut self, frames_per_second: NonZeroU32) -> Self {
        self.frames_per_second = frames_per_second;
        self
    }

    #[must_use]
    pub const fn encoding_profile(mut self, encoding_profile: EncodingProfile) -> Self {
        self.encoding_profile = encoding_profile;
        self
    }
}

/// Synchronous story observer producing a film and its typed execution trace.
pub struct Recorder {
    config: RecorderConfig,
    trace_path: PathBuf,
    trace: BufWriter<File>,
    encoder: Option<Encoder>,
    pointer: Option<Point>,
    target: Option<Rect>,
    begun: Instant,
    frames_written: u64,
    sealed: bool,
}

impl Recorder {
    pub fn forge(config: RecorderConfig) -> Result<Self> {
        demand(
            config
                .output
                .extension()
                .is_some_and(|extension| extension == "mp4"),
            format!(
                "egui-demo output must be an .mp4 file: {}",
                config.output.display()
            ),
        )?;
        if let Some(parent) = config.output.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| io("create demo artifact directory", parent, source))?;
        }
        let trace_path = config.output.with_extension("events.jsonl");
        let trace = File::create(&trace_path)
            .map(BufWriter::new)
            .map_err(|source| io("create story event trace", &trace_path, source))?;
        Ok(Self {
            config,
            trace_path,
            trace,
            encoder: None,
            pointer: None,
            target: None,
            begun: Instant::now(),
            frames_written: 0,
            sealed: false,
        })
    }

    #[must_use]
    pub fn output(&self) -> &Path {
        &self.config.output
    }

    #[must_use]
    pub fn trace_path(&self) -> &Path {
        &self.trace_path
    }

    #[must_use]
    pub const fn frames_written(&self) -> u64 {
        self.frames_written
    }

    /// Publish the sealed capture. Call this after terminating the product so
    /// an offline showpiece transcode cannot contend with an idle live app.
    pub fn publish(mut self) -> Result<Self> {
        self.seal()?;
        self.encoder
            .as_mut()
            .ok_or_else(|| Error::Verdict {
                detail: "egui-demo encoder vanished before publication".to_owned(),
            })?
            .publish()?;
        Ok(self)
    }

    fn aim(
        &mut self,
        surface: StorySurface<'_>,
        pointer: [i16; 2],
        anchor: Option<&Anchor>,
    ) -> Result<()> {
        let destination = Point::from(pointer);
        let origin = self.pointer.unwrap_or(destination);
        self.target = anchor.map(|anchor| Rect::from(anchor.rect));
        self.live_interval(surface, TARGET_FLIGHT, |step, steps| Scene::Target {
            pointer: origin.lerp(destination, step + 1, steps),
        })?;
        self.pointer = Some(destination);
        self.live_interval(surface, TARGET_REST, |_, _| Scene::Target {
            pointer: destination,
        })
    }

    fn action(&mut self, surface: StorySurface<'_>, pointer: Option<[i16; 2]>) -> Result<()> {
        if let Some(pointer) = pointer {
            self.pointer = Some(Point::from(pointer));
        }
        let pointer = self.pointer;
        self.live_interval(surface, ACTION_REST, |phase, phases| Scene::Action {
            pointer,
            phase,
            phases,
        })
    }

    fn observation(&mut self, surface: StorySurface<'_>) -> Result<()> {
        let pointer = self.pointer;
        self.live_interval(surface, OBSERVATION_REST, |_, _| Scene::Settled { pointer })
    }

    fn chapter(&mut self, surface: StorySurface<'_>, title: &str) -> Result<()> {
        self.live_interval(surface, CHAPTER_REST, |_, _| Scene::Chapter { title })
    }

    fn hold(&mut self, surface: StorySurface<'_>, duration: Duration) -> Result<()> {
        let pointer = self.pointer;
        self.live_interval(surface, duration, |_, _| Scene::Settled { pointer })
    }

    /// Sample continuously while advancing an invariant output clock. When a
    /// capture exceeds one film period, the freshest product frame fills every
    /// elapsed tick; compression or capture latency cannot stretch world time.
    fn live_interval<'a>(
        &mut self,
        surface: StorySurface<'_>,
        duration: Duration,
        mut scene: impl FnMut(u32, u32) -> Scene<'a>,
    ) -> Result<()> {
        let count = self.frames(duration);
        let period = self.frame_period();
        let begun = Instant::now();
        let mut phase = 0;
        let mut capture_cost = Duration::ZERO;
        while phase < count {
            let tick = begun + period * phase;
            let capture_at = tick.checked_sub(capture_cost).unwrap_or(begun);
            if let Some(rest) = capture_at.checked_duration_since(Instant::now()) {
                thread::sleep(rest);
            }
            let capture_begun = Instant::now();
            let frame = surface.capture()?;
            capture_cost = capture_begun.elapsed();
            let due = self.frames_due(begun.elapsed()).min(count).max(phase + 1);
            while phase < due {
                self.write_scene(&frame, scene(phase, count))?;
                phase += 1;
            }
        }
        Ok(())
    }

    fn frame_period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.config.frames_per_second.get()))
    }

    fn write_scene(&mut self, frame: &Frame, scene: Scene<'_>) -> Result<()> {
        let mut rgba = frame.rgba().to_vec();
        paint_scene(&mut rgba, frame.width(), frame.height(), self.target, scene);
        if self.encoder.is_none() {
            self.encoder = Some(Encoder::ignite(
                &self.config.output,
                frame.width(),
                frame.height(),
                self.config.frames_per_second,
                self.config.encoding_profile,
            )?);
        }
        self.encoder
            .as_mut()
            .ok_or_else(|| Error::Verdict {
                detail: "egui-demo encoder vanished after ignition".to_owned(),
            })?
            .write(frame.width(), frame.height(), &rgba)?;
        self.frames_written += 1;
        Ok(())
    }

    fn frames(&self, duration: Duration) -> u32 {
        let numerator = duration.as_nanos() * u128::from(self.config.frames_per_second.get());
        let count = numerator.div_ceil(1_000_000_000).max(1);
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn frames_due(&self, elapsed: Duration) -> u32 {
        let count = elapsed.as_nanos() * u128::from(self.config.frames_per_second.get())
            / 1_000_000_000
            + 1;
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn trace(&mut self, event: StoryEvent<'_>) -> Result<()> {
        let elapsed_ns = u64::try_from(self.begun.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let record = TraceRecord {
            schema: TRACE_SCHEMA,
            elapsed_ns,
            film_frame: self.frames_written,
            event,
        };
        serde_json::to_writer(&mut self.trace, &record).map_err(|source| Error::Verdict {
            detail: format!(
                "serialize story event trace {}: {source}",
                self.trace_path.display()
            ),
        })?;
        self.trace
            .write_all(b"\n")
            .map_err(|source| io("append story event trace", &self.trace_path, source))
    }

    fn seal(&mut self) -> Result<()> {
        if self.sealed {
            return Ok(());
        }
        self.trace
            .flush()
            .map_err(|source| io("flush story event trace", &self.trace_path, source))?;
        let encoder = self.encoder.as_mut().ok_or_else(|| Error::Verdict {
            detail: "story emitted no recordable surface frames".to_owned(),
        })?;
        encoder.seal()?;
        self.sealed = true;
        Ok(())
    }
}

impl StoryObserver for Recorder {
    fn observe(&mut self, event: StoryEvent<'_>, surface: StorySurface<'_>) -> Result<()> {
        self.trace(event)?;
        match event {
            StoryEvent::Cue(StoryCue::Chapter { title }) => self.chapter(surface, title),
            StoryEvent::Cue(StoryCue::Hold { duration }) => self.hold(surface, duration),
            StoryEvent::Fact(StoryFact::TargetResolved { .. }) => Ok(()),
            StoryEvent::Fact(StoryFact::PointerAimed {
                pointer, anchor, ..
            }) => self.aim(surface, pointer, anchor),
            StoryEvent::Fact(StoryFact::ActionDispatched { pointer, .. }) => {
                self.action(surface, pointer)
            }
            StoryEvent::Fact(StoryFact::ObservationMatched { .. }) => self.observation(surface),
        }
    }

    fn finish(&mut self, _surface: StorySurface<'_>) -> Result<()> {
        self.seal()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.sealed {
            return;
        }
        let _flushed = self.trace.flush();
        drop(self.encoder.take());
    }
}

#[derive(Serialize)]
struct TraceRecord<'a> {
    schema: &'static str,
    elapsed_ns: u64,
    film_frame: u64,
    event: StoryEvent<'a>,
}

struct Encoder {
    output: PathBuf,
    capture: PathBuf,
    encoding_profile: EncodingProfile,
    child: Option<Child>,
    input: Option<ChildStdin>,
    width: u32,
    height: u32,
    frame_bytes: usize,
    capture_sealed: bool,
    published: bool,
}

impl Encoder {
    fn ignite(
        output: &Path,
        width: u32,
        height: u32,
        frames_per_second: NonZeroU32,
        encoding_profile: EncodingProfile,
    ) -> Result<Self> {
        let geometry = format!("{width}x{height}");
        let rate = frames_per_second.to_string();
        let capture = if encoding_profile.stages_lossless_capture() {
            output.with_extension("capture.mkv")
        } else {
            output.to_owned()
        };
        let mut command = Command::new("ffmpeg");
        let _command = command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgba",
            "-video_size",
            &geometry,
            "-framerate",
            &rate,
            "-i",
            "pipe:0",
            "-an",
        ]);
        match encoding_profile {
            EncodingProfile::Proof => {
                let (preset, crf, _) = encoding_profile.x264();
                let _proof = command.args([
                    "-vf",
                    "pad=ceil(iw/2)*2:ceil(ih/2)*2",
                    "-c:v",
                    "libx264",
                    "-preset",
                    preset,
                    "-crf",
                    crf,
                    "-pix_fmt",
                    "yuv420p",
                    "-movflags",
                    "+faststart",
                ]);
            }
            EncodingProfile::Showpiece => {
                let _capture = command.args([
                    "-c:v",
                    "libx264rgb",
                    "-preset",
                    "ultrafast",
                    "-qp",
                    "0",
                    "-pix_fmt",
                    "bgr0",
                ]);
            }
        }
        let _command = command
            .arg(&capture)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|source| {
            if source.kind() == ErrorKind::NotFound {
                Error::MissingTool("ffmpeg")
            } else {
                io("spawn egui-demo encoder", output, source)
            }
        })?;
        let input = child.stdin.take().ok_or_else(|| Error::Verdict {
            detail: "ffmpeg did not expose its raw-video input".to_owned(),
        })?;
        let frame_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| Error::Verdict {
                detail: format!(
                    "egui-demo frame geometry {width}x{height} exceeds platform limits"
                ),
            })?;
        Ok(Self {
            output: output.to_owned(),
            capture,
            encoding_profile,
            child: Some(child),
            input: Some(input),
            width,
            height,
            frame_bytes,
            capture_sealed: false,
            published: false,
        })
    }

    fn write(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        demand(
            (width, height) == (self.width, self.height),
            format!(
                "egui-demo frame geometry changed from {}x{} to {width}x{height}",
                self.width, self.height
            ),
        )?;
        demand(
            rgba.len() == self.frame_bytes,
            format!(
                "egui-demo frame has {} RGBA bytes, expected {}",
                rgba.len(),
                self.frame_bytes
            ),
        )?;
        self.input
            .as_mut()
            .ok_or_else(|| Error::Verdict {
                detail: "egui-demo film was written after encoder closure".to_owned(),
            })?
            .write_all(rgba)
            .map_err(|source| io("write egui-demo frame", &self.output, source))
    }

    fn seal(&mut self) -> Result<()> {
        if self.capture_sealed {
            return Ok(());
        }
        drop(self.input.take());
        let child = self.child.take().ok_or_else(|| Error::Verdict {
            detail: "egui-demo encoder process vanished before closure".to_owned(),
        })?;
        let output = child
            .wait_with_output()
            .map_err(|source| io("wait for egui-demo encoder", &self.output, source))?;
        self.capture_sealed = true;
        demand_command(output, format!("ffmpeg -> {}", self.capture.display()))?;
        if !self.encoding_profile.stages_lossless_capture() {
            self.published = true;
        }
        Ok(())
    }

    fn publish(&mut self) -> Result<()> {
        self.seal()?;
        if self.published {
            return Ok(());
        }
        self.transcode_showpiece()?;
        fs::remove_file(&self.capture)
            .map_err(|source| io("remove lossless egui-demo capture", &self.capture, source))?;
        self.published = true;
        Ok(())
    }

    fn transcode_showpiece(&self) -> Result<()> {
        let (preset, crf, tune) = self.encoding_profile.x264();
        let mut command = Command::new("ffmpeg");
        let _command = command.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]);
        let _capture = command.arg(&self.capture);
        let _command = command.args([
            "-an",
            "-vf",
            "pad=ceil(iw/2)*2:ceil(ih/2)*2",
            "-c:v",
            "libx264",
            "-preset",
            preset,
            "-crf",
            crf,
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ]);
        if let Some(tune) = tune {
            let _tune = command.args(["-tune", tune]);
        }
        let output = command
            .arg(&self.output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| io("spawn egui-demo showpiece transcode", &self.output, source))?;
        demand_command(output, format!("ffmpeg -> {}", self.output.display()))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.capture_sealed {
            drop(self.input.take());
            if let Some(mut child) = self.child.take() {
                let _status = child.wait();
            }
        }
        if self.encoding_profile.stages_lossless_capture() && !self.published {
            let _removed = fs::remove_file(&self.capture);
        }
    }
}

fn demand_command(output: std::process::Output, command: String) -> Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Command {
            command,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Clone, Copy)]
enum Scene<'a> {
    Target {
        pointer: Point,
    },
    Action {
        pointer: Option<Point>,
        phase: u32,
        phases: u32,
    },
    Settled {
        pointer: Option<Point>,
    },
    Chapter {
        title: &'a str,
    },
}

#[derive(Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn lerp(self, other: Self, step: u32, steps: u32) -> Self {
        let step = i64::from(step);
        let steps = i64::from(steps);
        Self {
            x: (i64::from(self.x) + i64::from(other.x - self.x) * step / steps) as i32,
            y: (i64::from(self.y) + i64::from(other.y - self.y) * step / steps) as i32,
        }
    }
}

impl From<(i16, i16)> for Point {
    fn from((x, y): (i16, i16)) -> Self {
        Self {
            x: i32::from(x),
            y: i32::from(y),
        }
    }
}

impl From<[i16; 2]> for Point {
    fn from([x, y]: [i16; 2]) -> Self {
        Self::from((x, y))
    }
}

#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl From<[f32; 4]> for Rect {
    fn from([left, top, right, bottom]: [f32; 4]) -> Self {
        Self {
            left: left.floor() as i32,
            top: top.floor() as i32,
            right: right.ceil() as i32,
            bottom: bottom.ceil() as i32,
        }
    }
}

fn paint_scene(rgba: &mut [u8], width: u32, height: u32, target: Option<Rect>, scene: Scene<'_>) {
    match scene {
        Scene::Target { pointer } => {
            if let Some(target) = target {
                outline(rgba, width, height, target, [35, 211, 255, 210], 2);
            }
            ring(rgba, width, height, pointer, 8, 11, [255, 255, 255, 230]);
            ring(rgba, width, height, pointer, 11, 14, [20, 170, 220, 210]);
        }
        Scene::Action {
            pointer,
            phase,
            phases,
        } => {
            if let Some(target) = target {
                outline(rgba, width, height, target, [35, 211, 255, 180], 2);
            }
            if let Some(pointer) = pointer {
                let radius = 10 + i32::try_from(phase * 8 / phases.max(1)).unwrap_or(8);
                ring(
                    rgba,
                    width,
                    height,
                    pointer,
                    radius,
                    radius + 3,
                    [255, 206, 75, 230],
                );
            }
        }
        Scene::Settled { pointer } => {
            if let Some(pointer) = pointer {
                ring(rgba, width, height, pointer, 5, 8, [35, 211, 255, 150]);
            }
        }
        Scene::Chapter { title } => chapter_card(rgba, width, height, title),
    }
}

fn chapter_card(rgba: &mut [u8], width: u32, height: u32, title: &str) {
    fill(
        rgba,
        width,
        height,
        Rect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        },
        [4, 9, 13, 150],
    );
    let scale = (width / 520).clamp(2, 4) as i32;
    let text_width = i32::try_from(title.chars().count()).unwrap_or(i32::MAX) * 8 * scale;
    let panel_width = (text_width + 96).min(width as i32 - 64).max(240);
    let panel_height = 8 * scale + 72;
    let left = (width as i32 - panel_width) / 2;
    let top = (height as i32 - panel_height) / 2;
    fill(
        rgba,
        width,
        height,
        Rect {
            left,
            top,
            right: left + panel_width,
            bottom: top + panel_height,
        },
        [6, 13, 19, 225],
    );
    fill(
        rgba,
        width,
        height,
        Rect {
            left,
            top,
            right: left + 8,
            bottom: top + panel_height,
        },
        [35, 211, 255, 255],
    );
    text(
        rgba,
        width,
        height,
        Point {
            x: left + 48,
            y: top + 36,
        },
        title,
        scale,
        [235, 245, 249, 255],
    );
}

fn text(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    origin: Point,
    text: &str,
    scale: i32,
    color: [u8; 4],
) {
    let mut x = origin.x;
    for glyph in text
        .chars()
        .filter_map(|character| BASIC_FONTS.get(character))
    {
        for (row, bits) in glyph.into_iter().enumerate() {
            for column in 0..8 {
                if bits & (1 << column) == 0 {
                    continue;
                }
                fill(
                    rgba,
                    width,
                    height,
                    Rect {
                        left: x + column * scale,
                        top: origin.y + row as i32 * scale,
                        right: x + (column + 1) * scale,
                        bottom: origin.y + (row as i32 + 1) * scale,
                    },
                    color,
                );
            }
        }
        x += 8 * scale;
    }
}

fn outline(rgba: &mut [u8], width: u32, height: u32, rect: Rect, color: [u8; 4], stroke: i32) {
    fill(
        rgba,
        width,
        height,
        Rect {
            bottom: rect.top + stroke,
            ..rect
        },
        color,
    );
    fill(
        rgba,
        width,
        height,
        Rect {
            top: rect.bottom - stroke,
            ..rect
        },
        color,
    );
    fill(
        rgba,
        width,
        height,
        Rect {
            right: rect.left + stroke,
            ..rect
        },
        color,
    );
    fill(
        rgba,
        width,
        height,
        Rect {
            left: rect.right - stroke,
            ..rect
        },
        color,
    );
}

fn ring(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    center: Point,
    inner: i32,
    outer: i32,
    color: [u8; 4],
) {
    let inner_squared = inner * inner;
    let outer_squared = outer * outer;
    for y in center.y - outer..=center.y + outer {
        for x in center.x - outer..=center.x + outer {
            let distance = (x - center.x).pow(2) + (y - center.y).pow(2);
            if distance >= inner_squared && distance <= outer_squared {
                blend(rgba, width, height, x, y, color);
            }
        }
    }
}

fn fill(rgba: &mut [u8], width: u32, height: u32, rect: Rect, color: [u8; 4]) {
    let left = rect.left.clamp(0, width as i32);
    let right = rect.right.clamp(0, width as i32);
    let top = rect.top.clamp(0, height as i32);
    let bottom = rect.bottom.clamp(0, height as i32);
    for y in top..bottom {
        for x in left..right {
            blend(rgba, width, height, x, y, color);
        }
    }
}

fn blend(rgba: &mut [u8], width: u32, height: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let offset = (y as usize * width as usize + x as usize) * 4;
    let alpha = u16::from(color[3]);
    for channel in 0..3 {
        let under = u16::from(rgba[offset + channel]);
        let over = u16::from(color[channel]);
        rgba[offset + channel] = ((over * alpha + under * (255 - alpha)) / 255) as u8;
    }
    rgba[offset + 3] = 255;
}

fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        operation,
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct FantasyAbvObservation;

    fn fantasy_abv_story<O: StoryObserver>(
        story: &mut egui_tester::Story<'_, '_, FantasyAbvObservation, O>,
    ) -> Result<()> {
        let _hovered = story
            .point("thumbnail:42", egui_tester::Motion::default())?
            .next_frame()?;
        story.hold(Duration::from_millis(400))?;

        let grid = story.anchor("thumbnail-grid")?.center();
        let _zoomed = story
            .modified_wheel(
                grid,
                -4,
                egui_tester::Wheel::default(),
                egui_tester::Modifiers::CTRL,
            )?
            .next_frame()?;
        let _reparented = story
            .drag_to(
                "query-atom:0",
                "query-group:1",
                egui_tester::Drag::default(),
            )?
            .next_frame()?;
        let _moved = story
            .motion_to((240, 180), egui_tester::Motion::default())?
            .next_frame()?;
        let _clicked = story
            .click_current(egui_tester::Button::Primary)?
            .next_frame()?;
        Ok(())
    }

    #[test]
    fn abv_shaped_story_is_observer_agnostic() {
        let _silent = std::hint::black_box(fantasy_abv_story::<egui_tester::Silent>);
        let _filmed = std::hint::black_box(fantasy_abv_story::<Recorder>);
    }

    #[test]
    fn scene_painting_changes_only_valid_rgba() {
        let mut rgba = vec![0; 64 * 48 * 4];
        paint_scene(
            &mut rgba,
            64,
            48,
            Some(Rect {
                left: 4,
                top: 4,
                right: 40,
                bottom: 30,
            }),
            Scene::Target {
                pointer: Point { x: 20, y: 20 },
            },
        );
        assert!(rgba.chunks_exact(4).any(|pixel| pixel != [0, 0, 0, 0]));
        assert!(rgba.chunks_exact(4).all(|pixel| pixel.len() == 4));
    }

    #[test]
    fn frame_count_rounds_up_without_vanishing() {
        let config = RecorderConfig::new("film.mp4");
        let directory = tempfile::tempdir().expect("temporary demo directory");
        let recorder = Recorder::forge(RecorderConfig {
            output: directory.path().join(config.output),
            ..config
        })
        .expect("forge recorder");
        assert_eq!(recorder.config.frames_per_second.get(), 60);
        assert_eq!(recorder.config.encoding_profile, EncodingProfile::Proof);
        assert_eq!(recorder.frames(Duration::ZERO), 1);
        assert_eq!(recorder.frames(Duration::from_millis(84)), 6);
        assert_eq!(recorder.frames_due(Duration::ZERO), 1);
        assert_eq!(recorder.frames_due(Duration::from_millis(16)), 1);
        assert_eq!(recorder.frames_due(Duration::from_millis(17)), 2);
    }

    #[test]
    fn showpiece_profile_is_slow_and_animation_tuned() {
        assert!(!EncodingProfile::Proof.stages_lossless_capture());
        assert!(EncodingProfile::Showpiece.stages_lossless_capture());
        assert_eq!(
            EncodingProfile::Showpiece.x264(),
            ("slow", "12", Some("animation"))
        );
    }
}
