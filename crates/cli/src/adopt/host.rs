//! The side effects `adopt` needs from the machine, behind one trait.
//!
//! Production uses [`RealHost`]. Tests supply a scripted host, which lets the
//! whole command run in-process against a loopback stand-in for the service
//! with no real credential, terminal, browser or `PATH`.

use std::env;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::{Failure, config};

/// Everything outside the repository and the service that a run touches.
pub trait Host {
    /// Returns the credential the CLI selects for `origin`, as every other command does.
    fn token(&self, origin: &str, project_name: Option<&str>) -> anyhow::Result<String>;

    /// Describes where the shared credential file lives, for setup messages.
    fn credential_location(&self) -> String;

    /// True when prompts can be shown and answered (stdin and stderr are terminals).
    fn interactive(&self) -> bool;

    /// Shows `prompt` and returns the line the person typed, without its newline.
    fn ask(&mut self, prompt: &str) -> io::Result<String>;

    /// Finds an executable by name on `PATH`.
    fn find_program(&self, name: &str) -> Option<PathBuf>;

    /// Compares this client with the service's compatibility contract.
    fn compatibility(&self, info: &Value) -> Result<Value, Failure>;

    /// Opens `url` in the default browser; false when that could not be started.
    fn open_browser(&mut self, url: &str) -> bool;

    /// How long to wait between two looks for the new project.
    fn poll_interval(&self) -> Duration;

    /// How long to wait after a transport error while polling.
    fn backoff_interval(&self) -> Duration;
}

/// The real machine: the user's credential store, terminal, `PATH` and browser.
pub struct RealHost {
    /// Whether plain HTTP to a loopback service was explicitly permitted.
    pub allow_insecure_loopback: bool,
}

impl Host for RealHost {
    fn token(&self, origin: &str, project_name: Option<&str>) -> anyhow::Result<String> {
        config::token(origin, project_name, self.allow_insecure_loopback)
    }

    fn credential_location(&self) -> String {
        match config::credential_location(None) {
            Ok(path) => path.display().to_string(),
            Err(_) => "the agent-coordinator configuration directory".to_owned(),
        }
    }

    fn interactive(&self) -> bool {
        io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    fn ask(&mut self, prompt: &str) -> io::Result<String> {
        let mut stderr = io::stderr();
        write!(stderr, "{prompt}")?;
        stderr.flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim().to_owned())
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        let path = env::var_os("PATH")?;
        env::split_paths(&path).find_map(|directory| executable_in(&directory, name))
    }

    fn compatibility(&self, info: &Value) -> Result<Value, Failure> {
        crate::compatibility_value(info)
    }

    fn open_browser(&mut self, url: &str) -> bool {
        super::provision::open_in_browser(url)
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(3)
    }

    fn backoff_interval(&self) -> Duration {
        Duration::from_secs(10)
    }
}

/// Returns `directory/name` (trying Windows executable suffixes) when it is a file.
fn executable_in(directory: &Path, name: &str) -> Option<PathBuf> {
    let suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    suffixes
        .iter()
        .map(|suffix| directory.join(format!("{name}{suffix}")))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executables_are_found_only_as_files() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(executable_in(directory.path(), "claude"), None);
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let path = directory.path().join(format!("claude{suffix}"));
        std::fs::write(&path, "").unwrap();
        assert_eq!(executable_in(directory.path(), "claude"), Some(path));
    }
}
