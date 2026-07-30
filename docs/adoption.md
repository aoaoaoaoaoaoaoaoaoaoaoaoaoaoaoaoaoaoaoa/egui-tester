# Application Adoption

## Durable Shape

Each product owns a thin, unpublished `<product>-acceptance` executable. It
depends on `egui-tester`, not on product crates. Its only product knowledge is
fixture seeding, witness predicates, performance budgets, and real oracles.
Containment, display input, synchronization, timing, transcripts, and failure
artifacts remain library responsibilities.

The executable defines an acceptance-owned, deliberately partial
`Deserialize` Observation and drives the product through `Story<Observation>`.
Stable product Targets should live in a dependency-free product contract crate
shared with the GUI. Implementing `AsRef<str>` admits them directly to the
tester without making that contract depend on `egui-tester`.

The product exposes an `egui-test` feature that adds
`egui-tester-witness`. This feature is telemetry, never a control plane. The
acceptance executable must still launch the real optimized product binary and
drive native input.

A conventional repository provides:

```text
crates/<product>-acceptance/
  src/stories/
scripts/test-gui
```

`scripts/test-gui` builds the product in release mode with `egui-test`, then
runs the acceptance executable. The acceptance executable derives the sibling
product binary by default, accepts an artifact directory, and uses
`TestbedBuilder::failure_artifacts`.

Scenarios are modules named for user intent, not widgets or implementation
layers. Shared fixture and harness modules may contain containment setup,
selectors, and product-independent choreography; verdict logic stays in the
story that owns it.

## Adapter Lifecycle

One application frame follows this order:

1. install `egui_tester_witness::egui::install` once on the egui context;
2. begin a `FramePulse` immediately before taking product input;
3. run ordinary product UI and state work;
4. call `FramePulse::observe`;
5. tessellate, render, and present normally;
6. capture `ProductInstant::now()` immediately after presentation;
7. extract final-pass anchors and the smallest useful semantic state;
8. call `Publisher::present_at`.

The last three telemetry operations occur after presentation so their cost
cannot pollute either performance endpoint. Publish nothing when surface
acquisition fails or no frame was presented.

Structural state changes should call `Context::request_discard`. The witness
plugin clears anchors at every egui pass, so discarded layouts cannot leak
targets into the replacement pass. The published state and anchors must
describe the frame that actually reached the display.

Anchors are stable intent names such as `library.rename` or
`editor.support/1`, expressed in physical window-relative pixels. Do not name
layout positions or implementation types. Semantic state should expose facts
needed for synchronization, not duplicate the product model wholesale.

The product should publish a contract fingerprint in that minimal state.
Every story verifies it before the first injected input. A stale test binary
must fail as a schema mismatch rather than gesture against misnamed controls.

## Scenario Law

Every acceptance step has three distinct layers:

1. a native gesture through `X11Session`;
2. a fresh, frame-coherent witness predicate;
3. a rendered or external product oracle.

Witness state may prove that a route was recomputed; persisted geometry or
pixels decide whether the product worked. Require controls belonging to the
new state in transition predicates, for example `view == "edit"` together
with `editor.support/1`.

The atomic witness supplies current state and bounds. Budgeted Reactions read
the lossless semantic journal and choose the earliest causally eligible frame;
do not compensate for polling cadence with sleeps or an enlarged production
budget.

Fast native batches still obey human gesture causality. Modified clicks guard
modifier acquisition and release so an event loop cannot observe only the
final modifier state. `FrameProbe::trace` fences on a frame begun after the
gesture completed. Product kinetics that legitimately continue afterward,
such as smoothed wheel motion, use `JsonProbe::wait_stable` on the relevant
semantic projection before establishing a baseline.

Ordinary widgets may use `X11Session::drag`. Custom canvas gestures should
compose held operations:

1. `button_down` on the witnessed target;
2. wait until the product witnesses target acquisition;
3. `move_to` the destination and wait for the semantic/rendered result;
4. `button_up` and wait for release.

This removes scheduler-dependent sleeps and tests the same capture law a user
depends on.

## User-Story Law

An acceptance scenario is a full user story, not a widget smoke test. It begins
from a meaningful cold state, crosses every material transition through native
input, and ends at a user-valued result with an external oracle. Restart inside
the story whenever durability is part of that value.

The first acceptance basis for an application should collectively cover:

1. cold boot to a real presented frame;
2. one ordinary navigation or control transition;
3. one durable mutation checked outside the witness;
4. one application-defining rich interaction;
5. at least one input-to-presentation budget;
6. one sustained-interaction cadence budget where lag is product-critical;
7. cancellation or reversal of one nontrivial transaction;
8. a final screenshot and automatic failure bundle.

Trailgen supplies the reference basis as four stories: discover and keep,
refine deliberately, compare without lag, and draw from nothing. In
particular, its refinement story acquires pin 1, drags it to another graph
branch, proves a different route signature within budget, cancels without disk
mutation, repeats, saves, and compares durable support points after restart.

## Performance Law

`PerformanceBudget` judges one reaction from the gesture’s final
result-triggering input. Deliberate pointer transport, wheel pacing, tester
dwell, and witness I/O must not dilate it. `CadenceBudget` judges the complete
sustained gesture from the lossless frame journal and may constrain minimum
samples, median cadence, p95 cadence, worst cadence, and p95 product frame
work. Run these contracts against an optimized product binary and choose host
or software graphics explicitly; never invent an instrumentation multiplier.

The action must last long enough to produce a distribution. Lengthen the
gesture when it yields too few samples; do not weaken the minimum merely to
make a short trace pass.

## Platform Claim

X11 is the complete and release-tested backend: native input, private display,
capture, semantic and presentation fencing, budgets, and artifacts. Product
adoptions should first make this vertical incontrovertible. The optional
headless Wayland capture smoke is not parity, and no adoption should spend
architecture on Wayland until the X11 pattern has survived the next product.

## Skill Scaffold

An app-building skill may generate the conventional crate, script, feature,
publisher lifecycle, and baseline boot scenario. It must require the author to
name the semantic state, production budgets, rich interaction, and external
oracle. Those are product decisions and must not be fabricated by middleware.

## Design Defects

No adoption may hide an unsupported behavior behind xdotool. Park the story and
name the missing shared capability instead. The present defects are:

- window move/resize, multi-window focus, native dialogs, and tray surfaces;
- AccessKit selectors that can replace app-authored target anchors;
- clipboard, IME, and text beyond the current Latin-1 injector;
- a serializable selector/action timeline shared by tests and demo recording.

Generic native Wayland input remains a known horizontal-expansion gap, but is
deliberately deferred rather than part of the present adoption contract.
