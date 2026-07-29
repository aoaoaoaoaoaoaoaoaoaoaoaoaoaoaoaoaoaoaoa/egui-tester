use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use crate::{Error, Result, error::io};

/// One observed RGBA frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Frame {
    pub(crate) fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            rgba,
        }
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Fraction of pixels whose maximum channel delta exceeds `channel_slop`.
    pub fn difference(&self, other: &Self, channel_slop: u8) -> Result<f64> {
        if (self.width, self.height) != (other.width, other.height) {
            return Ok(1.0);
        }
        let changed = self
            .rgba
            .chunks_exact(4)
            .zip(other.rgba.chunks_exact(4))
            .filter(|(left, right)| {
                left.iter()
                    .zip(right.iter())
                    .take(3)
                    .any(|(a, b)| a.abs_diff(*b) > channel_slop)
            })
            .count();
        let pixels = usize::try_from(self.width)
            .ok()
            .and_then(|width| {
                usize::try_from(self.height)
                    .ok()
                    .map(|height| width * height)
            })
            .filter(|pixels| *pixels != 0)
            .ok_or_else(|| Error::X11 {
                operation: "compare frames",
                detail: "zero-sized or unrepresentable frame".to_owned(),
            })?;
        Ok(changed as f64 / pixels as f64)
    }

    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| io("create artifact directory", parent, err))?;
        }
        let file = File::create(path).map_err(|err| io("create PNG", path, err))?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|err| Error::X11 {
            operation: "encode PNG header",
            detail: err.to_string(),
        })?;
        writer
            .write_image_data(&self.rgba)
            .map_err(|err| Error::X11 {
                operation: "encode PNG pixels",
                detail: err.to_string(),
            })
    }

    pub(crate) fn load_png(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|err| io("open captured PNG", path, err))?;
        let mut decoder = png::Decoder::new(BufReader::new(file));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|err| Error::X11 {
            operation: "decode captured PNG header",
            detail: err.to_string(),
        })?;
        let size = reader.output_buffer_size().ok_or_else(|| Error::X11 {
            operation: "decode captured PNG",
            detail: "decoded buffer exceeds platform limits".to_owned(),
        })?;
        let mut bytes = vec![0; size];
        let info = reader.next_frame(&mut bytes).map_err(|err| Error::X11 {
            operation: "decode captured PNG",
            detail: err.to_string(),
        })?;
        bytes.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => bytes,
            png::ColorType::Rgb => bytes
                .chunks_exact(3)
                .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                .collect(),
            png::ColorType::Grayscale => bytes
                .into_iter()
                .flat_map(|gray| [gray, gray, gray, 255])
                .collect(),
            png::ColorType::GrayscaleAlpha => bytes
                .chunks_exact(2)
                .flat_map(|pair| [pair[0], pair[0], pair[0], pair[1]])
                .collect(),
            png::ColorType::Indexed => {
                return Err(Error::X11 {
                    operation: "decode captured PNG",
                    detail: "indexed pixels remained after PNG expansion".to_owned(),
                });
            }
        };
        Ok(Self::new(info.width, info.height, rgba))
    }
}

/// Pixel-quiescence policy.
#[derive(Clone, Copy, Debug)]
pub struct Quiet {
    pub timeout: Duration,
    pub sample_every: Duration,
    pub consecutive: u8,
    pub changed_pixel_fraction: f64,
    pub channel_slop: u8,
}

impl Default for Quiet {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            sample_every: Duration::from_millis(50),
            consecutive: 3,
            changed_pixel_fraction: 0.000_5,
            channel_slop: 2,
        }
    }
}

pub(crate) fn wait_quiet(
    mut capture: impl FnMut() -> Result<Frame>,
    policy: Quiet,
) -> Result<Frame> {
    let deadline = Instant::now() + policy.timeout;
    let mut prior = capture()?;
    let mut quiet = 0_u8;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Timeout {
                waiting: format!(
                    "pixels to remain within {:.5} changed fraction for {} samples",
                    policy.changed_pixel_fraction, policy.consecutive
                ),
                timeout: policy.timeout,
            });
        }
        thread::sleep(policy.sample_every);
        let next = capture()?;
        if prior.difference(&next, policy.channel_slop)? <= policy.changed_pixel_fraction {
            quiet = quiet.saturating_add(1);
            if quiet >= policy.consecutive {
                return Ok(next);
            }
        } else {
            quiet = 0;
        }
        prior = next;
    }
}
