# ADR 0008: Keep the piano engine Rust-native

## Status

Accepted.

## Context

Mature piano and sampler projects exist in C and C++, and they are useful
research references. Linking one through FFI would, however, add another build
system, runtime library boundary, realtime ownership model, and license surface.
It would also make the phrase "pure Rust piano engine" misleading.

The desktop application still has unavoidable platform adapters: CPAL reaches
the operating system audio API, while Tauri uses the system WebView. The Tauri
UI itself is HTML, CSS, and JavaScript. Those boundaries are distinct from the
music, sampler, and rendering implementation.

## Decision

All repository-owned project, MIDI, composition-review, piano synthesis, SF2
adapter, SFZ preprocessing, sample decoding, voice allocation, mixing, and WAV
rendering code is written in Rust. The workspace forbids `unsafe` in its own
crates. No C or C++ sampler is linked through FFI, and no such backend is on the
product roadmap.

C/C++ projects may be cited as research or run separately by a developer as a
listening oracle. They do not participate in a project render, desktop build,
or artifact reproducibility. New instrument backends must implement the Rust
`Instrument`/`InstrumentSession` seam.

## Consequences

- The audio and composition core has one language, ownership model, and build
  graph.
- SFZ compatibility grows explicitly inside the strict Rust implementation;
  unsupported semantics remain load errors rather than falling through to a
  native sampler.
- A desktop executable is not "zero native system libraries": audio and WebView
  adapters still depend on the target operating system.
- Replacing Tauri with an all-Rust widget toolkit would be a separate UI choice;
  it is not required to keep the piano engine Rust-native.
