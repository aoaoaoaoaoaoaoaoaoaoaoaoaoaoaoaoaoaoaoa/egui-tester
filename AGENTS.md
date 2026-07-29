# Project Instructions

Read and follow the canonical
[Rust style doctrine](/home/main/programming/projects/rust_starter/docs/rust-style-doctrine.md)
before making meaningful Rust style decisions.

This project treats containment claims as executable contracts. Never weaken a
hermeticity failure into a warning or silently fall back to the caller's
desktop, home directory, session bus, network namespace, or writable host
filesystem.
