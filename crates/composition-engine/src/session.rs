use crate::{
    AuthorizedCompositionTask, CompositionProposal, CompositionTask, CritiqueDisposition,
    CritiqueReport, EditScope, MAX_CRITIQUE_OBSERVATIONS, MAX_CRITIQUE_TEXT_LENGTH,
    ProposalApplication, ProposalReview, ProposalReviewer, StoredCritique, TaskAuthorization,
    TaskAuthorizationStatus, TickRange, TrackAccess,
};
use music_core::{Project, ProjectEngine, new_id};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Host-owned lifecycle for immutable, authorized composition tasks.
///
/// A model may inspect the returned task and submit many proposal revisions,
/// but review and application always use the private stored copy. A successful
/// application consumes the task so its authority cannot be replayed.
pub struct CompositionSessions {
    reviewer: ProposalReviewer,
    tasks: BTreeMap<String, SessionEntry>,
}

impl CompositionSessions {
    pub fn new(reviewer: ProposalReviewer) -> Self {
        Self {
            reviewer,
            tasks: BTreeMap::new(),
        }
    }

    pub fn authorize(&mut self, project: &Project, task: CompositionTask) -> TaskAuthorization {
        let violations = self.reviewer.task_violations(project, &task);
        if !violations.is_empty() {
            return TaskAuthorization {
                status: TaskAuthorizationStatus::Rejected,
                authorized_task: None,
                violations,
            };
        }

        let task_id = loop {
            let candidate = new_id("composition-task");
            if !self.tasks.contains_key(&candidate) {
                break candidate;
            }
        };
        let authorized_task = AuthorizedCompositionTask {
            task_id: task_id.clone(),
            task: task.clone(),
        };
        self.tasks.insert(
            task_id,
            SessionEntry {
                task,
                state: SessionState::Active,
                critiques: BTreeMap::new(),
            },
        );
        TaskAuthorization {
            status: TaskAuthorizationStatus::Authorized,
            authorized_task: Some(authorized_task),
            violations,
        }
    }

    pub fn review(
        &self,
        project: &Project,
        task_id: &str,
        proposal: &CompositionProposal,
    ) -> Result<ProposalReview, CompositionSessionError> {
        let entry = self.active_entry(task_id)?;
        let critique = linked_critique(entry, proposal)?;
        ensure_critique_responses(critique, proposal)?;
        Ok(self.reviewer.review_with_critique(
            project,
            &entry.task,
            proposal,
            critique.map(|stored| &stored.report),
        ))
    }

    pub fn apply(
        &mut self,
        engine: &mut ProjectEngine,
        task_id: &str,
        proposal: &CompositionProposal,
    ) -> Result<ProposalApplication, CompositionSessionError> {
        let entry = self.active_entry(task_id)?;
        let critique = linked_critique(entry, proposal)?;
        ensure_critique_responses(critique, proposal)?;
        let task = entry.task.clone();
        let critique = critique.map(|stored| stored.report.clone());
        let application =
            self.reviewer
                .apply_with_critique(engine, &task, proposal, critique.as_ref());
        if application.change.is_some() {
            self.tasks
                .get_mut(task_id)
                .expect("active task must still exist")
                .state = SessionState::Consumed;
        }
        Ok(application)
    }

    /// Validate and store one listening critique without changing the Project
    /// or consuming edit authority. The host assigns the opaque ID used by a
    /// later proposal to prove which listening pass it addresses.
    pub fn record_critique(
        &mut self,
        project: &Project,
        task_id: &str,
        report: CritiqueReport,
    ) -> Result<StoredCritique, CompositionSessionError> {
        let task = self.active_entry(task_id)?.task.clone();
        validate_critique(project, &task, &report)?;
        let critique_id = loop {
            let candidate = new_id("critique");
            let exists = self
                .tasks
                .values()
                .any(|entry| entry.critiques.contains_key(&candidate));
            if !exists {
                break candidate;
            }
        };
        let stored = StoredCritique {
            id: critique_id.clone(),
            report,
        };
        self.tasks
            .get_mut(task_id)
            .expect("active task must still exist")
            .critiques
            .insert(critique_id, stored.clone());
        Ok(stored)
    }

    /// Attach a critique produced by the evaluator side of a session. The
    /// caller must be a trusted host adapter: the report is revalidated here
    /// against the private task and current Project before it becomes usable
    /// by a composing model.
    pub fn attach_critique(
        &mut self,
        project: &Project,
        task_id: &str,
        stored: StoredCritique,
    ) -> Result<(), CompositionSessionError> {
        validate_critique_text("critique id", &stored.id)?;
        let task = self.active_entry(task_id)?.task.clone();
        validate_critique(project, &task, &stored.report)?;
        if self
            .tasks
            .values()
            .any(|entry| entry.critiques.contains_key(&stored.id))
        {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "critique id '{}' is already attached",
                stored.id
            )));
        }
        self.tasks
            .get_mut(task_id)
            .expect("active task must still exist")
            .critiques
            .insert(stored.id.clone(), stored);
        Ok(())
    }

    pub fn revoke(&mut self, task_id: &str) -> Result<(), CompositionSessionError> {
        let entry = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| CompositionSessionError::TaskNotFound(task_id.to_owned()))?;
        match entry.state {
            SessionState::Active => {
                entry.state = SessionState::Revoked;
                Ok(())
            }
            SessionState::Consumed => {
                Err(CompositionSessionError::TaskConsumed(task_id.to_owned()))
            }
            SessionState::Revoked => Err(CompositionSessionError::TaskRevoked(task_id.to_owned())),
        }
    }

    pub fn clear(&mut self) {
        self.tasks.clear();
    }

    fn active_entry(&self, task_id: &str) -> Result<&SessionEntry, CompositionSessionError> {
        let entry = self
            .tasks
            .get(task_id)
            .ok_or_else(|| CompositionSessionError::TaskNotFound(task_id.to_owned()))?;
        match entry.state {
            SessionState::Active => Ok(entry),
            SessionState::Consumed => {
                Err(CompositionSessionError::TaskConsumed(task_id.to_owned()))
            }
            SessionState::Revoked => Err(CompositionSessionError::TaskRevoked(task_id.to_owned())),
        }
    }
}

struct SessionEntry {
    task: CompositionTask,
    state: SessionState,
    critiques: BTreeMap<String, StoredCritique>,
}

#[derive(Clone, Copy)]
enum SessionState {
    Active,
    Consumed,
    Revoked,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompositionSessionError {
    #[error("authorized composition task not found: {0}")]
    TaskNotFound(String),
    #[error("authorized composition task was already consumed: {0}")]
    TaskConsumed(String),
    #[error("authorized composition task was revoked: {0}")]
    TaskRevoked(String),
    #[error("recorded critique not found for this task: {0}")]
    CritiqueNotFound(String),
    #[error("critique revision {expected} does not match current project revision {actual}")]
    CritiqueRevisionMismatch { expected: u64, actual: u64 },
    #[error("invalid critique: {0}")]
    InvalidCritique(String),
}

fn linked_critique<'a>(
    entry: &'a SessionEntry,
    proposal: &CompositionProposal,
) -> Result<Option<&'a StoredCritique>, CompositionSessionError> {
    if proposal.based_on_critique_id.is_none() && !entry.critiques.is_empty() {
        return Err(CompositionSessionError::InvalidCritique(
            "proposal must link an attached evaluator critique before it can be reviewed"
                .to_owned(),
        ));
    }
    proposal
        .based_on_critique_id
        .as_ref()
        .map(|critique_id| {
            entry
                .critiques
                .get(critique_id)
                .ok_or_else(|| CompositionSessionError::CritiqueNotFound(critique_id.clone()))
        })
        .transpose()
}

fn ensure_critique_responses(
    critique: Option<&StoredCritique>,
    proposal: &CompositionProposal,
) -> Result<(), CompositionSessionError> {
    let Some(critique) = critique else {
        if proposal.critique_responses.is_empty() {
            return Ok(());
        }
        return Err(CompositionSessionError::InvalidCritique(
            "critique responses require based_on_critique_id".to_owned(),
        ));
    };
    let expected: BTreeSet<&str> = critique
        .report
        .observations
        .iter()
        .map(|observation| observation.id.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for response in &proposal.critique_responses {
        validate_critique_text("critique response observation_id", &response.observation_id)?;
        if !expected.contains(response.observation_id.as_str()) {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "critique response references unknown observation '{}'",
                response.observation_id
            )));
        }
        if !seen.insert(response.observation_id.as_str()) {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "critique response for observation '{}' appears more than once",
                response.observation_id
            )));
        }
        if response.rationale.trim().is_empty()
            || response.rationale.chars().count() > MAX_CRITIQUE_TEXT_LENGTH
        {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "critique response for '{}' needs a bounded rationale",
                response.observation_id
            )));
        }
    }
    if seen != expected {
        let missing = expected
            .difference(&seen)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CompositionSessionError::InvalidCritique(format!(
            "proposal must respond to every critique observation; missing: {missing}"
        )));
    }
    Ok(())
}

fn validate_critique(
    project: &Project,
    task: &CompositionTask,
    report: &CritiqueReport,
) -> Result<(), CompositionSessionError> {
    if report.base_revision != project.revision {
        return Err(CompositionSessionError::CritiqueRevisionMismatch {
            expected: report.base_revision,
            actual: project.revision,
        });
    }
    if report.brief_id != task.brief.id {
        return Err(CompositionSessionError::InvalidCritique(format!(
            "critique brief '{}' does not match authorized brief '{}'",
            report.brief_id, task.brief.id
        )));
    }
    validate_critique_text("summary", &report.summary)?;
    if report.observations.is_empty() || report.observations.len() > MAX_CRITIQUE_OBSERVATIONS {
        return Err(CompositionSessionError::InvalidCritique(format!(
            "observations must contain between 1 and {MAX_CRITIQUE_OBSERVATIONS} entries"
        )));
    }
    if let Some(next_focus) = &report.next_focus {
        validate_critique_text("next_focus", next_focus)?;
    }

    let mut ids = BTreeSet::new();
    for observation in &report.observations {
        validate_critique_text("observation id", &observation.id)?;
        if !ids.insert(observation.id.as_str()) {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "observation id '{}' appears more than once",
                observation.id
            )));
        }
        validate_critique_text("observation", &observation.observation)?;
        validate_critique_text("consequence", &observation.consequence)?;
        if let Some(proposed_revision) = &observation.proposed_revision {
            validate_critique_text("proposed_revision", proposed_revision)?;
        }
        let location = &observation.location;
        if location.label.is_none() && location.track_id.is_none() && location.range.is_none() {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "observation '{}' needs a label, track, or tick range",
                observation.id
            )));
        }
        if let Some(label) = &location.label {
            validate_critique_text("location label", label)?;
        }
        if let Some(track_id) = &location.track_id
            && (project.track(track_id).is_none() || !scope_allows_track(&task.scope, track_id))
        {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "observation '{}' references track '{}' outside the authorized task",
                observation.id, track_id
            )));
        }
        if let Some(range) = location.range
            && (!range.is_valid()
                || !task.brief.target.contains(range)
                || !scope_allows_range(&task.scope, location.track_id.as_deref(), range))
        {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "observation '{}' references a range outside the authorized task",
                observation.id
            )));
        }
    }

    let observation_ids: BTreeSet<&str> = ids;
    if report.decisions.len() != report.observations.len() {
        return Err(CompositionSessionError::InvalidCritique(
            "the independent evaluator must decide every observation exactly once".to_owned(),
        ));
    }
    let mut decision_ids = BTreeSet::new();
    for decision in &report.decisions {
        validate_critique_text("critique decision observation_id", &decision.observation_id)?;
        if !observation_ids.contains(decision.observation_id.as_str()) {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "critique decision references unknown observation '{}'",
                decision.observation_id
            )));
        }
        if !decision_ids.insert(decision.observation_id.as_str()) {
            return Err(CompositionSessionError::InvalidCritique(format!(
                "critique decision for observation '{}' appears more than once",
                decision.observation_id
            )));
        }
        validate_critique_text("critique decision rationale", &decision.rationale)?;
        if decision.disposition == CritiqueDisposition::Modify {
            let observation = report
                .observations
                .iter()
                .find(|observation| observation.id == decision.observation_id)
                .expect("known critique decision observation checked above");
            if observation.location.track_id.is_none() && observation.location.range.is_none() {
                return Err(CompositionSessionError::InvalidCritique(format!(
                    "modify decision for observation '{}' needs a track or tick range so patch execution can be verified",
                    decision.observation_id
                )));
            }
        }
    }
    if decision_ids != observation_ids {
        let missing = observation_ids
            .difference(&decision_ids)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CompositionSessionError::InvalidCritique(format!(
            "the independent evaluator must decide every observation; missing: {missing}"
        )));
    }
    Ok(())
}

fn validate_critique_text(field: &str, value: &str) -> Result<(), CompositionSessionError> {
    if value.trim().is_empty() || value.chars().count() > MAX_CRITIQUE_TEXT_LENGTH {
        return Err(CompositionSessionError::InvalidCritique(format!(
            "{field} must be non-empty and at most {MAX_CRITIQUE_TEXT_LENGTH} characters"
        )));
    }
    Ok(())
}

fn scope_allows_track(scope: &EditScope, track_id: &str) -> bool {
    match &scope.tracks {
        TrackAccess::All => true,
        TrackAccess::Only { track_ids } => track_ids.iter().any(|allowed| allowed == track_id),
    }
}

fn scope_allows_range(scope: &EditScope, track_id: Option<&str>, range: TickRange) -> bool {
    scope.timeline.iter().any(|allowed| {
        allowed.range.contains(range)
            && match (&allowed.track_id, track_id) {
                (None, _) => true,
                (Some(allowed_track), Some(track_id)) => allowed_track == track_id,
                (Some(_), None) => false,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        COMPOSITION_SCHEMA_VERSION, CompositionPlan, CoverageEvidence, CreativeBrief,
        CreativeDecision, CreativeObjective, CritiqueDecision, CritiqueDisposition,
        CritiqueLocation, CritiqueObservation, CritiqueResponse, EditCapability, EditScope,
        FindingCode, ObjectiveCoverage, ObjectivePriority, PlannedSection, PlannedTrackRole,
        ReviewEnvironment, ReviewStatus, RhythmConstraints, ScopedTickRange, TickRange,
        TrackAccess,
    };
    use music_core::{Command, NoteEvent, Patch};

    fn sessions() -> CompositionSessions {
        CompositionSessions::new(ProposalReviewer::new(ReviewEnvironment {
            available_instrument_ids: vec!["piano".to_owned()],
        }))
    }

    fn task() -> CompositionTask {
        CompositionTask {
            brief: CreativeBrief {
                schema_version: COMPOSITION_SCHEMA_VERSION,
                id: "session-brief".to_owned(),
                summary: "Compose an opening gesture".to_owned(),
                target: TickRange {
                    start_tick: 0,
                    end_tick: 3_840,
                },
                objectives: vec![CreativeObjective {
                    id: "gesture".to_owned(),
                    description: "Introduce an identifiable gesture".to_owned(),
                    priority: ObjectivePriority::Required,
                }],
                freedoms: Vec::new(),
                style_context: vec!["Keep the opening sparse".to_owned()],
                change_required: true,
                rhythm: RhythmConstraints::default(),
            },
            scope: EditScope {
                base_revision: 0,
                tracks: TrackAccess::Only {
                    track_ids: vec!["piano".to_owned()],
                },
                timeline: vec![ScopedTickRange {
                    track_id: Some("piano".to_owned()),
                    range: TickRange {
                        start_tick: 0,
                        end_tick: 960,
                    },
                }],
                capabilities: vec![EditCapability::Notes],
                protected_regions: Vec::new(),
                allowed_instrument_ids: vec!["piano".to_owned()],
                allow_new_tracks: false,
                allow_remove_tracks: false,
                allow_remove_events: false,
                max_operations: 8,
            },
        }
    }

    fn proposal_at(start_tick: i64) -> CompositionProposal {
        CompositionProposal {
            brief_id: "session-brief".to_owned(),
            based_on_critique_id: None,
            critique_responses: Vec::new(),
            plan: CompositionPlan {
                summary: "Use one clear piano attack".to_owned(),
                sections: vec![PlannedSection {
                    id: "gesture".to_owned(),
                    range: TickRange {
                        start_tick,
                        end_tick: start_tick + 480,
                    },
                    intent: "State the gesture".to_owned(),
                }],
                track_roles: vec![PlannedTrackRole {
                    track_id: "piano".to_owned(),
                    role: "solo gesture".to_owned(),
                }],
                objective_coverage: vec![ObjectiveCoverage {
                    objective_id: "gesture".to_owned(),
                    evidence: vec![CoverageEvidence {
                        description: "The piano attack states the gesture".to_owned(),
                        section_id: Some("gesture".to_owned()),
                        track_id: Some("piano".to_owned()),
                        range: Some(TickRange {
                            start_tick,
                            end_tick: start_tick + 480,
                        }),
                    }],
                }],
                decisions: vec![CreativeDecision {
                    decision: "Use a single attack".to_owned(),
                    rationale: "Space keeps the gesture legible".to_owned(),
                }],
            },
            patch: Patch {
                base_revision: Some(0),
                description: Some("compose session gesture".to_owned()),
                operations: vec![Command::AddNote {
                    track_id: "piano".to_owned(),
                    clip_id: "piano-main".to_owned(),
                    note: NoteEvent {
                        id: format!("session-note-{start_tick}"),
                        start_tick,
                        duration_tick: 480,
                        pitch: 60,
                        velocity: 88,
                    },
                }],
            },
        }
    }

    fn critique_at(revision: u64, range: TickRange) -> CritiqueReport {
        CritiqueReport {
            brief_id: "session-brief".to_owned(),
            base_revision: revision,
            summary: "The opening needs a more legible attack hierarchy".to_owned(),
            observations: vec![CritiqueObservation {
                id: "attack-balance".to_owned(),
                location: CritiqueLocation {
                    label: Some("opening gesture".to_owned()),
                    track_id: Some("piano".to_owned()),
                    range: Some(range),
                },
                observation: "The first attack does not separate from the following space"
                    .to_owned(),
                consequence: "The gesture is harder to identify on first hearing".to_owned(),
                proposed_revision: Some(
                    "Emphasize the attack while preserving the silence".to_owned(),
                ),
            }],
            decisions: vec![CritiqueDecision {
                observation_id: "attack-balance".to_owned(),
                disposition: CritiqueDisposition::Modify,
                rationale: "The opening identity is a required objective, so the evaluator selects a targeted revision".to_owned(),
            }],
            next_focus: Some("opening attack and release".to_owned()),
        }
    }

    #[test]
    fn authorization_rejects_stale_or_unrenderable_tasks() {
        let project = Project::default();
        let mut sessions = sessions();
        let mut stale = task();
        stale.scope.base_revision = 9;
        let stale_result = sessions.authorize(&project, stale);
        assert_eq!(stale_result.status, TaskAuthorizationStatus::Rejected);
        assert!(stale_result.authorized_task.is_none());
        assert!(
            stale_result
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::RevisionMismatch)
        );

        let mut unavailable = task();
        unavailable.scope.allowed_instrument_ids = vec!["orchestra".to_owned()];
        let unavailable_result = sessions.authorize(&project, unavailable);
        assert_eq!(unavailable_result.status, TaskAuthorizationStatus::Rejected);
        assert!(
            unavailable_result
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::InstrumentUnavailable)
        );

        let mut unknown_track = task();
        unknown_track.scope.tracks = TrackAccess::Only {
            track_ids: vec!["missing".to_owned()],
        };
        unknown_track.scope.timeline[0].track_id = Some("missing".to_owned());
        let unknown_result = sessions.authorize(&project, unknown_track);
        assert_eq!(unknown_result.status, TaskAuthorizationStatus::Rejected);
        assert!(
            unknown_result
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::InvalidScope)
        );
    }

    #[test]
    fn model_side_task_mutation_cannot_expand_stored_authority() {
        let project = Project::default();
        let mut sessions = sessions();
        let mut authorization = sessions.authorize(&project, task());
        let authorized = authorization.authorized_task.as_mut().unwrap();
        authorized.task.scope.timeline[0].range.end_tick = 3_840;

        let review = sessions
            .review(&project, &authorized.task_id, &proposal_at(1_920))
            .unwrap();
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::TimelineOutOfScope)
        );
    }

    #[test]
    fn successful_application_consumes_authority() {
        let mut engine = ProjectEngine::new(Project::default());
        let mut sessions = sessions();
        let authorization = sessions.authorize(engine.project(), task());
        let task_id = &authorization.authorized_task.unwrap().task_id;

        let application = sessions
            .apply(&mut engine, task_id, &proposal_at(0))
            .unwrap();
        assert_eq!(application.review.status, ReviewStatus::Ready);
        assert!(application.change.is_some());
        assert_eq!(engine.revision(), 1);
        assert_eq!(
            sessions.review(engine.project(), task_id, &proposal_at(0)),
            Err(CompositionSessionError::TaskConsumed(task_id.clone()))
        );
    }

    #[test]
    fn blocked_application_keeps_authority_available_for_revision() {
        let mut engine = ProjectEngine::new(Project::default());
        let mut sessions = sessions();
        let authorization = sessions.authorize(engine.project(), task());
        let task_id = authorization.authorized_task.unwrap().task_id;

        let blocked = sessions
            .apply(&mut engine, &task_id, &proposal_at(1_920))
            .unwrap();
        assert_eq!(blocked.review.status, ReviewStatus::NeedsRevision);
        assert!(blocked.change.is_none());
        assert_eq!(engine.revision(), 0);

        let applied = sessions
            .apply(&mut engine, &task_id, &proposal_at(0))
            .unwrap();
        assert_eq!(applied.review.status, ReviewStatus::Ready);
        assert!(applied.change.is_some());
    }

    #[test]
    fn critique_is_bounded_to_the_task_and_can_anchor_a_proposal_revision() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;

        let outside = sessions.record_critique(
            &project,
            &task_id,
            critique_at(
                0,
                TickRange {
                    start_tick: 1_920,
                    end_tick: 2_400,
                },
            ),
        );
        assert!(matches!(
            outside,
            Err(CompositionSessionError::InvalidCritique(_))
        ));

        let stored = sessions
            .record_critique(
                &project,
                &task_id,
                critique_at(
                    0,
                    TickRange {
                        start_tick: 0,
                        end_tick: 480,
                    },
                ),
            )
            .unwrap();
        assert!(stored.id.starts_with("critique-"));

        let mut linked = proposal_at(0);
        linked.based_on_critique_id = Some(stored.id.clone());
        linked.critique_responses = vec![CritiqueResponse {
            observation_id: "attack-balance".to_owned(),
            rationale: "Keep the sparse shape but make the opening attack more distinct".to_owned(),
        }];
        assert_eq!(
            sessions.review(&project, &task_id, &linked).unwrap().status,
            ReviewStatus::Ready
        );

        linked.based_on_critique_id = Some("critique-not-recorded".to_owned());
        assert_eq!(
            sessions.review(&project, &task_id, &linked),
            Err(CompositionSessionError::CritiqueNotFound(
                "critique-not-recorded".to_owned()
            ))
        );
    }

    #[test]
    fn linked_critique_requires_one_reasoned_response_per_observation() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;
        let stored = sessions
            .record_critique(
                &project,
                &task_id,
                critique_at(
                    0,
                    TickRange {
                        start_tick: 0,
                        end_tick: 480,
                    },
                ),
            )
            .unwrap();

        let mut missing = proposal_at(0);
        missing.based_on_critique_id = Some(stored.id.clone());
        assert!(matches!(
            sessions.review(&project, &task_id, &missing),
            Err(CompositionSessionError::InvalidCritique(_))
        ));

        let mut unknown = proposal_at(0);
        unknown.based_on_critique_id = Some(stored.id.clone());
        unknown.critique_responses = vec![CritiqueResponse {
            observation_id: "unknown-observation".to_owned(),
            rationale: "Attempt to answer an observation the evaluator did not record".to_owned(),
        }];
        assert!(matches!(
            sessions.review(&project, &task_id, &unknown),
            Err(CompositionSessionError::InvalidCritique(message))
                if message.contains("unknown observation")
        ));

        let mut empty_rationale = proposal_at(0);
        empty_rationale.based_on_critique_id = Some(stored.id.clone());
        empty_rationale.critique_responses = vec![CritiqueResponse {
            observation_id: "attack-balance".to_owned(),
            rationale: " ".to_owned(),
        }];
        assert!(matches!(
            sessions.review(&project, &task_id, &empty_rationale),
            Err(CompositionSessionError::InvalidCritique(_))
        ));

        let mut acknowledged = proposal_at(0);
        acknowledged.based_on_critique_id = Some(stored.id);
        acknowledged.critique_responses = vec![CritiqueResponse {
            observation_id: "attack-balance".to_owned(),
            rationale: "Implement the evaluator's targeted attack revision while retaining the sparse release".to_owned(),
        }];
        assert_eq!(
            sessions
                .review(&project, &task_id, &acknowledged)
                .unwrap()
                .status,
            ReviewStatus::Ready
        );
    }

    #[test]
    fn modify_decision_requires_material_impact_at_the_observation_location() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;
        let stored = sessions
            .record_critique(
                &project,
                &task_id,
                critique_at(
                    0,
                    TickRange {
                        start_tick: 480,
                        end_tick: 960,
                    },
                ),
            )
            .unwrap();

        let response = CritiqueResponse {
            observation_id: "attack-balance".to_owned(),
            rationale: "Implement the evaluator's targeted attack revision".to_owned(),
        };
        let mut elsewhere = proposal_at(0);
        elsewhere.based_on_critique_id = Some(stored.id.clone());
        elsewhere.critique_responses = vec![response.clone()];
        let review = sessions.review(&project, &task_id, &elsewhere).unwrap();
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| { finding.code == FindingCode::UnimplementedCritiqueDecision })
        );

        let mut at_observation = proposal_at(480);
        at_observation.based_on_critique_id = Some(stored.id);
        at_observation.critique_responses = vec![response];
        assert_eq!(
            sessions
                .review(&project, &task_id, &at_observation)
                .unwrap()
                .status,
            ReviewStatus::Ready
        );
    }

    #[test]
    fn preserve_decision_does_not_force_an_unrelated_patch() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;
        let mut report = critique_at(
            0,
            TickRange {
                start_tick: 480,
                end_tick: 960,
            },
        );
        report.decisions[0].disposition = CritiqueDisposition::Preserve;
        report.decisions[0].rationale =
            "The evaluator keeps this sparse release because it serves the brief".to_owned();
        let stored = sessions
            .record_critique(&project, &task_id, report)
            .unwrap();

        let mut linked = proposal_at(0);
        linked.based_on_critique_id = Some(stored.id);
        linked.critique_responses = vec![CritiqueResponse {
            observation_id: "attack-balance".to_owned(),
            rationale:
                "Leave the evaluated release untouched while implementing the proposal elsewhere"
                    .to_owned(),
        }];
        assert_eq!(
            sessions.review(&project, &task_id, &linked).unwrap().status,
            ReviewStatus::Ready
        );
    }

    #[test]
    fn an_attached_critique_cannot_be_bypassed_by_omitting_its_link() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;
        sessions
            .record_critique(
                &project,
                &task_id,
                critique_at(
                    0,
                    TickRange {
                        start_tick: 0,
                        end_tick: 480,
                    },
                ),
            )
            .unwrap();

        assert!(matches!(
            sessions.review(&project, &task_id, &proposal_at(0)),
            Err(CompositionSessionError::InvalidCritique(message))
                if message.contains("must link an attached evaluator critique")
        ));
    }

    #[test]
    fn modify_decision_rejects_a_label_only_observation() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;
        let mut report = critique_at(
            0,
            TickRange {
                start_tick: 0,
                end_tick: 480,
            },
        );
        report.observations[0].location.track_id = None;
        report.observations[0].location.range = None;

        assert!(matches!(
            sessions.record_critique(&project, &task_id, report),
            Err(CompositionSessionError::InvalidCritique(message))
                if message.contains("patch execution can be verified")
        ));
    }

    #[test]
    fn critique_rejects_stale_revision_and_empty_observations() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;

        let stale = sessions.record_critique(
            &project,
            &task_id,
            critique_at(
                8,
                TickRange {
                    start_tick: 0,
                    end_tick: 480,
                },
            ),
        );
        assert_eq!(
            stale,
            Err(CompositionSessionError::CritiqueRevisionMismatch {
                expected: 8,
                actual: 0,
            })
        );

        let mut empty = critique_at(
            0,
            TickRange {
                start_tick: 0,
                end_tick: 480,
            },
        );
        empty.observations.clear();
        assert!(matches!(
            sessions.record_critique(&project, &task_id, empty),
            Err(CompositionSessionError::InvalidCritique(_))
        ));
    }

    #[test]
    fn revoked_authority_cannot_be_reviewed_or_applied() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;
        sessions.revoke(&task_id).unwrap();

        assert_eq!(
            sessions.review(&project, &task_id, &proposal_at(0)),
            Err(CompositionSessionError::TaskRevoked(task_id))
        );
    }

    #[test]
    fn clearing_a_workspace_invalidates_all_task_ids() {
        let project = Project::default();
        let mut sessions = sessions();
        let authorization = sessions.authorize(&project, task());
        let task_id = authorization.authorized_task.unwrap().task_id;

        sessions.clear();

        assert_eq!(
            sessions.review(&project, &task_id, &proposal_at(0)),
            Err(CompositionSessionError::TaskNotFound(task_id))
        );
    }
}
