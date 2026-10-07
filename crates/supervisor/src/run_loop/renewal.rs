//! Why a renewal failed. The coordinator CLI prints its error envelope
//! (`{"error": {"code", "message"}}`) on standard output under `--json`, so
//! a failure's detail comes from there, bounded, with standard error as the
//! fallback. A refusal that means the attempt is no longer this launch's to
//! renew (it was submitted, released, expired or handed to another session)
//! becomes an [`AttemptEnded`], which [`super::lease::supervise`] answers
//! by draining the launch; every other failure is retried as before.
use serde_json::Value;
use std::fmt;
use std::process::Output;

/// The most characters of a CLI error message kept in a log line.
pub const MAX_DETAIL_CHARS: usize = 300;
/// Service error codes that end the attempt's renewals for good: the lease
/// is no longer valid (`lease_expired`, also the answer for a submitted or
/// released attempt), the attempt belongs to another session
/// (`operation_not_permitted`), or it is gone (`record_not_found`).
pub const ENDING_CODES: [&str; 3] = [
    "lease_expired",
    "operation_not_permitted",
    "record_not_found",
];

/// A failed coordinator CLI call: its exit code and error envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct CliError {
    pub exit: Option<i32>,
    /// The envelope's `error.code`; empty when the CLI printed none.
    pub code: String,
    /// The envelope's `error.message`, or standard error, bounded.
    pub message: String,
}

impl CliError {
    /// The failure a finished CLI process reports.
    pub fn of(output: &Output) -> Self {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Self::parse(output.status.code(), &output.stdout, &stderr)
    }

    /// The failure from the CLI's exit code, standard output and standard
    /// error: the JSON envelope's code and message when there is one.
    pub fn parse(exit: Option<i32>, stdout: &[u8], stderr: &str) -> Self {
        let body: Value = serde_json::from_slice(stdout).unwrap_or(Value::Null);
        let error = &body["error"];
        let text = |key: &str| error[key].as_str().unwrap_or("").to_owned();
        let message = Some(text("message")).filter(|m| !m.is_empty());
        Self {
            exit,
            code: text("code"),
            message: bounded(message.as_deref().unwrap_or(stderr)),
        }
    }

    /// Whether the service refused because the attempt is no longer this
    /// launch's to renew.
    pub fn ends_attempt(&self) -> bool {
        ENDING_CODES.contains(&self.code.as_str())
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let exit = self.exit.map_or("signal".into(), |code| code.to_string());
        let code = if self.code.is_empty() {
            "no error code"
        } else {
            &self.code
        };
        write!(f, "exit {exit}, {code}: {}", self.message)
    }
}

/// `text` on one line, at most [`MAX_DETAIL_CHARS`] characters.
pub fn bounded(text: &str) -> String {
    let line: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    match line.char_indices().nth(MAX_DETAIL_CHARS) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

/// A renewal the service refused for good: the attempt has ended or is no
/// longer owned by the launch's session.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptEnded {
    /// The service's error code (one of [`ENDING_CODES`]).
    pub code: String,
    /// The attempt's state as the task detail reports it (`submitted`,
    /// `released`, `expired`, ...), when it could be read.
    pub state: Option<String>,
}

impl AttemptEnded {
    /// Whether the agent ended the attempt by submitting its work, which
    /// the loop treats as success: nothing to release.
    pub fn submitted(&self) -> bool {
        self.state.as_deref() == Some("submitted")
    }
}

impl fmt::Display for AttemptEnded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.as_deref().unwrap_or("unknown");
        write!(
            f,
            "the attempt is no longer renewable ({}; state {state})",
            self.code
        )
    }
}

impl std::error::Error for AttemptEnded {}

/// The state of attempt `attempt` in a task detail's `attempts` history.
pub fn attempt_state(task: &Value, attempt: &str) -> Option<String> {
    let attempts = task["attempts"].as_array()?;
    let found = attempts.iter().find(|a| a["id"] == attempt)?;
    Some(found["state"].as_str()?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_envelope_code_and_message_make_the_detail() {
        let stdout = br#"{"error":{"code":"lease_expired","message":"This ownership grant is no longer valid."}}"#;
        let error = CliError::parse(Some(5), stdout, "");
        assert!(error.ends_attempt());
        let shown = error.to_string();
        assert_eq!(
            shown,
            "exit 5, lease_expired: This ownership grant is no longer valid."
        );
    }

    #[test]
    fn transient_failures_do_not_end_the_attempt() {
        let stdout = br#"{"error":{"code":"transport_failure","message":"connection refused"}}"#;
        assert!(!CliError::parse(Some(7), stdout, "").ends_attempt());
        let pending = CliError::parse(Some(2), b"", "an earlier mutation is unresolved");
        assert!(!pending.ends_attempt());
        assert_eq!(
            pending.to_string(),
            "exit 2, no error code: an earlier mutation is unresolved"
        );
    }

    #[test]
    fn details_are_bounded_to_one_line() {
        let long = format!("line\n{}", "x".repeat(1000));
        let error = CliError::parse(None, b"not json", &long);
        assert!(error.message.starts_with("line x"), "{}", error.message);
        assert_eq!(error.message.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(
            error
                .to_string()
                .starts_with("exit signal, no error code: ")
        );
    }

    #[test]
    fn the_attempt_state_comes_from_the_task_history() {
        let task = json!({"attempts": [{"id": "a2", "state": "active"}, {"id": "a1", "state": "submitted"}]});
        assert_eq!(attempt_state(&task, "a1").as_deref(), Some("submitted"));
        assert_eq!(attempt_state(&task, "a9"), None);
        let ended = AttemptEnded {
            code: "lease_expired".into(),
            state: Some("submitted".into()),
        };
        assert!(ended.submitted());
        assert!(
            !AttemptEnded {
                state: None,
                ..ended
            }
            .submitted()
        );
    }
}
