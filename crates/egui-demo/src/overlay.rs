//! Paint a film lays over each captured frame of the product's surface.

use std::num::NonZeroUsize;

use egui_tester::{CursorImage, Result, StorySurface};
use font8x8::{BASIC_FONTS, UnicodeFonts as _};

/// What a film paints over the product's presented surface.
///
/// An overlay never changes the beat structure: beats, chapters, holds, and
/// approaches take the same film time under either overlay, so a take's trace
/// indexes either film frame for frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Overlay {
    /// Proof marks: the aimed target's outline, rings at the pointer, a ripple
    /// per dispatched action, and a title card per chapter.
    #[default]
    Diagnostic,
    /// Only the display server's own pointer bitmap, hotspot-aligned where the
    /// server presents it, as a viewer of the screen would see it. A chapter
    /// is a rest on the live surface; its title remains in the trace.
    Presentation,
}

impl Overlay {
    /// Bind this overlay to one capture instant of `surface`: its presented
    /// pointer, else the pointer the story last `declared` on a projection
    /// that presents none.
    pub(crate) fn bind(self, surface: StorySurface<'_>, declared: Option<Point>) -> Result<Paint> {
        let (pointer, image) = match surface.cursor()? {
            Some(cursor) => (Some(Point::from(cursor.position)), Some(cursor.image)),
            None => (declared, None),
        };
        Ok(Paint {
            overlay: self,
            pointer,
            image,
        })
    }
}

/// An overlay bound to one capture instant.
pub(crate) struct Paint {
    overlay: Overlay,
    pointer: Option<Point>,
    /// The presented pointer's bitmap, absent where none is presented.
    image: Option<CursorImage>,
}

impl Paint {
    pub(crate) fn apply(&self, mut canvas: Canvas<'_>, scene: Scene<'_>) {
        match (self.overlay, &self.image, self.pointer) {
            (Overlay::Diagnostic, _, pointer) => canvas.mark(scene, pointer),
            (Overlay::Presentation, Some(image), Some(pointer)) => {
                canvas.composite(image, pointer);
            }
            (Overlay::Presentation, ..) => {}
        }
    }
}

/// One film tick's choreographic moment.
#[derive(Clone, Copy)]
pub(crate) enum Scene<'a> {
    /// The pointer closes on an aimed destination, outlined when a target.
    Aim {
        target: Option<Rect>,
    },
    /// An action was dispatched; `progress` runs through its beat from 0 to 1.
    Strike {
        target: Option<Rect>,
        progress: f64,
    },
    Rest,
    Chapter {
        title: &'a str,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct Point {
    x: i32,
    y: i32,
}

impl From<[i16; 2]> for Point {
    fn from([x, y]: [i16; 2]) -> Self {
        Self {
            x: i32::from(x),
            y: i32::from(y),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Rect {
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

/// One tick's copy of a captured RGBA frame, painted in place.
pub(crate) struct Canvas<'a> {
    rgba: &'a mut [u8],
    width: i32,
    height: i32,
}

impl<'a> Canvas<'a> {
    pub(crate) const fn new(rgba: &'a mut [u8], width: u32, height: u32) -> Self {
        Self {
            rgba,
            width: width as i32,
            height: height as i32,
        }
    }

    fn mark(&mut self, scene: Scene<'_>, pointer: Option<Point>) {
        match scene {
            Scene::Aim { target } => {
                if let Some(target) = target {
                    self.outline(target, [35, 211, 255, 210], 2);
                }
                if let Some(pointer) = pointer {
                    self.ring(pointer, 8, 11, [255, 255, 255, 230]);
                    self.ring(pointer, 11, 14, [20, 170, 220, 210]);
                }
            }
            Scene::Strike { target, progress } => {
                if let Some(target) = target {
                    self.outline(target, [35, 211, 255, 180], 2);
                }
                if let Some(pointer) = pointer {
                    let radius = 10 + ((progress * 8.0) as i32).min(7);
                    self.ring(pointer, radius, radius + 3, [255, 206, 75, 230]);
                }
            }
            Scene::Rest => {
                if let Some(pointer) = pointer {
                    self.ring(pointer, 5, 8, [35, 211, 255, 150]);
                }
            }
            Scene::Chapter { title } => self.chapter_card(title),
        }
    }

    /// Lay the server's cursor over the frame with its hotspot on `pointer`.
    /// The X server premultiplies the bitmap's color by its alpha, so the source
    /// adds to the veiled destination rather than blending with it.
    fn composite(&mut self, cursor: &CursorImage, pointer: Point) {
        let Some(width) = NonZeroUsize::new(usize::from(cursor.width)) else {
            return;
        };
        let left = pointer.x - i32::from(cursor.hotspot_x);
        let top = pointer.y - i32::from(cursor.hotspot_y);
        for (y, row) in (top..).zip(cursor.argb.chunks_exact(width.get())) {
            for (x, argb) in (left..).zip(row) {
                let Some(offset) = self.offset(x, y) else {
                    continue;
                };
                let [alpha, red, green, blue] = argb.to_be_bytes();
                let veil = 255 - u16::from(alpha);
                for (channel, source) in self.rgba[offset..offset + 3]
                    .iter_mut()
                    .zip([red, green, blue])
                {
                    let veiled = (u16::from(*channel) * veil + 127) / 255;
                    *channel = source.saturating_add(veiled as u8);
                }
            }
        }
    }

    fn chapter_card(&mut self, title: &str) {
        let (width, height) = (self.width, self.height);
        self.fill(
            Rect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            },
            [4, 9, 13, 150],
        );
        let scale = (width / 520).clamp(2, 4);
        let panel_width = (title.chars().count() as i32)
            .saturating_mul(8 * scale)
            .saturating_add(96)
            .min(width - 64)
            .max(240);
        let panel_height = 8 * scale + 72;
        let left = (width - panel_width) / 2;
        let top = (height - panel_height) / 2;
        let bottom = top + panel_height;
        self.fill(
            Rect {
                left,
                top,
                right: left + panel_width,
                bottom,
            },
            [6, 13, 19, 225],
        );
        self.fill(
            Rect {
                left,
                top,
                right: left + 8,
                bottom,
            },
            [35, 211, 255, 255],
        );
        self.inscribe(
            Point {
                x: left + 48,
                y: top + 36,
            },
            title,
            scale,
            [235, 245, 249, 255],
        );
    }

    fn inscribe(&mut self, origin: Point, text: &str, scale: i32, color: [u8; 4]) {
        let glyphs = text
            .chars()
            .filter_map(|character| BASIC_FONTS.get(character));
        for (left, glyph) in (0..).map(|index| origin.x + index * 8 * scale).zip(glyphs) {
            for (top, bits) in (0..).map(|row| origin.y + row * scale).zip(glyph) {
                for column in (0..8).filter(|column| bits & (1 << column) != 0) {
                    let left = left + column * scale;
                    self.fill(
                        Rect {
                            left,
                            top,
                            right: left + scale,
                            bottom: top + scale,
                        },
                        color,
                    );
                }
            }
        }
    }

    fn outline(&mut self, rect: Rect, color: [u8; 4], stroke: i32) {
        for edge in [
            Rect {
                bottom: rect.top + stroke,
                ..rect
            },
            Rect {
                top: rect.bottom - stroke,
                ..rect
            },
            Rect {
                right: rect.left + stroke,
                ..rect
            },
            Rect {
                left: rect.right - stroke,
                ..rect
            },
        ] {
            self.fill(edge, color);
        }
    }

    fn ring(&mut self, center: Point, inner: i32, outer: i32, color: [u8; 4]) {
        let band = inner * inner..=outer * outer;
        for y in center.y - outer..=center.y + outer {
            for x in center.x - outer..=center.x + outer {
                if band.contains(&((x - center.x).pow(2) + (y - center.y).pow(2))) {
                    self.blend(x, y, color);
                }
            }
        }
    }

    fn fill(&mut self, rect: Rect, color: [u8; 4]) {
        for y in rect.top.max(0)..rect.bottom.min(self.height) {
            for x in rect.left.max(0)..rect.right.min(self.width) {
                self.blend(x, y, color);
            }
        }
    }

    /// Blend a straight-alpha color over one pixel.
    fn blend(&mut self, x: i32, y: i32, [red, green, blue, alpha]: [u8; 4]) {
        let Some(offset) = self.offset(x, y) else {
            return;
        };
        let alpha = u16::from(alpha);
        for (channel, over) in self.rgba[offset..offset + 3]
            .iter_mut()
            .zip([red, green, blue])
        {
            *channel =
                ((u16::from(over) * alpha + u16::from(*channel) * (255 - alpha)) / 255) as u8;
        }
    }

    fn offset(&self, x: i32, y: i32) -> Option<usize> {
        ((0..self.width).contains(&x) && (0..self.height).contains(&y))
            .then(|| (y as usize * self.width as usize + x as usize) * 4)
    }
}
