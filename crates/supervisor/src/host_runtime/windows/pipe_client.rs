//! The supervisor's side of the runner service's pipe ([`super::pipe`]).
//!
//! A client opens the pipe asking for exactly what the pipe's access list
//! grants a client ([`CLIENT_ACCESS`]) and lets the server learn who it is
//! and nothing more: the server may identify the connection, not act as the
//! client. Before a byte is written the client asks Windows who owns the
//! pipe and refuses any owner but the configured runner account. Only the
//! process that created a pipe owns it, so a pipe somebody else made under
//! the runner's name is never sent a request.
//!
//! The exchange is one request line, one answer line. A request is a typed
//! value the runner knows ([`super::runner::Request`]); none is a command
//! line. An exchange that fails leaves the client unusable: the caller
//! cannot tell whether the request was carried out, so it must not conclude
//! that a launch ended.
use super::sys::{Local, check, sid_text};
use crate::service::pipe_policy::{CLIENT_ACCESS, checked_sid, pipe_path};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

/// The longest answer line the client accepts.
const MAX_ANSWER_BYTES: u64 = 16 * 1024 * 1024;

/// One connection to a runner service whose pipe's owner was verified.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<File>,
    /// The process Windows says serves the pipe.
    server_process: u32,
    /// Set once an exchange failed; nothing more is sent after that.
    broken: bool,
}

impl Client {
    /// Connects to the pipe `name` and verifies that the account `runner`
    /// (SID text) owns it. A pipe owned by anyone else is closed unused.
    pub fn connect(name: &str, runner: &str) -> Result<Self> {
        let runner = checked_sid(runner)?;
        let file = open(name)?;
        let owner = owner_sid(&file).context("the pipe's owner cannot be read; it is not used")?;
        ensure!(
            owner.eq_ignore_ascii_case(runner),
            "the pipe {name} is owned by {owner}, not by the runner account {runner}; \
             it is not used"
        );
        let server_process = server_process(&file)?;
        Ok(Self {
            reader: BufReader::new(file),
            server_process,
            broken: false,
        })
    }

    /// The process Windows reported as the pipe's server, for the record.
    pub fn server_process(&self) -> u32 {
        self.server_process
    }

    /// Sends `request` and returns the runner's answer, whatever it says.
    /// An error means the exchange itself failed and the outcome of the
    /// request is unknown.
    pub fn ask(&mut self, request: &Value) -> Result<Value> {
        ensure!(!self.broken, "the connection to the runner already failed");
        let answer = self.exchange(request);
        self.broken = answer.is_err();
        answer
    }

    /// One line out, one line back.
    fn exchange(&mut self, request: &Value) -> Result<Value> {
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        let pipe = self.reader.get_mut();
        pipe.write_all(&line).context("send a request")?;
        let mut answer = Vec::new();
        let mut limited = (&mut self.reader).take(MAX_ANSWER_BYTES);
        let read = limited.read_until(b'\n', &mut answer);
        read.context("read the answer")?;
        ensure!(
            answer.last() == Some(&b'\n'),
            "the runner closed the pipe without a whole answer"
        );
        serde_json::from_slice(&answer).context("the answer is not JSON")
    }
}

/// Opens the pipe `name` with a client's rights. The server may identify
/// this process on the connection and may not impersonate it further.
fn open(name: &str) -> Result<File> {
    let path = pipe_path(name)?;
    let mut options = OpenOptions::new();
    options
        .access_mode(CLIENT_ACCESS)
        .security_qos_flags(SECURITY_IDENTIFICATION);
    (options.open(&path)).with_context(|| format!("open the runner's pipe {path}"))
}

/// The SID text of the pipe's owner, as Windows reports it for the handle.
fn owner_sid(pipe: &File) -> Result<String> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is live; `owner` receives a pointer into the
    // descriptor Windows allocates, which `descriptor` receives. The group
    // and the two access lists are not asked for.
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        bail!("GetSecurityInfo failed with Windows error {status}");
    }
    // `owner` points into the allocation `held` frees when it is dropped.
    let held = Local::owned(descriptor);
    let text = sid_text(owner);
    drop(held);
    text
}

/// The process id of the pipe's server.
fn server_process(pipe: &File) -> Result<u32> {
    let mut process = 0_u32;
    // SAFETY: the handle is live and `process` is writable.
    let read = unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut process) };
    check(read, "GetNamedPipeServerProcessId")?;
    Ok(process)
}

#[cfg(test)]
mod tests {
    use super::super::pipe::{Accepted, Line, Pipe};
    use super::super::token;
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    const PATIENCE: Duration = Duration::from_secs(10);

    /// A pipe name no other test or run uses.
    fn unique() -> String {
        let id = uuid::Uuid::new_v4().simple();
        format!("agentc-test-client-{}-{id}", std::process::id())
    }

    /// Serves one client of `pipe`: answers each request line with
    /// `{"echo": <request>}` until the client leaves.
    fn echo(pipe: &mut Pipe) -> u32 {
        let Accepted::Client(mut session) = pipe.accept(PATIENCE).unwrap() else {
            panic!("the client was not admitted");
        };
        let mut served = 0;
        while let Line::Request(bytes) = session.read_line(PATIENCE).unwrap() {
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let mut answer = json!({ "echo": request }).to_string();
            answer.push('\n');
            session.write_all(answer.as_bytes()).unwrap();
            served += 1;
        }
        served
    }

    #[test]
    fn a_client_exchanges_typed_lines_with_a_pipe_its_runner_owns() {
        let (name, me) = (unique(), token::current_user_sid().unwrap());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let asker = std::thread::spawn(move || {
            let mut client = Client::connect(&name, &me).unwrap();
            assert_eq!(client.server_process(), std::process::id());
            for wait in [1, 2] {
                let request = json!({"op": "observe", "launch": "l", "wait_ms": wait});
                let answer = client.ask(&request).unwrap();
                assert_eq!(answer["echo"], request);
            }
        });
        assert_eq!(echo(&mut pipe), 2);
        asker.join().unwrap();
    }

    #[test]
    fn a_pipe_another_account_owns_is_never_sent_a_request() {
        // The pipe is this account's; the supervisor is told the runner is
        // someone else, as it would be if a stranger made the pipe first.
        let (name, me) = (unique(), token::current_user_sid().unwrap());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let expected = "S-1-5-21-111-222-333-1111";
        let refused = Client::connect(&name, expected).unwrap_err().to_string();
        assert!(refused.contains("not by the runner account"), "{refused}");
        // The server saw at most a connection that sent nothing.
        match pipe.accept(Duration::from_millis(200)).unwrap() {
            Accepted::Idle | Accepted::Refused(_) => {}
            Accepted::Client(_) => panic!("a request reached a pipe that was refused"),
        }
    }

    #[test]
    fn a_pipe_that_is_not_there_and_a_runner_that_is_no_sid_are_refused() {
        let me = token::current_user_sid().unwrap();
        assert!(Client::connect(&unique(), &me).is_err());
        assert!(Client::connect("agentc-runner", "Everyone").is_err());
        assert!(Client::connect("..\\other", &me).is_err());
    }

    #[test]
    fn a_runner_that_leaves_mid_answer_breaks_the_client_for_good() {
        let (name, me) = (unique(), token::current_user_sid().unwrap());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let asker = std::thread::spawn(move || {
            let mut client = Client::connect(&name, &me).unwrap();
            assert!(client.ask(&json!({"op": "identity"})).is_err());
            let again = client.ask(&json!({"op": "identity"})).unwrap_err();
            assert!(again.to_string().contains("already failed"), "{again}");
        });
        {
            let Accepted::Client(mut session) = pipe.accept(PATIENCE).unwrap() else {
                panic!("the client was not admitted");
            };
            session.read_line(PATIENCE).unwrap();
            session.write_all(b"{\"ok\": tr").unwrap();
        }
        asker.join().unwrap();
    }
}
