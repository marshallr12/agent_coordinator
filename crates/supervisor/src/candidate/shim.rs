//! What the Windows shim decides before anything runs, and what it and the
//! candidate pipe say to each other (phase 5, step 5d).
//!
//! The shim is the reviewer's `CLAUDE_CODE_SHELL_PREFIX`. It gets the
//! harness's one argument, keeps only the command in it
//! ([`super::form::parse`]), sends that to the launch's candidate pipe as
//! one [`Request`] line and gets one [`Reply`] line back. It owns the
//! harness's cwd file: it writes the clone's path there itself, so a `cd` in
//! one command never carries to the next, as on Linux. Nothing here touches
//! the operating system, so every platform tests it.
use super::form::{self, single_quoted};
use serde::{Deserialize, Serialize};

/// The status of a command the shim refuses, as on Linux.
pub const REFUSED_STATUS: u32 = 126;
/// The file in a reviewer launch's control directory that the harness is
/// given as its shell prefix.
pub const SCRIPT_NAME: &str = "candidate-shell";
/// Why more or fewer arguments than one are refused.
pub const NOT_ONE_ARGUMENT: &str = "the harness passes its prefix exactly one argument";
/// The clone's path cannot be written as the harness reads it.
pub const CLONE_NOT_PLAIN: &str = "the clone is not a plain path on a drive";

/// One command for the candidate: the harness's eval'd text and nothing of
/// the reviewer's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub command: String,
}

/// How a command ended, as the pipe reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    /// The status to give the harness.
    pub status: u32,
    /// The command's output and error together, as hexadecimal text.
    pub output: String,
    /// Whether more was written than `output` holds.
    pub truncated: bool,
    /// What the reviewer is told besides the output, if anything.
    pub note: Option<String>,
}

/// The pipe's one line back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    /// The command ran, or the candidate refused it with a status.
    Ran(Answer),
    /// Nothing ran, and why.
    Failed(String),
}

/// What the shim does with one accepted argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The only text that reaches the candidate.
    pub command: String,
    /// The harness's cwd file, in Windows form.
    pub cwd_file: String,
    /// What the shim writes into it: the clone as the harness reads it.
    pub cwd_text: String,
}

/// The plan for the harness's `arguments`, or why they are refused. `temp`
/// is the reviewer's temp and `clone` its clone, both in Windows form.
pub fn plan(arguments: &[String], temp: &str, clone: &str) -> Result<Plan, &'static str> {
    let [argument] = arguments else {
        return Err(NOT_ONE_ARGUMENT);
    };
    let wrapped = form::parse(argument)?;
    let cwd_file = form::cwd_file(&wrapped, temp)?;
    let clone = form::git_bash_path(clone).ok_or(CLONE_NOT_PLAIN)?;
    Ok(Plan {
        command: wrapped.command,
        cwd_file,
        cwd_text: format!("{clone}\n"),
    })
}

/// The text of a reviewer launch's prefix script: it hands the harness's
/// arguments, unchanged, to `program`'s shim subcommand for the pipe
/// `pipe`. The shell that runs it knows `C:/a/b` paths and not `/c/a/b`.
pub fn script(program: &str, pipe: &str, temp: &str, clone: &str) -> String {
    let program = single_quoted(&program.replace('\\', "/"));
    let (pipe, temp, clone) = (
        single_quoted(pipe),
        single_quoted(temp),
        single_quoted(clone),
    );
    format!(
        "#!/bin/sh\nexec {program} windows-candidate-shell --pipe {pipe} --temp {temp} \
         --clone {clone} -- \"$@\"\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMP: &str = r"C:\launches\rev\work\tmp";
    const CLONE: &str = r"C:\launches\rev\source";
    const CWD: &str = "/c/launches/rev/work/tmp/claude-1a2b-cwd";

    /// The harness's argument for `command`, already one quoted word.
    fn argument(command: &str) -> Vec<String> {
        let snapshot = "/c/Users/role/.claude/shell-snapshots/snapshot-bash-1-x.sh";
        vec![form::recorded(snapshot, TEMP, command, CWD)]
    }

    #[test]
    fn the_plan_keeps_the_command_and_owns_the_cwd_file() {
        let plan = plan(&argument("'cd .. && cargo test'"), TEMP, CLONE).unwrap();
        assert_eq!(plan.command, "cd .. && cargo test");
        assert_eq!(plan.cwd_file, format!(r"{TEMP}\claude-1a2b-cwd"));
        assert_eq!(plan.cwd_text, "/c/launches/rev/source\n");
    }

    #[test]
    fn anything_but_one_recorded_argument_is_refused() {
        let good = argument("'true'");
        let two = [good.clone(), vec!["extra".to_owned()]].concat();
        assert_eq!(plan(&[], TEMP, CLONE), Err(NOT_ONE_ARGUMENT));
        assert_eq!(plan(&two, TEMP, CLONE), Err(NOT_ONE_ARGUMENT));
        let hook = vec!["echo hook".to_owned()];
        assert_eq!(plan(&hook, TEMP, CLONE), Err(form::NOT_THE_FORM));
        let elsewhere = r"C:\launches\other\work\tmp";
        assert_eq!(plan(&good, elsewhere, CLONE), Err(form::CWD_OUTSIDE));
        assert_eq!(plan(&good, TEMP, r"\\server\share"), Err(CLONE_NOT_PLAIN));
    }

    #[test]
    fn the_wire_lines_are_exact_and_refuse_unknown_fields() {
        let request = Request {
            command: "echo 'a'".into(),
        };
        let line = serde_json::to_string(&request).unwrap();
        assert_eq!(line, r#"{"command":"echo 'a'"}"#);
        let extra = r#"{"command":"true","directory":"C:\\"}"#;
        assert!(serde_json::from_str::<Request>(extra).is_err());
        let failed: Reply = serde_json::from_str(r#"{"failed":"why"}"#).unwrap();
        assert_eq!(failed, Reply::Failed("why".into()));
        let ran = r#"{"ran":{"status":3,"output":"6f","truncated":false,"note":null}}"#;
        let Reply::Ran(answer) = serde_json::from_str(ran).unwrap() else {
            panic!("not an answer");
        };
        assert_eq!((answer.status, answer.output.as_str()), (3, "6f"));
    }

    #[test]
    fn the_script_passes_the_arguments_on_unchanged() {
        let text = script(
            r"C:\tools\it's\agentc-supervisor.exe",
            "agentc-candidate-r1",
            TEMP,
            CLONE,
        );
        let wanted = "#!/bin/sh\nexec 'C:/tools/it'\\''s/agentc-supervisor.exe' \
             windows-candidate-shell --pipe 'agentc-candidate-r1' \
             --temp 'C:\\launches\\rev\\work\\tmp' --clone 'C:\\launches\\rev\\source' -- \"$@\"\n";
        assert_eq!(text, wanted);
    }
}
