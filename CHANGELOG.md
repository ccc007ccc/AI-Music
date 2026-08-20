# Changelog

All notable changes to this project will be documented here.

## 0.1.0 - 2026-08-20

- Added the natural-language Autopilot workflow with isolated Director,
  Composer, and Evaluator roles.
- Added deterministic Rust authorization, proposal review, revision-scoped
  commits, automatic render/evaluation/revision, and whole-instruction rollback.
- Added persistent Autopilot session memory for follow-up natural-language
  edits.
- Added `.aimusic` directory packages with project, asset, export, render, and
  history areas plus rollback-on-error artifact persistence.
- Added compare-and-swap package commits for Autopilot so concurrent writers are
  rejected rather than silently overwritten.
- Added MIDI import/export, the built-in physical-model piano, optional pure
  Rust SF2/SFZ backends, WAV rendering, playback, CLI, and Tauri desktop UI.
