//! Test fixtures for `adopt`: a throwaway Git repository, a scripted host,
//! an output capture, and a loopback stand-in for the service that records
//! every request it receives.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::host::Host;
use crate::Failure;

/// A temporary Git repository with a local identity and signing switched off.
pub struct TestRepo {
    /// Held only so the directory lives as long as the repository is used.
    _directory: tempfile::TempDir,
    root: PathBuf,
}

impl TestRepo {
    /// Creates an empty repository on branch `main`.
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let repo = Self {
            _directory: directory,
            root,
        };
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        repo.git(&["config", "user.name", "Adopt Test"]);
        repo.git(&["config", "user.email", "adopt@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo
    }

    /// The repository root, with symlinks resolved as Git reports it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs Git in the repository and returns stdout; panics with stderr on failure.
    pub fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Writes a file relative to the root, creating its directories.
    pub fn write(&self, name: &str, content: &str) {
        let path = self.root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Reads a file relative to the root.
    pub fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).unwrap()
    }

    /// Writes a file and commits it, so it counts as clean.
    pub fn commit_file(&self, name: &str, content: &str) {
        self.write(name, content);
        self.git(&["add", "--", name]);
        self.git(&[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            &format!("Add {name}"),
            "--",
            name,
        ]);
    }

    /// Installs an executable Git hook.
    pub fn write_hook(&self, name: &str, script: &str) {
        let path = self.root.join(".git/hooks").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Every file under the root outside `.git`, as `(relative path, bytes)`.
    pub fn files(&self) -> Vec<(String, Vec<u8>)> {
        let mut found = Vec::new();
        collect_files(&self.root, &self.root, &mut found);
        found.sort();
        found
    }
}

/// Walks a directory tree, skipping `.git`.
fn collect_files(root: &Path, directory: &Path, found: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(directory).unwrap().flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, found);
        } else if let Ok(bytes) = std::fs::read(&path) {
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            found.push((relative, bytes));
        }
    }
}

/// A cloneable in-memory writer, so a test can read what a reporter wrote.
#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// Everything written so far, as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A scripted machine: a fixed credential, queued answers and a chosen `PATH`.
pub struct TestHost {
    /// The credential the "CLI already holds"; `None` models a workstation without one.
    pub token: Option<String>,
    pub interactive: bool,
    /// Answers given to prompts, in order; running out models a closed terminal.
    pub answers: VecDeque<String>,
    /// Every prompt shown, in order.
    pub prompts: Vec<String>,
    /// Program names found on `PATH`.
    pub programs: Vec<String>,
    /// Whether the client is compatible with the service.
    pub compatible: bool,
}

impl TestHost {
    /// An interactive host holding `token`, with the client on `PATH` and no answers queued.
    pub fn new(token: &str) -> Self {
        Self {
            token: Some(token.to_owned()),
            interactive: true,
            answers: VecDeque::new(),
            prompts: Vec::new(),
            programs: vec!["agent-coordinator".to_owned()],
            compatible: true,
        }
    }

    /// Queues the answers the person will type.
    pub fn answering(mut self, answers: &[&str]) -> Self {
        self.answers = answers.iter().map(|answer| (*answer).to_owned()).collect();
        self
    }

    /// Models a run without a terminal.
    pub fn silent(mut self) -> Self {
        self.interactive = false;
        self
    }
}

impl Host for TestHost {
    fn token(&self, _origin: &str, _project_name: Option<&str>) -> anyhow::Result<String> {
        self.token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no credential is configured"))
    }

    fn credential_location(&self) -> String {
        "/home/test/.config/agent-coordinator/credentials.toml".to_owned()
    }

    fn interactive(&self) -> bool {
        self.interactive
    }

    fn ask(&mut self, prompt: &str) -> io::Result<String> {
        self.prompts.push(prompt.to_owned());
        self.answers
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no more answers"))
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        self.programs
            .iter()
            .any(|program| program == name)
            .then(|| PathBuf::from(format!("/usr/bin/{name}")))
    }

    fn compatibility(&self, _info: &Value) -> Result<Value, Failure> {
        if self.compatible {
            Ok(json!({"compatible": true}))
        } else {
            Err(crate::upgrade_required(json!({"compatible": false})))
        }
    }
}

/// One request the fake service received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// The path with its query string.
    pub target: String,
}

/// What the fake service knows and has seen.
#[derive(Default)]
pub struct ServiceState {
    /// The only token it accepts.
    pub token: String,
    /// The actor returned for that token.
    pub actor: Value,
    /// Project list items, in ID order.
    pub projects: Vec<Value>,
    /// Items per project page, to exercise paging.
    pub page_size: usize,
    pub seen: Vec<Seen>,
}

/// A loopback HTTP stand-in for the service: info, authentication help, `me` and projects.
pub struct FakeService {
    pub origin: String,
    pub state: Arc<Mutex<ServiceState>>,
}

impl FakeService {
    /// Starts the service on a free loopback port, accepting `token`.
    pub fn start(token: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let actor = json!({"id": "agent-1", "kind": "agent", "name": "test", "role": "agent",
            "credential_attributes": {"access": "write", "class": "interactive"}});
        let state = ServiceState {
            token: token.to_owned(),
            actor,
            page_size: 200,
            ..ServiceState::default()
        };
        let state = Arc::new(Mutex::new(state));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(stream, &shared);
            }
        });
        Self { origin, state }
    }

    /// Adds a project and returns its ID.
    pub fn add_project(&self, id: &str, name: &str, repository_url: &str) -> String {
        let project = json!({"id": id, "name": name, "repository_url": repository_url, "target_branch": "main"});
        self.state.lock().unwrap().projects.push(project);
        id.to_owned()
    }

    /// Every request received so far.
    pub fn seen(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }
}

/// Reads one request, records it and answers it.
fn serve(mut stream: TcpStream, state: &Mutex<ServiceState>) {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => raw.extend_from_slice(&chunk[..count]),
        }
    }
    let head = String::from_utf8_lossy(&raw).into_owned();
    let mut request = head.lines().next().unwrap_or_default().split(' ');
    let (method, target) = (
        request.next().unwrap_or_default(),
        request.next().unwrap_or_default(),
    );
    let authorization = head
        .lines()
        .filter_map(|line| line.split_once(": "))
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.to_owned());
    let (status, mut body) = answer(state, method, target, authorization.as_deref());
    body["request_id"] = json!("request");
    body["server_time"] = json!("2026-01-01T00:00:00.000Z");
    let payload = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
}

/// Routes one request the way the real service would for an agent credential.
fn answer(
    state: &Mutex<ServiceState>,
    method: &str,
    target: &str,
    authorization: Option<&str>,
) -> (u16, Value) {
    let mut state = state.lock().unwrap();
    state.seen.push(Seen {
        method: method.to_owned(),
        target: target.to_owned(),
    });
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let authorized = authorization == Some(format!("Bearer {}", state.token).as_str());
    match (method, path) {
        ("GET", "/api/v1/info") => (
            200,
            json!({"data": {"version": "test", "client_compatibility": {}}}),
        ),
        ("GET", "/api/v1/help/authentication") => {
            (200, json!({"data": {"steps": ["Ask an administrator."]}}))
        }
        ("GET", "/api/v1/me" | "/api/v1/projects") if !authorized => (
            401,
            json!({"error": {"code": "authentication_required", "message": "Sign in."}}),
        ),
        ("GET", "/api/v1/me") => (
            200,
            json!({"data": {"actor": state.actor, "csrf_token": null}}),
        ),
        ("GET", "/api/v1/projects") => (200, project_page(&state, query)),
        _ => (
            404,
            json!({"error": {"code": "not_found", "message": "No such route."}}),
        ),
    }
}

/// Returns one page of projects after the `cursor` in the query.
fn project_page(state: &ServiceState, query: &str) -> Value {
    let cursor = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("cursor="));
    let start = cursor
        .and_then(|cursor| {
            state
                .projects
                .iter()
                .position(|project| project["id"] == cursor)
        })
        .map_or(0, |index| index + 1);
    let end = (start + state.page_size).min(state.projects.len());
    let next = (end < state.projects.len()).then(|| state.projects[end - 1]["id"].clone());
    json!({"data": {"items": state.projects[start..end], "next_cursor": next}})
}
