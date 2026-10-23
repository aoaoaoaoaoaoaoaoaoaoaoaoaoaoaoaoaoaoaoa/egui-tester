//! Optional film, still, and trace observers for [`egui_tester::Story`].
//!
//! Ordinary acceptance stories remain the source of truth. Attaching a
//! [`Recorder`] turns the same causal event stream into an H.264 film and a
//! JSONL execution trace, painted with a diagnostic or presentation
//! [`Overlay`]; a [`Darkroom`] develops every still the score names, and a
//! [`Showcase`] joins both in one take. None grants any new product authority.
//! Every artifact develops in a staging directory beside its destination and
//! reaches its final path only when published, so an abandoned take leaves
//! nothing that passes for a finished one.

mod overlay;

use std::{
    env,
    fs::{self, File},
    io::{BufWriter, ErrorKind, Write as _},
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use egui_tester::{
    Approach, ArtifactStem, Error, Frame, Framing, Result, StoryCue, StoryEvent, StoryFact,
    StoryObserver, StorySurface, StoryTempo,
};
use serde::Serialize;
use tempfile::TempDir;

pub use overlay::Overlay;
use overlay::{Canvas, Paint, Point, Rect, Scene};

const TRACE_SCHEMA: &str = "egui-demo.trace/4";
const TARGET_FLIGHT: Duration = Duration::from_millis(220);
const TARGET_REST: Duration = Duration::from_millis(100);
const ACTION_REST: Duration = Duration::from_millis(160);
const OBSERVATION_REST: Duration = Duration::from_millis(220);
const CHAPTER_REST: Duration = Duration::from_millis(1_500);
/// Name prefix of every staging directory, and the lock file inside each.
const STAGING_PREFIX: &str = ".egui-demo-";
const STAGING_LOCK: &str = ".lock";

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
    /// Declared before the staging directory, so an abandoned take reaps its
    /// encoder before the directory and its partial artifacts vanish.
    encoder: Option<Encoder>,
    trace: BufWriter<File>,
    /// The staged trace, for diagnostics.
    trace_path: PathBuf,
    staging: Staging,
    scratch: Vec<u8>,
    clock: Option<FilmClock>,
    capture_cost: Duration,
    /// The previous capture: every tick due before the next capture began
    /// shows it, never a later one.
    last: Option<(Frame, Paint)>,
    /// The pointer the story last declared, for a projection presenting none.
    pointer: Option<Point>,
    /// Whether an aim awaits its action, and the target it outlines.
    aiming: bool,
    target: Option<Rect>,
    tempo: StoryTempo,
    begun: Instant,
    frames_written: u64,
}

impl Recorder {
    /// Stage a film for `config`'s output and its trace beside it; neither
    /// reaches its final path before [`Self::publish`].
    pub fn forge(config: RecorderConfig) -> Result<Self> {
        if config
            .output
            .extension()
            .is_none_or(|extension| extension != "mp4")
        {
            return Err(film_fault(format!(
                "egui-demo output must be an .mp4 file: {}",
                config.output.display()
            )));
        }
        let staging = stage(parent(&config.output))?;
        let trace_path = staged(&staging, &trace_path(&config.output));
        let trace = File::create(&trace_path)
            .map(BufWriter::new)
            .map_err(|source| io("create story event trace", &trace_path, source))?;
        Ok(Self {
            config,
            encoder: None,
            trace,
            trace_path,
            staging,
            scratch: Vec::new(),
            clock: None,
            capture_cost: Duration::ZERO,
            last: None,
            pointer: None,
            aiming: false,
            target: None,
            tempo: StoryTempo::STANDARD,
            begun: Instant::now(),
            frames_written: 0,
        })
    }

    /// Seal the take and publish its film, then its trace, returning both
    /// paths. Call this after terminating the product: a take may span
    /// relaunches, and an offline showpiece transcode must not contend with an
    /// idle live app.
    pub fn publish(mut self) -> Result<Vec<PathBuf>> {
        self.trace
            .flush()
            .map_err(|source| io("flush story event trace", &self.trace_path, source))?;
        self.encoder
            .take()
            .ok_or_else(|| film_fault("story emitted no recordable surface frames"))?
            .publish()?;
        let film = self.config.output.clone();
        let trace = trace_path(&film);
        promote(&staged(&self.staging, &film), &film)?;
        promote(&self.trace_path, &trace)?;
        Ok(vec![film, trace])
    }

    fn strike(&mut self, surface: StorySurface<'_>, pointer: Option<[i16; 2]>) -> Result<()> {
        if let Some(pointer) = pointer {
            self.pointer = Some(Point::from(pointer));
        }
        self.aiming = false;
        let target = self.target.take();
        self.live_interval(surface, self.beat(ACTION_REST), |progress| Scene::Strike {
            target,
            progress,
        })
    }

    fn rest(&mut self, surface: StorySurface<'_>, duration: Duration) -> Result<()> {
        let scene = self.idle();
        self.live_interval(surface, duration, |_| scene)
    }

    /// A chapter rests on the surface for the same span under every overlay;
    /// only the diagnostic overlay paints its title.
    fn chapter(&mut self, surface: StorySurface<'_>, title: &str) -> Result<()> {
        self.live_interval(surface, CHAPTER_REST, |_| Scene::Chapter { title })
    }

    /// The scene of a story between acts: closing on an aim, or at rest.
    const fn idle(&self) -> Scene<'static> {
        if self.aiming {
            Scene::Aim {
                target: self.target,
            }
        } else {
            Scene::Rest
        }
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

    /// Fill the ticks that fell due since the last sample with the story
    /// between acts: the film's share of time the story spent waiting or
    /// pacing its input.
    fn catch_up(&mut self, surface: StorySurface<'_>) -> Result<()> {
        let fps = self.config.frames_per_second;
        match self.clock {
            Some(clock) if clock.next_tick(fps) <= Instant::now() => {
                let scene = self.idle();
                self.sample(surface, |_| scene)
            }
            _ => Ok(()),
        }
    }

    /// Write every film tick due by now from one fresh capture. Ticks due
    /// before the capture began show the previous capture, never a later one;
    /// a segment's first capture alone fills the ticks before it, since no
    /// earlier surface exists.
    fn sample<'a>(
        &mut self,
        surface: StorySurface<'_>,
        mut scene: impl FnMut(Instant) -> Scene<'a>,
    ) -> Result<()> {
        let fps = self.config.frames_per_second;
        let captured = Instant::now();
        let fresh = (
            surface.capture()?,
            self.config.overlay.bind(surface, self.pointer)?,
        );
        self.capture_cost = captured.elapsed();
        let clock = *self.segment();
        let due = clock.due(Instant::now(), fps);
        let last = self.last.take();
        for index in clock.ticks..due {
            let at = clock.tick(index, fps);
            let (frame, paint) = match &last {
                Some(prior) if at < captured => prior,
                _ => &fresh,
            };
            self.write_scene(frame, paint, scene(at))?;
        }
        self.last = Some(fresh);
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
                &staged(&self.staging, &self.config.output),
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
        serde_json::to_writer(&mut self.trace, &record).map_err(|source| {
            film_fault(format!(
                "serialize story event trace {}: {source}",
                self.trace_path.display()
            ))
        })?;
        self.trace
            .write_all(b"\n")
            .map_err(|source| io("append story event trace", &self.trace_path, source))
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
            }) => {
                self.pointer = Some(Point::from(pointer));
                self.aiming = true;
                self.target = anchor.map(|anchor| Rect::from(anchor.rect));
                Ok(())
            }
            StoryEvent::Fact(StoryFact::ActionDispatched { pointer, .. }) => {
                self.strike(surface, pointer)
            }
            StoryEvent::Fact(StoryFact::ObservationMatched { .. }) => {
                self.rest(surface, self.beat(OBSERVATION_REST))
            }
        }
    }

    /// Sample whatever ticks fell due while the story waits on the product
    /// or paces its input.
    fn awaiting(&mut self, surface: StorySurface<'_>) -> Result<()> {
        self.catch_up(surface)
    }

    /// Close one launch's segment. The take stays open for a relaunch until
    /// [`Recorder::publish`] seals it.
    fn finish(&mut self, surface: StorySurface<'_>) -> Result<()> {
        self.catch_up(surface)?;
        self.clock = None;
        self.last = None;
        self.trace
            .flush()
            .map_err(|source| io("flush story event trace", &self.trace_path, source))
    }
}

impl Drop for Recorder {
    /// A proof film dropped unpublished is a failed take's evidence: finalize
    /// it with its trace beside its destination, under an `-abandoned` stem.
    /// A showpiece dropped unpublished is discarded with its staging.
    fn drop(&mut self) {
        let Some(encoder) = self.encoder.take() else {
            return;
        };
        if self.config.encoding_profile != EncodingProfile::Proof {
            return;
        }
        let _flushed = self.trace.flush();
        if encoder.publish().is_ok() {
            let film = abandoned(&self.config.output);
            let _film = promote(&staged(&self.staging, &self.config.output), &film);
            let _trace = promote(&self.trace_path, &trace_path(&film));
        }
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

/// Story observer developing every still a score names into a lossless PNG.
///
/// A still is the product's own presented surface at the instant of its cue,
/// once the story has let the presentation rest; nothing is painted over it.
/// One darkroom may span every launch of a take.
pub struct Darkroom {
    directory: PathBuf,
    staging: Staging,
    developed: Vec<ArtifactStem>,
}

impl Darkroom {
    /// Develop each still, to be published as `<directory>/<name>.png` over an
    /// earlier take's.
    pub fn forge(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        let staging = stage(&directory)?;
        Ok(Self {
            directory,
            staging,
            developed: Vec::new(),
        })
    }

    /// Publish every developed still in score order, refusing a take that
    /// named none.
    pub fn publish(self) -> Result<Vec<PathBuf>> {
        if self.developed.is_empty() {
            return Err(Error::Score {
                detail: format!("take named no stills for {}", self.directory.display()),
            });
        }
        self.developed
            .iter()
            .map(|name| {
                let still = self.directory.join(format!("{name}.png"));
                promote(&staged(&self.staging, &still), &still)?;
                Ok(still)
            })
            .collect()
    }
}

impl StoryObserver for Darkroom {
    fn observe(&mut self, event: StoryEvent<'_>, surface: StorySurface<'_>) -> Result<()> {
        let StoryEvent::Cue(StoryCue::Still { name }) = event else {
            return Ok(());
        };
        if self.developed.contains(name) {
            return Err(Error::Score {
                detail: format!("take named the still `{name}` twice"),
            });
        }
        surface
            .capture()?
            .save_png(self.staging.path().join(format!("{name}.png")))?;
        self.developed.push(name.clone());
        Ok(())
    }

    fn develops_stills(&self) -> bool {
        true
    }
}

/// Observer for one showcase take: every still its score names, led by its
/// [`ArtifactStem::FRONTISPIECE`], and, when asked, the take's showpiece film,
/// all in one framing, by default [`Framing::PORTFOLIO`].
///
/// A showcase is a presentation, not an acceptance execution: it requests the
/// framing and, when filmed, an approach that glides the real pointer.
pub struct Showcase {
    recorder: Option<Recorder>,
    darkroom: Darkroom,
    framing: Framing,
}

impl Showcase {
    /// Develop the take's stills into `directory`.
    pub fn stills(directory: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            recorder: None,
            darkroom: Darkroom::forge(directory)?,
            framing: Framing::PORTFOLIO,
        })
    }

    /// Also record the take as the showpiece `<directory>/<film>.mp4`, bearing
    /// only the display server's own pointer over the product.
    pub fn film(directory: impl Into<PathBuf>, film: &ArtifactStem) -> Result<Self> {
        let darkroom = Darkroom::forge(directory)?;
        let recorder = Recorder::forge(
            RecorderConfig::new(darkroom.directory.join(format!("{film}.mp4")))
                .overlay(Overlay::Presentation)
                .encoding_profile(EncodingProfile::Showpiece),
        )?;
        Ok(Self {
            recorder: Some(recorder),
            darkroom,
            framing: Framing::PORTFOLIO,
        })
    }

    /// Present the take in `framing` instead.
    #[must_use]
    pub fn framed(mut self, framing: Framing) -> Self {
        self.framing = framing;
        self
    }

    /// Publish the take after the product terminates: its film and trace
    /// first, then every still in score order. A take whose score named no
    /// frontispiece publishes nothing.
    pub fn publish(self) -> Result<Vec<PathBuf>> {
        if !self
            .darkroom
            .developed
            .contains(&ArtifactStem::FRONTISPIECE)
        {
            return Err(Error::Score {
                detail: format!(
                    "showcase take named no `{}` still",
                    ArtifactStem::FRONTISPIECE
                ),
            });
        }
        let mut artifacts = match self.recorder {
            Some(recorder) => recorder.publish()?,
            None => Vec::new(),
        };
        artifacts.extend(self.darkroom.publish()?);
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

    fn framing(&self) -> Option<Framing> {
        Some(self.framing)
    }

    /// A filmed showcase glides the real pointer to each destination for one
    /// beat and dwells there for another, so its film shows the approach a
    /// viewer would; a stills-only showcase jumps.
    fn approach(&self) -> Approach {
        self.recorder
            .as_ref()
            .map_or(Approach::INSTANT, |recorder| Approach {
                glide: recorder.beat(TARGET_FLIGHT),
                dwell: recorder.beat(TARGET_REST),
            })
    }

    fn develops_stills(&self) -> bool {
        true
    }
}

/// The cache directory beneath which a product's showcase keeps its specimen,
/// apart from everything an acceptance rail reads: `<product>-showcase` in the
/// XDG cache home, an absolute `XDG_CACHE_HOME` or else `$HOME/.cache`. It is
/// resolved, never created.
pub fn specimen_cache(product: &ArtifactStem) -> Result<PathBuf> {
    let absolute = |name| {
        env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let home = absolute("XDG_CACHE_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".cache")))
        .ok_or_else(|| Error::Unsupported {
            capability: "XDG cache home",
            detail: "neither XDG_CACHE_HOME nor HOME is an absolute path".to_owned(),
        })?;
    Ok(home.join(format!("{product}-showcase")))
}

#[derive(Serialize)]
struct TraceRecord<'a> {
    schema: &'static str,
    elapsed_ns: u64,
    film_frame: u64,
    event: StoryEvent<'a>,
}

/// One ffmpeg encoder writing a staged film, through a staged lossless
/// capture for a showpiece.
struct Encoder {
    output: PathBuf,
    capture: PathBuf,
    encoding_profile: EncodingProfile,
    /// The running encoder, until publication reaps it.
    child: Option<Child>,
    input: Option<ChildStdin>,
    width: u32,
    height: u32,
    frame_bytes: usize,
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
            "-n",
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
        let input = child
            .stdin
            .take()
            .ok_or_else(|| film_fault("ffmpeg did not expose its raw-video input"))?;
        let frame_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                film_fault(format!(
                    "egui-demo frame geometry {width}x{height} exceeds platform limits"
                ))
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
        })
    }

    fn write(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        if (width, height) != (self.width, self.height) {
            return Err(film_fault(format!(
                "egui-demo frame geometry changed from {}x{} to {width}x{height}",
                self.width, self.height
            )));
        }
        if rgba.len() != self.frame_bytes {
            return Err(film_fault(format!(
                "egui-demo frame has {} RGBA bytes, expected {}",
                rgba.len(),
                self.frame_bytes
            )));
        }
        self.input
            .as_mut()
            .ok_or_else(|| film_fault("egui-demo film was written after encoder closure"))?
            .write_all(rgba)
            .map_err(|source| io("write egui-demo frame", &self.output, source))
    }

    /// Seal the capture and, for a showpiece, transcode the staged film from it.
    fn publish(mut self) -> Result<()> {
        drop(self.input.take());
        let child = self
            .child
            .take()
            .ok_or_else(|| film_fault("egui-demo encoder process vanished before closure"))?;
        let output = child
            .wait_with_output()
            .map_err(|source| io("wait for egui-demo encoder", &self.output, source))?;
        demand_command(output, format!("ffmpeg -> {}", self.capture.display()))?;
        if self.encoding_profile.stages_lossless_capture() {
            self.transcode_showpiece()?;
        }
        Ok(())
    }

    fn transcode_showpiece(&self) -> Result<()> {
        let (preset, crf, tune) = self.encoding_profile.x264();
        let mut command = Command::new("ffmpeg");
        let _command = command.args(["-hide_banner", "-loglevel", "error", "-n", "-i"]);
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
    /// Kill an encoder abandoned mid-take; its staging directory discards
    /// whatever it wrote.
    fn drop(&mut self) {
        drop(self.input.take());
        if let Some(mut child) = self.child.take() {
            let _killed = child.kill();
            let _status = child.wait();
        }
    }
}

/// The trace published beside `film`.
fn trace_path(film: &Path) -> PathBuf {
    film.with_extension("events.jsonl")
}

/// The directory holding `path`, the working directory for a bare name.
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// `<stem>-abandoned.mp4` beside `film`.
fn abandoned(film: &Path) -> PathBuf {
    let stem = film.file_stem().unwrap_or_default().to_string_lossy();
    film.with_file_name(format!("{stem}-abandoned.mp4"))
}

/// A take's staging directory beside its artifacts' destination, locked for
/// as long as the take lives, so a later take can tell it from a dead one's.
struct Staging {
    directory: TempDir,
    _lock: File,
}

impl Staging {
    fn path(&self) -> &Path {
        self.directory.path()
    }
}

/// Create `directory` and a staging directory inside it, on the filesystem
/// every artifact published there will occupy, first sweeping away the
/// staging a killed take left there.
fn stage(directory: &Path) -> Result<Staging> {
    fs::create_dir_all(directory)
        .map_err(|source| io("create artifact directory", directory, source))?;
    sweep(directory);
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(directory)
        .map_err(|source| io("create artifact staging directory", directory, source))?;
    let lock_path = staging.path().join(STAGING_LOCK);
    let lock = File::create(&lock_path)
        .map_err(|source| io("create artifact staging lock", &lock_path, source))?;
    lock.try_lock()
        .map_err(|error| io("lock artifact staging", &lock_path, error.into()))?;
    Ok(Staging {
        directory: staging,
        _lock: lock,
    })
}

/// Remove, best effort, every staging directory in `directory` whose lock no
/// live take holds. One still being created has no lock yet and stays.
fn sweep(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
        {
            continue;
        }
        let staging = entry.path();
        if File::open(staging.join(STAGING_LOCK)).is_ok_and(|lock| lock.try_lock().is_ok()) {
            let _swept = fs::remove_dir_all(&staging);
        }
    }
}

/// Where `path` develops inside `staging`.
fn staged(staging: &Staging, path: &Path) -> PathBuf {
    staging.path().join(path.file_name().unwrap_or_default())
}

/// Move a staged artifact onto its final path, replacing an earlier take's.
fn promote(staged: &Path, path: &Path) -> Result<()> {
    fs::rename(staged, path).map_err(|source| io("publish artifact", path, source))
}

fn film_fault(detail: impl Into<String>) -> Error {
    Error::Film {
        detail: detail.into(),
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
