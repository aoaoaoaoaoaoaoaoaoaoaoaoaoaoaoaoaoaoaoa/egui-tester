# egui-tester

`egui-tester` is a hermetic black-box harness for native egui applications. It
launches an optimized executable, injects native display-server input, captures
real pixels, and judges external product effects. Optional one-way observations
locate controls and release waits; they cannot mutate the application and are
not verdicts.

## Present Surface

X11 is the complete, release-tested vertical:

- authenticated private Xvfb, never the caller's `DISPLAY`;
- XTEST clicks, held-button drags, strokes, wheels, modifiers, function keys,
  and Latin-1 keyboard input;
- exact window discovery, focus, RGBA capture, tolerant regional pixel
  comparison, and PNG artifacts;
- a private XDG tree, mount and network namespaces, a transient user-service
  cgroup, runtime limits, and complete descendant teardown;
- launch-sealed semantic and frame journals through `egui-tester-witness`;
- typed `Story<S>`, composable `Condition<S>`, and gesture `Reaction`
  porcelain;
- separate functional deadlines, reaction latency contracts, and sustained
  cadence contracts;
- curated logs, private outputs, captures, and diagnostics on failure.

The ignored Wayland fixture owns a headless Weston launch-and-capture smoke. It
has no native-input or acceptance-parity claim. Wayland remains deliberately
frozen while the X11 pattern spreads to another application.

## Containment

Each application is a transient `systemd --user` service whose cgroup contains
a bubblewrap sandbox. The guest sees read-only `/usr`, `/etc`, and the
application binary; a synthetic `/dev`; private namespaces; and a writable,
disposable `/test`. `HOME`, every XDG root, `TMPDIR`, logs, fixtures, and
observations live beneath `/test`.

Host data is absent by default. `AppCommand::borrow_read_only(path)` is the sole
data aperture and has no writable counterpart. Network authority is denied
unless declared. `Graphics::Software` uses the pinned lavapipe runtime;
`Graphics::Host` admits only discovered GPU character devices plus read-only
sysfs for representative performance runs.

Harness-side product files should be read through `Testbed::read_private`.
These capability operations use `openat2` beneath the private root and reject
application-created symlinks. Artifact destinations are never mounted into the
application.

## Evidence Model

A **witness** answers “where is this target?” or “does a later product
observation have this shape?” The standard witness is one append-only,
length-framed semantic journal. Every record carries a launch seal, frame and
surface sequence, monotonic product timestamps, scale, anchors, and
application-selected state. `Probe` consumes every complete record in order;
there is no competing latest-state file.

An observation whose timestamps follow an input is temporally eligible. That
does not prove the input caused it. A **verdict** therefore comes from pixels,
private durable state, process behavior, protocol effects, or a later cold
start.

The application enqueues observations only after `wgpu`'s surface-present call
returns. A private writer serializes both journals off the UI thread.
`ReactionBudget::functional` supplies only a missing-cue deadline.
`ReactionBudget::performance` additionally adjudicates the product timestamp;
`through_surface_present` extends the endpoint from completed product-state
work through surface submission. Neither endpoint claims compositor scanout or
physical display completion.

Sustained interactions use `FrameProbe::trace` and `CadenceBudget`. Their
statistics come directly from product timestamps; there is no guessed
instrumentation multiplier or post-hoc “witness tax” correction. Functional
stories normally use deterministic software graphics. Host graphics alone
adjudicates production GPU latency.

## Example

```rust,no_run
use std::time::Duration;

use egui_tester::{
    AppCommand, Condition, ReactionBudget, Story, Testbed, WindowQuery, demand,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Observation {
    submitted: bool,
}

let testbed = Testbed::raise()?;
let app = testbed.launch(
    AppCommand::new("/path/to/app")
        .witness("probes/app.observations")
        .runtime(Duration::from_secs(30)),
)?;
let mut story = Story::<Observation>::bind(
    &testbed,
    &app,
    WindowQuery::title_exact("window title"),
    ReactionBudget::functional(Duration::from_secs(5)),
)?;
let _ready = story.ready(Duration::from_secs(10))?;
let submitted = story
    .click("form.submit")?
    .within(
        ReactionBudget::performance(Duration::from_millis(250))
            .through_surface_present()
            .timeout(Duration::from_secs(5)),
    )
    .until(Condition::new("form submitted", |state: &Observation| {
        state.submitted
    }))?;
let rendered = story.capture()?;
demand(rendered.rgba().iter().any(|channel| *channel != 0), "blank window")?;
drop(submitted);
# Ok::<(), egui_tester::Error>(())
```

Rendered oracles compare bounded semantic regions with explicit tolerance;
whole-window stillness and exact snapshots are intentionally absent because
lawful product animation remains enabled. The application-side publisher is
documented in [Application Adoption](docs/adoption.md). Product Targets may be
ordinary enums implementing `Display`; the contract crate need not depend on
the tester.

## Verification

```console
./check.py verify
cargo test -p egui-tester-fixture --test e2e
cargo run -p egui-tester-doctor
```

`egui-tester-doctor` verifies the canonical user manager, raises one isolated
X11 universe, then destroys it. Trailgen is the reference full adoption:

```console
cd ../adequate_trailgen
scripts/test-gui
```

Its four release-mode stories cover project creation and provider acquisition;
rename, pin drag, recomputation, undo/redo, cancellation, save, and restart;
dense candidate comparison under host cadence budgets; and manual loop editing
with profile interaction.

See [Architecture](docs/architecture.md) for the trust boundary and
[Application Adoption](docs/adoption.md) for the reusable product seam.
