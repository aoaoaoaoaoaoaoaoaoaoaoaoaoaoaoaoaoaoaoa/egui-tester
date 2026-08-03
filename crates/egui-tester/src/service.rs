use std::{
    cell::Cell,
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::fs::FileTypeExt as _,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Error, FrameProbe, Probe, Result,
    error::io,
    testbed::{DisplaySeal, Testbed},
};

const GUEST_ROOT: &str = "/test";

/// Application network authority.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Network {
    /// A private network namespace with no interfaces.
    #[default]
    Deny,
    /// Share the host network namespace.
    Host,
}

/// Graphics device authority.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Graphics {
    /// A pinned, read-only lavapipe runtime with a synthetic `/dev`.
    #[default]
    Software,
    /// Host GPU devices plus read-only sysfs, for representative performance.
    Host,
}

/// A black-box application launch specification.
#[derive(Clone, Debug)]
pub struct AppCommand {
    binary: PathBuf,
    args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    borrows: Vec<PathBuf>,
    violations: Vec<String>,
    network: Network,
    graphics: Graphics,
    runtime: Duration,
    witness: Option<PathBuf>,
}

impl AppCommand {
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            borrows: Vec::new(),
            violations: Vec::new(),
            network: Network::Deny,
            graphics: Graphics::Software,
            runtime: Duration::from_mins(2),
            witness: None,
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set a non-reserved environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        let _previous = self.env.insert(key.into(), value.into());
        self
    }

    /// Set an environment variable to a path inside the disposable test root.
    #[must_use]
    pub fn private_env(mut self, key: impl Into<OsString>, relative: impl AsRef<Path>) -> Self {
        let relative = relative.as_ref();
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            self.violations.push(format!(
                "private environment path `{}` is not confined",
                relative.display()
            ));
        }
        let path = Path::new(GUEST_ROOT).join(relative);
        let _previous = self.env.insert(key.into(), path.into_os_string());
        self
    }

    /// Reveal one live host path read-only at the same absolute path.
    ///
    /// There is intentionally no writable counterpart.
    #[must_use]
    pub fn borrow_read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.borrows.push(path.into());
        self
    }

    #[must_use]
    pub fn network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    #[must_use]
    pub fn graphics(mut self, graphics: Graphics) -> Self {
        self.graphics = graphics;
        self
    }

    #[must_use]
    pub fn runtime(mut self, runtime: Duration) -> Self {
        self.runtime = runtime;
        self
    }

    /// Arm the standard one-way witness at a private `/test` path.
    #[must_use]
    pub fn witness(mut self, relative: impl AsRef<Path>) -> Self {
        let relative = relative.as_ref();
        if !confined_relative(relative) {
            self.violations.push(format!(
                "witness path `{}` is not confined",
                relative.display()
            ));
        }
        self.witness = Some(relative.to_owned());
        self
    }
}

/// Exit facts retained by the transient service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Exit {
    pub code: i32,
    pub result: String,
    pub stdout: String,
    pub stderr: String,
}

impl Exit {
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == 0 && self.result == "success"
    }
}

/// One application cgroup.
pub struct Application<'a> {
    unit: String,
    stdout: PathBuf,
    stderr: PathBuf,
    witness: Option<WitnessSeal>,
    stopped: Cell<bool>,
    liveness: Cell<Option<(Instant, bool)>>,
    testbed: &'a Testbed,
}

impl std::fmt::Debug for Application<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Application")
            .field("unit", &self.unit)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .field("witness", &self.witness)
            .finish_non_exhaustive()
    }
}

impl<'a> Application<'a> {
    pub(crate) fn raise(testbed: &'a Testbed, command: AppCommand, ordinal: u64) -> Result<Self> {
        validate_command(&command)?;
        let binary = command
            .binary
            .canonicalize()
            .map_err(|err| io("resolve application binary", &command.binary, err))?;
        if !binary.is_file() {
            return Err(Error::Containment {
                layer: "launcher",
                detail: format!("application binary `{}` is not a file", binary.display()),
            });
        }
        let borrows = command
            .borrows
            .iter()
            .map(|path| {
                path.canonicalize()
                    .map(|source| ReadOnlyMount {
                        source,
                        guest: path.clone(),
                    })
                    .map_err(|err| io("resolve read-only borrow", path, err))
            })
            .collect::<Result<Vec<_>>>()?;
        let unit = format!("egui-tester-{}-{ordinal}", testbed.id());
        let stdout = testbed.host_path(format!("logs/app-{ordinal}.stdout"));
        let stderr = testbed.host_path(format!("logs/app-{ordinal}.stderr"));
        create_empty(&stdout)?;
        create_empty(&stderr)?;
        let witness = command.witness.as_ref().map(|relative| WitnessSeal {
            host: testbed.host_path(relative),
            guest: Path::new(GUEST_ROOT).join(relative),
            frame_host: testbed.host_path(relative.with_extension("frames")),
            frame_guest: Path::new(GUEST_ROOT).join(relative.with_extension("frames")),
            launch: format!("{}-{ordinal}", testbed.id()),
        });
        if let Some(witness) = &witness {
            if let Some(parent) = witness.host.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| io("create witness parent", parent, err))?;
            }
            remove_stale(&witness.host, "remove stale witness")?;
            remove_stale(&witness.frame_host, "remove stale frame journal")?;
        }

        let bwrap = bwrap_argv(testbed, &command, &binary, &borrows, witness.as_ref())?;
        let mut systemd = testbed.user_command("systemd-run");
        let _command = systemd
            .args([
                "--user",
                "--remain-after-exit",
                "--service-type=exec",
                "--unit",
                &unit,
                "--property=KillMode=control-group",
                "--property=SendSIGKILL=yes",
                "--property=TimeoutStopSec=2s",
                "--property=ProtectSystem=strict",
                "--property=ProtectHome=read-only",
                "--property=NoNewPrivileges=yes",
                "--property=RestrictSUIDSGID=yes",
                "--property=LockPersonality=yes",
                "--property=RestrictRealtime=yes",
                "--property=ProtectKernelModules=yes",
                "--property=ProtectControlGroups=yes",
                "--property=ProtectClock=yes",
                "--property=SystemCallArchitectures=native",
                "--property=UMask=0077",
            ])
            .arg(format!(
                "--property=RuntimeMaxSec={}s",
                command
                    .runtime
                    .as_secs()
                    .saturating_add(u64::from(command.runtime.subsec_nanos() != 0))
            ))
            .arg(format!(
                "--property=ReadWritePaths={}",
                testbed.root().display()
            ))
            .arg(format!(
                "--property=StandardOutput=append:{}",
                stdout.display()
            ))
            .arg(format!(
                "--property=StandardError=append:{}",
                stderr.display()
            ));
        if command.network == Network::Deny {
            let _command = systemd.args([
                "--property=PrivateNetwork=yes",
                "--property=IPAddressDeny=any",
            ]);
        }
        let _command = systemd.arg("--").args(bwrap);
        let output = systemd
            .output()
            .map_err(|err| io("spawn transient application service", "systemd-run", err))?;
        if !output.status.success() {
            return Err(Error::Containment {
                layer: "systemd",
                detail: command_failure(&systemd, &output),
            });
        }
        let app = Self {
            unit,
            stdout,
            stderr,
            witness,
            stopped: Cell::new(false),
            liveness: Cell::new(None),
            testbed,
        };
        Ok(app)
    }

    #[must_use]
    pub fn unit(&self) -> &str {
        &self.unit
    }

    #[must_use]
    pub fn stdout_path(&self) -> &Path {
        &self.stdout
    }

    #[must_use]
    pub fn stderr_path(&self) -> &Path {
        &self.stderr
    }

    pub fn witness(&self) -> Result<Probe> {
        let seal = self.witness.as_ref().ok_or_else(|| Error::Unsupported {
            capability: "standard witness",
            detail: "launch the application with AppCommand::witness".to_owned(),
        })?;
        Ok(Probe::sealed(&seal.host, &seal.launch))
    }

    pub fn frames(&self) -> Result<FrameProbe> {
        let seal = self.witness.as_ref().ok_or_else(|| Error::Unsupported {
            capability: "standard frame journal",
            detail: "launch the application with AppCommand::witness".to_owned(),
        })?;
        Ok(FrameProbe::sealed(&seal.frame_host, &seal.launch))
    }

    #[must_use]
    pub fn witness_path(&self) -> Option<&Path> {
        self.witness.as_ref().map(|seal| seal.host.as_path())
    }

    pub fn ensure_running(&self, waiting: impl Into<String>) -> Result<()> {
        let waiting = waiting.into();
        if self.liveness.get().is_some_and(|(checked, running)| {
            running && checked.elapsed() < Duration::from_millis(100)
        }) {
            return Ok(());
        }
        let status = self.status()?;
        let running = status.sub_state == "running";
        self.liveness.set(Some((Instant::now(), running)));
        if running {
            return Ok(());
        }
        Err(Error::ApplicationExited {
            unit: self.unit.clone(),
            waiting,
            detail: format!(
                "active={}, sub={}, result={}, code={}; stderr: {}",
                status.active_state,
                status.sub_state,
                status.result,
                status.code,
                tail(&self.stderr, 4096)
            ),
        })
    }

    pub fn wait(&self, timeout: Duration) -> Result<Exit> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status()?;
            if status.sub_state != "running" && status.sub_state != "start" {
                return Ok(Exit {
                    code: status.code,
                    result: status.result,
                    stdout: read_lossy(&self.stdout),
                    stderr: read_lossy(&self.stderr),
                });
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: format!("application unit `{}` to exit", self.unit),
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Poll an external product predicate while continuously proving the app
    /// has not exited.
    pub fn wait_until(
        &self,
        timeout: Duration,
        description: impl Into<String>,
        mut predicate: impl FnMut() -> Result<bool>,
    ) -> Result<()> {
        let description = description.into();
        let deadline = Instant::now() + timeout;
        loop {
            self.ensure_running(&description)?;
            if predicate()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    waiting: description,
                    timeout,
                });
            }
            thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn terminate(&self) -> Result<()> {
        if self.stopped.get() {
            return Ok(());
        }
        let output = self
            .testbed
            .user_command("systemctl")
            .args(["--user", "stop", &self.unit])
            .output()
            .map_err(|err| io("stop application service", "systemctl", err))?;
        if !output.status.success() {
            return Err(Error::Containment {
                layer: "systemd",
                detail: format!(
                    "could not stop `{}`: {}",
                    self.unit,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        reset_unit(self.testbed, &self.unit);
        self.stopped.set(true);
        self.liveness.set(Some((Instant::now(), false)));
        Ok(())
    }

    fn status(&self) -> Result<UnitStatus> {
        let output = self
            .testbed
            .user_command("systemctl")
            .args([
                "--user",
                "show",
                &self.unit,
                "--property=ActiveState",
                "--property=SubState",
                "--property=Result",
                "--property=ExecMainStatus",
            ])
            .output()
            .map_err(|err| io("query application service", "systemctl", err))?;
        if !output.status.success() {
            return Err(Error::Containment {
                layer: "systemd",
                detail: format!(
                    "could not query `{}`: {}",
                    self.unit,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        UnitStatus::parse(&String::from_utf8_lossy(&output.stdout))
    }
}

impl Drop for Application<'_> {
    fn drop(&mut self) {
        if !self.stopped.get() {
            let _ignored = self
                .testbed
                .user_command("systemctl")
                .args(["--user", "stop", &self.unit])
                .status();
            reset_unit(self.testbed, &self.unit);
        }
    }
}

#[derive(Debug)]
struct UnitStatus {
    active_state: String,
    sub_state: String,
    result: String,
    code: i32,
}

impl UnitStatus {
    fn parse(text: &str) -> Result<Self> {
        let fields = text
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect::<BTreeMap<_, _>>();
        let get = |name| {
            fields.get(name).copied().ok_or_else(|| Error::Containment {
                layer: "systemd",
                detail: format!("unit status omitted `{name}`"),
            })
        };
        Ok(Self {
            active_state: get("ActiveState")?.to_owned(),
            sub_state: get("SubState")?.to_owned(),
            result: get("Result")?.to_owned(),
            code: get("ExecMainStatus")?
                .parse()
                .map_err(|err| Error::Containment {
                    layer: "systemd",
                    detail: format!("invalid ExecMainStatus: {err}"),
                })?,
        })
    }
}

fn bwrap_argv(
    testbed: &Testbed,
    command: &AppCommand,
    binary: &Path,
    borrows: &[ReadOnlyMount],
    witness: Option<&WitnessSeal>,
) -> Result<Vec<OsString>> {
    let mut args = [
        "/usr/bin/bwrap",
        "--die-with-parent",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/etc",
        "/etc",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--proc",
        "/proc",
        "--tmpfs",
        "/run",
        "--dir",
        "/tmp",
        "--bind",
    ]
    .map(OsString::from)
    .to_vec();
    args.extend([
        testbed.root().as_os_str().to_owned(),
        OsString::from(GUEST_ROOT),
        OsString::from("--dir"),
        OsString::from("/app"),
        OsString::from("--ro-bind"),
        binary.as_os_str().to_owned(),
        OsString::from("/app/application"),
    ]);
    match command.graphics {
        Graphics::Software => {
            let lavapipe = lavapipe_root()?;
            args.extend([
                OsString::from("--dev"),
                OsString::from("/dev"),
                OsString::from("--dir"),
                OsString::from("/opt"),
                OsString::from("--dir"),
                OsString::from("/opt/egui-tester"),
                OsString::from("--ro-bind"),
                lavapipe.into_os_string(),
                OsString::from("/opt/egui-tester/lavapipe"),
            ]);
        }
        Graphics::Host => {
            args.extend([OsString::from("--dev"), OsString::from("/dev")]);
            let devices = host_graphics_devices()?;
            append_device_parent_dirs(&mut args, &devices);
            for device in devices {
                args.extend([
                    OsString::from("--dev-bind"),
                    device.as_os_str().to_owned(),
                    device.into_os_string(),
                ]);
            }
            args.extend([
                OsString::from("--ro-bind"),
                OsString::from("/sys"),
                OsString::from("/sys"),
            ]);
        }
    }
    if command.network == Network::Deny {
        args.push(OsString::from("--unshare-net"));
    }
    testbed.display_seal().append_bwrap(&mut args);
    for borrow in borrows {
        append_parent_dirs(&mut args, &borrow.guest)?;
        args.extend([
            OsString::from("--ro-bind"),
            borrow.source.as_os_str().to_owned(),
            borrow.guest.as_os_str().to_owned(),
        ]);
    }
    let environment = sealed_environment(testbed, command, witness);
    for (key, value) in environment {
        args.extend([OsString::from("--setenv"), key, value]);
    }
    args.extend([
        OsString::from("--chdir"),
        OsString::from("/test/home"),
        OsString::from("--"),
        OsString::from("/app/application"),
    ]);
    args.extend(command.args.iter().cloned());
    Ok(args)
}

fn sealed_environment(
    testbed: &Testbed,
    command: &AppCommand,
    witness: Option<&WitnessSeal>,
) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::from([
        (OsString::from("PATH"), OsString::from("/usr/bin")),
        (OsString::from("HOME"), OsString::from("/test/home")),
        (
            OsString::from("XDG_CONFIG_HOME"),
            OsString::from("/test/xdg/config"),
        ),
        (
            OsString::from("XDG_CACHE_HOME"),
            OsString::from("/test/xdg/cache"),
        ),
        (
            OsString::from("XDG_DATA_HOME"),
            OsString::from("/test/xdg/data"),
        ),
        (
            OsString::from("XDG_STATE_HOME"),
            OsString::from("/test/xdg/state"),
        ),
        (
            OsString::from("XDG_RUNTIME_DIR"),
            OsString::from("/test/xdg/runtime"),
        ),
        (OsString::from("TMPDIR"), OsString::from("/test/tmp")),
        (OsString::from("LANG"), OsString::from("C.UTF-8")),
        (OsString::from("LC_ALL"), OsString::from("C.UTF-8")),
        (OsString::from("TZ"), OsString::from("UTC")),
        (OsString::from("RUST_BACKTRACE"), OsString::from("1")),
    ]);
    testbed.display_seal().append_environment(&mut env);
    if command.graphics == Graphics::Software {
        env.extend([
            (
                OsString::from("LD_LIBRARY_PATH"),
                OsString::from("/opt/egui-tester/lavapipe/usr/lib"),
            ),
            (
                OsString::from("VK_ICD_FILENAMES"),
                OsString::from("/opt/egui-tester/lavapipe/usr/share/vulkan/icd.d/lvp_icd.json"),
            ),
            (OsString::from("LIBGL_ALWAYS_SOFTWARE"), OsString::from("1")),
        ]);
    }
    if let Some(witness) = witness {
        env.extend([
            (
                OsString::from(egui_tester_witness::PATH_ENV),
                witness.guest.as_os_str().to_owned(),
            ),
            (
                OsString::from(egui_tester_witness::LAUNCH_ENV),
                OsString::from(&witness.launch),
            ),
            (
                OsString::from(egui_tester_witness::FRAMES_ENV),
                witness.frame_guest.as_os_str().to_owned(),
            ),
        ]);
    }
    env.extend(command.env.clone());
    env
}

fn lavapipe_root() -> Result<PathBuf> {
    let configured = std::env::var_os("EGUI_TESTER_LAVAPIPE").map(PathBuf::from);
    let conventional = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/share/x11-gui-testing/lavapipe"));
    configured
        .into_iter()
        .chain(conventional)
        .find(|root| {
            root.join("usr/lib/libvulkan_lvp.so").is_file()
                && root
                    .join("usr/share/vulkan/icd.d/lvp_icd.json")
                    .is_file()
        })
        .ok_or_else(|| Error::Unsupported {
            capability: "software graphics",
            detail: "lavapipe was not found; set EGUI_TESTER_LAVAPIPE or install the x11-gui-testing lavapipe runtime".to_owned(),
        })
}

fn validate_command(command: &AppCommand) -> Result<()> {
    const RESERVED: &[&str] = &[
        "PATH",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "WINIT_UNIX_BACKEND",
        "HOME",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "DBUS_SYSTEM_BUS_ADDRESS",
        "LANG",
        "LC_ALL",
        "TZ",
        "RUST_BACKTRACE",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "LD_AUDIT",
        "VK_ICD_FILENAMES",
        "VK_DRIVER_FILES",
        "VK_ADD_DRIVER_FILES",
        "LIBGL_ALWAYS_SOFTWARE",
        "LIBGL_DRIVERS_PATH",
        "MESA_LOADER_DRIVER_OVERRIDE",
        "WGPU_BACKEND",
        "WGPU_POWER_PREF",
        "__GLX_VENDOR_LIBRARY_NAME",
        "DRI_PRIME",
        egui_tester_witness::PATH_ENV,
        egui_tester_witness::LAUNCH_ENV,
        egui_tester_witness::FRAMES_ENV,
    ];
    if let Some(detail) = command.violations.first() {
        return Err(Error::Containment {
            layer: "private environment",
            detail: detail.clone(),
        });
    }
    for key in command.env.keys() {
        if RESERVED.iter().any(|reserved| key == OsStr::new(reserved)) {
            return Err(Error::Containment {
                layer: "environment seal",
                detail: format!(
                    "reserved environment variable `{}` cannot be overridden",
                    key.to_string_lossy()
                ),
            });
        }
    }
    for borrow in &command.borrows {
        if !borrow.is_absolute()
            || borrow
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        {
            return Err(Error::Containment {
                layer: "mount namespace",
                detail: format!(
                    "read-only borrow `{}` is not a normalized absolute path",
                    borrow.display()
                ),
            });
        }
        for forbidden in ["/app", "/dev", "/proc", "/run", "/sys", "/test"] {
            if borrow.starts_with(forbidden) {
                return Err(Error::Containment {
                    layer: "mount namespace",
                    detail: format!(
                        "read-only borrow `{}` overlaps reserved guest root `{forbidden}`",
                        borrow.display()
                    ),
                });
            }
        }
        if borrow.starts_with("/tmp/.X11-unix") {
            return Err(Error::Containment {
                layer: "mount namespace",
                detail: "X11 sockets are owned exclusively by the display seal".to_owned(),
            });
        }
    }
    if command.runtime.is_zero() {
        return Err(Error::Containment {
            layer: "systemd",
            detail: "application runtime must be nonzero".to_owned(),
        });
    }
    Ok(())
}

fn host_graphics_devices() -> Result<Vec<PathBuf>> {
    let mut devices = Vec::new();
    for root in [Path::new("/dev/dri"), Path::new("/dev/nvidia-caps")] {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(|error| io("enumerate graphics devices", root, error))?;
            let metadata = entry
                .metadata()
                .map_err(|error| io("inspect graphics device", entry.path(), error))?;
            if metadata.file_type().is_char_device() {
                devices.push(entry.path());
            }
        }
    }
    let dev = Path::new("/dev");
    let entries =
        std::fs::read_dir(dev).map_err(|error| io("enumerate graphics devices", dev, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io("enumerate graphics devices", dev, error))?;
        let name = entry.file_name();
        if (name == "kfd" || nvidia_device(&name))
            && entry
                .file_type()
                .map_err(|error| io("inspect graphics device", entry.path(), error))?
                .is_char_device()
        {
            devices.push(entry.path());
        }
    }
    devices.sort();
    devices.dedup();
    if devices.is_empty() {
        return Err(Error::Unsupported {
            capability: "host graphics",
            detail: "no DRM, KFD, or NVIDIA graphics devices are accessible".to_owned(),
        });
    }
    Ok(devices)
}

fn nvidia_device(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        matches!(
            name,
            "nvidiactl" | "nvidia-modeset" | "nvidia-uvm" | "nvidia-uvm-tools"
        ) || name
            .strip_prefix("nvidia")
            .is_some_and(|slot| !slot.is_empty() && slot.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

fn append_device_parent_dirs(args: &mut Vec<OsString>, devices: &[PathBuf]) {
    let mut parents = devices
        .iter()
        .filter_map(|device| device.parent())
        .filter(|parent| *parent != Path::new("/dev"))
        .map(Path::to_owned)
        .collect::<Vec<_>>();
    parents.sort();
    parents.dedup();
    for parent in parents {
        args.extend([OsString::from("--dir"), parent.as_os_str().to_owned()]);
    }
}

struct ReadOnlyMount {
    source: PathBuf,
    guest: PathBuf,
}

fn append_parent_dirs(args: &mut Vec<OsString>, path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| Error::Containment {
        layer: "mount namespace",
        detail: format!("borrow `{}` has no parent", path.display()),
    })?;
    let mut built = PathBuf::from("/");
    for component in parent.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                built.push(part);
                args.extend([OsString::from("--dir"), built.as_os_str().to_owned()]);
            }
            _ => {
                return Err(Error::Containment {
                    layer: "mount namespace",
                    detail: format!(
                        "borrow `{}` contains a non-normal component",
                        path.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

fn create_empty(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io("create log directory", parent, err))?;
    }
    std::fs::File::create(path)
        .map(|_| ())
        .map_err(|err| io("create service log", path, err))
}

fn remove_stale(path: &Path, operation: &'static str) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io(operation, path, err)),
    }
}

fn command_failure(command: &Command, output: &Output) -> String {
    format!(
        "`{:?}` returned {}; stdout: {}; stderr: {}",
        command,
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn read_lossy(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|err| format!("<could not read {}: {err}>", path.display()))
}

fn tail(path: &Path, limit: usize) -> String {
    let text = read_lossy(path);
    let start = text.floor_char_boundary(text.len().saturating_sub(limit));
    text[start..].trim().to_owned()
}

fn reset_unit(testbed: &Testbed, unit: &str) {
    let _ignored = testbed
        .user_command("systemctl")
        .args(["--user", "reset-failed", unit])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[derive(Clone, Debug)]
pub(crate) struct UserBus {
    runtime: OsString,
    address: OsString,
}

impl UserBus {
    pub(crate) fn discover() -> Result<Self> {
        let runtime = PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()));
        let metadata = std::fs::metadata(&runtime)
            .map_err(|err| io("inspect user runtime directory", &runtime, err))?;
        if !metadata.is_dir() {
            return Err(Error::Containment {
                layer: "systemd user manager",
                detail: format!("XDG runtime `{}` is not a directory", runtime.display()),
            });
        }
        let bus = runtime.join("bus");
        let metadata =
            std::fs::metadata(&bus).map_err(|err| io("inspect user session bus", &bus, err))?;
        if !metadata.file_type().is_socket() {
            return Err(Error::Containment {
                layer: "systemd user manager",
                detail: format!("canonical user bus `{}` is not a socket", bus.display()),
            });
        }
        let address = OsString::from(format!("unix:path={}", bus.display()));
        let user_bus = Self {
            runtime: runtime.into_os_string(),
            address,
        };
        user_bus.verify()?;
        Ok(user_bus)
    }

    pub(crate) fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program.as_ref());
        let _command = command
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address);
        command
    }

    fn verify(&self) -> Result<()> {
        let mut command = self.command("systemctl");
        let output = command
            .args(["--user", "show-environment"])
            .output()
            .map_err(|err| io("contact systemd user manager", "systemctl", err))?;
        if output.status.success() {
            return Ok(());
        }
        Err(Error::Containment {
            layer: "systemd user manager",
            detail: format!(
                "canonical user bus exists but `systemctl --user` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }
}

#[derive(Clone, Debug)]
struct WitnessSeal {
    host: PathBuf,
    guest: PathBuf,
    frame_host: PathBuf,
    frame_guest: PathBuf,
    launch: String,
}

fn confined_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_allowlist_admits_arbitrary_nvidia_slots_only() {
        for admitted in [
            "nvidia0",
            "nvidia27",
            "nvidiactl",
            "nvidia-modeset",
            "nvidia-uvm",
            "nvidia-uvm-tools",
        ] {
            assert!(nvidia_device(OsStr::new(admitted)), "{admitted}");
        }
        for rejected in ["nvidia", "nvidia2x", "nvidia-smi", "nvme0"] {
            assert!(!nvidia_device(OsStr::new(rejected)), "{rejected}");
        }
    }
}
