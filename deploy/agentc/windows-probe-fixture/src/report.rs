//! Observation rows written for the probe driver.
//!
//! A row is `key=value`. The value is a fact, never a verdict:
//!
//! - `ok` / `failed:CODE` for an operation that must succeed (an own resource
//!   or a positive control),
//! - `allowed` / `error:CODE` for an attack, where `CODE` is the raw OS error,
//! - `value:TEXT` for an informational fact.

use std::io;

/// Facts gathered by one launch, in the order they were observed.
#[derive(Default)]
pub struct Report {
    rows: Vec<(String, String)>,
}

impl Report {
    /// Records an operation that must succeed.
    pub fn control(&mut self, key: &str, result: io::Result<()>) {
        let value = match result {
            Ok(()) => "ok".to_string(),
            Err(error) => format!("failed:{}", code(&error)),
        };
        self.push(key, value);
    }

    /// Records an attack: `allowed` when it worked, otherwise the raw OS error.
    pub fn attack(&mut self, key: &str, result: io::Result<()>) {
        let value = match result {
            Ok(()) => "allowed".to_string(),
            Err(error) => format!("error:{}", code(&error)),
        };
        self.push(key, value);
    }

    /// Records an informational fact on one line.
    pub fn value(&mut self, key: &str, text: &str) {
        self.push(key, format!("value:{}", single_line(text)));
    }

    /// Appends a row; a repeated key would let one observation hide another.
    fn push(&mut self, key: &str, value: String) {
        assert!(
            self.rows.iter().all(|(existing, _)| existing != key),
            "duplicate observation {key}"
        );
        self.rows.push((key.to_string(), value));
    }

    /// Renders every row as a `key=value` line.
    pub fn render(&self) -> String {
        self.rows
            .iter()
            .map(|(key, value)| format!("{key}={value}\n"))
            .collect()
    }
}

/// The raw OS error code, or the error kind when the OS gave none.
pub fn code(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => code.to_string(),
        None => format!("{:?}", error.kind()),
    }
}

/// Keeps a value on one line so it cannot forge another row.
fn single_line(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A denied operation: Windows `ERROR_ACCESS_DENIED`.
    fn denied() -> io::Result<()> {
        Err(io::Error::from_raw_os_error(5))
    }

    #[test]
    fn attacks_report_the_raw_error_or_allowed() {
        let mut report = Report::default();
        report.attack("host.file_read", denied());
        report.attack("host.file_write", Ok(()));
        assert_eq!(
            report.render(),
            "host.file_read=error:5\nhost.file_write=allowed\n"
        );
    }

    #[test]
    fn controls_report_ok_or_the_failure() {
        let mut report = Report::default();
        report.control("own.write_read", Ok(()));
        report.control("own.child", denied());
        assert_eq!(report.render(), "own.write_read=ok\nown.child=failed:5\n");
    }

    #[test]
    fn values_cannot_forge_another_row() {
        let mut report = Report::default();
        report.value("env.names", "PATH\nhost.file_read=error:5");
        assert_eq!(
            report.render(),
            "env.names=value:PATH host.file_read=error:5\n"
        );
    }

    #[test]
    #[should_panic(expected = "duplicate observation")]
    fn a_repeated_key_is_a_programming_error() {
        let mut report = Report::default();
        report.value("token.lpac", "yes");
        report.value("token.lpac", "no");
    }

    #[test]
    fn an_error_without_an_os_code_reports_its_kind() {
        let error = io::Error::new(io::ErrorKind::TimedOut, "no reply");
        assert_eq!(code(&error), "TimedOut");
    }
}
