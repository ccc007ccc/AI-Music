# Contributing

Thanks for helping improve AI Music. The repository is a Rust workspace with a
Tauri desktop adapter and a MIDI-first project format.

## Development environment

Install a current stable Rust toolchain. Linux desktop builds additionally need
the platform packages required by Tauri 2, WebKitGTK, GTK, ALSA, and PipeWire.
Core crates and `musicctl` can be developed without launching the desktop app.

Run the repository gates before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Design constraints

- Keep `project.json` as the only authoritative renderable music source.
- Route edits through `ProjectEngine` commands or patches; do not let adapters
  mutate project fields directly.
- Keep model roles outside the trusted write path. Model output must be
  structured, reviewed, revision-scoped, and committed by Rust.
- Do not introduce aesthetic rules as universal validation gates. Deterministic
  validation protects correctness and authority; musical judgment remains
  contextual.
- Do not commit licensed SoundFonts, sample libraries, generated WAV files,
  credentials, or local provider configuration.
- Workspace crates forbid `unsafe` code.

Architecture details live in
[`docs/ai-composition-architecture.md`](docs/ai-composition-architecture.md).

## Pull requests

Keep changes focused, add tests at the public module interface, and explain any
format, schema, or compatibility impact. Security-sensitive findings should be
reported privately according to [`SECURITY.md`](SECURITY.md).
