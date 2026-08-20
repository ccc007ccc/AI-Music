//! Fully automatic natural-language music creation and revision.
//!
//! The public interface is one instruction at a time. Internally the module
//! isolates a trusted director, a composing model, deterministic Rust review,
//! rendering, and an independent evaluator. Callers never shuttle task,
//! proposal, or critique JSON between those roles.

mod codex;

pub use codex::CodexCliModel;

use audio_engine::{
    AudioBuffer, DEFAULT_SAMPLE_RATE, InstrumentRack, RenderError, render_project_with_rack,
};
use composition_engine::{
    ArrangementAnalyzer, ArrangementReport, COMPOSITION_SCHEMA_VERSION, CompositionProposal,
    CompositionSessionError, CompositionSessions, CompositionTask, CreativeBrief,
    CreativeObjective, CritiqueDecision, CritiqueDisposition, CritiqueObservation, CritiqueReport,
    EditCapability, EditScope, ProposalReview, ProposalReviewer, ReviewEnvironment, ReviewFinding,
    ReviewStatus, RhythmConstraints, StoredCritique, TickRange, TrackAccess,
};
use music_core::{ClipWindow, Project, ProjectEngine, ProjectSummary, TrackSource, new_id};
use schemars::{JsonSchema, schema_for};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use thiserror::Error;

pub const AUTOPILOT_SCHEMA_VERSION: u32 = 1;
const MAX_INSTRUCTION_CHARS: usize = 8_192;
const MAX_MEMORY_TURNS_IN_PROMPT: usize = 12;

/// The only model seam Autopilot needs. Adapters receive a complete prompt and
/// an exact response schema; they cannot edit the Project directly.
pub trait StructuredModel {
    fn complete(&mut self, request: ModelRequest<'_>) -> Result<Value, ModelBackendError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRole {
    Director,
    Composer,
    Evaluator,
}

pub struct ModelRequest<'a> {
    pub role: ModelRole,
    pub prompt: &'a str,
    pub schema: &'a Value,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ModelBackendError {
    message: String,
}

impl ModelBackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AutopilotConfig {
    pub max_director_attempts: usize,
    pub max_proposal_attempts: usize,
    pub max_evaluator_attempts: usize,
    /// Revisions after the first committed draft.
    pub max_revision_rounds: usize,
    pub max_target_bars: u32,
    pub max_operations_per_revision: usize,
    pub sample_rate: u32,
}

impl Default for AutopilotConfig {
    fn default() -> Self {
        Self {
            max_director_attempts: 2,
            max_proposal_attempts: 4,
            max_evaluator_attempts: 2,
            max_revision_rounds: 2,
            max_target_bars: 64,
            max_operations_per_revision: 2_048,
            sample_rate: DEFAULT_SAMPLE_RATE,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AutopilotMemory {
    pub schema_version: u32,
    pub session_id: String,
    #[serde(default)]
    pub turns: Vec<AutopilotMemoryTurn>,
}

impl Default for AutopilotMemory {
    fn default() -> Self {
        Self {
            schema_version: AUTOPILOT_SCHEMA_VERSION,
            session_id: new_id("autopilot-session"),
            turns: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AutopilotMemoryTurn {
    pub instruction: String,
    pub brief_id: String,
    pub brief_summary: String,
    pub starting_revision: u64,
    pub final_revision: u64,
    pub status: AutopilotStatus,
    pub evaluator_summary: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotStatus {
    Completed,
    RevisionLimitReached,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AutopilotOutcome {
    pub session_id: String,
    pub instruction: String,
    pub brief: CreativeBrief,
    pub starting_revision: u64,
    pub final_revision: u64,
    pub committed_revisions: Vec<u64>,
    pub proposal_attempts: usize,
    pub evaluator_rounds: usize,
    pub status: AutopilotStatus,
    pub evaluator_summary: String,
    pub render: RenderObservation,
}

pub struct AutopilotRun {
    pub outcome: AutopilotOutcome,
    pub final_audio: AudioBuffer,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RenderObservation {
    pub sample_rate: u32,
    pub channels: usize,
    pub frames: usize,
    pub duration_seconds: f64,
    pub peak: f32,
    pub rms_dbfs: f32,
    pub silent_frame_fraction: f32,
    pub stereo_balance: f32,
    pub zero_crossing_rate: f32,
    pub window_rms_dbfs: Vec<f32>,
}

#[derive(Debug, Error)]
pub enum AutopilotError {
    #[error("the user instruction must contain between 1 and {MAX_INSTRUCTION_CHARS} characters")]
    InvalidInstruction,
    #[error("model request failed for {role:?}: {message}")]
    Model { role: ModelRole, message: String },
    #[error("model returned an invalid {role:?} result after automatic retries: {message}")]
    InvalidModelResult { role: ModelRole, message: String },
    #[error("automatic task authorization failed: {0}")]
    Authorization(String),
    #[error("composer could not produce a reviewable proposal after automatic retries: {0}")]
    ProposalExhausted(String),
    #[error("composition session failed: {0}")]
    Session(#[from] CompositionSessionError),
    #[error("arrangement context failed: {0}")]
    Arrangement(String),
    #[error("render failed: {0}")]
    Render(#[from] RenderError),
    #[error("could not serialize automatic model context: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Deep orchestration module: callers submit one natural-language instruction;
/// every intermediate role, retry, decision, commit, and render stays inside.
pub struct Autopilot<M> {
    model: M,
    config: AutopilotConfig,
}

impl<M: StructuredModel> Autopilot<M> {
    pub fn new(model: M) -> Self {
        Self {
            model,
            config: AutopilotConfig::default(),
        }
    }

    pub fn with_config(model: M, config: AutopilotConfig) -> Self {
        Self { model, config }
    }

    pub fn into_model(self) -> M {
        self.model
    }

    pub fn run_instruction(
        &mut self,
        engine: &mut ProjectEngine,
        rack: &InstrumentRack,
        memory: &mut AutopilotMemory,
        instruction: &str,
    ) -> Result<AutopilotRun, AutopilotError> {
        let mut shadow_engine = engine.clone();
        let mut shadow_memory = memory.clone();
        let run =
            self.run_instruction_inner(&mut shadow_engine, rack, &mut shadow_memory, instruction)?;
        *engine = shadow_engine;
        *memory = shadow_memory;
        Ok(run)
    }

    fn run_instruction_inner(
        &mut self,
        engine: &mut ProjectEngine,
        rack: &InstrumentRack,
        memory: &mut AutopilotMemory,
        instruction: &str,
    ) -> Result<AutopilotRun, AutopilotError> {
        let instruction = instruction.trim();
        if instruction.is_empty() || instruction.chars().count() > MAX_INSTRUCTION_CHARS {
            return Err(AutopilotError::InvalidInstruction);
        }
        if memory.schema_version != AUTOPILOT_SCHEMA_VERSION {
            *memory = AutopilotMemory::default();
        }

        let starting_revision = engine.revision();
        let directed = self.direct(engine.project(), memory, instruction)?;
        let brief = CreativeBrief {
            schema_version: COMPOSITION_SCHEMA_VERSION,
            id: new_id("autopilot-brief"),
            summary: directed.summary,
            target: directed.target,
            objectives: directed.objectives,
            freedoms: directed.freedoms,
            style_context: directed.style_context,
            change_required: true,
            rhythm: directed.rhythm,
        };
        let reviewer = ProposalReviewer::new(ReviewEnvironment {
            available_instrument_ids: rack
                .catalog()
                .into_iter()
                .map(|instrument| instrument.id)
                .collect(),
        });
        let mut sessions = CompositionSessions::new(reviewer);
        let mut committed_revisions = Vec::new();
        let mut proposal_attempts = 0;
        let initial_task = self.task_for_revision(engine.project(), &brief, rack);
        let initial = authorize(&mut sessions, engine.project(), initial_task)?;
        let first = self.compose_and_apply(
            engine,
            memory,
            instruction,
            &mut sessions,
            &initial,
            None,
            &mut proposal_attempts,
        )?;
        committed_revisions.push(first.revision);

        let mut evaluator_rounds = 0;
        let (final_audio, evaluator_summary, status) = loop {
            let audio = render_project_with_rack(engine.project(), self.config.sample_rate, rack)?;
            let render = analyze_render(&audio);
            evaluator_rounds += 1;
            let evaluation_task = self.task_for_revision(engine.project(), &brief, rack);
            let authorization = authorize(&mut sessions, engine.project(), evaluation_task)?;
            let verdict = self.evaluate(
                engine.project(),
                memory,
                instruction,
                &authorization.task,
                &render,
            )?;
            match verdict.conclusion {
                EvaluationConclusion::Accept => {
                    sessions.revoke(&authorization.task_id)?;
                    break (audio, verdict.summary, AutopilotStatus::Completed);
                }
                EvaluationConclusion::Revise
                    if committed_revisions.len() > self.config.max_revision_rounds =>
                {
                    sessions.revoke(&authorization.task_id)?;
                    break (
                        audio,
                        verdict.summary,
                        AutopilotStatus::RevisionLimitReached,
                    );
                }
                EvaluationConclusion::Revise => {
                    let stored = self.record_evaluation(
                        engine.project(),
                        &mut sessions,
                        &authorization,
                        verdict,
                    )?;
                    let applied = self.compose_and_apply(
                        engine,
                        memory,
                        instruction,
                        &mut sessions,
                        &authorization,
                        Some(&stored),
                        &mut proposal_attempts,
                    )?;
                    committed_revisions.push(applied.revision);
                }
            }
        };

        let render = analyze_render(&final_audio);
        let final_revision = engine.revision();
        let outcome = AutopilotOutcome {
            session_id: memory.session_id.clone(),
            instruction: instruction.to_owned(),
            brief: brief.clone(),
            starting_revision,
            final_revision,
            committed_revisions,
            proposal_attempts,
            evaluator_rounds,
            status,
            evaluator_summary: evaluator_summary.clone(),
            render,
        };
        memory.turns.push(AutopilotMemoryTurn {
            instruction: instruction.to_owned(),
            brief_id: brief.id,
            brief_summary: brief.summary,
            starting_revision,
            final_revision,
            status,
            evaluator_summary,
        });
        Ok(AutopilotRun {
            outcome,
            final_audio,
        })
    }

    fn direct(
        &mut self,
        project: &Project,
        memory: &AutopilotMemory,
        instruction: &str,
    ) -> Result<DirectedBrief, AutopilotError> {
        let bar_tick = project
            .time_signature
            .bar_length_tick(project.ppq)
            .map_err(|error| AutopilotError::Arrangement(error.to_string()))?;
        let maximum_end_tick = bar_tick.saturating_mul(i64::from(self.config.max_target_bars));
        let context_end = project
            .duration_tick()
            .max(bar_tick * 4)
            .min(maximum_end_tick);
        let context = collect_context(
            project,
            TickRange {
                start_tick: 0,
                end_tick: context_end,
            },
        )?;
        let payload = json!({
            "user_instruction": instruction,
            "conversation_history": memory_prompt(memory),
            "project": context,
            "policy": {
                "maximum_end_tick": maximum_end_tick,
                "maximum_target_bars": self.config.max_target_bars,
                "registered_instruments_are_host_selected": true,
                "intermediate_questions_are_not_allowed": true
            }
        });
        let mut feedback = None;
        for _ in 0..self.config.max_director_attempts.max(1) {
            let prompt = format!(
                "You are the trusted music director inside a fully automatic music system. \
Translate the user's latest natural-language instruction into one concrete creative brief draft. \
Never ask the user a question and never expose JSON workflow details. Make reasonable musical \
choices from the conversation and current Project. Use exact project ticks for the target. \
At least one objective must be required. Rhythm constraints must remain empty unless the user \
explicitly requested a grid, bar alignment, or active-bar minimum. Repetition, silence, unusual \
harmony, asymmetry, and mechanical timing are not defects by default. Return only the schema result.\n\n\
INPUT:\n{}\n\nPREVIOUS INVALID RESULT FEEDBACK:\n{}",
                serde_json::to_string_pretty(&payload)?,
                feedback.as_deref().unwrap_or("none")
            );
            match self.complete_typed::<DirectedBrief>(ModelRole::Director, &prompt) {
                Ok(result) => match validate_directed(&result, maximum_end_tick) {
                    Ok(()) => return Ok(result),
                    Err(error) => feedback = Some(error),
                },
                Err(error @ AutopilotError::Model { .. }) => return Err(error),
                Err(error) => feedback = Some(error.to_string()),
            }
        }
        Err(AutopilotError::InvalidModelResult {
            role: ModelRole::Director,
            message: feedback.unwrap_or_else(|| "unknown director failure".to_owned()),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn compose_and_apply(
        &mut self,
        engine: &mut ProjectEngine,
        memory: &AutopilotMemory,
        instruction: &str,
        sessions: &mut CompositionSessions,
        authorization: &AuthorizedTask,
        critique: Option<&StoredCritique>,
        proposal_attempts: &mut usize,
    ) -> Result<AppliedRevision, AutopilotError> {
        let mut review_feedback: Option<ProposalReview> = None;
        let mut last_error = "no proposal was produced".to_owned();
        for _ in 0..self.config.max_proposal_attempts.max(1) {
            *proposal_attempts += 1;
            let context = collect_context(engine.project(), authorization.task.brief.target)?;
            let payload = json!({
                "user_instruction": instruction,
                "conversation_history": memory_prompt(memory),
                "authorized_task": authorization,
                "project": context,
                "linked_evaluator_critique": critique,
                "deterministic_review_feedback": review_feedback,
            });
            let prompt = format!(
                "You are the Composer inside a fully automatic music system. Create the complete \
CompositionProposal that implements the immutable authorized task. Do not ask questions and do \
not merely describe music: emit concrete operations using the exact track, clip, note, control, \
revision, and tick data supplied. Satisfy every required objective with truthful anchored evidence. \
Copy every objective ID exactly into plan.objective_coverage and give each required objective at \
least one evidence item intersecting the actual patch. Keep MIDI pitch/controller/value within \
0-127 and note velocity within 1-127; keep every created event ID unique. \
When an evaluator critique is linked, copy its opaque ID into based_on_critique_id, answer every \
observation exactly once, and implement every modify decision at its recorded location. You cannot \
change evaluator dispositions. Fix all deterministic review feedback rather than retrying cosmetic \
wording. Return only the schema result.\n\nINPUT:\n{}",
                serde_json::to_string_pretty(&payload)?
            );
            let proposal =
                match self.complete_typed::<CompositionProposal>(ModelRole::Composer, &prompt) {
                    Ok(proposal) => proposal,
                    Err(error @ AutopilotError::Model { .. }) => return Err(error),
                    Err(error) => {
                        last_error = error.to_string();
                        continue;
                    }
                };
            let review = sessions.review(engine.project(), &authorization.task_id, &proposal)?;
            if review.status != ReviewStatus::Ready {
                last_error = findings_summary(&review.violations);
                review_feedback = Some(review);
                continue;
            }
            let application = sessions.apply(engine, &authorization.task_id, &proposal)?;
            if let Some(change) = application.change {
                return Ok(AppliedRevision {
                    revision: change.revision,
                });
            }
            last_error = "review passed without committing a material change".to_owned();
            review_feedback = Some(application.review);
        }
        let _ = sessions.revoke(&authorization.task_id);
        Err(AutopilotError::ProposalExhausted(last_error))
    }

    fn evaluate(
        &mut self,
        project: &Project,
        memory: &AutopilotMemory,
        instruction: &str,
        task: &CompositionTask,
        render: &RenderObservation,
    ) -> Result<EvaluatorVerdict, AutopilotError> {
        let context = collect_context(project, task.brief.target)?;
        let payload = json!({
            "user_instruction": instruction,
            "conversation_history": memory_prompt(memory),
            "creative_brief": task.brief,
            "project_after_composer_commit": context,
            "rendered_audio_measurements": render,
        });
        let mut feedback = None;
        for _ in 0..self.config.max_evaluator_attempts.max(1) {
            let prompt = format!(
                "You are an independent music Evaluator in a fully automatic system. You did not \
compose this revision. Judge it against the user's instruction, required objectives, style_context, \
the post-commit events, neutral arrangement observations, and rendered-audio measurements. Do not \
apply a universal recipe or mark repetition, sparseness, dissonance, symmetry, rigidity, or \
complexity as inherently wrong. Choose accept when there is no concrete brief-related reason for \
another edit. Choose revise only for specific audible or structurally evidenced mismatches; then \
provide at least one modify decision, exactly one decision per observation, and a track or tick \
range for every modify observation. Never defer the decision back to the Composer and never ask the \
user. Return only the schema result.\n\nINPUT:\n{}\n\nPREVIOUS INVALID RESULT FEEDBACK:\n{}",
                serde_json::to_string_pretty(&payload)?,
                feedback.as_deref().unwrap_or("none")
            );
            match self.complete_typed::<EvaluatorVerdict>(ModelRole::Evaluator, &prompt) {
                Ok(result) => match validate_verdict(&result) {
                    Ok(()) => return Ok(result),
                    Err(error) => feedback = Some(error),
                },
                Err(error @ AutopilotError::Model { .. }) => return Err(error),
                Err(error) => feedback = Some(error.to_string()),
            }
        }
        Err(AutopilotError::InvalidModelResult {
            role: ModelRole::Evaluator,
            message: feedback.unwrap_or_else(|| "unknown evaluator failure".to_owned()),
        })
    }

    fn record_evaluation(
        &mut self,
        project: &Project,
        sessions: &mut CompositionSessions,
        authorization: &AuthorizedTask,
        verdict: EvaluatorVerdict,
    ) -> Result<StoredCritique, AutopilotError> {
        let report = CritiqueReport {
            brief_id: authorization.task.brief.id.clone(),
            base_revision: project.revision,
            summary: verdict.summary,
            observations: verdict.observations,
            decisions: verdict.decisions,
            next_focus: verdict.next_focus,
        };
        sessions
            .record_critique(project, &authorization.task_id, report)
            .map_err(AutopilotError::Session)
    }

    fn task_for_revision(
        &self,
        project: &Project,
        brief: &CreativeBrief,
        rack: &InstrumentRack,
    ) -> CompositionTask {
        CompositionTask {
            brief: brief.clone(),
            scope: EditScope {
                base_revision: project.revision,
                tracks: TrackAccess::All,
                timeline: vec![composition_engine::ScopedTickRange {
                    track_id: None,
                    range: brief.target,
                }],
                capabilities: vec![
                    EditCapability::Notes,
                    EditCapability::Controls,
                    EditCapability::Clips,
                    EditCapability::Tracks,
                    EditCapability::Tempo,
                    EditCapability::Meter,
                    EditCapability::Instruments,
                    EditCapability::Mixer,
                ],
                protected_regions: Vec::new(),
                allowed_instrument_ids: rack
                    .catalog()
                    .into_iter()
                    .map(|instrument| instrument.id)
                    .collect(),
                // Invoking Autopilot is explicit project-level musical edit
                // authority. The model still cannot touch anything outside the
                // Project or the bounded target range.
                allow_new_tracks: true,
                allow_remove_tracks: true,
                allow_remove_events: true,
                max_operations: self.config.max_operations_per_revision,
            },
        }
    }

    fn complete_typed<T: DeserializeOwned + JsonSchema>(
        &mut self,
        role: ModelRole,
        prompt: &str,
    ) -> Result<T, AutopilotError> {
        let mut schema = serde_json::to_value(schema_for!(T))?;
        normalize_structured_output_schema(&mut schema);
        let value = self
            .model
            .complete(ModelRequest {
                role,
                prompt,
                schema: &schema,
            })
            .map_err(|error| AutopilotError::Model {
                role,
                message: error.to_string(),
            })?;
        serde_json::from_value(value).map_err(|error| AutopilotError::InvalidModelResult {
            role,
            message: error.to_string(),
        })
    }
}

/// Converts Schemars' general Draft 2020-12 output into the strict subset
/// accepted by structured model responses: closed objects, every property
/// required, `anyOf` unions, and no Rust integer `format` annotations or
/// defaults.
fn normalize_structured_output_schema(schema: &mut Value) {
    match schema {
        Value::Array(values) => {
            for value in values {
                normalize_structured_output_schema(value);
            }
        }
        Value::Object(object) => {
            object.remove("$schema");
            object.remove("default");
            object.remove("format");
            if let Some(one_of) = object.remove("oneOf") {
                object.insert("anyOf".to_owned(), one_of);
            }
            if let Some(constant) = object.remove("const") {
                object.insert("enum".to_owned(), Value::Array(vec![constant]));
            }
            if let Some(Value::Object(properties)) = object.get("properties") {
                let required = properties.keys().cloned().map(Value::String).collect();
                object.insert("required".to_owned(), Value::Array(required));
                object.insert("additionalProperties".to_owned(), Value::Bool(false));
            }
            for value in object.values_mut() {
                normalize_structured_output_schema(value);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DirectedBrief {
    summary: String,
    target: TickRange,
    objectives: Vec<CreativeObjective>,
    #[serde(default)]
    freedoms: Vec<String>,
    #[serde(default)]
    style_context: Vec<String>,
    #[serde(default)]
    rhythm: RhythmConstraints,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EvaluationConclusion {
    Accept,
    Revise,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EvaluatorVerdict {
    conclusion: EvaluationConclusion,
    summary: String,
    #[serde(default)]
    observations: Vec<CritiqueObservation>,
    #[serde(default)]
    decisions: Vec<CritiqueDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_focus: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct MusicContext {
    summary: ProjectSummary,
    windows: Vec<ClipWindow>,
    arrangement: ArrangementReport,
}

#[derive(Clone, Debug, Serialize)]
struct AuthorizedTask {
    task_id: String,
    task: CompositionTask,
}

struct AppliedRevision {
    revision: u64,
}

fn authorize(
    sessions: &mut CompositionSessions,
    project: &Project,
    task: CompositionTask,
) -> Result<AuthorizedTask, AutopilotError> {
    let authorization = sessions.authorize(project, task);
    let Some(authorized) = authorization.authorized_task else {
        return Err(AutopilotError::Authorization(findings_summary(
            &authorization.violations,
        )));
    };
    Ok(AuthorizedTask {
        task_id: authorized.task_id,
        task: authorized.task,
    })
}

fn validate_directed(result: &DirectedBrief, maximum_end_tick: i64) -> Result<(), String> {
    if result.summary.trim().is_empty() {
        return Err("brief summary is empty".to_owned());
    }
    if !result.target.is_valid() || result.target.end_tick > maximum_end_tick {
        return Err(format!(
            "target must be valid and end no later than tick {maximum_end_tick}"
        ));
    }
    if result.objectives.is_empty()
        || !result
            .objectives
            .iter()
            .any(|objective| objective.priority == composition_engine::ObjectivePriority::Required)
    {
        return Err("at least one objective must be required".to_owned());
    }
    let mut ids = BTreeSet::new();
    if result.objectives.iter().any(|objective| {
        objective.id.trim().is_empty()
            || objective.description.trim().is_empty()
            || !ids.insert(objective.id.as_str())
    }) {
        return Err("objective IDs and descriptions must be non-empty and unique".to_owned());
    }
    Ok(())
}

fn validate_verdict(result: &EvaluatorVerdict) -> Result<(), String> {
    if result.summary.trim().is_empty() {
        return Err("evaluation summary is empty".to_owned());
    }
    match result.conclusion {
        EvaluationConclusion::Accept => {
            if result
                .decisions
                .iter()
                .any(|decision| decision.disposition == CritiqueDisposition::Modify)
            {
                return Err("an accepted result cannot contain a modify decision".to_owned());
            }
        }
        EvaluationConclusion::Revise => {
            if result.observations.is_empty() {
                return Err("a revision result needs at least one observation".to_owned());
            }
            if !result
                .decisions
                .iter()
                .any(|decision| decision.disposition == CritiqueDisposition::Modify)
            {
                return Err("a revision result needs at least one modify decision".to_owned());
            }
            let expected: BTreeSet<_> = result
                .observations
                .iter()
                .map(|observation| observation.id.as_str())
                .collect();
            let actual: BTreeSet<_> = result
                .decisions
                .iter()
                .map(|decision| decision.observation_id.as_str())
                .collect();
            if expected != actual || actual.len() != result.decisions.len() {
                return Err("every observation needs exactly one evaluator decision".to_owned());
            }
            for decision in &result.decisions {
                if decision.disposition != CritiqueDisposition::Modify {
                    continue;
                }
                let observation = result
                    .observations
                    .iter()
                    .find(|observation| observation.id == decision.observation_id)
                    .expect("validated decision must reference an observation");
                if observation.location.track_id.is_none() && observation.location.range.is_none() {
                    return Err(format!(
                        "modify observation '{}' needs a track or tick range",
                        observation.id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn collect_context(project: &Project, target: TickRange) -> Result<MusicContext, AutopilotError> {
    let bar_tick = project
        .time_signature
        .bar_length_tick(project.ppq)
        .map_err(|error| AutopilotError::Arrangement(error.to_string()))?;
    let start_tick = target.start_tick.saturating_sub(bar_tick).max(0);
    let end_tick = target.end_tick.saturating_add(bar_tick).max(start_tick + 1);
    let mut windows = Vec::new();
    for track in &project.tracks {
        let TrackSource::Midi { clips, .. } = &track.source else {
            continue;
        };
        for clip in clips {
            windows.push(
                project
                    .clip_window(&track.id, &clip.id, start_tick, end_tick)
                    .map_err(|error| AutopilotError::Arrangement(error.to_string()))?,
            );
        }
    }
    let arrangement = ArrangementAnalyzer
        .analyze(project)
        .map_err(|error| AutopilotError::Arrangement(error.to_string()))?;
    Ok(MusicContext {
        summary: project.summary(),
        windows,
        arrangement,
    })
}

fn memory_prompt(memory: &AutopilotMemory) -> &[AutopilotMemoryTurn] {
    let start = memory
        .turns
        .len()
        .saturating_sub(MAX_MEMORY_TURNS_IN_PROMPT);
    &memory.turns[start..]
}

fn findings_summary(findings: &[ReviewFinding]) -> String {
    if findings.is_empty() {
        return "no detailed finding was returned".to_owned();
    }
    findings
        .iter()
        .map(|finding| format!("{:?}: {}", finding.code, finding.message))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn analyze_render(buffer: &AudioBuffer) -> RenderObservation {
    let channels = buffer.channels.max(1);
    let frames = buffer.frames();
    let mut square_sum = 0.0_f64;
    let mut left_square = 0.0_f64;
    let mut right_square = 0.0_f64;
    let mut silent_frames = 0_usize;
    let mut zero_crossings = 0_usize;
    let mut previous_mono = 0.0_f32;
    let mut has_previous = false;
    for frame in 0..frames {
        let start = frame * channels;
        let samples = &buffer.samples[start..(start + channels).min(buffer.samples.len())];
        let mut frame_peak = 0.0_f32;
        let mut mono = 0.0_f32;
        for sample in samples {
            square_sum += f64::from(*sample) * f64::from(*sample);
            frame_peak = frame_peak.max(sample.abs());
            mono += *sample;
        }
        mono /= samples.len().max(1) as f32;
        if frame_peak < 0.000_5 {
            silent_frames += 1;
        }
        if has_previous && previous_mono.signum() != mono.signum() {
            zero_crossings += 1;
        }
        previous_mono = mono;
        has_previous = true;
        if let Some(left) = samples.first() {
            left_square += f64::from(*left) * f64::from(*left);
        }
        if let Some(right) = samples.get(1).or_else(|| samples.first()) {
            right_square += f64::from(*right) * f64::from(*right);
        }
    }
    let sample_count = buffer.samples.len().max(1) as f64;
    let rms = (square_sum / sample_count).sqrt() as f32;
    let left_rms = (left_square / frames.max(1) as f64).sqrt() as f32;
    let right_rms = (right_square / frames.max(1) as f64).sqrt() as f32;
    let balance_denominator = (left_rms + right_rms).max(f32::EPSILON);
    const WINDOWS: usize = 16;
    let mut window_rms_dbfs = Vec::with_capacity(WINDOWS);
    for window in 0..WINDOWS {
        let start_frame = frames * window / WINDOWS;
        let end_frame = frames * (window + 1) / WINDOWS;
        let start = start_frame * channels;
        let end = (end_frame * channels).min(buffer.samples.len());
        let samples = &buffer.samples[start.min(end)..end];
        let window_rms = if samples.is_empty() {
            0.0
        } else {
            (samples
                .iter()
                .map(|sample| f64::from(*sample) * f64::from(*sample))
                .sum::<f64>()
                / samples.len() as f64)
                .sqrt() as f32
        };
        window_rms_dbfs.push(dbfs(window_rms));
    }
    RenderObservation {
        sample_rate: buffer.sample_rate,
        channels: buffer.channels,
        frames,
        duration_seconds: frames as f64 / f64::from(buffer.sample_rate.max(1)),
        peak: buffer.peak(),
        rms_dbfs: dbfs(rms),
        silent_frame_fraction: silent_frames as f32 / frames.max(1) as f32,
        stereo_balance: (right_rms - left_rms) / balance_denominator,
        zero_crossing_rate: zero_crossings as f32 / frames.max(1) as f32,
        window_rms_dbfs,
    }
}

fn dbfs(value: f32) -> f32 {
    20.0 * value.max(1.0e-9).log10()
}

#[cfg(test)]
mod tests {
    use super::*;
    use composition_engine::{
        CompositionPlan, CoverageEvidence, CreativeDecision, CritiqueLocation, ObjectiveCoverage,
        ObjectivePriority, PlannedSection, PlannedTrackRole,
    };
    use music_core::{Command, NoteEvent, Patch};

    fn directed() -> Value {
        serde_json::to_value(DirectedBrief {
            summary: "Create a concise piano opening".to_owned(),
            target: TickRange {
                start_tick: 0,
                end_tick: 3_840,
            },
            objectives: vec![CreativeObjective {
                id: "opening".to_owned(),
                description: "Establish a clear opening gesture".to_owned(),
                priority: ObjectivePriority::Required,
            }],
            freedoms: vec!["Choose the voicing".to_owned()],
            style_context: vec!["Keep the result concise".to_owned()],
            rhythm: RhythmConstraints::default(),
        })
        .unwrap()
    }

    fn proposal(base_revision: u64, based_on: Option<String>, velocity: u8) -> Value {
        let operation = if base_revision == 0 {
            Command::AddNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note: NoteEvent {
                    id: "auto-note".to_owned(),
                    start_tick: 0,
                    duration_tick: 960,
                    pitch: 60,
                    velocity,
                },
            }
        } else {
            Command::SetNoteVelocity {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note_id: "auto-note".to_owned(),
                velocity,
            }
        };
        let critique_responses = based_on
            .as_ref()
            .map(|_| {
                vec![composition_engine::CritiqueResponse {
                    observation_id: "attack".to_owned(),
                    rationale: "Increase the attack at the evaluator location".to_owned(),
                }]
            })
            .unwrap_or_default();
        serde_json::to_value(CompositionProposal {
            brief_id: "placeholder".to_owned(),
            plan: CompositionPlan {
                summary: "Create the opening".to_owned(),
                sections: vec![PlannedSection {
                    id: "opening-section".to_owned(),
                    range: TickRange {
                        start_tick: 0,
                        end_tick: 960,
                    },
                    intent: "State the gesture".to_owned(),
                }],
                track_roles: vec![PlannedTrackRole {
                    track_id: "piano".to_owned(),
                    role: "opening voice".to_owned(),
                }],
                objective_coverage: vec![ObjectiveCoverage {
                    objective_id: "opening".to_owned(),
                    evidence: vec![CoverageEvidence {
                        description: "Opening note establishes the gesture".to_owned(),
                        section_id: Some("opening-section".to_owned()),
                        track_id: Some("piano".to_owned()),
                        range: Some(TickRange {
                            start_tick: 0,
                            end_tick: 960,
                        }),
                    }],
                }],
                decisions: vec![CreativeDecision {
                    decision: "Use a centered middle-C attack".to_owned(),
                    rationale: "It provides an unambiguous opening".to_owned(),
                }],
            },
            based_on_critique_id: based_on,
            critique_responses,
            patch: Patch {
                base_revision: Some(base_revision),
                description: Some("automatic opening".to_owned()),
                operations: vec![operation],
            },
        })
        .unwrap()
    }

    fn accept() -> Value {
        serde_json::to_value(EvaluatorVerdict {
            conclusion: EvaluationConclusion::Accept,
            summary: "The required opening gesture is present".to_owned(),
            observations: Vec::new(),
            decisions: Vec::new(),
            next_focus: None,
        })
        .unwrap()
    }

    fn revise() -> Value {
        serde_json::to_value(EvaluatorVerdict {
            conclusion: EvaluationConclusion::Revise,
            summary: "The opening attack is too restrained for the requested clarity".to_owned(),
            observations: vec![CritiqueObservation {
                id: "attack".to_owned(),
                location: CritiqueLocation {
                    label: Some("opening".to_owned()),
                    track_id: Some("piano".to_owned()),
                    range: Some(TickRange {
                        start_tick: 0,
                        end_tick: 960,
                    }),
                },
                observation: "The first attack is restrained".to_owned(),
                consequence: "The requested clarity is weakened".to_owned(),
                proposed_revision: Some("Strengthen the first attack".to_owned()),
            }],
            decisions: vec![CritiqueDecision {
                observation_id: "attack".to_owned(),
                disposition: CritiqueDisposition::Modify,
                rationale: "A stronger attack better serves the required objective".to_owned(),
            }],
            next_focus: Some("Opening articulation".to_owned()),
        })
        .unwrap()
    }

    fn patch_brief_id(value: &mut Value, brief_id: &str) {
        value["brief_id"] = Value::String(brief_id.to_owned());
    }

    struct PromptAwareModel {
        stage: usize,
        revise_once: bool,
        director_history_lengths: Vec<usize>,
    }

    impl StructuredModel for PromptAwareModel {
        fn complete(&mut self, request: ModelRequest<'_>) -> Result<Value, ModelBackendError> {
            self.stage += 1;
            match request.role {
                ModelRole::Director => {
                    let payload = extract_input(request.prompt)?;
                    self.director_history_lengths.push(
                        payload["conversation_history"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or_default(),
                    );
                    Ok(directed())
                }
                ModelRole::Composer => {
                    let payload: Value = extract_input(request.prompt)?;
                    let task = &payload["authorized_task"];
                    let brief_id = task["task"]["brief"]["id"]
                        .as_str()
                        .ok_or_else(|| ModelBackendError::new("missing brief ID"))?;
                    let revision = task["task"]["scope"]["base_revision"]
                        .as_u64()
                        .ok_or_else(|| ModelBackendError::new("missing revision"))?;
                    let based_on = payload["linked_evaluator_critique"]["id"]
                        .as_str()
                        .map(str::to_owned);
                    let mut result =
                        proposal(revision, based_on, if revision == 0 { 72 } else { 96 });
                    patch_brief_id(&mut result, brief_id);
                    Ok(result)
                }
                ModelRole::Evaluator if self.revise_once => {
                    self.revise_once = false;
                    Ok(revise())
                }
                ModelRole::Evaluator => Ok(accept()),
            }
        }
    }

    fn extract_input(prompt: &str) -> Result<Value, ModelBackendError> {
        let input = prompt
            .split_once("INPUT:\n")
            .map(|(_, input)| input)
            .ok_or_else(|| ModelBackendError::new("missing prompt input"))?;
        let json = input
            .split_once("\n\nPREVIOUS INVALID")
            .map(|(json, _)| json)
            .unwrap_or(input);
        serde_json::from_str(json.trim())
            .map_err(|error| ModelBackendError::new(format!("invalid prompt input: {error}")))
    }

    #[test]
    fn automatic_loop_commits_and_finishes_without_human_handoff() {
        let model = PromptAwareModel {
            stage: 0,
            revise_once: false,
            director_history_lengths: Vec::new(),
        };
        let mut autopilot = Autopilot::new(model);
        let mut engine = ProjectEngine::new(Project::default());
        let mut memory = AutopilotMemory::default();
        let run = autopilot
            .run_instruction(
                &mut engine,
                &InstrumentRack::default(),
                &mut memory,
                "写一个清晰的钢琴开头",
            )
            .unwrap();

        assert_eq!(run.outcome.status, AutopilotStatus::Completed);
        assert_eq!(run.outcome.final_revision, 1);
        assert_eq!(memory.turns.len(), 1);
        assert_eq!(
            engine
                .project()
                .midi_clip("piano", "piano-main")
                .unwrap()
                .notes
                .len(),
            1
        );
        assert!(run.final_audio.peak() > 0.0);
    }

    #[test]
    fn evaluator_modify_decision_is_automatically_implemented() {
        let model = PromptAwareModel {
            stage: 0,
            revise_once: true,
            director_history_lengths: Vec::new(),
        };
        let mut autopilot = Autopilot::new(model);
        let mut engine = ProjectEngine::new(Project::default());
        let mut memory = AutopilotMemory::default();
        let run = autopilot
            .run_instruction(
                &mut engine,
                &InstrumentRack::default(),
                &mut memory,
                "写一个清晰有力度的钢琴开头",
            )
            .unwrap();

        assert_eq!(run.outcome.final_revision, 2);
        assert_eq!(run.outcome.committed_revisions, vec![1, 2]);
        assert_eq!(run.outcome.evaluator_rounds, 2);
        let note = &engine
            .project()
            .midi_clip("piano", "piano-main")
            .unwrap()
            .notes[0];
        assert_eq!(note.velocity, 96);
    }

    #[test]
    fn consecutive_instructions_share_memory_and_continue_the_project() {
        let model = PromptAwareModel {
            stage: 0,
            revise_once: false,
            director_history_lengths: Vec::new(),
        };
        let mut autopilot = Autopilot::new(model);
        let mut engine = ProjectEngine::new(Project::default());
        let mut memory = AutopilotMemory::default();
        let session_id = memory.session_id.clone();

        let first = autopilot
            .run_instruction(
                &mut engine,
                &InstrumentRack::default(),
                &mut memory,
                "写一个安静的钢琴开头",
            )
            .unwrap();
        let second = autopilot
            .run_instruction(
                &mut engine,
                &InstrumentRack::default(),
                &mut memory,
                "把后续推进感加强，但保留开头",
            )
            .unwrap();

        assert_eq!(first.outcome.starting_revision, 0);
        assert_eq!(first.outcome.final_revision, 1);
        assert_eq!(second.outcome.starting_revision, 1);
        assert_eq!(second.outcome.final_revision, 2);
        assert_eq!(memory.session_id, session_id);
        assert_eq!(memory.turns.len(), 2);
        assert_eq!(memory.turns[1].instruction, "把后续推进感加强，但保留开头");
        assert_eq!(
            engine
                .project()
                .midi_clip("piano", "piano-main")
                .unwrap()
                .notes[0]
                .velocity,
            96
        );
        assert_eq!(autopilot.into_model().director_history_lengths, vec![0, 1]);
    }

    struct FailingEvaluatorModel {
        inner: PromptAwareModel,
        evaluator_calls: usize,
    }

    impl StructuredModel for FailingEvaluatorModel {
        fn complete(&mut self, request: ModelRequest<'_>) -> Result<Value, ModelBackendError> {
            if request.role == ModelRole::Evaluator {
                self.evaluator_calls += 1;
                return Err(ModelBackendError::new("evaluator unavailable"));
            }
            self.inner.complete(request)
        }
    }

    #[test]
    fn model_failure_rolls_back_the_entire_instruction() {
        let model = FailingEvaluatorModel {
            inner: PromptAwareModel {
                stage: 0,
                revise_once: false,
                director_history_lengths: Vec::new(),
            },
            evaluator_calls: 0,
        };
        let mut autopilot = Autopilot::new(model);
        let original_project = Project::default();
        let original_memory = AutopilotMemory::default();
        let mut engine = ProjectEngine::new(original_project.clone());
        let mut memory = original_memory.clone();

        let result = autopilot.run_instruction(
            &mut engine,
            &InstrumentRack::default(),
            &mut memory,
            "写一个钢琴开头",
        );

        assert!(result.is_err());
        assert_eq!(engine.project(), &original_project);
        assert_eq!(memory, original_memory);
        assert_eq!(autopilot.into_model().evaluator_calls, 1);
    }

    #[test]
    fn render_analysis_is_finite_for_silence() {
        let analysis = analyze_render(&AudioBuffer::new(48_000, 2, 480));
        assert!(analysis.rms_dbfs.is_finite());
        assert_eq!(analysis.silent_frame_fraction, 1.0);
        assert_eq!(analysis.window_rms_dbfs.len(), 16);
    }

    #[test]
    fn generated_model_schema_uses_the_strict_supported_subset() {
        let mut schema = serde_json::to_value(schema_for!(CompositionProposal)).unwrap();
        normalize_structured_output_schema(&mut schema);
        assert!(schema.get("$schema").is_none());
        assert_schema_is_strict(&schema);
    }

    fn assert_schema_is_strict(schema: &Value) {
        match schema {
            Value::Array(values) => {
                for value in values {
                    assert_schema_is_strict(value);
                }
            }
            Value::Object(object) => {
                assert!(!object.contains_key("oneOf"));
                assert!(!object.contains_key("const"));
                assert!(!object.contains_key("default"));
                assert!(!object.contains_key("format"));
                if let Some(Value::Object(properties)) = object.get("properties") {
                    assert_eq!(
                        object.get("additionalProperties"),
                        Some(&Value::Bool(false))
                    );
                    let required = object
                        .get("required")
                        .and_then(Value::as_array)
                        .expect("object properties need required keys");
                    let required: BTreeSet<_> = required.iter().filter_map(Value::as_str).collect();
                    let expected: BTreeSet<_> = properties.keys().map(String::as_str).collect();
                    assert_eq!(required, expected);
                }
                for value in object.values() {
                    assert_schema_is_strict(value);
                }
            }
            _ => {}
        }
    }
}
