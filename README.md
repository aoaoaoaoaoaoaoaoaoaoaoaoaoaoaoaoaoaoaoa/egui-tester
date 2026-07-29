# egui-tester

`egui-tester` is a hermetic black-box harness for native egui applications. It
launches the real executable, injects display-server input, observes real
pixels, and owns the surrounding desktop state. Named widget probes may locate
targets and synchronize frames; they do not decide whether the product worked.

The name is intentionally utilitarian.

## Present Surface

The X11 backend is the complete MVP:

- private authenticated Xvfb, never the caller's `DISPLAY`
- XTEST pointer, held-button, drag, wheel, modifier, function-key, and Latin-1
  keyboard input from Rust
- window discovery, focus, RGBA capture, PNG artifacts
- bounded waits for windows, witness predicates, external effects, pixel
  change, and pixel quiescence
- versioned, launch-sealed, post-present witnesses through
  `egui-tester-witness`
- input-to-observation and input-to-presentation performance budgets
- action transcripts, last-good captures, and automatic failure bundles

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

A **witness** answers “where is the control?” or “has a newer product frame
been presented?” The standard witness is one-way, atomic, launch-sealed
telemetry. Legacy application probes remain readable during migration.

An **oracle** answers “did the product work?” Oracles are captured pixels,
files the product emitted into private state, process exits, or externally
observable protocol effects. Tests should not adjudicate success from a
witness state that merely mirrors an implementation field.

There is no omnibus “settled” bit. Compose the synchronization appropriate to
the interaction:

- `JsonProbe::wait` and `wait_fresh`
- `Application::wait_until` for external predicates
- `X11Session::wait_changed`
- `X11Session::wait_quiet` with explicit tolerance and consecutive samples

Animated products should wait for a semantic or external predicate and then
sample pixels; they should not demand quiescence from an animation.

`PerformanceBudget` keeps the production threshold separate from its larger
functional timeout. `JsonProbe::wait_budgeted` measures a native-input
`ActionReceipt` against an in-product monotonic timestamp. Observation budgets
end after product-state work and before witness work. Calling
`through_presentation()` instead ends after the corresponding real frame was
presented. Polling, screenshots, anchor extraction, serialization, and witness
I/O cannot consume either budget.

## Example

```rust,no_run
use std::time::Duration;
use egui_tester::{
    AppCommand, Button, PerformanceBudget, Testbed, WindowQuery,
};

let testbed = Testbed::raise()?;
let app = testbed.launch(
    AppCommand::new("/path/to/app")
        .witness("probes/app.json")
        .runtime(Duration::from_secs(30)),
)?;
let session = testbed.x11_session(
    &app,
    WindowQuery::title_exact("window title"),
    Duration::from_secs(10),
)?;
let mut probe = app.witness()?;
session.wait_presented(&mut probe, Duration::from_secs(10))?;
let target = probe.wait_anchor(&app, "submit", Duration::from_secs(5))?;
let (x, y) = target.center();
let click = session.click(x, y, Button::Primary)?;
let _submitted = probe.wait_budgeted(
    &app,
    &click,
    PerformanceBudget::new(Duration::from_millis(250))
        .through_presentation(),
    "submit the form",
    |frame| frame.state["submitted"] == true,
)?;
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

`cargo run -p egui-tester-doctor` is the turnkey host preflight. It discovers
the canonical user manager without borrowing the caller's desktop session and
raises then destroys one isolated universe.

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

Trailgen is the first standard-witness adoption:

```console
cd ../adequate_trailgen
scripts/test-gui
```

Its release-mode acceptance creates a project through the real CLI, opens and
renames a saved trail, acquires and drags a map pin, proves live route
recomputation, persists it, and checks the library as an external oracle.

Wayland validation is optional until Weston is installed:

```console
cargo test -p egui-tester-fixture --test wayland -- --ignored
```

See [architecture.md](docs/architecture.md) for boundaries and
[adoption.md](docs/adoption.md) for the reusable application contract.
