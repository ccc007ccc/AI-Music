# ADR 0006: Reject unsupported SFZ semantics at load time

## Status

Accepted.

## Context

The sampled-piano backend needs an incremental path from small, testable SFZ
instruments to Salamander Grand Piano V3. SFZ parsers may recognize tokens
without implementing preprocessing, header inheritance, or the corresponding
playback behavior. Silently dropping one of those semantics can produce an
audible file with the wrong samples, velocity layers, tuning, or release
behavior, which is more dangerous than a clear load failure.

The audio thread must also remain independent of file parsing and decoding.
Asset formats should not leak into Project, AI proposal, CLI, or GUI models.

## Decision

`SfzPiano::from_asset_pack` accepts a strict piano-oriented subset based on
`<control>`, `<global>`, `<group>`, and `<region>`. It preloads referenced WAV
and FLAC samples, resolves every path inside the validated asset pack, and
converts supported opcodes into immutable sample regions before any
`InstrumentSession` is created.

Directives, variables, unsupported headers, and unsupported playback opcodes
produce an error with their source line. The implemented preprocessing layer
expands `#define` and recursive `#include` relative to the main SFZ file,
then the parser applies `<global>`, `<master>`, and `<group>` inheritance before
decoding samples. Supported ARIA-style controls are represented explicitly
(initial CC values, conditions, curves, amplitude/pan/velocity/release/offset
modulation); unsupported semantics still fail instead of being ignored.
`InstrumentRack::from_asset_pack` owns backend selection, so callers only
depend on the existing Instrument seam.

## Consequences

- Flat multi-velocity SFZ pianos can render through the same scheduler and
  mixer as built-in and SF2 pianos.
- Salamander's definition can be structurally expanded and validated, while
  the sample pack remains an external, license-tracked asset. `key=-1`
  mechanical pedal regions are represented as CC-triggered voices rather than
  ordinary pitched regions.
- Sample decoding and allocation occur on the control thread; sessions render
  from shared immutable sample memory without file I/O.
- Adding an opcode requires an explicit parser mapping, playback behavior, and
  regression test. Unknown data cannot silently change the sound.
- Preloading a complete large piano may use too much RAM. A later cache or disk
  streaming policy can be added inside this module without changing Project or
  AI interfaces.
