# CLI composition session protocol

`musicctl session` is a provider-neutral JSON Lines adapter around `CompositionSessions`. It lets a host keep authorization private while evaluator and composer model invocations exchange ordinary JSON over stdin/stdout. The role is selected by the trusted process launcher, not by the model.

## Start

```text
musicctl session song.json --task authorized-task.json --role evaluator
# evaluator output can be attached to a composer-side session:
musicctl session song.json --task authorized-task.json --role composer --critique evaluator-report.json
```

The host-selected task and any `--critique` attachments are validated before
requests are read or an authorization-success line is emitted. Invalid
attachments terminate startup without first announcing a usable task. The
first stdout line of a successfully initialized process is a `SessionResponse`
whose result is a `TaskAuthorization`. Subsequent stdin lines contain one
`SessionRequest`; each produces exactly one stdout line. Generate the current
wire schemas with:

```text
musicctl schema session-request
musicctl schema session-response
musicctl schema composition-proposal
musicctl schema critique-report
musicctl schema arrangement-report
```

Every response has either `{"ok":true,"result":...}` or `{"ok":false,"error":{"code":"...","message":"..."}}`.

The trusted launcher selects one role for a process:

| role | may read/analyze | may submit `critique` | may review/apply/lifecycle |
| --- | --- | --- | --- |
| `evaluator` | yes | yes | no |
| `composer` | yes | no | yes |

`--critique <stored-critique.json>` is accepted only for a composer process;
the host attaches those reports to the freshly authorized task before stdin is
read. This prevents the composing model from authoring the decision it is
supposed to implement. Once a report is attached, the composer must link it in
the proposal; omitting `based_on_critique_id` is not a creator opt-out.

## Example

```json
{"op":"context"}
{"op":"analyze"}
{"op":"events","track_id":"piano","clip_id":"piano-main","start_tick":0,"end_tick":3840}
{"op":"critique","task_id":"composition-task-...","report":{"brief_id":"opening-brief","base_revision":0,"summary":"...","observations":[{"id":"opening-attack","location":{"track_id":"piano","range":{"start_tick":0,"end_tick":960}},"observation":"...","consequence":"...","proposed_revision":"..."}],"decisions":[{"observation_id":"opening-attack","disposition":"modify","rationale":"The evaluator selects a focused revision for the brief"}],"next_focus":"..."}}
{"op":"review","task_id":"composition-task-...","proposal":{}}
{"op":"apply","task_id":"composition-task-...","proposal":{}}
{"op":"revoke","task_id":"composition-task-..."}
```

`apply` saves the Project only when the returned application contains a change. A blocked proposal leaves both the Project and active task unchanged so another proposal revision can be reviewed. A successful application consumes the task ID.

`critique` records a listening pass without changing the Project or consuming
authority. The host assigns an opaque critique ID and writes a package history
artifact when the project is a directory bundle. The report must use the
current revision, match the authorized task's `brief_id`, and each observation
must identify a label, track, or range inside the authorized task. An
independent evaluator must provide exactly one `modify` or `preserve` decision
and bounded rationale for every observation. A later proposal may set
`based_on_critique_id` to prove which critique it addresses; unknown critique
IDs are rejected before proposal review. A `modify` observation must include a
track or tick range so execution can be checked; a label alone is insufficient.

`analyze` returns a deterministic `ArrangementReport` for the current Project.
It reports measurable facts such as repeated onset/duration shapes, changes at
bar boundaries, contour intervals, and repeated expression controls. The
report's `semantics` explicitly marks these findings as advisory: they are not
quality scores, do not prescribe a correction, and cannot block an application.
Use the report as one observation source alongside actual listening.

When a proposal links `based_on_critique_id`, its `critique_responses` must
contain exactly one response for every observation in that critique. Each
response contains the observation ID and a rationale describing implementation;
it has no disposition field. The composing model therefore cannot override the
stored decision. For `modify`, Rust review also requires a material patch
impact intersecting the observation's track/range. `preserve` requires no edit
at that location and remains valid for intentional repetition, rigidity,
asymmetry, or any other stylistic choice supported by the brief's
`style_context`.

If the composer session has any attached critique, a proposal without
`based_on_critique_id` is rejected before deterministic proposal review. A
first proposal is still allowed when no critique has been attached.

`reload` rereads the Project file and clears all task IDs. The session also compares the semantic on-disk Project before context/event/review/apply/authorize requests. If another process changed it, the request returns `project_changed`, the new Project is loaded, and all old authority is invalidated before anything can be overwritten.

## Authorization boundary

By default, an `authorize` request on stdin returns `authorization_denied`. A trusted host may start the process with `--allow-authorize`, but this option must not be exposed to the model as a tool parameter. Prefer preauthorization through `--task`.

This protocol is a capability boundary only when the host controls process launch and exposes the running stdin/stdout channel rather than an unrestricted shell. A shell-capable model could invoke low-level commands or start a new process with different flags; such an environment must rely on its own authorization policy. The session protocol does not pretend to sandbox a model that already controls the operating system account.
