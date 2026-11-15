//! A reviewer launch's candidate: a second launch with its own package in
//! which each candidate command runs, one start in a fresh job per command
//! (phase 5, step 5c).
//!
//! The candidate is prepared with the reviewer launch and then detached
//! from the launcher, so the threads that serve its pipe (step 5d) can start
//! commands while the runner keeps answering its driver. A command is the
//! script [`crate::candidate::form::candidate_script`] makes, written to a
//! file in the candidate's control directory, which the candidate can read
//! and not change; Git Bash runs that file, so no command text passes
//! through a command line. The environment is exact, nothing is inherited
//! but the log, and the job ends with the command, so nothing a command
//! started in the background outlives it.
use super::job::Job;
use super::launcher::{Detached, LaunchSpec, Launcher, ended_within};
use super::process::{self, Boundary, Command};
use super::profile::Capabilities;
use super::reader::create_directory;
use super::sddl;
use super::sys::check;
use crate::candidate::{CALL_SECONDS, STUCK_CALLS};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use windows_sys::Win32::System::Threading::GetExitCodeProcess;

/// The most bytes of a command's output and error returned to the reviewer.
pub const OUTPUT_LIMIT: usize = 1024 * 1024;
/// The status of a command stopped at the time limit, as on Linux.
pub const LIMIT_STATUS: u32 = 252;
/// The status of a command the candidate refuses to run, as on Linux.
pub const REFUSED_STATUS: u32 = 126;
/// How long a job is given to empty once its command ended.
const EMPTY_PATIENCE: Duration = Duration::from_secs(10);
/// The candidate's writable directories, below its working directory.
const WRITABLE: [&str; 5] = ["home", "tmp", "cargo", "target", "local"];

/// What the driver fixes about a reviewer launch's candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSpec {
    /// The candidate's own profile, directories and limits. It must be less
    /// privileged; its working directory holds everything it may write.
    pub launch: LaunchSpec,
    /// The source tree to create: both packages may read it, the runner's
    /// account fills it.
    pub source: PathBuf,
    /// The pinned Git Bash.
    pub bash: PathBuf,
    /// The directories of the candidate's `PATH`, in order.
    pub path: Vec<PathBuf>,
}

/// A prepared candidate, shared by the threads that run its commands.
#[derive(Debug)]
pub struct Candidate {
    boundary: Detached,
    spec: CandidateSpec,
    /// The environment every command gets, and nothing else.
    environment: BTreeMap<String, String>,
    /// Consecutive commands stopped at the time limit.
    stuck: AtomicU32,
    tuning: Tuning,
}

/// The time limit and the stuck count; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct Tuning {
    pub call_seconds: u64,
    pub stuck_calls: u32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            call_seconds: CALL_SECONDS,
            stuck_calls: STUCK_CALLS,
        }
    }
}

/// How one command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The status to give the harness.
    pub status: u32,
    /// Output and error together, at most [`OUTPUT_LIMIT`] bytes.
    pub output: Vec<u8>,
    /// Whether more output was written than is returned.
    pub truncated: bool,
    /// What the reviewer is told besides the output, if anything.
    pub note: Option<String>,
}

/// The id of `launch`'s candidate in the launcher.
pub fn candidate_id(launch: &str) -> String {
    format!("{launch}.candidate")
}

/// Prepares `launch`'s candidate: its profile, directories and desktop,
/// the writable directories, and the source tree that `reviewer` (the
/// reviewer's package SID) and the candidate may read. Nothing stays behind
/// when any step fails.
pub fn prepare(
    launcher: &mut Launcher,
    launch: &str,
    spec: CandidateSpec,
    reviewer: &str,
    tuning: Tuning,
) -> Result<Candidate> {
    ensure!(
        spec.launch.less_privileged,
        "a candidate must run less privileged"
    );
    let id = candidate_id(launch);
    let prepared = launcher.prepare(&id, spec.launch.clone())?;
    let owner = launcher.owner().to_owned();
    if let Err(error) = furnish(&spec, &owner, &[reviewer, &prepared.package_sid]) {
        // The source tree is made last, so a failure never made it: only
        // the two directories the launcher made for this candidate go.
        let _ = launcher.cleanup(&id);
        for tree in [&spec.launch.control, &spec.launch.work] {
            let _ = std::fs::remove_dir_all(tree);
        }
        return Err(error);
    }
    let boundary = launcher.detach(&id)?;
    Ok(Candidate {
        environment: environment(&spec, &system_root()),
        boundary,
        spec,
        stuck: AtomicU32::new(0),
        tuning,
    })
}

/// The writable directories, then the shared source tree, which must not
/// exist yet: it is never adopted.
fn furnish(spec: &CandidateSpec, owner: &str, packages: &[&str]) -> Result<()> {
    for name in WRITABLE {
        let directory = spec.launch.work.join(name);
        std::fs::create_dir(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
    }
    create_directory(&spec.source, &sddl::shared_source(owner, packages)?)
}

/// Removes the three trees of a prepared candidate; a missing tree is fine.
pub fn remove_trees(spec: &CandidateSpec) {
    for tree in [&spec.launch.control, &spec.launch.work, &spec.source] {
        let _ = std::fs::remove_dir_all(tree);
    }
}

/// `SystemRoot` as the runner sees it, which every Windows program needs.
fn system_root() -> String {
    std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())
}

/// The exact environment of every command: the pinned tools on `PATH`,
/// home, temp, Cargo and the local application data in the candidate's
/// working directory, and the system root. No credential variable of the
/// Linux list can be among them, since nothing is inherited.
fn environment(spec: &CandidateSpec, root: &str) -> BTreeMap<String, String> {
    let work = |name: &str| spec.launch.work.join(name).display().to_string();
    let path: Vec<String> = spec.path.iter().map(|p| p.display().to_string()).collect();
    let pairs = [
        ("PATH", path.join(";")),
        ("HOME", work("home")),
        ("TEMP", work("tmp")),
        ("TMP", work("tmp")),
        ("CARGO_HOME", work("cargo")),
        ("CARGO_TARGET_DIR", work("target")),
        ("LOCALAPPDATA", work("local")),
        ("SystemRoot", root.to_owned()),
        ("windir", root.to_owned()),
    ];
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

impl Candidate {
    /// The candidate's package SID.
    pub fn package_sid(&self) -> &str {
        self.boundary.profile.sid_text()
    }

    /// The spec it was prepared from.
    pub fn spec(&self) -> &CandidateSpec {
        &self.spec
    }

    /// The environment every command gets.
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// Runs one harness command (its eval'd text) to its end or the time
    /// limit. After too many commands in a row were stopped it refuses, as
    /// the Linux shell does.
    pub fn run(&self, command: &str) -> Result<Call> {
        if self.stuck.load(Ordering::SeqCst) >= self.tuning.stuck_calls {
            return Ok(refused(self.tuning));
        }
        let call = format!("call-{}", uuid::Uuid::new_v4().simple());
        let script = self.spec.launch.control.join(format!("{call}.sh"));
        let log = self.spec.launch.control.join(format!("{call}.log"));
        let text = crate::candidate::form::candidate_script(command);
        std::fs::write(&script, text).context("write the candidate's command file")?;
        let outcome = self.start_and_wait(&script, &log);
        let _ = std::fs::remove_file(&script);
        let call = outcome.and_then(|(status, left)| self.finish(status, left, &log));
        let _ = std::fs::remove_file(&log);
        call
    }

    /// Starts the command in a fresh job and waits for it; the status, or
    /// `None` at the time limit, and whether anything else was still alive.
    /// The job is ended before this returns.
    fn start_and_wait(&self, script: &Path, log: &Path) -> Result<(Option<u32>, bool)> {
        let job = Job::create(self.spec.launch.limits)?;
        let mut capabilities = Capabilities::named(&self.spec.launch.capabilities)?;
        let boundary = Boundary {
            profile: &self.boundary.profile,
            less_privileged: true,
            capabilities: &mut capabilities,
            job: &job,
            desktop: &self.boundary.desktop,
            passed: &[],
        };
        let running = process::start(&self.command(script, log), boundary)?;
        let millis = u32::try_from(self.tuning.call_seconds.saturating_mul(1000));
        let ended = ended_within(running.process.raw(), millis.unwrap_or(u32::MAX - 1))?;
        let status = ended
            .then(|| exit_code(running.process.raw()))
            .transpose()?;
        let left = ended && job.active().is_some_and(|alive| alive > 0);
        job.terminate(EMPTY_PATIENCE);
        Ok((status, left))
    }

    /// Git Bash running the command file from the source tree, with the
    /// exact environment and the log as its output.
    fn command(&self, script: &Path, log: &Path) -> Command {
        let file = script.display().to_string().replace('\\', "/");
        Command {
            argv: vec![
                self.spec.bash.display().to_string(),
                "--noprofile".into(),
                "--norc".into(),
                file,
            ],
            directory: self.spec.source.clone(),
            environment: self.environment.clone(),
            output: log.to_owned(),
        }
    }

    /// The call's result from its status and log, counting a stop at the
    /// time limit toward the refusal and resetting the count otherwise.
    fn finish(&self, status: Option<u32>, left: bool, log: &Path) -> Result<Call> {
        let (output, truncated) = read_bounded(log, OUTPUT_LIMIT)?;
        let Some(status) = status else {
            self.stuck.fetch_add(1, Ordering::SeqCst);
            let note = format!(
                "candidate-shell: the command ran past the time limit of {} seconds and was stopped",
                self.tuning.call_seconds
            );
            return Ok(Call {
                status: LIMIT_STATUS,
                output,
                truncated,
                note: Some(note),
            });
        };
        self.stuck.store(0, Ordering::SeqCst);
        let note = left.then(|| LEFT_RUNNING.to_owned());
        Ok(Call {
            status,
            output,
            truncated,
            note,
        })
    }

    /// Ends the candidate: its desktop closes, its package leaves the
    /// station and its profile is deleted. Its directories stay for the
    /// caller, which removes them with the launch's.
    pub fn retire(self, launcher: &mut Launcher) -> Result<()> {
        launcher.retire(self.boundary)
    }
}

/// What a reviewer is told when a command left processes behind.
pub const LEFT_RUNNING: &str = "candidate-shell: warning: processes were still running when \
     the command ended. Background jobs do not survive a candidate-shell call: they are \
     killed now and nothing they write will appear. Run long commands in the foreground, \
     with their output redirected into $TMPDIR.";

/// The refusal once `tuning.stuck_calls` commands in a row were stopped.
fn refused(tuning: Tuning) -> Call {
    let note = format!(
        "candidate-shell: refused: the last {} commands each ran past the time limit of {} \
         seconds and were stopped. This review cannot make progress; run no more commands, \
         and return your verdict now from what you have (request_changes, naming the \
         commands that could not finish).",
        tuning.stuck_calls, tuning.call_seconds
    );
    Call {
        status: REFUSED_STATUS,
        output: Vec::new(),
        truncated: false,
        note: Some(note),
    }
}

/// The exit code of an ended process.
fn exit_code(process: windows_sys::Win32::Foundation::HANDLE) -> Result<u32> {
    let mut code = 0_u32;
    // SAFETY: the process handle is live and `code` is writable.
    check(
        unsafe { GetExitCodeProcess(process, &mut code) },
        "GetExitCodeProcess",
    )?;
    Ok(code)
}

/// At most `limit` bytes of the file at `path`, and whether it held more.
fn read_bounded(path: &Path, limit: usize) -> Result<(Vec<u8>, bool)> {
    let file = std::fs::File::open(path).context("open the candidate's log")?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .context("read the candidate's log")?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_runtime::windows::job::Limits;

    /// A candidate spec under `root`.
    fn spec(root: &Path) -> CandidateSpec {
        CandidateSpec {
            launch: LaunchSpec {
                profile: "agentc.test.candidate".into(),
                less_privileged: true,
                capabilities: vec!["registryRead".into()],
                limits: Limits {
                    processes: 16,
                    process_memory_mb: 512,
                    job_memory_mb: 1024,
                },
                control: root.join("cc"),
                work: root.join("cw"),
            },
            source: root.join("src"),
            bash: r"C:\Program Files\Git\usr\bin\bash.exe".into(),
            path: vec![r"C:\Program Files\Git\usr\bin".into(), r"C:\tools".into()],
        }
    }

    #[test]
    fn the_environment_is_exact_and_holds_no_credential() {
        let spec = spec(Path::new(r"C:\launches\r1"));
        let environment = environment(&spec, r"C:\Windows");
        let names: Vec<&str> = environment.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "CARGO_HOME",
                "CARGO_TARGET_DIR",
                "HOME",
                "LOCALAPPDATA",
                "PATH",
                "SystemRoot",
                "TEMP",
                "TMP",
                "windir"
            ]
        );
        for name in crate::candidate::UNSET {
            assert!(!environment.contains_key(*name), "{name}");
        }
        assert_eq!(
            environment["PATH"],
            r"C:\Program Files\Git\usr\bin;C:\tools"
        );
        assert_eq!(environment["TEMP"], r"C:\launches\r1\cw\tmp");
        for value in environment
            .values()
            .filter(|v| v.starts_with(r"C:\launches"))
        {
            assert!(value.starts_with(r"C:\launches\r1\cw\"), "{value}");
        }
    }

    #[test]
    fn output_is_cut_at_the_limit_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        std::fs::write(&log, b"abcdef").unwrap();
        assert_eq!(read_bounded(&log, 6).unwrap(), (b"abcdef".to_vec(), false));
        assert_eq!(read_bounded(&log, 4).unwrap(), (b"abcd".to_vec(), true));
    }

    #[test]
    fn the_refusal_tells_the_reviewer_to_return_its_verdict() {
        let call = refused(Tuning::default());
        assert_eq!(call.status, REFUSED_STATUS);
        let note = call.note.unwrap();
        assert!(note.contains("refused") && note.contains("return your verdict now"));
    }

    #[test]
    fn a_candidate_id_names_its_reviewer_launch() {
        assert_eq!(candidate_id("rev-1"), "rev-1.candidate");
    }
}
