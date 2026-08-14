use std::{
    thread,
    time::{Duration, Instant},
};

use x11rb::{
    CURRENT_TIME,
    connection::Connection as _,
    errors::ReplyError,
    protocol::{
        ErrorKind,
        xproto::{
            Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask,
            InputFocus, MapState, StackMode, Window,
        },
    },
    rust_connection::RustConnection,
};

use crate::{Error, Result};

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) fn activate(
    connection: &RustConnection,
    root: Window,
    window: Window,
    title: &str,
    active: Atom,
    supported: Atom,
) -> Result<bool> {
    let features = connection
        .get_property(false, root, supported, AtomEnum::ATOM, 0, u32::MAX)
        .map_err(|error| fault("read EWMH support", error))?
        .reply()
        .map_err(|error| fault("read EWMH support", error))?;
    if !features
        .value32()
        .is_some_and(|mut atoms| atoms.any(|atom| atom == active))
    {
        return Ok(false);
    }
    let current = active_window(connection, root, active)?.unwrap_or_default();
    let event = ClientMessageEvent::new(32, window, active, [2, CURRENT_TIME, current, 0, 0]);
    connection
        .send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .map_err(|error| fault("request window activation", error))?
        .check()
        .map_err(|error| fault("request window activation", error))?;
    connection
        .flush()
        .map_err(|error| fault("request window activation", error))?;
    let deadline = Instant::now() + ACTIVATION_TIMEOUT;
    loop {
        if active_window(connection, root, active)? == Some(window) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Err(Error::X11 {
                operation: "activate window",
                detail: format!(
                    "EWMH window manager did not activate `{title}` within {ACTIVATION_TIMEOUT:?}"
                ),
            });
        }
        thread::sleep(Duration::from_millis(8));
    }
}

fn active_window(
    connection: &RustConnection,
    root: Window,
    active: Atom,
) -> Result<Option<Window>> {
    let reply = connection
        .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
        .map_err(|error| fault("read active window", error))?
        .reply()
        .map_err(|error| fault("read active window", error))?;
    Ok(reply.value32().and_then(|mut windows| windows.next()))
}

pub(super) fn focus_unmanaged(connection: &RustConnection, window: Window) -> Result<()> {
    connection
        .configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )
        .map_err(|error| fault("raise window", error))?
        .check()
        .map_err(|error| fault("raise window", error))?;
    connection
        .set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)
        .map_err(|error| fault("focus window", error))?
        .check()
        .map_err(|error| fault("focus window", error))?;
    connection
        .flush()
        .map_err(|error| fault("focus window", error))
}

pub(super) fn exterior_candidates(
    connection: &RustConnection,
    screen: usize,
    window: Window,
    (left, top): (i16, i16),
) -> Result<Vec<(i16, i16)>> {
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
    let mut candidates = [
        (0, 0),
        (screen_right, 0),
        (0, screen_bottom),
        (screen_right, screen_bottom),
    ]
    .into_iter()
    .filter(|point| outside(*point))
    .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(Error::X11 {
            operation: "move pointer outside window",
            detail: "client window covers every addressable screen corner".to_owned(),
        });
    }
    let center_x = i64::from(left) + i64::from(geometry.width) / 2;
    let center_y = i64::from(top) + i64::from(geometry.height) / 2;
    candidates.sort_unstable_by_key(|&(x, y)| {
        let dx = i64::from(x) - center_x;
        let dy = i64::from(y) - center_y;
        std::cmp::Reverse(dx * dx + dy * dy)
    });
    Ok(candidates)
}

pub(super) fn pointer_is_outside(connection: &RustConnection, window: Window) -> Result<bool> {
    let geometry = connection
        .get_geometry(window)
        .map_err(|error| fault("query window geometry", error))?
        .reply()
        .map_err(|error| fault("query window geometry", error))?;
    let pointer = connection
        .query_pointer(window)
        .map_err(|error| fault("query pointer", error))?
        .reply()
        .map_err(|error| fault("query pointer", error))?;
    Ok(pointer.win_x < 0
        || pointer.win_y < 0
        || i32::from(pointer.win_x) >= i32::from(geometry.width)
        || i32::from(pointer.win_y) >= i32::from(geometry.height))
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
