use x11rb::{
    connection::Connection as _,
    errors::ReplyError,
    protocol::{
        ErrorKind,
        xproto::{Atom, AtomEnum, ConnectionExt as _, MapState, Window},
    },
    rust_connection::RustConnection,
};

use crate::{Error, Result};

pub(super) fn exterior_point(
    connection: &RustConnection,
    screen: usize,
    window: Window,
    (left, top): (i16, i16),
) -> Result<(i16, i16)> {
    let geometry = connection
        .get_geometry(window)
        .map_err(|error| fault("query window geometry", error))?
        .reply()
        .map_err(|error| fault("query window geometry", error))?;
    let screen = &connection.setup().roots[screen];
    let screen_right = i16::try_from(screen.width_in_pixels.saturating_sub(1))
        .map_or(i16::MAX, |coordinate| coordinate);
    let screen_bottom = i16::try_from(screen.height_in_pixels.saturating_sub(1))
        .map_or(i16::MAX, |coordinate| coordinate);
    let right = i32::from(left) + i32::from(geometry.width);
    let bottom = i32::from(top) + i32::from(geometry.height);
    let outside = |(x, y): (i16, i16)| {
        let (x, y) = (i32::from(x), i32::from(y));
        x < i32::from(left) || x >= right || y < i32::from(top) || y >= bottom
    };
    [
        (0, 0),
        (screen_right, 0),
        (0, screen_bottom),
        (screen_right, screen_bottom),
    ]
    .into_iter()
    .find(|point| outside(*point))
    .ok_or_else(|| Error::X11 {
        operation: "move pointer outside window",
        detail: "client window covers every addressable screen corner".to_owned(),
    })
}

pub(super) fn title(
    connection: &RustConnection,
    window: Window,
    net_name: Atom,
    utf8: Atom,
) -> Result<Option<String>> {
    for (property, kind) in [
        (net_name, utf8),
        (AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()),
    ] {
        let cookie = connection
            .get_property(false, window, property, kind, 0, 4096)
            .map_err(|error| fault("read window title", error))?;
        let Some(property) = reply("read window title", cookie.reply())? else {
            return Ok(None);
        };
        if !property.value.is_empty() {
            return Ok(Some(
                String::from_utf8_lossy(&property.value)
                    .trim_end_matches('\0')
                    .to_owned(),
            ));
        }
    }
    Ok(None)
}

pub(super) fn viewable(connection: &RustConnection, window: Window) -> Result<bool> {
    let cookie = connection
        .get_window_attributes(window)
        .map_err(|error| fault("inspect window visibility", error))?;
    Ok(reply("inspect window visibility", cookie.reply())?
        .is_some_and(|attributes| attributes.map_state == MapState::VIEWABLE))
}

pub(super) fn reply<T>(
    operation: &'static str,
    result: std::result::Result<T, ReplyError>,
) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => Ok(None),
        Err(error) => Err(fault(operation, error)),
    }
}

fn fault(operation: &'static str, error: impl std::fmt::Display) -> Error {
    Error::X11 {
        operation,
        detail: error.to_string(),
    }
}
