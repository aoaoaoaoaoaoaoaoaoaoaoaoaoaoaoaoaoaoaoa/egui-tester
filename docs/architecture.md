# Architecture

## Names

**Testbed** is the owned universe: display, private filesystem, process
boundary, and teardown.

**Application** is one transient service and its complete descendant cgroup.

**Witness** is one-way product telemetry used for targeting and
synchronization.

**Target** is a stable semantic address published by a product. Its wire name
uses `owner.semantic.path[/identity...]`: dotted kebab-case segments express
stable semantics, and slash-delimited encoded components carry runtime
identity. Semantic segments admit neither underscores nor colons. After the
first slash, only unreserved ASCII and canonical percent escapes are admitted;
raw user text is encoded with `encode_identity`. `TargetName` is that grammar as a
type: an Anchor's name is one, so `egui::take` refuses a pass that recorded an
off-grammar name, a probe refuses a frame carrying one, and a lookup by an
off-grammar name fails at once instead of waiting for a Target that cannot
exist.

**Anchor** is one Target's observed physical rectangle and keyboard-focus fact
in a presented frame. A Target may be absent; an Anchor is frame evidence.

**Observation** is the deliberately bounded product-state projection published
with one presented witness frame.

**Probe** is the sealed journal reader that advances through complete witness
records and retains the newest Observation locally.

**Condition** is a pure predicate over an Observation with one diagnostic
description.

**Reaction** is the causally ordered interval beginning with an injected action
receipt and ending at an eligible Condition, surface present, pixels, or
external effect. Eligibility does not itself prove causation.

**Oracle** is external product evidence used for a verdict.

**Verdict** is the final judgment obtained from pixels, private durable state,
process state, or another external Oracle. Witness state alone is not a Verdict.

**Choreography** is a product-owned semantic score. Its base law carries
editorial cues; a product trait extends that law with domain acts without
encoding platform targets or HID.

**Projection** binds Choreography to one platform's target resolution, native
input, synchronization, and verdict machinery.

**Observer** is a passive consumer of one execution's causal event stream. It
may retain evidence but has no product-mutation authority.

**Story** is one effectful execution of Choreography through a Projection and
Observer, bound to one launch of the application.

**Take** is one or more Stories sharing one Observer across relaunches. A take
yields at most one film and any number of stills.

**Still** is one publishable frame a score names with an `ArtifactStem`: the
product's presented surface once its witnessed presentation rests, in the
take's Framing, with nothing painted over it.

**Frontispiece** is the still named `frontispiece`, which leads a showcase:
its product's portfolio image. A showcase take that names none publishes
nothing.

**Framing** is the pixel geometry a showcase take presents: the product
window's size, and so its film's and every still's. A desktop story imposes it
when the observer attaches; a device screen must already hold it. Each pass's
host declares the surface size it presented, so readiness and stills can
demand the framing; a host that declares none leaves it unverified.

**Playtest** is a Story performed on a physical device for evidence. It is
not a Foundry law: acceptance stays the hermetic testbed proof.

**Fixture** is deterministic input and environment owned by the acceptance
executable. It is not a product backdoor or substitute implementation.

**Specimen** is operator-owned data admitted into a showcase, read-only or as a
private copy. It never enters an acceptance verdict. A product caches it
beneath `egui_demo::specimen_cache`: `<product>-showcase` in the XDG cache
home, apart from everything an acceptance rail reads.

**Showcase** is a product's presentation take: a score performed against a
specimen world, developing stills led by its frontispiece and optionally
recording its film.

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
product methods or exposes a mutation channel. Like a desktop session, the
harness gives the private display's root window the themed `left_ptr`, found
where the sandboxed product's libXcursor would look; windows that define no
pointer inherit it instead of the bare server's `X_cursor` placeholder.

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
`NoNewPrivileges`, control-group termination, and network denial. After
constructing the guest mount graph, Bubblewrap applies a seccomp filter that
denies payload creation of SUID/SGID filesystem objects. Bubblewrap also adds
process, mount, IPC, UTS, and normally network namespaces. Software graphics
sees only lavapipe. Host graphics adds discovered DRM, KFD, or NVIDIA character
devices and read-only sysfs.

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

A still's rest keeps that law, and only an observer that develops stills asks
for it; for any other the cue passes at once, as every other editorial cue
does. Before a developed still, `Story::still` waits until, for 400 ms, no
presented pass moved an anchor, changed the Observation, or was astir, and the
newest pass is calm; under a framing, the resting pass must also have
presented at the framing's size. It then publishes whatever the surface
shows. It reads witnessed passes, never pixels, and it is an editorial guard,
not a verdict. Neither pixels nor pass silence could serve: lawful animation
keeps presenting passes that change nothing a still must wait for.

A pass is **astir** when its host declares, through `PendingFrame::astir`,
that presentation motion the host drives is under way and has asked for the
next pass. Visual-only motion, such as a surface settling after a disturbance
or a loading animation, moves no anchor and changes no Observation, so only
the host can declare it. Ambient animation is whatever the host does not
declare, and it continues into the still. A host that declares nothing leaves
every pass calm, so its visual-only motion goes unguarded after the 400 ms.
Declared motion may outlast any ordinary wait, and the rest is bounded only to
fail perpetual motion loudly, so the still timeout defaults to 20 s, twice the
wait timeout, unless `Story::with_still_timeout` says otherwise. A
presentation that never rests within it fails the story with its pass rate,
the age of its last pass, how many passes were astir, the anchors that moved,
and how often the state changed. Android's projection reads no witness, so its
still cannot rest; its score settles first.

A `Reaction` considers only frames newer than its prior cursor and with product
timestamps after the gesture trigger. This is temporal eligibility, not proof
of causation. The resulting frame is a synchronization fence for a subsequent
pixel or external oracle.

Structural layout changes should request an egui discard pass. Application
instrumentation must clear targets at every pass and publish only the
replacement pass that was actually submitted.

## Story Stream

Choreography is deliberately not an eager action vector or persisted replay.
The product score calls semantic acts through a product-owned trait. A
Projection executes native input and synchronization while emitting
`StoryEvent` items synchronously in causal order. `StoryCue` carries authored
editorial intent: chapters, literal holds, persistent choreography tempo, and
named stills.
`StoryFact` carries resolved target geometry, dispatched input receipts, and
matched observation identity. Cues, facts, and events are exhaustive, and
every projection answers `still`, so a new kind reaches each observer as a
compile error; `Error` alone grows without breaking a match. An Observer sees
only a capture-only view of the product surface, its pixels and the pointer
the display server presents, and itself injects nothing and mutates nothing.
A showcase take alone may request two presentation policies, which the
desktop projection performs: a Framing, which resizes the window before the
take begins, and an `Approach`, which glides the real pointer before each
gesture that jumps to its destination. A showcase is therefore a
presentation, not the acceptance execution. Every other observer, a proof
film's recorder included, requests neither and so records exactly the input
an unobserved story injects; its beats only pause between acts.

Ordinary acceptance attaches `Silent`; film production attaches
`egui_demo::Recorder`, and still production attaches `egui_demo::Darkroom`. All
run the same scenario, so a recorded take remains an acceptance execution
rather than an independently scheduled imitation. `egui_demo::Showcase` develops
a take's stills and may record its film in the same execution. A filmed
showcase's approach is one beat of glide and one of dwell, so its film shows
the pointer's real flight rather than a painted one; a stills-only showcase
and every other observer approach instantly. An overlay never changes the beat
structure: the diagnostic overlay marks aimed targets, the pointer, dispatched
actions, and chapter titles; the presentation overlay, which showcase films
use, composites only the server's own pointer bitmap where the server
presents it, and its chapters are rests whose titles stay in the trace.
`Story::leave` withdraws the pointer to the screen corner
farthest from the window and reports that departure point, in window
coordinates, through the ordinary aimed and dispatched facts: a film glides
the pointer out of frame, and a still carries no hover state. It is no
`Choreography` act, because Android has no pointer to withdraw. The
recorder samples the product continuously against an invariant output clock so
ambient animation remains temporally honest between semantic events, even when
a capture misses one tick. Each launch segment keeps one film clock from its
first event to `Story::finish`. Every story wait offers its observer
`StoryObserver::awaiting` between journal polls, and every deliberate rest of
input pacing does the same, so the film keeps sampling while the story waits
on the product or paces a gesture, and loading keeps its real duration. A tick
due before a capture shows the previous capture, never a later one. A take
cuts only between segments, where no product surface exists. The showpiece
rail keeps that live clock upstream of compression by staging lossless RGB and
performing the expensive presentation transcode only after story execution
and product teardown. `Story::finish` closes one launch's segment, so a take
may continue through a relaunch; after the product terminates,
`Recorder::publish` seals the capture and performs the offline artifact work.
Every artifact develops in a staging directory beside its destination and is
renamed into place only on publication, so an abandoned take leaves nothing
behind that passes for a finished one. A staging directory forms under
another name, locks inside, and only then takes the name each new stage
sweeps: a sweep removes staging no live take holds, which a killed take
leaves, and never staging still forming. The one exception to discarding is a
proof film dropped unpublished: a failed take is when proof matters, so it is
finalized with its trace under an `-abandoned` stem, until a published take of
the same film retires it. An unpublished showpiece is discarded.
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

X11 is the sole complete backend. Android supplies an ADB Projection for
physical-device choreography: shell touch and key input, UiAutomation
multi-pointer gestures, screenshots, and an optional Perfetto Observer. It
does not yet provide a sealed semantic witness, predicate-driven
synchronization, private containment, or acceptance-parity verdicts. Its JSONL
event trace preserves causal host order; its host clock is not directly
comparable with the device Perfetto clock.

Headless Weston proves only isolated launch
and output capture. The Wayland testbed does not grant client authority when a
socket pathname merely appears; it first proves that the configured output is
capturable. Native Wayland input requires compositor authority, likely a test
protocol or plugin; AccessKit actions would bypass the pointer and keyboard
boundary. That expansion remains parked.

AccessKit is instead the likely replacement for hand-authored target rectangles
where its stable author IDs, roles, names, values, focus, and bounds suffice.
Product-specific observations will remain for facts an accessibility tree
cannot express.

Other named gaps are native-dialog choreography, window move/resize, clipboard
and IME input, text beyond Latin-1, and replay policy for persisted story
traces. Named X11 windows, including tray surfaces, may be driven directly;
embedded-window eviction and redock are first-class controller operations. A
product must park any remaining dependent story rather than smuggle xdotool or
ambient desktop authority back in.
