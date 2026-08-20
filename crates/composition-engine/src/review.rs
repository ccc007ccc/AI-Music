use crate::{
    COMPOSITION_SCHEMA_VERSION, CompositionProposal, CompositionTask, CritiqueDisposition,
    CritiqueReport, EditCapability, EditScope, FindingCode, ObjectivePriority, ProposalApplication,
    ProposalReview, ReviewEnvironment, ReviewFinding, ReviewMetrics, ReviewStatus, TickRange,
    TrackAccess,
};
use music_core::{Command, Project, ProjectEngine, TrackSource, quantize_tick};
use std::collections::{BTreeMap, BTreeSet};

/// Deterministic review interface at the creative-supervision seam.
///
/// The reviewer enforces correctness and authorization while returning
/// aesthetic/process observations as non-blocking advisories.
pub struct ProposalReviewer {
    available_instruments: BTreeSet<String>,
}

impl ProposalReviewer {
    pub fn new(environment: ReviewEnvironment) -> Self {
        Self {
            available_instruments: environment.available_instrument_ids.into_iter().collect(),
        }
    }

    pub fn review(
        &self,
        project: &Project,
        task: &CompositionTask,
        proposal: &CompositionProposal,
    ) -> ProposalReview {
        self.review_with_critique(project, task, proposal, None)
    }

    pub(crate) fn review_with_critique(
        &self,
        project: &Project,
        task: &CompositionTask,
        proposal: &CompositionProposal,
        critique: Option<&CritiqueReport>,
    ) -> ProposalReview {
        let mut violations = Vec::new();
        let mut advisories = Vec::new();
        let mut metrics = ReviewMetrics {
            operation_count: proposal.patch.operations.len(),
            ..ReviewMetrics::default()
        };

        violations.extend(self.task_violations(project, task));

        if proposal.brief_id.trim() != task.brief.id.trim() {
            violations.push(finding(
                FindingCode::BriefMismatch,
                format!(
                    "proposal brief '{}' does not match task brief '{}'",
                    proposal.brief_id, task.brief.id
                ),
            ));
        }
        if task.brief.change_required && proposal.patch.operations.is_empty() {
            violations.push(finding(
                FindingCode::EmptyRequiredChange,
                "the brief requires a change but the proposal patch is empty",
            ));
        }
        if proposal.patch.operations.len() > task.scope.max_operations {
            violations.push(finding(
                FindingCode::OperationBudgetExceeded,
                format!(
                    "proposal has {} operations but the scope allows at most {}",
                    proposal.patch.operations.len(),
                    task.scope.max_operations
                ),
            ));
        }

        let revision_is_usable = task.scope.base_revision == project.revision
            && proposal.patch.base_revision == Some(task.scope.base_revision);
        if proposal.patch.base_revision != Some(task.scope.base_revision) {
            violations.push(finding(
                FindingCode::RevisionMismatch,
                format!(
                    "patch base_revision {:?} must equal scope revision {}",
                    proposal.patch.base_revision, task.scope.base_revision
                ),
            ));
        }

        let mut patch_preview = None;
        let mut proposal_impact = None;
        if revision_is_usable {
            let engine = ProjectEngine::new(project.clone());
            match engine.preview_patch(&proposal.patch) {
                Ok(preview) => {
                    metrics.affected_tracks = preview.affected_tracks.clone();
                    patch_preview = Some(preview);
                    let impact = self.review_operations(
                        project,
                        &task.scope,
                        proposal,
                        &mut metrics,
                        &mut violations,
                    );
                    if task.brief.change_required
                        && !proposal.patch.operations.is_empty()
                        && !impact.material_change
                    {
                        violations.push(finding(
                            FindingCode::NoMaterialChange,
                            "the proposal contains operations but leaves the renderable project unchanged",
                        ));
                    }
                    proposal_impact = Some(impact);
                }
                Err(error) => violations.push(finding(
                    FindingCode::InvalidPatch,
                    format!("patch validation failed: {error}"),
                )),
            }
        }

        self.review_plan(
            project,
            task,
            proposal,
            proposal_impact.as_ref(),
            &mut metrics,
            &mut violations,
            &mut advisories,
        );
        if let (Some(critique), Some(impact)) = (critique, proposal_impact.as_ref()) {
            self.review_critique_decisions(critique, impact, &mut violations);
        }
        if revision_is_usable && proposal_impact.is_some() {
            self.review_rhythm_constraints(project, task, proposal, &mut violations);
        }

        let status = if violations.is_empty() {
            ReviewStatus::Ready
        } else {
            ReviewStatus::NeedsRevision
        };
        ProposalReview {
            status,
            patch_preview,
            violations,
            advisories,
            metrics,
        }
    }

    /// Reviews and, only when ready, commits the proposal through the same
    /// [`ProjectEngine`] used by GUI and CLI edits.
    pub fn apply(
        &self,
        engine: &mut ProjectEngine,
        task: &CompositionTask,
        proposal: &CompositionProposal,
    ) -> ProposalApplication {
        self.apply_with_critique(engine, task, proposal, None)
    }

    pub(crate) fn apply_with_critique(
        &self,
        engine: &mut ProjectEngine,
        task: &CompositionTask,
        proposal: &CompositionProposal,
        critique: Option<&CritiqueReport>,
    ) -> ProposalApplication {
        let mut review = self.review_with_critique(engine.project(), task, proposal, critique);
        if review.status != ReviewStatus::Ready {
            return ProposalApplication {
                review,
                change: None,
            };
        }
        match engine.apply_patch(proposal.patch.clone()) {
            Ok(change) => ProposalApplication {
                review,
                change: Some(change),
            },
            Err(error) => {
                review.status = ReviewStatus::NeedsRevision;
                review.violations.push(finding(
                    FindingCode::InvalidPatch,
                    format!("patch could not be committed after review: {error}"),
                ));
                ProposalApplication {
                    review,
                    change: None,
                }
            }
        }
    }

    fn review_critique_decisions(
        &self,
        critique: &CritiqueReport,
        proposal_impact: &ProposalImpact,
        violations: &mut Vec<ReviewFinding>,
    ) {
        for decision in &critique.decisions {
            if decision.disposition != CritiqueDisposition::Modify {
                continue;
            }
            let Some(observation) = critique
                .observations
                .iter()
                .find(|observation| observation.id == decision.observation_id)
            else {
                continue;
            };
            if !proposal_impact.supports(
                observation.location.track_id.as_deref(),
                observation.location.range,
            ) {
                violations.push(finding(
                    FindingCode::UnimplementedCritiqueDecision,
                    format!(
                        "modify decision for critique observation '{}' is not supported by a material patch impact at its track/range",
                        observation.id
                    ),
                ));
            }
        }
    }

    pub(crate) fn task_violations(
        &self,
        project: &Project,
        task: &CompositionTask,
    ) -> Vec<ReviewFinding> {
        let mut violations = Vec::new();
        self.review_task(task, &mut violations);
        if task.scope.base_revision != project.revision {
            violations.push(finding(
                FindingCode::RevisionMismatch,
                format!(
                    "scope expects revision {}, project is at {}",
                    task.scope.base_revision, project.revision
                ),
            ));
        }
        for instrument_id in &task.scope.allowed_instrument_ids {
            if !self.available_instruments.contains(instrument_id) {
                violations.push(finding(
                    FindingCode::InstrumentUnavailable,
                    format!("scope allows unavailable instrument '{instrument_id}'"),
                ));
            }
        }
        let capabilities: BTreeSet<_> = task.scope.capabilities.iter().copied().collect();
        if let TrackAccess::Only { track_ids } = &task.scope.tracks {
            for track_id in track_ids {
                if project.track(track_id).is_none() && !task.scope.allow_new_tracks {
                    violations.push(finding(
                        FindingCode::InvalidScope,
                        format!(
                            "restricted track '{track_id}' does not exist and new tracks are not authorized"
                        ),
                    ));
                }
            }
        }
        for range in &task.scope.timeline {
            let Some(track_id) = range.track_id.as_deref() else {
                continue;
            };
            if !track_allowed(&task.scope.tracks, track_id) {
                violations.push(finding(
                    FindingCode::InvalidScope,
                    format!("timeline track '{track_id}' is outside restricted track access"),
                ));
            }
            if project.track(track_id).is_none() && !task.scope.allow_new_tracks {
                violations.push(finding(
                    FindingCode::InvalidScope,
                    format!(
                        "timeline track '{track_id}' does not exist and new tracks are not authorized"
                    ),
                ));
            }
        }
        if (task.scope.allow_new_tracks || task.scope.allow_remove_tracks)
            && !capabilities.contains(&EditCapability::Tracks)
        {
            violations.push(finding(
                FindingCode::InvalidScope,
                "track creation/removal flags require the tracks capability",
            ));
        }
        if task.scope.allow_remove_events
            && !capabilities.contains(&EditCapability::Notes)
            && !capabilities.contains(&EditCapability::Controls)
        {
            violations.push(finding(
                FindingCode::InvalidScope,
                "event removal requires the notes or controls capability",
            ));
        }
        violations
    }

    fn review_task(&self, task: &CompositionTask, violations: &mut Vec<ReviewFinding>) {
        let brief = &task.brief;
        if brief.schema_version != COMPOSITION_SCHEMA_VERSION {
            violations.push(finding(
                FindingCode::UnsupportedSchema,
                format!(
                    "unsupported composition schema version {}",
                    brief.schema_version
                ),
            ));
        }
        if brief.id.trim().is_empty() || brief.summary.trim().is_empty() || !brief.target.is_valid()
        {
            violations.push(finding(
                FindingCode::InvalidBrief,
                "brief id, summary, and target range must be valid",
            ));
        }

        let mut objective_ids = BTreeSet::new();
        for objective in &brief.objectives {
            if objective.id.trim().is_empty() || objective.description.trim().is_empty() {
                violations.push(finding(
                    FindingCode::InvalidBrief,
                    "objective id and description must not be empty",
                ));
            } else if !objective_ids.insert(objective.id.as_str()) {
                violations.push(finding(
                    FindingCode::DuplicateObjective,
                    format!("objective '{}' appears more than once", objective.id),
                ));
            }
        }
        if brief.change_required
            && !brief
                .objectives
                .iter()
                .any(|objective| objective.priority == ObjectivePriority::Required)
        {
            violations.push(finding(
                FindingCode::MissingRequiredObjective,
                "a brief that requires change must name at least one required objective",
            ));
        }

        if task.scope.max_operations == 0
            || task.scope.timeline.iter().any(|range| {
                !range.range.is_valid()
                    || range
                        .track_id
                        .as_ref()
                        .is_some_and(|track_id| track_id.trim().is_empty())
            })
            || task.scope.protected_regions.iter().any(|region| {
                !region.range.is_valid()
                    || region
                        .track_id
                        .as_ref()
                        .is_some_and(|track_id| track_id.trim().is_empty())
            })
        {
            violations.push(finding(
                FindingCode::InvalidScope,
                "scope ranges must be valid and max_operations must be positive",
            ));
        }

        if task
            .brief
            .rhythm
            .onset_grid_tick
            .is_some_and(|grid| grid <= 0)
            || task
                .brief
                .rhythm
                .minimum_active_bars
                .is_some_and(|bars| bars == 0)
        {
            violations.push(finding(
                FindingCode::InvalidRhythmConstraint,
                "rhythm grid must be positive and minimum_active_bars must be greater than zero",
            ));
        }
        if let TrackAccess::Only { track_ids } = &task.scope.tracks {
            let mut unique = BTreeSet::new();
            if track_ids
                .iter()
                .any(|track_id| track_id.trim().is_empty() || !unique.insert(track_id))
            {
                violations.push(finding(
                    FindingCode::InvalidScope,
                    "restricted track ids must be non-empty and unique",
                ));
            }
        }
        let mut instrument_ids = BTreeSet::new();
        if task
            .scope
            .allowed_instrument_ids
            .iter()
            .any(|instrument_id| {
                instrument_id.trim().is_empty() || !instrument_ids.insert(instrument_id)
            })
        {
            violations.push(finding(
                FindingCode::InvalidScope,
                "allowed instrument ids must be non-empty and unique",
            ));
        }
        let capabilities: BTreeSet<_> = task.scope.capabilities.iter().copied().collect();
        if capabilities.len() != task.scope.capabilities.len()
            || (brief.change_required && capabilities.is_empty())
        {
            violations.push(finding(
                FindingCode::InvalidScope,
                "capabilities must be unique and a required change needs at least one capability",
            ));
        }
    }

    fn review_rhythm_constraints(
        &self,
        project: &Project,
        task: &CompositionTask,
        proposal: &CompositionProposal,
        violations: &mut Vec<ReviewFinding>,
    ) {
        let constraints = &task.brief.rhythm;
        if constraints.require_bar_aligned_sections
            && let Ok(bar_tick) = project.time_signature.bar_length_tick(project.ppq)
        {
            for section in &proposal.plan.sections {
                if section.range.start_tick % bar_tick != 0
                    || section.range.end_tick % bar_tick != 0
                {
                    violations.push(finding(
                        FindingCode::SectionNotBarAligned,
                        format!(
                            "section '{}' must start and end on a {}-tick bar boundary",
                            section.id, bar_tick
                        ),
                    ));
                }
            }
        }

        let mut shadow = ProjectEngine::new(project.clone());
        if shadow.apply_patch(proposal.patch.clone()).is_err() {
            return;
        }
        let before_onsets = note_onsets(project, task.brief.target);
        let after_onsets = note_onsets(shadow.project(), task.brief.target);

        if let Some(grid_tick) = constraints.onset_grid_tick
            && grid_tick > 0
        {
            let mut violations_count = 0;
            for (key, onset) in &after_onsets {
                let changed = before_onsets
                    .get(key)
                    .is_none_or(|previous| previous != onset);
                if changed && onset.rem_euclid(grid_tick) != 0 {
                    violations_count += 1;
                }
            }
            if violations_count > 0 {
                violations.push(finding(
                    FindingCode::OnsetGridViolation,
                    format!(
                        "{violations_count} newly created or moved onset(s) are outside the authorized {grid_tick}-tick grid"
                    ),
                ));
            }
        }

        if let Some(minimum_active_bars) = constraints.minimum_active_bars
            && let Ok(bar_tick) = project.time_signature.bar_length_tick(project.ppq)
        {
            let active_bars = active_bars(shadow.project(), task.brief.target, bar_tick);
            if active_bars < minimum_active_bars as usize {
                violations.push(finding(
                    FindingCode::MinimumActiveBarsUnmet,
                    format!(
                        "target has {active_bars} active bars but requires at least {minimum_active_bars}"
                    ),
                ));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn review_plan(
        &self,
        project: &Project,
        task: &CompositionTask,
        proposal: &CompositionProposal,
        proposal_impact: Option<&ProposalImpact>,
        metrics: &mut ReviewMetrics,
        violations: &mut Vec<ReviewFinding>,
        advisories: &mut Vec<ReviewFinding>,
    ) {
        if proposal.plan.summary.trim().is_empty() {
            violations.push(finding(
                FindingCode::MissingPlanEvidence,
                "composition plan summary must not be empty",
            ));
        }

        let mut section_ids = BTreeSet::new();
        for section in &proposal.plan.sections {
            if section.id.trim().is_empty()
                || section.intent.trim().is_empty()
                || !section.range.is_valid()
                || !task.brief.target.contains(section.range)
                || !section_ids.insert(section.id.as_str())
            {
                violations.push(finding(
                    FindingCode::InvalidPlanSection,
                    format!(
                        "section '{}' must be unique, non-empty, valid, and inside the brief target",
                        section.id
                    ),
                ));
            }
        }

        let created_tracks: BTreeSet<&str> = proposal
            .patch
            .operations
            .iter()
            .filter_map(|operation| match operation {
                Command::CreateTrack { track_id, .. } => Some(track_id.as_str()),
                _ => None,
            })
            .collect();
        for role in &proposal.plan.track_roles {
            if role.track_id.trim().is_empty()
                || role.role.trim().is_empty()
                || (project.track(&role.track_id).is_none()
                    && !created_tracks.contains(role.track_id.as_str()))
            {
                violations.push(finding(
                    FindingCode::UnknownPlanTrack,
                    format!("plan role references unavailable track '{}'", role.track_id),
                ));
            }
        }

        let valid_decisions = proposal
            .plan
            .decisions
            .iter()
            .filter(|decision| {
                !decision.decision.trim().is_empty() && !decision.rationale.trim().is_empty()
            })
            .count();
        if valid_decisions != proposal.plan.decisions.len() {
            violations.push(finding(
                FindingCode::InvalidCreativeDecision,
                "creative decisions must contain a non-empty decision and rationale",
            ));
        }

        let objective_ids: BTreeSet<&str> = task
            .brief
            .objectives
            .iter()
            .map(|objective| objective.id.as_str())
            .collect();
        let mut covered = BTreeSet::new();
        let mut coverage_ids = BTreeSet::new();
        for coverage in &proposal.plan.objective_coverage {
            if !coverage_ids.insert(coverage.objective_id.as_str()) {
                violations.push(finding(
                    FindingCode::DuplicateObjectiveCoverage,
                    format!(
                        "objective '{}' has more than one coverage entry; combine its evidence",
                        coverage.objective_id
                    ),
                ));
                continue;
            }
            if !objective_ids.contains(coverage.objective_id.as_str()) {
                violations.push(finding(
                    FindingCode::UnknownObjective,
                    format!("plan covers unknown objective '{}'", coverage.objective_id),
                ));
                continue;
            }
            if coverage.evidence.is_empty() {
                violations.push(finding(
                    FindingCode::MissingPlanEvidence,
                    format!(
                        "objective '{}' must have non-empty plan evidence",
                        coverage.objective_id
                    ),
                ));
                continue;
            }

            let evidence_is_valid = coverage.evidence.iter().all(|evidence| {
                let section = evidence.section_id.as_ref().and_then(|section_id| {
                    proposal
                        .plan
                        .sections
                        .iter()
                        .find(|section| section.id == *section_id)
                });
                let section_reference_is_valid =
                    evidence.section_id.is_none() || section.is_some();
                let track_reference_is_valid = evidence.track_id.as_ref().is_none_or(|track_id| {
                    project.track(track_id).is_some()
                        || created_tracks.contains(track_id.as_str())
                });
                let range_reference_is_valid = evidence
                    .range
                    .is_none_or(|range| range.is_valid() && task.brief.target.contains(range));
                let anchored = evidence.section_id.is_some()
                    || evidence.track_id.is_some()
                    || evidence.range.is_some();
                let evidence_range = evidence.range.or_else(|| section.map(|value| value.range));
                let impact_supports_evidence = proposal_impact.is_none_or(|impact| {
                    impact.supports(evidence.track_id.as_deref(), evidence_range)
                });
                let valid = !evidence.description.trim().is_empty()
                    && anchored
                    && section_reference_is_valid
                    && track_reference_is_valid
                    && range_reference_is_valid
                    && impact_supports_evidence;
                if !valid {
                    violations.push(finding(
                        FindingCode::UnverifiableObjectiveCoverage,
                        format!(
                            "objective '{}' has evidence that is unanchored, unknown, or unsupported by the patch",
                            coverage.objective_id
                        ),
                    ));
                }
                valid
            });
            if evidence_is_valid {
                covered.insert(coverage.objective_id.as_str());
            }
        }

        for objective in &task.brief.objectives {
            let is_covered = covered.contains(objective.id.as_str());
            match objective.priority {
                ObjectivePriority::Required => {
                    metrics.required_objectives += 1;
                    if is_covered {
                        metrics.covered_required_objectives += 1;
                    } else {
                        violations.push(finding(
                            FindingCode::MissingObjectiveCoverage,
                            format!("required objective '{}' is not covered", objective.id),
                        ));
                    }
                }
                ObjectivePriority::Preferred => {
                    metrics.preferred_objectives += 1;
                    if is_covered {
                        metrics.covered_preferred_objectives += 1;
                    } else {
                        advisories.push(finding(
                            FindingCode::PreferredObjectiveUncovered,
                            format!("preferred objective '{}' is not covered", objective.id),
                        ));
                    }
                }
            }
        }

        let target_beats = (task.brief.target.end_tick - task.brief.target.start_tick) as f64
            / project.ppq.max(1) as f64;
        if target_beats >= 4.0 && proposal.plan.sections.is_empty() {
            advisories.push(finding(
                FindingCode::NoPlanSections,
                "the target spans at least four beats but the plan names no formal sections",
            ));
        }
        if task.brief.objectives.len() > 1 && valid_decisions == 0 {
            advisories.push(finding(
                FindingCode::NoCreativeDecisions,
                "the plan addresses multiple objectives but records no intentional trade-offs",
            ));
        }
    }

    fn review_operations(
        &self,
        project: &Project,
        scope: &EditScope,
        proposal: &CompositionProposal,
        metrics: &mut ReviewMetrics,
        violations: &mut Vec<ReviewFinding>,
    ) -> ProposalImpact {
        let capabilities: BTreeSet<_> = scope.capabilities.iter().copied().collect();
        let mut affected_tracks = BTreeSet::new();
        let mut affected_ranges = Vec::new();
        let mut shadow = ProjectEngine::new(project.clone());

        for operation in &proposal.patch.operations {
            let before = shadow.project().clone();
            let impact = operation_impact(&before, operation);
            if let Some(track_id) = impact.track_id.as_deref()
                && !track_allowed(&scope.tracks, track_id)
            {
                violations.push(finding(
                    FindingCode::TrackOutOfScope,
                    format!("track '{track_id}' is outside the edit scope"),
                ));
            }
            if !capabilities.contains(&impact.capability) {
                violations.push(finding(
                    FindingCode::CapabilityDenied,
                    format!("operation requires {:?} capability", impact.capability),
                ));
            }

            if impact.creates_track {
                metrics.created_tracks += 1;
                if !scope.allow_new_tracks {
                    violations.push(finding(
                        FindingCode::NewTrackDenied,
                        "creating tracks is not authorized",
                    ));
                }
            }
            if impact.removes_track {
                metrics.removed_tracks += 1;
                if !scope.allow_remove_tracks {
                    violations.push(finding(
                        FindingCode::RemoveTrackDenied,
                        "removing tracks is not authorized",
                    ));
                }
            }
            if impact.removes_event {
                metrics.removed_events += 1;
                if !scope.allow_remove_events {
                    violations.push(finding(
                        FindingCode::RemoveEventDenied,
                        "removing note or control events is not authorized",
                    ));
                }
            }

            for range in &impact.ranges {
                if !timeline_allowed(scope, impact.track_id.as_deref(), *range) {
                    violations.push(finding(
                        FindingCode::TimelineOutOfScope,
                        format!(
                            "operation touches ticks {}..{} outside the edit scope",
                            range.start_tick, range.end_tick
                        ),
                    ));
                }
                if protected_region_touched(scope, impact.track_id.as_deref(), *range) {
                    violations.push(finding(
                        FindingCode::ProtectedRegionTouched,
                        format!(
                            "operation touches protected ticks {}..{}",
                            range.start_tick, range.end_tick
                        ),
                    ));
                }
            }
            if impact.removes_track
                && scope.protected_regions.iter().any(|region| {
                    region.track_id.is_none()
                        || region.track_id.as_deref() == impact.track_id.as_deref()
                })
            {
                violations.push(finding(
                    FindingCode::ProtectedRegionTouched,
                    "removing the track would remove a protected region",
                ));
            }

            if let Some(instrument) = impact.instrument.as_deref() {
                if !self.available_instruments.contains(instrument) {
                    violations.push(finding(
                        FindingCode::InstrumentUnavailable,
                        format!("instrument '{instrument}' is not available"),
                    ));
                }
                if !scope.allowed_instrument_ids.is_empty()
                    && !scope
                        .allowed_instrument_ids
                        .iter()
                        .any(|allowed| allowed == instrument)
                {
                    violations.push(finding(
                        FindingCode::InstrumentOutOfScope,
                        format!("instrument '{instrument}' is outside the edit scope"),
                    ));
                }
            }

            // Full patch preview already proved this succeeds. Applying each
            // operation keeps later impact resolution aware of newly created
            // tracks, clips, notes, and controls.
            let _ = shadow.apply(operation.clone());
            if project_content_changed(&before, shadow.project()) {
                if let Some(track_id) = impact.track_id.as_ref() {
                    affected_tracks.insert(track_id.clone());
                }
                affected_ranges.extend(
                    impact
                        .ranges
                        .iter()
                        .map(|range| (impact.track_id.clone(), *range)),
                );
            }
        }

        metrics.affected_tracks = affected_tracks.into_iter().collect();
        ProposalImpact {
            tracks: metrics.affected_tracks.iter().cloned().collect(),
            ranges: affected_ranges,
            material_change: project_content_changed(project, shadow.project()),
        }
    }
}

#[derive(Debug, Default)]
struct ProposalImpact {
    tracks: BTreeSet<String>,
    ranges: Vec<(Option<String>, TickRange)>,
    material_change: bool,
}

impl ProposalImpact {
    fn supports(&self, track_id: Option<&str>, range: Option<TickRange>) -> bool {
        if let Some(track_id) = track_id
            && !self.tracks.contains(track_id)
        {
            return false;
        }
        let Some(range) = range else {
            return track_id.is_some_and(|track_id| self.tracks.contains(track_id));
        };
        self.ranges.iter().any(|(affected_track, affected_range)| {
            (track_id.is_none() || affected_track.as_deref() == track_id)
                && affected_range.intersects(range)
        })
    }
}

#[derive(Debug)]
struct OperationImpact {
    track_id: Option<String>,
    capability: EditCapability,
    ranges: Vec<TickRange>,
    creates_track: bool,
    removes_track: bool,
    removes_event: bool,
    instrument: Option<String>,
}

impl OperationImpact {
    fn new(track_id: Option<&str>, capability: EditCapability) -> Self {
        Self {
            track_id: track_id.map(str::to_owned),
            capability,
            ranges: Vec::new(),
            creates_track: false,
            removes_track: false,
            removes_event: false,
            instrument: None,
        }
    }
}

fn operation_impact(project: &Project, operation: &Command) -> OperationImpact {
    match operation {
        Command::CreateTrack {
            track_id,
            instrument,
            ..
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Tracks);
            impact.creates_track = true;
            impact.instrument = Some(instrument.clone());
            impact
        }
        Command::RemoveTrack { track_id } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Tracks);
            impact.removes_track = true;
            impact.ranges = track_ranges(project, track_id);
            impact
        }
        Command::RenameTrack { track_id, .. } => {
            OperationImpact::new(Some(track_id), EditCapability::Tracks)
        }
        Command::SetTrackInstrument {
            track_id,
            instrument,
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Instruments);
            impact.instrument = Some(instrument.clone());
            impact
        }
        Command::AddClip {
            track_id,
            start_tick,
            length_tick,
            ..
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Clips);
            impact.ranges.push(TickRange {
                start_tick: *start_tick,
                end_tick: start_tick.saturating_add(*length_tick),
            });
            impact
        }
        Command::AddNote {
            track_id,
            clip_id,
            note,
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Notes);
            if let Some(clip) = project.midi_clip(track_id, clip_id) {
                let start_tick = clip.start_tick.saturating_add(note.start_tick);
                impact.ranges.push(TickRange {
                    start_tick,
                    end_tick: start_tick.saturating_add(note.duration_tick),
                });
            }
            impact
        }
        Command::AddControl {
            track_id,
            clip_id,
            control,
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Controls);
            if let Some(clip) = project.midi_clip(track_id, clip_id) {
                impact
                    .ranges
                    .push(point_range(clip.start_tick.saturating_add(control.tick)));
            }
            impact
        }
        Command::SetControl {
            track_id,
            clip_id,
            control_id,
            tick,
            ..
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Controls);
            if let Some(clip) = project.midi_clip(track_id, clip_id) {
                if let Some(control) = clip
                    .controls
                    .iter()
                    .find(|control| control.id == *control_id)
                {
                    impact
                        .ranges
                        .push(point_range(clip.start_tick.saturating_add(control.tick)));
                }
                impact
                    .ranges
                    .push(point_range(clip.start_tick.saturating_add(*tick)));
            }
            impact
        }
        Command::RemoveControl {
            track_id,
            clip_id,
            control_id,
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Controls);
            impact.removes_event = true;
            if let Some(clip) = project.midi_clip(track_id, clip_id)
                && let Some(control) = clip
                    .controls
                    .iter()
                    .find(|control| control.id == *control_id)
            {
                impact
                    .ranges
                    .push(point_range(clip.start_tick.saturating_add(control.tick)));
            }
            impact
        }
        Command::RemoveNote {
            track_id,
            clip_id,
            note_id,
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Notes);
            impact.removes_event = true;
            if let Some(range) = note_range(project, track_id, clip_id, note_id) {
                impact.ranges.push(range);
            }
            impact
        }
        Command::SetNoteVelocity {
            track_id,
            clip_id,
            note_id,
            ..
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Notes);
            if let Some(range) = note_range(project, track_id, clip_id, note_id) {
                impact.ranges.push(range);
            }
            impact
        }
        Command::MoveNote {
            track_id,
            clip_id,
            note_id,
            start_tick,
            ..
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Notes);
            if let Some(clip) = project.midi_clip(track_id, clip_id)
                && let Some(note) = clip.notes.iter().find(|note| note.id == *note_id)
            {
                impact.ranges.push(TickRange {
                    start_tick: clip.start_tick.saturating_add(note.start_tick),
                    end_tick: clip
                        .start_tick
                        .saturating_add(note.start_tick)
                        .saturating_add(note.duration_tick),
                });
                let absolute_start = clip.start_tick.saturating_add(*start_tick);
                impact.ranges.push(TickRange {
                    start_tick: absolute_start,
                    end_tick: absolute_start.saturating_add(note.duration_tick),
                });
            }
            impact
        }
        Command::ResizeNote {
            track_id,
            clip_id,
            note_id,
            duration_tick,
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Notes);
            if let Some(clip) = project.midi_clip(track_id, clip_id)
                && let Some(note) = clip.notes.iter().find(|note| note.id == *note_id)
            {
                let absolute_start = clip.start_tick.saturating_add(note.start_tick);
                impact.ranges.push(TickRange {
                    start_tick: absolute_start,
                    end_tick: absolute_start.saturating_add(note.duration_tick),
                });
                impact.ranges.push(TickRange {
                    start_tick: absolute_start,
                    end_tick: absolute_start.saturating_add(*duration_tick),
                });
            }
            impact
        }
        Command::QuantizeNotes {
            track_id,
            clip_id,
            start_tick,
            end_tick,
            grid_tick,
            strength,
        } => {
            let mut impact = OperationImpact::new(Some(track_id), EditCapability::Notes);
            if let Some(clip) = project.midi_clip(track_id, clip_id) {
                for note in &clip.notes {
                    if note.start_tick < *start_tick || note.start_tick >= *end_tick {
                        continue;
                    }
                    let old_start = clip.start_tick.saturating_add(note.start_tick);
                    impact.ranges.push(TickRange {
                        start_tick: old_start,
                        end_tick: old_start.saturating_add(note.duration_tick),
                    });
                    if let Ok(new_start) = quantize_tick(note.start_tick, *grid_tick, *strength)
                        && new_start >= *start_tick
                        && new_start < *end_tick
                    {
                        let new_start = clip.start_tick.saturating_add(new_start);
                        impact.ranges.push(TickRange {
                            start_tick: new_start,
                            end_tick: new_start.saturating_add(note.duration_tick),
                        });
                    }
                }
            }
            impact
        }
        Command::SetTempo { tick, .. } => {
            let mut impact = OperationImpact::new(None, EditCapability::Tempo);
            impact.ranges.push(point_range(*tick));
            impact
        }
        Command::SetTimeSignature { .. } => {
            let mut impact = OperationImpact::new(None, EditCapability::Meter);
            impact.ranges.push(project_range(project));
            impact
        }
        Command::SetTrackMixer { track_id, .. } => {
            OperationImpact::new(Some(track_id), EditCapability::Mixer)
        }
    }
}

fn note_range(
    project: &Project,
    track_id: &str,
    clip_id: &str,
    note_id: &str,
) -> Option<TickRange> {
    let clip = project.midi_clip(track_id, clip_id)?;
    let note = clip.notes.iter().find(|note| note.id == note_id)?;
    let start_tick = clip.start_tick.saturating_add(note.start_tick);
    Some(TickRange {
        start_tick,
        end_tick: start_tick.saturating_add(note.duration_tick),
    })
}

fn track_ranges(project: &Project, track_id: &str) -> Vec<TickRange> {
    let Some(track) = project.track(track_id) else {
        return Vec::new();
    };
    match &track.source {
        TrackSource::Midi { clips, .. } => clips
            .iter()
            .map(|clip| {
                let note_end = clip
                    .notes
                    .iter()
                    .map(|note| note.start_tick.saturating_add(note.duration_tick))
                    .max()
                    .unwrap_or(0);
                let control_end = clip
                    .controls
                    .iter()
                    .map(|control| control.tick.saturating_add(1))
                    .max()
                    .unwrap_or(0);
                TickRange {
                    start_tick: clip.start_tick,
                    end_tick: clip
                        .start_tick
                        .saturating_add(clip.length_tick.max(note_end).max(control_end).max(1)),
                }
            })
            .collect(),
        TrackSource::Audio { clips } => clips
            .iter()
            .map(|clip| TickRange {
                start_tick: clip.start_tick,
                end_tick: clip.start_tick.saturating_add(clip.length_tick.max(1)),
            })
            .collect(),
    }
}

fn point_range(tick: i64) -> TickRange {
    TickRange {
        start_tick: tick,
        end_tick: tick.saturating_add(1),
    }
}

fn project_range(project: &Project) -> TickRange {
    TickRange {
        start_tick: 0,
        end_tick: project.duration_tick().max(1),
    }
}

type NoteKey = (String, String, String);

fn note_onsets(project: &Project, target: TickRange) -> BTreeMap<NoteKey, i64> {
    let mut onsets = BTreeMap::new();
    for track in &project.tracks {
        let TrackSource::Midi { clips, .. } = &track.source else {
            continue;
        };
        for clip in clips {
            for note in &clip.notes {
                let absolute_tick = clip.start_tick.saturating_add(note.start_tick);
                if absolute_tick >= target.start_tick && absolute_tick < target.end_tick {
                    onsets.insert(
                        (track.id.clone(), clip.id.clone(), note.id.clone()),
                        absolute_tick,
                    );
                }
            }
        }
    }
    onsets
}

fn active_bars(project: &Project, target: TickRange, bar_tick: i64) -> usize {
    if bar_tick <= 0 {
        return 0;
    }
    note_onsets(project, target)
        .values()
        .map(|tick| tick / bar_tick)
        .collect::<BTreeSet<_>>()
        .len()
}

fn project_content_changed(before: &Project, after: &Project) -> bool {
    let mut before = before.clone();
    let mut after = after.clone();
    before.revision = 0;
    after.revision = 0;
    before != after
}

fn track_allowed(access: &TrackAccess, track_id: &str) -> bool {
    match access {
        TrackAccess::All => true,
        TrackAccess::Only { track_ids } => track_ids.iter().any(|allowed| allowed == track_id),
    }
}

fn timeline_allowed(scope: &EditScope, track_id: Option<&str>, range: TickRange) -> bool {
    scope.timeline.iter().any(|allowed| {
        (allowed.track_id.is_none() || allowed.track_id.as_deref() == track_id)
            && allowed.range.contains(range)
    })
}

fn protected_region_touched(scope: &EditScope, track_id: Option<&str>, range: TickRange) -> bool {
    scope.protected_regions.iter().any(|protected| {
        (protected.track_id.is_none() || protected.track_id.as_deref() == track_id)
            && protected.range.intersects(range)
    })
}

fn finding(code: FindingCode, message: impl Into<String>) -> ReviewFinding {
    ReviewFinding {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CompositionPlan, CoverageEvidence, CreativeBrief, CreativeDecision, CreativeObjective,
        ObjectiveCoverage, PlannedSection, PlannedTrackRole, ProtectedRegion, RhythmConstraints,
        ScopedTickRange,
    };
    use music_core::{NoteEvent, Patch};

    fn reviewer() -> ProposalReviewer {
        ProposalReviewer::new(ReviewEnvironment {
            available_instrument_ids: vec!["piano".to_owned()],
        })
    }

    fn task() -> CompositionTask {
        CompositionTask {
            brief: CreativeBrief {
                schema_version: COMPOSITION_SCHEMA_VERSION,
                id: "brief-1".to_owned(),
                summary: "Create a clear four-bar piano opening".to_owned(),
                target: TickRange {
                    start_tick: 0,
                    end_tick: 15_360,
                },
                objectives: vec![
                    CreativeObjective {
                        id: "motif".to_owned(),
                        description: "Introduce a recognizable motif".to_owned(),
                        priority: ObjectivePriority::Required,
                    },
                    CreativeObjective {
                        id: "contrast".to_owned(),
                        description: "Add a contrasting response".to_owned(),
                        priority: ObjectivePriority::Preferred,
                    },
                ],
                freedoms: vec!["May use silence and asymmetric phrasing".to_owned()],
                style_context: vec!["Contrast may be abrupt when it serves the brief".to_owned()],
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
                        end_tick: 15_360,
                    },
                }],
                capabilities: vec![EditCapability::Notes, EditCapability::Controls],
                protected_regions: Vec::new(),
                allowed_instrument_ids: vec!["piano".to_owned()],
                allow_new_tracks: false,
                allow_remove_tracks: false,
                allow_remove_events: false,
                max_operations: 64,
            },
        }
    }

    fn proposal() -> CompositionProposal {
        CompositionProposal {
            brief_id: "brief-1".to_owned(),
            based_on_critique_id: None,
            critique_responses: Vec::new(),
            plan: CompositionPlan {
                summary: "State a compact motif, then answer it higher".to_owned(),
                sections: vec![PlannedSection {
                    id: "opening".to_owned(),
                    range: TickRange {
                        start_tick: 0,
                        end_tick: 3_840,
                    },
                    intent: "Present the motif with space after it".to_owned(),
                }],
                track_roles: vec![PlannedTrackRole {
                    track_id: "piano".to_owned(),
                    role: "solo motif and response".to_owned(),
                }],
                objective_coverage: vec![ObjectiveCoverage {
                    objective_id: "motif".to_owned(),
                    evidence: vec![CoverageEvidence {
                        description: "opening section begins with the motif".to_owned(),
                        section_id: Some("opening".to_owned()),
                        track_id: Some("piano".to_owned()),
                        range: Some(TickRange {
                            start_tick: 0,
                            end_tick: 480,
                        }),
                    }],
                }],
                decisions: vec![CreativeDecision {
                    decision: "Leave the second half of the bar sparse".to_owned(),
                    rationale: "Silence makes the motif easier to recognize".to_owned(),
                }],
            },
            patch: Patch {
                base_revision: Some(0),
                description: Some("compose opening motif".to_owned()),
                operations: vec![Command::AddNote {
                    track_id: "piano".to_owned(),
                    clip_id: "piano-main".to_owned(),
                    note: NoteEvent {
                        id: "motif-c".to_owned(),
                        start_tick: 0,
                        duration_tick: 480,
                        pitch: 60,
                        velocity: 92,
                    },
                }],
            },
        }
    }

    #[test]
    fn valid_proposal_is_ready_while_uncovered_preference_is_advisory() {
        let review = reviewer().review(&Project::default(), &task(), &proposal());
        assert_eq!(review.status, ReviewStatus::Ready);
        assert!(review.violations.is_empty());
        assert!(review.patch_preview.is_some());
        assert!(
            review
                .advisories
                .iter()
                .any(|finding| finding.code == FindingCode::PreferredObjectiveUncovered)
        );
        assert_eq!(review.metrics.covered_required_objectives, 1);
    }

    #[test]
    fn empty_required_change_and_missing_coverage_are_blocked() {
        let mut proposal = proposal();
        proposal.patch.operations.clear();
        proposal.plan.objective_coverage.clear();
        let review = reviewer().review(&Project::default(), &task(), &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::EmptyRequiredChange)
        );
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::MissingObjectiveCoverage)
        );
    }

    #[test]
    fn destructive_and_protected_edits_are_blocked_by_scope() {
        let mut task = task();
        task.scope.protected_regions.push(ProtectedRegion {
            track_id: Some("piano".to_owned()),
            range: TickRange {
                start_tick: 0,
                end_tick: 960,
            },
        });
        let mut project = Project::default();
        if let TrackSource::Midi { clips, .. } = &mut project.tracks[0].source {
            clips[0].notes.push(NoteEvent {
                id: "existing".to_owned(),
                start_tick: 0,
                duration_tick: 480,
                pitch: 60,
                velocity: 80,
            });
        }
        let mut proposal = proposal();
        proposal.patch.operations = vec![Command::RemoveNote {
            track_id: "piano".to_owned(),
            clip_id: "piano-main".to_owned(),
            note_id: "existing".to_owned(),
        }];

        let review = reviewer().review(&project, &task, &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::RemoveEventDenied)
        );
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::ProtectedRegionTouched)
        );
    }

    #[test]
    fn aesthetic_structure_remains_advisory_not_a_hard_rule() {
        let mut task = task();
        task.brief.objectives.truncate(1);
        let mut proposal = proposal();
        proposal.plan.sections.clear();
        proposal.plan.decisions.clear();
        proposal.plan.objective_coverage[0].evidence[0].section_id = None;

        let review = reviewer().review(&Project::default(), &task, &proposal);
        assert_eq!(review.status, ReviewStatus::Ready);
        assert!(
            review
                .advisories
                .iter()
                .any(|finding| finding.code == FindingCode::NoPlanSections)
        );
    }

    #[test]
    fn sequential_new_track_edits_are_reviewed_against_shadow_state() {
        let mut task = task();
        task.scope.tracks = TrackAccess::Only {
            track_ids: vec!["harmony".to_owned()],
        };
        task.scope.timeline[0].track_id = Some("harmony".to_owned());
        task.scope.capabilities.push(EditCapability::Tracks);
        task.scope.allow_new_tracks = true;
        let mut proposal = proposal();
        proposal.plan.track_roles = vec![PlannedTrackRole {
            track_id: "harmony".to_owned(),
            role: "answering harmony".to_owned(),
        }];
        proposal.patch.operations = vec![
            Command::CreateTrack {
                track_id: "harmony".to_owned(),
                name: "Harmony".to_owned(),
                instrument: "piano".to_owned(),
            },
            Command::AddNote {
                track_id: "harmony".to_owned(),
                clip_id: "harmony-main".to_owned(),
                note: NoteEvent {
                    id: "answer".to_owned(),
                    start_tick: 960,
                    duration_tick: 480,
                    pitch: 67,
                    velocity: 76,
                },
            },
        ];
        proposal.plan.objective_coverage[0].evidence[0].track_id = Some("harmony".to_owned());
        proposal.plan.objective_coverage[0].evidence[0].range = Some(TickRange {
            start_tick: 960,
            end_tick: 1_440,
        });

        let review = reviewer().review(&Project::default(), &task, &proposal);
        assert_eq!(review.status, ReviewStatus::Ready);
        assert_eq!(review.metrics.created_tracks, 1);
        assert_eq!(review.metrics.affected_tracks, vec!["harmony"]);
    }

    #[test]
    fn unavailable_instrument_is_blocked_before_rendering() {
        let mut task = task();
        task.scope.capabilities.push(EditCapability::Instruments);
        let mut proposal = proposal();
        proposal.patch.operations = vec![Command::SetTrackInstrument {
            track_id: "piano".to_owned(),
            instrument: "orchestra".to_owned(),
        }];

        let review = reviewer().review(&Project::default(), &task, &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::InstrumentUnavailable)
        );
    }

    #[test]
    fn claimed_objective_evidence_must_match_actual_patch_impact() {
        let mut proposal = proposal();
        proposal.plan.objective_coverage[0].evidence[0].range = Some(TickRange {
            start_tick: 7_680,
            end_tick: 8_160,
        });

        let review = reviewer().review(&Project::default(), &task(), &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| { finding.code == FindingCode::UnverifiableObjectiveCoverage })
        );
    }

    #[test]
    fn quantize_impact_contains_both_original_and_destination_ranges() {
        let mut project = Project::default();
        if let TrackSource::Midi { clips, .. } = &mut project.tracks[0].source {
            clips[0].notes.push(NoteEvent {
                id: "humanized".to_owned(),
                start_tick: 1_100,
                duration_tick: 300,
                pitch: 60,
                velocity: 88,
            });
        }
        let impact = operation_impact(
            &project,
            &Command::QuantizeNotes {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                start_tick: 0,
                end_tick: 1_920,
                grid_tick: 480,
                strength: 100,
            },
        );
        assert!(
            impact
                .ranges
                .iter()
                .any(|range| { range.start_tick == 1_100 && range.end_tick == 1_400 })
        );
        assert!(
            impact
                .ranges
                .iter()
                .any(|range| { range.start_tick == 960 && range.end_tick == 1_260 })
        );
    }

    #[test]
    fn changing_meter_requires_the_explicit_meter_capability() {
        let mut proposal = proposal();
        proposal.patch.operations = vec![Command::SetTimeSignature {
            numerator: 6,
            denominator: 8,
        }];
        let review = reviewer().review(&Project::default(), &task(), &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::CapabilityDenied)
        );
    }

    #[test]
    fn explicit_rhythm_constraints_block_off_grid_and_underfilled_results() {
        let mut task = task();
        task.brief.rhythm = RhythmConstraints {
            onset_grid_tick: Some(480),
            require_bar_aligned_sections: true,
            minimum_active_bars: Some(2),
        };
        let mut proposal = proposal();
        proposal.plan.sections[0].range.end_tick = 1_000;
        proposal.plan.objective_coverage[0].evidence[0].range = Some(TickRange {
            start_tick: 100,
            end_tick: 580,
        });
        proposal.patch.operations = vec![Command::AddNote {
            track_id: "piano".to_owned(),
            clip_id: "piano-main".to_owned(),
            note: NoteEvent {
                id: "off-grid".to_owned(),
                start_tick: 100,
                duration_tick: 480,
                pitch: 60,
                velocity: 88,
            },
        }];

        let review = reviewer().review(&Project::default(), &task, &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::OnsetGridViolation)
        );
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::SectionNotBarAligned)
        );
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::MinimumActiveBarsUnmet)
        );
    }

    #[test]
    fn required_change_needs_at_least_one_required_objective() {
        let mut task = task();
        for objective in &mut task.brief.objectives {
            objective.priority = ObjectivePriority::Preferred;
        }

        let review = reviewer().review(&Project::default(), &task, &proposal());
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::MissingRequiredObjective)
        );
    }

    #[test]
    fn blank_decisions_and_duplicate_coverage_cannot_pad_a_plan() {
        let mut proposal = proposal();
        proposal.plan.decisions[0].decision = " ".to_owned();
        proposal
            .plan
            .objective_coverage
            .push(proposal.plan.objective_coverage[0].clone());

        let review = reviewer().review(&Project::default(), &task(), &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::InvalidCreativeDecision)
        );
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::DuplicateObjectiveCoverage)
        );
    }

    #[test]
    fn non_empty_no_op_patch_cannot_satisfy_required_change() {
        let mut project = Project::default();
        if let TrackSource::Midi { clips, .. } = &mut project.tracks[0].source {
            clips[0].notes.push(NoteEvent {
                id: "existing".to_owned(),
                start_tick: 0,
                duration_tick: 480,
                pitch: 60,
                velocity: 88,
            });
        }
        let mut proposal = proposal();
        proposal.patch.operations = vec![Command::SetNoteVelocity {
            track_id: "piano".to_owned(),
            clip_id: "piano-main".to_owned(),
            note_id: "existing".to_owned(),
            velocity: 88,
        }];

        let review = reviewer().review(&project, &task(), &proposal);
        assert_eq!(review.status, ReviewStatus::NeedsRevision);
        assert!(
            review
                .violations
                .iter()
                .any(|finding| finding.code == FindingCode::NoMaterialChange)
        );
        assert!(review.metrics.affected_tracks.is_empty());
    }

    #[test]
    fn apply_commits_ready_proposal_and_leaves_blocked_proposal_unchanged() {
        let mut engine = ProjectEngine::new(Project::default());
        let applied = reviewer().apply(&mut engine, &task(), &proposal());
        assert_eq!(applied.review.status, ReviewStatus::Ready);
        assert_eq!(
            applied.change.as_ref().map(|change| change.revision),
            Some(1)
        );
        assert_eq!(engine.revision(), 1);
        assert_eq!(
            engine
                .project()
                .midi_clip("piano", "piano-main")
                .unwrap()
                .notes
                .len(),
            1
        );

        let before = engine.project().clone();
        let blocked = reviewer().apply(&mut engine, &task(), &proposal());
        assert_eq!(blocked.review.status, ReviewStatus::NeedsRevision);
        assert!(blocked.change.is_none());
        assert_eq!(engine.project(), &before);
    }
}
