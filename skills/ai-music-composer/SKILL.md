---
name: ai-music-composer
description: Plan, inspect, review, and apply authorized MIDI-first composition changes in the AI Music project. Use when composing, arranging, revising, or critiquing music through musicctl; do not use for audio-engine implementation or unrestricted file rewrites.
---

# AI Music Composer

Turn a musical request into a deliberate, reviewable `CompositionProposal`, then commit it only through the Rust supervision seam.

This is the expert/manual composition skill. For ordinary product use, prefer
the fully automatic `musicctl autopilot <project> "<instruction>"` entry point
or the desktop “AI 自动创作” panel. Autopilot owns Director, Composer, evaluator,
automatic revision, persistence, and rollback internally; do not ask the user
to create task/proposal/critique JSON or approve intermediate judgments.

This skill is currently specialized for one instrument: the registered `piano`
renderer. Create contrast through piano register, voicing, velocity, timing,
articulation, tempo, and pedal rather than adding placeholder instruments. The
instrument seam remains extensible, but multi-instrument orchestration is not a
current completion criterion.

## Working contract

- Treat the Project as the renderable source of truth, the Creative Brief as intent, and the Edit Scope as authority.
- Treat a supplied Composition Task as immutable user/host authority. Never broaden or rewrite its scope to make a proposal pass; the model may propose broader authority, but only the user/host may grant it.
- In hosted sessions, use the opaque Authorized Task ID for review/application. The readable task snapshot is planning context, not a value to send back or modify as authority.
- Satisfy every required objective. Preferred objectives and musical conventions are judgment calls, not hidden hard rules.
- Never expand track, timeline, instrument, destructive, or operation authority. Ask for a revised task when the desired change needs broader authority.
- Use the stable `musicctl` commands and Rust-generated schemas. Do not write a
  one-off Python/JavaScript MIDI generator or bypass the Project/Proposal
  reviewer with an ad-hoc file rewrite.
- Never bypass `review-proposal` with `apply-patch`. Commit composition work only with `apply-proposal` after the exact proposal reviews as `ready`.
- Do not claim objective coverage without section, track, or tick-range anchors supported by the actual patch.
- Do not use note count, density, harmonic complexity, or conventional form as a proxy for quality. Intentional silence, repetition, asymmetry, dissonance, and minimalism are valid.
- Use `set_time_signature` or `quantize_notes` only when the brief calls for a
  meter/timing edit. `meter` is a separate capability; quantization is a
  reversible onset operation with explicit grid and strength, never a hidden
  requirement that every note be on-grid.
- Treat `CreativeBrief.rhythm` as host authority. Its optional active-bar or
  section-alignment requirements can prevent a deliberately requested shortcut,
  but an empty value keeps the full rhythmic and formal design space open.
- If the user asked for an actual edit, do not stop after analysis or a plan. Produce and review a concrete proposal unless authority or required context is missing.

## Workflow

Read [references/workflow.md](references/workflow.md) and follow its convergence loop for every composition or revision task.

For a host-managed AI session, read [references/session.md](references/session.md) as well. It defines the task-ID lifecycle and the separation between model proposal tools and trusted GUI editing.

Before writing events:

1. Read compact project context, then only the event windows needed for the target. For arrangement or revision work, also read the neutral `analyze` report as evidence, never as a score or mandatory to-do list.
2. Confirm the brief, scope, current revision, available tracks/clips, PPQ, meter, and instruments.
3. Form a plan whose sections, track roles, objective evidence, and creative decisions are concrete enough to audit.
4. Create one atomic proposal against the current revision.

Event windows include a lossless AI-readable projection: bar/beat positions,
pitch names, exact duration ratios, familiar duration labels, and piano pedal
meanings alongside authoritative IDs and ticks. Use the readable fields to
inspect musical relationships, but emit operations from the numeric fields and
never round an unusual value merely because it has no common label.

After a render/listen pass, record a bounded structured critique through the
host session before revising. Include the task's `brief_id`; anchor each
observation to a track, tick range, or named passage; state the audible
consequence and one testable revision. An independent evaluator must attach
one contextual `modify` or `preserve` decision and rationale to every
observation, using the brief's style context rather than a universal threshold.
Every `modify` decision needs a track or tick range that Rust can verify; a
named passage alone is insufficient for a required change.
Link the next proposal with the returned `based_on_critique_id`. A critique is
diagnostic evidence, not permission to widen scope. Answer every linked
observation in `critique_responses` with its observation ID and a rationale
describing implementation; do not submit another disposition. Rust requires
material patch impact at the observation's track/range for `modify`.
`preserve` requires acknowledgement but no artificial edit, so intentional
repetition, mechanical timing, asymmetry, or abrupt contrast can remain when
the evaluator's rationale supports the brief.
When the host has attached a critique to the composer session, omitting the
link is rejected; this is not a creator opt-out. A first proposal may omit it
only when no critique has been attached.

When the project path is a `.aimusic` directory, keep all derived files inside
the package: export MIDI to `exports/`, render audio to `renders/`, and place
proposal/critique history under `history/`. `project.json` is the only source
that may be edited or rendered as authoritative state.

Use [references/proposal-format.md](references/proposal-format.md) when constructing task/proposal JSON or diagnosing a review finding. Generate current JSON Schemas from Rust; never maintain a second handwritten schema.

## Musical decisions

Read only the references relevant to the requested work:

- [references/rhythm.md](references/rhythm.md) for pulse, meter, groove, phrase rhythm, and timing variation.
- [references/piano-writing.md](references/piano-writing.md) for register, texture, voicing, articulation, dynamics, and pedal.
- [references/form-and-development.md](references/form-and-development.md) for motifs, contrast, continuity, sections, and revision choices.

These references are creative lenses. Depart from them when that better serves the brief, and record important departures as creative decisions rather than hiding them.

The external `midi-agent-skill` project is a useful workflow reference, not a
schema or rule source for this skill. Its strongest ideas for us are stable
deterministic tools, structured input normalization, and progressive disclosure
of knowledge. We intentionally do not import its GM-128-instrument target,
absolute dissonance bans, silent fallback behavior, or note-count/length
padding. See [the comparison report](../../research/midi-agent-skill-review.md)
when auditing this boundary.

The condensed guidance is backed by the source trail in [the project research report](../../research/composition-knowledge.md). Read it when auditing, extending, or challenging the musical guidance; ordinary composition does not require loading the full report.

## Completion

A composition change is complete only when:

- Rust review returns `ready` with no violations;
- `apply-proposal` returns a committed change;
- the result has been rendered or played when the environment permits listening;
- a final context/event read confirms the intended region changed and unrelated protected material did not;
- any remaining advisories are consciously accepted, not silently ignored.

Stop iterating when the required objectives are met and further edits have no concrete brief-related purpose. Do not polish indefinitely.
