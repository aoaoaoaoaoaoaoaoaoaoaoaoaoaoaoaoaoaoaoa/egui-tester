//! What a take declares and sees beside its story stream: the stems naming
//! its artifacts, the framing it presents, how its pointer approaches each
//! destination, and the pointer the display server presents.

use std::{borrow::Cow, fmt, num::NonZeroU16, time::Duration};

use serde::Serialize;

use crate::{Error, Result};

/// Longest admitted [`ArtifactStem`], in bytes.
const STEM_LIMIT: usize = 64;

/// Kebab-case stem naming one artifact a take publishes, a still or a film:
/// nonempty hyphen-joined segments of lowercase ASCII letters and digits, at
/// most 64 bytes. It is therefore one portable path component, which no
/// observer sanitizes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ArtifactStem(Cow<'static, str>);

impl ArtifactStem {
    /// The still a showcase leads with: its product's portfolio image.
    pub const FRONTISPIECE: Self = Self::literal("frontispiece");

    /// Admit `stem` under the kebab-case law.
    pub fn new(stem: impl Into<String>) -> Result<Self> {
        let stem = stem.into();
        if lawful(stem.as_bytes()) {
            Ok(Self(Cow::Owned(stem)))
        } else {
            Err(Error::Score {
                detail: format!(
                    "`{stem}` is not a kebab-case artifact stem of at most {STEM_LIMIT} bytes"
                ),
            })
        }
    }

    /// Admit a literal `stem` in a const item, where an unlawful one fails
    /// compilation. Evaluated at run time it panics instead, so admit runtime
    /// text with [`Self::new`].
    #[must_use]
    pub const fn literal(stem: &'static str) -> Self {
        assert!(
            lawful(stem.as_bytes()),
            "not a kebab-case artifact stem of at most 64 bytes"
        );
        Self(Cow::Borrowed(stem))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The kebab-case law: nonempty hyphen-joined segments of lowercase ASCII
/// letters and digits, at most [`STEM_LIMIT`] bytes in all.
const fn lawful(stem: &[u8]) -> bool {
    if stem.is_empty() || stem.len() > STEM_LIMIT {
        return false;
    }
    let mut index = 0;
    let mut opening = true;
    while index < stem.len() {
        match stem[index] {
            b'-' if !opening => opening = true,
            b'a'..=b'z' | b'0'..=b'9' => opening = false,
            _ => return false,
        }
        index += 1;
    }
    !opening
}

impl fmt::Display for ArtifactStem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Pixel geometry of a take's presented surface, and so of its film and every
/// still. A desktop story imposes it on the product window before the take
/// begins; a device screen cannot be reframed, so there it must already hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Framing {
    width: NonZeroU16,
    height: NonZeroU16,
}

impl Framing {
    /// The line's portfolio framing, 1440×920.
    pub const PORTFOLIO: Self = Self::new(pixels(1440), pixels(920));

    #[must_use]
    pub const fn new(width: NonZeroU16, height: NonZeroU16) -> Self {
        Self { width, height }
    }

    #[must_use]
    pub const fn width(self) -> u16 {
        self.width.get()
    }

    #[must_use]
    pub const fn height(self) -> u16 {
        self.height.get()
    }

    /// Whether a `width`×`height` surface is exactly this framing.
    #[must_use]
    pub fn frames(self, width: u32, height: u32) -> bool {
        (width, height) == (u32::from(self.width()), u32::from(self.height()))
    }
}

impl fmt::Display for Framing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}×{}", self.width, self.height)
    }
}

/// A nonzero pixel count for a const framing; zero fails compilation.
#[expect(
    clippy::panic,
    reason = "evaluated only in const items, where a zero dimension fails compilation"
)]
const fn pixels(count: u16) -> NonZeroU16 {
    match NonZeroU16::new(count) {
        Some(count) => count,
        None => panic!("a framing dimension must be nonzero"),
    }
}

/// How a desktop story's real pointer closes on each aimed destination before
/// its gesture strikes: a paced linear glide lasting `glide` from wherever it
/// rests, then a `dwell` there. A gesture whose own transport reaches its
/// destination takes no approach.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Approach {
    pub glide: Duration,
    pub dwell: Duration,
}

impl Approach {
    /// No glide and no dwell: each gesture jumps to its destination.
    pub const INSTANT: Self = Self {
        glide: Duration::ZERO,
        dwell: Duration::ZERO,
    };
}

/// A pointer bitmap and its hotspot, as the display server presents it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorImage {
    /// Width of the bitmap in pixels.
    pub width: u16,
    /// Height of the bitmap in pixels.
    pub height: u16,
    /// Horizontal hotspot coordinate in bitmap pixels.
    pub hotspot_x: u16,
    /// Vertical hotspot coordinate in bitmap pixels.
    pub hotspot_y: u16,
    /// Row-major ARGB32 pixels, color premultiplied by alpha.
    pub argb: Vec<u32>,
}

/// The presented pointer: its bitmap, and its hotspot's position in surface
/// coordinates, beyond the surface's bounds when the pointer is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cursor {
    pub image: CursorImage,
    pub position: [i16; 2],
}
