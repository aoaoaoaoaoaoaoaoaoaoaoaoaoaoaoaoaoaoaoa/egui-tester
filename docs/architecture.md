# Architecture

## Names

**Testbed** is the owned universe: display, private filesystem, process
boundary, and teardown.

**Application** is one transient service and its complete descendant cgroup.

**Witness** is one-way product telemetry used for targeting and
synchronization.

**Oracle** is external product evidence used for a verdict.

**Story** is the effectful, typed program that drives one application and emits
a causal stream of authored cues and immutable execution facts.

**Borrow** is a live host path deliberately exposed read-only. It never means a
writable mount, overlay, or redirection.

**Surface present** means return from the graphics API's present operation. It
does not mean compositor scanout or physical display completion.

## Trust Boundary

```text
acceptance process
  ├─ authenticated private Xvfb
  ├─ transient systemd user service
  │    └─ bubblewrap
  │         └─ optimized product executable
  ├─ XTEST input and X11 pixel capture
  ├─ sealed witness readers
  └─ external oracle adjudicator
```

The application receives ordinary display-server input. Pixels cross its real
winit, egui, renderer, surface, and X server path. The harness never calls
product methods or exposes a mutation channel.

The harness uses the canonical `/run/user/<uid>/bus` only to govern transient
services. It ignores inherited desktop `DISPLAY`, session-bus, and XDG-runtime
values. The application receives neither that bus nor any live desktop
authority.

## Authority

The default guest mount graph is allowlisted:

| Guest path | Authority |
|---|---|
| `/usr`, `/etc` | read-only runtime |
| `/app/application` | read-only executable |
| `/test` | writable, private, disposable |
| `/dev` | synthetic |
| `/proc`, `/run` | private namespace instances |
| declared borrows | read-only at the same absolute path |

Systemd independently enforces a read-only host, a runtime limit,
`NoNewPrivileges`, control-group termination, and network denial. Bubblewrap
adds process, mount, IPC, UTS, and normally network namespaces. Software
graphics sees only lavapipe. Host graphics adds discovered DRM, KFD, or NVIDIA
character devices and read-only sysfs.

The harness validates every environment override and reserves display,
filesystem, loader, graphics, and witness authority variables. Private oracle
reads, writes, and exports are rooted capability operations using
`openat2(BENEATH | NO_MAGICLINKS | NO_SYMLINKS)`. Failure-bundle traversal also
refuses symlinks.

`Application::terminate` marks a service retired only after a successful
cgroup stop. Drop retries teardown if an explicit stop failed. No scenario may
fall back to an ambient process merely because containment is unavailable.

## Observation

The standard semantic surface is one launch-sealed append-only journal. Each
length-framed record contains:

- schema and launch identity;
- product frame and surface sequence;
- frame-begin, observation, and surface-present monotonic timestamps;
- physical scale, target rectangles, and presented egui focus where recorded;
- deliberately selected product state.

There is no atomic latest-state twin. `Probe<S>` incrementally consumes all
complete records and retains the newest locally; incomplete live tails wait for
their remainder. Required envelope fields cannot silently default.

The fixed-width frame journal carries the same frame identity and timestamps
for low-cost cadence analysis. One asynchronous publisher owns both files.
The UI thread projects state and anchors, captures the observation timestamp,
renders, calls surface present, then enqueues the owned record. Serialization
and filesystem writes occur on a private worker. Shutdown flushes the queue and
surfaces writer faults.

Legacy single-JSON and weak journal readers are explicitly named
`LegacyJsonProbe` and `LegacyProbe`. They exist only for migration and do not
inherit standard-witness guarantees.

## Synchronization

Every wait is predicate-driven, bounded, and liveness-aware. Polling intervals
are implementation detail, never choreography. A caller may await:

- a uniquely selected viewable window;
- a target, target focus, or state predicate in a complete observation;
- projection stability for a declared quiet interval;
- a private durable effect;
- changed pixels in a named region.

Whole-window pixel quiescence is not a synchronization primitive. Carets,
water, loading motion, and other lawful animation may continue after the
requested state is ready. Synchronize through a presented semantic predicate,
then judge a bounded region with a tolerant feature or change predicate, or use
an external effect. Production motion remains enabled during acceptance.

A `Reaction` considers only frames newer than its prior cursor and with product
timestamps after the gesture trigger. This is temporal eligibility, not proof
of causation. The resulting frame is a synchronization fence for a subsequent
pixel or external oracle.

Structural layout changes should request an egui discard pass. Application
instrumentation must clear targets at every pass and publish only the
replacement pass that was actually submitted.

## Story Stream

`Story` is deliberately not an eager action vector. It executes native input
and semantic waits while emitting `StoryEvent` items synchronously in causal
order. `StoryCue` carries authored editorial intent: chapters, literal holds,
and persistent choreography tempo. `StoryFact` carries resolved target
geometry, dispatched input receipts, and matched observation identity. A
consumer sees only a capture-capable view of the product surface and gains no
mutation authority.

Ordinary acceptance attaches `Silent`; film production attaches
`egui_demo::Recorder`. Both run the same scenario, so a recorded take remains
an acceptance execution rather than an independently scheduled imitation. The
recorder samples the product continuously against an invariant output clock so
ambient animation remains temporally honest between semantic events, even when
a capture misses one tick. The showpiece rail keeps that live clock upstream of
compression by staging lossless RGB and performing the expensive presentation
transcode only after story execution and product teardown. `Story::finish`
seals live capture; `Recorder::publish` owns only offline artifact work.
Facts are serializable provenance, not executable commands. Deterministic
replay remains a separate policy problem because targets may move and waits
may resolve along different lawful paths.

## Timing

An `ActionReceipt` records gesture start, the final input capable of satisfying
the postcondition, and injection completion in `CLOCK_MONOTONIC`. Reaction
latency begins at the trigger, excluding deliberate pointer transport and
wheel pacing.

`ReactionBudget::functional(timeout)` bounds progress but makes no performance
claim. `ReactionBudget::performance(limit)` additionally rejects a matching
product timestamp beyond `limit`; its larger timeout cannot dilute that
threshold. Observation is the default endpoint.
`through_surface_present()` extends it through graphics surface submission,
not scanout.

`FrameProbe::trace` isolates frames begun during a sustained gesture.
`CadenceBudget` may constrain minimum samples, p50, p95, worst cadence, and p95
product frame work. Reports use raw UI-thread timestamps. Instrumentation runs
off-thread, so no guessed correction is applied.

Functional stories use `Graphics::Software` for deterministic behavior.
Production reaction and cadence contracts use `Graphics::Host` on a
representative machine. Both run optimized product code. An “instrumented build
multiplier” is inadmissible.

## Platform Frontier

X11 is the sole complete backend. Headless Weston proves only isolated launch
and output capture. Native Wayland input requires compositor authority, likely
a test protocol or plugin; AccessKit actions would bypass the pointer and
keyboard boundary. That expansion remains parked.

AccessKit is instead the likely replacement for hand-authored target rectangles
where its stable author IDs, roles, names, values, focus, and bounds suffice.
Product-specific observations will remain for facts an accessibility tree
cannot express.

Other named gaps are multi-window and native-dialog choreography, tray
surfaces, window move/resize, clipboard and IME input, text beyond Latin-1, and
replay policy for persisted story traces. A product must park a dependent story
rather than smuggle xdotool or ambient desktop authority back in.
