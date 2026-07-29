# Application Adoption

## Durable Shape

Each product owns a thin, unpublished `<product>-acceptance` executable. It
depends on `egui-tester`, not on product crates. Its only product knowledge is
fixture seeding, witness predicates, performance budgets, and real oracles.
Containment, display input, synchronization, timing, transcripts, and failure
artifacts remain library responsibilities.

The product exposes an `egui-test` feature that adds
`egui-tester-witness`. This feature is telemetry, never a control plane. The
acceptance executable must still launch the real optimized product binary and
drive native input.

A conventional repository provides:

```text
crates/<product>-acceptance/
scripts/test-gui
```

`scripts/test-gui` builds the product in release mode with `egui-test`, then
runs the acceptance executable. The acceptance executable derives the sibling
product binary by default, accepts an artifact directory, and uses
`TestbedBuilder::failure_artifacts`.

## Adapter Lifecycle

One application frame follows this order:

1. clear the preceding pass's anchors;
2. run ordinary product UI and state work;
3. capture `ProductInstant::now()` as the observation endpoint;
4. tessellate, render, and present normally;
5. capture another `ProductInstant::now()` as the presentation endpoint;
6. extract anchors and the smallest useful semantic state;
7. call `Publisher::present_at`.

The last three telemetry operations occur after presentation so their cost
cannot pollute either performance endpoint. Publish nothing when surface
acquisition fails or no frame was presented.

Anchors are stable intent names such as `library.rename` or
`editor.support/1`, expressed in physical window-relative pixels. Do not name
layout positions or implementation types. Semantic state should expose facts
needed for synchronization, not duplicate the product model wholesale.

## Scenario Law

Every acceptance step has three distinct layers:

1. a native gesture through `X11Session`;
2. a fresh, frame-coherent witness predicate;
3. a rendered or external product oracle.

Witness state may prove that a route was recomputed; persisted geometry or
pixels decide whether the product worked. Require controls belonging to the
new state in transition predicates, for example `view == "edit"` together
with `editor.support/1`.

Ordinary widgets may use `X11Session::drag`. Custom canvas gestures should
compose held operations:

1. `button_down` on the witnessed target;
2. wait until the product witnesses target acquisition;
3. `move_to` the destination and wait for the semantic/rendered result;
4. `button_up` and wait for release.

This removes scheduler-dependent sleeps and tests the same capture law a user
depends on.

## Minimum Dogfood

The first scenario for an application should cover:

1. cold boot to a real presented frame;
2. one ordinary navigation or control transition;
3. one durable mutation checked outside the witness;
4. one application-defining rich interaction;
5. at least one input-to-presentation budget;
6. a final screenshot and automatic failure bundle.

Trailgen supplies the reference rich interaction: rename a saved trail, enter
its editor, acquire pin 1, drag it to another graph branch, prove a different
route signature was presented within budget, save, and compare durable
support points and leg geometry.

## Skill Scaffold

An app-building skill may generate the conventional crate, script, feature,
publisher lifecycle, and baseline boot scenario. It must require the author to
name the semantic state, production budgets, rich interaction, and external
oracle. Those are product decisions and must not be fabricated by middleware.
