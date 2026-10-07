egui_tester: hermetic black-box acceptance testing for native egui apps

## invariants
- Containment claims are executable contracts; never weaken hermeticity failure into warning
- Never silently fall back to caller's desktop, home dir, session bus, network namespace, or writable host fs
