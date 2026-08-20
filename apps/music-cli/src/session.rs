use anyhow::{Context, Result};
use clap::ValueEnum;
use composition_engine::{
    ArrangementAnalyzer, CompositionProposal, CompositionSessionError, CompositionSessions,
    CompositionTask, CritiqueReport, ProposalReviewer,
};
use music_core::{Project, ProjectEngine};
use project_package::{ArtifactDirectory, ProjectPackage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_str, json, to_string, to_value};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum SessionRole {
    /// Reads and writes evaluator reports, but cannot review or apply music.
    Evaluator,
    /// Reviews/applies proposals, but cannot author evaluator reports.
    Composer,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum SessionRequest {
    Context,
    Analyze,
    Events {
        track_id: String,
        clip_id: String,
        start_tick: i64,
        end_tick: i64,
    },
    Critique {
        task_id: String,
        report: CritiqueReport,
    },
    Authorize {
        task: CompositionTask,
    },
    Review {
        task_id: String,
        proposal: CompositionProposal,
    },
    Apply {
        task_id: String,
        proposal: CompositionProposal,
    },
    Revoke {
        task_id: String,
    },
    Reload,
    Ping,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SessionResponse {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<SessionFailure>,
}

impl SessionResponse {
    fn success(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(code: SessionFailureCode, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(SessionFailure {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct SessionFailure {
    code: SessionFailureCode,
    message: String,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SessionFailureCode {
    InvalidRequest,
    AuthorizationDenied,
    TaskNotFound,
    TaskConsumed,
    TaskRevoked,
    CritiqueNotFound,
    InvalidCritique,
    RoleDenied,
    ProjectChanged,
    RequestFailed,
}

const SESSION_PROJECT_CHANGED: &str =
    "project changed outside this session; project was reloaded and all task ids were invalidated";

pub(crate) fn run(
    project_path: PathBuf,
    task_path: Option<PathBuf>,
    role: SessionRole,
    critique_paths: Vec<PathBuf>,
    allow_authorize: bool,
    reviewer: ProposalReviewer,
) -> Result<()> {
    let initial_project = load(&project_path)?;
    let mut engine = ProjectEngine::new(initial_project);
    let mut sessions = CompositionSessions::new(reviewer);

    if let Some(task_path) = task_path {
        if task_path.as_os_str() == "-" {
            anyhow::bail!(
                "session --task cannot read from stdin because stdin carries JSONL requests"
            )
        }
        let task: CompositionTask = read_json(&task_path, "composition task")?;
        let authorization = sessions.authorize(engine.project(), task);
        if let Some(authorized_task) = authorization.authorized_task.as_ref() {
            if role == SessionRole::Composer {
                let task_id = &authorized_task.task_id;
                for critique_path in critique_paths {
                    let stored: composition_engine::StoredCritique =
                        read_json(&critique_path, "stored critique")?;
                    sessions.attach_critique(engine.project(), task_id, stored)?;
                }
            } else if !critique_paths.is_empty() {
                anyhow::bail!("--critique can only be used by a composer session")
            }
        }
        let envelope = SessionResponse::success(to_value(&authorization)?);
        write_response(&mut io::BufWriter::new(io::stdout().lock()), &envelope)?;
        if authorization.authorized_task.is_none() {
            anyhow::bail!("initial composition task was rejected")
        }
    } else if !critique_paths.is_empty() {
        anyhow::bail!("--critique requires --task so the host can bind it to an authorized task")
    }

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let envelope = match from_str::<SessionRequest>(&line) {
            Ok(request) => match handle_request(
                &project_path,
                &mut engine,
                &mut sessions,
                request,
                role,
                allow_authorize,
            ) {
                Ok(value) => SessionResponse::success(value),
                Err(error) => failure_from_error(&error),
            },
            Err(error) => SessionResponse::failure(
                SessionFailureCode::InvalidRequest,
                format!("invalid session request JSON: {error}"),
            ),
        };
        write_response(&mut writer, &envelope)?;
    }
    Ok(())
}

fn handle_request(
    project_path: &Path,
    engine: &mut ProjectEngine,
    sessions: &mut CompositionSessions,
    request: SessionRequest,
    role: SessionRole,
    allow_authorize: bool,
) -> Result<Value> {
    if !matches!(
        &request,
        SessionRequest::Ping | SessionRequest::Reload | SessionRequest::Revoke { .. }
    ) {
        ensure_project_current(project_path, engine, sessions)?;
    }
    match request {
        SessionRequest::Ping => Ok(json!({
            "service": "musicctl-session",
            "version": 1,
            "role": match role {
                SessionRole::Evaluator => "evaluator",
                SessionRole::Composer => "composer",
            }
        })),
        SessionRequest::Context => Ok(to_value(engine.project().summary())?),
        SessionRequest::Analyze => Ok(to_value(ArrangementAnalyzer.analyze(engine.project())?)?),
        SessionRequest::Events {
            track_id,
            clip_id,
            start_tick,
            end_tick,
        } => Ok(to_value(
            engine
                .project()
                .clip_window(&track_id, &clip_id, start_tick, end_tick)?,
        )?),
        SessionRequest::Critique { task_id, report } => {
            require_role(role, SessionRole::Evaluator)?;
            let stored = sessions.record_critique(engine.project(), &task_id, report)?;
            if let Ok(package) = open_package(project_path)
                && let Err(error) = package.write_json_artifact(
                    ArtifactDirectory::History,
                    &format!(
                        "revision-{}-{}.json",
                        stored.report.base_revision, stored.id
                    ),
                    &stored,
                )
            {
                eprintln!("warning: could not write session critique history: {error}");
            }
            Ok(to_value(stored)?)
        }
        SessionRequest::Authorize { task } => {
            if !allow_authorize {
                anyhow::bail!("session host does not allow model-side authorization")
            }
            Ok(to_value(sessions.authorize(engine.project(), task))?)
        }
        SessionRequest::Review { task_id, proposal } => {
            require_role(role, SessionRole::Composer)?;
            Ok(to_value(sessions.review(
                engine.project(),
                &task_id,
                &proposal,
            )?)?)
        }
        SessionRequest::Apply { task_id, proposal } => {
            require_role(role, SessionRole::Composer)?;
            let expected_revision = engine.revision();
            let application = sessions.apply(engine, &task_id, &proposal)?;
            if application.change.is_some() {
                save(project_path, engine.project(), Some(expected_revision))?;
                if let (Some(change), Ok(package)) =
                    (application.change.as_ref(), open_package(project_path))
                {
                    let revision = change.revision;
                    if let Err(error) = package.write_json_artifact(
                        ArtifactDirectory::History,
                        &format!("revision-{revision}-proposal.json"),
                        &proposal,
                    ) {
                        eprintln!("warning: could not write session proposal history: {error}");
                    }
                    if let Err(error) = package.write_json_artifact(
                        ArtifactDirectory::History,
                        &format!("revision-{revision}-application.json"),
                        &application,
                    ) {
                        eprintln!("warning: could not write session application history: {error}");
                    }
                }
            }
            Ok(to_value(application)?)
        }
        SessionRequest::Revoke { task_id } => {
            require_role(role, SessionRole::Composer)?;
            sessions.revoke(&task_id)?;
            Ok(json!({ "revoked": true, "task_id": task_id }))
        }
        SessionRequest::Reload => {
            require_role(role, SessionRole::Composer)?;
            let loaded = load(project_path)?;
            *engine = ProjectEngine::new(loaded);
            sessions.clear();
            Ok(json!({ "reloaded": true, "revision": engine.revision() }))
        }
    }
}

fn require_role(actual: SessionRole, required: SessionRole) -> Result<()> {
    if actual != required {
        let actual_name = match actual {
            SessionRole::Evaluator => "evaluator",
            SessionRole::Composer => "composer",
        };
        let required_name = match required {
            SessionRole::Evaluator => "evaluator",
            SessionRole::Composer => "composer",
        };
        anyhow::bail!(
            "session role '{actual_name}' cannot perform this operation; use a '{required_name}' session"
        )
    }
    Ok(())
}

fn failure_from_error(error: &anyhow::Error) -> SessionResponse {
    if let Some(error) = error.downcast_ref::<CompositionSessionError>() {
        let code = match error {
            CompositionSessionError::TaskNotFound(_) => SessionFailureCode::TaskNotFound,
            CompositionSessionError::TaskConsumed(_) => SessionFailureCode::TaskConsumed,
            CompositionSessionError::TaskRevoked(_) => SessionFailureCode::TaskRevoked,
            CompositionSessionError::CritiqueNotFound(_) => SessionFailureCode::CritiqueNotFound,
            CompositionSessionError::CritiqueRevisionMismatch { .. }
            | CompositionSessionError::InvalidCritique(_) => SessionFailureCode::InvalidCritique,
        };
        return SessionResponse::failure(code, error.to_string());
    }
    let message = error.to_string();
    let code = if message == "session host does not allow model-side authorization" {
        SessionFailureCode::AuthorizationDenied
    } else if message.starts_with("session role '") {
        SessionFailureCode::RoleDenied
    } else if message == SESSION_PROJECT_CHANGED {
        SessionFailureCode::ProjectChanged
    } else {
        SessionFailureCode::RequestFailed
    };
    SessionResponse::failure(code, message)
}

fn ensure_project_current(
    project_path: &Path,
    engine: &mut ProjectEngine,
    sessions: &mut CompositionSessions,
) -> Result<()> {
    let on_disk = load(project_path)?;
    if &on_disk != engine.project() {
        *engine = ProjectEngine::new(on_disk);
        sessions.clear();
        anyhow::bail!(SESSION_PROJECT_CHANGED)
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

fn save(path: &Path, project: &Project, expected_revision: Option<u64>) -> Result<()> {
    if path.is_dir() {
        let (package, _) = ProjectPackage::open(path)
            .with_context(|| format!("could not load project package {}", path.display()))?;
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

fn open_package(path: &Path) -> Result<ProjectPackage> {
    let (package, _) = ProjectPackage::open(path)
        .with_context(|| format!("could not load project package {}", path.display()))?;
    Ok(package)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, document: &str) -> Result<T> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {document} {}", path.display()))?;
    from_str(&text).with_context(|| format!("invalid {document} JSON in {}", path.display()))
}

fn write_response<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    writeln!(writer, "{}", to_string(value)?)?;
    writer.flush()?;
    Ok(())
}
