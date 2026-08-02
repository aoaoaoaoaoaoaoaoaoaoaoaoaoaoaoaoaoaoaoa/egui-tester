use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
};

use crate::{Anchor, Error, Result, error::io};

/// Physical-pixel rectangle used by rendered oracles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRegion {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl PixelRegion {
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    #[must_use]
    pub fn anchor(anchor: &Anchor) -> Self {
        let [left, top, right, bottom] = anchor.rect;
        Self::new(
            left.floor() as i32,
            top.floor() as i32,
            right.ceil() as i32,
            bottom.ceil() as i32,
        )
    }

    fn clipped(self, width: u32, height: u32) -> Option<[usize; 4]> {
        let width = i32::try_from(width).ok()?;
        let height = i32::try_from(height).ok()?;
        let left = self.left.clamp(0, width);
        let top = self.top.clamp(0, height);
        let right = self.right.clamp(0, width);
        let bottom = self.bottom.clamp(0, height);
        (left < right && top < bottom).then_some([
            left as usize,
            top as usize,
            right as usize,
            bottom as usize,
        ])
    }
}

/// One observed RGBA frame.
///
/// Frames deliberately have no exact equality: rendered evidence must declare
/// a bounded region and tolerance through [`Self::difference_region`].
#[derive(Clone, Debug)]
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
        self.difference_region(
            other,
            PixelRegion::new(0, 0, self.width as i32, self.height as i32),
            channel_slop,
        )
    }

    /// Changed-pixel fraction inside one physical window region.
    pub fn difference_region(
        &self,
        other: &Self,
        region: PixelRegion,
        channel_slop: u8,
    ) -> Result<f64> {
        if (self.width, self.height) != (other.width, other.height) {
            return Ok(1.0);
        }
        let [left, top, right, bottom] =
            region
                .clipped(self.width, self.height)
                .ok_or_else(|| Error::X11 {
                    operation: "compare frame region",
                    detail: "pixel region is empty or outside the frame".to_owned(),
                })?;
        let width = usize::try_from(self.width).map_err(|_| Error::X11 {
            operation: "compare frame region",
            detail: "frame width exceeds platform limits".to_owned(),
        })?;
        let mut changed = 0_usize;
        for y in top..bottom {
            for x in left..right {
                let offset = (y * width + x) * 4;
                let a = &self.rgba[offset..offset + 3];
                let b = &other.rgba[offset..offset + 3];
                changed += usize::from(
                    a.iter()
                        .zip(b)
                        .any(|(left, right)| left.abs_diff(*right) > channel_slop),
                );
            }
        }
        let pixels = (right - left) * (bottom - top);
        Ok(changed as f64 / pixels as f64)
    }

    /// Extract a physical window region as an independent frame.
    pub fn crop(&self, region: PixelRegion) -> Result<Self> {
        let [left, top, right, bottom] =
            region
                .clipped(self.width, self.height)
                .ok_or_else(|| Error::X11 {
                    operation: "crop frame",
                    detail: "pixel region is empty or outside the frame".to_owned(),
                })?;
        let stride = usize::try_from(self.width).map_err(|_| Error::X11 {
            operation: "crop frame",
            detail: "frame width exceeds platform limits".to_owned(),
        })? * 4;
        let mut rgba = Vec::with_capacity((right - left) * (bottom - top) * 4);
        for y in top..bottom {
            let start = y * stride + left * 4;
            let end = y * stride + right * 4;
            rgba.extend_from_slice(&self.rgba[start..end]);
        }
        Ok(Self::new(
            u32::try_from(right - left).map_err(|_| Error::X11 {
                operation: "crop frame",
                detail: "crop width exceeds u32".to_owned(),
            })?,
            u32::try_from(bottom - top).map_err(|_| Error::X11 {
                operation: "crop frame",
                detail: "crop height exceeds u32".to_owned(),
            })?,
            rgba,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(red: [u8; 4]) -> Frame {
        let mut rgba = vec![0; 4 * 4 * 4];
        rgba[4 * 4..4 * 4 + 4].copy_from_slice(&red);
        Frame::new(4, 4, rgba)
    }

    #[test]
    fn regional_difference_ignores_pixels_outside_the_oracle() {
        let blank = Frame::new(4, 4, vec![0; 4 * 4 * 4]);
        let marked = frame([255, 0, 0, 255]);
        assert_eq!(
            blank
                .difference_region(&marked, PixelRegion::new(0, 0, 4, 1), 0)
                .expect("compare untouched row"),
            0.0
        );
        assert_eq!(
            blank
                .difference_region(&marked, PixelRegion::new(0, 1, 4, 2), 0)
                .expect("compare marked row"),
            0.25
        );
    }

    #[test]
    fn crop_clips_to_the_frame_and_preserves_rows() {
        let marked = frame([255, 0, 0, 255]);
        let crop = marked
            .crop(PixelRegion::new(-4, 1, 2, 3))
            .expect("clip and crop");
        assert_eq!((crop.width(), crop.height()), (2, 2));
        assert_eq!(&crop.rgba()[..4], &[255, 0, 0, 255]);
        assert_eq!(&crop.rgba()[4..], &[0; 12]);
    }
}
