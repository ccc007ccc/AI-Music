# Proposal format and review findings

Rust types are authoritative. Generate current schemas instead of copying field lists into prompts or references:

```text
musicctl schema composition-task
musicctl schema composition-proposal
musicctl schema critique-report
musicctl schema stored-critique
musicctl schema proposal-review
musicctl schema authorized-composition-task
musicctl schema task-authorization
musicctl schema session-request
musicctl schema session-response
musicctl schema patch
musicctl schema clip-window
musicctl schema project-summary
```

Use `-` in place of one JSON path to read that document from standard input. A task and proposal cannot both use standard input in the same command.

## Structural expectations

A `CompositionTask` pairs:

- a brief ID, summary, absolute target tick range, required/preferred objectives,
  freedoms, contextual `style_context`, and whether change is required;
- optional host-authored `rhythm` constraints. An empty `rhythm` object is
  deliberately permissive. When present, `onset_grid_tick` applies only to
  newly created or moved onsets, `require_bar_aligned_sections` checks plan
  boundaries, and `minimum_active_bars` checks the resulting target material;
- an edit scope with base revision, track access, absolute timeline ranges, capabilities, protected regions, allowed instruments, destructive flags, and operation budget.

A `CompositionProposal` pairs:

- the matching brief ID;
- a plan with a summary, optional formal sections, track roles, objective coverage, and explicit creative decisions;
- an atomic Project patch.
- It may include `based_on_critique_id` when the proposal is a response to a
  host-recorded listening critique in the same authorized session.
- When that link is present, include one `critique_responses` entry per
  critique observation. Each entry contains only `observation_id` and a
  rationale explaining implementation. The independent evaluator's stored
  `modify` or `preserve` disposition is not a proposal field and cannot be
  submitted or overridden. Rust requires material patch impact at the observation's
  track/range for `modify`; `preserve` requires no edit and does not imply that
  repetition or rigidity is musically wrong.

In a session with a host-attached critique, `based_on_critique_id` is required;
the composing model cannot omit it to avoid the evaluator's decisions. A
proposal with no critique link is valid only before any critique is attached.

A `CritiqueReport` is separate from the musical source and has a `brief_id`,
base revision, concise summary, bounded observations, and one independent
evaluator decision for each observation. Every observation uses a concrete
location plus `observation` and `consequence`; subjective judgments remain
contextual rather than universal, but vague unanchored prose is rejected.
An observation decided as `modify` must include a track or absolute tick range;
a label alone cannot support deterministic execution verification.
Record it through the session `critique` request before linking its
host-assigned ID in a proposal.

Objective evidence must have a non-empty description and at least one anchor: `section_id`, `track_id`, or absolute `range`. Prefer a track plus a narrow range when possible. The reviewer verifies that references exist and that the patch actually affects the claimed track/range. It cannot prove a subjective sentence true, so make the description precise and inspect the rendered result.

## Coordinates

- Brief, scope, protected regions, planned sections, and evidence ranges use absolute Project ticks.
- `add_note.note.start_tick`, `move_note.start_tick`, and control ticks use clip-local ticks.
- `quantize_notes.start_tick` and `quantize_notes.end_tick` use clip-local ticks;
  the reviewer checks both the old and possible new sounding ranges.
- Absolute event tick equals `clip.start_tick + local tick`.
- One quarter note equals the Project's `ppq`. For a denominator `d`, one notated meter beat is `ppq * 4 / d`; one bar is that value times the numerator.
- Event-window `start_position`, `end_position`, and control `position` are
  one-based bar/beat coordinates derived from absolute ticks. `tick_in_beat`
  preserves off-grid timing exactly.
- `pitch_name`, `duration_quarters`, `common_duration`, and control `meaning`
  are read-only aids. Patch operations still use numeric MIDI pitch, clip-local
  ticks, duration ticks, velocity, controller, and value.

The shared edit commands include `set_time_signature` and `quantize_notes`.
Changing meter requires the explicit `meter` capability because it changes the
global bar interpretation. Quantization moves only note onsets in its selected
clip-local range; duration, velocity, pedal/control events, and notes whose
nearest target would leave the range are preserved. `strength` from 0 to 100
blends toward the nearest `grid_tick`, so human timing can be retained rather
than forcing every performance onto a grid.

Do not assume PPQ is 960 even though it is the current default.

## Finding classes

Schema, brief, objective, revision, patch, capability, scope, protection, destructive-action, and instrument findings are blockers. Resolve them before application.

A change-required brief must include at least one required objective. Coverage entries are unique per objective, and blank creative decisions do not count as accountability evidence. A non-empty operation list is not enough: if the final project equals the starting project, review returns `no_material_change`. If a linked `modify` decision is answered only in prose or by an unrelated patch, review returns `unimplemented_critique_decision`.

`preferred_objective_uncovered`, `no_plan_sections`, and `no_creative_decisions` are advisories. They flag potentially weak process evidence, not bad music. A deliberately through-composed, minimal, or single-gesture result may accept them.

The review `metrics` are audit facts: operation count, affected tracks, coverage counts, and destructive/create counts. They are not quality scores.

Rhythm requirements are an explicit anti-shortcut mechanism, not a default
complexity score. They should be added only when the user asks for a pulse,
bar-count, or section-shape contract. They do not require a particular harmony,
instrument count, density, or stylistic vocabulary.
