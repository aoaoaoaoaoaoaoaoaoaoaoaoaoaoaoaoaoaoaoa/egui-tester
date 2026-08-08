use x11rb::{
    errors::ReplyError,
    protocol::{
        ErrorKind,
        xproto::{Atom, AtomEnum, ConnectionExt as _, MapState, Window},
    },
    rust_connection::RustConnection,
};

use crate::{Error, Result};

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
