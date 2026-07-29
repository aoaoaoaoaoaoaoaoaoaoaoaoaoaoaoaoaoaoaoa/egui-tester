use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use crate::{
    AppCommand, Application, Error, Result, X11Controller, error::io, x11::connect_authenticated,
};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
static NEXT_DISPLAY: AtomicU64 = AtomicU64::new(0);

/// X11 virtual screen geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X11Config {
    pub width: u16,
    pub height: u16,
    pub depth: u8,
}

impl Default for X11Config {
    fn default() -> Self {
        Self {
            width: 1440,
            height: 920,
            depth: 24,
        }
    }
}

/// Headless Weston output geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaylandConfig {
    pub width: u16,
    pub height: u16,
}

impl Default for WaylandConfig {
    fn default() -> Self {
        Self {
            width: 1440,
            height: 920,
        }
    }
}

/// Display protocol under test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    X11(X11Config),
    Wayland(WaylandConfig),
}

impl Default for Backend {
    fn default() -> Self {
        Self::X11(X11Config::default())
    }
}

/// Construction policy for an isolated test universe.
#[derive(Clone, Debug, Default)]
pub struct TestbedBuilder {
    backend: Backend,
    failure_artifacts: Option<PathBuf>,
}

impl TestbedBuilder {
    #[must_use]
    pub fn backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    /// Directory into which the harness copies curated diagnostics on panic.
    ///
    /// This directory is never visible inside the application sandbox.
    #[must_use]
    pub fn failure_artifacts(mut self, path: impl Into<PathBuf>) -> Self {
        self.failure_artifacts = Some(path.into());
        self
    }

    pub fn raise(self) -> Result<Testbed> {
        for tool in ["bwrap", "systemd-run", "systemctl"] {
            require_tool(tool)?;
        }
        let ordinal = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        let id = format!("{}-{ordinal}", std::process::id());
        let root = tempfile::Builder::new()
            .prefix(&format!("egui-tester-{id}-"))
            .tempdir_in("/tmp")
            .map_err(|err| io("create private test root", "/tmp", err))?;
        harden_root(root.path())?;
        for relative in [
            "home",
            "tmp",
            "logs",
            "probes",
            "xdg/config",
            "xdg/cache",
            "xdg/data",
            "xdg/state",
            "xdg/runtime",
        ] {
            let path = root.path().join(relative);
            fs::create_dir_all(&path)
                .map_err(|err| io("create private test directory", path, err))?;
        }
        harden_root(&root.path().join("xdg/runtime"))?;
        let display = DisplayServer::raise(self.backend, root.path(), &id)?;
        Ok(Testbed {
            display,
            root,
            id,
            next_app: AtomicU64::new(1),
            failure_artifacts: self.failure_artifacts,
        })
    }
}

/// Owned display, process boundary, and disposable filesystem universe.
pub struct Testbed {
    // The display must die before the temporary tree carrying its sockets and
    // authorization state.
    display: DisplayServer,
    root: TempDir,
    id: String,
    next_app: AtomicU64,
    failure_artifacts: Option<PathBuf>,
}

impl std::fmt::Debug for Testbed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Testbed")
            .field("id", &self.id)
            .field("root", &self.root.path())
            .field("backend", &self.display)
            .finish_non_exhaustive()
    }
}

impl Testbed {
    pub fn builder() -> TestbedBuilder {
        TestbedBuilder::default()
    }

    pub fn raise() -> Result<Self> {
        Self::builder().raise()
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn private_path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let relative = relative.as_ref();
        validate_relative(relative)?;
        Ok(self.root.path().join(relative))
    }

    #[must_use]
    pub fn guest_path(relative: impl AsRef<Path>) -> PathBuf {
        Path::new("/test").join(relative)
    }

    pub fn create_private_dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.private_path(relative)?;
        fs::create_dir_all(&path).map_err(|err| io("create private directory", &path, err))?;
        Ok(path)
    }

    pub fn write_private(
        &self,
        relative: impl AsRef<Path>,
        bytes: impl AsRef<[u8]>,
    ) -> Result<PathBuf> {
        let path = self.private_path(relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io("create private file parent", parent, err))?;
        }
        fs::write(&path, bytes).map_err(|err| io("write private file", &path, err))?;
        Ok(path)
    }

    pub fn copy_private(
        &self,
        relative: impl AsRef<Path>,
        source: impl AsRef<Path>,
    ) -> Result<PathBuf> {
        let destination = self.private_path(relative)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io("create private copy parent", parent, err))?;
        }
        let _bytes = fs::copy(source.as_ref(), &destination)
            .map_err(|err| io("copy fixture into private tree", &destination, err))?;
        Ok(destination)
    }

    pub fn launch(&self, command: AppCommand) -> Result<Application<'_>> {
        let ordinal = self.next_app.fetch_add(1, Ordering::Relaxed);
        Application::raise(self, command, ordinal)
    }

    pub fn x11(&self) -> Result<X11Controller> {
        match &self.display {
            DisplayServer::X11(server) => server.controller(),
            DisplayServer::Wayland(_) => Err(Error::Unsupported {
                capability: "X11 control",
                detail: "the testbed owns a Wayland compositor".to_owned(),
            }),
        }
    }

    /// Capture the complete virtual Wayland output through Weston's output
    /// capture protocol.
    pub fn capture_wayland(&self) -> Result<crate::Frame> {
        match &self.display {
            DisplayServer::Wayland(server) => server.capture(self.root()),
            DisplayServer::X11(_) => Err(Error::Unsupported {
                capability: "Wayland output capture",
                detail: "the testbed owns an X11 server".to_owned(),
            }),
        }
    }

    /// Copy one private output into a caller-selected artifact path.
    ///
    /// The copy is performed by the harness, never by the sandboxed app.
    pub fn export(&self, relative: impl AsRef<Path>, destination: impl AsRef<Path>) -> Result<()> {
        let source = self.private_path(relative)?;
        let destination = destination.as_ref();
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io("create artifact directory", parent, err))?;
        }
        fs::copy(&source, destination)
            .map(|_| ())
            .map_err(|err| io("export private artifact", destination, err))
    }

    pub(crate) fn host_path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.path().join(relative)
    }

    pub(crate) fn display_seal(&self) -> &DisplayServer {
        &self.display
    }

    fn export_failure_logs(&self, sink: &Path) -> Result<()> {
        let target = sink.join(&self.id);
        fs::create_dir_all(&target)
            .map_err(|err| io("create failure artifact directory", &target, err))?;
        for entry in fs::read_dir(self.host_path("logs"))
            .map_err(|err| io("read private logs", self.host_path("logs"), err))?
        {
            let entry =
                entry.map_err(|err| io("read private log entry", self.host_path("logs"), err))?;
            if entry
                .file_type()
                .map_err(|err| io("inspect private log", entry.path(), err))?
                .is_file()
            {
                let destination = target.join(entry.file_name());
                let _bytes = fs::copy(entry.path(), &destination)
                    .map_err(|err| io("export failure log", destination, err))?;
            }
        }
        Ok(())
    }
}

impl Drop for Testbed {
    fn drop(&mut self) {
        if thread::panicking()
            && let Some(sink) = &self.failure_artifacts
        {
            let _ignored = self.export_failure_logs(sink);
        }
    }
}

#[derive(Debug)]
pub(crate) enum DisplayServer {
    X11(Xvfb),
    Wayland(Weston),
}

pub(crate) trait DisplaySeal {
    fn append_bwrap(&self, args: &mut Vec<OsString>);
    fn append_environment(&self, env: &mut BTreeMap<OsString, OsString>);
}

impl DisplaySeal for DisplayServer {
    fn append_bwrap(&self, args: &mut Vec<OsString>) {
        match self {
            Self::X11(server) => server.append_bwrap(args),
            Self::Wayland(_) => {}
        }
    }

    fn append_environment(&self, env: &mut BTreeMap<OsString, OsString>) {
        match self {
            Self::X11(server) => server.append_environment(env),
            Self::Wayland(server) => server.append_environment(env),
        }
    }
}

impl DisplayServer {
    fn raise(backend: Backend, root: &Path, id: &str) -> Result<Self> {
        match backend {
            Backend::X11(config) => Xvfb::raise(config, root).map(Self::X11),
            Backend::Wayland(config) => Weston::raise(config, root, id).map(Self::Wayland),
        }
    }
}

pub(crate) struct Xvfb {
    child: Child,
    display: u16,
    cookie: Vec<u8>,
    socket: PathBuf,
}

impl std::fmt::Debug for Xvfb {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Xvfb")
            .field("pid", &self.child.id())
            .field("display", &self.display)
            .finish_non_exhaustive()
    }
}

impl Xvfb {
    fn raise(config: X11Config, root: &Path) -> Result<Self> {
        require_tool("Xvfb")?;
        require_tool("xauth")?;
        let cookie_text = random_cookie()?;
        let cookie = decode_hex(&cookie_text)?;
        let authority = root.join("xauthority");
        let log = fs::File::create(root.join("logs/xvfb.log"))
            .map_err(|err| io("create Xvfb log", root.join("logs/xvfb.log"), err))?;
        for attempt in 0..24 {
            let seed = NEXT_DISPLAY.fetch_add(1, Ordering::Relaxed);
            let display = 200 + ((u64::from(std::process::id()) + seed + attempt) % 700) as u16;
            let display_text = format!(":{display}");
            let _removed = fs::remove_file(&authority);
            let output = Command::new("xauth")
                .args(["-f", authority.to_string_lossy().as_ref(), "add"])
                .arg(&display_text)
                .args(["MIT-MAGIC-COOKIE-1", &cookie_text])
                .output()
                .map_err(|err| io("create X11 authority", "xauth", err))?;
            if !output.status.success() {
                return Err(Error::Command {
                    command: "xauth".to_owned(),
                    status: output.status.to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                });
            }
            harden_file(&authority)?;
            let geometry = format!("{}x{}x{}", config.width, config.height, config.depth);
            let stderr = log
                .try_clone()
                .map_err(|err| io("clone Xvfb log", root.join("logs/xvfb.log"), err))?;
            let mut child = Command::new("Xvfb")
                .args([
                    &display_text,
                    "-screen",
                    "0",
                    &geometry,
                    "-nolisten",
                    "tcp",
                    "-noreset",
                    "-auth",
                ])
                .arg(&authority)
                .stdout(Stdio::from(log.try_clone().map_err(|err| {
                    io("clone Xvfb log", root.join("logs/xvfb.log"), err)
                })?))
                .stderr(Stdio::from(stderr))
                .spawn()
                .map_err(|err| io("spawn Xvfb", "Xvfb", err))?;
            let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
            if wait_x11(&mut child, &socket, &cookie, Duration::from_secs(3))? {
                return Ok(Self {
                    child,
                    display,
                    cookie,
                    socket,
                });
            }
        }
        Err(Error::Containment {
            layer: "Xvfb",
            detail: "could not allocate a private display after 24 attempts".to_owned(),
        })
    }

    fn controller(&self) -> Result<X11Controller> {
        X11Controller::connect(self.display, &self.cookie)
    }

    fn append_bwrap(&self, args: &mut Vec<OsString>) {
        args.extend([
            OsString::from("--dir"),
            OsString::from("/tmp/.X11-unix"),
            OsString::from("--ro-bind"),
            self.socket.as_os_str().to_owned(),
            self.socket.as_os_str().to_owned(),
        ]);
    }

    fn append_environment(&self, env: &mut BTreeMap<OsString, OsString>) {
        env.extend([
            (
                OsString::from("DISPLAY"),
                OsString::from(format!(":{}", self.display)),
            ),
            (
                OsString::from("XAUTHORITY"),
                OsString::from("/test/xauthority"),
            ),
            (OsString::from("WINIT_UNIX_BACKEND"), OsString::from("x11")),
        ]);
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

pub(crate) struct Weston {
    child: Child,
    socket_name: String,
    runtime: WestonRuntime,
}

impl std::fmt::Debug for Weston {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Weston")
            .field("pid", &self.child.id())
            .field("socket", &self.socket_name)
            .finish_non_exhaustive()
    }
}

impl Weston {
    fn raise(config: WaylandConfig, root: &Path, id: &str) -> Result<Self> {
        let runtime = root.join("xdg/runtime");
        let socket_name = format!("egui-tester-{id}");
        let log_path = root.join("logs/weston.log");
        let geometry = format!("{}x{}", config.width, config.height);
        let weston_runtime = WestonRuntime::resolve()?;
        let mut command = Command::new(&weston_runtime.weston);
        let _command = command
            .env_clear()
            .env("PATH", "/usr/bin")
            .env("HOME", root.join("home"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .args([
                "--backend=headless",
                "--renderer=pixman",
                "--no-config",
                "--idle-time=0",
                "--fake-seat",
            ])
            .arg(format!("--socket={socket_name}"))
            .arg(format!("--width={}", config.width))
            .arg(format!("--height={}", config.height))
            .arg(format!("--log={}", log_path.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        weston_runtime.apply_environment(&mut command);
        let mut child = command
            .spawn()
            .map_err(|err| io("spawn Weston", &weston_runtime.weston, err))?;
        let socket = runtime.join(&socket_name);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if socket.exists() {
                return Ok(Self {
                    child,
                    socket_name,
                    runtime: weston_runtime,
                });
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|err| io("poll Weston", "weston", err))?
            {
                return Err(Error::Containment {
                    layer: "Weston",
                    detail: format!(
                        "headless compositor exited with {status}; log: {}",
                        fs::read_to_string(&log_path).unwrap_or_default()
                    ),
                });
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ignored = child.kill();
        let _ignored = child.wait();
        Err(Error::Timeout {
            waiting: format!("Weston socket for {geometry} headless output"),
            timeout: Duration::from_secs(5),
        })
    }

    fn append_environment(&self, env: &mut BTreeMap<OsString, OsString>) {
        env.extend([
            (
                OsString::from("WAYLAND_DISPLAY"),
                OsString::from(&self.socket_name),
            ),
            (
                OsString::from("WINIT_UNIX_BACKEND"),
                OsString::from("wayland"),
            ),
        ]);
    }

    fn capture(&self, root: &Path) -> Result<crate::Frame> {
        let directory = root.join("captures/wayland");
        let _removed = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory)
            .map_err(|err| io("create Wayland capture directory", &directory, err))?;
        let mut command = Command::new(&self.runtime.screenshooter);
        let _command = command
            .env_clear()
            .env("PATH", "/usr/bin")
            .env("XDG_RUNTIME_DIR", root.join("xdg/runtime"))
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .current_dir(&directory);
        self.runtime.apply_environment(&mut command);
        let output = command.output().map_err(|err| {
            io(
                "run Weston output capture",
                &self.runtime.screenshooter,
                err,
            )
        })?;
        if !output.status.success() {
            return Err(Error::Command {
                command: "weston-screenshooter".to_owned(),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let captures = fs::read_dir(&directory)
            .map_err(|err| io("read Wayland captures", &directory, err))?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
            .collect::<Vec<_>>();
        let [capture] = captures.as_slice() else {
            return Err(Error::X11 {
                operation: "capture Wayland output",
                detail: format!(
                    "expected one PNG from one virtual output, found {}",
                    captures.len()
                ),
            });
        };
        crate::Frame::load_png(capture)
    }
}

#[derive(Debug)]
struct WestonRuntime {
    weston: PathBuf,
    screenshooter: PathBuf,
    root: Option<PathBuf>,
}

impl WestonRuntime {
    fn resolve() -> Result<Self> {
        if let Some(root) = std::env::var_os("EGUI_TESTER_WESTON_ROOT").map(PathBuf::from) {
            let weston = root.join("usr/bin/weston");
            let screenshooter = root.join("usr/bin/weston-screenshooter");
            if weston.is_file() && screenshooter.is_file() {
                return Ok(Self {
                    weston,
                    screenshooter,
                    root: Some(root),
                });
            }
            return Err(Error::Unsupported {
                capability: "headless Wayland",
                detail: format!(
                    "EGUI_TESTER_WESTON_ROOT `{}` lacks Weston executables",
                    root.display()
                ),
            });
        }
        Ok(Self {
            weston: find_tool("weston").ok_or(Error::MissingTool("weston"))?,
            screenshooter: find_tool("weston-screenshooter")
                .ok_or(Error::MissingTool("weston-screenshooter"))?,
            root: None,
        })
    }

    fn apply_environment(&self, command: &mut Command) {
        let Some(root) = &self.root else {
            return;
        };
        let weston_lib = root.join("usr/lib/weston");
        let backend_lib = root.join("usr/lib/libweston-15");
        let module =
            |name: &str, directory: &Path| format!("{name}={}", directory.join(name).display());
        let map = [
            module("headless-backend.so", &backend_lib),
            module("desktop-shell.so", &weston_lib),
            module("weston-desktop-shell", &weston_lib),
            module("weston-keyboard", &weston_lib),
            module("weston-simple-im", &weston_lib),
        ]
        .join(";");
        let _command = command
            .env(
                "LD_LIBRARY_PATH",
                format!(
                    "{}:{}",
                    root.join("usr/lib").display(),
                    weston_lib.display()
                ),
            )
            .env("WESTON_MODULE_MAP", map)
            .env("WESTON_DATA_DIR", root.join("usr/share"));
    }
}

impl Drop for Weston {
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

fn wait_x11(child: &mut Child, socket: &Path, cookie: &[u8], timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|err| io("poll Xvfb", "Xvfb", err))?
            .is_some()
        {
            return Ok(false);
        }
        if socket.exists() && connect_authenticated(socket, cookie).map(|_| ()).is_ok() {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(15));
    }
    let _ignored = child.kill();
    let _ignored = child.wait();
    Ok(false)
}

fn require_tool(name: &'static str) -> Result<()> {
    if find_tool(name).is_some() {
        Ok(())
    } else {
        Err(Error::MissingTool(name))
    }
}

fn find_tool(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn random_cookie() -> Result<String> {
    let path = Path::new("/proc/sys/kernel/random/uuid");
    let uuid = fs::read_to_string(path).map_err(|err| io("read kernel random UUID", path, err))?;
    let cookie = uuid.trim().replace('-', "");
    if cookie.len() == 32 && cookie.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(cookie)
    } else {
        Err(Error::Containment {
            layer: "X11 authority",
            detail: "kernel UUID did not yield a 128-bit hexadecimal cookie".to_owned(),
        })
    }
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                .ok_or_else(|| Error::Containment {
                    layer: "X11 authority",
                    detail: "generated cookie is not hexadecimal".to_owned(),
                })
        })
        .collect()
}

fn validate_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::Containment {
            layer: "private filesystem",
            detail: format!("`{}` is not a confined relative path", path.display()),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn harden_root(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|err| io("harden private directory", path, err))
}

#[cfg(unix)]
fn harden_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|err| io("harden private file", path, err))
}
