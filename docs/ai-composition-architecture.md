# AI composition architecture

AI Music separates renderable state, creative intent, deterministic supervision, and musical judgment so each can evolve without weakening the others.

## Modules and seams

```text
User natural-language instruction
             |
             v
       autopilot-engine
  Director -> Creative Brief
  Composer -> Composition Proposal
             |
             v
 CompositionSessions + ProposalReviewer
             |
             v
 ProjectEngine shadow transaction
             |
             v
 InstrumentRack -> rendered audio
             |
             v
 independent Evaluator --revise--> Composer
             |
          accept
             v
 Project + memory + outcome + WAV
```

- `autopilot-engine` is the primary product seam. Its public operation accepts
  one natural-language instruction plus the current engine, instrument rack,
  and session memory. Director, Composer, evaluator, retries, authorization,
  commits, renders, and revisions remain private implementation details. The
  whole instruction runs against cloned Project and memory state; callers see
  the result only after the complete loop succeeds.
- `music-core` is the deep renderable-state module. `ProjectEngine` owns validation, atomic patch application, revisioning, undo, and redo.
- `project-package` is the durable workspace module. A `.aimusic` directory contains `manifest.json`, the authoritative `project.json`, and fixed `assets/`, `exports/`, `renders/`, and `history/` areas. It performs atomic per-file writes, validates artifact paths, and provides rollback-on-error persistence for one Project plus its Autopilot memory, outcome, and WAV. Package-backed musical saves share an advisory package write lock; revision-scoped CLI, desktop, session, and Autopilot commits additionally compare the expected on-disk revision, so overlapping writers fail instead of silently replacing one another.
- `composition-engine` is the deep creative-supervision module. `ProposalReviewer` owns authorization and verifiable proposal review behind the two-method `review`/`apply` interface; `ArrangementAnalyzer` is a separate read-only seam that returns neutral, location-aware observations.
- `CompositionSessions` is the host-authority module. It stores immutable Authorized Tasks and exposes authorize, review, apply, revoke, and clear; callers never pass an editable scope back into model-facing review/apply operations.
- The session also stores bounded `CritiqueReport` records. A critique is tied
  to the current revision, authorized task range, and `brief_id`, receives a
  host-generated ID, and may be referenced by the next proposal without
  granting any edit capability. The independent evaluator records exactly one
  contextual `modify` or `preserve` decision for every observation. A linked
  proposal must acknowledge each stored decision and explain implementation;
  it contains no replacement disposition. Rust additionally verifies that
  every `modify` decision is backed by material patch impact at the
  observation's track/range, while `preserve` does not force an edit. The
  decision is contextual rather than a score or universal aesthetic gate.
- `audio-engine` owns the instrument seam. A track names an instrument ID; `InstrumentRack` resolves that ID to an independent stateful render session.
- CLI and Tauri are adapters. They expose the same modules and do not reimplement their rules.
- `ai-music-composer` is the expert/manual workflow and knowledge package. It guides observation,
  planning, implementation, listening, and revision; an independent evaluator
  interprets findings against the brief, while the Skill cannot grant authority
  or commit around Rust review.

## Sources of truth

The `Project` in a `.aimusic/project.json` file is the only authoritative renderable music. A Creative Brief describes the desired outcome. An Edit Scope describes permission. A Composition Plan explains intent and evidence. None of these planning artifacts silently mutate the Project. MIDI exports and audio renders are package artifacts, not alternate sources of truth.

The reviewer treats the Composition Task as a trusted capability. It can prove that a proposal stays inside the supplied scope, but it cannot prove that a model-authored scope matches an earlier natural-language request. `CompositionSessions` therefore validates and stores a private copy, returns an opaque task ID plus a read-only task snapshot, and resolves that stored copy for every later review/application. Mutating the returned snapshot cannot expand authority. A successful application consumes the task, explicit revocation disables it, and loading/newing a Project clears all task IDs.

`musicctl autopilot` and the desktop natural-language panel are the normal user
adapters. The CLI task/proposal/session commands remain expert interfaces for
debugging and alternate trusted hosts; supplying `task.json` there is an
explicit authorization act. Any hosted adapter must keep enforced scope in
`CompositionSessions` so the model never chooses it.

Current JSON Schemas are generated from Rust types through `musicctl schema`. The Skill links to those commands instead of copying a second schema. Available instrument IDs come from the same `InstrumentRack` used for rendering.

## Deterministic convergence and anti-laziness

The reviewer does not attempt to score beauty. It checks facts that can be proved from the task, proposal, project, and runtime environment:

- schema/brief validity and unique objective IDs;
- exact brief ID and base revision agreement;
- a non-empty patch when change is required;
- a material final project change when change is required (same-state/no-op operation sequences are blocked);
- optional host-authored rhythm requirements: resulting active-bar coverage,
  section/bar alignment, and explicit onset-grid adherence;
- full shadow-project patch validation before any commit;
- operation budget and capability authorization;
- allowed tracks and absolute timeline regions;
- protected-region exclusion;
- explicit authority for new tracks, track removal, and event deletion;
- available and in-scope instrument IDs;
- explicit `meter` capability for global time-signature edits;
- deterministic quantization ranges: an onset edit must remain inside the
  authorized timeline both before and after the requested grid move;
- coverage for every required objective;
- structured coverage evidence anchored to known sections/tracks/ranges and supported by actual patch impact;
- material patch impact at every linked observation whose evaluator decision
  is `modify`;
- atomic commit only after the proposal is reviewed against the current engine state.

This prevents a model from declaring success with a plan-only response, empty patch, no-op patch, stale edit, unverifiable evidence, fabricated instrument, or unauthorized/destructive shortcut. Application returns the complete review and a `ChangeSet` only when a commit occurred. A rejected application leaves the Project unchanged.

Deterministic checks cannot prove that a motif is memorable or an emotional arc succeeds. Autopilot therefore renders and invokes a separate evaluator before revision. The current evaluator receives events, arrangement observations, and numerical render analysis; it is not yet an audio-capable perceptual adapter that hears the waveform. A future audio adapter must still record contextual decisions against the brief without becoming an implicit authorization source.

`ArrangementAnalyzer` follows the same rule. Its report carries explicit
semantics that findings are advisory, absence is not a quality guarantee, and
the application may not be blocked by the report. It measures what is present
without telling a composer that repetition, regularity, asymmetry, or contrast
must be changed.

## Creativity policy

Hard constraints protect correctness, user authority, and auditability. Musical heuristics stay advisory:

- no minimum note, chord, track, or section count;
- no required tonal system, voice-leading style, form, or groove;
- silence, repetition, asymmetry, dissonance, mechanical timing, and physically impossible piano textures may all be intentional;
- complexity and density are never treated as quality scores;
- preferred objectives may be consciously left uncovered and are reported without blocking application.
- `RhythmConstraints` are the narrow exception: they are empty by default and
  become hard checks only when the host puts a concrete pulse/section contract
  in the brief. Even then they do not prescribe harmony or timbral complexity.
- meter and quantization are available creative tools, not implicit musical
  defaults. A host may authorize them when a brief asks for a metric change;
  otherwise the proposal must stay within the ordinary note/control scope.

The Composition Plan asks the model to name its formal intent, roles, evidence, and important trade-offs. A separate evaluator then records a location, audible observation, consequence, and contextual disposition. The composing model implements the host-held disposition rather than choosing whether its own work should be excused; the brief's `style_context` still prevents the evaluator from collapsing all styles into one recipe.

## Behavior boundaries

An AI composer may inspect context and bounded event windows, propose any material inside the supplied scope, use any registered/allowed instrument, and implement evaluator-directed revisions. It may not decide to ignore a stored evaluator disposition, widen scope, touch protected regions, infer deletion authority, invent resources, ignore a stale revision, bypass a rejected proposal with the low-level patch command, or claim completion without a committed change when the request was to edit music.

When a desired musical solution needs broader authority, the correct result is a request for a revised Edit Scope—not a disguised workaround.

## Interfaces

CLI:

```text
musicctl autopilot <project> "<natural-language instruction>"
musicctl context <project>
musicctl analyze-arrangement <project>
musicctl events <project> ...
musicctl schema composition-task|composition-proposal|proposal-review
musicctl review-proposal <project> <task.json> <proposal.json>
musicctl apply-proposal <project> <task.json> <proposal.json>
musicctl session <project> --task <authorized-task.json>
musicctl render|play <project> ...

Directory projects can be created with `musicctl new-project <parent> <name>`
and passed to the same Autopilot, context, edit, render, export, and session commands.
CLI patch/proposal edits record revision-scoped JSON under `history/`; these
records explain how a source revision was made but never replace
`project.json`. Autopilot commits Project, memory, outcome, and its default WAV
through package-level rollback-on-error persistence and rejects a stale
starting revision. This protects normal failure and overlapping-writer cases;
it is not yet a power-loss-safe multi-file transaction journal. Ordinary desktop edits
remain in memory until the user saves the source.
```

`musicctl autopilot` is the user-facing adapter. The task/proposal commands are
expert one-shot adapters, and `musicctl session` is an expert model-hosting
adapter: a host preauthorizes one task and exchanges versioned JSONL
requests/responses without exposing low-level patch application. Its `analyze`
request shares the same read-only `ArrangementAnalyzer` as the CLI command. It
detects external Project-file changes before servicing stateful requests,
reloads the new state, and invalidates every task ID instead of overwriting
another writer.

Tauri exposes `run_autopilot` as its normal natural-language entry point and
also retains `authorize_composition_task`, `review_authorized_proposal`,
`apply_authorized_proposal`, and `revoke_composition_task`. It deliberately does
not register critique authoring in the model-facing invoke handler; a future
desktop evaluator adapter must call the trusted host-side workspace interface.
Model-facing review/apply accept only `task_id + proposal`. Project state and
composition sessions share one workspace mutex, so authority resolution,
current-revision review, commit, and task consumption are atomic relative to
GUI edits.

Neither adapter can create a security boundary against a model that already has unrestricted process/shell access. In that deployment, the host must restrict the actual tools exposed to the model; task IDs still improve auditability and prevent accidental scope drift, but they cannot revoke operating-system authority the model already possesses.
