#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use audio_engine::{
    AssetPack, AudioBuffer, DEFAULT_SAMPLE_RATE, InstrumentDescriptor, InstrumentRack,
    PlaybackHandle, play_buffer, render_project_with_rack, wav_bytes, write_wav,
};
use autopilot_engine::{Autopilot, AutopilotMemory, AutopilotOutcome, CodexCliModel};
use composition_engine::{
    CompositionProposal, CompositionSessions, CompositionTask, CritiqueReport, ProposalApplication,
    ProposalReview, ProposalReviewer, ReviewEnvironment, StoredCritique, TaskAuthorization,
};
use music_core::{
    ClipWindow, Command, Patch, PatchPreview, Project, ProjectEngine, ProjectSummary,
};
use project_package::{
    ArtifactDirectory, ArtifactWrite, ProjectPackage, SourceAssetLocation, SourceAssetReference,
};
use rfd::FileDialog;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::State;

struct AppState {
    workspace: Mutex<Workspace>,
    playback: Arc<Mutex<PlaybackState>>,
    instruments: Mutex<InstrumentRack>,
}

struct Workspace {
    project: ProjectEngine,
    composition_sessions: CompositionSessions,
    package: Option<ProjectPackage>,
    name: String,
    path: Option<PathBuf>,
    saved_revision: u64,
    autopilot_memory: AutopilotMemory,
}

impl Workspace {
    fn new(instruments: &InstrumentRack) -> Self {
        let project = Project::default();
        Self {
            saved_revision: project.revision,
            autopilot_memory: AutopilotMemory::default(),
            project: ProjectEngine::new(project),
            composition_sessions: CompositionSessions::new(proposal_reviewer(instruments)),
            package: None,
            name: "未命名工程".to_owned(),
            path: None,
        }
    }

    fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            project: self.project.project().clone(),
            name: self.name.clone(),
            path: self
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            dirty: self.project.revision() != self.saved_revision,
            saved_revision: self.saved_revision,
        }
    }

    fn mark_saved(&mut self) {
        self.saved_revision = self.project.revision();
    }

    fn mark_changed(&mut self) {
        // The session authority is scoped to the revision it observed.  GUI
        // edits therefore keep the same invalidation behavior as AI edits.
        self.composition_sessions.clear();
    }

    fn authorize_task(&mut self, task: CompositionTask) -> TaskAuthorization {
        self.composition_sessions
            .authorize(self.project.project(), task)
    }

    fn review_proposal(
        &self,
        task_id: &str,
        proposal: &CompositionProposal,
    ) -> Result<ProposalReview, String> {
        self.composition_sessions
            .review(self.project.project(), task_id, proposal)
            .map_err(|error| error.to_string())
    }

    #[allow(dead_code)] // Reserved for a trusted evaluator host, never a model-facing Tauri command.
    fn record_critique(
        &mut self,
        task_id: &str,
        report: CritiqueReport,
    ) -> Result<StoredCritique, String> {
        let stored = self
            .composition_sessions
            .record_critique(self.project.project(), task_id, report)
            .map_err(|error| error.to_string())?;
        if let Some(package) = self.package.as_ref() {
            let _ = package
                .write_json_artifact(
                    ArtifactDirectory::History,
                    &format!(
                        "revision-{}-{}.json",
                        stored.report.base_revision, stored.id
                    ),
                    &stored,
                )
                .map_err(|error| eprintln!("warning: could not write critique history: {error}"));
        }
        Ok(stored)
    }

    fn apply_proposal(
        &mut self,
        task_id: &str,
        proposal: &CompositionProposal,
    ) -> Result<ProposalApplication, String> {
        let result = self
            .composition_sessions
            .apply(&mut self.project, task_id, proposal)
            .map_err(|error| error.to_string())?;
        // A successful proposal changes the same ProjectEngine as GUI edits;
        // the saved revision remains the last persisted revision.
        if let (Some(package), Some(change)) = (self.package.as_ref(), result.change.as_ref()) {
            let revision = change.revision;
            let _ = package
                .write_json_artifact(
                    ArtifactDirectory::History,
                    &format!("revision-{revision}-proposal.json"),
                    proposal,
                )
                .map_err(|error| eprintln!("warning: could not write proposal history: {error}"));
            let _ = package
                .write_json_artifact(
                    ArtifactDirectory::History,
                    &format!("revision-{revision}-application.json"),
                    &result,
                )
                .map_err(|error| {
                    eprintln!("warning: could not write application history: {error}")
                });
        }
        Ok(result)
    }

    fn replace_project(&mut self, project: Project, instruments: &InstrumentRack) {
        self.project = ProjectEngine::new(project);
        self.composition_sessions = CompositionSessions::new(proposal_reviewer(instruments));
        self.package = None;
        self.path = None;
        self.name = "未命名工程".to_owned();
        self.saved_revision = self.project.revision();
        self.autopilot_memory = AutopilotMemory::default();
    }

    fn replace_package(
        &mut self,
        package: ProjectPackage,
        project: Project,
        instruments: &InstrumentRack,
    ) {
        self.saved_revision = project.revision;
        self.project = ProjectEngine::new(project);
        self.composition_sessions.clear();
        self.name = package.manifest().name.clone();
        self.path = Some(package.root().to_path_buf());
        self.autopilot_memory = load_package_autopilot_memory(&package);
        self.package = Some(package);
        self.composition_sessions = CompositionSessions::new(proposal_reviewer(instruments));
    }

    fn save_as(&mut self, parent: &Path, name: &str) -> Result<(), String> {
        let package = match self.package.as_ref() {
            Some(source) => source.duplicate(parent, name, self.project.project()),
            None => ProjectPackage::create(parent, name, self.project.project()),
        }
        .map_err(|error| error.to_string())?;
        self.name = package.manifest().name.clone();
        self.path = Some(package.root().to_path_buf());
        self.package = Some(package);
        self.autopilot_memory = AutopilotMemory::default();
        self.mark_saved();
        Ok(())
    }

    fn create_new(
        &mut self,
        parent: &Path,
        name: &str,
        instruments: &InstrumentRack,
    ) -> Result<(), String> {
        let project = Project::default();
        let package =
            ProjectPackage::create(parent, name, &project).map_err(|error| error.to_string())?;
        self.replace_package(package, project, instruments);
        Ok(())
    }

    fn save(&mut self) -> Result<(), String> {
        let package = self
            .package
            .as_ref()
            .ok_or_else(|| "工程尚未命名，请使用保存为指定工程位置".to_owned())?;
        package
            .save_if_revision(self.saved_revision, self.project.project())
            .map_err(|error| error.to_string())?;
        self.mark_saved();
        Ok(())
    }
}

struct PlaybackState {
    handle: Option<PlaybackHandle>,
    started_at: Option<Instant>,
    rendering: bool,
    generation: u64,
    error: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        let instruments = InstrumentRack::default();
        Self {
            workspace: Mutex::new(Workspace::new(&instruments)),
            playback: Arc::new(Mutex::new(PlaybackState {
                handle: None,
                started_at: None,
                rendering: false,
                generation: 0,
                error: None,
            })),
            instruments: Mutex::new(instruments),
        }
    }
}

#[derive(serde::Serialize)]
struct PlaybackSnapshot {
    playing: bool,
    rendering: bool,
    elapsed_seconds: f64,
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct WorkspaceSnapshot {
    project: Project,
    name: String,
    path: Option<String>,
    dirty: bool,
    saved_revision: u64,
}

#[tauri::command]
fn project_snapshot(state: State<'_, AppState>) -> Result<Project, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    Ok(workspace.project.project().clone())
}

#[tauri::command]
fn workspace_snapshot(state: State<'_, AppState>) -> Result<WorkspaceSnapshot, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    Ok(workspace.snapshot())
}

#[derive(serde::Serialize)]
struct PianoAssetSnapshot {
    asset_id: Option<String>,
    name: Option<String>,
    location: Option<String>,
}

#[tauri::command]
fn piano_asset_snapshot(state: State<'_, AppState>) -> Result<PianoAssetSnapshot, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let Some(asset) = workspace
        .package
        .as_ref()
        .and_then(|package| package.source_asset(PIANO_ASSET_ROLE))
    else {
        return Ok(PianoAssetSnapshot {
            asset_id: None,
            name: None,
            location: None,
        });
    };
    let location = match &asset.location {
        SourceAssetLocation::External { manifest_path }
        | SourceAssetLocation::Package { manifest_path } => manifest_path.clone(),
    };
    Ok(PianoAssetSnapshot {
        asset_id: Some(asset.asset_id.clone()),
        name: Some(asset.name.clone()),
        location: Some(location),
    })
}

#[tauri::command]
fn project_revision(state: State<'_, AppState>) -> Result<u64, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    Ok(workspace.project.revision())
}

#[tauri::command]
fn project_summary(state: State<'_, AppState>) -> Result<ProjectSummary, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    Ok(workspace.project.project().summary())
}

#[tauri::command]
fn clip_window(
    state: State<'_, AppState>,
    track_id: String,
    clip_id: String,
    start_tick: i64,
    end_tick: i64,
) -> Result<ClipWindow, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    workspace
        .project
        .project()
        .clip_window(&track_id, &clip_id, start_tick, end_tick)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn instrument_catalog(state: State<'_, AppState>) -> Vec<InstrumentDescriptor> {
    state
        .instruments
        .lock()
        .map(|instruments| instruments.catalog())
        .unwrap_or_default()
}

#[tauri::command]
fn apply_command(state: State<'_, AppState>, command: Command) -> Result<u64, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let change = workspace
        .project
        .apply(command)
        .map_err(|error| error.to_string())?;
    workspace.mark_changed();
    Ok(change.revision)
}

#[tauri::command]
fn apply_patch(state: State<'_, AppState>, patch: Patch) -> Result<u64, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let change = workspace
        .project
        .apply_patch(patch)
        .map_err(|error| error.to_string())?;
    workspace.mark_changed();
    Ok(change.revision)
}

#[tauri::command]
fn preview_patch(state: State<'_, AppState>, patch: Patch) -> Result<PatchPreview, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    workspace
        .project
        .preview_patch(&patch)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn authorize_composition_task(
    state: State<'_, AppState>,
    task: CompositionTask,
) -> Result<TaskAuthorization, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    Ok(workspace.authorize_task(task))
}

#[tauri::command]
fn review_authorized_proposal(
    state: State<'_, AppState>,
    task_id: String,
    proposal: CompositionProposal,
) -> Result<ProposalReview, String> {
    let workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    workspace.review_proposal(&task_id, &proposal)
}

#[tauri::command]
fn apply_authorized_proposal(
    state: State<'_, AppState>,
    task_id: String,
    proposal: CompositionProposal,
) -> Result<ProposalApplication, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    workspace.apply_proposal(&task_id, &proposal)
}

#[tauri::command]
fn revoke_composition_task(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    workspace
        .composition_sessions
        .revoke(&task_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn undo(state: State<'_, AppState>) -> Result<bool, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let changed = workspace.project.undo();
    if changed {
        workspace.mark_changed();
    }
    Ok(changed)
}

#[tauri::command]
fn redo(state: State<'_, AppState>) -> Result<bool, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let changed = workspace.project.redo();
    if changed {
        workspace.mark_changed();
    }
    Ok(changed)
}

#[tauri::command]
fn new_project(
    state: State<'_, AppState>,
    parent: Option<String>,
    name: Option<String>,
) -> Result<WorkspaceSnapshot, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let instruments = InstrumentRack::default();
    match (parent, name) {
        (Some(parent), Some(name)) => {
            workspace.create_new(Path::new(&parent), &name, &instruments)?;
        }
        (None, None) => workspace.replace_project(Project::default(), &instruments),
        _ => return Err("新建工程需要同时提供位置和名称".to_owned()),
    }
    let snapshot = workspace.snapshot();
    *state
        .instruments
        .lock()
        .map_err(|_| "instrument rack lock poisoned".to_owned())? = instruments;
    Ok(snapshot)
}

#[tauri::command]
fn choose_project_location() -> Option<String> {
    FileDialog::new()
        .set_title("选择工程位置")
        .pick_folder()
        .map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn load_project(
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<WorkspaceSnapshot, String> {
    let path = path
        .map(PathBuf::from)
        .or_else(|| {
            FileDialog::new()
                .set_title("打开 AI Music 工程")
                .pick_folder()
        })
        .ok_or_else(|| "已取消打开工程".to_owned())?;
    let (package, loaded, instruments) = tauri::async_runtime::spawn_blocking(move || {
        let (package, loaded) = ProjectPackage::open(&path).map_err(|error| error.to_string())?;
        let instruments = rack_for_package(&package, &loaded)?;
        Ok::<_, String>((package, loaded, instruments))
    })
    .await
    .map_err(|error| format!("工程加载任务异常结束：{error}"))??;
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    workspace.replace_package(package, loaded, &instruments);
    *state
        .instruments
        .lock()
        .map_err(|_| "instrument rack lock poisoned".to_owned())? = instruments;
    Ok(workspace.snapshot())
}

#[tauri::command]
fn save_project(
    state: State<'_, AppState>,
    parent: Option<String>,
    name: Option<String>,
) -> Result<WorkspaceSnapshot, String> {
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    if workspace.package.is_none() {
        let parent = parent
            .map(PathBuf::from)
            .or_else(|| FileDialog::new().set_title("选择工程位置").pick_folder())
            .ok_or_else(|| "已取消保存工程".to_owned())?;
        let name = name.ok_or_else(|| "保存工程需要名称".to_owned())?;
        workspace.save_as(&parent, &name)?;
    } else if parent.is_some() || name.is_some() {
        let parent = parent.ok_or_else(|| "保存为需要同时提供位置和名称".to_owned())?;
        let name = name.ok_or_else(|| "保存为需要同时提供位置和名称".to_owned())?;
        workspace.save_as(Path::new(&parent), &name)?;
    } else {
        workspace.save()?;
    }
    Ok(workspace.snapshot())
}

#[tauri::command]
async fn render_preview(
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<String, String> {
    let (project, package) = {
        let workspace = state
            .workspace
            .lock()
            .map_err(|_| "workspace lock poisoned".to_owned())?;
        (
            workspace.project.project().clone(),
            workspace.package.clone(),
        )
    };
    let output = if let Some(path) = path {
        PathBuf::from(path)
    } else if let Some(package) = &package {
        package
            .artifact_path(
                project_package::ArtifactDirectory::Renders,
                &format!("preview-r{}.wav", project.revision),
            )
            .map_err(|error| error.to_string())?
    } else {
        FileDialog::new()
            .set_title("导出钢琴 WAV")
            .add_filter("WAV 音频", &["wav"])
            .set_file_name("preview.wav")
            .save_file()
            .ok_or_else(|| "已取消导出".to_owned())?
    };
    let fallback_rack = if package.is_none() {
        Some(
            state
                .instruments
                .lock()
                .map_err(|_| "instrument rack lock poisoned".to_owned())?
                .clone(),
        )
    } else {
        None
    };
    tauri::async_runtime::spawn_blocking(move || {
        let instruments = match package.as_ref() {
            Some(package) => rack_for_package(package, &project)?,
            None => fallback_rack.ok_or_else(|| "缺少默认钢琴音源".to_owned())?,
        };
        let buffer = render_project_with_rack(&project, DEFAULT_SAMPLE_RATE, &instruments)
            .map_err(|error| error.to_string())?;
        write_wav(&buffer, &output).map_err(|error| error.to_string())?;
        Ok(output.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| format!("渲染任务异常结束：{error}"))?
}

#[tauri::command]
fn play(state: State<'_, AppState>) -> Result<(), String> {
    let (project, package) = {
        let workspace = state
            .workspace
            .lock()
            .map_err(|_| "workspace lock poisoned".to_owned())?;
        (
            workspace.project.project().clone(),
            workspace.package.clone(),
        )
    };
    let playback_state = state.playback.clone();
    let fallback_rack = if package.is_none() {
        Some(
            state
                .instruments
                .lock()
                .map_err(|_| "instrument rack lock poisoned".to_owned())?
                .clone(),
        )
    } else {
        None
    };
    let generation = {
        let mut playback = playback_state
            .lock()
            .map_err(|_| "playback lock poisoned".to_owned())?;
        if let Some(previous) = playback.handle.take() {
            previous.stop();
        }
        playback.generation = playback.generation.wrapping_add(1);
        playback.rendering = true;
        playback.started_at = None;
        playback.error = None;
        playback.generation
    };

    // Offline synthesis can take longer than a Tauri command round trip.  Keep
    // it away from the UI thread and publish the ready buffer only if this is
    // still the newest play request.
    std::thread::spawn(move || {
        let result = (|| {
            let instruments = match package.as_ref() {
                Some(package) => rack_for_package(package, &project)?,
                None => fallback_rack.ok_or_else(|| "缺少默认钢琴音源".to_owned())?,
            };
            let buffer = render_project_with_rack(&project, DEFAULT_SAMPLE_RATE, &instruments)
                .map_err(|error| error.to_string())?;
            play_buffer(buffer).map_err(|error| error.to_string())
        })();
        match result {
            Ok(handle) => {
                let Ok(mut playback) = playback_state.lock() else {
                    handle.stop();
                    return;
                };
                if playback.generation != generation || !playback.rendering {
                    handle.stop();
                    return;
                }
                playback.rendering = false;
                playback.handle = Some(handle);
                playback.started_at = Some(Instant::now());
                drop(playback);
                monitor_playback(playback_state, generation);
            }
            Err(error) => {
                if let Ok(mut playback) = playback_state.lock()
                    && playback.generation == generation
                {
                    playback.rendering = false;
                    playback.started_at = None;
                    playback.error = Some(error);
                }
            }
        }
    });
    Ok(())
}

fn monitor_playback(playback_state: Arc<Mutex<PlaybackState>>, generation: u64) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let Ok(mut playback) = playback_state.lock() else {
                break;
            };
            if playback.generation != generation {
                break;
            }
            let finished = playback
                .handle
                .as_ref()
                .is_none_or(PlaybackHandle::is_finished);
            if finished {
                playback.handle = None;
                playback.started_at = None;
                break;
            }
        }
    });
}

#[tauri::command]
fn stop(state: State<'_, AppState>) -> Result<(), String> {
    let mut playback = state
        .playback
        .lock()
        .map_err(|_| "playback lock poisoned".to_owned())?;
    playback.generation = playback.generation.wrapping_add(1);
    playback.rendering = false;
    playback.error = None;
    if let Some(handle) = playback.handle.take() {
        handle.stop();
    }
    playback.started_at = None;
    Ok(())
}

#[tauri::command]
fn playback_snapshot(state: State<'_, AppState>) -> Result<PlaybackSnapshot, String> {
    let mut playback = state
        .playback
        .lock()
        .map_err(|_| "playback lock poisoned".to_owned())?;
    let finished = playback
        .handle
        .as_ref()
        .is_some_and(PlaybackHandle::is_finished);
    if finished {
        playback.handle = None;
        playback.started_at = None;
    }
    let playing = playback.handle.is_some();
    let elapsed_seconds = playback
        .started_at
        .map(|started_at| started_at.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    Ok(PlaybackSnapshot {
        playing,
        rendering: playback.rendering,
        elapsed_seconds,
        error: playback.error.clone(),
    })
}

fn proposal_reviewer(instruments: &InstrumentRack) -> ProposalReviewer {
    ProposalReviewer::new(ReviewEnvironment {
        available_instrument_ids: instruments
            .catalog()
            .into_iter()
            .map(|instrument| instrument.id)
            .collect(),
    })
}

const PIANO_ASSET_ROLE: &str = "instrument:piano";
const AUTOPILOT_MEMORY_FILE: &str = "autopilot-session.json";

fn load_package_autopilot_memory(package: &ProjectPackage) -> AutopilotMemory {
    package
        .read_json_artifact(ArtifactDirectory::History, AUTOPILOT_MEMORY_FILE)
        .unwrap_or_default()
}

fn persist_autopilot_result(
    package: &ProjectPackage,
    expected_revision: u64,
    project: &Project,
    memory: &AutopilotMemory,
    outcome: &AutopilotOutcome,
    final_audio: &AudioBuffer,
) -> Result<(), String> {
    let memory_bytes = serde_json::to_vec_pretty(memory).map_err(|error| error.to_string())?;
    let outcome_bytes = serde_json::to_vec_pretty(outcome).map_err(|error| error.to_string())?;
    let audio_bytes = wav_bytes(final_audio).map_err(|error| error.to_string())?;
    let outcome_filename = format!("revision-{}-autopilot.json", outcome.final_revision);
    let render_filename = format!("autopilot-r{}.wav", outcome.final_revision);
    package
        .save_with_artifacts_if_revision(
            expected_revision,
            project,
            &[
                ArtifactWrite {
                    directory: ArtifactDirectory::History,
                    filename: AUTOPILOT_MEMORY_FILE,
                    bytes: &memory_bytes,
                },
                ArtifactWrite {
                    directory: ArtifactDirectory::History,
                    filename: &outcome_filename,
                    bytes: &outcome_bytes,
                },
                ArtifactWrite {
                    directory: ArtifactDirectory::Renders,
                    filename: &render_filename,
                    bytes: &audio_bytes,
                },
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
async fn run_autopilot(
    state: State<'_, AppState>,
    instruction: String,
) -> Result<AutopilotOutcome, String> {
    let (mut engine, mut memory, package_root) = {
        let workspace = state
            .workspace
            .lock()
            .map_err(|_| "workspace lock poisoned".to_owned())?;
        (
            workspace.project.clone(),
            workspace.autopilot_memory.clone(),
            workspace
                .package
                .as_ref()
                .map(|package| package.root().to_owned()),
        )
    };
    let rack = state
        .instruments
        .lock()
        .map_err(|_| "instrument rack lock poisoned".to_owned())?
        .clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let executable = std::env::var("AI_MUSIC_CODEX_BIN").unwrap_or_else(|_| "codex".to_owned());
        let mut autopilot = Autopilot::new(CodexCliModel::new(executable));
        let run = autopilot
            .run_instruction(&mut engine, &rack, &mut memory, &instruction)
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((engine, memory, run.outcome, run.final_audio))
    })
    .await
    .map_err(|error| format!("AI 自动创作任务异常结束：{error}"))??;
    let (engine, memory, outcome, final_audio) = outcome;
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let current_package_root = workspace
        .package
        .as_ref()
        .map(|package| package.root().to_owned());
    if workspace.project.revision() != outcome.starting_revision
        || current_package_root != package_root
    {
        return Err("AI 创作期间工程已被修改或切换，自动结果未提交".to_owned());
    }
    if let Some(package) = workspace.package.as_ref() {
        persist_autopilot_result(
            package,
            outcome.starting_revision,
            engine.project(),
            &memory,
            &outcome,
            &final_audio,
        )?;
    }
    workspace.project = engine;
    workspace.autopilot_memory = memory;
    let instruments = state
        .instruments
        .lock()
        .map_err(|_| "instrument rack lock poisoned".to_owned())?;
    workspace.composition_sessions = CompositionSessions::new(proposal_reviewer(&instruments));
    if workspace.package.is_some() {
        workspace.mark_saved();
    }
    Ok(outcome)
}

fn rack_for_package(package: &ProjectPackage, project: &Project) -> Result<InstrumentRack, String> {
    let Some(manifest_path) = package
        .resolve_source_asset(PIANO_ASSET_ROLE)
        .map_err(|error| error.to_string())?
    else {
        return Ok(InstrumentRack::default());
    };
    let pack = AssetPack::load(&manifest_path).map_err(|error| {
        format!(
            "工程钢琴音色资源不可用（{}）：{error}",
            manifest_path.display()
        )
    })?;
    InstrumentRack::from_asset_pack_for_project(&pack, project).map_err(|error| {
        format!(
            "无法加载工程钢琴音色（{}）：{error}",
            manifest_path.display()
        )
    })
}

#[tauri::command]
async fn choose_piano_asset(state: State<'_, AppState>) -> Result<WorkspaceSnapshot, String> {
    let manifest_path = FileDialog::new()
        .set_title("选择钢琴音色资源包")
        .add_filter("AI Music 音色资源", &["json"])
        .pick_file()
        .ok_or_else(|| "已取消选择音色".to_owned())?;
    let manifest_path = manifest_path
        .canonicalize()
        .map_err(|error| format!("无法读取音色资源路径：{error}"))?;
    let persisted_manifest_path = manifest_path.clone();
    let (project, package_root) = {
        let workspace = state
            .workspace
            .lock()
            .map_err(|_| "workspace lock poisoned".to_owned())?;
        let package = workspace
            .package
            .as_ref()
            .ok_or_else(|| "请先创建或保存工程，再绑定钢琴音色".to_owned())?;
        (
            workspace.project.project().clone(),
            package.root().to_owned(),
        )
    };
    let (pack, rack) = tauri::async_runtime::spawn_blocking(move || {
        let pack = AssetPack::load(&manifest_path).map_err(|error| error.to_string())?;
        if pack.manifest().instrument_id != "piano" {
            return Err(format!(
                "音色资源 instrument_id 必须是 'piano'，当前是 '{}'",
                pack.manifest().instrument_id
            ));
        }
        let rack = InstrumentRack::from_asset_pack_for_project(&pack, &project)
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((pack, rack))
    })
    .await
    .map_err(|error| format!("音色加载任务异常结束：{error}"))??;
    let reference = SourceAssetReference {
        asset_id: pack.manifest().id.clone(),
        name: pack.manifest().name.clone(),
        location: SourceAssetLocation::External {
            manifest_path: persisted_manifest_path.to_string_lossy().into_owned(),
        },
        license_source: pack.manifest().license.source.clone(),
        attribution: pack.manifest().license.attribution.clone(),
    };
    let mut workspace = state
        .workspace
        .lock()
        .map_err(|_| "workspace lock poisoned".to_owned())?;
    let package = workspace
        .package
        .as_mut()
        .ok_or_else(|| "请先创建或保存工程，再绑定钢琴音色".to_owned())?;
    if package.root() != package_root {
        return Err("音色加载期间工程已切换，请重新选择".to_owned());
    }
    package
        .set_source_asset(PIANO_ASSET_ROLE, reference)
        .map_err(|error| error.to_string())?;
    workspace.composition_sessions = CompositionSessions::new(proposal_reviewer(&rack));
    *state
        .instruments
        .lock()
        .map_err(|_| "instrument rack lock poisoned".to_owned())? = rack;
    Ok(workspace.snapshot())
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            project_snapshot,
            workspace_snapshot,
            piano_asset_snapshot,
            project_revision,
            project_summary,
            clip_window,
            instrument_catalog,
            apply_command,
            apply_patch,
            preview_patch,
            run_autopilot,
            authorize_composition_task,
            review_authorized_proposal,
            apply_authorized_proposal,
            revoke_composition_task,
            undo,
            redo,
            new_project,
            choose_project_location,
            load_project,
            save_project,
            choose_piano_asset,
            render_preview,
            play,
            stop,
            playback_snapshot
        ])
        .run(tauri::generate_context!())
        .expect("error while running AI Music");
}

#[cfg(test)]
mod tests {
    use super::*;
    use composition_engine::{
        COMPOSITION_SCHEMA_VERSION, CompositionPlan, CoverageEvidence, CreativeBrief,
        CreativeDecision, CreativeObjective, CritiqueDecision, CritiqueDisposition,
        CritiqueLocation, CritiqueObservation, CritiqueResponse, EditCapability, EditScope,
        ObjectiveCoverage, ObjectivePriority, PlannedSection, PlannedTrackRole, ReviewStatus,
        RhythmConstraints, ScopedTickRange, TaskAuthorizationStatus, TickRange, TrackAccess,
    };
    use music_core::{NoteEvent, Patch};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn task() -> CompositionTask {
        CompositionTask {
            brief: CreativeBrief {
                schema_version: COMPOSITION_SCHEMA_VERSION,
                id: "desktop-session".to_owned(),
                summary: "Add one piano gesture".to_owned(),
                target: TickRange {
                    start_tick: 0,
                    end_tick: 960,
                },
                objectives: vec![CreativeObjective {
                    id: "gesture".to_owned(),
                    description: "Introduce an audible gesture".to_owned(),
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
                max_operations: 4,
            },
        }
    }

    fn proposal() -> CompositionProposal {
        CompositionProposal {
            brief_id: "desktop-session".to_owned(),
            based_on_critique_id: None,
            critique_responses: Vec::new(),
            plan: CompositionPlan {
                summary: "Use one C4 attack".to_owned(),
                sections: vec![PlannedSection {
                    id: "gesture".to_owned(),
                    range: TickRange {
                        start_tick: 0,
                        end_tick: 480,
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
                        description: "The C4 attack is the audible gesture".to_owned(),
                        section_id: Some("gesture".to_owned()),
                        track_id: Some("piano".to_owned()),
                        range: Some(TickRange {
                            start_tick: 0,
                            end_tick: 480,
                        }),
                    }],
                }],
                decisions: vec![CreativeDecision {
                    decision: "Use a single note".to_owned(),
                    rationale: "A sparse attack clearly states the gesture".to_owned(),
                }],
            },
            patch: Patch {
                base_revision: Some(0),
                description: Some("add desktop session gesture".to_owned()),
                operations: vec![Command::AddNote {
                    track_id: "piano".to_owned(),
                    clip_id: "piano-main".to_owned(),
                    note: NoteEvent {
                        id: "desktop-c4".to_owned(),
                        start_tick: 0,
                        duration_tick: 480,
                        pitch: 60,
                        velocity: 88,
                    },
                }],
            },
        }
    }

    fn critique() -> CritiqueReport {
        CritiqueReport {
            brief_id: "desktop-session".to_owned(),
            base_revision: 0,
            summary: "The gesture needs a clearer attack".to_owned(),
            observations: vec![CritiqueObservation {
                id: "attack".to_owned(),
                location: CritiqueLocation {
                    label: Some("opening".to_owned()),
                    track_id: Some("piano".to_owned()),
                    range: Some(TickRange {
                        start_tick: 0,
                        end_tick: 480,
                    }),
                },
                observation: "The opening depends on one attack".to_owned(),
                consequence: "The attack must carry the phrase identity".to_owned(),
                proposed_revision: Some("Keep a distinct accented onset".to_owned()),
            }],
            decisions: vec![CritiqueDecision {
                observation_id: "attack".to_owned(),
                disposition: CritiqueDisposition::Modify,
                rationale: "The evaluator selects a clearer attack for the stated opening brief"
                    .to_owned(),
            }],
            next_focus: Some("opening attack".to_owned()),
        }
    }

    #[test]
    fn workspace_resolves_private_authority_and_consumes_it_on_commit() {
        let instruments = InstrumentRack::default();
        let mut workspace = Workspace::new(&instruments);
        let authorization = workspace.authorize_task(task());
        assert_eq!(authorization.status, TaskAuthorizationStatus::Authorized);
        let task_id = authorization.authorized_task.unwrap().task_id;

        assert_eq!(
            workspace
                .review_proposal(&task_id, &proposal())
                .unwrap()
                .status,
            ReviewStatus::Ready
        );
        let application = workspace.apply_proposal(&task_id, &proposal()).unwrap();
        assert_eq!(application.review.status, ReviewStatus::Ready);
        assert_eq!(workspace.project.revision(), 1);
        assert!(workspace.review_proposal(&task_id, &proposal()).is_err());
    }

    #[test]
    fn replacing_project_clears_authorized_task_ids() {
        let instruments = InstrumentRack::default();
        let mut workspace = Workspace::new(&instruments);
        let task_id = workspace
            .authorize_task(task())
            .authorized_task
            .unwrap()
            .task_id;

        workspace.replace_project(Project::default(), &instruments);

        assert!(workspace.review_proposal(&task_id, &proposal()).is_err());
    }

    #[test]
    fn workspace_records_and_resolves_a_linked_critique() {
        let instruments = InstrumentRack::default();
        let mut workspace = Workspace::new(&instruments);
        let task_id = workspace
            .authorize_task(task())
            .authorized_task
            .unwrap()
            .task_id;
        let stored = workspace.record_critique(&task_id, critique()).unwrap();
        let mut linked = proposal();
        linked.based_on_critique_id = Some(stored.id);
        linked.critique_responses = vec![CritiqueResponse {
            observation_id: "attack".to_owned(),
            rationale: "Implement the evaluator's clearer-attack decision while retaining the sparse identity".to_owned(),
        }];

        assert_eq!(
            workspace.review_proposal(&task_id, &linked).unwrap().status,
            ReviewStatus::Ready
        );
    }

    #[test]
    fn desktop_autopilot_persistence_writes_project_memory_outcome_and_render() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "ai-music-desktop-autopilot-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&parent).unwrap();
        let project = Project {
            revision: 1,
            ..Project::default()
        };
        let package = ProjectPackage::create(&parent, "Persistence", &Project::default()).unwrap();
        let memory = AutopilotMemory::default();
        let outcome = AutopilotOutcome {
            session_id: memory.session_id.clone(),
            instruction: "测试自动落盘".to_owned(),
            brief: task().brief,
            starting_revision: 0,
            final_revision: 1,
            committed_revisions: vec![1],
            proposal_attempts: 1,
            evaluator_rounds: 1,
            status: autopilot_engine::AutopilotStatus::Completed,
            evaluator_summary: "accepted".to_owned(),
            render: autopilot_engine::analyze_render(&AudioBuffer {
                sample_rate: 8_000,
                channels: 2,
                samples: vec![0.25, -0.25, 0.1, -0.1],
            }),
        };
        let audio = AudioBuffer {
            sample_rate: 8_000,
            channels: 2,
            samples: vec![0.25, -0.25, 0.1, -0.1],
        };

        persist_autopilot_result(&package, 0, &project, &memory, &outcome, &audio).unwrap();

        let (_, saved) = ProjectPackage::open(package.root()).unwrap();
        assert_eq!(saved.revision, 1);
        assert!(
            package
                .artifact_path(ArtifactDirectory::Renders, "autopilot-r1.wav")
                .unwrap()
                .is_file()
        );
        let saved_memory: AutopilotMemory = package
            .read_json_artifact(ArtifactDirectory::History, AUTOPILOT_MEMORY_FILE)
            .unwrap();
        assert_eq!(saved_memory.session_id, memory.session_id);
        let saved_outcome: AutopilotOutcome = package
            .read_json_artifact(ArtifactDirectory::History, "revision-1-autopilot.json")
            .unwrap();
        assert_eq!(saved_outcome.final_revision, 1);

        let _ = fs::remove_dir_all(parent);
    }
}
