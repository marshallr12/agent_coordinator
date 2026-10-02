//! Candidate pushes through a credential-holding helper.
//!
//! An implementer without push credentials streams a Git bundle of its
//! candidate to a helper that holds them, and the helper publishes it to the
//! one candidate ref fixed when its [`HelperSpec`] is built. Both sides live
//! here and are generic over `Read`/`Write`, so any byte stream can carry
//! them; the tests use a Unix socket pair.
//!
//! Wire protocol, version [`PROTOCOL_VERSION`]:
//!
//! 1. The client sends one JSON line `{"version":1,"revision":R,"tree":T}`
//!    naming the full commit and tree object IDs it is publishing.
//! 2. The client sends the bundle length as a big-endian `u64`, then exactly
//!    that many bundle bytes.
//! 3. The helper answers with one JSON line, either
//!    `{"ok":true,"reference":…,"revision":…,"tree":…,"previous":…}` (with
//!    `previous` the ref's commit before this push, or `null` when this
//!    push creates it) or `{"ok":false,"code":…,"message":…}` with a
//!    [`RefusalCode`].
//!
//! The request never names a ref, and refs carried inside the bundle are
//! ignored: the helper imports the requested commit by object ID and pushes it
//! only to [`HelperSpec::reference`]. Reply messages are fixed text or
//! secret-scan descriptions, never Git output, URLs or credentials.
//!
//! A helper publishes at most one commit: it creates the ref and never moves
//! it. Once its ref names a commit it pushed, a request for that same commit
//! is accepted again without pushing, and a request for any other commit is
//! refused with [`RefusalCode::CandidateAlreadyPublished`]; when the helper
//! has recorded its lease, that refusal comes before any bundle byte is read.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::git_workflow::{
    canonical_git_root, git_ok, git_raw, git_raw_from_file, git_remote_argument, git_text,
    observe_remote_reference_with, outgoing_secret_findings, protect_directory,
    protected_create_new, resolve_exact_commit, validate_full_oid, validate_remote_argument,
};

/// The protocol version a client sends and a helper accepts.
pub const PROTOCOL_VERSION: u32 = 1;
/// The bundle size a helper accepts unless [`HelperSpec::with_max_bundle_bytes`]
/// sets another limit: room for a large candidate while bounding the disk one
/// request can fill.
pub const DEFAULT_MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
/// The namespace every helper-written ref lives under.
pub const CANDIDATE_REF_PREFIX: &str = "refs/agent-coordinator/candidates/";

/// The longest request line: a version and two object IDs need far less, and
/// the bound keeps a peer from making the helper buffer without end.
const MAX_REQUEST_LINE: usize = 4 * 1024;
/// The longest reply line either side handles; [`fit_reply`] keeps every
/// helper reply within it.
const MAX_REPLY_LINE: usize = 64 * 1024;
/// The most bundle bytes read while looking for the header's end, bounding
/// the memory a bundle with a huge prerequisite or tip list can claim.
const MAX_BUNDLE_HEADER: u64 = 1024 * 1024;
/// The longest task or launch id: room for a UUID, while keeping each ref
/// component well under the 255-byte file-name limit loose refs are stored
/// under.
const MAX_ID_LENGTH: usize = 128;
/// The message sent instead of one that would overflow [`MAX_REPLY_LINE`].
const OVERSIZED_MESSAGE: &str = "candidate push refused; the details exceed the reply size limit";

/// Why a helper refused a candidate push. Serialized in `snake_case`; these
/// names are the stable wire codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// The request line is missing, malformed, or names another version.
    BadRequest,
    /// The declared bundle length exceeds the helper's limit.
    TooLarge,
    /// The bundle is malformed, truncated, or needs commits the remote lacks.
    BundleInvalid,
    /// The bundle does not carry the requested commit with the requested tree.
    RevisionMismatch,
    /// A commit the push would send adds a likely secret.
    SecretDetected,
    /// The candidate ref is not one this helper pushed.
    LeaseConflict,
    /// This helper already published a different commit to its ref, and it
    /// publishes only one.
    CandidateAlreadyPublished,
    /// Git could not observe or push the ref, or the remote did not show the
    /// pushed commit.
    PushFailed,
    /// The helper could not prepare its private repository, store the bundle,
    /// run the secret scan, or record its lease or intent.
    Internal,
}

/// A published candidate: the fixed ref, the commit and tree it now names,
/// and its commit before this push (`None` when this push creates it).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushReceipt {
    pub reference: String,
    pub revision: String,
    pub tree: String,
    pub previous: Option<String>,
}

/// A helper's refusal. Returned inside `anyhow::Error` by [`send_candidate`],
/// so callers can `downcast_ref::<PushRefusal>()` to branch on the code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushRefusal {
    pub code: RefusalCode,
    pub message: String,
}

impl fmt::Display for PushRefusal {
    /// Shows the helper's message; the code is available as a field.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PushRefusal {}

/// The single reply line a helper sends for one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PushReply {
    /// The fixed ref names the requested commit, confirmed by readback.
    Accepted(PushReceipt),
    /// The helper refused the request; the refusal's code says why.
    Refused(PushRefusal),
}

impl PushReply {
    /// Encodes the reply as one JSON line, newline included.
    pub fn to_line(&self) -> Vec<u8> {
        let value = match self {
            Self::Accepted(receipt) => json!({
                "ok": true,
                "reference": receipt.reference,
                "revision": receipt.revision,
                "tree": receipt.tree,
                "previous": receipt.previous,
            }),
            Self::Refused(refusal) => json!({
                "ok": false,
                "code": refusal.code,
                "message": refusal.message,
            }),
        };
        let mut line = value.to_string().into_bytes();
        line.push(b'\n');
        line
    }

    /// Decodes one reply line, requiring exactly the fields its `ok` implies.
    pub fn parse(line: &[u8]) -> Result<Self> {
        let wire: WireReply =
            serde_json::from_slice(line).context("candidate push reply is not valid JSON")?;
        match wire {
            WireReply {
                ok: true,
                reference: Some(reference),
                revision: Some(revision),
                tree: Some(tree),
                previous,
                code: None,
                message: None,
            } => Ok(Self::Accepted(PushReceipt {
                reference,
                revision,
                tree,
                previous,
            })),
            WireReply {
                ok: false,
                reference: None,
                revision: None,
                tree: None,
                previous: None,
                code: Some(code),
                message: Some(message),
            } => Ok(Self::Refused(PushRefusal { code, message })),
            _ => bail!("candidate push reply has an unexpected shape"),
        }
    }
}

/// The request line the client sends before the bundle.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PushRequest {
    version: u32,
    revision: String,
    tree: String,
}

/// Either reply shape as it appears on the wire, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReply {
    ok: bool,
    reference: Option<String>,
    revision: Option<String>,
    tree: Option<String>,
    previous: Option<String>,
    code: Option<RefusalCode>,
    message: Option<String>,
}

/// Everything a helper is fixed to at spawn: the remote, the one ref it may
/// write (derived from validated task and launch ids), its private working
/// repository, the credential digests the secret scan refuses, the bundle
/// size limit, and extra environment for Git children that contact the
/// remote (where push credentials are supplied).
#[derive(Clone)]
pub struct HelperSpec {
    remote: String,
    reference: String,
    work_dir: PathBuf,
    known_digests: Vec<String>,
    max_bundle_bytes: u64,
    git_environment: Vec<(String, String)>,
}

impl HelperSpec {
    /// Validates `task` and `launch` as single ref components and fixes the
    /// helper's ref to `refs/agent-coordinator/candidates/<task>/<launch>`.
    /// `work_dir` becomes a bare repository owned by the helper.
    pub fn new(remote: &str, task: &str, launch: &str, work_dir: &Path) -> Result<Self> {
        validate_remote_argument(remote)?;
        validate_ref_component("task", task)?;
        validate_ref_component("launch", launch)?;
        ensure!(
            work_dir.is_absolute(),
            "helper work directory must be an absolute path"
        );
        Ok(Self {
            remote: remote.to_owned(),
            reference: format!("{CANDIDATE_REF_PREFIX}{task}/{launch}"),
            work_dir: work_dir.to_path_buf(),
            known_digests: Vec::new(),
            max_bundle_bytes: DEFAULT_MAX_BUNDLE_BYTES,
            git_environment: Vec::new(),
        })
    }

    /// Sets the lowercase SHA-256 digests of credentials the scan refuses.
    pub fn with_known_digests(mut self, digests: Vec<String>) -> Self {
        self.known_digests = digests;
        self
    }

    /// Sets the largest bundle, in bytes, the helper will accept.
    pub fn with_max_bundle_bytes(mut self, limit: u64) -> Self {
        self.max_bundle_bytes = limit;
        self
    }

    /// Sets environment variables for the Git children that contact the
    /// remote (for example `GIT_ASKPASS` and what it reads).
    pub fn with_git_environment(mut self, environment: Vec<(String, String)>) -> Self {
        self.git_environment = environment;
        self
    }

    /// The only ref this helper ever writes.
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// The Git environment in the borrowed form `git_raw` takes.
    fn environment(&self) -> Vec<(&str, &str)> {
        self.git_environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
    }
}

impl fmt::Debug for HelperSpec {
    /// Omits the remote URL and environment values, which may carry
    /// credentials; environment names are shown.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .git_environment
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        formatter
            .debug_struct("HelperSpec")
            .field("reference", &self.reference)
            .field("work_dir", &self.work_dir)
            .field("known_digests", &self.known_digests.len())
            .field("max_bundle_bytes", &self.max_bundle_bytes)
            .field("git_environment", &names)
            .finish_non_exhaustive()
    }
}

/// Accepts one id as a single ref component: 1 to [`MAX_ID_LENGTH`] bytes of
/// `[A-Za-z0-9._-]`, not starting with `.` or `-`, without `..`, and not
/// ending with `.` or `.lock`.
fn validate_ref_component(kind: &str, value: &str) -> Result<()> {
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
    ensure!(
        (1..=MAX_ID_LENGTH).contains(&value.len())
            && value.bytes().all(allowed)
            && !value.starts_with(['.', '-'])
            && !value.contains("..")
            && !value.ends_with('.')
            && !value.ends_with(".lock"),
        "{kind} id is not a safe single ref component"
    );
    Ok(())
}

/// Streams `revision` from `checkout` to a helper and returns its receipt.
///
/// `prerequisites` are full commit IDs the caller knows the remote already
/// holds (normally the launch's base revision); the bundle omits their
/// history, and the helper fetches the bundle's boundary commits from the
/// remote, so a wrong prerequisite fails closed as `bundle_invalid`. The
/// client never contacts the remote itself. An empty list sends the full
/// history. A refusal is returned as a [`PushRefusal`] error.
pub fn send_candidate<S: Read + Write>(
    stream: &mut S,
    checkout: &Path,
    revision: &str,
    prerequisites: &[&str],
) -> Result<PushReceipt> {
    let checkout = canonical_git_root(checkout)?;
    let revision = resolve_exact_commit(&checkout, revision)?;
    let tree = git_text(&checkout, ["rev-parse", &format!("{revision}^{{tree}}")])?;
    let bundle = OutgoingBundle::create(&checkout, &revision, prerequisites)?;
    let sent = send_request(stream, &revision, &tree, &bundle.path);
    let reply = read_line(stream, MAX_REPLY_LINE).and_then(|line| PushReply::parse(&line));
    match (sent, reply) {
        (_, Ok(PushReply::Accepted(receipt))) => check_receipt(receipt, &revision, &tree),
        (_, Ok(PushReply::Refused(refusal))) => Err(refusal.into()),
        (Err(error), Err(_)) | (Ok(()), Err(error)) => Err(error),
    }
}

/// A bundle file in a private temporary directory, removed on drop.
struct OutgoingBundle {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl OutgoingBundle {
    /// Bundles `revision` minus `prerequisites`. `git bundle create` needs a
    /// ref name for each tip, so a uniquely named temporary ref is created in
    /// the checkout and deleted again whether or not bundling succeeds.
    fn create(checkout: &Path, revision: &str, prerequisites: &[&str]) -> Result<Self> {
        let mut exclusions = Vec::with_capacity(prerequisites.len());
        for prerequisite in prerequisites {
            exclusions.push(format!(
                "^{}",
                resolve_exact_commit(checkout, prerequisite)?
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix("agent-coordinator-candidate-bundle-")
            .tempdir()
            .context("create temporary candidate bundle directory")?;
        let path = directory.path().join("candidate.bundle");
        let tip = format!("refs/agent-coordinator/outgoing/{}", Uuid::new_v4());
        git_ok(checkout, ["update-ref", tip.as_str(), revision])?;
        let bundled = bundle_create(checkout, &path, &tip, &exclusions);
        git_ok(checkout, ["update-ref", "-d", tip.as_str()])?;
        bundled?;
        Ok(Self {
            _directory: directory,
            path,
        })
    }
}

/// Runs `git bundle create <path> <tip> ^<prerequisite>…`.
fn bundle_create(checkout: &Path, path: &Path, tip: &str, exclusions: &[String]) -> Result<()> {
    let arguments = [OsString::from("bundle"), OsString::from("create")]
        .into_iter()
        .chain([path.as_os_str().to_owned(), OsString::from(tip)])
        .chain(exclusions.iter().map(OsString::from));
    let output = git_raw(checkout, arguments, None, &[])?;
    ensure!(
        output.status.success(),
        "Git could not bundle the candidate (it may add no commits beyond its prerequisites)"
    );
    Ok(())
}

/// Writes the request line, the big-endian bundle length and the bundle.
fn send_request<W: Write>(stream: &mut W, revision: &str, tree: &str, bundle: &Path) -> Result<()> {
    let request = PushRequest {
        version: PROTOCOL_VERSION,
        revision: revision.to_owned(),
        tree: tree.to_owned(),
    };
    let mut line = serde_json::to_vec(&request).context("encode candidate push request")?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .context("send candidate push request")?;
    let mut file = File::open(bundle).context("open candidate bundle")?;
    let length = file.metadata().context("inspect candidate bundle")?.len();
    stream
        .write_all(&length.to_be_bytes())
        .context("send candidate bundle length")?;
    io::copy(&mut file, stream).context("send candidate bundle")?;
    stream.flush().context("flush candidate bundle")
}

/// Accepts a receipt only for the commit and tree this client sends, under the
/// candidate namespace.
fn check_receipt(receipt: PushReceipt, revision: &str, tree: &str) -> Result<PushReceipt> {
    ensure!(
        receipt.revision == revision
            && receipt.tree == tree
            && receipt.reference.starts_with(CANDIDATE_REF_PREFIX),
        "candidate push helper acknowledged a different commit, tree or ref"
    );
    Ok(receipt)
}

/// Reads bytes up to a newline (excluded), refusing more than `limit` bytes
/// or end of stream first. Reads one byte at a time so nothing after the
/// newline is consumed.
fn read_line<R: Read>(stream: &mut R, limit: usize) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        stream.read_exact(&mut byte).context("read protocol line")?;
        if byte[0] == b'\n' {
            return Ok(line);
        }
        ensure!(line.len() < limit, "protocol line is too long");
        line.push(byte[0]);
    }
}

/// A step's result on the helper side: a refusal becomes the reply.
type Step<T> = std::result::Result<T, PushRefusal>;

/// Builds a refusal with fixed text.
fn refusal(code: RefusalCode, message: &str) -> PushRefusal {
    PushRefusal {
        code,
        message: message.to_owned(),
    }
}

/// Maps an internal error to a refusal with fixed text, so error chains
/// (which can mention paths or remote URLs) never reach the client.
trait Refuse<T> {
    fn refuse(self, code: RefusalCode, message: &str) -> Step<T>;
}

impl<T, E> Refuse<T> for std::result::Result<T, E> {
    /// Replaces any error with the given code and message.
    fn refuse(self, code: RefusalCode, message: &str) -> Step<T> {
        self.map_err(|_| refusal(code, message))
    }
}

/// Serves one request on `stream` for the helper fixed by `spec` and sends
/// exactly one reply line, which is also returned. The error case is only a
/// reply that could not be written. One helper serves its connections one at
/// a time: concurrent calls sharing a work directory are not supported.
pub fn serve_one<S: Read + Write>(stream: &mut S, spec: &HelperSpec) -> Result<PushReply> {
    let reply = fit_reply(match receive_and_publish(stream, spec) {
        Ok(receipt) => PushReply::Accepted(receipt),
        Err(refusal) => PushReply::Refused(refusal),
    });
    stream
        .write_all(&reply.to_line())
        .and_then(|()| stream.flush())
        .context("send candidate push reply")?;
    Ok(reply)
}

/// Keeps a reply within [`MAX_REPLY_LINE`] by replacing an oversized refusal
/// message with [`OVERSIZED_MESSAGE`]; the code is kept.
fn fit_reply(reply: PushReply) -> PushReply {
    match reply {
        PushReply::Refused(refused) if reply_too_long(&refused) => {
            PushReply::Refused(refusal(refused.code, OVERSIZED_MESSAGE))
        }
        other => other,
    }
}

/// Whether `refusal` encodes to a line longer than [`MAX_REPLY_LINE`].
fn reply_too_long(refusal: &PushRefusal) -> bool {
    PushReply::Refused(refusal.clone()).to_line().len() > MAX_REPLY_LINE
}

/// Reads the request, refuses a second commit, then reads the bundle and
/// imports, scans and publishes it.
fn receive_and_publish<R: Read>(stream: &mut R, spec: &HelperSpec) -> Step<PushReceipt> {
    let request = read_request(stream)?;
    let length = read_length(stream, spec.max_bundle_bytes)?;
    let helper = Helper::open(spec)?;
    helper.refuse_other_than_leased(&request)?;
    let bundle = helper.receive_bundle(stream, length)?;
    let quarantine = helper.import(&bundle, &request)?;
    helper.scan(&request.revision)?;
    let receipt = helper.publish(&request);
    drop(quarantine);
    receipt
}

/// Reads and validates the request line, normalising object IDs to lowercase.
fn read_request<R: Read>(stream: &mut R) -> Step<PushRequest> {
    const MALFORMED: &str = "request line is missing, too long, or not a version 1 request";
    let line = read_line(stream, MAX_REQUEST_LINE).refuse(RefusalCode::BadRequest, MALFORMED)?;
    let mut request: PushRequest =
        serde_json::from_slice(&line).refuse(RefusalCode::BadRequest, MALFORMED)?;
    if request.version != PROTOCOL_VERSION {
        return Err(refusal(
            RefusalCode::BadRequest,
            "unsupported candidate push protocol version",
        ));
    }
    validate_full_oid(&request.revision)
        .and_then(|()| validate_full_oid(&request.tree))
        .refuse(
            RefusalCode::BadRequest,
            "revision and tree must be full object IDs",
        )?;
    request.revision.make_ascii_lowercase();
    request.tree.make_ascii_lowercase();
    Ok(request)
}

/// Reads the big-endian bundle length and enforces the helper's limit before
/// any bundle byte is read.
fn read_length<R: Read>(stream: &mut R, limit: u64) -> Step<u64> {
    let mut bytes = [0_u8; 8];
    stream
        .read_exact(&mut bytes)
        .refuse(RefusalCode::BadRequest, "bundle length is missing")?;
    let length = u64::from_be_bytes(bytes);
    if length > limit {
        return Err(refusal(
            RefusalCode::TooLarge,
            "bundle exceeds the helper's size limit",
        ));
    }
    Ok(length)
}

/// The helper's private bare repository and the remote it pushes to.
struct Helper<'a> {
    spec: &'a HelperSpec,
    remote: OsString,
}

/// A received bundle file in the work directory, removed on drop.
struct IncomingBundle(PathBuf);

impl Drop for IncomingBundle {
    /// Deletes the received bundle; a failure leaves only a private file.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A local ref holding the imported commit until publication ends.
struct Quarantine<'a> {
    work_dir: &'a Path,
    reference: String,
}

impl Drop for Quarantine<'_> {
    /// Deletes the quarantine ref; a failure leaves only a private ref.
    fn drop(&mut self) {
        let _ = git_ok(self.work_dir, ["update-ref", "-d", self.reference.as_str()]);
    }
}

impl<'a> Helper<'a> {
    /// Creates (or reopens) the private bare repository, without template
    /// hooks, and resolves the remote argument.
    fn open(spec: &'a HelperSpec) -> Step<Self> {
        const FAILED: &str = "helper could not prepare its private repository";
        fs::create_dir_all(&spec.work_dir).refuse(RefusalCode::Internal, FAILED)?;
        protect_directory(&spec.work_dir).refuse(RefusalCode::Internal, FAILED)?;
        git_ok(
            &spec.work_dir,
            ["init", "--bare", "--quiet", "--template=", "."],
        )
        .refuse(RefusalCode::Internal, FAILED)?;
        let remote = git_remote_argument(&spec.work_dir, &spec.remote)
            .refuse(RefusalCode::Internal, FAILED)?;
        Ok(Self { spec, remote })
    }

    /// Refuses a request for any commit other than this helper's recorded
    /// lease, the commit it already published. Without a lease (before the
    /// first confirmed push) every request passes.
    fn refuse_other_than_leased(&self, request: &PushRequest) -> Step<()> {
        match self.read_local_ref("leases")? {
            Some(lease) if lease != request.revision => Err(already_published()),
            _ => Ok(()),
        }
    }

    /// Copies exactly `length` bundle bytes into a new private file.
    fn receive_bundle<R: Read>(&self, stream: &mut R, length: u64) -> Step<IncomingBundle> {
        let path = self
            .spec
            .work_dir
            .join(format!("incoming-{}.bundle", Uuid::new_v4()));
        let mut file = protected_create_new(&path)
            .refuse(RefusalCode::Internal, "helper could not store the bundle")?;
        let bundle = IncomingBundle(path);
        let copied = io::copy(&mut stream.take(length), &mut file)
            .refuse(RefusalCode::BundleInvalid, "bundle could not be received")?;
        if copied != length {
            return Err(refusal(
                RefusalCode::BundleInvalid,
                "bundle ended before its declared length",
            ));
        }
        Ok(bundle)
    }

    /// Fetches the bundle's prerequisites from the remote, verifies the
    /// bundle and its objects, and imports the requested commit by object ID
    /// into a quarantine ref, checking its commit and tree. Bundle ref names
    /// are never used.
    fn import(&self, bundle: &IncomingBundle, request: &PushRequest) -> Step<Quarantine<'a>> {
        let header = read_bundle_header(&bundle.0)
            .refuse(RefusalCode::BundleInvalid, "bundle header is malformed")?;
        self.fetch_prerequisites(&header.prerequisites)?;
        git_ok(
            &self.spec.work_dir,
            [
                OsString::from("bundle"),
                OsString::from("verify"),
                bundle.0.clone().into(),
            ],
        )
        .refuse(RefusalCode::BundleInvalid, "bundle failed verification")?;
        if !header.tips.contains(&request.revision) {
            return Err(refusal(
                RefusalCode::RevisionMismatch,
                "bundle does not carry the requested revision as a tip",
            ));
        }
        self.check_objects(bundle, header.pack_offset)?;
        let quarantine = self.fetch_from_bundle(bundle, &request.revision)?;
        self.check_identity(&quarantine, request)?;
        Ok(quarantine)
    }

    /// Fetches each prerequisite commit the private repository lacks from the
    /// remote, keeping it under a local ref.
    fn fetch_prerequisites(&self, prerequisites: &[String]) -> Step<()> {
        let missing: Vec<&String> = prerequisites
            .iter()
            .filter(|oid| !has_commit(&self.spec.work_dir, oid))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let refspecs = missing
            .iter()
            .map(|oid| OsString::from(format!("{oid}:refs/agent-coordinator/prerequisites/{oid}")));
        let arguments = fetch_arguments(self.remote.clone()).chain(refspecs);
        let fetched = git_raw(
            &self.spec.work_dir,
            arguments,
            None,
            &self.spec.environment(),
        );
        match fetched {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err(refusal(
                RefusalCode::BundleInvalid,
                "the remote does not provide the bundle's prerequisite commits",
            )),
        }
    }

    /// Runs the bundle's pack through `git index-pack --strict`, which checks
    /// every object the way a receiving server does (refusing, for example, a
    /// tree entry named `.git`). A bundle fetch does not apply
    /// `fetch.fsckObjects`, so this is the object check.
    fn check_objects(&self, bundle: &IncomingBundle, pack_offset: u64) -> Step<()> {
        const MALFORMED: &str = "bundle contains malformed objects";
        let mut pack = File::open(&bundle.0).refuse(RefusalCode::Internal, MALFORMED)?;
        pack.seek(SeekFrom::Start(pack_offset))
            .refuse(RefusalCode::BundleInvalid, MALFORMED)?;
        let arguments = ["index-pack", "--stdin", "--fix-thin", "--strict"].map(OsString::from);
        match git_raw_from_file(&self.spec.work_dir, arguments, pack, &[]) {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err(refusal(RefusalCode::BundleInvalid, MALFORMED)),
        }
    }

    /// Imports `revision` from the bundle into a fresh quarantine ref.
    fn fetch_from_bundle(&self, bundle: &IncomingBundle, revision: &str) -> Step<Quarantine<'a>> {
        let quarantine = Quarantine {
            work_dir: &self.spec.work_dir,
            reference: format!("refs/agent-coordinator/quarantine/{}", Uuid::new_v4()),
        };
        let refspec = format!("{revision}:{}", quarantine.reference);
        let arguments =
            fetch_arguments(bundle.0.clone().into_os_string()).chain([OsString::from(refspec)]);
        match git_raw(&self.spec.work_dir, arguments, None, &[]) {
            Ok(output) if output.status.success() => Ok(quarantine),
            _ => Err(refusal(
                RefusalCode::BundleInvalid,
                "the requested revision could not be imported from the bundle",
            )),
        }
    }

    /// Requires the quarantined commit and its tree to be exactly the
    /// requested object IDs.
    fn check_identity(&self, quarantine: &Quarantine<'_>, request: &PushRequest) -> Step<()> {
        let peel = |kind: &str| {
            git_text(
                &self.spec.work_dir,
                ["rev-parse", &format!("{}^{{{kind}}}", quarantine.reference)],
            )
        };
        match (peel("commit"), peel("tree")) {
            (Ok(revision), Ok(tree)) if revision == request.revision && tree == request.tree => {
                Ok(())
            }
            _ => Err(refusal(
                RefusalCode::RevisionMismatch,
                "the bundled commit or its tree differs from the request",
            )),
        }
    }

    /// Runs the candidate-push secret scan over every commit a push of
    /// `revision` would send, bounded by what the remote advertises.
    fn scan(&self, revision: &str) -> Step<()> {
        let digests: Vec<&str> = self.spec.known_digests.iter().map(String::as_str).collect();
        let findings = outgoing_secret_findings(
            &self.spec.work_dir,
            self.remote.clone(),
            revision,
            &digests,
            &self.spec.environment(),
        )
        .refuse(
            RefusalCode::Internal,
            "helper could not scan the outgoing commits",
        )?;
        if findings.is_empty() {
            return Ok(());
        }
        Err(PushRefusal {
            code: RefusalCode::SecretDetected,
            message: crate::secret_scan::describe(&findings),
        })
    }

    /// Publishes `request.revision` to the fixed ref. The remote ref must be
    /// one this helper owns (see [`Helper::owns`]), so a ref created or moved
    /// by anyone else is never overwritten, and is adopted only when it names
    /// the revision this helper intends to push. An absent ref is created
    /// after the intent is recorded; an owned ref naming the requested
    /// revision is accepted without pushing; an owned ref naming another
    /// commit is refused as [`RefusalCode::CandidateAlreadyPublished`].
    fn publish(&self, request: &PushRequest) -> Step<PushReceipt> {
        let observed = self.observe()?;
        if !self.owns(observed.as_deref())? {
            return Err(refusal(
                RefusalCode::LeaseConflict,
                "the candidate ref is not one this helper pushed",
            ));
        }
        match observed.as_deref() {
            None => {
                git_ok(
                    &self.spec.work_dir,
                    ["update-ref", &self.local_ref("intents"), &request.revision],
                )
                .refuse(RefusalCode::Internal, "helper could not record its intent")?;
                self.push(&request.revision)?;
            }
            Some(current) if current == request.revision => {}
            Some(_) => return Err(already_published()),
        }
        self.confirm(request, observed)
    }

    /// Whether the remote value `observed` is this helper's: equal to its
    /// lease (its confirmed push, or absent before it), or to its
    /// recorded intent, which covers a push that reaches the remote while the
    /// lease write after it fails.
    fn owns(&self, observed: Option<&str>) -> Step<bool> {
        if observed == self.read_local_ref("leases")?.as_deref() {
            return Ok(true);
        }
        let intent = self.read_local_ref("intents")?;
        Ok(observed.is_some() && observed == intent.as_deref())
    }

    /// Creates the fixed ref at `revision`, with a `--force-with-lease` that
    /// requires it to be absent. A failed push whose ref now names `revision`
    /// counts as pushed; one whose ref now names another commit is a lease
    /// conflict, and one whose ref is still absent failed. Both definite
    /// failures clear the intent; an observation error keeps it, since the
    /// push may have landed.
    fn push(&self, revision: &str) -> Step<()> {
        let reference = self.spec.reference();
        let arguments = [
            OsString::from("push"),
            OsString::from("--no-verify"),
            OsString::from("--no-follow-tags"),
            OsString::from(format!("--force-with-lease={reference}:")),
            self.remote.clone(),
            OsString::from(format!("{revision}:{reference}")),
        ];
        let pushed = git_raw(
            &self.spec.work_dir,
            arguments,
            None,
            &self.spec.environment(),
        );
        if matches!(&pushed, Ok(output) if output.status.success()) {
            return Ok(());
        }
        let now = self.observe()?;
        if now.as_deref() == Some(revision) {
            return Ok(());
        }
        self.clear_intent()?;
        if now.is_some() {
            return Err(refusal(
                RefusalCode::LeaseConflict,
                "the candidate ref moved while this helper pushed it",
            ));
        }
        Err(refusal(
            RefusalCode::PushFailed,
            "Git could not push the candidate",
        ))
    }

    /// Reads the remote ref back, requires exactly `request.revision`, records
    /// it as this helper's lease, and then clears the intent. A readback
    /// naming another commit also clears the intent.
    fn confirm(&self, request: &PushRequest, previous: Option<String>) -> Step<PushReceipt> {
        if self.observe()?.as_deref() != Some(request.revision.as_str()) {
            self.clear_intent()?;
            return Err(refusal(
                RefusalCode::PushFailed,
                "the remote does not show the pushed candidate",
            ));
        }
        git_ok(
            &self.spec.work_dir,
            ["update-ref", &self.local_ref("leases"), &request.revision],
        )
        .refuse(RefusalCode::Internal, "helper could not record its lease")?;
        self.clear_intent()?;
        Ok(PushReceipt {
            reference: self.spec.reference.clone(),
            revision: request.revision.clone(),
            tree: request.tree.clone(),
            previous,
        })
    }

    /// Deletes the intent ref, once a push's outcome is known.
    fn clear_intent(&self) -> Step<()> {
        git_ok(
            &self.spec.work_dir,
            ["update-ref", "-d", &self.local_ref("intents")],
        )
        .refuse(RefusalCode::Internal, "helper could not clear its intent")
    }

    /// The commit the remote advertises for the fixed ref, if any.
    fn observe(&self) -> Step<Option<String>> {
        observe_remote_reference_with(
            &self.spec.work_dir,
            self.remote.clone(),
            self.spec.reference(),
            &self.spec.environment(),
        )
        .refuse(
            RefusalCode::PushFailed,
            "helper could not observe the candidate ref",
        )
    }

    /// The commit a local bookkeeping ref (`leases` or `intents`) names for
    /// this helper's fixed ref, or `None` when it is absent. Kept as refs in
    /// the private repository so they survive helper restarts.
    fn read_local_ref(&self, namespace: &str) -> Step<Option<String>> {
        let name = format!("{}^{{commit}}", self.local_ref(namespace));
        let arguments = ["rev-parse", "--verify", "--quiet", &name].map(OsString::from);
        let output = git_raw(&self.spec.work_dir, arguments, None, &[]).refuse(
            RefusalCode::Internal,
            "helper could not read its bookkeeping refs",
        )?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8(output.stdout).refuse(
            RefusalCode::Internal,
            "helper could not read its bookkeeping refs",
        )?;
        Ok(Some(text.trim().to_owned()))
    }

    /// The local ref `refs/agent-coordinator/<namespace>/<task>/<launch>`.
    fn local_ref(&self, namespace: &str) -> String {
        let suffix = &self.spec.reference[CANDIDATE_REF_PREFIX.len()..];
        format!("refs/agent-coordinator/{namespace}/{suffix}")
    }
}

/// The refusal for a request naming a commit other than the one this helper
/// already published.
fn already_published() -> PushRefusal {
    refusal(
        RefusalCode::CandidateAlreadyPublished,
        "this helper already published a different commit and publishes only one",
    )
}

/// `git fetch` arguments that write no tags and no `FETCH_HEAD`, followed by
/// `source`.
fn fetch_arguments(source: OsString) -> impl Iterator<Item = OsString> {
    ["fetch", "--no-tags", "--no-write-fetch-head"]
        .map(OsString::from)
        .into_iter()
        .chain([source])
}

/// Whether `repository` holds `oid` as a commit.
fn has_commit(repository: &Path, oid: &str) -> bool {
    let object = format!("{oid}^{{commit}}");
    git_ok(repository, ["cat-file", "-e", object.as_str()]).is_ok()
}

/// The object IDs a bundle header lists (prerequisite commits and tips) and
/// the byte offset where its pack data starts.
#[derive(Debug, Default)]
struct BundleHeader {
    prerequisites: Vec<String>,
    tips: Vec<String>,
    pack_offset: u64,
}

/// Parses a v2 or v3 bundle header (signature line, then capability,
/// prerequisite and tip lines up to a blank line), reading at most
/// [`MAX_BUNDLE_HEADER`] bytes.
fn read_bundle_header(path: &Path) -> Result<BundleHeader> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BUNDLE_HEADER)
        .read_to_end(&mut bytes)?;
    let end = bytes
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .context("bundle header has no end")?;
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut lines = text.lines();
    ensure!(
        matches!(lines.next(), Some("# v2 git bundle" | "# v3 git bundle")),
        "not a Git bundle"
    );
    let mut header = BundleHeader {
        pack_offset: end as u64 + 2,
        ..BundleHeader::default()
    };
    for line in lines.filter(|line| !line.starts_with('@')) {
        let (list, rest) = match line.strip_prefix('-') {
            Some(rest) => (&mut header.prerequisites, rest),
            None => (&mut header.tips, line),
        };
        let oid = rest.split(' ').next().unwrap_or_default();
        validate_full_oid(oid)?;
        list.push(oid.to_ascii_lowercase());
    }
    Ok(header)
}

#[cfg(all(test, unix))]
mod tests;
