//! The runner service's named pipe.
//!
//! The pipe is created as the first instance of its name, with a single
//! instance, remote clients rejected and a protected access list that names
//! only the runner and the configured client account
//! ([`crate::service::pipe_policy::descriptor`]). A pipe that cannot be
//! created that way is not created at all: there is no pipe with a default
//! access list to fall back on.
//!
//! A connection is served only through a [`Session`], and a session exists
//! only once Windows has told the pipe which account and which process
//! connected and [`crate::service::pipe_policy::admit`] accepted them. The
//! account is read from the connection's own token. A client that cannot be
//! identified, or is not on the list, is disconnected without a byte read.
//!
//! All I/O is overlapped, so the one thread that serves the pipe can also
//! wake for the service's stop request and keep the drain going.
use super::sys::{Descriptor, Handle, check, last_error};
use super::text::wide;
use super::token;
use crate::service::pipe_policy::{
    Admitted, Client, ClientFacts, Refusal, admit, descriptor, pipe_path,
};
use anyhow::{Context, Result, bail, ensure};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GetLastError, HANDLE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{RevertToSelf, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, OpenThreadToken, ResetEvent, WaitForMultipleObjects,
};

/// The pipe's buffer, each way.
const BUFFER_BYTES: u32 = 64 * 1024;
/// The longest request line the pipe accepts.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// How long a client may leave an answer unread before it is dropped.
const WRITE_PATIENCE: Duration = Duration::from_secs(10);
/// One read of a request, at most.
const CHUNK: usize = 8192;

/// How an overlapped operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Io {
    /// It finished, transferring this many bytes.
    Done(u32),
    /// The patience ran out; the operation was cancelled.
    Timeout,
    /// The wake event was set; the operation was cancelled.
    Woken,
    /// The other end is gone.
    Closed,
}

/// Whether a Windows error says the other end of the pipe is gone.
fn closed(code: u32) -> bool {
    [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED].contains(&code)
}

/// What one wait for a client came to.
pub enum Accepted<'a> {
    /// Nobody connected yet, or the service was woken.
    Idle,
    /// A client connected and was refused; it has been disconnected.
    Refused(Refusal),
    /// A client connected, was identified and admitted.
    Client(Session<'a>),
}

/// What one wait for a request came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// A complete request line, without its newline.
    Request(Vec<u8>),
    /// Nothing complete arrived yet, or the service was woken.
    Idle,
    /// The client is gone.
    Closed,
}

/// The pipe's one instance.
pub struct Pipe {
    handle: Handle,
    event: Handle,
    /// The service's wake event, which interrupts waits for a client or a
    /// request; none in a test.
    wake: Option<HANDLE>,
    served: Admitted,
    /// Bytes read from the client after the last complete line.
    pending: Vec<u8>,
}

impl Pipe {
    /// Creates the first and only instance of the pipe `name`, secured for
    /// `runner` and `client` (SID text). `wake` interrupts waits.
    pub fn create(name: &str, runner: &str, client: &str, wake: Option<HANDLE>) -> Result<Self> {
        let served = Admitted::new(runner, client)?;
        let list = descriptor(runner, client)?;
        Self::with_policy(name, &list, served, wake)
    }

    /// As [`Pipe::create`] with an access list and a policy given apart; the
    /// service always derives both from the same two SIDs.
    fn with_policy(
        name: &str,
        access_list: &str,
        served: Admitted,
        wake: Option<HANDLE>,
    ) -> Result<Self> {
        let path = wide(&pipe_path(name)?)?;
        let security = Descriptor::parse(access_list)
            .context("the pipe's access list cannot be built; the pipe is not created")?;
        let attributes = security.attributes(false);
        let open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED;
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: the name is NUL-terminated; the attributes and the
        // descriptor they borrow outlive the call. One instance, so one
        // client at a time; the default time-out is unused.
        let raw = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                open_mode,
                mode,
                1,
                BUFFER_BYTES,
                BUFFER_BYTES,
                0,
                &attributes,
            )
        };
        let handle = Handle::owned(raw, "creating the pipe as its first instance")?;
        // SAFETY: no attributes and no name; a manual-reset event, not set.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        Ok(Self {
            handle,
            event: Handle::owned(event, "CreateEventW")?,
            wake,
            served,
            pending: Vec::new(),
        })
    }

    /// Runs one overlapped operation to its end or to `patience`; with
    /// `interruptible` the wake event also ends the wait. An operation that
    /// is still running when the wait ends is cancelled and collected before
    /// this returns, so the structure and the buffer it uses are never
    /// touched after they go out of scope.
    fn run(
        &self,
        start: impl FnOnce(*mut OVERLAPPED) -> i32,
        patience: Duration,
        interruptible: bool,
    ) -> Result<Io> {
        // SAFETY: the event handle is live.
        check(unsafe { ResetEvent(self.event.raw()) }, "ResetEvent")?;
        // SAFETY: OVERLAPPED is plain data; all-zero is a valid value.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = self.event.raw();
        if start(std::ptr::from_mut(&mut overlapped)) == 0 {
            // SAFETY: reads the calling thread's last error.
            let code = unsafe { GetLastError() };
            match code {
                ERROR_IO_PENDING => {}
                ERROR_PIPE_CONNECTED => return Ok(Io::Done(0)),
                code if closed(code) => return Ok(Io::Closed),
                code => bail!("a pipe operation failed with Windows error {code}"),
            }
        }
        let wake = self.wake.filter(|_| interruptible);
        let handles = [self.event.raw(), wake.unwrap_or(std::ptr::null_mut())];
        let count = if wake.is_some() { 2 } else { 1 };
        let millis = u32::try_from(patience.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: `handles` holds `count` live handles.
        let waited = unsafe { WaitForMultipleObjects(count, handles.as_ptr(), 0, millis) };
        let mut transferred = 0_u32;
        if waited == WAIT_OBJECT_0 {
            // SAFETY: the operation has completed; `overlapped` is its own.
            let finished =
                unsafe { GetOverlappedResult(self.handle.raw(), &overlapped, &mut transferred, 0) };
            return self.outcome(finished, transferred);
        }
        let ended = if waited == WAIT_OBJECT_0 + 1 {
            Ok(Io::Woken)
        } else if waited == WAIT_TIMEOUT {
            Ok(Io::Timeout)
        } else {
            Err(last_error("WaitForMultipleObjects"))
        };
        // SAFETY: cancels this operation only, then waits for it to end.
        let finished = unsafe {
            CancelIoEx(self.handle.raw(), &overlapped);
            GetOverlappedResult(self.handle.raw(), &overlapped, &mut transferred, 1)
        };
        if finished != 0 {
            // It completed before the cancel took effect.
            return Ok(Io::Done(transferred));
        }
        // SAFETY: reads the calling thread's last error.
        let code = unsafe { GetLastError() };
        if code == ERROR_OPERATION_ABORTED {
            return ended;
        }
        self.outcome(finished, transferred)
    }

    /// The outcome of a finished operation whose result Windows reported as
    /// `finished`.
    fn outcome(&self, finished: i32, transferred: u32) -> Result<Io> {
        if finished != 0 {
            return Ok(Io::Done(transferred));
        }
        // SAFETY: reads the calling thread's last error.
        let code = unsafe { GetLastError() };
        if closed(code) {
            return Ok(Io::Closed);
        }
        bail!("a pipe operation failed with Windows error {code}")
    }

    /// Waits up to `patience` for a client. A client that connects is
    /// identified at once and is either admitted, as a [`Session`], or
    /// disconnected.
    pub fn accept(&mut self, patience: Duration) -> Result<Accepted<'_>> {
        let handle = self.handle.raw();
        // SAFETY: the pipe handle is live and the structure is the one
        // `run` passes.
        let connect = |overlapped| unsafe { ConnectNamedPipe(handle, overlapped) };
        match self.run(connect, patience, true)? {
            Io::Done(_) => {}
            Io::Timeout | Io::Woken => return Ok(Accepted::Idle),
            Io::Closed => {
                // A client came and went before it was served.
                self.disconnect();
                return Ok(Accepted::Idle);
            }
        }
        let facts = self.identify();
        match admit(&self.served, &facts) {
            Ok(client) => Ok(Accepted::Client(Session { pipe: self, client })),
            Err(refusal) => {
                self.disconnect();
                Ok(Accepted::Refused(refusal))
            }
        }
    }

    /// What Windows says about the connected client. A fact Windows will not
    /// give is `None`, never a guess.
    fn identify(&self) -> ClientFacts {
        ClientFacts {
            account: self.client_account().ok(),
            process: self.client_process().ok(),
        }
    }

    /// The process id of the connected client.
    fn client_process(&self) -> Result<u32> {
        let mut process = 0_u32;
        // SAFETY: the pipe handle is live and `process` is writable.
        let read = unsafe { GetNamedPipeClientProcessId(self.handle.raw(), &mut process) };
        check(read, "GetNamedPipeClientProcessId")?;
        ensure!(process != 0, "Windows reported no client process");
        Ok(process)
    }

    /// The account of the connected client's token, read while this thread
    /// impersonates the client and only then.
    fn client_account(&self) -> Result<String> {
        // SAFETY: the pipe handle is live and has a connected client.
        let impersonating = unsafe { ImpersonateNamedPipeClient(self.handle.raw()) };
        check(impersonating, "ImpersonateNamedPipeClient")?;
        let reverting = Revert;
        let mut raw: HANDLE = std::ptr::null_mut();
        // SAFETY: the calling thread impersonates; `raw` receives a handle.
        // The token is opened as this process, so the client's own limits
        // do not stop the query.
        let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut raw) };
        let thread_token =
            check(opened, "OpenThreadToken").and_then(|()| Handle::owned(raw, "OpenThreadToken"));
        drop(reverting);
        token::user_sid(&thread_token?)
    }

    /// Ends the current client's connection so the pipe can take the next.
    fn disconnect(&mut self) {
        self.pending.clear();
        // SAFETY: the pipe handle is live. A failure means nobody was
        // connected; the next connect reports anything that matters.
        unsafe { DisconnectNamedPipe(self.handle.raw()) };
    }
}

/// While alive, the calling thread impersonates the pipe's client; dropping
/// it reverts. If Windows refuses to revert, continuing would run this
/// thread as someone else, so the process ends.
struct Revert;

impl Drop for Revert {
    fn drop(&mut self) {
        // SAFETY: takes no arguments.
        if unsafe { RevertToSelf() } == 0 {
            eprintln!("agentc-supervisor: the pipe thread could not stop impersonating; ending");
            std::process::abort();
        }
    }
}

/// One admitted client's connection. Requests are read and answers written
/// only through it; dropping it disconnects the client.
pub struct Session<'a> {
    pipe: &'a mut Pipe,
    client: Client,
}

impl Session<'_> {
    /// The client Windows identified and the pipe admitted.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Waits up to `patience` for a complete request line.
    pub fn read_line(&mut self, patience: Duration) -> Result<Line> {
        loop {
            if let Some(end) = self.pipe.pending.iter().position(|&byte| byte == b'\n') {
                let mut line: Vec<u8> = self.pipe.pending.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Line::Request(line));
            }
            ensure!(
                self.pipe.pending.len() <= MAX_REQUEST_BYTES,
                "a request is longer than {MAX_REQUEST_BYTES} bytes"
            );
            let mut chunk = [0_u8; CHUNK];
            let handle = self.pipe.handle.raw();
            let (pointer, size) = (chunk.as_mut_ptr(), chunk.len() as u32);
            // SAFETY: `pointer` is writable for `size` bytes and `run` waits
            // for the read to end before `chunk` goes out of scope.
            let read = |overlapped| unsafe {
                ReadFile(handle, pointer, size, std::ptr::null_mut(), overlapped)
            };
            match self.pipe.run(read, patience, true)? {
                Io::Done(0) | Io::Closed => return Ok(Line::Closed),
                Io::Done(count) => self
                    .pipe
                    .pending
                    .extend_from_slice(&chunk[..count as usize]),
                Io::Timeout | Io::Woken => return Ok(Line::Idle),
            }
        }
    }

    /// Writes all of `bytes`, or fails if the client leaves them unread.
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let handle = self.pipe.handle.raw();
            let size = u32::try_from(rest.len()).unwrap_or(u32::MAX);
            let pointer = rest.as_ptr();
            // SAFETY: `pointer` is readable for `size` bytes and `run` waits
            // for the write to end before `rest` is used again.
            let write = |overlapped| unsafe {
                WriteFile(handle, pointer, size, std::ptr::null_mut(), overlapped)
            };
            match self.pipe.run(write, WRITE_PATIENCE, false)? {
                Io::Done(count) if count > 0 => rest = &rest[count as usize..],
                Io::Timeout => bail!("the client did not read its answer"),
                Io::Done(_) | Io::Woken | Io::Closed => bail!("the client is gone"),
            }
        }
        Ok(())
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        self.pipe.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::fs::OpenOptionsExt;

    /// A pipe name no other test or run uses.
    fn unique() -> String {
        format!(
            "agentc-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        )
    }

    fn me() -> String {
        token::current_user_sid().unwrap()
    }

    /// What the access list grants its client: read and write data, read
    /// attributes and synchronize; a client asks for no more than this.
    const CLIENT_ACCESS: u32 = 0x1 | 0x2 | 0x80 | 0x10_0000;

    /// Connects to the pipe `name` as a client with only that access.
    fn client(name: &str) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options.access_mode(CLIENT_ACCESS);
        options.open(pipe_path(name).unwrap())
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn the_same_account_is_admitted_identified_and_served() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let talker = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut file = client(&name).unwrap();
                file.write_all(b"ping\nsecond\n").unwrap();
                let mut reader = BufReader::new(file);
                let mut answer = String::new();
                reader.read_line(&mut answer).unwrap();
                answer
            }
        });
        let Accepted::Client(mut session) = pipe.accept(10 * SECOND).unwrap() else {
            panic!("the client was not admitted");
        };
        assert_eq!(session.client().account, me);
        assert!(session.client().process > 0);
        assert_eq!(
            session.read_line(10 * SECOND).unwrap(),
            Line::Request(b"ping".to_vec())
        );
        assert_eq!(
            session.read_line(10 * SECOND).unwrap(),
            Line::Request(b"second".to_vec())
        );
        session.write_all(b"pong\n").unwrap();
        assert_eq!(talker.join().unwrap(), "pong\n");
    }

    #[test]
    fn the_configured_client_connects_with_only_the_rights_the_list_grants_it() {
        // The runner of this pipe is another account, so this account is
        // served by its narrow entry alone.
        let (name, me) = (unique(), me());
        let runner = "S-1-5-21-111-222-333-1111";
        let mut pipe = Pipe::create(&name, runner, &me, None).unwrap();
        let talker = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut file = client(&name).unwrap();
                file.write_all(b"hello\n").unwrap();
            }
        });
        let Accepted::Client(mut session) = pipe.accept(10 * SECOND).unwrap() else {
            panic!("the configured client was not admitted");
        };
        assert_eq!(session.client().account, me);
        assert_eq!(
            session.read_line(10 * SECOND).unwrap(),
            Line::Request(b"hello".to_vec())
        );
        talker.join().unwrap();
    }

    #[test]
    fn a_pipe_name_already_in_use_is_never_taken_over() {
        let (name, me) = (unique(), me());
        let _first = Pipe::create(&name, &me, &me, None).unwrap();
        assert!(Pipe::create(&name, &me, &me, None).is_err());
    }

    #[test]
    fn nobody_connecting_is_idle_and_not_an_error() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        assert!(matches!(
            pipe.accept(Duration::from_millis(50)).unwrap(),
            Accepted::Idle
        ));
    }

    #[test]
    fn an_account_the_access_list_omits_cannot_even_connect() {
        let (name, nobody) = (unique(), "S-1-5-21-111-222-333-1111");
        let _pipe = Pipe::create(&name, nobody, "S-1-5-21-111-222-333-1112", None).unwrap();
        let refused = client(&name).unwrap_err();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{refused}"
        );
    }

    #[test]
    fn a_client_the_policy_does_not_list_is_disconnected_unserved() {
        // The access list lets this account in, as a mistake would; the
        // identity check is the second wall and must still refuse.
        let (name, me) = (unique(), me());
        let list = descriptor(&me, &me).unwrap();
        let stranger = Admitted::new("S-1-5-21-111-222-333-1111", "S-1-5-21-111-222-333-1112");
        let mut pipe = Pipe::with_policy(&name, &list, stranger.unwrap(), None).unwrap();
        let talker = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut file = client(&name).unwrap();
                let _ = file.write_all(b"ping\n");
                let mut reader = BufReader::new(file);
                let mut answer = String::new();
                reader.read_line(&mut answer).unwrap_or(0)
            }
        });
        match pipe.accept(10 * SECOND).unwrap() {
            Accepted::Refused(Refusal::NotAllowed(account)) => assert_eq!(account, me),
            Accepted::Refused(other) => panic!("refused for the wrong reason: {other}"),
            Accepted::Idle => panic!("the client never connected"),
            Accepted::Client(_) => panic!("a client that is not listed was served"),
        }
        assert_eq!(
            talker.join().unwrap(),
            0,
            "the refused client got an answer"
        );
    }

    #[test]
    fn a_silent_client_leaves_the_server_free_to_do_other_work() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let _quiet = std::thread::spawn({
            let name = name.clone();
            move || {
                let file = client(&name).unwrap();
                std::thread::sleep(Duration::from_millis(800));
                drop(file);
            }
        });
        let Accepted::Client(mut session) = pipe.accept(10 * SECOND).unwrap() else {
            panic!("the client was not admitted");
        };
        assert_eq!(
            session.read_line(Duration::from_millis(100)).unwrap(),
            Line::Idle
        );
        assert_eq!(session.read_line(10 * SECOND).unwrap(), Line::Closed);
    }

    #[test]
    fn a_dropped_session_frees_the_pipe_for_the_next_client() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        for round in 0..2 {
            let talker = std::thread::spawn({
                let name = name.clone();
                move || {
                    let mut file = client(&name).unwrap();
                    file.write_all(format!("round {round}\n").as_bytes())
                        .unwrap();
                }
            });
            let Accepted::Client(mut session) = pipe.accept(10 * SECOND).unwrap() else {
                panic!("the client was not admitted");
            };
            let wanted = Line::Request(format!("round {round}").into_bytes());
            assert_eq!(session.read_line(10 * SECOND).unwrap(), wanted);
            talker.join().unwrap();
        }
    }
}
