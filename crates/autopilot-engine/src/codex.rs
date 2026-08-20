use crate::{ModelBackendError, ModelRequest, StructuredModel};
use serde_json::Value;
use std::fs::{self, File};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::tempdir;
use wait_timeout::ChildExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Non-interactive Codex CLI adapter for the structured-model seam.
///
/// Each request is an ephemeral, read-only process with approvals disabled.
/// The model receives all musical context in the prompt and can only return a
/// value conforming to the supplied JSON Schema.
#[derive(Clone, Debug)]
pub struct CodexCliModel {
    executable: String,
    model: Option<String>,
    timeout: Duration,
}

impl Default for CodexCliModel {
    fn default() -> Self {
        Self {
            executable: "codex".to_owned(),
            model: None,
            timeout: Duration::from_secs(120),
        }
    }
}

impl CodexCliModel {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            ..Self::default()
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl StructuredModel for CodexCliModel {
    fn complete(&mut self, request: ModelRequest<'_>) -> Result<Value, ModelBackendError> {
        let directory = tempdir().map_err(|error| {
            ModelBackendError::new(format!("could not create model working directory: {error}"))
        })?;
        let schema_path = directory.path().join("output.schema.json");
        let output_path = directory.path().join("output.json");
        let stdout_path = directory.path().join("stdout.log");
        let stderr_path = directory.path().join("stderr.log");
        let schema_file = File::create(&schema_path).map_err(|error| {
            ModelBackendError::new(format!("could not create model output schema: {error}"))
        })?;
        serde_json::to_writer_pretty(schema_file, request.schema).map_err(|error| {
            ModelBackendError::new(format!("could not serialize model output schema: {error}"))
        })?;
        let stdout = File::create(&stdout_path).map_err(|error| {
            ModelBackendError::new(format!("could not create model stdout log: {error}"))
        })?;
        let stderr = File::create(&stderr_path).map_err(|error| {
            ModelBackendError::new(format!("could not create model stderr log: {error}"))
        })?;

        let mut command = Command::new(&self.executable);
        command
            .arg("--ask-for-approval")
            .arg("never")
            .arg("exec")
            .arg("--config")
            .arg("model_reasoning_effort=\"medium\"")
            .arg("--disable")
            .arg("shell_tool")
            .arg("--disable")
            .arg("unified_exec")
            .arg("--disable")
            .arg("code_mode")
            .arg("--disable")
            .arg("hooks")
            .arg("--disable")
            .arg("apps")
            .arg("--disable")
            .arg("multi_agent")
            .arg("--disable")
            .arg("browser_use")
            .arg("--disable")
            .arg("computer_use")
            .arg("--ephemeral")
            .arg("--skip-git-repo-check")
            .arg("--ignore-rules")
            .arg("--sandbox")
            .arg("read-only")
            .arg("--output-schema")
            .arg(&schema_path)
            .arg("--output-last-message")
            .arg(&output_path)
            .arg("--cd")
            .arg(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .env("NO_COLOR", "1");
        #[cfg(unix)]
        command.process_group(0);
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        command.arg("-");

        let mut child = command.spawn().map_err(|error| {
            ModelBackendError::new(format!(
                "could not start '{}' for {:?}: {error}",
                self.executable, request.role
            ))
        })?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| ModelBackendError::new("model stdin was not available"))?
            .write_all(request.prompt.as_bytes())
            .map_err(|error| {
                ModelBackendError::new(format!("could not send prompt to model: {error}"))
            })?;
        drop(child.stdin.take());

        let status = child.wait_timeout(self.timeout).map_err(|error| {
            ModelBackendError::new(format!("could not wait for model: {error}"))
        })?;
        let status = match status {
            Some(status) => status,
            None => {
                terminate_child_tree(&mut child);
                return Err(ModelBackendError::new(format!(
                    "model request for {:?} exceeded {} seconds",
                    request.role,
                    self.timeout.as_secs()
                )));
            }
        };
        let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
        if !status.success() {
            return Err(ModelBackendError::new(format!(
                "model request for {:?} failed with {status}: {}",
                request.role,
                bounded_log(&stderr)
            )));
        }

        let output = fs::read_to_string(&output_path)
            .or_else(|_| fs::read_to_string(&stdout_path))
            .map_err(|error| {
                ModelBackendError::new(format!("could not read model response: {error}"))
            })?;
        parse_json_output(&output).map_err(|error| {
            ModelBackendError::new(format!(
                "model returned invalid structured JSON for {:?}: {error}",
                request.role
            ))
        })
    }
}

fn terminate_child_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("kill")
            .args(["-TERM", "--", &process_group])
            .status();
        if child
            .wait_timeout(Duration::from_secs(2))
            .ok()
            .flatten()
            .is_none()
        {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &process_group])
                .status();
        }
        let _ = child.wait();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn parse_json_output(output: &str) -> Result<Value, serde_json::Error> {
    let trimmed = output.trim();
    if let Some(fenced) = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
    {
        serde_json::from_str(fenced.trim())
    } else {
        serde_json::from_str(trimmed)
    }
}

fn bounded_log(log: &str) -> String {
    const MAX_CHARS: usize = 4_096;
    let trimmed = log.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_owned();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(MAX_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_fenced_json() {
        assert_eq!(parse_json_output("{\"ok\":true}").unwrap()["ok"], true);
        assert_eq!(
            parse_json_output("```json\n{\"ok\":true}\n```").unwrap()["ok"],
            true
        );
    }

    #[test]
    fn bounds_model_logs_from_the_tail() {
        let log = format!("prefix{}tail", "x".repeat(5_000));
        let bounded = bounded_log(&log);
        assert!(bounded.starts_with('…'));
        assert!(bounded.ends_with("tail"));
        assert!(bounded.chars().count() <= 4_097);
    }
}
