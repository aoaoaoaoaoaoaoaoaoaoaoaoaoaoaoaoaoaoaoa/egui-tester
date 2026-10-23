use std::{
    fmt::Display,
    fs,
    path::{Path, PathBuf},
};

use x11rb::{
    connection::{Connection as _, RequestConnection},
    cookie::VoidCookie,
    errors::ConnectionError,
    protocol::{
        render::{ConnectionExt as _, CreatePictureAux, PictType},
        xfixes::ConnectionExt as _,
        xproto::{ChangeWindowAttributesAux, ConnectionExt as _, CreateGCAux, ImageFormat},
    },
    rust_connection::RustConnection,
};

use crate::{Error, Result};

use super::{X11Controller, x11};

/// Icon roots a product inside the testbed searches for cursor themes: its
/// private home holds none, leaving only the system's.
const ICON_ROOTS: [&str; 2] = ["/usr/share/icons", "/usr/share/pixmaps"];
/// Xcursor table-of-contents type of an image chunk.
const XCURSOR_IMAGE: u32 = 0xfffd_0002;
/// `XC_left_ptr` in the core cursor font; its mask is the next glyph.
const LEFT_PTR_GLYPH: u16 = 68;

/// A pointer bitmap and its hotspot, as the X server projects it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct X11CursorImage {
    /// Width of the server cursor bitmap in pixels.
    pub width: u16,
    /// Height of the server cursor bitmap in pixels.
    pub height: u16,
    /// Horizontal hotspot coordinate in bitmap pixels.
    pub hotspot_x: u16,
    /// Vertical hotspot coordinate in bitmap pixels.
    pub hotspot_y: u16,
    /// Row-major ARGB32 pixels, color premultiplied by alpha.
    pub argb: Vec<u32>,
}

pub(super) fn prime(connection: &RustConnection) -> Result<()> {
    let _version = connection
        .xfixes_query_version(5, 0)
        .map_err(|err| x11("query XFixes", err))?
        .reply()
        .map_err(|err| x11("query XFixes", err))?;
    Ok(())
}

impl X11Controller {
    /// Read the pointer image from this controller's sealed X11 display.
    pub fn cursor_image(&self) -> Result<X11CursorImage> {
        let image = self
            .connection
            .xfixes_get_cursor_image()
            .map_err(|err| x11("read pointer image", err))?
            .reply()
            .map_err(|err| x11("read pointer image", err))?;
        Ok(X11CursorImage {
            width: image.width,
            height: image.height,
            hotspot_x: image.xhot,
            hotspot_y: image.yhot,
            argb: image.cursor_image,
        })
    }

    /// Define the root window's pointer as every desktop session does, like
    /// `xsetroot -cursor_name left_ptr` through libXcursor: the themed
    /// `left_ptr` a product in the testbed would load, at libXcursor's default
    /// size, else the core font's arrow. Windows that define no pointer of
    /// their own, as winit leaves its default, inherit it; a bare server would
    /// show its `X_cursor` placeholder.
    pub(crate) fn dress_root(&self) -> Result<()> {
        let screen = &self.connection.setup().roots[self.screen];
        let size = u32::from(screen.width_in_pixels.min(screen.height_in_pixels) / 48);
        let cursor = match theme_cursor("default", "left_ptr", 8)
            .and_then(|path| fs::read(path).ok())
            .and_then(|file| xcursor_image(&file, size))
        {
            Some(image) => self.render_cursor(screen.root, &image)?,
            None => self.glyph_cursor()?,
        };
        let connection = &self.connection;
        dressed(connection.change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::new().cursor(cursor),
        ))?;
        dressed(connection.free_cursor(cursor))
    }

    fn render_cursor(&self, root: u32, image: &X11CursorImage) -> Result<u32> {
        let connection = &self.connection;
        let format = connection
            .render_query_pict_formats()
            .map_err(dressing)?
            .reply()
            .map_err(dressing)?
            .formats
            .into_iter()
            .find(|format| {
                let direct = format.direct;
                format.type_ == PictType::DIRECT
                    && format.depth == 32
                    && (direct.alpha_shift, direct.alpha_mask) == (24, 0xff)
                    && (direct.red_shift, direct.green_shift, direct.blue_shift) == (16, 8, 0)
            })
            .ok_or_else(|| dressing("server offers no ARGB32 picture format"))?
            .id;
        let id = || connection.generate_id().map_err(dressing);
        let [pixmap, gc, picture, cursor] = [id()?, id()?, id()?, id()?];
        let pixels: Vec<u8> = image
            .argb
            .iter()
            .flat_map(|pixel| pixel.to_ne_bytes())
            .collect();
        dressed(connection.create_pixmap(32, pixmap, root, image.width, image.height))?;
        dressed(connection.create_gc(gc, pixmap, &CreateGCAux::new()))?;
        dressed(connection.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            image.width,
            image.height,
            0,
            0,
            0,
            32,
            &pixels,
        ))?;
        dressed(connection.render_create_picture(
            picture,
            pixmap,
            format,
            &CreatePictureAux::new(),
        ))?;
        dressed(connection.render_create_cursor(
            cursor,
            picture,
            image.hotspot_x,
            image.hotspot_y,
        ))?;
        dressed(connection.render_free_picture(picture))?;
        dressed(connection.free_gc(gc))?;
        dressed(connection.free_pixmap(pixmap))?;
        Ok(cursor)
    }

    fn glyph_cursor(&self) -> Result<u32> {
        let connection = &self.connection;
        let id = || connection.generate_id().map_err(dressing);
        let [font, cursor] = [id()?, id()?];
        dressed(connection.open_font(font, b"cursor"))?;
        dressed(connection.create_glyph_cursor(
            cursor,
            font,
            font,
            LEFT_PTR_GLYPH,
            LEFT_PTR_GLYPH + 1,
            0,
            0,
            0,
            u16::MAX,
            u16::MAX,
            u16::MAX,
        ))?;
        dressed(connection.close_font(font))?;
        Ok(cursor)
    }
}

/// libXcursor's theme scan: the named file in any icon root, else the
/// themes the first `index.theme` inherits, depth-bounded against cycles.
fn theme_cursor(theme: &str, name: &str, depth: u8) -> Option<PathBuf> {
    let themes = ICON_ROOTS.map(|root| Path::new(root).join(theme));
    if let Some(file) = themes
        .iter()
        .map(|directory| directory.join("cursors").join(name))
        .find(|file| file.is_file())
    {
        return Some(file);
    }
    let index = themes
        .iter()
        .find_map(|directory| fs::read_to_string(directory.join("index.theme")).ok())?;
    let inherits = index.lines().find_map(|line| {
        line.strip_prefix("Inherits")?
            .trim_start()
            .strip_prefix('=')
            .map(str::to_owned)
    })?;
    inherits
        .split([',', ';'])
        .map(str::trim)
        .filter(|parent| !parent.is_empty() && *parent != theme)
        .find_map(|parent| theme_cursor(parent, name, depth.checked_sub(1)?))
}

/// The first image of the nominal size nearest `size` in an Xcursor file.
fn xcursor_image(file: &[u8], size: u32) -> Option<X11CursorImage> {
    let word = |at: usize| Some(u32::from_le_bytes(file.get(at..at + 4)?.try_into().ok()?));
    let index = |at: u32| usize::try_from(at).ok();
    if file.get(..4)? != b"Xcur" {
        return None;
    }
    let table = index(word(4)?)?;
    let (_, chunk) = (0..index(word(12)?)?)
        .map(|entry| table + entry * 12)
        .filter(|&entry| word(entry) == Some(XCURSOR_IMAGE))
        .map(|entry| Some((word(entry + 4)?, index(word(entry + 8)?)?)))
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .min_by_key(|&(nominal, _)| nominal.abs_diff(size))?;
    let dimension = |at: usize| u16::try_from(word(chunk + at)?).ok();
    let (width, height) = (dimension(16)?, dimension(20)?);
    let pixels = chunk + index(word(chunk)?)?;
    let argb = (0..usize::from(width) * usize::from(height))
        .map(|pixel| word(pixels + 4 * pixel))
        .collect::<Option<_>>()?;
    Some(X11CursorImage {
        width,
        height,
        hotspot_x: dimension(24)?,
        hotspot_y: dimension(28)?,
        argb,
    })
}

fn dressed<C: RequestConnection>(
    request: std::result::Result<VoidCookie<'_, C>, ConnectionError>,
) -> Result<()> {
    request.map_err(dressing)?.check().map_err(dressing)
}

fn dressing(error: impl Display) -> Error {
    x11("dress the root pointer", error)
}
