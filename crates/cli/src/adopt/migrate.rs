//! Step 9: offer to migrate `HANDOFF.md` / `BACKLOG.md` into coordinator
//! records by starting an **interactive** Claude Code or Codex session.
//!
//! `adopt` itself creates no records. It materialises the migration kit (the
//! procedure, its mapping rules and a Python helper, compiled into this
//! binary) into a private directory and starts the assistant with a fixed
//! prompt. The person at the terminal approves the plan inside that session,
//! under the assistant's normal permission prompts: no permission-bypass
//! option is ever passed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use tempfile::NamedTempFile;

use super::host::Host;
use super::report::{Reporter, Status};
use super::{AgentArg, Harnesses, MigrateArg, Options, interview};
use crate::state;

/// The line in a migrated `HANDOFF.md` stub that marks the repository as already migrated.
pub const MIGRATED_MARKER: &str = "<!-- agent-coordinator:migrated -->";
/// Where migrated files are archived; its presence also marks a migrated repository.
const ARCHIVE_DIRECTORY: &str = "docs/pre-coordinator";
/// The command that starts the migration at any later time.
const LATER: &str = "agent-coordinator adopt --migrate now";
/// The environment variable that names the CLI session the migration uses.
pub const SESSION_VARIABLE: &str = "AGENT_COORDINATOR_SESSION";
/// The kit, compiled in so the installed CLI needs no other file.
const KIT: [(&str, &str); 3] = [
    (
        "PROCEDURE.md",
        include_str!("../../assets/migrate/PROCEDURE.md"),
    ),
    (
        "mapping.md",
        include_str!("../../assets/migrate/mapping.md"),
    ),
    (
        "migrate.py",
        include_str!("../../assets/migrate/migrate.py"),
    ),
];

/// A Markdown file that holds project memory to migrate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub bytes: u64,
}

/// A program to start in the foreground: what the migration step hands to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The repository root.
    pub cwd: PathBuf,
    /// Variables added to the inherited environment (nothing is removed).
    pub env: Vec<(String, String)>,
}

/// Finds `HANDOFF.md` and `BACKLOG.md` at the repository root, in any letter case.
///
/// A repository that has the archive directory, or whose handoff file carries
/// the migrated marker, is already migrated and has no sources.
pub fn sources(root: &Path) -> Vec<Source> {
    if root.join(ARCHIVE_DIRECTORY).is_dir() {
        return Vec::new();
    }
    let entries = std::fs::read_dir(root).into_iter().flatten().flatten();
    let mut found: Vec<Source> = entries
        .filter_map(|entry| source_at(&entry.path()))
        .collect();
    found.sort_by_key(|source| std::cmp::Reverse(source.name.to_ascii_lowercase()));
    let migrated = found
        .iter()
        .any(|source| has_marker(&root.join(&source.name)));
    if migrated { Vec::new() } else { found }
}

/// Returns the source at `path` when it is a regular file named like a memory file.
fn source_at(path: &Path) -> Option<Source> {
    let name = path.file_name()?.to_str()?.to_owned();
    let wanted = matches!(
        name.to_ascii_lowercase().as_str(),
        "handoff.md" | "backlog.md"
    );
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let bytes = metadata.len();
    (wanted && metadata.is_file()).then_some(Source { name, bytes })
}

/// True when the file is the stub a finished migration leaves behind.
fn has_marker(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|content| content.contains(MIGRATED_MARKER))
}

/// Lists sources with their sizes, e.g. `HANDOFF.md (4 KiB), BACKLOG.md (9 KiB)`.
pub fn describe(sources: &[Source]) -> String {
    let item = |source: &Source| {
        format!(
            "{} ({} KiB)",
            source.name,
            source.bytes.div_ceil(1024).max(1)
        )
    };
    sources.iter().map(item).collect::<Vec<_>>().join(", ")
}

/// The plan's "Migration" line, or `None` when there is nothing to migrate.
pub fn plan_line(root: &Path, options: &Options) -> Option<String> {
    let found = sources(root);
    let listed = describe(&found);
    match options.args.migrate {
        _ if found.is_empty() => None,
        MigrateArg::Off => Some("off (--migrate off)".to_owned()),
        MigrateArg::Now => Some(format!("starts afterwards: {listed}")),
        MigrateArg::Ask => Some(format!("offered afterwards: {listed}")),
    }
}

/// Runs the migration step and returns its summary for the final report.
///
/// Whatever happens here never changes adopt's exit code: the repository is
/// already connected.
pub fn run(
    root: &Path,
    harnesses: Harnesses,
    options: &Options,
    host: &mut dyn Host,
    reporter: &mut Reporter,
) -> Value {
    match prepare(root, harnesses, options, host, reporter) {
        Ok(launch) => start(&launch, host, reporter),
        Err(reason) => skipped(&reason, reporter),
    }
}

/// Decides whether to start a session and builds its command; `Err` is the reason for skipping.
fn prepare(
    root: &Path,
    harnesses: Harnesses,
    options: &Options,
    host: &mut dyn Host,
    reporter: &mut Reporter,
) -> Result<Launch, String> {
    let found = sources(root);
    let (agent, program) = ready_to_offer(&found, harnesses, options, host)?;
    if options.args.migrate == MigrateArg::Ask && !offer_accepted(&found, host)? {
        return Err("declined".to_owned());
    }
    let unwritable = |error| format!("the kit could not be written ({error:#})");
    let kit = materialize_kit(host).map_err(unwritable)?;
    let prompt = prompt(&kit);
    let help = host.help_text(&program).unwrap_or_default();
    if !accepts_prompt(&help) {
        reporter.text(&paste_instructions(agent, &prompt));
        return Err(format!("{agent} takes no prompt on its command line here"));
    }
    let args = arguments(agent, &help, &kit, prompt);
    let (cwd, env) = (root.to_path_buf(), session_environment(host));
    Ok(Launch {
        program,
        args,
        cwd,
        env,
    })
}

/// Asks the last interview question, listing the files and their sizes.
fn offer_accepted(found: &[Source], host: &mut dyn Host) -> Result<bool, String> {
    let question = format!("Start a session now to migrate {}?", describe(found));
    interview::ask_yes_no(host, &question, true).map_err(|_| "no answer was given".to_owned())
}

/// Checks every precondition for offering a session; returns the assistant and its executable.
fn ready_to_offer(
    found: &[Source],
    harnesses: Harnesses,
    options: &Options,
    host: &dyn Host,
) -> Result<(&'static str, PathBuf), String> {
    if found.is_empty() {
        return Err("no HANDOFF.md or BACKLOG.md to migrate (or already migrated)".to_owned());
    }
    if options.args.migrate == MigrateArg::Off {
        return Err("--migrate off".to_owned());
    }
    if !options.prompting(host) {
        return Err("the migration needs a terminal and a person".to_owned());
    }
    if host.find_program("python3").is_none() {
        return Err("python3 is not on PATH".to_owned());
    }
    let agent = agent_name(options.args.migration_agent, harnesses, host);
    let program = host
        .find_program(agent)
        .ok_or(format!("{agent} is not on PATH"))?;
    Ok((agent, program))
}

/// Picks the assistant: `--migration-agent`, else the first selected one that is installed.
fn agent_name(flag: Option<AgentArg>, harnesses: Harnesses, host: &dyn Host) -> &'static str {
    match flag {
        Some(AgentArg::Claude) => return "claude",
        Some(AgentArg::Codex) => return "codex",
        None => {}
    }
    let selected = [("claude", harnesses.claude), ("codex", harnesses.codex)];
    let wanted = selected
        .iter()
        .filter(|(_, wanted)| *wanted)
        .map(|(name, _)| *name);
    let installed = wanted
        .clone()
        .find(|name| host.find_program(name).is_some());
    installed.or(wanted.clone().next()).unwrap_or("claude")
}

/// The fixed prompt the session starts with. It names files; it carries no instructions of its own.
pub fn prompt(kit: &Path) -> String {
    let kit = kit.display();
    format!(
        "Read `{kit}/PROCEDURE.md` and follow it exactly to migrate this repository's Markdown \
         project memory to Agent Coordinator. The helper is `python3 {kit}/migrate.py`. Do not \
         claim, checkpoint or submit work during the migration."
    )
}

/// True when the assistant's help shows a positional prompt (`[prompt]` or `[PROMPT]`).
fn accepts_prompt(help: &str) -> bool {
    help.to_ascii_lowercase().contains("[prompt]")
}

/// What to tell the person when the session cannot be started with the prompt.
fn paste_instructions(agent: &str, prompt: &str) -> Vec<String> {
    vec![
        format!("Start {agent} in this repository yourself and paste this prompt:"),
        format!("  {prompt}"),
    ]
}

/// Builds the arguments: the prompt, then only options the installed assistant's help lists.
///
/// The prompt comes before `--add-dir` because that option takes a list of
/// directories and would otherwise consume the prompt as one of them. Codex
/// is started with the prompt alone.
fn arguments(agent: &str, help: &str, kit: &Path, prompt: String) -> Vec<String> {
    let mut args = vec![prompt];
    if agent == "claude" && help.contains("--add-dir") {
        args.extend(["--add-dir".to_owned(), kit.display().to_string()]);
    }
    args
}

/// The one variable added for the session: a fresh session name, unless one is already set.
fn session_environment(host: &dyn Host) -> Vec<(String, String)> {
    match host.session_name() {
        Some(_) => Vec::new(),
        None => vec![(
            SESSION_VARIABLE.to_owned(),
            format!("adopt-migrate-{}", host.random_suffix()),
        )],
    }
}

/// Starts the session in the foreground and reports how it ended, as information only.
fn start(launch: &Launch, host: &mut dyn Host, reporter: &mut Reporter) -> Value {
    let program = launch.program.display().to_string();
    let starting = format!("starting {program}; the session takes over the terminal");
    reporter.event("migrate", Status::Waiting, &starting, json!({}));
    let code = match host.launch(launch) {
        Ok(code) => code,
        Err(error) => {
            return skipped(
                &format!("{program} could not be started ({error})"),
                reporter,
            );
        }
    };
    let ending = code.map_or("by a signal".to_owned(), |code| {
        format!("exit status {code}")
    });
    let fields = json!({"exit_status": code});
    reporter.event(
        "migrate",
        Status::Done,
        &format!("the session ended ({ending})"),
        fields,
    );
    json!({"status": "finished", "exit_status": code})
}

/// Reports a skipped migration with the command that starts it later.
fn skipped(reason: &str, reporter: &mut Reporter) -> Value {
    let summary = format!("{reason}. Start it later with: {LATER}");
    reporter.event(
        "migrate",
        Status::Skipped,
        &summary,
        json!({"later": LATER}),
    );
    json!({"status": "skipped", "reason": reason, "later": LATER})
}

/// Writes the kit into `<state home>/adopt/migrate-kit-<client commit>/` and returns that directory.
///
/// The directory is outside every repository and private to the user. Files
/// that already hold the same bytes are left alone, so a rerun is a no-op.
pub fn materialize_kit(host: &dyn Host) -> anyhow::Result<PathBuf> {
    let commit: String = coordinator_core::BUILD_SOURCE_COMMIT
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let directory = host
        .state_home()?
        .join("adopt")
        .join(format!("migrate-kit-{commit}"));
    std::fs::create_dir_all(&directory)?;
    state::protect_directory(&directory)?;
    for (name, content) in KIT {
        let path = directory.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(content.as_bytes()) {
            write_private(&directory, &path, content)?;
        }
    }
    Ok(directory)
}

/// Writes one kit file atomically with owner-only access.
fn write_private(directory: &Path, path: &Path, content: &str) -> anyhow::Result<()> {
    let mut temporary = NamedTempFile::new_in(directory)?;
    temporary.write_all(content.as_bytes())?;
    state::protect_file(temporary.path())?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Runs `<program> --help` and returns everything it printed, to see which options exist.
pub fn help_of(program: &Path) -> Option<String> {
    let output = Command::new(program)
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text)
}

/// Starts the program as a foreground child on the inherited terminal and waits for it.
pub fn run_foreground(launch: &Launch) -> std::io::Result<Option<i32>> {
    let mut command = Command::new(&launch.program);
    command.args(&launch.args).current_dir(&launch.cwd);
    command.envs(launch.env.iter().map(|(name, value)| (name, value)));
    Ok(command.status()?.code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::testing::{TestHost, TestRepo};

    #[test]
    fn sources_are_root_files_in_any_letter_case() {
        let repo = TestRepo::new();
        assert!(sources(repo.root()).is_empty());
        repo.write("HANDOFF.md", &"h".repeat(4000));
        repo.write("Backlog.md", &"b".repeat(9000));
        repo.write("docs/HANDOFF.md", "not at the root\n");
        repo.write("NOTES.md", "not a source\n");
        let found = sources(repo.root());
        assert_eq!(describe(&found), "HANDOFF.md (4 KiB), Backlog.md (9 KiB)");
    }

    #[test]
    fn a_migrated_repository_has_no_sources() {
        let archived = TestRepo::new();
        archived.write("BACKLOG.md", "left behind\n");
        archived.write("docs/pre-coordinator/HANDOFF.md", "old\n");
        assert!(sources(archived.root()).is_empty());
        let stubbed = TestRepo::new();
        stubbed.write(
            "HANDOFF.md",
            &format!("# Project handoff\n\n{MIGRATED_MARKER}\n\nState moved.\n"),
        );
        stubbed.write("BACKLOG.md", "also ignored once the stub is there\n");
        assert!(sources(stubbed.root()).is_empty());
    }

    #[test]
    fn the_prompt_names_the_kit_and_forbids_the_work_loop() {
        let text = prompt(Path::new("/state/adopt/migrate-kit-abc"));
        assert_eq!(
            text,
            "Read `/state/adopt/migrate-kit-abc/PROCEDURE.md` and follow it exactly to migrate this \
             repository's Markdown project memory to Agent Coordinator. The helper is `python3 \
             /state/adopt/migrate-kit-abc/migrate.py`. Do not claim, checkpoint or submit work during \
             the migration."
        );
    }

    #[test]
    fn claude_gets_the_prompt_first_and_add_dir_only_when_its_help_lists_it() {
        let kit = Path::new("/kit");
        let listed = "[prompt]\n --add-dir <directories...>";
        assert_eq!(
            arguments("claude", listed, kit, "PROMPT".into()),
            ["PROMPT", "--add-dir", "/kit"]
        );
        assert_eq!(
            arguments("claude", "[prompt]", kit, "PROMPT".into()),
            ["PROMPT"]
        );
        let codex = arguments("codex", "[PROMPT] --add-dir <DIR>", kit, "PROMPT".into());
        assert_eq!(codex, ["PROMPT"], "codex is started with the prompt alone");
    }

    #[test]
    fn a_session_name_is_provided_unless_one_is_already_set() {
        let mut host = TestHost::new("token");
        let provided = [(
            SESSION_VARIABLE.to_owned(),
            "adopt-migrate-0123abcd".to_owned(),
        )];
        assert_eq!(session_environment(&host), provided);
        host.session = Some("my-session".into());
        assert!(
            session_environment(&host).is_empty(),
            "the inherited variable is left as it is"
        );
    }

    #[test]
    fn the_assistant_is_the_flag_else_the_first_installed_selection() {
        let mut host = TestHost::new("token");
        host.programs.push("codex".into());
        assert_eq!(agent_name(None, Harnesses::BOTH, &host), "codex");
        assert_eq!(
            agent_name(Some(AgentArg::Claude), Harnesses::BOTH, &host),
            "claude"
        );
        host.programs.push("claude".into());
        assert_eq!(agent_name(None, Harnesses::BOTH, &host), "claude");
        assert_eq!(
            agent_name(None, Harnesses::new(false, true), &host),
            "codex"
        );
    }

    #[test]
    fn the_kit_is_private_complete_and_rewritten_only_when_it_differs() {
        let host = TestHost::new("token");
        let kit = materialize_kit(&host).unwrap();
        assert!(kit.starts_with(host.state.path().join("adopt")));
        assert!(
            kit.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("migrate-kit-")
        );
        for (name, content) in KIT {
            assert_eq!(std::fs::read_to_string(kit.join(name)).unwrap(), content);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&kit), 0o700);
            assert_eq!(mode(&kit.join("migrate.py")), 0o600);
        }
        let stamp = |name: &str| {
            std::fs::metadata(kit.join(name))
                .unwrap()
                .modified()
                .unwrap()
        };
        let before = stamp("PROCEDURE.md");
        std::fs::write(kit.join("mapping.md"), "tampered").unwrap();
        materialize_kit(&host).unwrap();
        assert_eq!(
            stamp("PROCEDURE.md"),
            before,
            "an unchanged file is not rewritten"
        );
        assert_eq!(
            std::fs::read_to_string(kit.join("mapping.md")).unwrap(),
            KIT[1].1
        );
    }

    #[test]
    fn help_must_show_a_positional_prompt() {
        assert!(accepts_prompt("Usage: claude [options] [command] [prompt]"));
        assert!(accepts_prompt("Usage: codex [OPTIONS] [PROMPT]"));
        assert!(!accepts_prompt("Usage: other [OPTIONS]"));
        assert!(!accepts_prompt(""));
    }
}
