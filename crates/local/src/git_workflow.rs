//! Native Git evidence and publication primitives for the completion workflow.
//!
//! The coordinator service never runs these commands. A trusted workstation
//! captures exact clean snapshots, prepares an integration result in an isolated
//! worktree, and publishes it with a remote-side compare-and-swap. Publication
//! intent is durable: every retry observes the remote before fresh service
//! authority is requested or another push is attempted.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const INTENT_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanSnapshot {
    pub checkout: PathBuf,
    pub git_dir: PathBuf,
    pub common_git_dir: PathBuf,
    pub branch: Option<String>,
    pub remote: CanonicalRemoteIdentity,
    pub revision: String,
    pub tree: String,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalRemoteIdentity(String);

impl CanonicalRemoteIdentity {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for CanonicalRemoteIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("CanonicalRemoteIdentity")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrepareIntegration<'a> {
    pub state_file: &'a Path,
    pub checkout: &'a Path,
    pub configured_remote: &'a str,
    pub target_branch: &'a str,
    pub expected_target: &'a str,
    pub candidate_base: &'a str,
    pub candidate: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationPhase {
    Preparing,
    Prepared,
    PushIntent,
    Published,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntegrationIntentSummary {
    pub state_file: PathBuf,
    pub phase: IntegrationPhase,
    pub checkout: PathBuf,
    pub git_dir: PathBuf,
    pub common_git_dir: PathBuf,
    pub checkout_branch: String,
    pub remote: CanonicalRemoteIdentity,
    pub target_branch: String,
    pub expected_target: String,
    pub candidate_base: String,
    pub candidate: String,
    pub candidate_tree: String,
    pub result: Option<String>,
    pub result_tree: Option<String>,
    pub local_result_ref: Option<String>,
    pub last_observed_target: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteTargetObservation {
    pub target_branch: String,
    pub revision: Option<String>,
    /// Present when the observed commit object is available locally. An exact
    /// observed result revision can always use the prepared result tree.
    pub tree: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublicationAuthorizationContext {
    pub target_branch: String,
    pub expected_target: String,
    pub candidate: String,
    pub result: String,
    pub result_tree: String,
}

/// A short-lived local budget derived from a freshly validated service grant.
/// The library checks it immediately before spawning the publication process.
#[derive(Clone, Debug)]
pub struct FreshPublicationAuthority {
    granted_at: Instant,
    valid_for: Duration,
}

impl FreshPublicationAuthority {
    pub fn valid_for(valid_for: Duration) -> Result<Self> {
        ensure!(
            !valid_for.is_zero(),
            "publication authority has already expired"
        );
        Ok(Self {
            granted_at: Instant::now(),
            valid_for,
        })
    }

    fn remaining(&self) -> Duration {
        self.valid_for.saturating_sub(self.granted_at.elapsed())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PublicationOutcome {
    Published {
        revision: String,
    },
    TargetMoved {
        expected: String,
        observed: Option<String>,
    },
    NotPublished {
        observed: Option<String>,
    },
    Uncertain {
        observed: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct IntegrationIntent {
    version: u32,
    revision: u64,
    phase: IntegrationPhase,
    checkout: PathBuf,
    git_dir: PathBuf,
    common_git_dir: PathBuf,
    checkout_branch: String,
    remote: CanonicalRemoteIdentity,
    target_branch: String,
    expected_target: String,
    candidate_base: String,
    candidate: String,
    candidate_tree: String,
    result: Option<String>,
    result_tree: Option<String>,
    local_result_ref: Option<String>,
    last_observed_target: Option<String>,
}

#[derive(Debug, Serialize)]
struct JournalEvent<'a> {
    revision: u64,
    phase: IntegrationPhase,
    event: &'a str,
    unix_millis: u128,
    observed_target: &'a Option<String>,
}

struct IntentPaths {
    state: PathBuf,
    lock: PathBuf,
    journal: PathBuf,
    directory: PathBuf,
}

struct IntentLock(File);

impl Drop for IntentLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Captures the exact committed tree of a clean checkout and validates that its
/// configured repository identity appears among the checkout's Git remotes.
pub fn capture_clean_snapshot(checkout: &Path, configured_remote: &str) -> Result<CleanSnapshot> {
    let checkout = canonical_git_root(checkout)?;
    ensure_clean(&checkout)?;
    let remote = ensure_remote_matches(&checkout, configured_remote)?;
    let git_dir = canonical_git_dir(&checkout)?;
    let common_git_dir = canonical_common_git_dir(&checkout)?;
    let branch = symbolic_branch(&checkout)?;
    let revision = resolve_head(&checkout)?;
    let tree = git_text(&checkout, ["rev-parse", "HEAD^{tree}"])?;
    ensure_clean(&checkout)?;
    ensure!(
        branch == symbolic_branch(&checkout)?
            && revision == resolve_head(&checkout)?
            && tree == git_text(&checkout, ["rev-parse", "HEAD^{tree}"])?
            && git_dir == canonical_git_dir(&checkout)?
            && common_git_dir == canonical_common_git_dir(&checkout)?,
        "checkout changed while its clean snapshot was captured"
    );
    Ok(CleanSnapshot {
        checkout,
        git_dir,
        common_git_dir,
        branch,
        remote,
        revision,
        tree,
    })
}

/// Rechecks checkout identity, configured remote, cleanliness, commit, and tree.
pub fn verify_clean_snapshot(
    checkout: &Path,
    configured_remote: &str,
    expected: &CleanSnapshot,
) -> Result<()> {
    let actual = capture_clean_snapshot(checkout, configured_remote)?;
    ensure!(actual == *expected, "the clean checkout snapshot changed");
    Ok(())
}

/// Compares the trees named by two exact full commit object IDs.
pub fn commits_have_same_tree(checkout: &Path, left: &str, right: &str) -> Result<bool> {
    let checkout = canonical_git_root(checkout)?;
    let left = resolve_exact_commit(&checkout, left)?;
    let right = resolve_exact_commit(&checkout, right)?;
    Ok(
        git_text(&checkout, ["rev-parse", &format!("{left}^{{tree}}")])?
            == git_text(&checkout, ["rev-parse", &format!("{right}^{{tree}}")])?,
    )
}

/// Confirms that a still-clean checkout has the same tree as an exact commit.
pub fn clean_snapshot_matches_commit(
    checkout: &Path,
    configured_remote: &str,
    expected_snapshot: &CleanSnapshot,
    commit: &str,
) -> Result<bool> {
    verify_clean_snapshot(checkout, configured_remote, expected_snapshot)?;
    let commit = resolve_exact_commit(checkout, commit)?;
    Ok(expected_snapshot.tree == git_text(checkout, ["rev-parse", &format!("{commit}^{{tree}}")])?)
}

/// Reads a protected integration intent without contacting the remote.
pub fn load_integration_intent(state_file: &Path) -> Result<IntegrationIntentSummary> {
    let paths = intent_paths(state_file)?;
    let _lock = lock_intent(&paths)?;
    Ok(summary(&paths.state, &load_intent(&paths)?))
}

/// Prepares a deterministic candidate-containing integration result without
/// moving the target branch. The checkout must be a clean linked worktree at the
/// exact expected target, and the remote target must still equal that commit.
pub fn prepare_integration(request: PrepareIntegration<'_>) -> Result<IntegrationIntentSummary> {
    validate_branch(request.checkout, request.target_branch)?;
    let paths = intent_paths(request.state_file)?;
    let _lock = lock_intent(&paths)?;

    if paths.state.exists() {
        let mut intent = load_intent(&paths)?;
        ensure_same_prepare_request(&intent, &request)?;
        finish_preparation(&paths, &mut intent, request.configured_remote)?;
        return Ok(summary(&paths.state, &intent));
    }

    let snapshot = capture_clean_snapshot(request.checkout, request.configured_remote)?;
    ensure!(
        snapshot.git_dir != snapshot.common_git_dir,
        "integration requires an isolated linked Git worktree"
    );
    let checkout_branch = snapshot
        .branch
        .clone()
        .context("integration worktree must have its task branch checked out")?;
    ensure!(
        checkout_branch != request.target_branch,
        "integration worktree task branch must differ from the target branch"
    );
    let expected_target = resolve_exact_commit(&snapshot.checkout, request.expected_target)?;
    ensure!(
        snapshot.revision == expected_target,
        "integration checkout is not at the expected target commit"
    );
    let candidate_base = resolve_exact_commit(&snapshot.checkout, request.candidate_base)?;
    let candidate = resolve_exact_commit(&snapshot.checkout, request.candidate)?;
    ensure_ancestor(
        &snapshot.checkout,
        &candidate_base,
        &candidate,
        "candidate base is not an ancestor of the candidate",
    )?;
    ensure_related(&snapshot.checkout, &expected_target, &candidate)?;
    let observation = observe_remote_target(
        &snapshot.checkout,
        request.configured_remote,
        request.target_branch,
    )?;
    ensure!(
        observation.revision.as_deref() == Some(expected_target.as_str()),
        "configured remote target does not equal the expected target commit"
    );
    let candidate_tree = git_text(
        &snapshot.checkout,
        ["rev-parse", &format!("{candidate}^{{tree}}")],
    )?;
    let mut intent = IntegrationIntent {
        version: INTENT_VERSION,
        revision: 1,
        phase: IntegrationPhase::Preparing,
        checkout: snapshot.checkout,
        git_dir: snapshot.git_dir,
        common_git_dir: snapshot.common_git_dir,
        checkout_branch,
        remote: snapshot.remote,
        target_branch: request.target_branch.to_owned(),
        expected_target,
        candidate_base,
        candidate,
        candidate_tree,
        result: None,
        result_tree: None,
        local_result_ref: None,
        last_observed_target: observation.revision,
    };
    save_intent(&paths, &intent, "preparation_started")?;
    finish_preparation(&paths, &mut intent, request.configured_remote)?;
    Ok(summary(&paths.state, &intent))
}

/// Observes one exact branch on the explicitly configured remote URL. Error
/// details from Git are deliberately not returned because transports may embed
/// credentials in diagnostics.
pub fn observe_remote_target(
    checkout: &Path,
    configured_remote: &str,
    target_branch: &str,
) -> Result<RemoteTargetObservation> {
    let checkout = canonical_git_root(checkout)?;
    validate_remote_argument(configured_remote)?;
    validate_branch(&checkout, target_branch)?;
    ensure_remote_matches(&checkout, configured_remote)?;
    let remote_argument = git_remote_argument(&checkout, configured_remote)?;
    let target_ref = format!("refs/heads/{target_branch}");
    let output = git_raw(
        &checkout,
        [
            OsString::from("ls-remote"),
            OsString::from("--exit-code"),
            OsString::from("--refs"),
            remote_argument,
            OsString::from(&target_ref),
        ],
        None,
        &[],
    )?;
    let revision = match output.status.code() {
        Some(0) => {
            let text = String::from_utf8(output.stdout)
                .context("Git returned non-UTF-8 remote evidence")?;
            let mut lines = text.lines();
            let line = lines.next().context("Git returned empty remote evidence")?;
            ensure!(
                lines.next().is_none(),
                "Git returned ambiguous remote branch evidence"
            );
            let (revision, reference) = line
                .split_once(char::is_whitespace)
                .context("Git returned malformed remote branch evidence")?;
            ensure!(
                reference.trim() == target_ref,
                "Git returned evidence for an unexpected remote ref"
            );
            validate_full_oid(revision)?;
            Some(revision.to_ascii_lowercase())
        }
        Some(2) => None,
        _ => bail!("Git could not observe the configured remote target"),
    };
    let tree = match revision.as_deref() {
        Some(revision) => locally_available_tree(&checkout, revision)?,
        None => None,
    };
    Ok(RemoteTargetObservation {
        target_branch: target_branch.to_owned(),
        revision,
        tree,
    })
}

/// Reconciles an interrupted publication by observation only. This operation
/// never asks for authority and can never invoke `git push`.
pub fn reconcile_publication(
    state_file: &Path,
    configured_remote: &str,
) -> Result<PublicationOutcome> {
    let paths = intent_paths(state_file)?;
    let _lock = lock_intent(&paths)?;
    let mut intent = load_intent(&paths)?;
    ensure!(
        matches!(
            intent.phase,
            IntegrationPhase::Prepared | IntegrationPhase::PushIntent | IntegrationPhase::Published
        ),
        "integration result has not finished preparation"
    );
    validate_intent_checkout(&intent, configured_remote)?;
    validate_result(&intent)?;
    let observation =
        observe_remote_target(&intent.checkout, configured_remote, &intent.target_branch)?;
    record_observation(
        &paths,
        &mut intent,
        observation.revision.clone(),
        "publication_reconciled",
    )?;
    let result = intent
        .result
        .clone()
        .context("prepared integration has no result")?;
    if observation.revision.as_deref() == Some(result.as_str()) {
        mark_published(&paths, &mut intent)?;
        return Ok(PublicationOutcome::Published { revision: result });
    }
    if observation.revision.as_deref() != Some(intent.expected_target.as_str()) {
        return Ok(PublicationOutcome::TargetMoved {
            expected: intent.expected_target,
            observed: observation.revision,
        });
    }
    if intent.phase == IntegrationPhase::Prepared {
        Ok(PublicationOutcome::NotPublished {
            observed: observation.revision,
        })
    } else {
        Ok(PublicationOutcome::Uncertain {
            observed: observation.revision,
        })
    }
}

/// Restores the isolated checkout to the exact prepared result after verifying
/// that it is clean and still at either the expected target or that result.
/// This never changes a branch ref or contacts the service.
pub fn materialize_prepared_result(
    state_file: &Path,
    configured_remote: &str,
) -> Result<IntegrationIntentSummary> {
    let paths = intent_paths(state_file)?;
    let _lock = lock_intent(&paths)?;
    let mut intent = load_intent(&paths)?;
    ensure!(
        matches!(
            intent.phase,
            IntegrationPhase::Prepared | IntegrationPhase::PushIntent | IntegrationPhase::Published
        ),
        "integration result has not finished preparation"
    );
    materialize_result_checkout(&intent, configured_remote)?;
    intent.revision += 1;
    save_intent(&paths, &intent, "result_materialized")?;
    Ok(summary(&paths.state, &intent))
}

/// Reconciles or publishes a prepared result. The callback is invoked only after
/// observing the expected remote target and must obtain fresh service authority.
/// A second observation follows the callback before a durable push intent and an
/// exact `--force-with-lease` compare-and-swap push.
pub async fn publish_prepared<F, Fut>(
    state_file: &Path,
    configured_remote: &str,
    authorize: F,
) -> Result<PublicationOutcome>
where
    F: FnOnce(PublicationAuthorizationContext) -> Fut,
    Fut: Future<Output = Result<FreshPublicationAuthority>>,
{
    let paths = intent_paths(state_file)?;
    let _lock = lock_intent(&paths)?;
    let mut intent = load_intent(&paths)?;
    ensure!(
        matches!(
            intent.phase,
            IntegrationPhase::Prepared | IntegrationPhase::PushIntent | IntegrationPhase::Published
        ),
        "integration result has not finished preparation"
    );
    validate_intent_checkout(&intent, configured_remote)?;
    validate_result(&intent)?;

    let first = observe_remote_target(&intent.checkout, configured_remote, &intent.target_branch)?;
    record_observation(
        &paths,
        &mut intent,
        first.revision.clone(),
        "remote_observed",
    )?;
    let result = intent
        .result
        .clone()
        .context("prepared integration has no result")?;
    if first.revision.as_deref() == Some(result.as_str()) {
        mark_published(&paths, &mut intent)?;
        return Ok(PublicationOutcome::Published { revision: result });
    }
    if first.revision.as_deref() != Some(intent.expected_target.as_str()) {
        return Ok(PublicationOutcome::TargetMoved {
            expected: intent.expected_target,
            observed: first.revision,
        });
    }
    if intent.phase == IntegrationPhase::PushIntent {
        return Ok(PublicationOutcome::Uncertain {
            observed: first.revision,
        });
    }
    if intent.phase == IntegrationPhase::Published {
        return Ok(PublicationOutcome::TargetMoved {
            expected: result,
            observed: first.revision,
        });
    }

    let context = PublicationAuthorizationContext {
        target_branch: intent.target_branch.clone(),
        expected_target: intent.expected_target.clone(),
        candidate: intent.candidate.clone(),
        result: result.clone(),
        result_tree: intent
            .result_tree
            .clone()
            .context("prepared integration has no result tree")?,
    };
    let authority = authorize(context)
        .await
        .context("fresh publication authority was not granted")?;

    validate_intent_checkout(&intent, configured_remote)?;
    validate_result(&intent)?;
    let second = observe_remote_target(&intent.checkout, configured_remote, &intent.target_branch)?;
    record_observation(
        &paths,
        &mut intent,
        second.revision.clone(),
        "remote_reobserved",
    )?;
    if second.revision.as_deref() == Some(result.as_str()) {
        mark_published(&paths, &mut intent)?;
        return Ok(PublicationOutcome::Published { revision: result });
    }
    if second.revision.as_deref() != Some(intent.expected_target.as_str()) {
        return Ok(PublicationOutcome::TargetMoved {
            expected: intent.expected_target,
            observed: second.revision,
        });
    }
    ensure!(
        authority.remaining() > Duration::from_secs(2),
        "fresh publication authority is too close to expiry"
    );

    intent.phase = IntegrationPhase::PushIntent;
    intent.revision += 1;
    save_intent(&paths, &intent, "push_intent")?;
    let target_ref = format!("refs/heads/{}", intent.target_branch);
    let lease = format!("--force-with-lease={target_ref}:{}", intent.expected_target);
    let refspec = format!("{}:{target_ref}", result);
    let remote_argument = git_remote_argument(&intent.checkout, configured_remote)?;
    if authority.remaining() <= Duration::from_secs(2) {
        intent.phase = IntegrationPhase::Prepared;
        intent.revision += 1;
        save_intent(&paths, &intent, "push_not_started_authority_expired")?;
        bail!("fresh publication authority expired before Git could start");
    }
    let push = git_raw(
        &intent.checkout,
        [
            OsString::from("push"),
            OsString::from("--porcelain"),
            OsString::from(lease),
            remote_argument,
            OsString::from(refspec),
        ],
        None,
        &[],
    )?;

    let after = observe_remote_target(&intent.checkout, configured_remote, &intent.target_branch)?;
    record_observation(
        &paths,
        &mut intent,
        after.revision.clone(),
        "post_push_observed",
    )?;
    if after.revision.as_deref() == Some(result.as_str()) {
        mark_published(&paths, &mut intent)?;
        return Ok(PublicationOutcome::Published { revision: result });
    }
    if after.revision.as_deref() != Some(intent.expected_target.as_str()) {
        return Ok(PublicationOutcome::TargetMoved {
            expected: intent.expected_target,
            observed: after.revision,
        });
    }
    if push.status.success() {
        Ok(PublicationOutcome::Uncertain {
            observed: after.revision,
        })
    } else {
        Ok(PublicationOutcome::NotPublished {
            observed: after.revision,
        })
    }
}

fn finish_preparation(
    paths: &IntentPaths,
    intent: &mut IntegrationIntent,
    remote: &str,
) -> Result<()> {
    validate_intent_checkout(intent, remote)?;
    if intent.phase != IntegrationPhase::Preparing {
        validate_result(intent)?;
        return Ok(());
    }
    if intent.result.is_none() {
        let (result, result_tree) = create_integration_result(intent)?;
        let local_ref = format!("refs/agent-coordinator/integrations/{result}");
        update_isolated_ref(&intent.checkout, &local_ref, &result)?;
        intent.result = Some(result);
        intent.result_tree = Some(result_tree);
        intent.local_result_ref = Some(local_ref);
        intent.revision += 1;
        validate_result(intent)?;
        save_intent(paths, intent, "result_created")?;
    }
    materialize_result_checkout(intent, remote)?;
    intent.phase = IntegrationPhase::Prepared;
    intent.revision += 1;
    validate_result(intent)?;
    save_intent(paths, intent, "prepared")
}

fn create_integration_result(intent: &IntegrationIntent) -> Result<(String, String)> {
    if is_ancestor(&intent.checkout, &intent.candidate, &intent.expected_target)? {
        let tree = git_text(
            &intent.checkout,
            ["rev-parse", &format!("{}^{{tree}}", intent.expected_target)],
        )?;
        return Ok((intent.expected_target.clone(), tree));
    }
    if is_ancestor(&intent.checkout, &intent.expected_target, &intent.candidate)? {
        return Ok((intent.candidate.clone(), intent.candidate_tree.clone()));
    }

    let merge = git_raw(
        &intent.checkout,
        [
            OsString::from("merge-tree"),
            OsString::from("--write-tree"),
            OsString::from(&intent.expected_target),
            OsString::from(&intent.candidate),
        ],
        None,
        &[],
    )?;
    if !merge.status.success() {
        bail!("candidate conflicts with the expected target; no target ref was changed");
    }
    let output =
        String::from_utf8(merge.stdout).context("Git returned a non-UTF-8 merge result")?;
    let tree = output
        .lines()
        .next()
        .map(str::trim)
        .context("Git returned no merge tree")?;
    validate_full_oid(tree)?;
    git_ok(
        &intent.checkout,
        ["cat-file", "-e", &format!("{tree}^{{tree}}")],
    )?;

    let message = format!(
        "Agent Coordinator integration\n\ntarget {}\ncandidate {}\n",
        intent.expected_target, intent.candidate
    );
    let parents = [
        OsString::from("commit-tree"),
        OsString::from(tree),
        OsString::from("-p"),
        OsString::from(&intent.expected_target),
        OsString::from("-p"),
        OsString::from(&intent.candidate),
    ];
    let environment = [
        ("GIT_AUTHOR_NAME", "Agent Coordinator"),
        ("GIT_AUTHOR_EMAIL", "agent-coordinator@example.invalid"),
        ("GIT_AUTHOR_DATE", "2000-01-01T00:00:00 +0000"),
        ("GIT_COMMITTER_NAME", "Agent Coordinator"),
        ("GIT_COMMITTER_EMAIL", "agent-coordinator@example.invalid"),
        ("GIT_COMMITTER_DATE", "2000-01-01T00:00:00 +0000"),
    ];
    let commit = git_raw(
        &intent.checkout,
        parents,
        Some(message.as_bytes()),
        &environment,
    )?;
    ensure!(
        commit.status.success(),
        "Git could not create the deterministic integration commit"
    );
    let commit = String::from_utf8(commit.stdout)
        .context("Git returned a non-UTF-8 integration commit")?
        .trim()
        .to_owned();
    validate_full_oid(&commit)?;
    Ok((commit, tree.to_ascii_lowercase()))
}

fn validate_result(intent: &IntegrationIntent) -> Result<()> {
    let result = intent
        .result
        .as_deref()
        .context("integration result is missing")?;
    let result = resolve_exact_commit(&intent.checkout, result)?;
    ensure_ancestor(
        &intent.checkout,
        &intent.expected_target,
        &result,
        "integration result no longer contains the expected target",
    )?;
    ensure_ancestor(
        &intent.checkout,
        &intent.candidate,
        &result,
        "integration result no longer contains the candidate",
    )?;
    let tree = git_text(
        &intent.checkout,
        ["rev-parse", &format!("{result}^{{tree}}")],
    )?;
    ensure!(
        intent.result_tree.as_deref() == Some(tree.as_str()),
        "integration result tree changed"
    );
    Ok(())
}

fn validate_intent_checkout(intent: &IntegrationIntent, configured_remote: &str) -> Result<()> {
    let snapshot = capture_clean_snapshot(&intent.checkout, configured_remote)?;
    ensure!(
        snapshot.git_dir == intent.git_dir,
        "integration worktree Git identity changed"
    );
    ensure!(
        snapshot.common_git_dir == intent.common_git_dir,
        "integration repository identity changed"
    );
    ensure!(
        snapshot.remote == intent.remote,
        "configured repository identity changed"
    );
    ensure!(
        snapshot.branch.as_deref() == Some(intent.checkout_branch.as_str()),
        "integration task branch changed"
    );
    let at_expected = snapshot.revision == intent.expected_target;
    let at_result = intent.result.as_deref() == Some(snapshot.revision.as_str());
    let valid_revision = match intent.phase {
        IntegrationPhase::Preparing => at_expected || at_result,
        IntegrationPhase::Prepared | IntegrationPhase::PushIntent | IntegrationPhase::Published => {
            at_result
        }
    };
    ensure!(
        valid_revision,
        "integration checkout moved from its recorded commit"
    );
    Ok(())
}

fn materialize_result_checkout(intent: &IntegrationIntent, configured_remote: &str) -> Result<()> {
    let snapshot = capture_clean_snapshot(&intent.checkout, configured_remote)?;
    ensure!(
        snapshot.git_dir == intent.git_dir && snapshot.common_git_dir == intent.common_git_dir,
        "integration checkout identity changed"
    );
    ensure!(
        snapshot.remote == intent.remote,
        "configured repository identity changed"
    );
    ensure!(
        snapshot.branch.as_deref() == Some(intent.checkout_branch.as_str()),
        "integration task branch changed"
    );
    let result = intent
        .result
        .as_deref()
        .context("integration result is missing")?;
    if snapshot.revision == result {
        return Ok(());
    }
    ensure!(
        snapshot.revision == intent.expected_target,
        "integration checkout moved from its recorded commit"
    );
    git_ok(
        &intent.checkout,
        ["merge", "--ff-only", "--no-edit", result],
    )
    .context("materialize the prepared integration result")?;
    let materialized = capture_clean_snapshot(&intent.checkout, configured_remote)?;
    ensure!(
        materialized.revision == result
            && materialized.git_dir == intent.git_dir
            && materialized.common_git_dir == intent.common_git_dir
            && materialized.remote == intent.remote
            && materialized.branch.as_deref() == Some(intent.checkout_branch.as_str()),
        "prepared integration result did not materialize exactly"
    );
    Ok(())
}

fn ensure_same_prepare_request(
    intent: &IntegrationIntent,
    request: &PrepareIntegration<'_>,
) -> Result<()> {
    let checkout = canonical_git_root(request.checkout)?;
    let remote = canonical_remote_identity(&checkout, request.configured_remote)?;
    ensure!(
        intent.checkout == checkout
            && intent.remote == remote
            && intent.target_branch == request.target_branch
            && intent.expected_target == request.expected_target.to_ascii_lowercase()
            && intent.candidate_base == request.candidate_base.to_ascii_lowercase()
            && intent.candidate == request.candidate.to_ascii_lowercase(),
        "this integration intent was prepared with different immutable inputs"
    );
    Ok(())
}

fn update_isolated_ref(checkout: &Path, reference: &str, result: &str) -> Result<()> {
    let existing = git(
        checkout,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    match existing.status.code() {
        Some(0) => {
            let value = String::from_utf8(existing.stdout)
                .context("Git returned non-UTF-8 ref evidence")?;
            ensure!(
                value.trim() == result,
                "isolated integration result ref already names another commit"
            );
        }
        Some(1) => git_ok(checkout, ["update-ref", reference, result, ""])?,
        _ => bail!("Git could not inspect the isolated integration result ref"),
    }
    Ok(())
}

fn mark_published(paths: &IntentPaths, intent: &mut IntegrationIntent) -> Result<()> {
    if intent.phase != IntegrationPhase::Published {
        intent.phase = IntegrationPhase::Published;
        intent.revision += 1;
        save_intent(paths, intent, "published")?;
    }
    Ok(())
}

fn record_observation(
    paths: &IntentPaths,
    intent: &mut IntegrationIntent,
    observed: Option<String>,
    event: &str,
) -> Result<()> {
    intent.last_observed_target = observed;
    intent.revision += 1;
    save_intent(paths, intent, event)
}

fn summary(state_file: &Path, intent: &IntegrationIntent) -> IntegrationIntentSummary {
    IntegrationIntentSummary {
        state_file: state_file.to_path_buf(),
        phase: intent.phase,
        checkout: intent.checkout.clone(),
        git_dir: intent.git_dir.clone(),
        common_git_dir: intent.common_git_dir.clone(),
        checkout_branch: intent.checkout_branch.clone(),
        remote: intent.remote.clone(),
        target_branch: intent.target_branch.clone(),
        expected_target: intent.expected_target.clone(),
        candidate_base: intent.candidate_base.clone(),
        candidate: intent.candidate.clone(),
        candidate_tree: intent.candidate_tree.clone(),
        result: intent.result.clone(),
        result_tree: intent.result_tree.clone(),
        local_result_ref: intent.local_result_ref.clone(),
        last_observed_target: intent.last_observed_target.clone(),
    }
}

fn ensure_related(checkout: &Path, left: &str, right: &str) -> Result<()> {
    let output = git(checkout, ["merge-base", left, right])?;
    ensure!(
        output.status.success(),
        "candidate and target have unrelated histories"
    );
    Ok(())
}

fn ensure_ancestor(checkout: &Path, ancestor: &str, descendant: &str, message: &str) -> Result<()> {
    ensure!(is_ancestor(checkout, ancestor, descendant)?, "{message}");
    Ok(())
}

fn is_ancestor(checkout: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = git(
        checkout,
        ["merge-base", "--is-ancestor", ancestor, descendant],
    )?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("Git could not verify commit ancestry"),
    }
}

fn resolve_head(checkout: &Path) -> Result<String> {
    git_text(checkout, ["rev-parse", "HEAD^{commit}"])
}

fn symbolic_branch(checkout: &Path) -> Result<Option<String>> {
    let output = git(checkout, ["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    match output.status.code() {
        Some(0) => String::from_utf8(output.stdout)
            .context("Git returned a non-UTF-8 branch name")
            .map(|value| Some(value.trim().to_owned())),
        Some(1) => Ok(None),
        _ => bail!("Git could not inspect the checkout branch"),
    }
}

fn resolve_exact_commit(checkout: &Path, revision: &str) -> Result<String> {
    validate_full_oid(revision)?;
    let revision = revision.to_ascii_lowercase();
    git_ok(
        checkout,
        ["cat-file", "-e", &format!("{revision}^{{commit}}")],
    )?;
    let resolved = git_text(checkout, ["rev-parse", &format!("{revision}^{{commit}}")])?;
    ensure!(
        resolved == revision,
        "commit object ID did not resolve exactly"
    );
    Ok(revision)
}

fn locally_available_tree(checkout: &Path, revision: &str) -> Result<Option<String>> {
    let object = format!("{revision}^{{commit}}");
    let available = git(checkout, ["cat-file", "-e", &object])?;
    if !available.status.success() {
        return Ok(None);
    }
    Ok(Some(git_text(
        checkout,
        ["rev-parse", &format!("{revision}^{{tree}}")],
    )?))
}

fn validate_full_oid(value: &str) -> Result<()> {
    ensure!(
        matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "revision must be a full hexadecimal Git object ID"
    );
    Ok(())
}

fn ensure_clean(checkout: &Path) -> Result<()> {
    let output = git(
        checkout,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    ensure!(
        output.status.success(),
        "Git could not inspect checkout cleanliness"
    );
    ensure!(
        output.stdout.is_empty(),
        "checkout is dirty; existing changes were preserved"
    );
    Ok(())
}

fn ensure_remote_matches(
    checkout: &Path,
    configured_remote: &str,
) -> Result<CanonicalRemoteIdentity> {
    let expected = canonical_remote_identity(checkout, configured_remote)?;
    let names = git_text(checkout, ["remote"])?;
    for name in names.lines().filter(|name| !name.is_empty()) {
        let output = git(checkout, ["remote", "get-url", "--all", name])?;
        if !output.status.success() {
            continue;
        }
        let urls =
            String::from_utf8(output.stdout).context("Git returned a non-UTF-8 remote URL")?;
        if urls
            .lines()
            .any(|url| canonical_remote_identity(checkout, url).ok().as_ref() == Some(&expected))
        {
            return Ok(expected);
        }
    }
    bail!("checkout has no remote matching the configured repository identity")
}

fn canonical_remote_identity(checkout: &Path, value: &str) -> Result<CanonicalRemoteIdentity> {
    validate_remote_argument(value)?;
    let value = value.trim();
    if let Some(path) = value.strip_prefix("file://") {
        return canonical_local_remote(Path::new(path));
    }
    if value.contains("://") {
        let (scheme, rest) = value.split_once("://").expect("checked above");
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let path = normalize_remote_path(path);
        return Ok(CanonicalRemoteIdentity(format!(
            "{}://{}/{}",
            scheme.to_ascii_lowercase(),
            authority.to_ascii_lowercase(),
            path
        )));
    }
    if !looks_like_windows_drive(value)
        && let Some((host, path)) = value.split_once(':')
        && !host.contains('/')
        && !host.contains('\\')
    {
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        return Ok(CanonicalRemoteIdentity(format!(
            "{}:{}",
            host.to_ascii_lowercase(),
            normalize_remote_path(path)
        )));
    }
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        checkout.join(path)
    };
    canonical_local_remote(&path)
}

fn canonical_local_remote(path: &Path) -> Result<CanonicalRemoteIdentity> {
    let path =
        fs::canonicalize(path).with_context(|| "resolve configured local repository identity")?;
    Ok(CanonicalRemoteIdentity(format!("file:{}", path.display())))
}

fn normalize_remote_path(value: &str) -> String {
    value
        .split(['?', '#'])
        .next()
        .unwrap_or(value)
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_owned()
}

fn looks_like_windows_drive(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn git_remote_argument(checkout: &Path, value: &str) -> Result<OsString> {
    if value.starts_with("file://") || value.contains("://") {
        return Ok(OsString::from(value));
    }
    if !looks_like_windows_drive(value)
        && let Some((host, _)) = value.split_once(':')
        && !host.contains('/')
        && !host.contains('\\')
    {
        return Ok(OsString::from(value));
    }
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        checkout.join(path)
    };
    Ok(git_compatible_path(
        &fs::canonicalize(path).context("resolve configured local repository")?,
    )?
    .into_os_string())
}

fn validate_remote_argument(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty(),
        "configured repository URL is empty"
    );
    ensure!(
        !value.starts_with('-'),
        "configured repository URL cannot begin with a dash"
    );
    Ok(())
}

fn validate_branch(checkout: &Path, branch: &str) -> Result<()> {
    ensure!(
        !branch.starts_with('-'),
        "target branch cannot begin with a dash"
    );
    git_ok(checkout, ["check-ref-format", "--branch", branch])
        .context("target branch is not a valid Git branch name")
}

fn canonical_git_root(path: &Path) -> Result<PathBuf> {
    let root = git_text(path, ["rev-parse", "--show-toplevel"])?;
    fs::canonicalize(&root).with_context(|| format!("resolve Git checkout {}", path.display()))
}

fn canonical_git_dir(path: &Path) -> Result<PathBuf> {
    let directory = git_text(path, ["rev-parse", "--path-format=absolute", "--git-dir"])?;
    fs::canonicalize(&directory).context("resolve Git worktree identity")
}

fn canonical_common_git_dir(path: &Path) -> Result<PathBuf> {
    let directory = git_text(
        path,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    fs::canonicalize(&directory).context("resolve Git repository identity")
}

fn git_text<I, S>(checkout: &Path, arguments: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(checkout, arguments)?;
    ensure!(output.status.success(), "Git command failed");
    String::from_utf8(output.stdout)
        .context("Git returned non-UTF-8 output")
        .map(|value| value.trim().to_owned())
}

fn git_ok<I, S>(checkout: &Path, arguments: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(checkout, arguments)?;
    ensure!(output.status.success(), "Git command failed");
    Ok(())
}

fn git<I, S>(checkout: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_raw(
        checkout,
        arguments
            .into_iter()
            .map(|value| value.as_ref().to_os_string()),
        None,
        &[],
    )
}

fn git_raw<I>(
    checkout: &Path,
    arguments: I,
    stdin: Option<&[u8]>,
    environment: &[(&str, &str)],
) -> Result<Output>
where
    I: IntoIterator<Item = OsString>,
{
    let checkout = git_compatible_path(checkout)?;
    let mut command = Command::new("git");
    command
        .args(arguments)
        .current_dir(checkout)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (name, _) in std::env::vars_os() {
        let normalized = name.to_string_lossy().to_ascii_uppercase();
        if normalized.starts_with("AGENT_COORDINATOR_")
            || normalized.starts_with("COORDINATOR_")
            || matches!(
                normalized.as_str(),
                "GIT_DIR"
                    | "GIT_WORK_TREE"
                    | "GIT_COMMON_DIR"
                    | "GIT_INDEX_FILE"
                    | "GIT_OBJECT_DIRECTORY"
                    | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                    | "GIT_NAMESPACE"
            )
        {
            command.env_remove(name);
        }
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().context("run Git")?;
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .context("open Git standard input")?
            .write_all(input)
            .context("write Git standard input")?;
    }
    child.wait_with_output().context("wait for Git")
}

#[cfg(not(windows))]
fn git_compatible_path(path: &Path) -> Result<PathBuf> {
    Ok(path.to_path_buf())
}

#[cfg(windows)]
fn git_compatible_path(path: &Path) -> Result<PathBuf> {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Ok(path.to_path_buf());
    };
    let mut compatible = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => {
            let mut value = OsString::from(r"\\");
            value.push(server);
            value.push(r"\");
            value.push(share);
            PathBuf::from(value)
        }
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => {
            bail!("Git cannot use this Windows device path")
        }
        Prefix::Disk(_) | Prefix::UNC(_, _) => return Ok(path.to_path_buf()),
    };
    for component in components {
        match component {
            Component::Prefix(_) => bail!("invalid Windows path"),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => compatible.push(".."),
            Component::Normal(value) => compatible.push(value),
        }
    }
    Ok(compatible)
}

fn intent_paths(state_file: &Path) -> Result<IntentPaths> {
    let directory = state_file
        .parent()
        .context("integration state file must have a parent")?
        .to_path_buf();
    let stem = state_file
        .file_stem()
        .and_then(OsStr::to_str)
        .context("integration state file must have a Unicode stem")?;
    Ok(IntentPaths {
        state: state_file.to_path_buf(),
        lock: directory.join(format!("{stem}.lock")),
        journal: directory.join(format!("{stem}.journal.jsonl")),
        directory,
    })
}

fn lock_intent(paths: &IntentPaths) -> Result<IntentLock> {
    fs::create_dir_all(&paths.directory).context("create integration state directory")?;
    protect_directory(&paths.directory)?;
    let file = protected_open(&paths.lock, false)?;
    file.lock().context("lock integration state")?;
    Ok(IntentLock(file))
}

fn load_intent(paths: &IntentPaths) -> Result<IntegrationIntent> {
    let bytes = fs::read(&paths.state).context("read integration intent")?;
    let intent: IntegrationIntent =
        serde_json::from_slice(&bytes).context("decode integration intent")?;
    ensure!(
        intent.version == INTENT_VERSION,
        "unsupported integration intent version"
    );
    Ok(intent)
}

fn save_intent(paths: &IntentPaths, intent: &IntegrationIntent, event: &str) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(intent).context("encode integration intent")?;
    let temporary = paths.directory.join(format!(
        ".integration-{}-{}.tmp",
        std::process::id(),
        Uuid::new_v4()
    ));
    let mut file = protected_create_new(&temporary)?;
    let save = (|| -> io::Result<()> {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, &paths.state)?;
        sync_directory(&paths.directory)?;
        Ok(())
    })();
    if let Err(error) = save {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("durably save integration intent");
    }
    protect_file(&paths.state)?;
    append_journal(paths, intent, event)
}

fn append_journal(paths: &IntentPaths, intent: &IntegrationIntent, event: &str) -> Result<()> {
    let mut file = protected_open(&paths.journal, false)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .context("inspect integration journal")?;
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        let valid = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        file.set_len(valid as u64)
            .context("repair integration journal tail")?;
    }
    file.seek(SeekFrom::End(0))
        .context("seek integration journal")?;
    let unix_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    serde_json::to_writer(
        &mut file,
        &JournalEvent {
            revision: intent.revision,
            phase: intent.phase,
            event,
            unix_millis,
            observed_target: &intent.last_observed_target,
        },
    )
    .context("encode integration journal event")?;
    file.write_all(b"\n")
        .context("append integration journal event")?;
    file.sync_all().context("sync integration journal")
}

fn protected_open(path: &Path, append: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .context("open protected integration state")?;
    protect_file(path)?;
    Ok(file)
}

fn protected_create_new(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .context("create protected integration state")
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("protect integration state directory")
}

#[cfg(unix)]
fn protect_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("protect integration state file")
}

#[cfg(windows)]
fn protect_directory(_path: &Path) -> Result<()> {
    Ok(())
}
#[cfg(windows)]
fn protect_file(_path: &Path) -> Result<()> {
    Ok(())
}
#[cfg(not(any(unix, windows)))]
fn protect_directory(_path: &Path) -> Result<()> {
    bail!("protected local state is unsupported on this platform")
}
#[cfg(not(any(unix, windows)))]
fn protect_file(_path: &Path) -> Result<()> {
    bail!("protected local state is unsupported on this platform")
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tempfile::TempDir;

    struct Repository {
        _directory: TempDir,
        remote: PathBuf,
        source: PathBuf,
        integration: PathBuf,
        base: String,
    }

    fn repository() -> Repository {
        let directory = tempfile::Builder::new()
            .prefix("git workflow paths with spaces ")
            .tempdir()
            .unwrap();
        let remote = directory.path().join("trusted remote with spaces.git");
        git_ok(
            directory.path(),
            ["init", "--bare", remote.to_str().unwrap()],
        )
        .unwrap();
        let source = directory.path().join("source checkout with spaces");
        git_ok(
            directory.path(),
            ["clone", remote.to_str().unwrap(), source.to_str().unwrap()],
        )
        .unwrap();
        configure_identity(&source);
        fs::write(source.join("base.txt"), "base\n").unwrap();
        git_ok(&source, ["add", "base.txt"]).unwrap();
        git_ok(&source, ["commit", "-m", "base"]).unwrap();
        git_ok(&source, ["branch", "-M", "main"]).unwrap();
        git_ok(&source, ["push", "origin", "main"]).unwrap();
        let base = resolve_head(&source).unwrap();
        let integration = directory.path().join("integration worktree with spaces");
        git_ok(
            &source,
            [
                "worktree",
                "add",
                "-b",
                "integration/test-task",
                integration.to_str().unwrap(),
                &base,
            ],
        )
        .unwrap();
        Repository {
            _directory: directory,
            remote,
            source,
            integration,
            base,
        }
    }

    fn configure_identity(repository: &Path) {
        git_ok(repository, ["config", "user.name", "Coordinator Test"]).unwrap();
        git_ok(repository, ["config", "user.email", "test@example.invalid"]).unwrap();
    }

    fn candidate(repository: &Repository, name: &str, content: &str) -> String {
        git_ok(
            &repository.source,
            ["checkout", "-B", name, &repository.base],
        )
        .unwrap();
        fs::write(repository.source.join(format!("{name}.txt")), content).unwrap();
        git_ok(&repository.source, ["add", "."]).unwrap();
        git_ok(&repository.source, ["commit", "-m", name]).unwrap();
        resolve_head(&repository.source).unwrap()
    }

    fn request<'a>(
        repository: &'a Repository,
        state: &'a Path,
        candidate: &'a str,
    ) -> PrepareIntegration<'a> {
        PrepareIntegration {
            state_file: state,
            checkout: &repository.integration,
            configured_remote: repository.remote.to_str().unwrap(),
            target_branch: "main",
            expected_target: &repository.base,
            candidate_base: &repository.base,
            candidate,
        }
    }

    #[test]
    fn clean_snapshot_and_tree_equivalence_reject_dirty_inputs() {
        let repository = repository();
        let remote = repository.remote.to_str().unwrap();
        let snapshot = capture_clean_snapshot(&repository.source, remote).unwrap();
        assert!(
            clean_snapshot_matches_commit(&repository.source, remote, &snapshot, &repository.base)
                .unwrap()
        );
        fs::write(repository.source.join("dirty.txt"), "dirty\n").unwrap();
        assert!(verify_clean_snapshot(&repository.source, remote, &snapshot).is_err());
        assert!(
            clean_snapshot_matches_commit(&repository.source, remote, &snapshot, &repository.base)
                .is_err()
        );
    }

    #[test]
    fn prepares_deterministic_merge_without_moving_target() {
        let repository = repository();
        let candidate = candidate(&repository, "candidate", "candidate\n");
        git_ok(&repository.source, ["checkout", "main"]).unwrap();
        fs::write(repository.source.join("target.txt"), "target\n").unwrap();
        git_ok(&repository.source, ["add", "target.txt"]).unwrap();
        git_ok(&repository.source, ["commit", "-m", "target"]).unwrap();
        git_ok(&repository.source, ["push", "origin", "main"]).unwrap();
        let target = resolve_head(&repository.source).unwrap();
        git_ok(
            &repository.integration,
            ["merge", "--ff-only", "--no-edit", &target],
        )
        .unwrap();

        let state = repository
            ._directory
            .path()
            .join("state dir")
            .join("intent.json");
        let request = PrepareIntegration {
            expected_target: &target,
            ..request(&repository, &state, &candidate)
        };
        let first = prepare_integration(request.clone()).unwrap();
        let second = prepare_integration(request).unwrap();
        assert_eq!(first.result, second.result);
        let result = first.result.unwrap();
        assert_eq!(resolve_head(&repository.integration).unwrap(), result);
        assert!(is_ancestor(&repository.integration, &target, &result).unwrap());
        assert!(is_ancestor(&repository.integration, &candidate, &result).unwrap());
        assert_eq!(
            observe_remote_target(
                &repository.integration,
                repository.remote.to_str().unwrap(),
                "main"
            )
            .unwrap()
            .revision,
            Some(target)
        );
    }

    #[tokio::test]
    async fn successful_publish_uses_cas_and_retry_only_observes() {
        let repository = repository();
        let candidate = candidate(&repository, "candidate", "candidate\n");
        let state = repository
            ._directory
            .path()
            .join("publish state")
            .join("intent.json");
        let prepared = prepare_integration(request(&repository, &state, &candidate)).unwrap();
        let result = prepared.result.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let first_calls = calls.clone();
        let outcome = publish_prepared(&state, repository.remote.to_str().unwrap(), move |_| {
            first_calls.fetch_add(1, Ordering::SeqCst);
            async { FreshPublicationAuthority::valid_for(Duration::from_secs(30)) }
        })
        .await
        .unwrap();
        assert_eq!(
            outcome,
            PublicationOutcome::Published {
                revision: result.clone()
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let retry_calls = calls.clone();
        let retried = publish_prepared(&state, repository.remote.to_str().unwrap(), move |_| {
            retry_calls.fetch_add(1, Ordering::SeqCst);
            async { bail!("must not request authority after observing the result") }
        })
        .await
        .unwrap();
        assert_eq!(retried, PublicationOutcome::Published { revision: result });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn moved_target_blocks_publication_before_authority() {
        let repository = repository();
        let candidate = candidate(&repository, "candidate", "candidate\n");
        let state = repository
            ._directory
            .path()
            .join("moved target")
            .join("intent.json");
        prepare_integration(request(&repository, &state, &candidate)).unwrap();
        git_ok(&repository.source, ["checkout", "main"]).unwrap();
        fs::write(repository.source.join("moved.txt"), "moved\n").unwrap();
        git_ok(&repository.source, ["add", "moved.txt"]).unwrap();
        git_ok(&repository.source, ["commit", "-m", "moved"]).unwrap();
        git_ok(&repository.source, ["push", "origin", "main"]).unwrap();
        let moved = resolve_head(&repository.source).unwrap();
        let outcome = publish_prepared(&state, repository.remote.to_str().unwrap(), |_| async {
            bail!("authority callback must not run for a moved target")
        })
        .await
        .unwrap();
        assert_eq!(
            outcome,
            PublicationOutcome::TargetMoved {
                expected: repository.base,
                observed: Some(moved)
            }
        );
    }

    #[tokio::test]
    async fn moved_target_after_authority_blocks_before_push() {
        let repository = repository();
        let candidate = candidate(&repository, "candidate", "candidate\n");
        let state = repository
            ._directory
            .path()
            .join("move during authority")
            .join("intent.json");
        prepare_integration(request(&repository, &state, &candidate)).unwrap();
        let source = repository.source.clone();
        let remote = repository.remote.clone();
        let outcome = publish_prepared(
            &state,
            repository.remote.to_str().unwrap(),
            move |_| async move {
                git_ok(&source, ["checkout", "main"])?;
                fs::write(source.join("concurrent.txt"), "concurrent\n")?;
                git_ok(&source, ["add", "concurrent.txt"])?;
                git_ok(&source, ["commit", "-m", "concurrent target"])?;
                git_ok(&source, ["push", remote.to_str().unwrap(), "main"])?;
                FreshPublicationAuthority::valid_for(Duration::from_secs(30))
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, PublicationOutcome::TargetMoved { .. }));
    }

    #[tokio::test]
    async fn near_expiry_authority_never_creates_push_intent() {
        let repository = repository();
        let candidate = candidate(&repository, "candidate", "candidate\n");
        let state = repository
            ._directory
            .path()
            .join("expired authority")
            .join("intent.json");
        prepare_integration(request(&repository, &state, &candidate)).unwrap();
        let result = publish_prepared(&state, repository.remote.to_str().unwrap(), |_| async {
            FreshPublicationAuthority::valid_for(Duration::from_secs(1))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            load_integration_intent(&state).unwrap().phase,
            IntegrationPhase::Prepared
        );
        assert_eq!(
            observe_remote_target(
                &repository.integration,
                repository.remote.to_str().unwrap(),
                "main"
            )
            .unwrap()
            .revision,
            Some(repository.base)
        );
    }

    #[tokio::test]
    async fn interrupted_push_intent_reconciles_remote_before_republish() {
        let repository = repository();
        let candidate = candidate(&repository, "candidate", "candidate\n");
        let state = repository
            ._directory
            .path()
            .join("retry inspection")
            .join("intent.json");
        let prepared = prepare_integration(request(&repository, &state, &candidate)).unwrap();
        let result = prepared.result.clone().unwrap();
        let paths = intent_paths(&state).unwrap();
        {
            let _lock = lock_intent(&paths).unwrap();
            let mut intent = load_intent(&paths).unwrap();
            intent.phase = IntegrationPhase::PushIntent;
            intent.revision += 1;
            save_intent(&paths, &intent, "simulated_interruption").unwrap();
        }
        git_ok(
            &repository.integration,
            [
                "push",
                repository.remote.to_str().unwrap(),
                &format!("{result}:refs/heads/main"),
            ],
        )
        .unwrap();
        let outcome = reconcile_publication(&state, repository.remote.to_str().unwrap()).unwrap();
        assert_eq!(outcome, PublicationOutcome::Published { revision: result });
    }

    #[tokio::test]
    async fn unresolved_push_intent_never_republishes() {
        let repository = repository();
        let candidate = candidate(&repository, "candidate", "candidate\n");
        let state = repository
            ._directory
            .path()
            .join("unresolved retry")
            .join("intent.json");
        prepare_integration(request(&repository, &state, &candidate)).unwrap();
        let paths = intent_paths(&state).unwrap();
        {
            let _lock = lock_intent(&paths).unwrap();
            let mut intent = load_intent(&paths).unwrap();
            intent.phase = IntegrationPhase::PushIntent;
            intent.revision += 1;
            save_intent(&paths, &intent, "simulated_crash_before_observation").unwrap();
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let outcome = publish_prepared(&state, repository.remote.to_str().unwrap(), move |_| {
            callback_calls.fetch_add(1, Ordering::SeqCst);
            async { FreshPublicationAuthority::valid_for(Duration::from_secs(30)) }
        })
        .await
        .unwrap();
        assert_eq!(
            outcome,
            PublicationOutcome::Uncertain {
                observed: Some(repository.base)
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rejects_unrelated_histories_wrong_remote_and_nonisolated_checkout() {
        let repository = repository();
        let other = tempfile::tempdir().unwrap();
        git_ok(other.path(), ["init"]).unwrap();
        configure_identity(other.path());
        fs::write(other.path().join("other.txt"), "other\n").unwrap();
        git_ok(other.path(), ["add", "."]).unwrap();
        git_ok(other.path(), ["commit", "-m", "other"]).unwrap();
        let unrelated = resolve_head(other.path()).unwrap();
        let fetched = format!("{unrelated}:refs/heads/unrelated");
        git_ok(
            &repository.source,
            ["fetch", other.path().to_str().unwrap(), &fetched],
        )
        .unwrap();
        let state = repository._directory.path().join("unrelated.json");
        assert!(prepare_integration(request(&repository, &state, &unrelated)).is_err());

        assert!(
            capture_clean_snapshot(&repository.source, "https://wrong.example/repository.git")
                .is_err()
        );
        let candidate = candidate(&repository, "ordinary", "ordinary\n");
        let ordinary_state = repository._directory.path().join("ordinary.json");
        let ordinary = PrepareIntegration {
            checkout: &repository.source,
            ..request(&repository, &ordinary_state, &candidate)
        };
        assert!(prepare_integration(ordinary).is_err());
    }
}
