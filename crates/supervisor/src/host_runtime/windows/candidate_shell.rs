//! The shim a reviewer launch runs as its shell prefix: the client of its
//! launch's candidate pipe (phase 5, step 5d).
//!
//! It runs as the reviewer, with the reviewer's token, and decides
//! everything before anything runs ([`crate::candidate::shim::plan`]): an
//! argument that is not the recorded form, or a cwd file outside the
//! reviewer's temp, ends it with status 126 and nothing is sent. Otherwise
//! only the eval'd command goes to the pipe. The candidate's output goes to
//! standard output, what the reviewer is told besides it to standard error,
//! and the shim writes the clone's path into the harness's cwd file itself.
//!
//! A pipe whose instances are all taken is waited for, up to
//! [`Shim::wait`]; after that the call is refused and says why. A pipe the
//! shim's own account does not own is never sent a command.
use super::pipe_client::owner_sid;
use super::token;
use crate::candidate::shim::{self, Answer, REFUSED_STATUS, Reply, Request};
use crate::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;

/// How long a call waits for a free instance of the pipe by default.
pub const BUSY_WAIT: Duration = Duration::from_secs(60);
/// How often a waiting call tries the pipe again.
const RETRY: Duration = Duration::from_millis(100);
/// The longest reply line the shim accepts: the candidate's bounded output
/// as hexadecimal text, and room for the rest.
const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;

/// What the runner fixed for one reviewer launch's shim.
#[derive(Debug, Clone)]
pub struct Shim {
    /// The launch's candidate pipe.
    pub pipe: String,
    /// The reviewer's temp, in Windows form.
    pub temp: String,
    /// The reviewer's clone, in Windows form.
    pub clone: String,
    /// How long a call waits for a free instance of the pipe.
    pub wait: Duration,
}

/// Runs the harness's `arguments` through the candidate and returns the
/// status for the harness. A refusal is printed and is status 126.
pub fn run(shim: &Shim, arguments: &[String]) -> u32 {
    match call(shim, arguments) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("candidate-shell: refused: {error:#}");
            REFUSED_STATUS
        }
    }
}

/// Plans, asks the candidate, hands its answer to the harness and resets
/// the cwd file. Nothing is sent unless the plan is accepted.
fn call(shim: &Shim, arguments: &[String]) -> Result<u32> {
    let plan = shim::plan(arguments, &shim.temp, &shim.clone).map_err(|why| anyhow!(why))?;
    let request = Request {
        command: plan.command,
    };
    let answer = match ask(shim, &request)? {
        Reply::Ran(answer) => answer,
        Reply::Failed(why) => bail!("{why}"),
    };
    deliver(&answer)?;
    std::fs::write(&plan.cwd_file, &plan.cwd_text).context("write the harness cwd file")?;
    Ok(answer.status)
}

/// One request line to the launch's pipe and its reply line.
fn ask(shim: &Shim, request: &Request) -> Result<Reply> {
    let mut pipe = connect(&shim.pipe, shim.wait)?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    pipe.write_all(&line).context("send the command")?;
    let mut reply = Vec::new();
    let mut limited = BufReader::new(pipe).take(MAX_REPLY_BYTES);
    limited
        .read_until(b'\n', &mut reply)
        .context("read the candidate's answer")?;
    ensure!(
        reply.last() == Some(&b'\n'),
        "the candidate pipe closed without a whole answer"
    );
    serde_json::from_slice(&reply).context("the candidate's answer is not one the shim knows")
}

/// Opens the pipe `name`, waiting up to `wait` while every instance is
/// taken, and checks that this process's own account owns it.
fn connect(name: &str, wait: Duration) -> Result<File> {
    let path = pipe_path(name)?;
    let deadline = Instant::now() + wait;
    let pipe = loop {
        match open(&path) {
            Ok(pipe) => break pipe,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                ensure!(Instant::now() < deadline, busy(wait));
                std::thread::sleep(RETRY);
            }
            Err(error) => return Err(error).context("open the candidate pipe"),
        }
    };
    let (owner, me) = (owner_sid(&pipe)?, token::current_user_sid()?);
    ensure!(
        owner.eq_ignore_ascii_case(&me),
        "the candidate pipe is owned by {owner}, not by this launch's account; it is not used"
    );
    Ok(pipe)
}

/// Why a call that waited `wait` for a free instance is refused.
fn busy(wait: Duration) -> String {
    format!(
        "every command slot of this review was still taken after {} seconds. Wait for a \
         running command to end, or stop a background one, and run the command again.",
        wait.as_secs()
    )
}

/// Opens the pipe at `path` with a client's rights; the server may identify
/// this process on the connection and may not act as it.
fn open(path: &str) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .access_mode(CLIENT_ACCESS)
        .security_qos_flags(SECURITY_IDENTIFICATION);
    options.open(path)
}

/// The candidate's output to standard output, and what the reviewer is told
/// besides it to standard error.
fn deliver(answer: &Answer) -> Result<()> {
    let output = hex::decode(&answer.output).context("the candidate's output is not hex")?;
    let mut out = std::io::stdout().lock();
    out.write_all(&output).context("write the output")?;
    out.flush().context("write the output")?;
    if answer.truncated {
        eprintln!(
            "candidate-shell: the output was longer than {} bytes and was cut there",
            output.len()
        );
    }
    if let Some(note) = &answer.note {
        eprintln!("{note}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::form;

    /// A shim for a pipe nobody serves.
    fn shim() -> Shim {
        Shim {
            pipe: format!("agentc-test-shell-{}", uuid::Uuid::new_v4().simple()),
            temp: r"C:\launches\rev\work\tmp".into(),
            clone: r"C:\launches\rev\source".into(),
            wait: Duration::from_millis(200),
        }
    }

    #[test]
    fn a_refused_argument_is_status_126_and_reaches_no_pipe() {
        // The pipe does not exist; a refusal must come before it is opened,
        // and it does whether the pipe exists or not.
        let shim = shim();
        assert_eq!(run(&shim, &["echo hook".into()]), REFUSED_STATUS);
        assert_eq!(run(&shim, &[]), REFUSED_STATUS);
        let refused = call(&shim, &["echo hook".into()]).unwrap_err();
        assert_eq!(refused.to_string(), form::NOT_THE_FORM);
    }

    #[test]
    fn an_accepted_argument_with_no_pipe_is_refused_and_writes_no_cwd_file() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().display().to_string();
        let mut shim = shim();
        shim.temp = temp.clone();
        let cwd = format!("{}/claude-1a2b-cwd", form::git_bash_path(&temp).unwrap());
        let argument = form::recorded("/c/s.sh", &temp, "'true'", &cwd);
        let refused = call(&shim, &[argument]).unwrap_err();
        assert!(format!("{refused:#}").contains("open the candidate pipe"));
        assert!(!dir.path().join("claude-1a2b-cwd").exists());
    }

    #[test]
    fn a_pipe_with_every_instance_taken_is_waited_for_and_then_refused() {
        use super::super::pipe::Pipe;
        let me = token::current_user_sid().unwrap();
        let name = shim().pipe;
        let package = "S-1-15-2-1-2-3-4-5-6-7";
        let _pipes = Pipe::package_instances(&name, &me, package, 1, None).unwrap();
        let _held = open(&pipe_path(&name).unwrap()).unwrap();
        let started = Instant::now();
        let refused = connect(&name, Duration::from_millis(300)).unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(
            refused.to_string().contains("every command slot"),
            "{refused}"
        );
    }
}
