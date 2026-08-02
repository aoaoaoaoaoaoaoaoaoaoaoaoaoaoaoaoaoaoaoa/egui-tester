# Application Adoption

## Repository Shape

Each product owns a thin, unpublished `<product>-acceptance` executable. It
depends on `egui-tester` and a small product contract crate, never the GUI or
domain implementation. Product knowledge is limited to fixture seeding,
semantic predicates, budgets, and external oracles.

```text
crates/
  <product>-contract/
  <product>-acceptance/
    src/stories/
scripts/test-gui
```

The contract crate owns stable product vocabulary shared by GUI and
acceptance: application identity, schema fingerprint, Target names, and any
small enums that cross the observation wire. A Target need only implement
`Display`; the contract remains independent of the tester. Handwritten Rust is
the present source of truth. Derives or a shared contract-language crate should
appear only after a second product reveals repeated syntax.

Acceptance defines a deliberately partial `Deserialize` observation. It may
reuse contract enums but should not depend on the GUI's witness struct or
mirror the product model wholesale.

## Build Classes

The product exposes an `egui-test` feature that adds only
`egui-tester-witness`. This feature must not change defaults, layout, product
authority, timing policy, or behavior. It is a one-way telemetry aperture, not
a control plane.

The canonical script builds optimized artifacts and runs two classes:

1. an uninstrumented launch-and-pixels smoke against the exact ordinary
   product build;
2. instrumented stories against the same production path.

Functional stories use deterministic software graphics and
`ReactionBudget::functional`. Representative host-graphics runs alone may use
`ReactionBudget::performance` or `CadenceBudget` to enforce production
latency. A functional deadline is not a disguised generous performance
threshold.

## Publisher Lifecycle

One product frame follows this order:

1. install target instrumentation once on the egui context;
2. begin a `FramePulse` immediately before taking product input;
3. run normal UI, state work, replacement passes, tessellation, and any
   application projection needed by the observation;
4. extract final-pass targets and project the minimal state;
5. call `FramePulse::observe` after that projection;
6. forge an owned `PendingFrame`;
7. render and call the graphics surface's present operation;
8. capture `ProductInstant::now()` and enqueue with
   `Publisher::surface_present_at`;
9. flush the publisher before orderly process exit.

Enqueue nothing when surface acquisition or rendering fails. The publisher
serializes off-thread, in order. Any writer failure must eventually reach the
event loop through the next enqueue or the final flush.

Structural state changes should call `Context::request_discard`.
Instrumentation clears targets at every egui pass, so discarded layouts cannot
leak rectangles into their replacements. Wire rectangles are physical,
window-relative pixels.

## Story Law

Scenarios are modules named for user intent, not widgets or implementation
layers. A full story begins from a meaningful cold state, crosses material
transitions with native input, and ends at user-valued evidence. Restart inside
the story when durability is part of that value.

Each consequential step has distinct layers:

1. a native gesture;
2. a temporally eligible witnessed state used only to synchronize;
3. a rendered, durable, process, protocol, or restart oracle.

The witness may say a route signature changed. Persisted geometry or visible
pixels decide whether routing worked. A post-trigger frame is not called
“caused” merely because no earlier frame matched.

Fast native batches still preserve gesture integrity. Modified clicks fence
modifier press and release. Custom canvas drags should use:

1. `button_down` on the witnessed target;
2. wait for product acquisition;
3. `move_to` and wait for recomputation;
4. judge pixels or durable geometry;
5. `button_up` and wait for release.

This tests pointer capture without scheduler sleeps. Product kinetics such as
smoothed zoom use projection-scoped `Probe::wait_stable` before a baseline is
recorded. Polyline tools may set `Stroke::knot_dwell` when the product must
observe every corner despite native motion coalescing.

The first acceptance basis should collectively prove:

1. cold boot to nontrivial pixels;
2. ordinary navigation through semantic Targets;
3. a durable mutation read through a confined private oracle;
4. one application-defining gesture;
5. cancellation or reversal of a nontrivial transaction;
6. restart restoration;
7. at least one host reaction budget;
8. sustained host cadence where lag is product-critical;
9. failure artifacts and a decisive success capture.

Trailgen's four stories are the reference basis: discover and keep; refine
deliberately; compare without lag; and draw from nothing. Its pin-drag story
requires native target acquisition, real motion, route recomputation, durable
geometry change, undo/redo, cancellation, save, and restart.

## Fixtures And Oracles

Fixture seeding occurs before launch through `Testbed::copy_private`,
`write_private`, or the product's public CLI. The product receives no
test-only ingestion API. Network policy is scenario-declared: denied, a private
fixture transport, or explicitly admitted host networking.

After launch, product files are read through `read_private` or exported through
`export`; raw host paths are not oracle APIs because the application controls
names beneath its writable tree. Regional pixel assertions should bind to a
witnessed Target with `PixelRegion::anchor`, then compare captured frames.

Do not use exact snapshots or whole-window stillness as readiness oracles.
Ambient product motion remains live in acceptance builds. Fence the relevant
presented state through the witness, then inspect a named region with tolerant
pixel features or relative change, or prefer a durable external effect.

Witnesses and artifacts need a disclosure budget. Publish only state necessary
to synchronize stories, and retain only diagnostics that explain a failure.
Credentials, ambient user data, and unrelated model state have no place in
either channel.

Failed `Story` and `X11Session` waits attempt one final capture before returning
their original error. Capture failure never launders the primary fault.

## Porcelain Boundary

`Story<S>` is the authoring language; the containment and input crates are the
kernel. Product scenarios should remain terse enough that direct Rust expresses
their intent. If two adoptions expose repeated residual ceremony, a proc macro
may compile declarative stories into this runtime. It must preserve spans,
support explanation, and leave the ordinary API usable. It may not become a
second scheduler or redefine evidence semantics.

No product may hide an unsupported interaction behind xdotool. Missing shared
capabilities are defects to name and implement in the kernel. The current
frontier is recorded in [Architecture](architecture.md).
