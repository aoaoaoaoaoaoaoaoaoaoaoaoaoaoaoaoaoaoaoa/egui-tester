use x11rb::{protocol::xfixes::ConnectionExt as _, rust_connection::RustConnection};

use crate::Result;

use super::{X11Controller, x11};

/// Exact server-side bitmap currently projected by the private X11 pointer.
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
    /// Row-major, unpremultiplied ARGB32 pixels.
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
}
