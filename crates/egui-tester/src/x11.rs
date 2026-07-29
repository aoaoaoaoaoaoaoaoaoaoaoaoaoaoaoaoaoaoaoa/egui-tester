use std::{
    collections::VecDeque,
    os::unix::net::UnixStream,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use x11rb::{
    CURRENT_TIME,
    connection::Connection as _,
    image::Image,
    protocol::{
        xproto::{
            Atom, AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConfigureWindowAux,
            ConnectionExt as _, InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
            MOTION_NOTIFY_EVENT, MapState, Window as WindowId,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::{DefaultStream, RustConnection},
};
use xkeysym::Keysym;

use crate::{Application, Error, Frame, Quiet, Result, pixels::wait_quiet};

const AUTH_PROTOCOL: &[u8] = b"MIT-MAGIC-COOKIE-1";

/// X11 mouse button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Button {
    Primary = 1,
    Middle = 2,
    Secondary = 3,
}

/// Portable key subset plus Latin-1 characters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Character(char),
    Return,
    Escape,
    Tab,
    Backspace,
    Delete,
    Home,
    End,
    Left,
    Right,
    Up,
    Down,
}

/// A real top-level X11 window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Window {
    id: WindowId,
    title: String,
}

impl Window {
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
}

/// Authenticated control connection to the testbed's private X server.
pub struct X11Controller {
    connection: RustConnection,
    screen: usize,
}

impl std::fmt::Debug for X11Controller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("X11Controller")
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}

impl X11Controller {
    pub(crate) fn connect(display: u16, cookie: &[u8]) -> Result<Self> {
        let socket = Path::new("/tmp/.X11-unix").join(format!("X{display}"));
        let connection = connect_authenticated(&socket, cookie)?;
        let controller = Self {
            connection,
            screen: 0,
        };
        let _version = controller
            .connection
            .xtest_get_version(2, 2)
            .map_err(|err| x11("query XTEST", err))?
            .reply()
            .map_err(|err| x11("query XTEST", err))?;
        Ok(controller)
    }

    pub fn wait_window(
        &self,
        app: &Application<'_>,
        title_fragment: &str,
        timeout: Duration,
    ) -> Result<Window> {
        let deadline = Instant::now() + timeout;
        loop {
            app.ensure_running(format!("window containing `{title_fragment}`"))?;
            if let Some(window) = self.find_window(title_fragment)? {
                return Ok(window);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: format!("X11 window containing `{title_fragment}`"),
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn find_window(&self, title_fragment: &str) -> Result<Option<Window>> {
        let root = self.root();
        let net_name = self.atom("_NET_WM_NAME")?;
        let utf8 = self.atom("UTF8_STRING")?;
        let mut queue = VecDeque::from([root]);
        while let Some(parent) = queue.pop_front() {
            let tree = self
                .connection
                .query_tree(parent)
                .map_err(|err| x11("query window tree", err))?
                .reply()
                .map_err(|err| x11("query window tree", err))?;
            for child in tree.children {
                if let Some(title) = self.window_title(child, net_name, utf8)?
                    && title.contains(title_fragment)
                    && self.window_viewable(child)?
                {
                    return Ok(Some(Window { id: child, title }));
                }
                queue.push_back(child);
            }
        }
        Ok(None)
    }

    pub fn focus(&self, window: &Window) -> Result<()> {
        self.connection
            .configure_window(
                window.id,
                &ConfigureWindowAux::new()
                    .x(0)
                    .y(0)
                    .stack_mode(x11rb::protocol::xproto::StackMode::ABOVE),
            )
            .map_err(|err| x11("raise window", err))?
            .check()
            .map_err(|err| x11("raise window", err))?;
        self.connection
            .set_input_focus(InputFocus::PARENT, window.id, CURRENT_TIME)
            .map_err(|err| x11("focus window", err))?
            .check()
            .map_err(|err| x11("focus window", err))?;
        self.flush("focus window")
    }

    pub fn move_to(&self, window: &Window, x: i16, y: i16) -> Result<()> {
        let (root_x, root_y) = self.window_origin(window)?;
        self.fake(
            MOTION_NOTIFY_EVENT,
            0,
            root_x.saturating_add(x),
            root_y.saturating_add(y),
        )?;
        self.flush("move pointer")
    }

    pub fn click(&self, window: &Window, x: i16, y: i16, button: Button) -> Result<()> {
        self.move_to(window, x, y)?;
        self.fake(BUTTON_PRESS_EVENT, button as u8, 0, 0)?;
        self.fake(BUTTON_RELEASE_EVENT, button as u8, 0, 0)?;
        self.flush("click pointer")
    }

    pub fn scroll(&self, window: &Window, x: i16, y: i16, vertical_ticks: i32) -> Result<()> {
        self.move_to(window, x, y)?;
        let detail = if vertical_ticks < 0 { 4 } else { 5 };
        for _ in 0..vertical_ticks.unsigned_abs() {
            self.fake(BUTTON_PRESS_EVENT, detail, 0, 0)?;
            self.fake(BUTTON_RELEASE_EVENT, detail, 0, 0)?;
        }
        self.flush("scroll pointer")
    }

    pub fn key(&self, key: Key) -> Result<()> {
        let keysym = key.keysym()?;
        let (keycode, shift) = self.keycode(keysym)?;
        if shift {
            let (shift_code, _) = self.keycode(Keysym::Shift_L)?;
            self.fake(KEY_PRESS_EVENT, shift_code, 0, 0)?;
            self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
            self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)?;
            self.fake(KEY_RELEASE_EVENT, shift_code, 0, 0)?;
        } else {
            self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
            self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)?;
        }
        self.flush("send key")
    }

    pub fn type_text(&self, text: &str) -> Result<()> {
        for character in text.chars() {
            self.key(Key::Character(character))?;
        }
        Ok(())
    }

    pub fn capture(&self, window: &Window) -> Result<Frame> {
        let geometry = self
            .connection
            .get_geometry(window.id)
            .map_err(|err| x11("query window geometry", err))?
            .reply()
            .map_err(|err| x11("query window geometry", err))?;
        if geometry.width == 0 || geometry.height == 0 {
            return Err(Error::X11 {
                operation: "capture window",
                detail: "window has zero area".to_owned(),
            });
        }
        let (image, visual_id) = Image::get(
            &self.connection,
            window.id,
            0,
            0,
            geometry.width,
            geometry.height,
        )
        .map_err(|err| x11("capture window pixels", err))?;
        let visual = self
            .connection
            .setup()
            .roots
            .iter()
            .flat_map(|screen| &screen.allowed_depths)
            .flat_map(|depth| &depth.visuals)
            .find(|visual| visual.visual_id == visual_id)
            .ok_or_else(|| Error::X11 {
                operation: "decode window pixels",
                detail: format!("server omitted visual {visual_id:#x}"),
            })?;
        let capacity = usize::from(geometry.width) * usize::from(geometry.height) * 4;
        let mut rgba = Vec::with_capacity(capacity);
        for y in 0..geometry.height {
            for x in 0..geometry.width {
                let pixel = image.get_pixel(x, y);
                rgba.extend([
                    channel(pixel, visual.red_mask),
                    channel(pixel, visual.green_mask),
                    channel(pixel, visual.blue_mask),
                    255,
                ]);
            }
        }
        Ok(Frame::new(
            u32::from(geometry.width),
            u32::from(geometry.height),
            rgba,
        ))
    }

    pub fn wait_quiet(&self, window: &Window, policy: Quiet) -> Result<Frame> {
        wait_quiet(|| self.capture(window), policy)
    }

    pub fn wait_changed(
        &self,
        window: &Window,
        baseline: &Frame,
        minimum_fraction: f64,
        channel_slop: u8,
        timeout: Duration,
    ) -> Result<Frame> {
        let deadline = Instant::now() + timeout;
        loop {
            let frame = self.capture(window)?;
            if baseline.difference(&frame, channel_slop)? >= minimum_fraction {
                return Ok(frame);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: format!("window pixels to change by at least {minimum_fraction:.5}"),
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(15));
        }
    }

    fn root(&self) -> WindowId {
        self.connection.setup().roots[self.screen].root
    }

    fn atom(&self, name: &str) -> Result<Atom> {
        self.connection
            .intern_atom(false, name.as_bytes())
            .map_err(|err| x11("intern atom", err))?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|err| x11("intern atom", err))
    }

    fn window_title(&self, window: WindowId, net_name: Atom, utf8: Atom) -> Result<Option<String>> {
        for (property, kind) in [
            (net_name, utf8),
            (AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()),
        ] {
            let reply = self
                .connection
                .get_property(false, window, property, kind, 0, 4096)
                .map_err(|err| x11("read window title", err))?
                .reply()
                .map_err(|err| x11("read window title", err))?;
            if !reply.value.is_empty() {
                return Ok(Some(
                    String::from_utf8_lossy(&reply.value)
                        .trim_end_matches('\0')
                        .to_owned(),
                ));
            }
        }
        Ok(None)
    }

    fn window_viewable(&self, window: WindowId) -> Result<bool> {
        self.connection
            .get_window_attributes(window)
            .map_err(|err| x11("inspect window visibility", err))?
            .reply()
            .map(|attributes| attributes.map_state == MapState::VIEWABLE)
            .map_err(|err| x11("inspect window visibility", err))
    }

    fn window_origin(&self, window: &Window) -> Result<(i16, i16)> {
        self.connection
            .translate_coordinates(window.id, self.root(), 0, 0)
            .map_err(|err| x11("translate window coordinates", err))?
            .reply()
            .map(|reply| (reply.dst_x, reply.dst_y))
            .map_err(|err| x11("translate window coordinates", err))
    }

    fn fake(&self, event: u8, detail: u8, root_x: i16, root_y: i16) -> Result<()> {
        self.connection
            .xtest_fake_input(event, detail, CURRENT_TIME, self.root(), root_x, root_y, 0)
            .map_err(|err| x11("inject XTEST input", err))?
            .check()
            .map_err(|err| x11("inject XTEST input", err))
    }

    fn keycode(&self, symbol: Keysym) -> Result<(u8, bool)> {
        let setup = self.connection.setup();
        let count = setup.max_keycode - setup.min_keycode + 1;
        let mapping = self
            .connection
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|err| x11("read keyboard map", err))?
            .reply()
            .map_err(|err| x11("read keyboard map", err))?;
        let stride = usize::from(mapping.keysyms_per_keycode);
        for (row, symbols) in mapping.keysyms.chunks(stride).enumerate() {
            if let Some(column) = symbols
                .iter()
                .position(|candidate| *candidate == symbol.raw())
            {
                let row = u8::try_from(row).map_err(|err| Error::X11 {
                    operation: "resolve key",
                    detail: err.to_string(),
                })?;
                return Ok((setup.min_keycode + row, column % 2 == 1));
            }
        }
        Err(Error::Unsupported {
            capability: "keyboard symbol",
            detail: format!("the X11 keymap has no binding for {symbol:?}"),
        })
    }

    fn flush(&self, operation: &'static str) -> Result<()> {
        self.connection.flush().map_err(|err| x11(operation, err))
    }
}

impl Key {
    fn keysym(self) -> Result<Keysym> {
        Ok(match self {
            Self::Character(character) => {
                if u32::from(character) > 0xff {
                    return Err(Error::Unsupported {
                        capability: "text input",
                        detail: format!(
                            "`{character}` is outside the MVP's Latin-1 X11 key injection"
                        ),
                    });
                }
                Keysym::new(u32::from(character))
            }
            Self::Return => Keysym::Return,
            Self::Escape => Keysym::Escape,
            Self::Tab => Keysym::Tab,
            Self::Backspace => Keysym::BackSpace,
            Self::Delete => Keysym::Delete,
            Self::Home => Keysym::Home,
            Self::End => Keysym::End,
            Self::Left => Keysym::Left,
            Self::Right => Keysym::Right,
            Self::Up => Keysym::Up,
            Self::Down => Keysym::Down,
        })
    }
}

pub(crate) fn connect_authenticated(socket: &Path, cookie: &[u8]) -> Result<RustConnection> {
    let unix = UnixStream::connect(socket)
        .map_err(|err| crate::error::io("connect X11 socket", socket, err))?;
    let (stream, _peer) =
        DefaultStream::from_unix_stream(unix).map_err(|err| x11("prepare X11 socket", err))?;
    let connection = RustConnection::connect_to_stream_with_auth_info(
        stream,
        0,
        AUTH_PROTOCOL.to_vec(),
        cookie.to_vec(),
    )
    .map_err(|err| x11("authenticate X11 connection", err))?;
    Ok(connection)
}

fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let value = (pixel & mask) >> mask.trailing_zeros();
    let maximum = mask >> mask.trailing_zeros();
    ((value * 255 + maximum / 2) / maximum) as u8
}

fn x11(operation: &'static str, error: impl std::fmt::Display) -> Error {
    Error::X11 {
        operation,
        detail: error.to_string(),
    }
}
