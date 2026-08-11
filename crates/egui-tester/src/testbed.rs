use std::{
    cell::RefCell,
    collections::BTreeMap,
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::{Read as _, Write as _},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use crate::{
    AppCommand, Application, Error, Result, WindowQuery, X11Controller, X11Session, error::io,
    service::UserBus, x11::connect_authenticated,
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

    /// Directory into which the harness copies curated diagnostics on failure.
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
        let user_bus = UserBus::discover()?;
        let ordinal = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        let nonce = random_cookie()?;
        let id = format!("{}-{ordinal}-{}", std::process::id(), &nonce[..12]);
        let root = tempfile::Builder::new()
            .prefix(&format!("egui-tester-{id}-"))
            .tempdir_in("/tmp")
            .map_err(|err| io("create private test root", "/tmp", err))?;
        harden_root(root.path())?;
        for relative in [
            "home",
            "tmp",
            "logs",
            "captures",
            "diagnostics",
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
            retained: RefCell::new(BTreeSet::new()),
            user_bus,
        })
    }

    /// Run a complete scenario, retaining diagnostics for both `Err` and panic.
    pub fn run<T>(self, scenario: impl FnOnce(&Testbed) -> Result<T>) -> Result<T> {
        let testbed = self.raise()?;
        let verdict = catch_unwind(AssertUnwindSafe(|| scenario(&testbed)));
        match verdict {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => {
                testbed.export_failure_best_effort();
                Err(err)
            }
            Err(payload) => resume_unwind(payload),
        }
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
    retained: RefCell<BTreeSet<PathBuf>>,
    user_bus: UserBus,
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

    /// Resolve a path handle inside the owned tree.
    ///
    /// This is appropriate for APIs that must watch a live path. Use
    /// `read_private`, `write_private`, or `export` for oracle I/O: those
    /// capability operations refuse symlinks created by the application.
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
        let relative = relative.as_ref();
        let path = self.private_path(relative)?;
        self.create_private_dirs(relative)?;
        Ok(path)
    }

    pub fn write_private(
        &self,
        relative: impl AsRef<Path>,
        bytes: impl AsRef<[u8]>,
    ) -> Result<PathBuf> {
        let relative = relative.as_ref();
        let path = self.private_path(relative)?;
        let mut output = self.open_private_write(relative)?;
        output
            .write_all(bytes.as_ref())
            .map_err(|err| io("write private file", &path, err))?;
        Ok(path)
    }

    pub fn copy_private(
        &self,
        relative: impl AsRef<Path>,
        source: impl AsRef<Path>,
    ) -> Result<PathBuf> {
        let relative = relative.as_ref();
        let destination = self.private_path(relative)?;
        let mut input = fs::File::open(source.as_ref())
            .map_err(|err| io("open fixture for private copy", source.as_ref(), err))?;
        let mut output = self.open_private_write(relative)?;
        let _bytes = std::io::copy(&mut input, &mut output)
            .map_err(|err| io("copy fixture into private tree", &destination, err))?;
        Ok(destination)
    }

    /// Read one file beneath the private root without following app-created
    /// symlinks or escaping through magic links.
    pub fn read_private(&self, relative: impl AsRef<Path>) -> Result<Vec<u8>> {
        let relative = relative.as_ref();
        let path = self.private_path(relative)?;
        let mut input = self.open_private_read(relative)?;
        let mut bytes = Vec::new();
        let _bytes_read = input
            .read_to_end(&mut bytes)
            .map_err(|err| io("read private file", path, err))?;
        Ok(bytes)
    }

    pub fn read_private_to_string(&self, relative: impl AsRef<Path>) -> Result<String> {
        let relative = relative.as_ref();
        let bytes = self.read_private(relative)?;
        String::from_utf8(bytes).map_err(|error| Error::Containment {
            layer: "private filesystem",
            detail: format!(
                "private file `{}` is not UTF-8: {error}",
                relative.display()
            ),
        })
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

    pub fn x11_session<'app, 'bed>(
        &'bed self,
        app: &'app Application<'bed>,
        query: impl Into<WindowQuery>,
        timeout: Duration,
    ) -> Result<X11Session<'app, 'bed>> {
        let controller = self.x11()?;
        let window = controller.wait_window_query(app, query, timeout)?;
        X11Session::forge(self, app, controller, window)
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
        let relative = relative.as_ref();
        let source = self.private_path(relative)?;
        let destination = destination.as_ref();
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io("create artifact directory", parent, err))?;
        }
        let mut input = self.open_private_read(relative)?;
        let mut output = fs::File::create(destination)
            .map_err(|err| io("create exported artifact", destination, err))?;
        std::io::copy(&mut input, &mut output)
            .map(|_| ())
            .map_err(|err| io("export private artifact", &source, err))
    }

    /// Include one private file or tree in failure diagnostics.
    pub fn retain_on_failure(&self, relative: impl AsRef<Path>) -> Result<()> {
        let relative = relative.as_ref();
        validate_relative(relative)?;
        let _inserted = self.retained.borrow_mut().insert(relative.to_owned());
        Ok(())
    }

    pub(crate) fn host_path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.path().join(relative)
    }

    pub(crate) fn display_seal(&self) -> &DisplayServer {
        &self.display
    }

    pub(crate) fn user_command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        self.user_bus.command(program)
    }

    pub(crate) fn diagnostic_path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.host_path(Path::new("diagnostics").join(relative))
    }

    fn open_private_read(&self, relative: &Path) -> Result<fs::File> {
        validate_relative(relative)?;
        let root = self.open_private_root()?;
        let fd = rustix::fs::openat2(
            &root,
            relative,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
            private_resolve(),
        )
        .map_err(|error| io("open confined private file", relative, error.into()))?;
        Ok(fd.into())
    }

    fn open_private_write(&self, relative: &Path) -> Result<fs::File> {
        validate_relative(relative)?;
        let (parent, leaf) = self.open_private_parent(relative)?;
        let fd = rustix::fs::openat2(
            &parent,
            Path::new(&leaf),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::TRUNC
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            private_resolve(),
        )
        .map_err(|error| io("open confined private output", relative, error.into()))?;
        Ok(fd.into())
    }

    fn create_private_dirs(&self, relative: &Path) -> Result<()> {
        validate_relative(relative)?;
        let mut directory = self.open_private_root()?;
        for component in relative.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            match rustix::fs::mkdirat(
                &directory,
                part,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(io(
                        "create confined private directory",
                        relative,
                        error.into(),
                    ));
                }
            }
            directory = rustix::fs::openat2(
                &directory,
                part,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
                private_resolve(),
            )
            .map_err(|error| io("descend confined private directory", relative, error.into()))?;
        }
        Ok(())
    }

    fn open_private_parent(&self, relative: &Path) -> Result<(std::os::fd::OwnedFd, OsString)> {
        let mut components = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_owned()),
                Component::CurDir => None,
                _ => None,
            })
            .collect::<Vec<_>>();
        let leaf = components.pop().ok_or_else(|| Error::Containment {
            layer: "private filesystem",
            detail: format!("private path `{}` has no filename", relative.display()),
        })?;
        let mut directory = self.open_private_root()?;
        for part in components {
            match rustix::fs::mkdirat(
                &directory,
                &part,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(io("create confined private parent", relative, error.into()));
                }
            }
            directory = rustix::fs::openat2(
                &directory,
                &part,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
                private_resolve(),
            )
            .map_err(|error| io("descend confined private parent", relative, error.into()))?;
        }
        Ok((directory, leaf))
    }

    fn open_private_root(&self) -> Result<std::os::fd::OwnedFd> {
        rustix::fs::open(
            self.root.path(),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            io(
                "open private root capability",
                self.root.path(),
                error.into(),
            )
        })
    }

    fn export_failure_artifacts(&self, sink: &Path) -> Result<()> {
        let target = sink.join(&self.id);
        fs::create_dir_all(&target)
            .map_err(|err| io("create failure artifact directory", &target, err))?;
        for relative in ["logs", "probes", "captures", "diagnostics"]
            .into_iter()
            .map(PathBuf::from)
            .chain(self.retained.borrow().iter().cloned())
        {
            let source = self.host_path(&relative);
            if source.exists() {
                copy_tree(&source, &target.join(relative))?;
            }
        }
        Ok(())
    }

    fn export_failure_best_effort(&self) {
        if let Some(sink) = &self.failure_artifacts
            && let Err(err) = self.export_failure_artifacts(sink)
        {
            eprintln!("egui-tester could not retain failure artifacts: {err}");
        }
    }
}

impl Drop for Testbed {
    fn drop(&mut self) {
        if thread::panicking() {
            self.export_failure_best_effort();
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
        let fake_seat = weston_runtime.supports_fake_seat()?;
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
                // Screenshooter is gated behind Weston's debug protocol. This
                // compositor owns a unique socket beneath a mode-0700 root.
                "--debug",
            ])
            .arg(format!("--socket={socket_name}"))
            .arg(format!("--width={}", config.width))
            .arg(format!("--height={}", config.height))
            .arg(format!("--log={}", log_path.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if fake_seat {
            let _command = command.arg("--fake-seat");
        }
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
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !output.status.success() || !stderr.is_empty() {
            return Err(Error::Command {
                command: "weston-screenshooter".to_owned(),
                status: output.status.to_string(),
                stderr,
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

    fn supports_fake_seat(&self) -> Result<bool> {
        let mut command = Command::new(&self.weston);
        let _command = command.arg("--help").env_clear().env("PATH", "/usr/bin");
        self.apply_environment(&mut command);
        let output = command
            .output()
            .map_err(|err| io("inspect Weston capabilities", &self.weston, err))?;
        if !output.status.success() {
            return Err(Error::Command {
                command: format!("{} --help", self.weston.display()),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok([output.stdout.as_slice(), output.stderr.as_slice()]
            .into_iter()
            .any(|help| {
                help.split(|byte| byte.is_ascii_whitespace())
                    .any(|word| word == b"--fake-seat")
            }))
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

fn private_resolve() -> rustix::fs::ResolveFlags {
    rustix::fs::ResolveFlags::BENEATH
        | rustix::fs::ResolveFlags::NO_MAGICLINKS
        | rustix::fs::ResolveFlags::NO_SYMLINKS
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

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(source).map_err(|err| io("inspect failure artifact", source, err))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Containment {
            layer: "failure artifacts",
            detail: format!("refusing to follow symlink `{}`", source.display()),
        });
    }
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io("create failure artifact parent", parent, err))?;
        }
        let _bytes = fs::copy(source, destination)
            .map_err(|err| io("copy failure artifact", destination, err))?;
        return Ok(());
    }
    fs::create_dir_all(destination)
        .map_err(|err| io("create failure artifact tree", destination, err))?;
    for entry in
        fs::read_dir(source).map_err(|err| io("read failure artifact tree", source, err))?
    {
        let entry = entry.map_err(|err| io("read failure artifact entry", source, err))?;
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}
