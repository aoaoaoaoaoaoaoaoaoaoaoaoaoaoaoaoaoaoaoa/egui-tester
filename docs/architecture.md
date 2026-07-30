# Architecture

## Names

**Testbed** is the owned universe: display or compositor, private filesystem,
and teardown boundary.

**Application** is one transient systemd service inside a testbed.

**Witness** is one-way application telemetry used for targeting and
synchronization.

**Oracle** is product evidence used for a verdict.

**Borrow** is a live host path deliberately revealed read-only. A borrow never
means a writable mount, overlay, or redirection.

These terms should not be used interchangeably. In particular, application
state is not an oracle merely because it is convenient to serialize.

## Boundary

The harness sits outside the application and controls the operating-system
boundary:

```text
test process
  ├─ Xvfb or Weston
  ├─ systemd transient application cgroup
  │    └─ bubblewrap
  │         └─ real application executable
  ├─ XTEST / Weston output capture
  └─ witness reader and oracle adjudicator
```

The application receives native input from its display server. The pixels
cross the real winit, egui, tessellation, renderer, surface, and display path.
The harness does not call application methods or mutate application state.

## Filesystem Authority

The default guest mount graph is allowlisted:

| Guest path | Authority |
|---|---|
| `/usr`, `/etc` | read-only runtime |
| `/app/application` | read-only executable |
| `/test` | writable, private, disposable |
| `/dev` | synthetic under software graphics |
| `/proc`, `/run` | private namespace instances |
| declared borrows | read-only at the same absolute path |

Systemd independently marks the host system read-only and grants write access
only to the harness-created session root needed as bubblewrap's backing store.
The app sees that store solely as `/test`. On normal failure and panic,
systemd kills the complete cgroup before the testbed removes the store.

The kernel returns `EROFS` or `EACCES` for writes through a borrow. The harness
retains stderr and exit facts. An application that deliberately swallows an
I/O error remains responsible for exposing the resulting product failure; a
future syscall-audit mode may make attempted denied writes independently
observable.

## Synchronization

Every wait has a deadline and continuously checks application liveness.
Polling intervals are implementation detail, not choreography. The caller
states a predicate:

- a window exists and is viewable
- an anchor or semantic value exists in a fresh witness frame
- a private product file contains a committed value
- enough pixels differ from a baseline
- sufficiently few pixels differ for N consecutive samples

The standard witness is published only after its product frame is presented.
Its semantic state and anchors must describe one coherent egui pass. A
pass plugin clears anchors before every egui pass, including replacement passes
requested through `Context::request_discard`. A transition predicate should
still require both the new state and a control that belongs to that state.

A witness transition still must not substitute for a product verdict. It may
release a subsequent pixel or external-oracle wait.

## Performance

Every native input operation may return an `ActionReceipt` with three
`CLOCK_MONOTONIC` instants: gesture start, the final input that can satisfy its
postcondition, and injection completion. A reaction budget begins at the
trigger; a cadence trace spans the gesture. Deliberate pointer transport and
wheel pacing therefore enter cadence evidence without taxing product reaction
latency.

A standard witness and its lossless frame journal carry four timestamps from
the same epoch:

1. `begun_ns`, captured at product frame entry;
2. `observed_ns`, captured after product-state work;
3. `presented_ns`, captured immediately after the corresponding real frame is
   presented;
4. `retired_ns`, captured after post-present witness publication.

The adapter collects anchors, constructs test-only state, serializes JSON, and
atomically publishes only after presentation. Harness polling,
screenshots, and filesystem latency are therefore outside both verdicts.
`PerformanceBudget` defaults to observation and may opt into presentation.
Its functional timeout bounds a missing result but never dilates the
production threshold.

`FrameProbe::trace` does not mistake an in-flight presentation for a
post-gesture frame: its causal fence requires a frame begun after action
completion. `CadenceBudget` subtracts each preceding frame’s
`presented_ns..retired_ns` witness tax before computing p50, p95, and worst
cadence; p95 frame work remains the unadjusted
`begun_ns..presented_ns` product interval.

Run performance acceptance against an optimized product binary. Software
graphics is a conservative presentation environment; use
`Graphics::Host` only when the question specifically requires representative
GPU timing. Never invent an “instrumented build multiplier.”

## Wayland

Headless Weston supplies a real Wayland socket and pixman output without
changing the operator's desktop session. Output capture is already a real
compositor observation.

Input remains a named gap. Wayland gives input authority to the compositor,
not arbitrary clients. The correct next increment is a small Weston test
plugin or another compositor test protocol that synthesizes seat events before
normal dispatch. AccessKit actions are unsuitable as the primary E2E input
path because they bypass pointer and keyboard handling.

## Future Surfaces

The next reusable seam should replace app-specific anchor JSON with a one-way
AccessKit/frame-presented stream. Stable author IDs locate widgets; the
standard accessibility tree carries role, name, value, focus, and bounds.
Booru's current adapter can then contract to application-specific predicates
that AccessKit cannot express.

An optional MCP should wrap the Rust controller, not enter the application.
Useful tools are `launch`, `inspect`, `click`, `type`, `wait`, `capture`, and
`artifacts`. Every response should include epoch and evidence paths so a model
cannot confuse stale telemetry with the last input.

Demo recording should reuse a serializable action and selector IR while
retaining separate timing policy. Tests optimize for predicate-driven speed;
videos add deliberate cursor motion, dwell, and capture cadence. The oracle
and containment machinery is shared, not the sleeps.
