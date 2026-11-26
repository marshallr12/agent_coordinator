//! Progress output for `adopt`.
//!
//! Human mode prints short step lines. With `--json`, every event is one JSON
//! object on its own line, so a calling program can follow the run; the final
//! `{"result": ...}` envelope is printed by `main` like any other command.

use std::io::{self, Write};

use serde_json::{Value, json};

/// The state of one step, as shown to the person and written to the event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Done,
    Skipped,
    Failed,
}

impl Status {
    /// The lowercase word used in both output forms.
    fn word(self) -> &'static str {
        match self {
            Status::Done => "done",
            Status::Skipped => "skipped",
            Status::Failed => "failed",
        }
    }
}

/// Writes step events and collects the warnings for the final report.
pub struct Reporter {
    json: bool,
    out: Box<dyn Write>,
    warnings: Vec<String>,
}

impl Reporter {
    /// Reports to standard output.
    pub fn stdout(json: bool) -> Self {
        Self::new(json, Box::new(io::stdout()))
    }

    /// Reports to any writer; tests use this to capture every byte of output.
    pub fn new(json: bool, out: Box<dyn Write>) -> Self {
        Self {
            json,
            out,
            warnings: Vec::new(),
        }
    }

    /// Records one step. `fields` must be a JSON object; it is merged into the
    /// event in JSON mode, while human mode shows only `summary`.
    pub fn event(&mut self, step: &str, status: Status, summary: &str, fields: Value) {
        if self.json {
            let mut event = json!({"step": step, "status": status.word(), "summary": summary});
            merge(&mut event, fields);
            self.line(&event.to_string());
        } else {
            self.line(&format!("{step:<10} {:<8} {summary}", status.word()));
        }
    }

    /// Shows a block of text to a person; machine readers get the same facts as event fields.
    pub fn text(&mut self, lines: &[String]) {
        if !self.json {
            for line in lines {
                self.line(line);
            }
        }
    }

    /// Records a warning: shown now, and repeated in the final report.
    pub fn warn(&mut self, step: &str, message: impl Into<String>) {
        let message = message.into();
        if self.json {
            let event = json!({"step": step, "status": "done", "warning": message});
            self.line(&event.to_string());
        } else {
            self.line(&format!("{step:<10} warning  {message}"));
        }
        self.warnings.push(message);
    }

    /// The warnings recorded so far, in order.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Writes one line; a closed output pipe must not abort the run half way.
    fn line(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
        let _ = self.out.flush();
    }
}

/// Copies the members of `fields` into `event` without replacing its own keys.
fn merge(event: &mut Value, fields: Value) {
    let (Some(target), Value::Object(source)) = (event.as_object_mut(), fields) else {
        return;
    };
    for (key, value) in source {
        target.entry(key).or_insert(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::testing::Capture;

    #[test]
    fn json_mode_writes_one_object_per_line() {
        let capture = Capture::default();
        let mut reporter = Reporter::new(true, Box::new(capture.clone()));
        reporter.event(
            "plan",
            Status::Done,
            "ready",
            json!({"files": 2, "step": "x"}),
        );
        reporter.text(&["only for people".to_owned()]);
        reporter.warn("verify", "claude is not on PATH");
        let lines: Vec<Value> = capture
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["step"], "plan");
        assert_eq!(lines[0]["status"], "done");
        assert_eq!(lines[0]["files"], 2);
        assert_eq!(lines[1]["warning"], "claude is not on PATH");
        assert_eq!(reporter.warnings(), ["claude is not on PATH"]);
    }

    #[test]
    fn human_mode_writes_step_lines_and_text() {
        let capture = Capture::default();
        let mut reporter = Reporter::new(false, Box::new(capture.clone()));
        reporter.event("commit", Status::Skipped, "nothing to commit", json!({}));
        reporter.text(&["Service  https://example.test".to_owned()]);
        let text = capture.text();
        assert!(
            text.contains("commit     skipped  nothing to commit"),
            "{text}"
        );
        assert!(text.contains("Service  https://example.test"), "{text}");
    }
}
