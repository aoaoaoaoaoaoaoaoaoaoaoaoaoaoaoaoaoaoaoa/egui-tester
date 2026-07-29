# egui-tester

`egui-tester` is a hermetic black-box harness for native egui applications. It
launches the real executable, injects display-server input, observes real
pixels, and owns the surrounding desktop state. Named widget probes may locate
targets and synchronize frames; they do not decide whether the product worked.

The name is intentionally utilitarian.

## Present Surface

The X11 backend is the complete MVP:

- private authenticated Xvfb, never the caller's `DISPLAY`
- XTEST pointer, button, wheel, and Latin-1 keyboard input from Rust
- window discovery, focus, RGBA capture, PNG artifacts
- bounded waits for windows, witness predicates, external effects, pixel
  change, and pixel quiescence
- atomic JSON witness compatibility for booru's present `devtools` probe

The Wayland backend owns a headless Weston compositor and captures its virtual
output through `weston-screenshooter`. Launch-and-pixel smoke tests therefore
work on an X11 workstation. Generic real input is not yet claimed: Wayland
intentionally has no XTEST-like client protocol, so that requires a
compositor-side test input facility.

## Containment

The application runs as a transient `systemd --user` service. The service owns
the entire descendant cgroup and applies `ProtectSystem=strict`,
`ProtectHome=read-only`, `NoNewPrivileges`, a runtime deadline, network denial,
and kernel/control-group hardening.

Bubblewrap independently constructs the process, mount, IPC, UTS, and,
normally, network namespaces. The visible runtime is a read-only `/usr` and
`/etc`, the read-only application binary, a synthetic `/dev`, and `/test`.
`/test` contains private `HOME`, every XDG root, `TMPDIR`, probes, and logs; it
is deleted with the testbed.

Undeclared host data is invisible. `AppCommand::borrow_read_only(path)` is the
sole data aperture and mounts the same absolute path read-only. There is no
writable counterpart. Artifact destinations are never mounted into the app:
the harness captures or copies selected outputs from outside containment.

`Graphics::Software` is deterministic and mounts a pinned lavapipe runtime
read-only. `Graphics::Host` exposes host GPU devices and read-only sysfs for
representative performance runs; it still grants no writable host files.

## Model

A **witness** answers “where is the control?” or “has a newer frame been
built?” A witness may be an AccessKit tree, booru's JSON probe, or a startup
marker.

An **oracle** answers “did the product work?” Oracles are captured pixels,
files the product emitted into private state, process exits, or externally
observable protocol effects. Tests should not adjudicate success from a
witness state that merely mirrors an implementation field.

There is no omnibus “settled” bit. Compose the synchronization appropriate to
the interaction:

- `JsonProbe::wait` and `wait_fresh`
- `Application::wait_until` for external predicates
- `X11Controller::wait_changed`
- `X11Controller::wait_quiet` with explicit tolerance and consecutive samples

Animated products should wait for a semantic or external predicate and then
sample pixels; they should not demand quiescence from an animation.

## Example

```rust,no_run
use std::time::Duration;
use egui_tester::{AppCommand, Button, JsonProbe, Testbed};

let testbed = Testbed::raise()?;
let probe_path = testbed.private_path("probes/app.json")?;
let app = testbed.launch(
    AppCommand::new("/path/to/app")
        .private_env("MY_TEST_PROBE", "probes/app.json")
        .runtime(Duration::from_secs(30)),
)?;
let x11 = testbed.x11()?;
let window = x11.wait_window(&app, "window title", Duration::from_secs(10))?;
let mut probe = JsonProbe::new(probe_path);
let target = probe.wait_anchor(&app, "submit", Duration::from_secs(5))?;
let (x, y) = target.center();
x11.click(&window, x, y, Button::Primary)?;
# Ok::<(), egui_tester::Error>(())
```

## Verification

```console
./check.py verify
cargo test -p egui-tester-fixture --test e2e
```

The fixture suite proves real input and pixels, teardown of private state,
default host invisibility, and denial of writes through an explicit read-only
borrow.

The out-of-tree booru acceptance scenario uses its checked-in demo state:

```console
cd ../booru_viewer
cargo build --release --bin abv --features devtools

cd ../egui_tester
cargo run -p booru-acceptance -- \
  /data/main/cargo-target/release/abv \
  ../booru_viewer \
  /tmp/booru-acceptance-artifacts
```

It opens the UI recess, changes water mode, adjudicates real pixel changes,
waits for the private slate without a scripted sleep, restarts booru, and
proves persistence.

Wayland validation is optional until Weston is installed:

```console
cargo test -p egui-tester-fixture --test wayland -- --ignored
```

See [architecture.md](docs/architecture.md) for boundaries and the next
increments.
