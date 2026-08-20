# Roadmap and known limitations

AI Music 0.1.0 proves the complete unattended workflow, but it is not the end
state. These items are deliberately public so “fully automatic” is not confused
with “production-complete.”

## Highest priority

- **Direct audio perception.** The Evaluator currently receives MIDI events,
  neutral arrangement observations, and numeric WAV measurements. It does not
  receive the waveform through an audio-capable model, so timbral artifacts and
  nuanced performance quality can escape evaluation.
- **Crash recovery journal.** Package artifact writes are atomic per file and
  roll back ordinary write errors. A process or power loss between file renames
  can still leave a mixed generation. Add a durable transaction journal or
  generation-directory swap with recovery on open.
- **Large-composition generation.** The Composer currently returns complete
  low-level proposal JSON. Long, dense arrangements consume large context and
  output budgets. Add a host-owned musical construction interface or bounded
  section-by-section generation while preserving one final atomic commit.

## Reliability and operations

- Track the upstream GTK3/glib advisory inherited by Tauri's Linux WebKitGTK
  stack and migrate when a supported patched dependency path is available
  ([#3](https://github.com/ccc007ccc/AI-Music/issues/3)). The affected
  `glib::VariantStrIter` API is not called by this repository.
- Replace the current fail-fast provider behavior with bounded retry/backoff,
  provider health classification, and configurable fallback adapters.
- Add cancellation and progress events for long desktop Autopilot jobs.
- Add resumable checkpoints that preserve trust and revision invariants without
  committing half-finished music.
- Add automated migration for future Project, package, memory, and outcome
  schema versions. Autopilot memory currently resets when its schema version is
  unsupported.
- Extend compare-and-swap persistence to loose legacy `.json` projects and
  manifest/source-binding mutations. Package-backed musical edits and Autopilot
  commits use the shared package lock and expected revision in 0.1.0.

## Musical capability

- Add structural semantic memory for motifs, sections, user preferences, and
  explicit preserve constraints; current prompt memory stores concise turn
  summaries and the latest twelve turns.
- Support more instruments, automation, effects, stems, mastering measurements,
  and richer mix evaluation.
- Add deterministic regression fixtures for longer multi-section pieces and
  cross-renderer comparisons.

## Distribution

- Add signed desktop packages and release automation for supported Linux
  distributions.
- Add CI matrices for the documented minimum glibc baselines and packaged
  `.deb`/`.rpm` installation tests.
