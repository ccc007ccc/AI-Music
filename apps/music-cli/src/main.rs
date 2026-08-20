use anyhow::{Context, Result};
use audio_engine::{
    AssetPack, DEFAULT_SAMPLE_RATE, InstrumentRack, play_buffer, render_project,
    render_project_with_rack, render_wav, wav_bytes, write_wav,
};
use autopilot_engine::{
    Autopilot, AutopilotConfig, AutopilotMemory, AutopilotOutcome, CodexCliModel,
};
use clap::{Parser, Subcommand, ValueEnum};
use composition_engine::{
    ArrangementAnalyzer, AuthorizedCompositionTask, CompositionProposal, CompositionTask,
    CritiqueReport, ProposalReview, ProposalReviewer, ReviewEnvironment, StoredCritique,
    TaskAuthorization,
};
use midi_io::{export_project, import_midi};
use music_core::{Command, NoteEvent, Patch, Project, ProjectEngine, new_id};
use project_package::{
    ArtifactDirectory, ArtifactWrite, ProjectPackage, SourceAssetLocation, SourceAssetReference,
};
use serde::de::DeserializeOwned;
use serde_json::from_str;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

mod session;
use session::SessionRole;

#[derive(Debug, Parser)]
#[command(
    name = "musicctl",
    version,
    about = "A small MIDI-first music workspace"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create an empty project with one piano track.
    New { path: PathBuf },
    /// Create an empty directory-based `.aimusic` project package.
    NewProject { parent: PathBuf, name: String },
    /// Create a small audible demo project.
    Demo {
        #[arg(default_value = "demo.json")]
        path: PathBuf,
    },
    /// Create a small audible demo project package.
    DemoProject { parent: PathBuf, name: String },
    /// Print a project summary or its JSON representation.
    Inspect {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Print the compact context intended for AI planning.
    Context { path: PathBuf },
    /// Print neutral, location-aware observations about arrangement structure.
    AnalyzeArrangement { path: PathBuf },
    /// Print MIDI events from one clip in an absolute tick window.
    Events {
        path: PathBuf,
        #[arg(long, default_value = "piano")]
        track: String,
        #[arg(long, default_value = "piano-main")]
        clip: String,
        #[arg(long, default_value_t = 0)]
        from: i64,
        #[arg(long)]
        to: Option<i64>,
    },
    /// Add one note to a MIDI clip.
    AddNote {
        path: PathBuf,
        #[arg(long, default_value = "piano")]
        track: String,
        #[arg(long, default_value = "piano-main")]
        clip: String,
        #[arg(long)]
        pitch: u8,
        #[arg(long)]
        start: i64,
        #[arg(long)]
        duration: i64,
        #[arg(long, default_value_t = 90)]
        velocity: u8,
    },
    /// Render a project to a WAV file.
    Render {
        path: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = DEFAULT_SAMPLE_RATE)]
        sample_rate: u32,
        /// Optional SoundFont path. Requires --features rustysynth-backend.
        #[arg(long)]
        soundfont: Option<PathBuf>,
        /// Optional licensed SF2 or SFZ instrument asset-pack manifest.
        #[arg(long, visible_alias = "soundfont-pack", conflicts_with = "soundfont")]
        instrument_pack: Option<PathBuf>,
    },
    /// Import a standard MIDI file into the project format.
    ImportMidi { input: PathBuf, output: PathBuf },
    /// Export the project as a standard MIDI file.
    ExportMidi {
        input: PathBuf,
        output: Option<PathBuf>,
    },
    /// Bind a licensed SF2/SFZ pack to a directory project for future renders.
    BindInstrumentPack { project: PathBuf, manifest: PathBuf },
    /// Apply an AI-generated JSON patch as one transaction.
    ApplyPatch { project: PathBuf, patch: PathBuf },
    /// Validate an AI-generated patch without changing the project.
    CheckPatch { project: PathBuf, patch: PathBuf },
    /// Review an AI composition proposal without changing the project.
    ReviewProposal {
        project: PathBuf,
        task: PathBuf,
        proposal: PathBuf,
    },
    /// Review and atomically apply an authorized AI composition proposal.
    ApplyProposal {
        project: PathBuf,
        task: PathBuf,
        proposal: PathBuf,
    },
    /// Run a provider-neutral JSONL host session for an AI composer.
    Session {
        project: PathBuf,
        /// Optional host-issued task file to authorize before reading stdin.
        #[arg(long)]
        task: Option<PathBuf>,
        /// Session side: evaluator authors listening decisions; composer implements them.
        #[arg(long, value_enum, default_value_t = SessionRole::Composer)]
        role: SessionRole,
        /// Host-attached evaluator reports for a composer session. May be repeated.
        #[arg(long = "critique", value_name = "PATH")]
        critiques: Vec<PathBuf>,
        /// Permit `authorize` requests on stdin. Prefer --task for a model-facing process.
        #[arg(long)]
        allow_authorize: bool,
    },
    /// Fully automatically create or revise music from one natural-language instruction.
    Autopilot {
        project: PathBuf,
        /// What the user wants created or changed. No task/proposal JSON is required.
        instruction: String,
        /// Optional Codex model override; the authenticated CLI default is used otherwise.
        #[arg(long)]
        model: Option<String>,
        /// Automatic evaluator-directed revisions after the first committed draft.
        #[arg(long, default_value_t = 2)]
        max_revisions: usize,
        /// Optional final WAV path. Package projects default to their renders directory.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Print the JSON Schema used by AI and other clients.
    Schema { document: SchemaDocument },
    /// Play a project through the default audio device.
    Play {
        path: PathBuf,
        #[arg(long, default_value_t = DEFAULT_SAMPLE_RATE)]
        sample_rate: u32,
        /// Optional SoundFont path. Requires --features rustysynth-backend.
        #[arg(long)]
        soundfont: Option<PathBuf>,
        /// Optional licensed SF2 or SFZ instrument asset-pack manifest.
        #[arg(long, visible_alias = "soundfont-pack", conflicts_with = "soundfont")]
        instrument_pack: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum SchemaDocument {
    ClipWindow,
    ArrangementReport,
    Project,
    ProjectSummary,
    Patch,
    CompositionTask,
    CompositionProposal,
    CritiqueReport,
    StoredCritique,
    ProposalReview,
    AuthorizedCompositionTask,
    TaskAuthorization,
    SessionRequest,
    SessionResponse,
    AutopilotMemory,
    AutopilotOutcome,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::New { path } => {
            if is_package_path(&path) {
                ProjectPackage::create_at(
                    &path,
                    package_name_from_path(&path),
                    &Project::default(),
                )?;
            } else {
                Project::default().save(&path)?;
            }
            println!("created {}", path.display());
        }
        Commands::NewProject { parent, name } => {
            let package = ProjectPackage::create(&parent, &name, &Project::default())
                .with_context(|| {
                    format!(
                        "could not create project package below {}",
                        parent.display()
                    )
                })?;
            println!("created {}", package.root().display());
        }
        Commands::Demo { path } => {
            if is_package_path(&path) {
                ProjectPackage::create_at(&path, package_name_from_path(&path), &Project::demo())?;
            } else {
                Project::demo().save(&path)?;
            }
            println!("created demo {}", path.display());
        }
        Commands::DemoProject { parent, name } => {
            let package =
                ProjectPackage::create(&parent, &name, &Project::demo()).with_context(|| {
                    format!(
                        "could not create project package below {}",
                        parent.display()
                    )
                })?;
            println!("created demo package {}", package.root().display());
        }
        Commands::Inspect { path, json } => {
            let project = load(&path)?;
            if json {
                println!("{}", project.to_pretty_json()?);
            } else {
                println!("tracks: {}", project.tracks.len());
                println!("revision: {}", project.revision);
                println!("tempo: {:.2} BPM", project.tempo_map.points[0].bpm);
                println!("length: {} ticks", project.duration_tick());
                println!("notes: {}", project.scheduled_notes().len());
            }
        }
        Commands::Context { path } => {
            let project = load(&path)?;
            println!("{}", serde_json::to_string_pretty(&project.summary())?);
        }
        Commands::AnalyzeArrangement { path } => {
            let project = load(&path)?;
            let report = ArrangementAnalyzer.analyze(&project)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Commands::Events {
            path,
            track,
            clip,
            from,
            to,
        } => {
            let project = load(&path)?;
            let to = to.unwrap_or_else(|| project.duration_tick().max(from.saturating_add(1)));
            let window = project.clip_window(&track, &clip, from, to)?;
            println!("{}", serde_json::to_string_pretty(&window)?);
        }
        Commands::AddNote {
            path,
            track,
            clip,
            pitch,
            start,
            duration,
            velocity,
        } => {
            let (project, package) = load_editable(&path)?;
            let expected_revision = project.revision;
            let mut engine = ProjectEngine::new(project);
            engine.apply(Command::AddNote {
                track_id: track,
                clip_id: clip,
                note: NoteEvent {
                    id: new_id("note"),
                    start_tick: start,
                    duration_tick: duration,
                    pitch,
                    velocity,
                },
            })?;
            save_editable(
                &path,
                engine.project(),
                package.as_ref(),
                Some(expected_revision),
            )?;
            println!("updated {}", path.display());
        }
        Commands::Render {
            path,
            output,
            sample_rate,
            soundfont,
            instrument_pack,
        } => {
            let project = load(&path)?;
            let output = render_output_path(&path, output, &project)?;
            if let Some(soundfont) = soundfont {
                let rack = soundfont_rack(&soundfont)?;
                let buffer = render_project_with_rack(&project, sample_rate, &rack)?;
                write_wav(&buffer, &output)?;
            } else if let Some(instrument_pack) = instrument_pack {
                let rack = instrument_pack_rack(&instrument_pack, &project)?;
                let buffer = render_project_with_rack(&project, sample_rate, &rack)?;
                write_wav(&buffer, &output)?;
            } else if let Some(rack) = package_instrument_rack(&path, &project)? {
                let buffer = render_project_with_rack(&project, sample_rate, &rack)?;
                write_wav(&buffer, &output)?;
            } else {
                render_wav(&project, sample_rate, &output)?;
            }
            println!("rendered {}", output.display());
        }
        Commands::ImportMidi { input, output } => {
            let project = import_midi(&input)?;
            if output
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("aimusic"))
            {
                let name = output
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Imported")
                    .to_owned();
                ProjectPackage::create_at(&output, name, &project)?;
            } else {
                project.save(&output)?;
            }
            println!("imported {} -> {}", input.display(), output.display());
        }
        Commands::ExportMidi { input, output } => {
            let project = load(&input)?;
            let output = export_output_path(&input, output, &project)?;
            export_project(&project, &output)?;
            println!("exported {} -> {}", input.display(), output.display());
        }
        Commands::BindInstrumentPack { project, manifest } => {
            let (mut package, _) = ProjectPackage::open(&project)
                .with_context(|| format!("could not load project package {}", project.display()))?;
            let manifest = manifest
                .canonicalize()
                .with_context(|| format!("could not resolve asset pack {}", manifest.display()))?;
            let pack = AssetPack::load(&manifest)
                .with_context(|| format!("could not load asset pack {}", manifest.display()))?;
            if pack.manifest().instrument_id != "piano" {
                anyhow::bail!(
                    "bind-instrument-pack currently accepts only instrument_id=piano (found {})",
                    pack.manifest().instrument_id
                );
            }
            let role = instrument_asset_role(&pack.manifest().instrument_id);
            package.set_source_asset(
                role,
                SourceAssetReference {
                    asset_id: pack.manifest().id.clone(),
                    name: pack.manifest().name.clone(),
                    location: SourceAssetLocation::External {
                        manifest_path: manifest.to_string_lossy().into_owned(),
                    },
                    license_source: pack.manifest().license.source.clone(),
                    attribution: pack.manifest().license.attribution.clone(),
                },
            )?;
            println!("bound {} to {}", pack.manifest().name, project.display());
        }
        Commands::ApplyPatch { project, patch } => {
            let (current, package) = load_editable(&project)?;
            let expected_revision = current.revision;
            let patch_text = if patch.as_os_str() == "-" {
                let mut value = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut value)?;
                value
            } else {
                std::fs::read_to_string(&patch)
                    .with_context(|| format!("could not read patch {}", patch.display()))?
            };
            let patch: Patch = from_str(&patch_text)
                .with_context(|| format!("invalid patch JSON in {}", patch.display()))?;
            let history_patch = patch.clone();
            let mut engine = ProjectEngine::new(current);
            let change = engine.apply_patch(patch)?;
            save_editable(
                &project,
                engine.project(),
                package.as_ref(),
                Some(expected_revision),
            )?;
            if let Some(package) = package.as_ref()
                && let Err(error) = package.write_json_artifact(
                    ArtifactDirectory::History,
                    &format!("revision-{}-patch.json", change.revision),
                    &history_patch,
                )
            {
                eprintln!("warning: could not write patch history: {error}");
            }
            println!(
                "applied revision {} to {}",
                change.revision,
                project.display()
            );
        }
        Commands::CheckPatch { project, patch } => {
            let current = load(&project)?;
            let patch: Patch = read_json(&patch, "patch")?;
            let engine = ProjectEngine::new(current);
            let preview = engine.preview_patch(&patch)?;
            println!("{}", serde_json::to_string_pretty(&preview)?);
        }
        Commands::ReviewProposal {
            project,
            task,
            proposal,
        } => {
            ensure_distinct_stdin_sources(&task, &proposal)?;
            let current = load(&project)?;
            let task: CompositionTask = read_json(&task, "composition task")?;
            let proposal: CompositionProposal = read_json(&proposal, "composition proposal")?;
            let review = default_proposal_reviewer().review(&current, &task, &proposal);
            println!("{}", serde_json::to_string_pretty(&review)?);
        }
        Commands::ApplyProposal {
            project,
            task,
            proposal,
        } => {
            ensure_distinct_stdin_sources(&task, &proposal)?;
            let (current, package) = load_editable(&project)?;
            let expected_revision = current.revision;
            let task: CompositionTask = read_json(&task, "composition task")?;
            let proposal: CompositionProposal = read_json(&proposal, "composition proposal")?;
            let mut engine = ProjectEngine::new(current);
            let application = default_proposal_reviewer().apply(&mut engine, &task, &proposal);
            if application.change.is_some() {
                save_editable(
                    &project,
                    engine.project(),
                    package.as_ref(),
                    Some(expected_revision),
                )?;
                if let (Some(package), Some(change)) =
                    (package.as_ref(), application.change.as_ref())
                {
                    let revision = change.revision;
                    if let Err(error) = package.write_json_artifact(
                        ArtifactDirectory::History,
                        &format!("revision-{revision}-task.json"),
                        &task,
                    ) {
                        eprintln!("warning: could not write task history: {error}");
                    }
                    if let Err(error) = package.write_json_artifact(
                        ArtifactDirectory::History,
                        &format!("revision-{revision}-proposal.json"),
                        &proposal,
                    ) {
                        eprintln!("warning: could not write proposal history: {error}");
                    }
                    if let Err(error) = package.write_json_artifact(
                        ArtifactDirectory::History,
                        &format!("revision-{revision}-application.json"),
                        &application,
                    ) {
                        eprintln!("warning: could not write application history: {error}");
                    }
                }
            }
            println!("{}", serde_json::to_string_pretty(&application)?);
            if application.change.is_none() {
                anyhow::bail!("proposal was not applied; inspect the review violations")
            }
        }
        Commands::Session {
            project,
            task,
            role,
            critiques,
            allow_authorize,
        } => session::run(
            project,
            task,
            role,
            critiques,
            allow_authorize,
            default_proposal_reviewer(),
        )?,
        Commands::Autopilot {
            project,
            instruction,
            model,
            max_revisions,
            output,
        } => {
            let (loaded, package) = load_editable(&project)?;
            let rack = package_instrument_rack(&project, &loaded)?.unwrap_or_default();
            let mut engine = ProjectEngine::new(loaded);
            let mut memory = load_autopilot_memory(&project, package.as_ref())?;
            let executable =
                std::env::var("AI_MUSIC_CODEX_BIN").unwrap_or_else(|_| "codex".to_owned());
            let mut backend = CodexCliModel::new(executable);
            if let Some(model) = model {
                backend = backend.with_model(model);
            }
            let config = AutopilotConfig {
                max_revision_rounds: max_revisions,
                ..AutopilotConfig::default()
            };
            let mut autopilot = Autopilot::with_config(backend, config);
            let run = autopilot.run_instruction(&mut engine, &rack, &mut memory, &instruction)?;
            let package_default_render = package.is_some() && output.is_none();
            let render_path =
                autopilot_render_output_path(&project, output, engine.project(), package.as_ref())?;
            if let Some(package) = package.as_ref() {
                let memory_bytes = serde_json::to_vec_pretty(&memory)?;
                let outcome_bytes = serde_json::to_vec_pretty(&run.outcome)?;
                let outcome_filename =
                    format!("revision-{}-autopilot.json", run.outcome.final_revision);
                if package_default_render {
                    let audio_bytes = wav_bytes(&run.final_audio)?;
                    let render_filename = format!("autopilot-r{}.wav", run.outcome.final_revision);
                    package.save_with_artifacts_if_revision(
                        run.outcome.starting_revision,
                        engine.project(),
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
                    )?;
                } else {
                    write_wav(&run.final_audio, &render_path)?;
                    package.save_with_artifacts_if_revision(
                        run.outcome.starting_revision,
                        engine.project(),
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
                        ],
                    )?;
                }
            } else {
                save_editable(&project, engine.project(), None, None)?;
                save_autopilot_memory(&project, None, &memory)?;
                write_wav(&run.final_audio, &render_path)?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "outcome": run.outcome,
                    "render_path": render_path
                }))?
            );
        }
        Commands::Schema { document } => {
            let schema = match document {
                SchemaDocument::ClipWindow => schemars::schema_for!(music_core::ClipWindow),
                SchemaDocument::ArrangementReport => {
                    schemars::schema_for!(composition_engine::ArrangementReport)
                }
                SchemaDocument::Project => schemars::schema_for!(Project),
                SchemaDocument::ProjectSummary => schemars::schema_for!(music_core::ProjectSummary),
                SchemaDocument::Patch => schemars::schema_for!(Patch),
                SchemaDocument::CompositionTask => schemars::schema_for!(CompositionTask),
                SchemaDocument::CompositionProposal => {
                    schemars::schema_for!(CompositionProposal)
                }
                SchemaDocument::CritiqueReport => schemars::schema_for!(CritiqueReport),
                SchemaDocument::StoredCritique => schemars::schema_for!(StoredCritique),
                SchemaDocument::ProposalReview => schemars::schema_for!(ProposalReview),
                SchemaDocument::AuthorizedCompositionTask => {
                    schemars::schema_for!(AuthorizedCompositionTask)
                }
                SchemaDocument::TaskAuthorization => schemars::schema_for!(TaskAuthorization),
                SchemaDocument::SessionRequest => schemars::schema_for!(session::SessionRequest),
                SchemaDocument::SessionResponse => schemars::schema_for!(session::SessionResponse),
                SchemaDocument::AutopilotMemory => schemars::schema_for!(AutopilotMemory),
                SchemaDocument::AutopilotOutcome => schemars::schema_for!(AutopilotOutcome),
            };
            println!("{}", serde_json::to_string_pretty(&schema)?);
        }
        Commands::Play {
            path,
            sample_rate,
            soundfont,
            instrument_pack,
        } => {
            let project = load(&path)?;
            let buffer = if let Some(soundfont) = soundfont {
                let rack = soundfont_rack(&soundfont)?;
                render_project_with_rack(&project, sample_rate, &rack)?
            } else if let Some(instrument_pack) = instrument_pack {
                let rack = instrument_pack_rack(&instrument_pack, &project)?;
                render_project_with_rack(&project, sample_rate, &rack)?
            } else if let Some(rack) = package_instrument_rack(&path, &project)? {
                render_project_with_rack(&project, sample_rate, &rack)?
            } else {
                render_project(&project, sample_rate)?
            };
            let handle = play_buffer(buffer)?;
            while !handle.is_finished() {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(())
}

fn load(path: &Path) -> Result<Project> {
    if path.is_dir() {
        let (_, project) = ProjectPackage::open(path)
            .with_context(|| format!("could not load project package {}", path.display()))?;
        Ok(project)
    } else {
        Project::load(path).with_context(|| format!("could not load {}", path.display()))
    }
}

fn load_editable(path: &Path) -> Result<(Project, Option<ProjectPackage>)> {
    if path.is_dir() {
        let (package, project) = ProjectPackage::open(path)
            .with_context(|| format!("could not load project package {}", path.display()))?;
        Ok((project, Some(package)))
    } else {
        Ok((load(path)?, None))
    }
}

fn save_editable(
    path: &Path,
    project: &Project,
    package: Option<&ProjectPackage>,
    expected_revision: Option<u64>,
) -> Result<()> {
    if let Some(package) = package {
        if let Some(expected_revision) = expected_revision {
            package
                .save_if_revision(expected_revision, project)
                .with_context(|| format!("could not save project package {}", path.display()))?;
        } else {
            package
                .save(project)
                .with_context(|| format!("could not save project package {}", path.display()))?;
        }
    } else {
        project
            .save(path)
            .with_context(|| format!("could not save {}", path.display()))?;
    }
    Ok(())
}

const AUTOPILOT_MEMORY_FILE: &str = "autopilot-session.json";

fn load_autopilot_memory(
    project_path: &Path,
    package: Option<&ProjectPackage>,
) -> Result<AutopilotMemory> {
    let path = if let Some(package) = package {
        package.artifact_path(ArtifactDirectory::History, AUTOPILOT_MEMORY_FILE)?
    } else {
        autopilot_sidecar_path(project_path)
    };
    if !path.exists() {
        return Ok(AutopilotMemory::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read autopilot memory {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid autopilot memory {}", path.display()))
}

fn save_autopilot_memory(
    project_path: &Path,
    package: Option<&ProjectPackage>,
    memory: &AutopilotMemory,
) -> Result<()> {
    if let Some(package) = package {
        package.write_json_artifact(ArtifactDirectory::History, AUTOPILOT_MEMORY_FILE, memory)?;
        return Ok(());
    }
    let path = autopilot_sidecar_path(project_path);
    let bytes = serde_json::to_vec_pretty(memory)?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("could not save autopilot memory {}", path.display()))
}

fn autopilot_sidecar_path(project_path: &Path) -> PathBuf {
    let mut path = project_path.as_os_str().to_os_string();
    path.push(".autopilot.json");
    PathBuf::from(path)
}

fn autopilot_render_output_path(
    project_path: &Path,
    requested: Option<PathBuf>,
    project: &Project,
    package: Option<&ProjectPackage>,
) -> Result<PathBuf> {
    if let Some(path) = requested {
        return Ok(path);
    }
    if let Some(package) = package {
        return Ok(package.artifact_path(
            ArtifactDirectory::Renders,
            &format!("autopilot-r{}.wav", project.revision),
        )?);
    }
    let mut path = project_path.to_path_buf();
    path.set_extension("autopilot.wav");
    Ok(path)
}

fn is_package_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("aimusic"))
}

fn package_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_owned()
}

fn render_output_path(
    project_path: &Path,
    requested: Option<PathBuf>,
    project: &Project,
) -> Result<PathBuf> {
    if let Some(path) = requested {
        return Ok(path);
    }
    if project_path.is_dir() {
        let (package, _) = ProjectPackage::open(project_path).with_context(|| {
            format!("could not load project package {}", project_path.display())
        })?;
        Ok(package.artifact_path(
            project_package::ArtifactDirectory::Renders,
            &format!("preview-r{}.wav", project.revision),
        )?)
    } else {
        Ok(PathBuf::from("render.wav"))
    }
}

fn export_output_path(
    project_path: &Path,
    requested: Option<PathBuf>,
    project: &Project,
) -> Result<PathBuf> {
    if let Some(path) = requested {
        return Ok(path);
    }
    if project_path.is_dir() {
        let (package, _) = ProjectPackage::open(project_path).with_context(|| {
            format!("could not load project package {}", project_path.display())
        })?;
        Ok(package.artifact_path(
            project_package::ArtifactDirectory::Exports,
            &format!("revision-{}.mid", project.revision),
        )?)
    } else {
        Ok(PathBuf::from("export.mid"))
    }
}

fn read_json<T: DeserializeOwned>(path: &Path, document: &str) -> Result<T> {
    let text = if is_stdin(path) {
        let mut value = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut value)
            .with_context(|| format!("could not read {document} from standard input"))?;
        value
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("could not read {document} {}", path.display()))?
    };
    from_str(&text).with_context(|| {
        if is_stdin(path) {
            format!("invalid {document} JSON from standard input")
        } else {
            format!("invalid {document} JSON in {}", path.display())
        }
    })
}

fn is_stdin(path: &Path) -> bool {
    path.as_os_str() == "-"
}

fn ensure_distinct_stdin_sources(first: &Path, second: &Path) -> Result<()> {
    if is_stdin(first) && is_stdin(second) {
        anyhow::bail!("task and proposal cannot both be read from standard input")
    }
    Ok(())
}

fn default_proposal_reviewer() -> ProposalReviewer {
    let rack = InstrumentRack::default();
    ProposalReviewer::new(ReviewEnvironment {
        available_instrument_ids: rack
            .catalog()
            .into_iter()
            .map(|instrument| instrument.id)
            .collect(),
    })
}

fn soundfont_rack(path: &Path) -> Result<InstrumentRack> {
    #[cfg(feature = "rustysynth-backend")]
    {
        let piano = audio_engine::RustySynthPiano::from_path(path)
            .with_context(|| format!("could not load SoundFont {}", path.display()))?;
        let mut rack = InstrumentRack::new();
        rack.register_named("piano", "Piano", std::sync::Arc::new(piano));
        Ok(rack)
    }
    #[cfg(not(feature = "rustysynth-backend"))]
    {
        let _ = path;
        anyhow::bail!("SoundFont support is disabled; rebuild with --features rustysynth-backend")
    }
}

fn instrument_pack_rack(path: &Path, project: &Project) -> Result<InstrumentRack> {
    let pack = AssetPack::load(path)
        .with_context(|| format!("could not load asset pack {}", path.display()))?;
    InstrumentRack::from_asset_pack_for_project(&pack, project)
        .with_context(|| format!("could not initialize asset pack {}", path.display()))
}

fn package_instrument_rack(
    project_path: &Path,
    project: &Project,
) -> Result<Option<InstrumentRack>> {
    if !project_path.is_dir() {
        return Ok(None);
    }
    let (package, _) = ProjectPackage::open(project_path)
        .with_context(|| format!("could not load project package {}", project_path.display()))?;
    let role = instrument_asset_role("piano");
    let Some(manifest) = package
        .resolve_source_asset(&role)
        .with_context(|| format!("could not resolve bound source asset '{role}'"))?
    else {
        return Ok(None);
    };
    let pack = AssetPack::load(&manifest)
        .with_context(|| format!("could not load bound asset pack {}", manifest.display()))?;
    InstrumentRack::from_asset_pack_for_project(&pack, project)
        .with_context(|| {
            format!(
                "could not initialize bound asset pack {}",
                manifest.display()
            )
        })
        .map(Some)
}

fn instrument_asset_role(instrument_id: &str) -> String {
    format!("instrument:{instrument_id}")
}
