use std::{
    cell::RefCell,
    collections::VecDeque,
    fs::File,
    io::{BufWriter, Write as _},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
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

use crate::{
    ActionReceipt, Application, Error, Frame, JsonProbe, Quiet, Result, Testbed, pixels::wait_quiet,
};

const AUTH_PROTOCOL: &[u8] = b"MIT-MAGIC-COOKIE-1";

/// X11 mouse button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Button {
    Primary = 1,
    Middle = 2,
    Secondary = 3,
}

bitflags::bitflags! {
    /// Keyboard modifiers held around one atomic input gesture.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct Modifiers: u8 {
        const SHIFT = 1 << 0;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
    }
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
    Function(u8),
    Shift,
    Control,
    Alt,
    Super,
}

/// Human-like pointer drag policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Drag {
    pub button: Button,
    /// Time allowed for the application to acquire the pressed target before motion.
    pub press_duration: Duration,
    pub steps: u16,
    /// Total time spent transporting the pointer after acquisition.
    pub duration: Duration,
}

impl Default for Drag {
    fn default() -> Self {
        Self {
            button: Button::Primary,
            press_duration: Duration::from_millis(32),
            steps: 8,
            duration: Duration::from_millis(120),
        }
    }
}

/// Top-level window selection law.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WindowQuery {
    TitleContains(String),
    TitleExact(String),
}

impl WindowQuery {
    #[must_use]
    pub fn title_contains(fragment: impl Into<String>) -> Self {
        Self::TitleContains(fragment.into())
    }

    #[must_use]
    pub fn title_exact(title: impl Into<String>) -> Self {
        Self::TitleExact(title.into())
    }

    fn matches(&self, title: &str) -> bool {
        match self {
            Self::TitleContains(fragment) => title.contains(fragment),
            Self::TitleExact(expected) => title == expected,
        }
    }

    fn description(&self) -> String {
        match self {
            Self::TitleContains(fragment) => format!("title containing `{fragment}`"),
            Self::TitleExact(title) => format!("title `{title}`"),
        }
    }
}

impl From<&str> for WindowQuery {
    fn from(fragment: &str) -> Self {
        Self::title_contains(fragment)
    }
}

impl From<String> for WindowQuery {
    fn from(fragment: String) -> Self {
        Self::TitleContains(fragment)
    }
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
        self.wait_window_query(app, WindowQuery::title_contains(title_fragment), timeout)
    }

    pub fn wait_window_query(
        &self,
        app: &Application<'_>,
        query: impl Into<WindowQuery>,
        timeout: Duration,
    ) -> Result<Window> {
        let query = query.into();
        let description = query.description();
        let deadline = Instant::now() + timeout;
        loop {
            app.ensure_running(format!("window with {description}"))?;
            if let Some(window) = self.find_windows(&query)?.into_iter().next() {
                return Ok(window);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: format!("X11 window with {description}"),
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn find_window(&self, title_fragment: &str) -> Result<Option<Window>> {
        Ok(self
            .find_windows(&WindowQuery::title_contains(title_fragment))?
            .into_iter()
            .next())
    }

    pub fn find_windows(&self, query: &WindowQuery) -> Result<Vec<Window>> {
        let root = self.root();
        let net_name = self.atom("_NET_WM_NAME")?;
        let utf8 = self.atom("UTF8_STRING")?;
        let mut queue = VecDeque::from([root]);
        let mut matches = Vec::new();
        while let Some(parent) = queue.pop_front() {
            let tree = self
                .connection
                .query_tree(parent)
                .map_err(|err| x11("query window tree", err))?
                .reply()
                .map_err(|err| x11("query window tree", err))?;
            for child in tree.children {
                if let Some(title) = self.window_title(child, net_name, utf8)?
                    && query.matches(&title)
                    && self.window_viewable(child)?
                {
                    matches.push(Window { id: child, title });
                }
                queue.push_back(child);
            }
        }
        Ok(matches)
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

    pub fn click(&self, window: &Window, x: i16, y: i16, button: Button) -> Result<ActionReceipt> {
        self.modified_click(window, x, y, button, Modifiers::empty())
    }

    pub fn modified_click(
        &self,
        window: &Window,
        x: i16,
        y: i16,
        button: Button,
        modifiers: Modifiers,
    ) -> Result<ActionReceipt> {
        self.move_to(window, x, y)?;
        let receipt = ActionReceipt::begin(format!("{modifiers:?} {button:?} click at ({x}, {y})"));
        let held = self.press_modifiers(modifiers)?;
        let result = (|| {
            self.fake(BUTTON_PRESS_EVENT, button as u8, 0, 0)?;
            self.fake(BUTTON_RELEASE_EVENT, button as u8, 0, 0)
        })();
        self.release_keycodes(&held);
        result?;
        self.flush("click pointer").map(|()| receipt)
    }

    pub fn button_down(
        &self,
        window: &Window,
        x: i16,
        y: i16,
        button: Button,
    ) -> Result<ActionReceipt> {
        self.move_to(window, x, y)?;
        let receipt = ActionReceipt::begin(format!("{button:?} down at ({x}, {y})"));
        self.fake(BUTTON_PRESS_EVENT, button as u8, 0, 0)?;
        self.flush("press pointer button")?;
        Ok(receipt)
    }

    pub fn button_up(&self, button: Button) -> Result<ActionReceipt> {
        let receipt = ActionReceipt::begin(format!("{button:?} up"));
        self.fake(BUTTON_RELEASE_EVENT, button as u8, 0, 0)?;
        self.flush("release pointer button")?;
        Ok(receipt)
    }

    pub fn scroll(
        &self,
        window: &Window,
        x: i16,
        y: i16,
        vertical_ticks: i32,
    ) -> Result<ActionReceipt> {
        self.move_to(window, x, y)?;
        let receipt = ActionReceipt::begin(format!("scroll {vertical_ticks} ticks at ({x}, {y})"));
        let detail = if vertical_ticks < 0 { 4 } else { 5 };
        for _ in 0..vertical_ticks.unsigned_abs() {
            self.fake(BUTTON_PRESS_EVENT, detail, 0, 0)?;
            self.fake(BUTTON_RELEASE_EVENT, detail, 0, 0)?;
        }
        self.flush("scroll pointer")?;
        Ok(receipt)
    }

    pub fn key(&self, key: Key) -> Result<ActionReceipt> {
        self.chord(Modifiers::empty(), key)
    }

    pub fn chord(&self, modifiers: Modifiers, key: Key) -> Result<ActionReceipt> {
        let keysym = key.keysym()?;
        let (keycode, shift) = self.keycode(keysym)?;
        let modifiers = if shift {
            modifiers | Modifiers::SHIFT
        } else {
            modifiers
        };
        let receipt = ActionReceipt::begin(format!("{modifiers:?}+{key:?}"));
        let held = self.press_modifiers(modifiers)?;
        let result = (|| {
            self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
            self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)?;
            Ok(())
        })();
        self.release_keycodes(&held);
        result?;
        self.flush("send key")?;
        Ok(receipt)
    }

    pub fn key_down(&self, key: Key) -> Result<ActionReceipt> {
        let (keycode, shift) = self.keycode(key.keysym()?)?;
        if shift {
            return Err(Error::Unsupported {
                capability: "held shifted key",
                detail: "hold Shift explicitly and use the unshifted key".to_owned(),
            });
        }
        let receipt = ActionReceipt::begin(format!("{key:?} down"));
        self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
        self.flush("press key")?;
        Ok(receipt)
    }

    pub fn key_up(&self, key: Key) -> Result<ActionReceipt> {
        let (keycode, shift) = self.keycode(key.keysym()?)?;
        if shift {
            return Err(Error::Unsupported {
                capability: "held shifted key",
                detail: "release the unshifted key and Shift explicitly".to_owned(),
            });
        }
        let receipt = ActionReceipt::begin(format!("{key:?} up"));
        self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)?;
        self.flush("release key")?;
        Ok(receipt)
    }

    pub fn type_text(&self, text: &str) -> Result<ActionReceipt> {
        let receipt = ActionReceipt::begin(format!("type {} character(s)", text.chars().count()));
        for character in text.chars() {
            let _receipt = self.key(Key::Character(character))?;
        }
        Ok(receipt)
    }

    pub fn drag(
        &self,
        window: &Window,
        from: (i16, i16),
        to: (i16, i16),
        policy: Drag,
    ) -> Result<ActionReceipt> {
        if policy.steps == 0 {
            return Err(Error::X11 {
                operation: "drag pointer",
                detail: "drag policy requires at least one motion step".to_owned(),
            });
        }
        self.move_to(window, from.0, from.1)?;
        self.fake(BUTTON_PRESS_EVENT, policy.button as u8, 0, 0)?;
        self.flush("begin pointer drag")?;
        if !policy.press_duration.is_zero() {
            thread::sleep(policy.press_duration);
        }
        let pause = policy.duration / u32::from(policy.steps);
        let mut receipt = None;
        for step in 1..=policy.steps {
            if step == policy.steps {
                receipt = Some(ActionReceipt::begin(format!(
                    "{:?} drag commit ({}, {}) → ({}, {})",
                    policy.button, from.0, from.1, to.0, to.1
                )));
            }
            let fraction = f64::from(step) / f64::from(policy.steps);
            let x = f64::from(from.0)
                .mul_add(1.0 - fraction, f64::from(to.0) * fraction)
                .round() as i16;
            let y = f64::from(from.1)
                .mul_add(1.0 - fraction, f64::from(to.1) * fraction)
                .round() as i16;
            if let Err(err) = self.move_to(window, x, y) {
                let _released = self.fake(BUTTON_RELEASE_EVENT, policy.button as u8, 0, 0);
                let _flushed = self.flush("abort pointer drag");
                return Err(err);
            }
            if step < policy.steps && !pause.is_zero() {
                thread::sleep(pause);
            }
        }
        if let Err(err) = self.fake(BUTTON_RELEASE_EVENT, policy.button as u8, 0, 0) {
            let _released = self.fake(BUTTON_RELEASE_EVENT, policy.button as u8, 0, 0);
            let _flushed = self.flush("recover pointer drag release");
            return Err(err);
        }
        self.flush("finish pointer drag")?;
        receipt.ok_or_else(|| Error::X11 {
            operation: "drag pointer",
            detail: "drag produced no commit receipt".to_owned(),
        })
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

    pub fn wait_quiet(
        &self,
        app: &Application<'_>,
        window: &Window,
        policy: Quiet,
    ) -> Result<Frame> {
        wait_quiet(
            || {
                app.ensure_running("window pixels to become quiet")?;
                self.capture(window)
            },
            policy,
        )
    }

    pub fn wait_changed(
        &self,
        app: &Application<'_>,
        window: &Window,
        baseline: &Frame,
        minimum_fraction: f64,
        channel_slop: u8,
        timeout: Duration,
    ) -> Result<Frame> {
        let deadline = Instant::now() + timeout;
        loop {
            app.ensure_running("window pixels to change")?;
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

    fn press_modifiers(&self, modifiers: Modifiers) -> Result<Vec<u8>> {
        let mut held = Vec::new();
        for (flag, symbol) in [
            (Modifiers::SHIFT, Keysym::Shift_L),
            (Modifiers::CTRL, Keysym::Control_L),
            (Modifiers::ALT, Keysym::Alt_L),
            (Modifiers::SUPER, Keysym::Super_L),
        ] {
            if !modifiers.contains(flag) {
                continue;
            }
            let (keycode, _) = match self.keycode(symbol) {
                Ok(binding) => binding,
                Err(err) => {
                    self.release_keycodes(&held);
                    return Err(err);
                }
            };
            if let Err(err) = self.fake(KEY_PRESS_EVENT, keycode, 0, 0) {
                self.release_keycodes(&held);
                return Err(err);
            }
            held.push(keycode);
        }
        Ok(held)
    }

    fn release_keycodes(&self, held: &[u8]) {
        for keycode in held.iter().rev() {
            let _released = self.fake(KEY_RELEASE_EVENT, *keycode, 0, 0);
        }
    }
}

/// Liveness-coupled application window and action transcript.
pub struct X11Session<'app, 'bed> {
    app: &'app Application<'bed>,
    controller: X11Controller,
    window: Window,
    transcript: RefCell<BufWriter<File>>,
    latest_capture: PathBuf,
}

impl std::fmt::Debug for X11Session<'_, '_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("X11Session")
            .field("unit", &self.app.unit())
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

impl<'app, 'bed> X11Session<'app, 'bed> {
    pub(crate) fn forge(
        testbed: &Testbed,
        app: &'app Application<'bed>,
        controller: X11Controller,
        window: Window,
    ) -> Result<Self> {
        let stem = app.unit().replace('.', "_");
        let transcript_path = testbed.diagnostic_path(format!("{stem}-actions.jsonl"));
        let transcript = File::create(&transcript_path).map_err(|err| {
            crate::error::io("create X11 action transcript", &transcript_path, err)
        })?;
        Ok(Self {
            app,
            controller,
            window,
            transcript: RefCell::new(BufWriter::new(transcript)),
            latest_capture: testbed.host_path(format!("captures/{stem}-latest.png")),
        })
    }

    #[must_use]
    pub const fn window(&self) -> &Window {
        &self.window
    }

    #[must_use]
    pub const fn application(&self) -> &Application<'bed> {
        self.app
    }

    pub fn focus(&self) -> Result<()> {
        self.app.ensure_running("window focus")?;
        self.controller.focus(&self.window)?;
        self.note("focus", egui_tester_witness::monotonic_ns())
    }

    pub fn click(&self, x: i16, y: i16, button: Button) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer click")?;
        let receipt = self.controller.click(&self.window, x, y, button)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn move_to(&self, x: i16, y: i16) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer motion")?;
        let receipt = ActionReceipt::begin(format!("pointer motion to ({x}, {y})"));
        self.controller.move_to(&self.window, x, y)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn modified_click(
        &self,
        x: i16,
        y: i16,
        button: Button,
        modifiers: Modifiers,
    ) -> Result<ActionReceipt> {
        self.app.ensure_running("modified pointer click")?;
        let receipt = self
            .controller
            .modified_click(&self.window, x, y, button, modifiers)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn chord(&self, modifiers: Modifiers, key: Key) -> Result<ActionReceipt> {
        self.app.ensure_running("keyboard chord")?;
        let receipt = self.controller.chord(modifiers, key)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn key(&self, key: Key) -> Result<ActionReceipt> {
        self.chord(Modifiers::empty(), key)
    }

    pub fn type_text(&self, text: &str) -> Result<ActionReceipt> {
        self.app.ensure_running("keyboard text")?;
        let receipt = self.controller.type_text(text)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn key_down(&self, key: Key) -> Result<ActionReceipt> {
        self.app.ensure_running("held key press")?;
        let receipt = self.controller.key_down(key)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn key_up(&self, key: Key) -> Result<ActionReceipt> {
        self.app.ensure_running("held key release")?;
        let receipt = self.controller.key_up(key)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn button_down(&self, x: i16, y: i16, button: Button) -> Result<ActionReceipt> {
        self.app.ensure_running("held pointer press")?;
        let receipt = self.controller.button_down(&self.window, x, y, button)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn button_up(&self, button: Button) -> Result<ActionReceipt> {
        self.app.ensure_running("held pointer release")?;
        let receipt = self.controller.button_up(button)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn drag(&self, from: (i16, i16), to: (i16, i16), policy: Drag) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer drag")?;
        let receipt = self.controller.drag(&self.window, from, to, policy)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn scroll(&self, x: i16, y: i16, ticks: i32) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer scroll")?;
        let receipt = self.controller.scroll(&self.window, x, y, ticks)?;
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn capture(&self) -> Result<Frame> {
        self.app.ensure_running("window capture")?;
        let frame = self.controller.capture(&self.window)?;
        self.remember(&frame)?;
        Ok(frame)
    }

    /// Wait for the standard post-present witness, then sample product pixels.
    pub fn wait_presented(&self, probe: &mut JsonProbe, timeout: Duration) -> Result<Frame> {
        let _presented = probe.wait_presented(self.app, timeout)?;
        self.capture()
    }

    pub fn wait_changed(
        &self,
        baseline: &Frame,
        minimum_fraction: f64,
        channel_slop: u8,
        timeout: Duration,
    ) -> Result<Frame> {
        let frame = self.controller.wait_changed(
            self.app,
            &self.window,
            baseline,
            minimum_fraction,
            channel_slop,
            timeout,
        )?;
        self.remember(&frame)?;
        Ok(frame)
    }

    pub fn wait_quiet(&self, policy: Quiet) -> Result<Frame> {
        let frame = self.controller.wait_quiet(self.app, &self.window, policy)?;
        self.remember(&frame)?;
        Ok(frame)
    }

    fn record(&self, receipt: &ActionReceipt) -> Result<()> {
        self.note(receipt.action(), receipt.started_ns())
    }

    fn note(&self, action: &str, at_ns: u64) -> Result<()> {
        let mut transcript = self.transcript.borrow_mut();
        serde_json::to_writer(
            &mut *transcript,
            &serde_json::json!({"at_ns": at_ns, "action": action}),
        )
        .map_err(|err| Error::X11 {
            operation: "write action transcript",
            detail: err.to_string(),
        })?;
        writeln!(transcript)
            .map_err(|err| crate::error::io("write X11 action transcript", "<transcript>", err))?;
        transcript
            .flush()
            .map_err(|err| crate::error::io("flush X11 action transcript", "<transcript>", err))
    }

    fn remember(&self, frame: &Frame) -> Result<()> {
        frame.save_png(&self.latest_capture)
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
            Self::Function(number) => match number {
                1 => Keysym::F1,
                2 => Keysym::F2,
                3 => Keysym::F3,
                4 => Keysym::F4,
                5 => Keysym::F5,
                6 => Keysym::F6,
                7 => Keysym::F7,
                8 => Keysym::F8,
                9 => Keysym::F9,
                10 => Keysym::F10,
                11 => Keysym::F11,
                12 => Keysym::F12,
                _ => {
                    return Err(Error::Unsupported {
                        capability: "function key",
                        detail: format!("F{number} is outside the portable F1–F12 set"),
                    });
                }
            },
            Self::Shift => Keysym::Shift_L,
            Self::Control => Keysym::Control_L,
            Self::Alt => Keysym::Alt_L,
            Self::Super => Keysym::Super_L,
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
