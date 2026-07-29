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

Frame-built witnesses precede presentation. Therefore a witness transition may
release a subsequent pixel-change wait, but must never substitute for it.

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
