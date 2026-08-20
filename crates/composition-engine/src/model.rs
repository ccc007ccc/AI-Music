use music_core::{ChangeSet, InstrumentId, Patch, PatchPreview, Tick, TrackId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const COMPOSITION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CompositionTask {
    pub brief: CreativeBrief,
    pub scope: EditScope,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AuthorizedCompositionTask {
    pub task_id: String,
    pub task: CompositionTask,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskAuthorizationStatus {
    Authorized,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskAuthorization {
    pub status: TaskAuthorizationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_task: Option<AuthorizedCompositionTask>,
    pub violations: Vec<ReviewFinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CreativeBrief {
    pub schema_version: u32,
    pub id: String,
    pub summary: String,
    pub target: TickRange,
    pub objectives: Vec<CreativeObjective>,
    #[serde(default)]
    pub freedoms: Vec<String>,
    /// High-level stylistic intent for the independent evaluator and composing model.
    /// These are contextual constraints or preferences, not a recipe or a
    /// universal definition of musical quality.
    #[serde(default)]
    pub style_context: Vec<String>,
    #[serde(default = "default_change_required")]
    pub change_required: bool,
    #[serde(default)]
    pub rhythm: RhythmConstraints,
}

fn default_change_required() -> bool {
    true
}

/// Optional, host-authored rhythm requirements. An empty value intentionally
/// imposes no meter/grid/density rule, preserving the composer's freedom.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RhythmConstraints {
    /// Absolute project-tick grid for newly created or moved onsets.
    #[serde(default)]
    pub onset_grid_tick: Option<Tick>,
    /// Require every planned section boundary to land on a complete bar.
    #[serde(default)]
    pub require_bar_aligned_sections: bool,
    /// Require this many distinct bars with at least one note onset in target.
    #[serde(default)]
    pub minimum_active_bars: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CreativeObjective {
    pub id: String,
    pub description: String,
    pub priority: ObjectivePriority,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectivePriority {
    Required,
    Preferred,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct TickRange {
    pub start_tick: Tick,
    pub end_tick: Tick,
}

impl TickRange {
    pub fn is_valid(self) -> bool {
        self.start_tick >= 0 && self.end_tick > self.start_tick
    }

    pub fn contains(self, other: Self) -> bool {
        self.start_tick <= other.start_tick && self.end_tick >= other.end_tick
    }

    pub fn intersects(self, other: Self) -> bool {
        self.start_tick < other.end_tick && other.start_tick < self.end_tick
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct EditScope {
    pub base_revision: u64,
    pub tracks: TrackAccess,
    pub timeline: Vec<ScopedTickRange>,
    pub capabilities: Vec<EditCapability>,
    #[serde(default)]
    pub protected_regions: Vec<ProtectedRegion>,
    #[serde(default)]
    pub allowed_instrument_ids: Vec<InstrumentId>,
    #[serde(default)]
    pub allow_new_tracks: bool,
    #[serde(default)]
    pub allow_remove_tracks: bool,
    #[serde(default)]
    pub allow_remove_events: bool,
    #[serde(default = "default_max_operations")]
    pub max_operations: usize,
}

fn default_max_operations() -> usize {
    512
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackAccess {
    All,
    Only { track_ids: Vec<TrackId> },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScopedTickRange {
    #[serde(default)]
    pub track_id: Option<TrackId>,
    #[serde(flatten)]
    pub range: TickRange,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ProtectedRegion {
    #[serde(default)]
    pub track_id: Option<TrackId>,
    #[serde(flatten)]
    pub range: TickRange,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum EditCapability {
    Notes,
    Controls,
    Clips,
    Tracks,
    Tempo,
    Meter,
    Instruments,
    Mixer,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CompositionProposal {
    pub brief_id: String,
    pub plan: CompositionPlan,
    /// Optional link to a host-recorded listening critique. The link is
    /// checked by `CompositionSessions`, but remains optional so a first draft
    /// does not need a previous listening pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on_critique_id: Option<String>,
    /// When a proposal is based on a hosted listening critique, it must
    /// acknowledge every evaluator decision. The disposition exists only in
    /// the stored critique; `rationale` records how the proposal implements it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critique_responses: Vec<CritiqueResponse>,
    pub patch: Patch,
}

pub const MAX_CRITIQUE_OBSERVATIONS: usize = 32;
pub const MAX_CRITIQUE_TEXT_LENGTH: usize = 2_048;

/// A structured listening result. Its observations are advisory metadata; an
/// independent evaluator's stored decisions become execution requirements for
/// a linked proposal, but never grant edit authority or replace the Project
/// source of truth.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CritiqueReport {
    /// The brief whose style and objectives the evaluator used. Keeping this
    /// explicit prevents a listening pass from being attached to a different
    /// creative task at the same revision.
    pub brief_id: String,
    pub base_revision: u64,
    pub summary: String,
    pub observations: Vec<CritiqueObservation>,
    /// Decisions belong to the independent evaluator, not to the composer.
    /// They are kept separate from observations so the deterministic analyzer
    /// can remain purely descriptive.
    pub decisions: Vec<CritiqueDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_focus: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CritiqueObservation {
    pub id: String,
    pub location: CritiqueLocation,
    pub observation: String,
    pub consequence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_revision: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CritiqueLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<TrackId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<TickRange>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct StoredCritique {
    pub id: String,
    pub report: CritiqueReport,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CritiqueDecision {
    pub observation_id: String,
    pub disposition: CritiqueDisposition,
    /// Why this disposition follows from the task brief, style context, and
    /// rendered result. This is not a generic quality score or a prescribed
    /// musical recipe.
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CritiqueResponse {
    pub observation_id: String,
    /// Describes how the proposal implements the evaluator's already-stored
    /// decision. The composing model does not submit a second disposition.
    pub rationale: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CritiqueDisposition {
    Modify,
    Preserve,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CompositionPlan {
    pub summary: String,
    #[serde(default)]
    pub sections: Vec<PlannedSection>,
    #[serde(default)]
    pub track_roles: Vec<PlannedTrackRole>,
    pub objective_coverage: Vec<ObjectiveCoverage>,
    #[serde(default)]
    pub decisions: Vec<CreativeDecision>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct PlannedSection {
    pub id: String,
    #[serde(flatten)]
    pub range: TickRange,
    pub intent: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct PlannedTrackRole {
    pub track_id: TrackId,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ObjectiveCoverage {
    pub objective_id: String,
    pub evidence: Vec<CoverageEvidence>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CoverageEvidence {
    pub description: String,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub track_id: Option<TrackId>,
    #[serde(default)]
    pub range: Option<TickRange>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CreativeDecision {
    pub decision: String,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReviewEnvironment {
    pub available_instrument_ids: Vec<InstrumentId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ProposalReview {
    pub status: ReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_preview: Option<PatchPreview>,
    pub violations: Vec<ReviewFinding>,
    pub advisories: Vec<ReviewFinding>,
    pub metrics: ReviewMetrics,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ProposalApplication {
    pub review: ProposalReview,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<ChangeSet>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Ready,
    NeedsRevision,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReviewFinding {
    pub code: FindingCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingCode {
    UnsupportedSchema,
    InvalidBrief,
    MissingRequiredObjective,
    InvalidScope,
    BriefMismatch,
    DuplicateObjective,
    UnknownObjective,
    DuplicateObjectiveCoverage,
    MissingObjectiveCoverage,
    MissingPlanEvidence,
    UnverifiableObjectiveCoverage,
    InvalidPlanSection,
    UnknownPlanTrack,
    InvalidCreativeDecision,
    RevisionMismatch,
    EmptyRequiredChange,
    NoMaterialChange,
    UnimplementedCritiqueDecision,
    OperationBudgetExceeded,
    InvalidPatch,
    CapabilityDenied,
    TrackOutOfScope,
    TimelineOutOfScope,
    ProtectedRegionTouched,
    NewTrackDenied,
    RemoveTrackDenied,
    RemoveEventDenied,
    InstrumentUnavailable,
    InstrumentOutOfScope,
    PreferredObjectiveUncovered,
    NoPlanSections,
    NoCreativeDecisions,
    InvalidRhythmConstraint,
    OnsetGridViolation,
    SectionNotBarAligned,
    MinimumActiveBarsUnmet,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ReviewMetrics {
    pub operation_count: usize,
    pub affected_tracks: Vec<TrackId>,
    pub required_objectives: usize,
    pub covered_required_objectives: usize,
    pub preferred_objectives: usize,
    pub covered_preferred_objectives: usize,
    pub created_tracks: usize,
    pub removed_tracks: usize,
    pub removed_events: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_without_critique_responses_remains_backward_compatible() {
        let proposal: CompositionProposal = serde_json::from_value(serde_json::json!({
            "brief_id": "legacy-proposal",
            "plan": {
                "summary": "Legacy first draft",
                "objective_coverage": []
            },
            "patch": {
                "operations": []
            }
        }))
        .unwrap();

        assert!(proposal.based_on_critique_id.is_none());
        assert!(proposal.critique_responses.is_empty());
    }

    #[test]
    fn critique_response_cannot_carry_a_composer_disposition() {
        let result = serde_json::from_value::<CritiqueResponse>(serde_json::json!({
            "observation_id": "opening",
            "rationale": "Implement the stored evaluator decision",
            "disposition": "preserve"
        }));
        assert!(result.is_err());
    }
}
