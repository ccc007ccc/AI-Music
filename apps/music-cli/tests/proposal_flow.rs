use audio_engine::{AudioBuffer, write_wav};
use composition_engine::{
    COMPOSITION_SCHEMA_VERSION, CompositionPlan, CompositionProposal, CompositionTask,
    CoverageEvidence, CreativeBrief, CreativeDecision, CreativeObjective, CritiqueDecision,
    CritiqueDisposition, CritiqueLocation, CritiqueObservation, CritiqueReport, CritiqueResponse,
    EditCapability, EditScope, ObjectiveCoverage, ObjectivePriority, PlannedSection,
    PlannedTrackRole, ProposalApplication, ProposalReview, ReviewStatus, RhythmConstraints,
    ScopedTickRange, StoredCritique, TickRange, TrackAccess,
};
use music_core::{Command, NoteEvent, Patch, Project, ProjectEngine};
use project_package::{ProjectPackage, SourceAssetLocation};
use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command as ProcessCommand, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ai-music-proposal-flow-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn proposal_commands_review_commit_and_reject_stale_reapplication() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let project_path = directory.join("project.json");
    let task_path = directory.join("task.json");
    let proposal_path = directory.join("proposal.json");
    Project::default().save(&project_path)?;
    fs::write(&task_path, serde_json::to_vec_pretty(&task())?)?;
    fs::write(&proposal_path, serde_json::to_vec_pretty(&proposal())?)?;

    let review_output = musicctl(&[
        "review-proposal",
        path(&project_path),
        path(&task_path),
        path(&proposal_path),
    ])?;
    assert!(review_output.status.success());
    let review: ProposalReview = serde_json::from_slice(&review_output.stdout)?;
    assert_eq!(review.status, ReviewStatus::Ready);

    let apply_output = musicctl(&[
        "apply-proposal",
        path(&project_path),
        path(&task_path),
        path(&proposal_path),
    ])?;
    assert!(apply_output.status.success());
    let application: ProposalApplication = serde_json::from_slice(&apply_output.stdout)?;
    assert_eq!(application.review.status, ReviewStatus::Ready);
    assert_eq!(
        application.change.as_ref().map(|change| change.revision),
        Some(1)
    );

    let project = Project::load(&project_path)?;
    assert_eq!(project.revision, 1);
    assert_eq!(
        project
            .midi_clip("piano", "piano-main")
            .expect("default piano clip")
            .notes
            .len(),
        1
    );

    let before = fs::read(&project_path)?;
    let stale_output = musicctl(&[
        "apply-proposal",
        path(&project_path),
        path(&task_path),
        path(&proposal_path),
    ])?;
    assert!(!stale_output.status.success());
    let blocked: ProposalApplication = serde_json::from_slice(&stale_output.stdout)?;
    assert_eq!(blocked.review.status, ReviewStatus::NeedsRevision);
    assert!(blocked.change.is_none());
    assert_eq!(fs::read(&project_path)?, before);

    Ok(())
}

#[test]
fn arrangement_analysis_is_available_from_cli_and_session_without_editing()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let project_path = directory.join("project.json");
    let task_path = directory.join("task.json");
    Project::default().save(&project_path)?;
    fs::write(&task_path, serde_json::to_vec_pretty(&task())?)?;

    let output = musicctl(&["analyze-arrangement", path(&project_path)])?;
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["project_revision"], 0);
    assert_eq!(report["semantics"]["findings_are_advisory"], true);
    assert_eq!(report["semantics"]["application_may_be_blocked"], false);

    let before = fs::read(&project_path)?;
    let mut session = SessionProcess::start(&project_path, &task_path)?;
    assert_eq!(session.read_response()?["ok"], true);
    let analyzed = session.request(serde_json::json!({ "op": "analyze" }))?;
    assert_eq!(analyzed["ok"], true);
    assert_eq!(analyzed["result"]["project_revision"], 0);
    assert_eq!(fs::read(&project_path)?, before);
    assert!(session.finish()?.success());
    Ok(())
}

#[test]
fn jsonl_session_keeps_authority_host_side_and_consumes_it_once() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let project_path = directory.join("project.json");
    let task_path = directory.join("task.json");
    let critique_path = directory.join("critique.json");
    Project::default().save(&project_path)?;
    fs::write(&task_path, serde_json::to_vec_pretty(&task())?)?;

    let mut evaluator = SessionProcess::start_with(&project_path, &task_path, "evaluator", &[])?;
    let evaluator_initial = evaluator.read_response()?;
    assert_eq!(evaluator_initial["ok"], true);
    let evaluator_task_id = evaluator_initial["result"]["authorized_task"]["task_id"]
        .as_str()
        .expect("evaluator task id")
        .to_owned();
    let critique = evaluator.request(serde_json::json!({
        "op": "critique",
        "task_id": evaluator_task_id,
        "report": critique_report()
    }))?;
    assert_eq!(critique["ok"], true);
    let evaluator_revoke = evaluator.request(serde_json::json!({
        "op": "revoke",
        "task_id": evaluator_task_id
    }))?;
    assert_eq!(evaluator_revoke["ok"], false);
    assert_eq!(evaluator_revoke["error"]["code"], "role_denied");
    let evaluator_reload = evaluator.request(serde_json::json!({ "op": "reload" }))?;
    assert_eq!(evaluator_reload["ok"], false);
    assert_eq!(evaluator_reload["error"]["code"], "role_denied");
    fs::write(
        &critique_path,
        serde_json::to_vec_pretty(&critique["result"])?,
    )?;
    let evaluator_review = evaluator.request(serde_json::json!({
        "op": "review",
        "task_id": evaluator_task_id,
        "proposal": proposal()
    }))?;
    assert_eq!(evaluator_review["ok"], false);
    assert_eq!(evaluator_review["error"]["code"], "role_denied");
    assert!(evaluator.finish()?.success());

    let mut session = SessionProcess::start_with(
        &project_path,
        &task_path,
        "composer",
        &[critique_path.as_path()],
    )?;
    let initial = session.read_response()?;
    assert_eq!(initial["ok"], true);
    let task_id = initial["result"]["authorized_task"]["task_id"]
        .as_str()
        .expect("authorized task id")
        .to_owned();

    let unauthorized = session.request(serde_json::json!({
        "op": "authorize",
        "task": task()
    }))?;
    assert_eq!(unauthorized["ok"], false);
    assert_eq!(unauthorized["error"]["code"], "authorization_denied");

    let composer_critique = session.request(serde_json::json!({
        "op": "critique",
        "task_id": task_id,
        "report": critique_report()
    }))?;
    assert_eq!(composer_critique["ok"], false);
    assert_eq!(composer_critique["error"]["code"], "role_denied");
    let stored: StoredCritique = serde_json::from_slice(&fs::read(&critique_path)?)?;
    let critique_id = stored.id;

    let mut revised_proposal = proposal();
    revised_proposal.based_on_critique_id = Some(critique_id);
    revised_proposal.critique_responses = vec![CritiqueResponse {
        observation_id: "opening-attack".to_owned(),
        rationale: "Retain the single gesture while making its attack explicit".to_owned(),
    }];
    let review = session.request(serde_json::json!({
        "op": "review",
        "task_id": task_id,
        "proposal": revised_proposal
    }))?;
    assert_eq!(review["ok"], true);
    assert_eq!(review["result"]["status"], "ready");

    let bypass = session.request(serde_json::json!({
        "op": "review",
        "task_id": task_id,
        "proposal": proposal()
    }))?;
    assert_eq!(bypass["ok"], false);
    assert_eq!(bypass["error"]["code"], "invalid_critique");

    let application = session.request(serde_json::json!({
        "op": "apply",
        "task_id": task_id,
        "proposal": revised_proposal
    }))?;
    assert_eq!(application["ok"], true);
    assert_eq!(application["result"]["change"]["revision"], 1);

    let events = session.request(serde_json::json!({
        "op": "events",
        "track_id": "piano",
        "clip_id": "piano-main",
        "start_tick": 0,
        "end_tick": 3840
    }))?;
    assert_eq!(events["ok"], true);
    assert_eq!(events["result"]["ppq"], 960);
    assert_eq!(events["result"]["bar_length_tick"], 3840);
    assert_eq!(events["result"]["notes"][0]["pitch"], 60);
    assert_eq!(events["result"]["notes"][0]["pitch_name"], "C4");
    assert_eq!(events["result"]["notes"][0]["start_position"]["bar"], 1);
    assert_eq!(
        events["result"]["notes"][0]["duration_quarters"],
        serde_json::json!({ "numerator": 1, "denominator": 2 })
    );
    assert_eq!(events["result"]["notes"][0]["common_duration"], "eighth");

    let consumed = session.request(serde_json::json!({
        "op": "review",
        "task_id": task_id,
        "proposal": proposal()
    }))?;
    assert_eq!(consumed["ok"], false);
    assert_eq!(consumed["error"]["code"], "task_consumed");

    let status = session.finish()?;
    assert!(status.success());
    assert_eq!(Project::load(&project_path)?.revision, 1);
    Ok(())
}

#[test]
fn composer_session_validates_attached_critique_before_announcing_authorization()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let project_path = directory.join("project.json");
    let task_path = directory.join("task.json");
    let critique_path = directory.join("invalid-critique.json");
    Project::default().save(&project_path)?;
    fs::write(&task_path, serde_json::to_vec_pretty(&task())?)?;
    let mut report = critique_report();
    report.brief_id = "different-brief".to_owned();
    fs::write(
        &critique_path,
        serde_json::to_vec_pretty(&StoredCritique {
            id: "critique-invalid-attachment".to_owned(),
            report,
        })?,
    )?;

    let output = musicctl(&[
        "session",
        path(&project_path),
        "--task",
        path(&task_path),
        "--role",
        "composer",
        "--critique",
        path(&critique_path),
    ])?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("critique brief"));
    Ok(())
}

#[test]
fn jsonl_session_detects_external_project_changes_before_overwriting() -> Result<(), Box<dyn Error>>
{
    let directory = TestDirectory::new()?;
    let project_path = directory.join("project.json");
    let task_path = directory.join("task.json");
    Project::default().save(&project_path)?;
    fs::write(&task_path, serde_json::to_vec_pretty(&task())?)?;

    let mut session = SessionProcess::start(&project_path, &task_path)?;
    let initial = session.read_response()?;
    let task_id = initial["result"]["authorized_task"]["task_id"]
        .as_str()
        .expect("authorized task id")
        .to_owned();

    let mut external = ProjectEngine::new(Project::load(&project_path)?);
    external.apply(Command::SetTempo { tick: 0, bpm: 90.0 })?;
    external.project().save(&project_path)?;

    let changed = session.request(serde_json::json!({ "op": "context" }))?;
    assert_eq!(changed["ok"], false);
    assert_eq!(changed["error"]["code"], "project_changed");

    let refreshed = session.request(serde_json::json!({ "op": "context" }))?;
    assert_eq!(refreshed["ok"], true);
    assert_eq!(refreshed["result"]["revision"], 1);

    let invalidated = session.request(serde_json::json!({
        "op": "review",
        "task_id": task_id,
        "proposal": proposal()
    }))?;
    assert_eq!(invalidated["ok"], false);
    assert_eq!(invalidated["error"]["code"], "task_not_found");

    assert!(session.finish()?.success());
    let project = Project::load(&project_path)?;
    assert_eq!(project.tempo_map.points[0].bpm, 90.0);
    assert!(
        project
            .midi_clip("piano", "piano-main")
            .expect("default piano clip")
            .notes
            .is_empty()
    );
    Ok(())
}

#[test]
fn jsonl_session_reload_supports_directory_project_packages() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let package_path = directory.join("Session Package.aimusic");
    let task_path = directory.join("task.json");
    let critique_path = directory.join("critique.json");
    let created = musicctl(&["new-project", path(&directory.0), "Session Package"])?;
    assert!(created.status.success());
    fs::write(&task_path, serde_json::to_vec_pretty(&task())?)?;

    let mut evaluator = SessionProcess::start_with(&package_path, &task_path, "evaluator", &[])?;
    let initial = evaluator.read_response()?;
    assert_eq!(initial["ok"], true);
    let task_id = initial["result"]["authorized_task"]["task_id"]
        .as_str()
        .expect("authorized task id")
        .to_owned();

    let critique = evaluator.request(serde_json::json!({
        "op": "critique",
        "task_id": task_id,
        "report": critique_report()
    }))?;
    assert_eq!(critique["ok"], true);
    fs::write(
        &critique_path,
        serde_json::to_vec_pretty(&critique["result"])?,
    )?;
    let critique_id = critique["result"]["id"]
        .as_str()
        .expect("host-assigned critique id");
    let critique_filename = format!("revision-0-{critique_id}.json");
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(
        package_path.join("history").join(critique_filename),
    )?)?;
    assert_eq!(stored["id"], critique_id);
    assert_eq!(stored["report"]["base_revision"], 0);

    assert!(evaluator.finish()?.success());
    let mut session = SessionProcess::start_with(
        &package_path,
        &task_path,
        "composer",
        &[critique_path.as_path()],
    )?;
    let composer_initial = session.read_response()?;
    assert_eq!(composer_initial["ok"], true);
    let task_id = composer_initial["result"]["authorized_task"]["task_id"]
        .as_str()
        .expect("composer task id")
        .to_owned();

    let reloaded = session.request(serde_json::json!({ "op": "reload" }))?;
    assert_eq!(reloaded["ok"], true);
    assert_eq!(reloaded["result"]["revision"], 0);

    let invalidated = session.request(serde_json::json!({
        "op": "review",
        "task_id": task_id,
        "proposal": proposal()
    }))?;
    assert_eq!(invalidated["ok"], false);
    assert_eq!(invalidated["error"]["code"], "task_not_found");
    assert!(session.finish()?.success());
    Ok(())
}

#[test]
fn bound_sfz_pack_is_used_by_package_render_without_an_override() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let package_path = directory.join("Bound Piano.aimusic");
    let pack_directory = directory.join("piano-pack");
    fs::create_dir(&pack_directory)?;
    let sample_path = pack_directory.join("sample.wav");
    let frames = 480;
    let mut samples = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let phase = frame as f32 / 48_000.0 * std::f32::consts::TAU * 261.625_57;
        let sample = phase.sin() * (1.0 - frame as f32 / frames as f32) * 0.25;
        samples.extend([sample, sample]);
    }
    write_wav(
        &AudioBuffer {
            sample_rate: 48_000,
            channels: 2,
            samples,
        },
        &sample_path,
    )?;
    fs::write(
        pack_directory.join("piano.sfz"),
        "<region> sample=sample.wav key=60 pitch_keycenter=60 ampeg_release=0.05\n",
    )?;
    let manifest_path = pack_directory.join("pack.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "id": "cli-bound-piano",
            "name": "CLI Bound Piano",
            "instrument_id": "piano",
            "engine": "sfz",
            "entry": "piano.sfz",
            "license": {
                "spdx": "CC0-1.0",
                "name": "CC0 1.0",
                "source": "https://example.test/cli-bound-piano",
                "attribution": "Generated test fixture"
            }
        }))?,
    )?;

    assert!(
        musicctl(&["new-project", path(&directory.0), "Bound Piano"])?
            .status
            .success()
    );
    assert!(
        musicctl(&[
            "add-note",
            path(&package_path),
            "--pitch",
            "60",
            "--start",
            "0",
            "--duration",
            "480",
            "--velocity",
            "90",
        ])?
        .status
        .success()
    );
    assert!(
        musicctl(&[
            "bind-instrument-pack",
            path(&package_path),
            path(&manifest_path),
        ])?
        .status
        .success()
    );

    let (package, project) = ProjectPackage::open(&package_path)?;
    let binding = package
        .source_asset("instrument:piano")
        .expect("piano source binding");
    assert!(matches!(
        &binding.location,
        SourceAssetLocation::External { manifest_path }
            if Path::new(manifest_path).is_absolute()
    ));
    let render = musicctl(&["render", path(&package_path)])?;
    assert!(
        render.status.success(),
        "{}",
        String::from_utf8_lossy(&render.stderr)
    );
    let output = package
        .artifact_dir(project_package::ArtifactDirectory::Renders)
        .join(format!("preview-r{}.wav", project.revision));
    assert!(fs::metadata(output)?.len() > 44);
    Ok(())
}

struct SessionProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl SessionProcess {
    fn start(project: &Path, task: &Path) -> Result<Self, Box<dyn Error>> {
        Self::start_with(project, task, "composer", &[])
    }

    fn start_with(
        project: &Path,
        task: &Path,
        role: &str,
        critiques: &[&Path],
    ) -> Result<Self, Box<dyn Error>> {
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_musicctl"))
            .args([
                "session",
                path(project),
                "--task",
                path(task),
                "--role",
                role,
            ])
            .args(
                critiques
                    .iter()
                    .flat_map(|critique| ["--critique", path(critique)]),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("session stdin");
        let stdout = child.stdout.take().expect("session stdout");
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        })
    }

    fn request(&mut self, request: serde_json::Value) -> Result<serde_json::Value, Box<dyn Error>> {
        let stdin = self.stdin.as_mut().expect("session is open");
        serde_json::to_writer(&mut *stdin, &request)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        self.read_response()
    }

    fn read_response(&mut self) -> Result<serde_json::Value, Box<dyn Error>> {
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        if line.is_empty() {
            return Err("session ended before returning a response".into());
        }
        Ok(serde_json::from_str(&line)?)
    }

    fn finish(mut self) -> Result<ExitStatus, Box<dyn Error>> {
        drop(self.stdin.take());
        Ok(self.child.wait()?)
    }
}

impl Drop for SessionProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn musicctl(arguments: &[&str]) -> Result<std::process::Output, std::io::Error> {
    ProcessCommand::new(env!("CARGO_BIN_EXE_musicctl"))
        .args(arguments)
        .output()
}

fn path(value: &Path) -> &str {
    value.to_str().expect("test paths are valid UTF-8")
}

fn task() -> CompositionTask {
    CompositionTask {
        brief: CreativeBrief {
            schema_version: COMPOSITION_SCHEMA_VERSION,
            id: "cli-flow".to_owned(),
            summary: "Add a compact piano opening".to_owned(),
            target: TickRange {
                start_tick: 0,
                end_tick: 3_840,
            },
            objectives: vec![CreativeObjective {
                id: "motif".to_owned(),
                description: "Introduce one identifiable opening gesture".to_owned(),
                priority: ObjectivePriority::Required,
            }],
            freedoms: vec!["Silence may follow the gesture".to_owned()],
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
                    end_tick: 3_840,
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

fn proposal() -> CompositionProposal {
    CompositionProposal {
        brief_id: "cli-flow".to_owned(),
        based_on_critique_id: None,
        critique_responses: Vec::new(),
        plan: CompositionPlan {
            summary: "Use one accented C4 followed by space".to_owned(),
            sections: vec![PlannedSection {
                id: "opening".to_owned(),
                range: TickRange {
                    start_tick: 0,
                    end_tick: 960,
                },
                intent: "State the opening identity".to_owned(),
            }],
            track_roles: vec![PlannedTrackRole {
                track_id: "piano".to_owned(),
                role: "solo gesture".to_owned(),
            }],
            objective_coverage: vec![ObjectiveCoverage {
                objective_id: "motif".to_owned(),
                evidence: vec![CoverageEvidence {
                    description: "The accented C4 is the opening gesture".to_owned(),
                    section_id: Some("opening".to_owned()),
                    track_id: Some("piano".to_owned()),
                    range: Some(TickRange {
                        start_tick: 0,
                        end_tick: 480,
                    }),
                }],
            }],
            decisions: vec![CreativeDecision {
                decision: "Leave space after the first note".to_owned(),
                rationale: "The brief asks for a compact identifiable gesture".to_owned(),
            }],
        },
        patch: Patch {
            base_revision: Some(0),
            description: Some("Add CLI test motif".to_owned()),
            operations: vec![Command::AddNote {
                track_id: "piano".to_owned(),
                clip_id: "piano-main".to_owned(),
                note: NoteEvent {
                    id: "cli-c4".to_owned(),
                    start_tick: 0,
                    duration_tick: 480,
                    pitch: 60,
                    velocity: 92,
                },
            }],
        },
    }
}

fn critique_report() -> CritiqueReport {
    CritiqueReport {
        brief_id: "cli-flow".to_owned(),
        base_revision: 0,
        summary: "The opening gesture needs a clearer attack".to_owned(),
        observations: vec![CritiqueObservation {
            id: "opening-attack".to_owned(),
            location: CritiqueLocation {
                label: Some("opening".to_owned()),
                track_id: Some("piano".to_owned()),
                range: Some(TickRange {
                    start_tick: 0,
                    end_tick: 480,
                }),
            },
            observation: "The first onset is the sole identifying event".to_owned(),
            consequence: "Its attack must remain distinct from the following silence".to_owned(),
            proposed_revision: Some("Keep the accented onset and preserve the release".to_owned()),
        }],
        decisions: vec![CritiqueDecision {
            observation_id: "opening-attack".to_owned(),
            disposition: CritiqueDisposition::Modify,
            rationale: "The evaluator selects a focused attack revision for the stated brief"
                .to_owned(),
        }],
        next_focus: Some("attack and release".to_owned()),
    }
}
