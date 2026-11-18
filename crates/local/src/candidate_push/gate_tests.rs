//! The two gate calls of [`serve_one_gated`], on every platform: the
//! Windows publication broker relies on them, and the other push tests run
//! only on Unix.
use super::*;
use std::cell::Cell;
use std::io::Cursor;
use std::process::Command;

/// Runs Git in `directory` and returns its trimmed output.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output();
    let output = output.unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A client's bytes read from memory and the helper's reply kept.
struct Duplex {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl Read for Duplex {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.input.read(buffer)
    }
}

impl Write for Duplex {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A bare remote holding a base commit, and a whole request for a
/// candidate on it whose bundle carries the ref `refs/heads/evil`.
struct Fixture {
    _directory: tempfile::TempDir,
    remote: PathBuf,
    work: PathBuf,
    request: Vec<u8>,
    /// The bytes of the request line and the length, before the bundle.
    head: u64,
}

/// Commits `name` in `checkout` and returns the commit.
fn commit(checkout: &Path, name: &str) -> String {
    commit_holding(checkout, name, name)
}

/// Commits a file `name` holding `content` and returns the commit.
fn commit_holding(checkout: &Path, name: &str, content: &str) -> String {
    std::fs::write(checkout.join(name), content).unwrap();
    git(checkout, &["add", "--", name]);
    let identity = ["-c", "user.name=T", "-c", "user.email=t@example.invalid"];
    let mut arguments = identity.to_vec();
    arguments.extend(["commit", "--quiet", "-m", name]);
    git(checkout, &arguments);
    git(checkout, &["rev-parse", "HEAD"])
}

/// Builds the remote and the request.
fn fixture() -> Fixture {
    fixture_holding("candidate")
}

/// As [`fixture`], the candidate's one new file holding `content`.
fn fixture_holding(content: &str) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let (remote, checkout) = (root.join("remote.git"), root.join("checkout"));
    git(
        &root,
        &["init", "--bare", "--quiet", remote.to_str().unwrap()],
    );
    git(
        &root,
        &[
            "clone",
            "--quiet",
            remote.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    let base = commit(&checkout, "base");
    git(
        &checkout,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );
    let candidate = commit_holding(&checkout, "candidate", content);
    let tree = git(&checkout, &["rev-parse", "HEAD^{tree}"]);
    git(&checkout, &["update-ref", "refs/heads/evil", &candidate]);
    let bundle = root.join("candidate.bundle");
    let excluded = format!("^{base}");
    git(
        &checkout,
        &[
            "bundle",
            "create",
            bundle.to_str().unwrap(),
            "refs/heads/evil",
            &excluded,
        ],
    );
    let bytes = std::fs::read(&bundle).unwrap();
    let line = json!({"version": 1, "revision": candidate, "tree": tree});
    let mut request = format!("{line}\n").into_bytes();
    request.extend((bytes.len() as u64).to_be_bytes());
    let head = request.len() as u64;
    request.extend(bytes);
    let work = root.join("work");
    Fixture {
        _directory: directory,
        remote,
        work,
        request,
        head,
    }
}

impl Fixture {
    /// Serves the request through `gate`: the reply and how many request
    /// bytes the helper read.
    fn serve(&self, gate: Gate<'_>) -> (PushReply, u64) {
        let spec = HelperSpec::new(
            self.remote.to_str().unwrap(),
            "task-1",
            "launch-1",
            &self.work,
        );
        let input = Cursor::new(self.request.clone());
        let mut stream = Duplex {
            input,
            output: Vec::new(),
        };
        let reply = serve_one_gated(&mut stream, &spec.unwrap(), gate).unwrap();
        (reply, stream.input.position())
    }

    /// The remote's refs, one name per line.
    fn refs(&self) -> String {
        git(&self.remote, &["for-each-ref", "--format=%(refname)"])
    }
}

/// The refusal a gate gives.
fn denied() -> PushRefusal {
    refusal(RefusalCode::NotAuthorized, "not now")
}

/// Whether `reply` is a `not_authorized` refusal.
fn not_authorized(reply: &PushReply) -> bool {
    matches!(reply, PushReply::Refused(refused) if refused.code == RefusalCode::NotAuthorized)
}

#[test]
fn an_open_gate_is_asked_twice_and_only_the_fixed_ref_is_published() {
    let fixture = fixture();
    let asked = Cell::new(0);
    let gate = || {
        asked.set(asked.get() + 1);
        Ok(())
    };
    let (reply, _) = fixture.serve(&gate);
    assert!(matches!(reply, PushReply::Accepted(_)), "{reply:?}");
    assert_eq!(asked.get(), 2);
    let refs = fixture.refs();
    assert!(
        refs.contains("refs/agent-coordinator/candidates/task-1/launch-1"),
        "{refs}"
    );
    assert!(!refs.contains("evil"), "{refs}");
}

#[test]
fn a_first_refusal_comes_before_any_bundle_byte_is_read() {
    let fixture = fixture();
    let asked = Cell::new(0);
    let gate = || {
        asked.set(asked.get() + 1);
        Err(denied())
    };
    let (reply, read) = fixture.serve(&gate);
    assert!(not_authorized(&reply), "{reply:?}");
    assert_eq!((asked.get(), read), (1, fixture.head));
    assert!(!fixture.refs().contains("refs/agent-coordinator/"));
    assert!(
        !fixture.work.exists(),
        "the helper prepared its repository first"
    );
}

#[test]
fn a_refusal_just_before_publishing_publishes_nothing() {
    let fixture = fixture();
    let asked = Cell::new(0);
    let gate = || {
        asked.set(asked.get() + 1);
        if asked.get() == 1 {
            Ok(())
        } else {
            Err(denied())
        }
    };
    let (reply, read) = fixture.serve(&gate);
    assert!(not_authorized(&reply), "{reply:?}");
    assert_eq!((asked.get(), read), (2, fixture.request.len() as u64));
    assert!(
        !fixture.refs().contains("refs/agent-coordinator/"),
        "{}",
        fixture.refs()
    );
}

#[test]
fn the_second_gate_comes_only_after_the_import_and_the_scan() {
    // A candidate the secret scan refuses is never asked about twice.
    let fixture = fixture_holding(&format!("key = AKIA{}\n", "ABCDEFGHIJKLMNOP"));
    let asked = Cell::new(0);
    let gate = || {
        asked.set(asked.get() + 1);
        Ok(())
    };
    let (reply, _) = fixture.serve(&gate);
    let refused = matches!(&reply, PushReply::Refused(r) if r.code == RefusalCode::SecretDetected);
    assert!(refused, "{reply:?}");
    assert_eq!(asked.get(), 1);
}

#[test]
fn at_the_second_gate_the_commit_is_already_imported() {
    let fixture = fixture();
    let revision: serde_json::Value =
        serde_json::from_slice(fixture.request.split(|byte| *byte == b'\n').next().unwrap())
            .unwrap();
    let object = format!("{}^{{commit}}", revision["revision"].as_str().unwrap());
    let asked = Cell::new(0);
    let gate = || {
        asked.set(asked.get() + 1);
        let imported = git_ok(&fixture.work, ["cat-file", "-e", object.as_str()]).is_ok();
        assert_eq!(imported, asked.get() == 2, "ask {}", asked.get());
        Ok(())
    };
    let (reply, _) = fixture.serve(&gate);
    assert!(matches!(reply, PushReply::Accepted(_)), "{reply:?}");
}
