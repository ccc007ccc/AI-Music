# AI Music Composition

This context describes how a creative request becomes an authorized, reviewable change to a renderable musical project.

## Automatic interaction

**Autopilot Instruction**:
The user's natural-language request to create or revise the current Project. It
is the primary product input; the user does not author tasks, proposals, or
critique decisions.
_Avoid_: Task JSON, proposal handoff, creator approval step

**Autopilot Session**:
Persistent history tying successive natural-language instructions to the same
Project. It lets a later instruction such as “make the second half more driven”
continue from current events and prior outcomes without exposing internal role
messages.
_Avoid_: A model transcript, permanent edit authority

**Director**:
An isolated model role that translates the latest instruction, current Project,
and Autopilot Session into a bounded Creative Brief. It cannot edit the Project.
_Avoid_: User form, composer self-authorization

**Composer**:
An isolated model role that produces a concrete Composition Proposal for an
immutable Authorized Task. It implements evaluator decisions but cannot choose
or rewrite their dispositions.
_Avoid_: Evaluator, direct Project writer

**Evaluator**:
An isolated model role that judges the committed revision against the user's
instruction, Creative Brief, style context, Project events, neutral arrangement
facts, and rendered-audio measurements. It accepts or records contextual
`modify`/`preserve` decisions; it does not grant edit authority.
_Avoid_: Universal taste score, composer veto, human approval prompt

**Autopilot Outcome**:
The durable summary of one completed instruction, including committed revisions,
evaluation rounds, final render measurements, and final status. The editable
Project remains the source of truth and the WAV remains a derived artifact.
_Avoid_: Intermediate model JSON exposed as the product workflow

## Renderable music

**Project**:
The authoritative, renderable musical state containing tracks, clips, MIDI events, timing, and mixer settings.
_Avoid_: Song document, AI output

**Project Package**:
A directory bundle ending in `.aimusic` that owns one `project.json` source plus its
assets, exports, renders, and history directories. The package is the durable
workspace boundary; `project.json` remains the only musical source of truth,
while `manifest.json` records identity and source-asset bindings.
_Avoid_: A loose collection of generated files, a rendered WAV as source

**Source Asset**:
A licensed or user-provided instrument/resource referenced from the package's
`assets/` directory or an external asset manifest. The package manifest records
the role, asset ID, manifest location, license source, and attribution so a
reopened project does not silently switch timbre.
_Avoid_: An implicit download, an untracked sample

**Render Artifact**:
A derived WAV/MP3 or stem written under `renders/` and never used as the
authoritative musical state.
_Avoid_: Replacing the Project with audio

**Export Artifact**:
An interchange file such as MIDI written under `exports/`.
_Avoid_: The editable Project

**Event Patch**:
An atomic set of concrete edits to a Project, guarded by its base revision.
_Avoid_: Rewrite, answer

**Time Signature**:
The Project's explicit numerator/denominator meter used to derive beat and bar
tick lengths. Changing it is a global edit requiring explicit meter authority.
_Avoid_: A visual ruler preference

**Quantization**:
A bounded, deterministic movement of selected note onsets toward a chosen tick
grid with an explicit strength. It preserves duration, velocity, and controls;
it is a performance edit, not an aesthetic quality score.
_Avoid_: A rule that every note must be on-grid

**AI Event View**:
A lossless, read-only projection of a bounded Clip window. It pairs the
authoritative IDs and ticks with derived bar/beat coordinates, MIDI pitch names,
exact quarter-note duration ratios, common duration labels, and known piano
pedal meanings. Derived labels help reasoning but are never accepted as a
second editable music format.
_Avoid_: Simplified composition JSON, normalized notes

## Creative intent

**Creative Brief**:
The user's requested outcome, expressed as required and preferred objectives,
explicitly granted freedoms, and optional `style_context` used by an independent
evaluator and composing model. Style context is not a universal quality recipe.
_Avoid_: Prompt, specification

**Composition Task**:
A user-authorized pairing of a Creative Brief and an Edit Scope presented to a composing model for one proposed change; an independent evaluator may later assess the rendered result against the same brief.
_Avoid_: Prompt, self-granted permission

**Authorized Task**:
An immutable, host-held Composition Task identified by an opaque ID and available for proposal revisions until it is consumed or revoked.
_Avoid_: Task JSON supplied by the composer, reusable permission

**Composition Session**:
The bounded lifecycle in which one Authorized Task can be reviewed, revised, committed once, revoked, or invalidated by replacing the Project.
_Avoid_: Chat session, permanent permission

**Objective**:
One independently reviewable musical outcome in a Creative Brief.
_Avoid_: Instruction, checkbox

**Composition Plan**:
The proposed formal structure, track roles, and intentional decisions that explain how objectives will be realized before event edits are committed.
_Avoid_: Patch, chain of thought

**Creative Decision**:
A concise record of a consequential musical choice and why it serves the Creative Brief.
_Avoid_: Chain of thought, hidden reasoning

**Proposal**:
A Composition Plan paired with the Event Patch intended to realize it.
_Avoid_: Completion, draft project

**Critique Report**:
A bounded, structured listening record tied to one Project revision and
Authorized Task. Its observations identify a location, what was heard, the
musical consequence, and an optional proposed revision. An independent
evaluator also records one `modify` or `preserve` decision and rationale for
each observation, using the brief's style context rather than a universal
quality rule. The host assigns an opaque ID that a later Proposal may
reference.
_Avoid_: Permission, quality score, creator opt-in/opt-out, free-form praise

**Arrangement Report**:
A read-only, deterministic projection of measurable arrangement facts such as
repeated onset/duration shapes, neighboring-bar changes, contour intervals, and
repeated expression controls. Its findings are advisory observations, never
quality scores, style rules, or application gates; the absence of a finding is
not evidence of quality.
_Avoid_: Automated taste verdict, required correction

**Critique Response**:
A Proposal's acknowledgement of the independent evaluator's already-recorded
`modify` or `preserve` decision for one observation in a linked Critique
Report. It contains the observation ID and a rationale describing
implementation, not another disposition. The composing model cannot override
the decision; Rust requires material patch impact at the observation's
track/range for `modify`, while `preserve` does not force an edit. The
evaluator's decision is still contextual and does not encode a universal
aesthetic score or recipe.
_Avoid_: Creator veto, automatic acceptance, aesthetic score

## Authorization and review

**Edit Scope**:
The explicit authorization envelope defining which tracks, timeline regions, and kinds of change a Proposal may affect.
_Avoid_: Sandbox, permissions prompt

**Protected Region**:
A track and timeline range that must remain unchanged by a Proposal.
_Avoid_: Locked clip

**Proposal Review**:
A deterministic assessment of a Proposal against the Project, Creative Brief, Edit Scope, and available instruments.
_Avoid_: Taste score, approval

**Violation**:
A correctness, authorization, or required-objective failure that prevents a Proposal from being committed.
_Avoid_: Warning, bad music

**Advisory**:
A non-blocking musical observation intended to improve a Proposal without dictating style.
_Avoid_: Error, rule

**Objective Coverage**:
The explicit link from a Composition Plan to the objectives it claims to realize.
_Avoid_: Note count, complexity score

**Rhythm Constraints**:
Optional, host-authored requirements for resulting onset grids, bar-aligned
planned sections, or a minimum number of active target bars. An empty set leaves
timing and form open to the composer.
_Avoid_: Hidden quantization or a universal minimum-density rule
