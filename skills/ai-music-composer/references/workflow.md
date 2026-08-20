# Composition workflow

Use this loop to converge on a musical result without narrowing creative choices prematurely.

## 1. Observe before editing

Run `musicctl context <project>` first. It gives the authoritative revision, PPQ, meter, tempo map, tracks, clips, event counts, ranges, and mixer state without flooding context with every note. `<project>` may be a loose legacy JSON file or a `.aimusic` directory; for a package, treat `project.json` as the source and keep all outputs in its fixed artifact folders.

Fetch `musicctl events <project> --track <id> --clip <id> --from <absolute-tick> --to <absolute-tick>` for the target and enough neighboring material to understand continuity. Event windows report both absolute project ticks and clip-local ticks. Plans and scope use absolute ticks; note/control commands use clip-local ticks.

The same event window also derives one-based `bar`/`beat` positions,
`tick_in_beat`, scientific MIDI pitch names such as `C4`, exact duration ratios
in quarter-note units, common duration labels when the value is exact, and
known piano pedal meanings for CC64/66/67. These fields are an AI-readable view,
not another source format. Preserve the numeric pitch, tick, duration, velocity,
controller, value, and event IDs when constructing operations. A null
`common_duration` means the expressive duration is intentionally represented
only by its exact ratio and ticks; do not silently quantize it.

Do not infer missing events from summary counts. Do not load the whole project when a bounded window is enough.

## 2. Frame the task

The task must contain a Creative Brief and Edit Scope.

In a hosted session, the host authorizes this task once and returns an opaque task ID. Keep using that ID while revising proposals. Do not send a rewritten task with the proposal. Successful application consumes the ID; request a newly authorized task for any subsequent revision.

- Split the request into independently reviewable objectives. Mark only non-negotiable outcomes `required`; use `preferred` for direction that may yield to a better musical solution.
- Put stylistic latitude in `freedoms`. Freedoms are positive permission to explore, not mandatory tricks.
- Put evaluator-relevant stylistic context in `style_context`. It should explain
  how to interpret the brief without defining one universal form, density,
  groove, or harmony as quality.
- Set the target and scope from the requested region, not from the easiest patch to write.
- Grant destructive flags only when the user authorized removal. Editing a note's velocity, position, or duration does not itself require removal authority; deleting it does.
- Use the context revision as both `scope.base_revision` and `patch.base_revision`.

If the task was supplied by another caller, do not quietly rewrite it to make a proposal pass. When only a natural-language request exists, draft the Creative Brief freely but derive the narrowest non-destructive scope that clearly follows from the selected project/track/range. Creating tracks, deleting events/tracks, or changing outside that region requires explicit authority; do not treat your own task JSON as proof that it was granted.

## 3. Compose at two resolutions

First decide macro intent: energy path, sections or phrase functions, track roles, motif/texture strategy, and where contrast or silence matters. Then decide concrete events: onset, duration, pitch, velocity, controls, and mixer changes.

Keep the plan concise but falsifiable. Each required objective needs evidence anchored to a real section, track, or range. An anchor says where the patch realizes the objective; its prose says how. Evidence may share material when one passage genuinely serves several objectives.

Before emitting JSON, audit the patch against the plan:

- every referenced track/clip exists or is created earlier in the same patch;
- all event IDs are unique and stable;
- all affected absolute ranges fit the scope and avoid protected regions;
- every operation has an authorized capability;
- instruments are available and allowed;
- the current instrument is the registered piano renderer unless the host
  explicitly authorizes another registered backend;
- claimed evidence intersects the patch's actual affected track/range;
- applying the whole sequence leaves a material final-state change, not merely a non-empty/no-op operation list;
- the proposal is musically sufficient for the required objectives, not merely non-empty.

## 4. Review, diagnose, revise

Before revising from a listening pass, `musicctl session` can return the
read-only `analyze` report. It describes observable structure but never labels
it good or bad. Treat each finding as a question to interpret in the brief;
do not converge on a uniform style merely because a measurement repeats.

After listening, an independent evaluator—not the composing model—combines
those observations with the brief, objectives, and `style_context`, then records
one `modify` or `preserve` decision per observation. A `modify` observation must
include a track or tick range. The trusted host attaches that stored report to
the composer session. The composing model only explains how it implements each
stored decision; Rust blocks `modify` when the material patch impact does not
intersect the observation location, while `preserve` requires no edit there.

For manual CLI work, run:

```text
musicctl review-proposal <project> <task.json> <proposal.json>
```

Treat `violations` as deterministic blockers. Fix their cause, not just their wording. Treat `advisories` as questions: accept, reject, or act on them according to the brief.

If review reports a stale revision, discard assumptions derived from the old state, reread context/events, update the task scope and proposal together, then review again. Do not merely replace the revision number on a patch whose musical context may have changed.

If the same class of violation repeats, inspect the Rust-generated schema and the relevant event window before trying another patch. Do not retry random JSON variants.

## 5. Commit atomically

Run the exact reviewed values through:

```text
musicctl apply-proposal <project> <task.json> <proposal.json>
```

The command reviews again at commit time. A blocked application leaves the project unchanged. Do not fall back to `apply-patch` when a proposal is rejected.

In the hosted desktop adapter, call `review_authorized_proposal(task_id, proposal)` and `apply_authorized_proposal(task_id, proposal)` instead. The host resolves its private task copy and consumes the task ID after a successful commit.

## 6. Hear and verify

Render or play the result. Listen at least once for the edited passage in context, not only in isolation. Evaluate concrete issues such as pulse clarity, phrase direction, register masking, attacks, releases, pedal blur, dynamic shape, and the intended contrast/continuity.

After listening, name one specific mismatch before making another revision. Re-enter the full loop with the new project revision. A revision should have a musical reason, not merely make the output different.

Finish by rereading context and the changed event window. Report what was committed, which objectives it serves, and any consciously accepted advisories or listening limitations.
