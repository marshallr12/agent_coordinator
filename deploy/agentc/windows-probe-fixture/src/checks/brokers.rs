//! What a launch reaches of the host's egress and publication brokers and
//! of the files only they may use (Windows host phase 6 follow-up).
//!
//! The launch first proves that its own reads work: a file and a directory
//! the spec names as controls must be readable from inside. Only then do
//! the refusals mean anything: the push key, the publication broker's work
//! directory and, after a push, the launch's own repository there must each
//! be refused. A broker the launch was admitted to is reached over the
//! pipe its environment names; without that variable the row says so.

use super::files::{list, read};
use crate::report::Report;
use crate::spec::Spec;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

/// Names the launch's pipe to the egress broker.
const EGRESS_PIPE: &str = "AGENTC_EGRESS_PIPE";
/// Names the launch's pipe to the publication broker.
const PUBLICATION_PIPE: &str = "AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET";
/// What a broker pipe's one client may ask for: read and write data, read
/// attributes and the security descriptor, synchronize. Never
/// `FILE_CREATE_PIPE_INSTANCE`, which generic write access would include.
const CLIENT_ACCESS: u32 = 0x12_0083;
/// Lets the broker learn who the client is and nothing more.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
/// The longest answer line read from a broker.
const LINE_LIMIT: usize = 4096;

/// Records the controls, the refusals, each egress request and the push.
pub fn run(spec: &Spec, launch: &str, report: &mut Report) -> Result<(), String> {
    controls(spec, report)?;
    refusals(spec, report);
    for (name, target) in spec.group("egress") {
        report.value(&format!("egress.{name}"), &said(connect(target)));
    }
    publication(spec, launch, report);
    Ok(())
}

/// The positive controls: reads of the same kinds that must work in here.
fn controls(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let file = Path::new(spec.require("control_file")?);
    report.control("control.file_read", read(file));
    let directory = Path::new(spec.require("control_dir")?);
    report.control("control.dir_list", list(directory));
    Ok(())
}

/// Every file and directory the spec says the launch must not reach.
fn refusals(spec: &Spec, report: &mut Report) {
    for (name, path) in spec.group("refused_file") {
        report.attack(&format!("refused.file_read.{name}"), read(Path::new(path)));
    }
    for (name, path) in spec.group("refused_dir") {
        report.attack(&format!("refused.dir_list.{name}"), list(Path::new(path)));
    }
}

/// An answer line, or the error that kept it from coming.
fn said(answer: io::Result<String>) -> String {
    answer.unwrap_or_else(|error| format!("error:{}", crate::report::code(&error)))
}

/// Opens the broker pipe the variable `name` gives with a client's rights.
fn open_pipe(name: &str) -> io::Result<File> {
    let unset = || io::Error::new(io::ErrorKind::NotFound, "no pipe was given");
    let path = std::env::var(name).map_err(|_| unset())?;
    let mut options = OpenOptions::new();
    options.access_mode(CLIENT_ACCESS);
    options.security_qos_flags(SECURITY_IDENTIFICATION);
    options.open(path)
}

/// Reads one line of at most [`LINE_LIMIT`] bytes, without its ending.
fn read_line(pipe: &mut File) -> io::Result<String> {
    let (mut line, mut byte) = (Vec::new(), [0_u8; 1]);
    while line.len() < LINE_LIMIT && pipe.read(&mut byte)? == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&line).trim_end().to_owned())
}

/// Asks the egress broker for a tunnel to `target` (`host:port`) and
/// returns the status line of its answer.
fn connect(target: &str) -> io::Result<String> {
    let mut pipe = open_pipe(EGRESS_PIPE)?;
    pipe.write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())?;
    read_line(&mut pipe)
}

/// Pushes the candidate the spec names, if it names one, and then tries
/// the launch's own repository below the broker's work directory, which
/// exists from that push until the launch is closed.
fn publication(spec: &Spec, launch: &str, report: &mut Report) {
    let Some(bundle) = spec.get("bundle") else {
        return;
    };
    report.value("publication.push", &said(push(spec, Path::new(bundle))));
    let Some(root) = spec.get("work_root") else {
        return;
    };
    let repository = Path::new(root).join(launch);
    report.attack("repository.dir_list", list(&repository));
    report.attack("repository.head_read", read(&repository.join("HEAD")));
}

/// Sends one candidate push (the request line, the bundle's length and
/// its bytes) and returns the broker's reply line.
fn push(spec: &Spec, bundle: &Path) -> io::Result<String> {
    let field = |name: &str| spec.get(name).unwrap_or_default();
    let request = format!(
        "{{\"version\":1,\"revision\":\"{}\",\"tree\":\"{}\"}}\n",
        field("revision"),
        field("tree")
    );
    let bytes = std::fs::read(bundle)?;
    let mut pipe = open_pipe(PUBLICATION_PIPE)?;
    pipe.write_all(request.as_bytes())?;
    pipe.write_all(&(bytes.len() as u64).to_be_bytes())?;
    pipe.write_all(&bytes)?;
    read_line(&mut pipe)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spec whose controls are this crate's manifest and its directory.
    fn spec(extra: &str) -> Spec {
        let here = env!("CARGO_MANIFEST_DIR");
        let text = format!("control_file={here}\\Cargo.toml\ncontrol_dir={here}\n{extra}");
        Spec::parse(&text).unwrap()
    }

    #[test]
    fn the_controls_must_be_named_and_are_reported_as_controls() {
        let mut report = Report::default();
        run(&spec(""), "l", &mut report).unwrap();
        assert_eq!(
            report.render(),
            "control.file_read=ok\ncontrol.dir_list=ok\n"
        );
        let unnamed = Spec::parse("control_dir=C:\\\n").unwrap();
        assert!(run(&unnamed, "l", &mut Report::default()).is_err());
    }

    #[test]
    fn a_target_that_is_reached_is_reported_as_allowed_and_one_that_is_not_by_its_error() {
        let here = env!("CARGO_MANIFEST_DIR");
        let targets = format!(
            "refused_file.open={here}\\Cargo.toml\nrefused_file.gone={here}\\none\n\
             refused_dir.open={here}\nrefused_dir.gone={here}\\none\n"
        );
        let mut report = Report::default();
        run(&spec(&targets), "l", &mut report).unwrap();
        let rows = report.render();
        for row in [
            "refused.file_read.open=allowed",
            "refused.file_read.gone=error:2",
            "refused.dir_list.open=allowed",
            "refused.dir_list.gone=error:3",
        ] {
            assert!(rows.contains(row), "{row} is not in {rows}");
        }
    }

    #[test]
    fn a_launch_given_no_pipe_says_so_for_egress_and_for_its_push() {
        let here = env!("CARGO_MANIFEST_DIR");
        let asked = format!(
            "egress.allowed=github.com:443\nbundle={here}\\Cargo.toml\nwork_root={here}\\none\n"
        );
        let mut report = Report::default();
        run(&spec(&asked), "l", &mut report).unwrap();
        let rows = report.render();
        for row in [
            "egress.allowed=value:error:NotFound",
            "publication.push=value:error:NotFound",
            "repository.dir_list=error:3",
            "repository.head_read=error:3",
        ] {
            assert!(rows.contains(row), "{row} is not in {rows}");
        }
    }
}
