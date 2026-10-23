//! Optional film, still, and trace observers for [`egui_tester::Story`].
//!
//! Ordinary acceptance stories remain the source of truth. Attaching a
//! [`Recorder`] turns the same causal event stream into an H.264 film and a
//! JSONL execution trace, painted with a diagnostic or presentation
//! [`Overlay`]; a [`Darkroom`] develops every still the score names.
//! Neither grants any new product authority, and both may share one take.

mod overlay;

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
    StoryTempo, demand,
};
use serde::Serialize;

pub use overlay::Overlay;
use overlay::{Canvas, Paint, Point, Rect, Scene};

const TRACE_SCHEMA: &str = "egui-demo.trace/4";
const TARGET_FLIGHT: Duration = Duration::from_millis(220);
const TARGET_REST: Duration = Duration::from_millis(100);
const ACTION_REST: Duration = Duration::from_millis(160);
const OBSERVATION_REST: Duration = Duration::from_millis(220);
const CHAPTER_REST: Duration = Duration::from_millis(1_500);

/// Immutable policy for one story film: destination, clock, overlay, and
/// encoding.
#[derive(Clone, Debug)]
pub struct RecorderConfig {
    output: PathBuf,
    frames_per_second: NonZeroU32,
    overlay: Overlay,
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
            overlay: Overlay::Diagnostic,
            encoding_profile: EncodingProfile::Proof,
        }
    }

    #[must_use]
    pub const fn frames_per_second(mut self, frames_per_second: NonZeroU32) -> Self {
        self.frames_per_second = frames_per_second;
        self
    }

    #[must_use]
    pub const fn overlay(mut self, overlay: Overlay) -> Self {
        self.overlay = overlay;
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
    scratch: Vec<u8>,
    clock: Option<FilmClock>,
    capture_cost: Duration,
    pointer: Option<Point>,
    target: Option<Rect>,
    tempo: StoryTempo,
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
            scratch: Vec::new(),
            clock: None,
            capture_cost: Duration::ZERO,
            pointer: None,
            target: None,
            tempo: StoryTempo::STANDARD,
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

    /// Seal the take and publish its film. Call this after terminating the
    /// product: a take may span relaunches, and an offline showpiece transcode
    /// must not contend with an idle live app.
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
        let target = anchor.map(|anchor| Rect::from(anchor.rect));
        self.target = target;
        self.live_interval(surface, self.beat(TARGET_FLIGHT), |progress| Scene::Aim {
            pointer: origin.lerp(destination, progress),
            target,
        })?;
        self.pointer = Some(destination);
        self.live_interval(surface, self.beat(TARGET_REST), |_| Scene::Aim {
            pointer: destination,
            target,
        })
    }

    fn strike(&mut self, surface: StorySurface<'_>, pointer: Option<[i16; 2]>) -> Result<()> {
        if let Some(pointer) = pointer {
            self.pointer = Some(Point::from(pointer));
        }
        let (pointer, target) = (self.pointer, self.target);
        self.live_interval(surface, self.beat(ACTION_REST), |progress| Scene::Strike {
            pointer,
            target,
            progress,
        })
    }

    fn rest(&mut self, surface: StorySurface<'_>, duration: Duration) -> Result<()> {
        let pointer = self.pointer;
        self.live_interval(surface, duration, |_| Scene::Rest { pointer })
    }

    /// A chapter rests on the surface for the same span under every overlay;
    /// only the diagnostic overlay paints its title.
    fn chapter(&mut self, surface: StorySurface<'_>, title: &str) -> Result<()> {
        let pointer = self.pointer;
        self.live_interval(surface, CHAPTER_REST, |_| Scene::Chapter { title, pointer })
    }

    fn beat(&self, duration: Duration) -> Duration {
        duration / u32::from(self.tempo.factor())
    }

    /// Hold the story for `duration` of live product time, sampling every film
    /// tick it spans; `scene` paints a tick by its progress through the interval.
    /// Each capture is scheduled one capture cost ahead of its tick.
    fn live_interval<'a>(
        &mut self,
        surface: StorySurface<'_>,
        duration: Duration,
        mut scene: impl FnMut(f64) -> Scene<'a>,
    ) -> Result<()> {
        let fps = self.config.frames_per_second;
        let begun = Instant::now();
        let end = begun + duration;
        loop {
            let tick = self.segment().next_tick(fps);
            if tick > end {
                if let Some(rest) = end.checked_duration_since(Instant::now()) {
                    thread::sleep(rest);
                }
                return Ok(());
            }
            if let Some(rest) = tick
                .checked_sub(self.capture_cost)
                .and_then(|wake| wake.checked_duration_since(Instant::now()))
            {
                thread::sleep(rest);
            }
            self.sample(surface, |at| scene(progress(begun, duration, at)))?;
        }
    }

    /// The current segment's clock, begun now if no story event began it.
    fn segment(&mut self) -> &mut FilmClock {
        let base = self.frames_written;
        self.clock
            .get_or_insert_with(|| FilmClock::start(Instant::now(), base))
    }

    /// Fill the ticks that fell due since the last sample with the story at
    /// rest: the film's share of time the story spent waiting or acting.
    fn catch_up(&mut self, surface: StorySurface<'_>) -> Result<()> {
        let fps = self.config.frames_per_second;
        match self.clock {
            Some(clock) if clock.next_tick(fps) <= Instant::now() => {
                let pointer = self.pointer;
                self.sample(surface, |_| Scene::Rest { pointer })
            }
            _ => Ok(()),
        }
    }

    /// Write every film tick due by now from one fresh capture; the freshest
    /// product frame fills every tick that elapsed since the last.
    fn sample<'a>(
        &mut self,
        surface: StorySurface<'_>,
        mut scene: impl FnMut(Instant) -> Scene<'a>,
    ) -> Result<()> {
        let fps = self.config.frames_per_second;
        let captured = Instant::now();
        let frame = surface.capture()?;
        let paint = self.config.overlay.bind(surface)?;
        self.capture_cost = captured.elapsed();
        let clock = *self.segment();
        let due = clock.due(Instant::now(), fps);
        for index in clock.ticks..due {
            self.write_scene(&frame, &paint, scene(clock.tick(index, fps)))?;
        }
        self.clock = Some(FilmClock {
            ticks: clock.ticks.max(due),
            ..clock
        });
        Ok(())
    }

    fn write_scene(&mut self, frame: &Frame, paint: &Paint, scene: Scene<'_>) -> Result<()> {
        let (width, height) = (frame.width(), frame.height());
        self.scratch.clear();
        self.scratch.extend_from_slice(frame.rgba());
        paint.apply(Canvas::new(&mut self.scratch, width, height), scene);
        let encoder = match &mut self.encoder {
            Some(encoder) => encoder,
            vacant @ None => vacant.insert(Encoder::ignite(
                &self.config.output,
                width,
                height,
                self.config.frames_per_second,
                self.config.encoding_profile,
            )?),
        };
        encoder.write(width, height, &self.scratch)?;
        self.frames_written += 1;
        Ok(())
    }

    /// Record `event` as it stood at `at`: its film frame is the first tick at
    /// or after that instant, wherever encoding has reached.
    fn trace(&mut self, event: StoryEvent<'_>, at: Instant) -> Result<()> {
        let fps = self.config.frames_per_second;
        let elapsed_ns =
            u64::try_from(at.saturating_duration_since(self.begun).as_nanos()).unwrap_or(u64::MAX);
        let film_frame = self
            .clock
            .map_or(self.frames_written, |clock| clock.frame_at(at, fps));
        let record = TraceRecord {
            schema: TRACE_SCHEMA,
            elapsed_ns,
            film_frame,
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
        let at = Instant::now();
        match self.clock {
            None => self.clock = Some(FilmClock::start(at, self.frames_written)),
            Some(_) => self.catch_up(surface)?,
        }
        self.trace(event, at)?;
        match event {
            StoryEvent::Cue(StoryCue::Chapter { title }) => self.chapter(surface, title),
            StoryEvent::Cue(StoryCue::Hold { duration }) => self.rest(surface, duration),
            StoryEvent::Cue(StoryCue::Tempo { tempo }) => {
                self.tempo = tempo;
                Ok(())
            }
            StoryEvent::Cue(StoryCue::Still { .. })
            | StoryEvent::Fact(StoryFact::TargetResolved { .. }) => Ok(()),
            StoryEvent::Fact(StoryFact::PointerAimed {
                pointer, anchor, ..
            }) => self.aim(surface, pointer, anchor),
            StoryEvent::Fact(StoryFact::ActionDispatched { pointer, .. }) => {
                self.strike(surface, pointer)
            }
            StoryEvent::Fact(StoryFact::ObservationMatched { .. }) => {
                self.rest(surface, self.beat(OBSERVATION_REST))
            }
        }
    }

    /// Sample whatever ticks fell due while the story waits on the product.
    fn awaiting(&mut self, surface: StorySurface<'_>) -> Result<()> {
        self.catch_up(surface)
    }

    /// Close one launch's segment. The take stays open for a relaunch until
    /// [`Recorder::publish`] seals it.
    fn finish(&mut self, surface: StorySurface<'_>) -> Result<()> {
        self.catch_up(surface)?;
        self.clock = None;
        self.trace
            .flush()
            .map_err(|source| io("flush story event trace", &self.trace_path, source))
    }
}

/// One launch segment's film clock: tick `k` stands for `epoch + k / fps`.
///
/// A segment begins at its story's first event and ends at `finish`; every
/// instant between belongs to the film, whether the story acts, rests, or
/// waits on the product. Between segments no product surface exists, so the
/// film cuts there and nowhere else.
#[derive(Clone, Copy)]
struct FilmClock {
    epoch: Instant,
    /// Film frames earlier segments wrote: tick `k` is frame `base + k`.
    base: u64,
    ticks: u64,
}

impl FilmClock {
    const fn start(epoch: Instant, base: u64) -> Self {
        Self {
            epoch,
            base,
            ticks: 0,
        }
    }

    /// The film frame of the first tick at or after `at`.
    fn frame_at(self, at: Instant, fps: NonZeroU32) -> u64 {
        let elapsed = at.saturating_duration_since(self.epoch).as_nanos();
        let ticks = (elapsed * u128::from(fps.get())).div_ceil(1_000_000_000);
        self.base + u64::try_from(ticks).unwrap_or(u64::MAX)
    }

    fn tick(self, index: u64, fps: NonZeroU32) -> Instant {
        let nanos = u128::from(index) * 1_000_000_000 / u128::from(fps.get());
        self.epoch + Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    fn next_tick(self, fps: NonZeroU32) -> Instant {
        self.tick(self.ticks, fps)
    }

    /// How many ticks stand at or before `now`.
    fn due(self, now: Instant, fps: NonZeroU32) -> u64 {
        let elapsed = now.saturating_duration_since(self.epoch).as_nanos();
        u64::try_from(elapsed * u128::from(fps.get()) / 1_000_000_000)
            .unwrap_or(u64::MAX)
            .saturating_add(1)
    }
}

/// How far `at` lies through `begun..begun + duration`, within `[0, 1]`.
fn progress(begun: Instant, duration: Duration, at: Instant) -> f64 {
    if duration.is_zero() {
        return 1.0;
    }
    (at.saturating_duration_since(begun).as_secs_f64() / duration.as_secs_f64()).min(1.0)
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

/// Story observer developing every still a score names into a lossless PNG.
///
/// A still is the product's own presented surface at the instant of its cue;
/// nothing is painted over it. One darkroom may span every launch of a take.
pub struct Darkroom {
    directory: PathBuf,
    developed: Vec<PathBuf>,
}

impl Darkroom {
    /// Develop each still as `<directory>/<name>.png`, replacing an earlier take's.
    pub fn forge(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)
            .map_err(|source| io("create still directory", &directory, source))?;
        Ok(Self {
            directory,
            developed: Vec::new(),
        })
    }

    /// Every developed still in score order, refusing a take that named none.
    pub fn publish(self) -> Result<Vec<PathBuf>> {
        if self.developed.is_empty() {
            return Err(Error::Score {
                detail: format!("take named no stills for {}", self.directory.display()),
            });
        }
        Ok(self.developed)
    }
}

impl StoryObserver for Darkroom {
    fn observe(&mut self, event: StoryEvent<'_>, surface: StorySurface<'_>) -> Result<()> {
        let StoryEvent::Cue(StoryCue::Still { name }) = event else {
            return Ok(());
        };
        let still = self.directory.join(format!("{name}.png"));
        if self.developed.contains(&still) {
            return Err(Error::Score {
                detail: format!("take named the still `{name}` twice"),
            });
        }
        surface.capture()?.save_png(&still)?;
        self.developed.push(still);
        Ok(())
    }
}

/// Observer for one showcase take: every still its score names and, when
/// asked, the take's showpiece film.
pub struct Showcase {
    recorder: Option<Recorder>,
    darkroom: Darkroom,
}

impl Showcase {
    /// Develop the take's stills into `directory`.
    pub fn stills(directory: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            recorder: None,
            darkroom: Darkroom::forge(directory)?,
        })
    }

    /// Also record the take as the showpiece `<directory>/<film>.mp4`, bearing
    /// only the display server's own pointer over the product.
    pub fn film(directory: impl Into<PathBuf>, film: &str) -> Result<Self> {
        let darkroom = Darkroom::forge(directory)?;
        let recorder = Recorder::forge(
            RecorderConfig::new(darkroom.directory.join(format!("{film}.mp4")))
                .overlay(Overlay::Presentation)
                .encoding_profile(EncodingProfile::Showpiece),
        )?;
        Ok(Self {
            recorder: Some(recorder),
            darkroom,
        })
    }

    /// Publish every artifact of the take, film first, after the product
    /// terminates. A filmed take may name no stills; a stills-only take that
    /// named none published nothing and is refused.
    pub fn publish(self) -> Result<Vec<PathBuf>> {
        let Some(recorder) = self.recorder else {
            return self.darkroom.publish();
        };
        let recorder = recorder.publish()?;
        let mut artifacts = vec![
            recorder.output().to_owned(),
            recorder.trace_path().to_owned(),
        ];
        artifacts.extend(self.darkroom.developed);
        Ok(artifacts)
    }
}

impl StoryObserver for Showcase {
    fn observe(&mut self, event: StoryEvent<'_>, surface: StorySurface<'_>) -> Result<()> {
        if let Some(recorder) = &mut self.recorder {
            recorder.observe(event, surface)?;
        }
        self.darkroom.observe(event, surface)
    }

    fn awaiting(&mut self, surface: StorySurface<'_>) -> Result<()> {
        match &mut self.recorder {
            Some(recorder) => recorder.awaiting(surface),
            None => Ok(()),
        }
    }

    fn finish(&mut self, surface: StorySurface<'_>) -> Result<()> {
        match &mut self.recorder {
            Some(recorder) => recorder.finish(surface),
            None => Ok(()),
        }
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

    #[test]
    fn film_clock_counts_every_tick_at_or_before_now() {
        let fps = NonZeroU32::new(60).expect("film rate");
        let clock = FilmClock::start(Instant::now(), 10);
        let at = |millis| clock.epoch + Duration::from_millis(millis);
        assert_eq!(clock.due(clock.epoch, fps), 1);
        assert_eq!(clock.due(at(16), fps), 1);
        assert_eq!(clock.due(at(17), fps), 2);
        assert_eq!(clock.due(at(8_400), fps), 505);
        assert_eq!(clock.tick(504, fps), at(8_400));
        assert_eq!(clock.frame_at(clock.epoch, fps), 10);
        assert_eq!(clock.frame_at(at(1), fps), 11);
        assert_eq!(clock.frame_at(at(8_400), fps), 514);
    }
}
