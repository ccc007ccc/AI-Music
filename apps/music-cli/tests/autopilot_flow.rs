#![cfg(unix)]

use autopilot_engine::{AutopilotMemory, AutopilotOutcome};
use project_package::{ArtifactDirectory, ProjectPackage};
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ai-music-autopilot-flow-{}-{nonce}",
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
fn autopilot_cli_preserves_session_across_consecutive_instructions() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let project_path = directory.join("Automatic Trial.aimusic");
    let created = Command::new(env!("CARGO_BIN_EXE_musicctl"))
        .args(["new", path(&project_path)])
        .output()?;
    assert!(created.status.success(), "{}", stderr(&created));

    let fake_codex = directory.join("fake-codex.py");
    let call_log = directory.join("fake-codex.jsonl");
    fs::write(&fake_codex, FAKE_CODEX)?;
    let mut permissions = fs::metadata(&fake_codex)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions)?;

    let output = Command::new(env!("CARGO_BIN_EXE_musicctl"))
        .args([
            "autopilot",
            path(&project_path),
            "创作一个安静、逐渐明亮的钢琴开头",
            "--max-revisions",
            "0",
        ])
        .env("AI_MUSIC_CODEX_BIN", &fake_codex)
        .env("FAKE_CODEX_LOG", &call_log)
        .output()?;
    assert!(output.status.success(), "{}", stderr(&output));

    let first_response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(first_response["outcome"]["starting_revision"], 0);
    assert_eq!(first_response["outcome"]["final_revision"], 1);
    assert_eq!(first_response["outcome"]["status"], "completed");
    let first_session_id = first_response["outcome"]["session_id"]
        .as_str()
        .expect("first outcome has a session ID")
        .to_owned();
    let first_render_path = PathBuf::from(
        first_response["render_path"]
            .as_str()
            .expect("CLI returns the render path"),
    );
    assert!(first_render_path.is_file());
    assert!(fs::metadata(&first_render_path)?.len() > 44);

    let output = Command::new(env!("CARGO_BIN_EXE_musicctl"))
        .args([
            "autopilot",
            path(&project_path),
            "后两小节更有推动感，但保留安静的开头",
            "--max-revisions",
            "0",
        ])
        .env("AI_MUSIC_CODEX_BIN", &fake_codex)
        .env("FAKE_CODEX_LOG", &call_log)
        .output()?;
    assert!(output.status.success(), "{}", stderr(&output));
    let second_response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(second_response["outcome"]["starting_revision"], 1);
    assert_eq!(second_response["outcome"]["final_revision"], 2);
    assert_eq!(second_response["outcome"]["status"], "completed");
    assert_eq!(second_response["outcome"]["session_id"], first_session_id);
    let second_render_path = PathBuf::from(
        second_response["render_path"]
            .as_str()
            .expect("CLI returns the second render path"),
    );
    assert!(second_render_path.is_file());
    assert!(fs::metadata(&second_render_path)?.len() > 44);
    assert_ne!(first_render_path, second_render_path);

    let (package, project) = ProjectPackage::open(&project_path)?;
    assert_eq!(project.revision, 2);
    assert_eq!(
        project
            .midi_clip("piano", "piano-main")
            .expect("default piano clip")
            .notes
            .len(),
        2
    );
    let memory: AutopilotMemory =
        package.read_json_artifact(ArtifactDirectory::History, "autopilot-session.json")?;
    assert_eq!(memory.session_id, first_session_id);
    assert_eq!(memory.turns.len(), 2);
    assert_eq!(memory.turns[1].starting_revision, 1);
    assert_eq!(memory.turns[1].final_revision, 2);
    let first_outcome: AutopilotOutcome =
        package.read_json_artifact(ArtifactDirectory::History, "revision-1-autopilot.json")?;
    let second_outcome: AutopilotOutcome =
        package.read_json_artifact(ArtifactDirectory::History, "revision-2-autopilot.json")?;
    assert_eq!(first_outcome.session_id, memory.session_id);
    assert_eq!(first_outcome.final_revision, 1);
    assert_eq!(second_outcome.session_id, memory.session_id);
    assert_eq!(second_outcome.final_revision, 2);

    let calls = fs::read_to_string(call_log)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        calls
            .iter()
            .map(|call| call["role"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "director",
            "composer",
            "evaluator",
            "director",
            "composer",
            "evaluator"
        ]
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["role"] == "director")
            .map(|call| call["history_length"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    for call in &calls {
        let args = call["args"]
            .as_array()
            .expect("fake Codex records argv")
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(has_pair(
            &args,
            "--config",
            "model_reasoning_effort=\"medium\""
        ));
        assert!(has_pair(&args, "--disable", "code_mode"));
        assert!(!has_pair(&args, "--disable", "code_mode_host"));
        assert!(has_pair(&args, "--sandbox", "read-only"));
        assert!(has_pair(&args, "--ask-for-approval", "never"));
        assert!(args.contains(&"--ephemeral"));
        assert!(!args.contains(&"--ignore-user-config"));
    }

    Ok(())
}

#[test]
fn failed_autopilot_cli_run_leaves_package_and_artifacts_unchanged() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let project_path = directory.join("Rollback Trial.aimusic");
    let created = Command::new(env!("CARGO_BIN_EXE_musicctl"))
        .args(["new", path(&project_path)])
        .output()?;
    assert!(created.status.success(), "{}", stderr(&created));
    let before = fs::read(project_path.join("project.json"))?;

    let failing_codex = directory.join("failing-codex.py");
    fs::write(&failing_codex, FAILING_CODEX)?;
    let mut permissions = fs::metadata(&failing_codex)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&failing_codex, permissions)?;

    let output = Command::new(env!("CARGO_BIN_EXE_musicctl"))
        .args([
            "autopilot",
            path(&project_path),
            "创作一段钢琴音乐",
            "--max-revisions",
            "0",
        ])
        .env("AI_MUSIC_CODEX_BIN", &failing_codex)
        .output()?;
    assert!(!output.status.success());
    assert_eq!(fs::read(project_path.join("project.json"))?, before);
    assert!(fs::read_dir(project_path.join("history"))?.next().is_none());
    assert!(fs::read_dir(project_path.join("renders"))?.next().is_none());

    Ok(())
}

fn has_pair(arguments: &[&str], first: &str, second: &str) -> bool {
    arguments
        .windows(2)
        .any(|window| window[0] == first && window[1] == second)
}

fn path(value: &Path) -> &str {
    value.to_str().expect("test paths are valid UTF-8")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const FAKE_CODEX: &str = r#"#!/usr/bin/env python3
import json
import os
import sys

args = sys.argv[1:]
prompt = sys.stdin.read()

def option(name):
    index = args.index(name)
    return args[index + 1]

if "trusted music director" in prompt:
    role = "director"
    director_input = prompt.split("INPUT:\n", 1)[1].split(
        "\n\nPREVIOUS INVALID RESULT FEEDBACK:\n", 1
    )[0]
    director_payload = json.loads(director_input)
    history_length = len(director_payload["conversation_history"])
    result = {
        "summary": "Create a quiet piano opening that becomes brighter",
        "target": {"start_tick": 0, "end_tick": 3840},
        "objectives": [{
            "id": "opening",
            "description": "Establish an audible quiet-to-bright opening gesture",
            "priority": "required"
        }],
        "freedoms": ["Choose the exact voicing"],
        "style_context": ["Begin quietly and become brighter"],
        "rhythm": {
            "onset_grid_tick": None,
            "require_bar_aligned_sections": False,
            "minimum_active_bars": None
        }
    }
elif "You are the Composer" in prompt:
    role = "composer"
    payload = json.loads(prompt.split("INPUT:\n", 1)[1])
    task = payload["authorized_task"]["task"]
    brief = task["brief"]
    revision = task["scope"]["base_revision"]
    if revision == 0:
        section_id = "opening-section"
        section_start = 0
        section_end = 960
        operation = {
            "op": "add_note",
            "track_id": "piano",
            "clip_id": "piano-main",
            "note": {
                "id": "fake-autopilot-opening",
                "start_tick": 0,
                "duration_tick": 960,
                "pitch": 60,
                "velocity": 70
            }
        }
    else:
        section_id = "continuation-section"
        section_start = 2880
        section_end = 3360
        operation = {
            "op": "add_note",
            "track_id": "piano",
            "clip_id": "piano-main",
            "note": {
                "id": "fake-autopilot-continuation",
                "start_tick": 2880,
                "duration_tick": 480,
                "pitch": 67,
                "velocity": 92
            }
        }
    result = {
        "brief_id": brief["id"],
        "plan": {
            "summary": "Use a soft middle-register attack as the opening seed",
            "sections": [{
                "id": section_id,
                "start_tick": section_start,
                "end_tick": section_end,
                "intent": "State the quiet opening gesture"
            }],
            "track_roles": [{"track_id": "piano", "role": "solo opening voice"}],
            "objective_coverage": [{
                "objective_id": "opening",
                "evidence": [{
                    "description": "The opening note establishes the requested gesture",
                    "section_id": section_id,
                    "track_id": "piano",
                    "start_tick": section_start,
                    "end_tick": section_end
                }]
            }],
            "decisions": [{
                "decision": "Start with one soft C4",
                "rationale": "A restrained attack creates room for later brightening"
            }]
        },
        "based_on_critique_id": None,
        "critique_responses": [],
        "patch": {
            "base_revision": revision,
            "description": "Create the automatic opening",
            "operations": [operation]
        }
    }
elif "independent music Evaluator" in prompt:
    role = "evaluator"
    result = {
        "conclusion": "accept",
        "summary": "The committed opening satisfies the requested restrained gesture",
        "observations": [],
        "decisions": [],
        "next_focus": None
    }
else:
    raise SystemExit("unknown model role")

record = {"role": role, "args": args}
if role == "director":
    record["history_length"] = history_length
with open(os.environ["FAKE_CODEX_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps(record, ensure_ascii=False) + "\n")
with open(option("--output-last-message"), "w", encoding="utf-8") as output:
    json.dump(result, output, ensure_ascii=False)
"#;

const FAILING_CODEX: &str = r#"#!/usr/bin/env python3
import sys
sys.stderr.write("simulated provider outage\n")
raise SystemExit(1)
"#;
