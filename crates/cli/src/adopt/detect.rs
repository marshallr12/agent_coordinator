//! Step 2: gather defaults from the repository so the interview asks only
//! what could not be detected. Reads only; a flag always wins over detection.

use std::path::Path;

use super::host::Host;
use super::preflight::Preflight;
use super::report::Reporter;
use super::service::Project;
use super::{HarnessArg, Harnesses, Options, files, git, repo_url};

/// The defaults found in the repository and on the service.
#[derive(Clone, Debug)]
pub struct Detected {
    /// The repository URL, already stripped of anything secret; `None` when there is no remote.
    pub repository_url: Option<String>,
    pub branch: String,
    pub name: Option<String>,
    /// Existing projects whose repository URL names this repository.
    pub matches: Vec<Project>,
    /// The assistants in use here; `None` when nothing indicates either.
    pub harnesses: Option<Harnesses>,
}

/// Detects every default, warning when a secret was stripped from the remote URL.
pub fn run(
    pre: &Preflight,
    options: &Options,
    host: &dyn Host,
    reporter: &mut Reporter,
) -> Detected {
    let repository_url = repository_url(&pre.root, options, reporter);
    let matches = matching_projects(&pre.projects, repository_url.as_deref());
    Detected {
        branch: branch(&pre.root, options),
        name: name(&pre.root, options, repository_url.as_deref()),
        harnesses: harnesses(&pre.root, options.args.harness, host),
        repository_url,
        matches,
    }
}

/// The repository URL from `--repository-url`, else `origin`, else the sole remote.
fn repository_url(root: &Path, options: &Options, reporter: &mut Reporter) -> Option<String> {
    let raw = options
        .args
        .repository_url
        .clone()
        .or_else(|| git::remote_url(root))?;
    let cleaned = repo_url::sanitize(&raw);
    if cleaned.changed {
        reporter.warn(
            "detect",
            "Sign-in details, query or fragment were removed from the repository URL before use.",
        );
    }
    (!cleaned.url.is_empty()).then_some(cleaned.url)
}

/// The target branch from `--branch`, else the remote default, else the current branch, else `main`.
fn branch(root: &Path, options: &Options) -> String {
    options
        .args
        .branch
        .clone()
        .or_else(|| git::default_branch(root))
        .unwrap_or_else(|| "main".to_owned())
}

/// The project name from `--name`, else the URL's last segment, else the directory name.
fn name(root: &Path, options: &Options, url: Option<&str>) -> Option<String> {
    let directory = || {
        root.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    };
    options
        .args
        .name
        .clone()
        .or_else(|| url.and_then(repo_url::default_name))
        .or_else(directory)
}

/// Finds the existing projects that already name this repository.
fn matching_projects(projects: &[Project], url: Option<&str>) -> Vec<Project> {
    let Some(wanted) = url.map(repo_url::normalize) else {
        return Vec::new();
    };
    projects
        .iter()
        .filter(|project| repo_url::normalize(&project.repository_url) == wanted)
        .cloned()
        .collect()
}

/// Decides which assistants work here: the flag, else what is installed or already configured.
fn harnesses(root: &Path, flag: HarnessArg, host: &dyn Host) -> Option<Harnesses> {
    let pick = |claude, codex| Some(Harnesses { claude, codex });
    match flag {
        HarnessArg::Claude => return pick(true, false),
        HarnessArg::Codex => return pick(false, true),
        HarnessArg::Both => return pick(true, true),
        HarnessArg::Auto => {}
    }
    let present = |name: &str| root.join(name).exists();
    let claude =
        host.find_program("claude").is_some() || present(files::CLAUDE_FILE) || present(".claude");
    let codex =
        host.find_program("codex").is_some() || present(files::AGENTS_FILE) || present(".codex");
    (claude || codex).then_some(Harnesses { claude, codex })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::AdoptArgs;
    use crate::adopt::testing::{TestHost, TestRepo};

    /// Options for a run with the given flags.
    fn options(args: AdoptArgs) -> Options {
        Options {
            args,
            allow_insecure_loopback: false,
            start: ".".into(),
        }
    }

    /// A project with the given ID and repository URL.
    fn project(id: &str, url: &str) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            repository_url: url.into(),
            target_branch: "main".into(),
        }
    }

    #[test]
    fn projects_match_by_normalised_repository_url() {
        let projects = [
            project("a", "git@github.com:acme/billing.git"),
            project("b", "https://github.com/acme/other"),
            project("c", "https://GitHub.com/acme/billing/"),
        ];
        let found = matching_projects(&projects, Some("https://github.com/acme/billing.git"));
        let ids: Vec<&str> = found.iter().map(|project| project.id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
        assert!(matching_projects(&projects, None).is_empty());
    }

    #[test]
    fn name_and_branch_fall_back_in_order() {
        let repo = TestRepo::new();
        let bare = options(AdoptArgs::default());
        assert_eq!(
            name(
                repo.root(),
                &bare,
                Some("https://example.com/acme/billing.git")
            )
            .as_deref(),
            Some("billing")
        );
        let directory = repo
            .root()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(name(repo.root(), &bare, None), Some(directory));
        assert_eq!(branch(repo.root(), &bare), "main");
        let flagged = options(AdoptArgs {
            name: Some("Atlas".into()),
            branch: Some("trunk".into()),
            ..AdoptArgs::default()
        });
        assert_eq!(name(repo.root(), &flagged, None).as_deref(), Some("Atlas"));
        assert_eq!(branch(repo.root(), &flagged), "trunk");
    }

    #[test]
    fn harnesses_come_from_the_flag_or_from_signals() {
        let repo = TestRepo::new();
        let mut host = TestHost::new("token");
        assert_eq!(harnesses(repo.root(), HarnessArg::Auto, &host), None);
        assert_eq!(
            harnesses(repo.root(), HarnessArg::Codex, &host),
            Some(Harnesses {
                claude: false,
                codex: true
            })
        );
        host.programs.push("claude".into());
        assert_eq!(
            harnesses(repo.root(), HarnessArg::Auto, &host),
            Some(Harnesses {
                claude: true,
                codex: false
            })
        );
        repo.write(files::AGENTS_FILE, "Rules.\n");
        assert_eq!(
            harnesses(repo.root(), HarnessArg::Auto, &host),
            Some(Harnesses {
                claude: true,
                codex: true
            })
        );
    }
}
