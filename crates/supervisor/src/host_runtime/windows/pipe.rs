//! The runner service's named pipe.
//!
//! The pipe is created as the first instance of its name, with a single
//! instance, remote clients rejected and a protected access list that names
//! only the runner and the configured client account
//! ([`crate::service::pipe_policy::descriptor`]). A pipe that cannot be
//! created that way is not created at all: there is no pipe with a default
//! access list to fall back on. A reviewer launch's candidate pipe
//! ([`Pipe::package_instances`]) is the one pipe with several instances,
//! each made by this process under the same access list and served by its
//! own thread.
//!
//! A connection is served only through a [`Session`], and a session exists
//! only once Windows has told the pipe which account and which process
//! connected and [`crate::service::pipe_policy::admit`] accepted them. The
//! account is read from the connection's own token. Windows gives that token
//! only after something was read from the connection (it fails with error
//! 1368 before), so the pipe first takes in the client's first bytes and
//! keeps them unparsed. A client that cannot be identified, or is not on the
//! list, is disconnected and those bytes are thrown away: no request of its
//! is looked at and nothing is written to it. A client that connects and
//! stays silent is disconnected after [`IDENTIFY_PATIENCE`]. A launch's
//! broker pipe ([`Pipe::broker_instances`]) is made the same way, owned by
//! the broker's account and serving only the launch's AppContainer token.
//!
//! All I/O is overlapped, so the one thread that serves the pipe can also
//! wake for the service's stop request and keep the drain going. A session
//! carries lines ([`Session::read_line`]) or a byte stream
//! ([`Session::read`]), and splits into a [`Reader`] and a [`Writer`] that
//! a reading thread and a writing thread use at once. Reads and writes each
//! have their own event, so a read and a write can be outstanding together.
use super::sys::{Descriptor, Handle, check, last_error};
use super::text::wide;
use super::token;
use crate::service::pipe_policy::{
    Admitted, Client, ClientFacts, Package, Refusal, admit, broker_descriptor, control_descriptor,
    descriptor, package_descriptor, pipe_path,
};
use anyhow::{Context, Result, bail, ensure};
use std::time::{Duration, Instant};
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
/// How long a connected client may stay silent before it is disconnected
/// unidentified, so it cannot hold the pipe's one instance.
pub const IDENTIFY_PATIENCE: Duration = Duration::from_secs(10);

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

/// What one raw read from the client came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Received {
    /// Bytes the client sent, at least one and at most the read's maximum.
    Data(Vec<u8>),
    /// Nothing came within the patience, or the wake event is set.
    Idle,
    /// The client is gone.
    Closed,
}

/// What one overlapped read into a buffer came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Got {
    /// This many bytes, at least one, are at the start of the buffer.
    Bytes(usize),
    /// Nothing came within the patience, or the wake event is set.
    Idle,
    /// The client is gone.
    Closed,
}

/// What one read from the connected client came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// Bytes arrived and were added to the pending ones.
    Read,
    /// Nothing arrived yet, or the service was woken.
    Idle,
    /// The client is gone.
    Closed,
}

/// Which instance of a pipe to create and how many the pipe may have.
#[derive(Debug, Clone, Copy)]
struct Shape {
    /// Whether this must be the first instance of the name.
    first: bool,
    /// The most instances of the name, the same for each of them.
    count: u32,
}

impl Shape {
    /// The only instance of its name.
    const ALONE: Self = Self {
        first: true,
        count: 1,
    };
}

/// A manual-reset event that one kind of overlapped operation on a pipe
/// instance waits on.
struct Event(Handle);

// SAFETY: an event handle is a process-wide value; resetting it and waiting
// on it from any thread is thread-safe. Each event is used by one operation
// at a time: reads by the one reader, writes by the one writer.
unsafe impl Sync for Event {}

impl Event {
    /// A manual-reset event, not set.
    fn new() -> Result<Self> {
        // SAFETY: no attributes and no name; a manual-reset event, not set.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        Ok(Self(Handle::owned(event, "CreateEventW")?))
    }

    /// The handle value, for a Windows call.
    fn raw(&self) -> HANDLE {
        self.0.raw()
    }
}

/// A pipe instance's handle and the wake event that interrupts its waits:
/// what every overlapped operation on the instance needs.
struct Port {
    handle: Handle,
    /// The service's wake event, which interrupts waits for a client or a
    /// request; none in a test.
    wake: Option<HANDLE>,
}

// SAFETY: `handle` is owned by the instance and `wake` is a kernel handle
// whose owner outlives every thread that serves the instance; a handle is a
// process-wide value any thread may use. Overlapped operations from two
// threads on one pipe handle are allowed when each has its own OVERLAPPED
// structure and event, which `Port::run` takes from its caller.
unsafe impl Send for Port {}
// SAFETY: as above; `Port` has no state of its own that a call changes.
unsafe impl Sync for Port {}

/// One instance of a pipe: the only one, or one of a launch's candidate
/// or broker pipe.
pub struct Pipe {
    port: Port,
    /// The event of connects and reads, which never overlap each other.
    reads: Event,
    /// The event of writes, which may be outstanding beside a read.
    writes: Event,
    served: Admitted,
    /// Bytes read from the client after the last complete line.
    pending: Vec<u8>,
    /// When the client that is connected but not yet identified connected.
    connected: Option<Instant>,
    /// How long that client may stay silent.
    identify_patience: Duration,
}

impl Port {
    /// Runs one overlapped operation, signalling `event`, to its end or to
    /// `patience`; with `interruptible` the wake event also ends the wait.
    /// An operation that is still running when the wait ends is cancelled
    /// and collected before this returns, so the structure and the buffer
    /// it uses are never touched after they go out of scope. Operations
    /// that may be outstanding together must be given different events.
    fn run(
        &self,
        event: &Event,
        start: impl FnOnce(*mut OVERLAPPED) -> i32,
        patience: Duration,
        interruptible: bool,
    ) -> Result<Io> {
        // SAFETY: the event handle is live.
        check(unsafe { ResetEvent(event.raw()) }, "ResetEvent")?;
        // SAFETY: OVERLAPPED is plain data; all-zero is a valid value.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.raw();
        if start(std::ptr::from_mut(&mut overlapped)) == 0
            && let Some(ended) = not_pending()?
        {
            return Ok(ended);
        }
        let waited = self.wait(event, patience, interruptible);
        if waited == WAIT_OBJECT_0 {
            let mut transferred = 0_u32;
            // SAFETY: the operation has completed; `overlapped` is its own.
            let finished =
                unsafe { GetOverlappedResult(self.handle.raw(), &overlapped, &mut transferred, 0) };
            return outcome(finished, transferred);
        }
        let ended = if waited == WAIT_OBJECT_0 + 1 {
            Ok(Io::Woken)
        } else if waited == WAIT_TIMEOUT {
            Ok(Io::Timeout)
        } else {
            Err(last_error("WaitForMultipleObjects"))
        };
        self.cancel(&overlapped, ended)
    }

    /// Waits up to `patience` for `event`, and with `interruptible` for the
    /// wake event as well; what `WaitForMultipleObjects` returned.
    fn wait(&self, event: &Event, patience: Duration, interruptible: bool) -> u32 {
        let wake = self.wake.filter(|_| interruptible);
        let handles = [event.raw(), wake.unwrap_or(std::ptr::null_mut())];
        let count = if wake.is_some() { 2 } else { 1 };
        let millis = u32::try_from(patience.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: `handles` holds `count` live handles.
        unsafe { WaitForMultipleObjects(count, handles.as_ptr(), 0, millis) }
    }

    /// Cancels the operation of `overlapped`, which the wait left running,
    /// and waits for it to end. One that completed before the cancel took
    /// effect is reported as done; one the cancel ended is `ended`.
    fn cancel(&self, overlapped: &OVERLAPPED, ended: Result<Io>) -> Result<Io> {
        let mut transferred = 0_u32;
        // SAFETY: cancels this operation only, then waits for it to end.
        let finished = unsafe {
            CancelIoEx(self.handle.raw(), overlapped);
            GetOverlappedResult(self.handle.raw(), overlapped, &mut transferred, 1)
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
        outcome(finished, transferred)
    }

    /// Reads once from the connected client into `buffer`, waiting up to
    /// `patience`; the wake event ends the wait.
    fn read_into(&self, event: &Event, buffer: &mut [u8], patience: Duration) -> Result<Got> {
        let handle = self.handle.raw();
        let size = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let pointer = buffer.as_mut_ptr();
        // SAFETY: `pointer` is writable for `size` bytes and `run` waits for
        // the read to end before `buffer` is used again.
        let read = |overlapped| unsafe {
            ReadFile(handle, pointer, size, std::ptr::null_mut(), overlapped)
        };
        Ok(match self.run(event, read, patience, true)? {
            Io::Done(0) | Io::Closed => Got::Closed,
            Io::Done(count) => Got::Bytes(count as usize),
            Io::Timeout | Io::Woken => Got::Idle,
        })
    }

    /// Writes all of `bytes`, or fails if the client leaves them unread for
    /// `patience`; with `interruptible` the wake event ends it too.
    fn write_all(
        &self,
        event: &Event,
        bytes: &[u8],
        (interruptible, patience): (bool, Duration),
    ) -> Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let handle = self.handle.raw();
            let size = u32::try_from(rest.len()).unwrap_or(u32::MAX);
            let pointer = rest.as_ptr();
            // SAFETY: `pointer` is readable for `size` bytes and `run` waits
            // for the write to end before `rest` is used again.
            let write = |overlapped| unsafe {
                WriteFile(handle, pointer, size, std::ptr::null_mut(), overlapped)
            };
            match self.run(event, write, patience, interruptible)? {
                Io::Done(count) if count > 0 => rest = &rest[count as usize..],
                Io::Timeout => bail!("the client did not read its answer"),
                Io::Woken => bail!("the session was stopped"),
                Io::Done(_) | Io::Closed => bail!("the client is gone"),
            }
        }
        Ok(())
    }
}

/// What an operation that did not start pending came to, read from the
/// calling thread's last error; `None` when it is pending after all.
fn not_pending() -> Result<Option<Io>> {
    // SAFETY: reads the calling thread's last error.
    let code = unsafe { GetLastError() };
    match code {
        ERROR_IO_PENDING => Ok(None),
        ERROR_PIPE_CONNECTED => Ok(Some(Io::Done(0))),
        code if closed(code) => Ok(Some(Io::Closed)),
        code => bail!("a pipe operation failed with Windows error {code}"),
    }
}

/// The outcome of a finished operation whose result Windows reported as
/// `finished`.
fn outcome(finished: i32, transferred: u32) -> Result<Io> {
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

impl Pipe {
    /// Creates the first and only instance of the pipe `name`, secured for
    /// `runner` and `client` (SID text). `wake` interrupts waits.
    pub fn create(name: &str, runner: &str, client: &str, wake: Option<HANDLE>) -> Result<Self> {
        let served = Admitted::new(runner, client)?;
        let list = descriptor(runner, client)?;
        Self::with_policy(name, &list, served, wake)
    }

    /// Creates the first and only instance of one launch's candidate pipe
    /// `name`: the access list names `runner` and `package`
    /// ([`package_descriptor`]), and a client is served only when its token
    /// is of the runner's account and carries exactly `package`.
    pub fn for_package(
        name: &str,
        runner: &str,
        package: &str,
        wake: Option<HANDLE>,
    ) -> Result<Self> {
        let served = Admitted::package(runner, package)?;
        let list = package_descriptor(runner, package)?;
        Self::with_policy(name, &list, served, wake)
    }

    /// As [`Pipe::for_package`] with `count` instances of the pipe, each to
    /// be served by its own thread, so that many clients are served at
    /// once. The first is created as the name's first instance and the rest
    /// join it; a name somebody else holds is never joined.
    pub fn package_instances(
        name: &str,
        runner: &str,
        package: &str,
        count: u32,
        wake: Option<HANDLE>,
    ) -> Result<Vec<Self>> {
        let served = Admitted::package(runner, package)?;
        let list = package_descriptor(runner, package)?;
        Self::instances(name, &list, &served, count, wake)
    }

    /// `count` instances of one launch's broker pipe `name`, each to be
    /// served by its own thread. The broker's account `broker` owns the
    /// pipe; the access list lets in the launch's `account` and `package`
    /// ([`broker_descriptor`]), and a client is served only when its token
    /// is of `account` and carries exactly `package` ([`Admitted::launch`]).
    pub fn broker_instances(
        name: &str,
        broker: &str,
        (account, package): (&str, &str),
        count: u32,
        wake: Option<HANDLE>,
    ) -> Result<Vec<Self>> {
        let served = Admitted::launch(account, package)?;
        let list = broker_descriptor(broker, account, package)?;
        Self::instances(name, &list, &served, count, wake)
    }

    /// The first and only instance of a broker's control pipe `name`. The
    /// broker's account `broker` owns it ([`control_descriptor`]), so the
    /// runner can tell it from a pipe somebody else made under the name,
    /// and only the runner's account `client` is served.
    pub fn control(name: &str, broker: &str, client: &str, wake: Option<HANDLE>) -> Result<Self> {
        let served = Admitted::new(client, client)?;
        let list = control_descriptor(broker, client)?;
        Self::with_policy(name, &list, served, wake)
    }

    /// `count` instances of the pipe `name` under `access_list` and the
    /// policy `served`. The first is created as the name's first instance
    /// and the rest join it.
    fn instances(
        name: &str,
        access_list: &str,
        served: &Admitted,
        count: u32,
        wake: Option<HANDLE>,
    ) -> Result<Vec<Self>> {
        ensure!(count > 0, "a pipe needs at least one instance");
        let instance = |first| {
            let shape = Shape { first, count };
            Self::shaped(name, access_list, served.clone(), wake, shape)
        };
        (0..count).map(|index| instance(index == 0)).collect()
    }

    /// As [`Pipe::create`] with an access list and a policy given apart; the
    /// service always derives both from the same two SIDs.
    fn with_policy(
        name: &str,
        access_list: &str,
        served: Admitted,
        wake: Option<HANDLE>,
    ) -> Result<Self> {
        Self::shaped(name, access_list, served, wake, Shape::ALONE)
    }

    /// One instance of the pipe `name` as `shape` says.
    fn shaped(
        name: &str,
        access_list: &str,
        served: Admitted,
        wake: Option<HANDLE>,
        shape: Shape,
    ) -> Result<Self> {
        let path = wide(&pipe_path(name)?)?;
        let security = Descriptor::parse(access_list)
            .context("the pipe's access list cannot be built; the pipe is not created")?;
        let attributes = security.attributes(false);
        let first = if shape.first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
        let open_mode = PIPE_ACCESS_DUPLEX | first | FILE_FLAG_OVERLAPPED;
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: the name is NUL-terminated; the attributes and the
        // descriptor they borrow outlive the call. Each instance serves one
        // client at a time; the default time-out is unused.
        let raw = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                open_mode,
                mode,
                shape.count,
                BUFFER_BYTES,
                BUFFER_BYTES,
                0,
                &attributes,
            )
        };
        let handle = Handle::owned(raw, "creating an instance of the pipe")?;
        Ok(Self {
            port: Port { handle, wake },
            reads: Event::new()?,
            writes: Event::new()?,
            served,
            pending: Vec::new(),
            connected: None,
            identify_patience: IDENTIFY_PATIENCE,
        })
    }

    /// Waits up to `patience` for a client, and then for its first bytes.
    /// Once they arrived the client is identified and is either admitted,
    /// as a [`Session`], or disconnected. A client still silent when the
    /// wait ends stays connected for the next call, up to the patience a
    /// silent client gets.
    pub fn accept(&mut self, patience: Duration) -> Result<Accepted<'_>> {
        if self.connected.is_none() && !self.connect(patience)? {
            return Ok(Accepted::Idle);
        }
        match self.fill(patience)? {
            Fill::Read => {}
            Fill::Idle => return Ok(self.still_silent()),
            Fill::Closed => {
                // A client came and went before it was served.
                self.disconnect();
                return Ok(Accepted::Idle);
            }
        }
        let facts = self.identify();
        match admit(&self.served, &facts) {
            Ok(client) => {
                self.connected = None;
                Ok(Accepted::Client(Session { pipe: self, client }))
            }
            Err(refusal) => {
                self.disconnect();
                Ok(Accepted::Refused(refusal))
            }
        }
    }

    /// Waits up to `patience` for a client to connect; true when one did.
    fn connect(&mut self, patience: Duration) -> Result<bool> {
        let handle = self.port.handle.raw();
        // SAFETY: the pipe handle is live and the structure is the one
        // `run` passes.
        let connect = |overlapped| unsafe { ConnectNamedPipe(handle, overlapped) };
        match self.port.run(&self.reads, connect, patience, true)? {
            Io::Done(_) => {
                self.connected = Some(Instant::now());
                Ok(true)
            }
            Io::Timeout | Io::Woken => Ok(false),
            Io::Closed => {
                // A client came and went before it was served.
                self.disconnect();
                Ok(false)
            }
        }
    }

    /// What to do with a connected client that has sent nothing yet: keep
    /// waiting, or disconnect it once its patience ran out.
    fn still_silent(&mut self) -> Accepted<'_> {
        let waited = self
            .connected
            .map_or(Duration::ZERO, |since| since.elapsed());
        if waited < self.identify_patience {
            return Accepted::Idle;
        }
        self.disconnect();
        Accepted::Refused(Refusal::Silent)
    }

    /// Reads once from the connected client into the pending bytes, waiting
    /// up to `patience`. The bytes are only stored here, never parsed.
    fn fill(&mut self, patience: Duration) -> Result<Fill> {
        let mut chunk = [0_u8; CHUNK];
        match self.port.read_into(&self.reads, &mut chunk, patience)? {
            Got::Bytes(count) => {
                self.pending.extend_from_slice(&chunk[..count]);
                Ok(Fill::Read)
            }
            Got::Idle => Ok(Fill::Idle),
            Got::Closed => Ok(Fill::Closed),
        }
    }

    /// What Windows says about the connected client. A fact Windows will not
    /// give is `None`, never a guess.
    fn identify(&self) -> ClientFacts {
        let token = self.client_token();
        let account = token.as_ref().ok().and_then(|t| token::user_sid(t).ok());
        let package = match token.as_ref().map(token::package_sid) {
            Ok(Ok(Some(sid))) => Package::Sid(sid),
            Ok(Ok(None)) => Package::None,
            _ => Package::Unknown,
        };
        ClientFacts {
            account,
            process: self.client_process().ok(),
            package,
        }
    }

    /// The process id of the connected client.
    fn client_process(&self) -> Result<u32> {
        let mut process = 0_u32;
        // SAFETY: the pipe handle is live and `process` is writable.
        let read = unsafe { GetNamedPipeClientProcessId(self.port.handle.raw(), &mut process) };
        check(read, "GetNamedPipeClientProcessId")?;
        ensure!(process != 0, "Windows reported no client process");
        Ok(process)
    }

    /// The connected client's token, opened while this thread impersonates
    /// the client and only then; its account and package are read from it
    /// after the revert. Windows refuses to impersonate a connection nothing
    /// was read from, so the caller reads the client's first bytes before
    /// this.
    fn client_token(&self) -> Result<Handle> {
        // SAFETY: the pipe handle is live and has a connected client.
        let impersonating = unsafe { ImpersonateNamedPipeClient(self.port.handle.raw()) };
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
        thread_token
    }

    /// Ends the current client's connection so the pipe can take the next.
    fn disconnect(&mut self) {
        self.pending.clear();
        self.connected = None;
        // SAFETY: the pipe handle is live. A failure means nobody was
        // connected; the next connect reports anything that matters.
        unsafe { DisconnectNamedPipe(self.port.handle.raw()) };
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
            match self.pipe.fill(patience)? {
                Fill::Read => {}
                Fill::Idle => return Ok(Line::Idle),
                Fill::Closed => return Ok(Line::Closed),
            }
        }
    }

    /// Writes all of `bytes`, or fails if the client leaves them unread.
    /// The wake event does not interrupt it, so an answer is written whole
    /// while the service drains.
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let shape = (false, WRITE_PATIENCE);
        self.pipe.port.write_all(&self.pipe.writes, bytes, shape)
    }

    /// Waits up to `patience` for bytes from the client and returns at most
    /// `max` of them, whatever they are: bytes already read and not yet
    /// taken come first, before anything is read from the pipe again.
    pub fn read(&mut self, max: usize, patience: Duration) -> Result<Received> {
        self.split().0.read(max, patience)
    }

    /// The session's reading and writing directions, so that one thread
    /// reads from the client while another writes to it. Both borrow the
    /// session, which disconnects the client once it is dropped after them.
    pub fn split(&mut self) -> (Reader<'_>, Writer<'_>) {
        let pipe = &mut *self.pipe;
        let reader = Reader {
            port: &pipe.port,
            event: &pipe.reads,
            pending: &mut pipe.pending,
        };
        let writer = Writer {
            port: &pipe.port,
            event: &pipe.writes,
        };
        (reader, writer)
    }
}

/// The reading direction of a [`Session`].
pub struct Reader<'s> {
    port: &'s Port,
    event: &'s Event,
    /// The session's bytes read and not yet taken.
    pending: &'s mut Vec<u8>,
}

impl Reader<'_> {
    /// As [`Session::read`]: pending bytes first, then one read of at most
    /// `max` bytes (and at most [`MAX_REQUEST_BYTES`]) from the pipe. The
    /// wake event ends the wait as [`Received::Idle`].
    pub fn read(&mut self, max: usize, patience: Duration) -> Result<Received> {
        ensure!(max > 0, "a read takes at least one byte");
        if !self.pending.is_empty() {
            let count = max.min(self.pending.len());
            return Ok(Received::Data(self.pending.drain(..count).collect()));
        }
        let mut buffer = vec![0_u8; max.min(MAX_REQUEST_BYTES)];
        Ok(
            match self.port.read_into(self.event, &mut buffer, patience)? {
                Got::Bytes(count) => {
                    buffer.truncate(count);
                    Received::Data(buffer)
                }
                Got::Idle => Received::Idle,
                Got::Closed => Received::Closed,
            },
        )
    }
}

/// The writing direction of a [`Session`].
pub struct Writer<'s> {
    port: &'s Port,
    event: &'s Event,
}

impl Writer<'_> {
    /// Writes all of `bytes`, or fails if the client leaves them unread.
    /// Unlike [`Session::write_all`], the wake event ends the write: a
    /// stream ends at once when its pipe is stopped.
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.port
            .write_all(self.event, bytes, (true, WRITE_PATIENCE))
    }

    /// Writes all of `bytes` however long the client takes to read them, as
    /// a stream to a client that applies back-pressure must: only the wake
    /// event or the client's leaving ends the write. A pipe made without a
    /// wake event is refused, since nothing could end the write then.
    pub fn stream_all(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.port.wake.is_some(),
            "a pipe without a wake event cannot stream"
        );
        self.port
            .write_all(self.event, bytes, (true, Duration::MAX))
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
    fn a_candidate_pipe_refuses_the_runners_own_account_without_its_package() {
        // This process is the runner's account, so the access list lets it
        // connect; it carries no package, so admission must refuse it and
        // write it nothing.
        let (name, me) = (unique(), me());
        let package = "S-1-15-2-1-2-3-4-5-6-7";
        let mut pipe = Pipe::for_package(&name, &me, package, None).unwrap();
        let talker = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut file = client(&name).unwrap();
                let _ = file.write_all(b"{\"command\":\"true\"}\n");
                let mut answer = String::new();
                BufReader::new(file).read_line(&mut answer).unwrap_or(0)
            }
        });
        match pipe.accept(10 * SECOND).unwrap() {
            Accepted::Refused(Refusal::WrongPackage(None)) => {}
            Accepted::Refused(other) => panic!("refused for the wrong reason: {other}"),
            Accepted::Idle => panic!("the client never connected"),
            Accepted::Client(_) => panic!("a client without the package was served"),
        }
        assert_eq!(
            talker.join().unwrap(),
            0,
            "the refused client got an answer"
        );
    }

    #[test]
    fn a_pipe_name_already_in_use_is_never_taken_over() {
        let (name, me) = (unique(), me());
        let _first = Pipe::create(&name, &me, &me, None).unwrap();
        assert!(Pipe::create(&name, &me, &me, None).is_err());
    }

    #[test]
    fn every_instance_serves_a_client_and_one_more_finds_the_pipe_busy() {
        // No client here carries the package, so each is refused; what
        // matters is that two are connected at once and a third is not.
        const ERROR_PIPE_BUSY: i32 = 231;
        let (name, me) = (unique(), me());
        let package = "S-1-15-2-1-2-3-4-5-6-7";
        let mut pipes = Pipe::package_instances(&name, &me, package, 2, None).unwrap();
        assert!(Pipe::for_package(&name, &me, package, None).is_err());
        let mut clients = [client(&name).unwrap(), client(&name).unwrap()];
        let busy = client(&name).unwrap_err();
        assert_eq!(busy.raw_os_error(), Some(ERROR_PIPE_BUSY), "{busy}");
        // The clients stay connected until both were judged: Windows names
        // a connection's account only while its client is there.
        for held in &mut clients {
            held.write_all(b"x\n").unwrap();
        }
        for pipe in &mut pipes {
            let refused = pipe.accept(10 * SECOND).unwrap();
            assert!(matches!(
                refused,
                Accepted::Refused(Refusal::WrongPackage(None))
            ));
        }
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
    fn a_client_that_connects_and_stays_silent_is_dropped_unidentified() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        pipe.identify_patience = Duration::from_millis(300);
        let quiet = std::thread::spawn({
            let name = name.clone();
            move || {
                let file = client(&name).unwrap();
                let mut answer = String::new();
                BufReader::new(file).read_line(&mut answer).unwrap_or(0)
            }
        });
        // The first wait ends with the client connected and still silent.
        let tick = Duration::from_millis(100);
        assert!(matches!(pipe.accept(tick).unwrap(), Accepted::Idle));
        let refused = (0..50).find_map(|_| match pipe.accept(tick).unwrap() {
            Accepted::Idle => None,
            Accepted::Refused(refusal) => Some(refusal),
            Accepted::Client(_) => panic!("a silent client was served"),
        });
        assert_eq!(refused, Some(Refusal::Silent));
        assert_eq!(quiet.join().unwrap(), 0, "the silent client got an answer");
    }

    #[test]
    fn a_client_between_requests_leaves_the_server_free_to_do_other_work() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let _quiet = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut file = client(&name).unwrap();
                // The start of a request, and then nothing more.
                file.write_all(b"{").unwrap();
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

    /// Bytes that are never a newline, more of them than the pipe buffers
    /// each way.
    fn payload() -> Vec<u8> {
        let length = 3 * BUFFER_BYTES as usize + 123;
        (0..length).map(|i| [0, 7, 0xff, 13, 200][i % 5]).collect()
    }

    /// Reads from `session` in reads of at most `max` bytes until `wanted`
    /// bytes came, and returns them.
    fn read_stream(session: &mut Session<'_>, wanted: usize, max: usize) -> Vec<u8> {
        let mut got = Vec::new();
        while got.len() < wanted {
            match session.read(max, 10 * SECOND).unwrap() {
                Received::Data(bytes) => {
                    assert!(!bytes.is_empty() && bytes.len() <= max, "{}", bytes.len());
                    got.extend(bytes);
                }
                other => panic!("the stream ended early: {other:?}"),
            }
        }
        got
    }

    #[test]
    fn a_binary_stream_larger_than_the_pipe_buffer_round_trips() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let sent = payload();
        assert!(!sent.contains(&b'\n') && sent.len() > BUFFER_BYTES as usize);
        let talker = std::thread::spawn({
            let (name, sent) = (name.clone(), sent.clone());
            move || {
                let mut file = client(&name).unwrap();
                file.write_all(&sent).unwrap();
                let mut back = vec![0_u8; sent.len()];
                std::io::Read::read_exact(&mut file, &mut back).unwrap();
                back
            }
        });
        let Accepted::Client(mut session) = pipe.accept(10 * SECOND).unwrap() else {
            panic!("the client was not admitted");
        };
        let got = read_stream(&mut session, sent.len(), 64 * 1024);
        assert_eq!(got, sent, "the server read the stream in order");
        session.write_all(&got).unwrap();
        assert_eq!(talker.join().unwrap(), sent, "the client read it back");
    }

    #[test]
    fn a_raw_read_says_when_nothing_came_and_when_the_client_left() {
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let talker = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut file = client(&name).unwrap();
                file.write_all(b"abc").unwrap();
                std::thread::sleep(Duration::from_millis(600));
            }
        });
        let Accepted::Client(mut session) = pipe.accept(10 * SECOND).unwrap() else {
            panic!("the client was not admitted");
        };
        let first = session.read(2, 10 * SECOND).unwrap();
        assert_eq!(first, Received::Data(b"ab".to_vec()));
        assert_eq!(
            session.read(9, SECOND).unwrap(),
            Received::Data(b"c".to_vec())
        );
        let quiet = session.read(9, Duration::from_millis(50)).unwrap();
        assert_eq!(quiet, Received::Idle);
        assert_eq!(session.read(9, 10 * SECOND).unwrap(), Received::Closed);
        assert!(session.read(0, SECOND).is_err(), "a read of nothing");
        talker.join().unwrap();
    }

    #[test]
    fn a_read_and_a_write_of_a_split_session_are_outstanding_at_once() {
        // The client writes "pong" only after it reads "ping", so the
        // server's read ends only after its write, which starts while the
        // read waits.
        let (name, me) = (unique(), me());
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let talker = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut file = client(&name).unwrap();
                file.write_all(b"hello").unwrap();
                let mut ping = [0_u8; 4];
                std::io::Read::read_exact(&mut file, &mut ping).unwrap();
                assert_eq!(&ping, b"ping");
                file.write_all(b"pong").unwrap();
            }
        });
        let Accepted::Client(mut session) = pipe.accept(10 * SECOND).unwrap() else {
            panic!("the client was not admitted");
        };
        let started = Instant::now();
        let (mut reader, mut writer) = session.split();
        let heard = std::thread::scope(|scope| {
            let listening = scope.spawn(move || {
                let mut heard = Vec::new();
                while heard.len() < b"hellopong".len() {
                    match reader.read(64, 10 * SECOND).unwrap() {
                        Received::Data(bytes) => heard.extend(bytes),
                        other => panic!("the read ended without the answer: {other:?}"),
                    }
                }
                heard
            });
            std::thread::sleep(Duration::from_millis(300));
            writer.write_all(b"ping").unwrap();
            listening.join().unwrap()
        });
        assert_eq!(heard, b"hellopong");
        assert!(started.elapsed() < 5 * SECOND, "{:?}", started.elapsed());
        talker.join().unwrap();
    }
}
