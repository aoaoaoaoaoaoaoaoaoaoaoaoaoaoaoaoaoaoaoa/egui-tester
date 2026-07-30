//! One-way, versioned semantic telemetry for black-box GUI tests.
//!
//! Applications publish immutable observations after presenting a frame. The
//! harness may use them to locate controls and synchronize, never to mutate
//! product state or substitute for an external oracle.

use std::{
    collections::BTreeSet,
    env, fs,
    fs::{File, OpenOptions},
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

use rustix::time::{ClockId, clock_gettime};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const SCHEMA: u32 = 3;
pub const PATH_ENV: &str = "EGUI_TESTER_WITNESS";
pub const LAUNCH_ENV: &str = "EGUI_TESTER_LAUNCH";
pub const FRAMES_ENV: &str = "EGUI_TESTER_FRAMES";

const FRAME_MAGIC: &[u8; 8] = b"EGUIFRM\0";
const FRAME_SCHEMA: u32 = 2;
const FRAME_RECORD_BYTES: usize = 5 * size_of::<u64>();
const OBSERVATION_MAGIC: &[u8; 8] = b"EGUIOBS\0";
const OBSERVATION_SCHEMA: u32 = 1;
const MAX_OBSERVATION_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("witness environment is incomplete: {0}")]
    Environment(&'static str),
    #[error("invalid witness anchor `{name}`: {detail}")]
    Anchor { name: String, detail: &'static str },
    #[error("serialize witness frame: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("{operation} witness `{path}`: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid frame journal `{path}`: {detail}")]
    FrameJournal { path: PathBuf, detail: String },
    #[error("invalid observation journal `{path}`: {detail}")]
    ObservationJournal { path: PathBuf, detail: String },
    #[error("witness writer failed: {0}")]
    Writer(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Named hit-test rectangle in physical, window-relative pixels.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Anchor {
    pub name: String,
    pub rect: [f32; 4],
}

impl Anchor {
    pub fn physical(name: impl Into<String>, rect: [f32; 4]) -> Result<Self> {
        let anchor = Self {
            name: name.into(),
            rect,
        };
        anchor.validate()?;
        Ok(anchor)
    }

    pub fn logical(name: impl Into<String>, rect: [f32; 4], pixels_per_point: f32) -> Result<Self> {
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return Err(Error::Anchor {
                name: name.into(),
                detail: "pixels per point must be positive and finite",
            });
        }
        Self::physical(name, rect.map(|coordinate| coordinate * pixels_per_point))
    }

    pub fn validate(&self) -> Result<()> {
        let [x0, y0, x1, y1] = self.rect;
        if self.name.is_empty() {
            return Err(Error::Anchor {
                name: self.name.clone(),
                detail: "name is empty",
            });
        }
        if !self.rect.into_iter().all(f32::is_finite) {
            return Err(Error::Anchor {
                name: self.name.clone(),
                detail: "rectangle is not finite",
            });
        }
        if x0 > x1 || y0 > y1 {
            return Err(Error::Anchor {
                name: self.name.clone(),
                detail: "rectangle is inverted",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn center(&self) -> (i16, i16) {
        let [x0, y0, x1, y1] = self.rect;
        (
            f32::midpoint(x0, x1).round() as i16,
            f32::midpoint(y0, y1).round() as i16,
        )
    }
}

/// Product observation captured before witness serialization.
pub struct PendingFrame<T> {
    frame: u64,
    begun_ns: u64,
    observed_ns: u64,
    pixels_per_point: f32,
    anchors: Vec<Anchor>,
    state: T,
}

impl<T> PendingFrame<T> {
    pub fn forge(
        frame: u64,
        pixels_per_point: f32,
        anchors: impl IntoIterator<Item = Anchor>,
        state: T,
    ) -> Result<Self> {
        let now = ProductInstant::now();
        Self::forge_at(
            FrameObservation::from_instants(now, now)?,
            frame,
            pixels_per_point,
            anchors,
            state,
        )
    }

    /// Forge telemetry against timestamps captured around product-state work.
    pub fn forge_at(
        observation: FrameObservation,
        frame: u64,
        pixels_per_point: f32,
        anchors: impl IntoIterator<Item = Anchor>,
        state: T,
    ) -> Result<Self> {
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return Err(Error::Anchor {
                name: "<frame>".to_owned(),
                detail: "pixels per point must be positive and finite",
            });
        }
        let anchors = anchors.into_iter().collect::<Vec<_>>();
        let mut names = BTreeSet::new();
        for anchor in &anchors {
            anchor.validate()?;
            if !names.insert(&anchor.name) {
                return Err(Error::Anchor {
                    name: anchor.name.clone(),
                    detail: "name is duplicated in one frame",
                });
            }
        }
        Ok(Self {
            frame,
            begun_ns: observation.begun_ns,
            observed_ns: observation.observed_ns,
            pixels_per_point,
            anchors,
            state,
        })
    }

    #[must_use]
    pub const fn observed_ns(&self) -> u64 {
        self.observed_ns
    }
}

/// Asynchronous witness sink armed by the standard harness environment.
///
/// The event loop enqueues owned observations after `wgpu` returns from
/// `Surface::present`. A private worker serializes and appends them in order,
/// keeping test-only filesystem work outside the measured product frame.
pub struct Publisher<T> {
    path: PathBuf,
    frame_path: PathBuf,
    launch: String,
    surface_sequence: u64,
    sender: Sender<WriterMessage<T>>,
    fault: Arc<Mutex<Option<String>>>,
    worker: Option<JoinHandle<()>>,
}

impl<T: Serialize + Send + 'static> Publisher<T> {
    pub fn from_env() -> Result<Option<Self>> {
        match (
            env::var_os(PATH_ENV),
            env::var_os(LAUNCH_ENV),
            env::var_os(FRAMES_ENV),
        ) {
            (None, None, None) => Ok(None),
            (Some(path), Some(launch), Some(frame_path)) => {
                let launch = launch
                    .into_string()
                    .map_err(|_| Error::Environment("EGUI_TESTER_LAUNCH is not valid Unicode"))?;
                if launch.is_empty() {
                    return Err(Error::Environment("EGUI_TESTER_LAUNCH is empty"));
                }
                Self::raise(path.into(), frame_path.into(), launch).map(Some)
            }
            (None, _, _) => Err(Error::Environment(
                "EGUI_TESTER_LAUNCH and EGUI_TESTER_FRAMES require EGUI_TESTER_WITNESS",
            )),
            (_, None, _) => Err(Error::Environment(
                "EGUI_TESTER_WITNESS and EGUI_TESTER_FRAMES require EGUI_TESTER_LAUNCH",
            )),
            (_, _, None) => Err(Error::Environment(
                "EGUI_TESTER_WITNESS and EGUI_TESTER_LAUNCH require EGUI_TESTER_FRAMES",
            )),
        }
    }

    fn raise(path: PathBuf, frame_path: PathBuf, launch: String) -> Result<Self> {
        let frames = open_frame_journal(&frame_path, &launch)?;
        let observations = open_observation_journal(&path, &launch)?;
        let (sender, receiver) = mpsc::channel();
        let fault = Arc::new(Mutex::new(None));
        let worker_fault = Arc::clone(&fault);
        let worker_path = path.clone();
        let worker_frame_path = frame_path.clone();
        let worker_launch = launch.clone();
        let worker = thread::Builder::new()
            .name("egui-tester-witness".to_owned())
            .spawn(move || {
                writer_loop(
                    receiver,
                    Writer {
                        path: worker_path,
                        frame_path: worker_frame_path,
                        launch: worker_launch,
                        frames,
                        observations,
                    },
                    &worker_fault,
                );
            })
            .map_err(|source| Error::Io {
                operation: "spawn writer for",
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            path,
            frame_path,
            launch,
            surface_sequence: 0,
            sender,
            fault,
            worker: Some(worker),
        })
    }

    /// Enqueue one observation after its product frame reaches surface present.
    pub fn surface_present(&mut self, pending: PendingFrame<T>) -> Result<u64> {
        self.surface_present_at(pending, ProductInstant::now())
    }

    /// Enqueue telemetry against a timestamp captured after surface present.
    pub fn surface_present_at(
        &mut self,
        pending: PendingFrame<T>,
        surface_presented: ProductInstant,
    ) -> Result<u64> {
        self.check_fault()?;
        self.surface_sequence = self.surface_sequence.saturating_add(1);
        if surface_presented.0 < pending.observed_ns {
            return Err(Error::FrameJournal {
                path: self.frame_path.clone(),
                detail: format!(
                    "surface present {} predates observation {}",
                    surface_presented.0, pending.observed_ns
                ),
            });
        }
        self.sender
            .send(WriterMessage::Frame(PresentedFrame {
                pending,
                surface_presented_ns: surface_presented.0,
                surface_sequence: self.surface_sequence,
            }))
            .map_err(|_| Error::Writer("writer thread retired unexpectedly".to_owned()))?;
        Ok(self.surface_sequence)
    }

    /// Drain every queued record and surface any asynchronous writer fault.
    pub fn flush(&self) -> Result<()> {
        self.check_fault()?;
        let (reply, answer) = mpsc::channel();
        self.sender
            .send(WriterMessage::Flush(reply))
            .map_err(|_| Error::Writer("writer thread retired unexpectedly".to_owned()))?;
        answer
            .recv()
            .map_err(|_| Error::Writer("writer thread retired before flush".to_owned()))?
            .map_err(Error::Writer)?;
        self.check_fault()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn launch(&self) -> &str {
        &self.launch
    }

    #[must_use]
    pub fn frame_path(&self) -> &Path {
        &self.frame_path
    }

    fn check_fault(&self) -> Result<()> {
        let fault = self
            .fault
            .lock()
            .map_err(|_| Error::Writer("writer fault lock was poisoned".to_owned()))?;
        match fault.as_ref() {
            Some(detail) => Err(Error::Writer(detail.clone())),
            None => Ok(()),
        }
    }
}

impl<T> Drop for Publisher<T> {
    fn drop(&mut self) {
        let _ignored = self.sender.send(WriterMessage::Halt);
        if let Some(worker) = self.worker.take() {
            let _ignored = worker.join();
        }
    }
}

struct PresentedFrame<T> {
    pending: PendingFrame<T>,
    surface_presented_ns: u64,
    surface_sequence: u64,
}

enum WriterMessage<T> {
    Frame(PresentedFrame<T>),
    Flush(Sender<std::result::Result<(), String>>),
    Halt,
}

struct Writer {
    path: PathBuf,
    frame_path: PathBuf,
    launch: String,
    frames: File,
    observations: File,
}

fn writer_loop<T: Serialize>(
    receiver: Receiver<WriterMessage<T>>,
    mut writer: Writer,
    fault: &Mutex<Option<String>>,
) {
    while let Ok(message) = receiver.recv() {
        match message {
            WriterMessage::Frame(frame) => {
                if writer_fault(fault).is_none()
                    && let Err(error) = writer.append(frame)
                {
                    lodge_writer_fault(fault, error.to_string());
                }
            }
            WriterMessage::Flush(reply) => {
                let result = writer
                    .flush()
                    .map_err(|error| error.to_string())
                    .and_then(|()| match writer_fault(fault) {
                        Some(detail) => Err(detail),
                        None => Ok(()),
                    });
                let _ignored = reply.send(result);
            }
            WriterMessage::Halt => break,
        }
    }
}

impl Writer {
    fn append<T: Serialize>(&mut self, frame: PresentedFrame<T>) -> Result<()> {
        let pending = frame.pending;
        let wire = WireFrame {
            schema: SCHEMA,
            launch: &self.launch,
            frame: pending.frame,
            begun_ns: pending.begun_ns,
            observed_ns: pending.observed_ns,
            surface_presented_ns: frame.surface_presented_ns,
            surface_sequence: frame.surface_sequence,
            ppp: pending.pixels_per_point,
            anchors: &pending.anchors,
            state: &pending.state,
        };
        let bytes = serde_json::to_vec(&wire)?;
        append_observation(&mut self.observations, &self.path, &bytes)?;
        append_frame(
            &mut self.frames,
            &self.frame_path,
            FrameSample {
                frame: pending.frame,
                surface_sequence: frame.surface_sequence,
                begun_ns: pending.begun_ns,
                observed_ns: pending.observed_ns,
                surface_presented_ns: frame.surface_presented_ns,
            },
        )
    }

    fn flush(&mut self) -> Result<()> {
        self.observations.flush().map_err(|source| Error::Io {
            operation: "flush",
            path: self.path.clone(),
            source,
        })?;
        self.frames.flush().map_err(|source| Error::Io {
            operation: "flush",
            path: self.frame_path.clone(),
            source,
        })
    }
}

fn writer_fault(fault: &Mutex<Option<String>>) -> Option<String> {
    match fault.lock() {
        Ok(fault) => fault.clone(),
        Err(poisoned) => Some(
            poisoned
                .into_inner()
                .clone()
                .unwrap_or_else(|| "writer fault lock was poisoned".to_owned()),
        ),
    }
}

fn lodge_writer_fault(fault: &Mutex<Option<String>>, detail: String) {
    match fault.lock() {
        Ok(mut fault) => {
            if fault.is_none() {
                *fault = Some(detail);
            }
        }
        Err(poisoned) => {
            let mut fault = poisoned.into_inner();
            if fault.is_none() {
                *fault = Some(detail);
            }
        }
    }
}

#[derive(Serialize)]
struct WireFrame<'a, T> {
    schema: u32,
    launch: &'a str,
    frame: u64,
    begun_ns: u64,
    observed_ns: u64,
    surface_presented_ns: u64,
    surface_sequence: u64,
    ppp: f32,
    anchors: &'a [Anchor],
    state: &'a T,
}

/// Incremental reader for the canonical semantic observation journal.
#[derive(Debug)]
pub struct ObservationJournal {
    path: PathBuf,
    launch: String,
    input: Option<File>,
}

impl ObservationJournal {
    #[must_use]
    pub fn sealed(path: impl Into<PathBuf>, launch: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            launch: launch.into(),
            input: None,
        }
    }

    pub fn read_new<T: DeserializeOwned>(&mut self) -> Result<Vec<T>> {
        self.read_new_bytes()?
            .into_iter()
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|error| Error::ObservationJournal {
                    path: self.path.clone(),
                    detail: format!("decode record: {error}"),
                })
            })
            .collect()
    }

    pub fn read_new_bytes(&mut self) -> Result<Vec<Vec<u8>>> {
        if self.input.is_none() {
            self.input = Some(open_observation_reader(&self.path, &self.launch)?);
        }
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| Error::ObservationJournal {
                path: self.path.clone(),
                detail: "reader did not open".to_owned(),
            })?;
        let mut observations = Vec::new();
        while let Some(bytes) = read_observation(input, &self.path)? {
            observations.push(bytes);
        }
        Ok(observations)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// An unforgeable timestamp in the harness's shared monotonic epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductInstant(u64);

impl ProductInstant {
    #[must_use]
    pub fn now() -> Self {
        Self(monotonic_ns())
    }
}

/// One product frame from event-loop entry through completed semantic work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramePulse {
    begun: ProductInstant,
}

impl FramePulse {
    #[must_use]
    pub fn begin() -> Self {
        Self {
            begun: ProductInstant::now(),
        }
    }

    #[must_use]
    pub fn observe(self) -> FrameObservation {
        FrameObservation {
            begun_ns: self.begun.0,
            observed_ns: ProductInstant::now().0,
        }
    }
}

/// Monotonic bounds around product work, excluding post-present telemetry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameObservation {
    begun_ns: u64,
    observed_ns: u64,
}

impl FrameObservation {
    pub fn from_instants(begun: ProductInstant, observed: ProductInstant) -> Result<Self> {
        if observed.0 < begun.0 {
            return Err(Error::FrameJournal {
                path: PathBuf::from("<product-clock>"),
                detail: format!(
                    "observation {} predates frame start {}",
                    observed.0, begun.0
                ),
            });
        }
        Ok(Self {
            begun_ns: begun.0,
            observed_ns: observed.0,
        })
    }
}

/// One lossless frame sample in the standard low-tax journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameSample {
    pub frame: u64,
    pub surface_sequence: u64,
    pub begun_ns: u64,
    pub observed_ns: u64,
    pub surface_presented_ns: u64,
}

/// Read all complete records from a live frame journal.
pub fn read_frame_journal(path: &Path, expected_launch: &str) -> Result<Vec<FrameSample>> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        operation: "read",
        path: path.to_owned(),
        source,
    })?;
    let mut cursor = 0;
    demand_bytes(path, &bytes, cursor, FRAME_MAGIC.len())?;
    if &bytes[..FRAME_MAGIC.len()] != FRAME_MAGIC {
        return invalid_journal(path, "magic mismatch");
    }
    cursor += FRAME_MAGIC.len();
    let schema = take_u32(path, &bytes, &mut cursor)?;
    if schema != FRAME_SCHEMA {
        return invalid_journal(
            path,
            format!("expected schema {FRAME_SCHEMA}, found {schema}"),
        );
    }
    let launch_bytes = take_u32(path, &bytes, &mut cursor)? as usize;
    demand_bytes(path, &bytes, cursor, launch_bytes)?;
    let launch = std::str::from_utf8(&bytes[cursor..cursor + launch_bytes]).map_err(|error| {
        Error::FrameJournal {
            path: path.to_owned(),
            detail: format!("launch seal is not UTF-8: {error}"),
        }
    })?;
    if launch != expected_launch {
        return invalid_journal(
            path,
            format!("launch nonce mismatch: expected `{expected_launch}`, found `{launch}`"),
        );
    }
    cursor += launch_bytes;
    let complete = (bytes.len() - cursor) / FRAME_RECORD_BYTES;
    let mut samples = Vec::with_capacity(complete);
    for _ in 0..complete {
        samples.push(FrameSample {
            frame: take_u64(path, &bytes, &mut cursor)?,
            surface_sequence: take_u64(path, &bytes, &mut cursor)?,
            begun_ns: take_u64(path, &bytes, &mut cursor)?,
            observed_ns: take_u64(path, &bytes, &mut cursor)?,
            surface_presented_ns: take_u64(path, &bytes, &mut cursor)?,
        });
    }
    validate_samples(path, &samples)?;
    Ok(samples)
}

/// Shared `CLOCK_MONOTONIC` epoch used for input-to-observation latency.
#[must_use]
pub fn monotonic_ns() -> u64 {
    let now = clock_gettime(ClockId::Monotonic);
    u64::try_from(now.tv_sec)
        .unwrap_or_default()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(now.tv_nsec).unwrap_or_default())
}

fn open_frame_journal(path: &Path, launch: &str) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            operation: "create parent for",
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut output = File::create(path).map_err(|source| Error::Io {
        operation: "create",
        path: path.to_owned(),
        source,
    })?;
    let launch_bytes = launch.as_bytes();
    let launch_len = u32::try_from(launch_bytes.len()).map_err(|error| Error::FrameJournal {
        path: path.to_owned(),
        detail: format!("launch seal is too long: {error}"),
    })?;
    let mut header = Vec::with_capacity(16 + launch_bytes.len());
    header.extend(FRAME_MAGIC);
    header.extend(FRAME_SCHEMA.to_le_bytes());
    header.extend(launch_len.to_le_bytes());
    header.extend(launch_bytes);
    output.write_all(&header).map_err(|source| Error::Io {
        operation: "write header to",
        path: path.to_owned(),
        source,
    })?;
    Ok(output)
}

fn open_observation_journal(path: &Path, launch: &str) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            operation: "create parent for",
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut output = File::create(path).map_err(|source| Error::Io {
        operation: "create",
        path: path.to_owned(),
        source,
    })?;
    write_observation_header(&mut output, path, launch)?;
    Ok(output)
}

fn open_observation_reader(path: &Path, expected_launch: &str) -> Result<File> {
    let mut input = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| Error::Io {
            operation: "open",
            path: path.to_owned(),
            source,
        })?;
    let mut magic = [0_u8; OBSERVATION_MAGIC.len()];
    read_journal_header(&mut input, path, &mut magic)?;
    if &magic != OBSERVATION_MAGIC {
        return invalid_observation_journal(path, "magic mismatch");
    }
    let schema = read_observation_u32(&mut input, path, "schema")?;
    if schema != OBSERVATION_SCHEMA {
        return invalid_observation_journal(
            path,
            format!("expected schema {OBSERVATION_SCHEMA}, found {schema}"),
        );
    }
    let launch_bytes = read_observation_u32(&mut input, path, "launch length")? as usize;
    let mut launch = vec![0_u8; launch_bytes];
    read_journal_header(&mut input, path, &mut launch)?;
    let launch = std::str::from_utf8(&launch).map_err(|error| Error::ObservationJournal {
        path: path.to_owned(),
        detail: format!("launch seal is not UTF-8: {error}"),
    })?;
    if launch != expected_launch {
        return invalid_observation_journal(
            path,
            format!("launch nonce mismatch: expected `{expected_launch}`, found `{launch}`"),
        );
    }
    Ok(input)
}

fn write_observation_header(output: &mut File, path: &Path, launch: &str) -> Result<()> {
    let launch_bytes = launch.as_bytes();
    let launch_len =
        u32::try_from(launch_bytes.len()).map_err(|error| Error::ObservationJournal {
            path: path.to_owned(),
            detail: format!("launch seal is too long: {error}"),
        })?;
    let mut header = Vec::with_capacity(16 + launch_bytes.len());
    header.extend(OBSERVATION_MAGIC);
    header.extend(OBSERVATION_SCHEMA.to_le_bytes());
    header.extend(launch_len.to_le_bytes());
    header.extend(launch_bytes);
    output.write_all(&header).map_err(|source| Error::Io {
        operation: "write header to",
        path: path.to_owned(),
        source,
    })
}

fn append_observation(output: &mut File, path: &Path, bytes: &[u8]) -> Result<()> {
    let length = u32::try_from(bytes.len()).map_err(|error| Error::ObservationJournal {
        path: path.to_owned(),
        detail: format!("record is too large: {error}"),
    })?;
    output
        .write_all(&length.to_le_bytes())
        .and_then(|()| output.write_all(bytes))
        .map_err(|source| Error::Io {
            operation: "append to",
            path: path.to_owned(),
            source,
        })
}

fn read_observation(input: &mut File, path: &Path) -> Result<Option<Vec<u8>>> {
    let start = input.stream_position().map_err(|source| Error::Io {
        operation: "read position from",
        path: path.to_owned(),
        source,
    })?;
    let mut encoded_length = [0_u8; size_of::<u32>()];
    if !read_complete(input, &mut encoded_length).map_err(|source| Error::Io {
        operation: "read from",
        path: path.to_owned(),
        source,
    })? {
        let _position = input
            .seek(SeekFrom::Start(start))
            .map_err(|source| Error::Io {
                operation: "rewind",
                path: path.to_owned(),
                source,
            })?;
        return Ok(None);
    }
    let length = u32::from_le_bytes(encoded_length) as usize;
    if length > MAX_OBSERVATION_BYTES {
        return invalid_observation_journal(
            path,
            format!("record length {length} exceeds {MAX_OBSERVATION_BYTES} bytes"),
        );
    }
    let mut bytes = vec![0_u8; length];
    if !read_complete(input, &mut bytes).map_err(|source| Error::Io {
        operation: "read from",
        path: path.to_owned(),
        source,
    })? {
        let _position = input
            .seek(SeekFrom::Start(start))
            .map_err(|source| Error::Io {
                operation: "rewind",
                path: path.to_owned(),
                source,
            })?;
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn read_complete(input: &mut File, bytes: &mut [u8]) -> std::io::Result<bool> {
    let mut cursor = 0;
    while cursor < bytes.len() {
        let read = input.read(&mut bytes[cursor..])?;
        if read == 0 {
            return Ok(false);
        }
        cursor += read;
    }
    Ok(true)
}

fn read_journal_header(input: &mut File, path: &Path, bytes: &mut [u8]) -> Result<()> {
    input.read_exact(bytes).map_err(|source| Error::Io {
        operation: "read header from",
        path: path.to_owned(),
        source,
    })
}

fn read_observation_u32(input: &mut File, path: &Path, field: &str) -> Result<u32> {
    let mut bytes = [0_u8; size_of::<u32>()];
    read_journal_header(input, path, &mut bytes)?;
    let value = u32::from_le_bytes(bytes);
    if field == "launch length" && value as usize > MAX_OBSERVATION_BYTES {
        return invalid_observation_journal(path, "launch seal is unreasonably large");
    }
    Ok(value)
}

fn invalid_observation_journal<T>(path: &Path, detail: impl Into<String>) -> Result<T> {
    Err(Error::ObservationJournal {
        path: path.to_owned(),
        detail: detail.into(),
    })
}

fn append_frame(output: &mut File, path: &Path, sample: FrameSample) -> Result<()> {
    let mut bytes = [0_u8; FRAME_RECORD_BYTES];
    for (slot, value) in [
        sample.frame,
        sample.surface_sequence,
        sample.begun_ns,
        sample.observed_ns,
        sample.surface_presented_ns,
    ]
    .into_iter()
    .enumerate()
    {
        let start = slot * size_of::<u64>();
        bytes[start..start + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
    }
    output.write_all(&bytes).map_err(|source| Error::Io {
        operation: "append to",
        path: path.to_owned(),
        source,
    })
}

fn validate_samples(path: &Path, samples: &[FrameSample]) -> Result<()> {
    for pair in samples.windows(2) {
        let [before, after] = pair else {
            continue;
        };
        if after.surface_sequence <= before.surface_sequence
            || after.frame < before.frame
            || after.begun_ns < before.begun_ns
        {
            return invalid_journal(path, "frame order regressed");
        }
    }
    for sample in samples {
        if !(sample.begun_ns <= sample.observed_ns
            && sample.observed_ns <= sample.surface_presented_ns)
        {
            return invalid_journal(path, format!("frame {} timestamps regress", sample.frame));
        }
    }
    Ok(())
}

fn take_u32(path: &Path, bytes: &[u8], cursor: &mut usize) -> Result<u32> {
    demand_bytes(path, bytes, *cursor, size_of::<u32>())?;
    let end = *cursor + size_of::<u32>();
    let value =
        u32::from_le_bytes(
            bytes[*cursor..end]
                .try_into()
                .map_err(|_| Error::FrameJournal {
                    path: path.to_owned(),
                    detail: "truncated u32".to_owned(),
                })?,
        );
    *cursor = end;
    Ok(value)
}

fn take_u64(path: &Path, bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    demand_bytes(path, bytes, *cursor, size_of::<u64>())?;
    let end = *cursor + size_of::<u64>();
    let value =
        u64::from_le_bytes(
            bytes[*cursor..end]
                .try_into()
                .map_err(|_| Error::FrameJournal {
                    path: path.to_owned(),
                    detail: "truncated u64".to_owned(),
                })?,
        );
    *cursor = end;
    Ok(value)
}

fn demand_bytes(path: &Path, bytes: &[u8], cursor: usize, count: usize) -> Result<()> {
    if bytes.len().saturating_sub(cursor) < count {
        invalid_journal(path, "truncated header")
    } else {
        Ok(())
    }
}

fn invalid_journal<T>(path: &Path, detail: impl Into<String>) -> Result<T> {
    Err(Error::FrameJournal {
        path: path.to_owned(),
        detail: detail.into(),
    })
}

#[cfg(feature = "egui")]
pub mod egui {
    use super::{Anchor, Result};
    use egui::{Context, Id, Rect, Ui, plugin::Plugin};

    const STORE: &str = "egui-tester-witness-anchors";

    #[derive(Clone, Default)]
    struct Anchors(Vec<(String, Rect)>);

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

    /// Register a semantic target for the current egui pass.
    pub fn record(ui: &Ui, name: impl Into<String>, rect: Rect) {
        record_rect(ui.ctx(), name, rect);
    }

    /// Register a semantic target from painter-only code.
    pub fn record_rect(ctx: &Context, name: impl Into<String>, rect: Rect) {
        ctx.data_mut(|data| {
            data.get_temp_mut_or_default::<Anchors>(Id::new(STORE))
                .0
                .push((name.into(), rect));
        });
    }

    /// Consume the current pass's targets in physical-pixel coordinates.
    pub fn take(ctx: &Context, pixels_per_point: f32) -> Result<Vec<Anchor>> {
        ctx.data_mut(|data| data.remove_temp::<Anchors>(Id::new(STORE)))
            .unwrap_or_default()
            .0
            .into_iter()
            .map(|(name, rect)| {
                Anchor::logical(
                    name,
                    [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                    pixels_per_point,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_rectangles_become_physical() {
        let anchor =
            Anchor::logical("blade", [1.0, 2.0, 3.0, 4.0], 1.5).expect("forge physical anchor");
        assert_eq!(anchor.rect, [1.5, 3.0, 4.5, 6.0]);
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let anchors = [
            Anchor::physical("blade", [0.0, 0.0, 1.0, 1.0]).expect("first anchor"),
            Anchor::physical("blade", [1.0, 1.0, 2.0, 2.0]).expect("second anchor"),
        ];
        assert!(PendingFrame::forge(1, 1.0, anchors, ()).is_err());
    }

    #[test]
    fn frame_journal_round_trips_complete_records() {
        let temporary = tempfile::NamedTempFile::new().expect("temporary journal");
        let mut output = open_frame_journal(temporary.path(), "launch").expect("journal header");
        let first = FrameSample {
            frame: 4,
            surface_sequence: 1,
            begun_ns: 10,
            observed_ns: 20,
            surface_presented_ns: 30,
        };
        let second = FrameSample {
            frame: 5,
            surface_sequence: 2,
            begun_ns: 40,
            observed_ns: 50,
            surface_presented_ns: 60,
        };
        append_frame(&mut output, temporary.path(), first).expect("first frame");
        append_frame(&mut output, temporary.path(), second).expect("second frame");
        output.write_all(&[0xAA, 0xBB]).expect("partial tail");
        output.flush().expect("flush journal");
        assert_eq!(
            read_frame_journal(temporary.path(), "launch").expect("read journal"),
            vec![first, second]
        );
    }

    #[test]
    fn observation_journal_retains_brief_and_partial_records() {
        #[derive(Debug, Deserialize, PartialEq, Serialize)]
        struct Mark {
            value: u8,
        }

        let root = tempfile::tempdir().expect("temporary journal root");
        let path = root.path().join("witness.observations");
        let mut output = open_observation_journal(&path, "launch").expect("journal header");
        let first = serde_json::to_vec(&Mark { value: 1 }).expect("first record");
        append_observation(&mut output, &path, &first).expect("append first record");
        let second = serde_json::to_vec(&Mark { value: 2 }).expect("second record");
        let length = u32::try_from(second.len())
            .expect("tiny record length")
            .to_le_bytes();
        output.write_all(&length).expect("partial record length");
        output.write_all(&second[..2]).expect("partial record body");

        let mut reader = ObservationJournal::sealed(&path, "launch");
        assert_eq!(
            reader.read_new::<Mark>().expect("first read"),
            vec![Mark { value: 1 }]
        );
        assert!(
            reader
                .read_new::<Mark>()
                .expect("partial tail is not corruption")
                .is_empty()
        );

        output
            .write_all(&second[2..])
            .expect("complete second record");
        assert_eq!(
            reader.read_new::<Mark>().expect("second read"),
            vec![Mark { value: 2 }]
        );
    }

    #[test]
    fn publisher_drop_drains_both_journals_in_surface_order() {
        #[derive(Debug, Deserialize, PartialEq, Serialize)]
        struct Mark {
            value: u8,
        }

        let root = tempfile::tempdir().expect("publisher root");
        let observations = root.path().join("observations");
        let frames = root.path().join("frames");
        let mut publisher =
            Publisher::raise(observations.clone(), frames.clone(), "launch".to_owned())
                .expect("raise publisher");
        for (frame, value) in [(7, 1), (7, 2)] {
            let begun = ProductInstant(u64::from(value) * 10);
            let observed = ProductInstant(begun.0 + 1);
            let pending = PendingFrame::forge_at(
                FrameObservation::from_instants(begun, observed).expect("ordered observation"),
                frame,
                1.0,
                [],
                Mark { value },
            )
            .expect("stage frame");
            let _sequence = publisher
                .surface_present_at(pending, ProductInstant(observed.0 + 1))
                .expect("enqueue frame");
        }
        drop(publisher);

        let mut journal = ObservationJournal::sealed(&observations, "launch");
        assert_eq!(
            journal
                .read_new::<WireFrameOwned<Mark>>()
                .expect("read observations"),
            [
                WireFrameOwned {
                    surface_sequence: 1,
                    state: Mark { value: 1 },
                },
                WireFrameOwned {
                    surface_sequence: 2,
                    state: Mark { value: 2 },
                },
            ]
        );
        let samples = read_frame_journal(&frames, "launch").expect("read frame journal");
        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.surface_sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct WireFrameOwned<T> {
        surface_sequence: u64,
        state: T,
    }

    #[cfg(feature = "egui")]
    #[test]
    fn discarded_egui_passes_leave_only_the_presented_targets() {
        use ::egui::{Context, RawInput, Rect, pos2};

        let ctx = Context::default();
        egui::install(&ctx);
        let mut pass = 0;
        let _output = ctx.run_ui(RawInput::default(), |ui| {
            pass += 1;
            egui::record_rect(
                ui.ctx(),
                "blade",
                Rect::from_min_max(pos2(pass as f32, 0.0), pos2(pass as f32 + 1.0, 1.0)),
            );
            if pass == 1 {
                ui.ctx().request_discard("exercise final-pass telemetry");
            }
        });
        let anchors = egui::take(&ctx, 1.0).expect("final-pass anchors");
        assert_eq!(pass, 2);
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].rect, [2.0, 0.0, 3.0, 1.0]);
    }
}
