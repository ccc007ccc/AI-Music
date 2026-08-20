# ADR 0005: Keep listening critiques advisory and host-scoped

## Status

Accepted.

## Context

The composition loop needs a durable bridge from rendering/listening to a
specific revision proposal. Free-form chat is difficult to audit, while a
subjective score or automatic aesthetic gate would narrow valid musical choices
and could be gamed as easily as note-count targets.

A model-authored critique must also not become a way to widen an Authorized
Task. It can describe a problem and propose an experiment, but only the host can
grant tracks, timeline ranges, capabilities, destructive actions, or operation
budget.

The composing model must not be made responsible for deciding whether the
system's own critique should be ignored. That would turn the quality loop into
an opt-in checkbox and make the creator absorb the evaluator's job.

## Decision

`CompositionSessions` accepts a structured `CritiqueReport` only for an active
task and the current Project revision. Each observation has a stable local ID,
a location, an observation, a consequence, and an optional proposed revision.
Locations are validated against the task's tracks and ranges; report size and
text length are bounded.

The host assigns an opaque critique ID. A later `CompositionProposal` may set
`based_on_critique_id`; a hosted session rejects IDs it did not record for that
task. Each report is tied to the task's `brief_id` and contains exactly one
independent-evaluator disposition (`modify` or `preserve`) plus rationale for
each observation. A later proposal responds with only the observation ID and an
implementation rationale; the evaluator's disposition remains in trusted
host-held state and cannot be rewritten by the composing model.

The observation text remains advisory rather than an automatic taste gate. The
independent evaluator's explicit disposition is different: once linked, it is
an execution requirement for the composing proposal, while still granting no
new authority.

A `modify` decision must have a verifiable track or tick range. Deterministic
review then requires the proposal's material patch impact to support that
location. A `preserve` decision requires a reasoned response but no artificial
edit. Recording a critique does not mutate the Project, consume the task, grant
capabilities, or turn an arrangement measurement into a universal style rule.
Directory projects persist accepted critiques under `history/`.

Once a critique is attached to a composing session, a proposal must link that
critique and answer every observation; omitting the link is not an opt-out.
First proposals remain possible only before any critique is attached.

## Consequences

- Revision intent and evaluator decisions are traceable from a listening
  observation to a proposal.
- Vague or out-of-scope critique padding is rejected without judging musical
  taste.
- First drafts remain possible without a prior critique; the link is optional.
- The composing model cannot silently convert an evaluator's `modify` decision
  into `preserve` (or vice versa), while stylistic latitude remains in the
  brief and evaluator rationale rather than in fixed thresholds.
- The composing model also cannot satisfy `modify` with prose alone or with an
  unrelated patch elsewhere in the task.
- One-shot expert CLI proposal review cannot authenticate a critique link. Only
  a host-managed `CompositionSessions` flow provides that guarantee.
- A post-commit listening pass uses a newly authorized task at the new Project
  revision before recording another critique and revision proposal.
