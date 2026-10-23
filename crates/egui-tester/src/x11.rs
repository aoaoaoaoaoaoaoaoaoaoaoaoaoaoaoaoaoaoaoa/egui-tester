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

use serde::de::DeserializeOwned;
use x11rb::{
    CURRENT_TIME,
    connection::Connection as _,
    image::Image,
    protocol::{
        xproto::{
            Atom, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ClientMessageEvent, ConnectionExt as _,
            EventMask, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, Window as WindowId,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::{DefaultStream, RustConnection},
};
use xkeysym::Keysym;

use crate::{
    ActionReceipt, Application, Button, Cursor, Error, Frame, Key, Modifiers, PixelRegion, Probe,
    Result, Testbed,
};

mod controller;
mod cursor;
mod embedded;
mod gesture;
mod keymap;
mod pixels;
mod window;

pub use controller::X11Controller;
pub(crate) use gesture::Rest;
use keymap::Keymap;

const AUTH_PROTOCOL: &[u8] = b"MIT-MAGIC-COOKIE-1";
const MODIFIER_GUARD: Duration = Duration::from_millis(32);
const POINTER_DELIVERY_GUARD: Duration = Duration::from_millis(32);

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
        cursor::prime(&controller.connection)?;
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
                .map_err(|err| x11("query window tree", err))?;
            let Some(tree) = window::reply("query window tree", tree.reply())? else {
                continue;
            };
            for child in tree.children {
                if let Some(title) = window::title(&self.connection, child, net_name, utf8)?
                    && query.matches(&title)
                    && window::viewable(&self.connection, child)?
                {
                    matches.push(Window { id: child, title });
                }
                queue.push_back(child);
            }
        }
        Ok(matches)
    }

    /// Activate a managed window through EWMH, or focus it directly without a window manager.
    pub fn focus(&self, window: &Window) -> Result<()> {
        let active = self.atom("_NET_ACTIVE_WINDOW")?;
        let supported = self.atom("_NET_SUPPORTED")?;
        if window::activate(
            &self.connection,
            self.root(),
            window.id,
            &window.title,
            active,
            supported,
        )? {
            return Ok(());
        }
        window::focus_unmanaged(&self.connection, window.id)
    }

    /// Deliver the ICCCM window-manager close protocol to one client window.
    pub fn close(&self, window: &Window) -> Result<ActionReceipt> {
        let (protocols, close) = (self.atom("WM_PROTOCOLS")?, self.atom("WM_DELETE_WINDOW")?);
        let receipt = ActionReceipt::begin(format!("close window `{}`", window.title));
        let event =
            ClientMessageEvent::new(32, window.id, protocols, [close, CURRENT_TIME, 0, 0, 0]);
        self.connection
            .send_event(false, window.id, EventMask::NO_EVENT, event)
            .map_err(|err| x11("request window close", err))?
            .check()
            .map_err(|err| x11("request window close", err))?;
        self.flush("request window close")?;
        Ok(receipt.trigger().finish())
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

    pub fn pointer(&self, window: &Window) -> Result<(i16, i16)> {
        let reply = self
            .connection
            .query_pointer(window.id)
            .map_err(|err| x11("query pointer", err))?
            .reply()
            .map_err(|err| x11("query pointer", err))?;
        Ok((reply.win_x, reply.win_y))
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
        let held = self.hold(modifiers)?;
        if let Err(err) = self.flush("acquire click modifiers") {
            self.release_keycodes(&held);
            let _flushed = self.flush("recover click modifiers");
            return Err(err);
        }
        if !held.is_empty() {
            thread::sleep(MODIFIER_GUARD);
        }
        let receipt = ActionReceipt::begin(format!("{modifiers:?} {button:?} click at ({x}, {y})"));
        let result = (|| -> Result<()> {
            self.fake(BUTTON_PRESS_EVENT, button as u8, 0, 0)?;
            self.fake(BUTTON_RELEASE_EVENT, button as u8, 0, 0)?;
            self.flush("click pointer")?;
            if !held.is_empty() {
                thread::sleep(MODIFIER_GUARD);
            }
            Ok(())
        })();
        self.release_keycodes(&held);
        let released = self.flush("release click modifiers");
        result?;
        released.map(|()| receipt.finish())
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
        Ok(receipt.finish())
    }

    pub fn button_up(&self, button: Button) -> Result<ActionReceipt> {
        let receipt = ActionReceipt::begin(format!("{button:?} up"));
        self.fake(BUTTON_RELEASE_EVENT, button as u8, 0, 0)?;
        self.flush("release pointer button")?;
        Ok(receipt.finish())
    }

    pub fn key(&self, key: Key) -> Result<ActionReceipt> {
        self.chord(Modifiers::empty(), key)
    }

    pub fn chord(&self, modifiers: Modifiers, key: Key) -> Result<ActionReceipt> {
        let keysym = key.keysym()?;
        let keymap = self.lend([keysym])?;
        let (keycode, shift) = keymap.keycode(keysym)?;
        let modifiers = if shift {
            modifiers | Modifiers::SHIFT
        } else {
            modifiers
        };
        let receipt = ActionReceipt::begin(format!("{modifiers:?}+{key:?}"));
        self.strike(&keymap, keycode, modifiers)?;
        Ok(receipt.finish())
    }

    pub fn key_down(&self, key: Key) -> Result<ActionReceipt> {
        let keysym = key.keysym()?;
        let (keycode, shift) = self.lend([keysym])?.keycode(keysym)?;
        if shift {
            return Err(Error::Unsupported {
                capability: "held shifted key",
                detail: "hold Shift explicitly and use the unshifted key".to_owned(),
            });
        }
        let receipt = ActionReceipt::begin(format!("{key:?} down"));
        self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
        self.flush("press key")?;
        Ok(receipt.finish())
    }

    pub fn key_up(&self, key: Key) -> Result<ActionReceipt> {
        let keysym = key.keysym()?;
        let (keycode, shift) = self.lend([keysym])?.keycode(keysym)?;
        if shift {
            return Err(Error::Unsupported {
                capability: "held shifted key",
                detail: "release the unshifted key and Shift explicitly".to_owned(),
            });
        }
        let receipt = ActionReceipt::begin(format!("{key:?} up"));
        self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)?;
        self.flush("release key")?;
        Ok(receipt.finish())
    }

    /// Type `text` one stroke per character, resolving every keycode once.
    pub fn type_text(&self, text: &str) -> Result<ActionReceipt> {
        let symbols = text
            .chars()
            .map(|character| Key::Character(character).keysym())
            .collect::<Result<Vec<_>>>()?;
        let keymap = self.lend(symbols.iter().copied())?;
        let strokes = symbols
            .into_iter()
            .map(|symbol| keymap.keycode(symbol))
            .collect::<Result<Vec<_>>>()?;
        let mut receipt = ActionReceipt::begin(format!("type {} character(s)", strokes.len()));
        for (index, &(keycode, shift)) in strokes.iter().enumerate() {
            if index + 1 == strokes.len() {
                receipt = receipt.trigger();
            }
            let modifiers = if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::empty()
            };
            self.strike(&keymap, keycode, modifiers)?;
        }
        Ok(receipt.finish())
    }

    /// Press and release `keycode` with `modifiers` held around it.
    fn strike(&self, keymap: &Keymap, keycode: u8, modifiers: Modifiers) -> Result<()> {
        let held = self.press_modifiers(keymap, modifiers)?;
        let result = (|| -> Result<()> {
            self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
            self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)
        })();
        self.release_keycodes(&held);
        result?;
        self.flush("send key")
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
        let rgba = pixels::decode(&image, visual.red_mask, visual.green_mask, visual.blue_mask);
        Ok(Frame::new(
            u32::from(geometry.width),
            u32::from(geometry.height),
            rgba,
        ))
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

    pub fn wait_changed_region(
        &self,
        app: &Application<'_>,
        window: &Window,
        baseline: &Frame,
        region: PixelRegion,
        minimum_fraction: f64,
        channel_slop: u8,
        timeout: Duration,
    ) -> Result<Frame> {
        let deadline = Instant::now() + timeout;
        loop {
            app.ensure_running("window pixel region to change")?;
            let frame = self.capture(window)?;
            if baseline.difference_region(&frame, region, channel_slop)? >= minimum_fraction {
                return Ok(frame);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: format!(
                        "window pixel region to change by at least {minimum_fraction:.5}"
                    ),
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

    fn flush(&self, operation: &'static str) -> Result<()> {
        self.connection.flush().map_err(|err| x11(operation, err))
    }

    /// Press `modifiers`, reading the keymap only when there is one to press.
    fn hold(&self, modifiers: Modifiers) -> Result<Vec<u8>> {
        if modifiers.is_empty() {
            return Ok(Vec::new());
        }
        self.press_modifiers(&self.keymap()?, modifiers)
    }

    fn press_modifiers(&self, keymap: &Keymap, modifiers: Modifiers) -> Result<Vec<u8>> {
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
            let (keycode, _) = match keymap.keycode(symbol) {
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

    /// The pointer the session's sealed X11 display presents over the window.
    pub fn cursor(&self) -> Result<Cursor> {
        self.controller.cursor(&self.window)
    }

    /// Make the bound client the visible native input destination.
    pub fn focus(&self) -> Result<()> {
        self.app.ensure_running("window focus")?;
        self.controller.focus(&self.window)?;
        self.note("focus", egui_tester_witness::monotonic_ns())
    }

    pub fn close(&self) -> Result<ActionReceipt> {
        self.app.ensure_running("window close")?;
        let receipt = self.controller.close(&self.window)?;
        self.record(&receipt)?;
        Ok(receipt)
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
        let receipt = receipt.finish();
        self.record(&receipt)?;
        Ok(receipt)
    }

    /// Move the pointer outside the bound client window.
    pub fn leave(&self) -> Result<ActionReceipt> {
        self.depart(self.departure()?)
    }

    /// The window coordinate outside the bound client to which
    /// [`Self::leave`] withdraws the pointer.
    pub(crate) fn departure(&self) -> Result<(i16, i16)> {
        self.controller.departure(&self.window)
    }

    pub(crate) fn depart(&self, departure: (i16, i16)) -> Result<ActionReceipt> {
        self.app.ensure_running("pointer departure")?;
        let receipt = ActionReceipt::begin("pointer departure");
        self.controller.depart(&self.window, departure)?;
        let receipt = receipt.finish();
        self.record(&receipt)?;
        Ok(receipt)
    }

    pub fn pointer(&self) -> Result<(i16, i16)> {
        self.app.ensure_running("query pointer")?;
        self.controller.pointer(&self.window)
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

    pub fn capture(&self) -> Result<Frame> {
        let frame = self.capture_ephemeral()?;
        self.remember(&frame)?;
        Ok(frame)
    }

    /// Capture product pixels without serializing the diagnostic latest-frame
    /// PNG. Stream observers already own the returned frame; persisting every
    /// sample would put PNG compression in their temporal hot path.
    pub(crate) fn capture_ephemeral(&self) -> Result<Frame> {
        self.app.ensure_running("window capture")?;
        self.controller.capture(&self.window)
    }

    /// Wait for the standard surface-present cue, then sample product pixels.
    pub fn wait_surface_presented<S: DeserializeOwned>(
        &self,
        probe: &mut Probe<S>,
        timeout: Duration,
    ) -> Result<Frame> {
        let presented = probe.wait_surface_presented(self.app, timeout);
        self.retain_failure_frame(&presented);
        let _presented = presented?;
        self.capture()
    }

    pub fn wait_changed(
        &self,
        baseline: &Frame,
        minimum_fraction: f64,
        channel_slop: u8,
        timeout: Duration,
    ) -> Result<Frame> {
        self.remember_result(self.controller.wait_changed(
            self.app,
            &self.window,
            baseline,
            minimum_fraction,
            channel_slop,
            timeout,
        ))
    }

    pub fn wait_changed_region(
        &self,
        baseline: &Frame,
        region: PixelRegion,
        minimum_fraction: f64,
        channel_slop: u8,
        timeout: Duration,
    ) -> Result<Frame> {
        self.remember_result(self.controller.wait_changed_region(
            self.app,
            &self.window,
            baseline,
            region,
            minimum_fraction,
            channel_slop,
            timeout,
        ))
    }

    fn record(&self, receipt: &ActionReceipt) -> Result<()> {
        self.write_transcript(&serde_json::json!({
            "gesture_started_ns": receipt.gesture_started_ns(),
            "triggered_ns": receipt.triggered_ns(),
            "completed_ns": receipt.completed_ns(),
            "action": receipt.action(),
        }))
    }

    fn note(&self, action: &str, at_ns: u64) -> Result<()> {
        self.write_transcript(&serde_json::json!({"at_ns": at_ns, "action": action}))
    }

    fn write_transcript(&self, entry: &serde_json::Value) -> Result<()> {
        let mut transcript = self.transcript.borrow_mut();
        serde_json::to_writer(&mut *transcript, entry).map_err(|err| Error::X11 {
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

    fn remember_result(&self, result: Result<Frame>) -> Result<Frame> {
        match result {
            Ok(frame) => {
                self.remember(&frame)?;
                Ok(frame)
            }
            Err(error) => {
                let _capture = self.capture();
                Err(error)
            }
        }
    }

    fn retain_failure_frame<T>(&self, result: &Result<T>) {
        if result.is_err() {
            let _capture = self.capture();
        }
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

fn x11(operation: &'static str, error: impl std::fmt::Display) -> Error {
    Error::X11 {
        operation,
        detail: error.to_string(),
    }
}
