//! Service-verifiable recovery evidence. A checkpoint may record the full
//! commit SHA of the work-in-progress commit its owner pushed to a durable ref;
//! the recorded SHA is authoritative and the ref is only transport. Recovery
//! of an attempt whose latest checkpoint recorded one is verified by the
//! service; a legacy checkpoint keeps the recoverer's local attestations.
use crate::{coordination::Attempt, error::AppError};
use coordinator_core::RecoveryInput;
use serde_json::json;
use sqlx::SqliteConnection;

/// How a recovery resolution's evidence was established.
pub(crate) enum Evidence {
    /// The service checked the old attempt, the generation and the revision.
    Verified,
    /// A legacy checkpoint: the recoverer attested its own inspection.
    Attested,
}

impl Evidence {
    /// The label recorded in the resolution's response and event.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Verified => "service_verified",
            Self::Attested => "local_attestation",
        }
    }
}

/// Refuses a present `value` that is not a full lowercase 40-hex commit SHA.
pub(crate) fn validate_revision(value: Option<&str>, name: &str) -> Result<(), AppError> {
    match value {
        Some(v) if v.len() != 40 || !v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) => {
            Err(AppError::bad_request(&format!(
                "{name} must be a full 40-character lowercase hexadecimal commit SHA."
            )))
        }
        _ => Ok(()),
    }
}

/// Establishes the evidence for resolving `recovery`, the caller's recovery
/// attempt. Runs inside the resolution's write transaction.
pub(crate) async fn establish(
    c: &mut SqliteConnection,
    recovery: &Attempt,
    input: &RecoveryInput,
) -> Result<Evidence, AppError> {
    let Some(prior) = predecessor(c, recovery).await? else {
        return attested(input);
    };
    match latest_revision(c, &prior.id).await? {
        Some(recorded) => verify(
            &prior,
            recovery,
            &recorded,
            input.fetched_revision.as_deref(),
        ),
        None => attested(input),
    }
}

/// The attempt the recovery claim superseded: the task's attempt with the
/// highest generation below the recovery attempt's.
async fn predecessor(
    c: &mut SqliteConnection,
    recovery: &Attempt,
) -> Result<Option<Attempt>, AppError> {
    Ok(sqlx::query_as("SELECT * FROM attempts WHERE project_id=? AND task_id=? AND generation<? ORDER BY generation DESC LIMIT 1")
        .bind(&recovery.project_id).bind(&recovery.task_id).bind(recovery.generation)
        .fetch_optional(&mut *c).await?)
}

/// The revision recorded by the attempt's latest checkpoint, if any.
async fn latest_revision(
    c: &mut SqliteConnection,
    attempt: &str,
) -> Result<Option<String>, AppError> {
    let latest: Option<Option<String>> = sqlx::query_scalar(
        "SELECT revision FROM checkpoints WHERE attempt_id=? ORDER BY created_at DESC,rowid DESC LIMIT 1",
    )
    .bind(attempt)
    .fetch_optional(&mut *c)
    .await?;
    Ok(latest.flatten())
}

/// Requires the old attempt to be terminal (expired or revoked, both stored as
/// `expired`), the recovery claim to have bumped the generation, and the
/// recoverer's fetched SHA to equal the recorded revision.
fn verify(
    prior: &Attempt,
    recovery: &Attempt,
    recorded: &str,
    fetched: Option<&str>,
) -> Result<Evidence, AppError> {
    if prior.state != "expired" || recovery.generation <= prior.generation {
        return Err(AppError::conflict(
            "recovery_evidence_unverified",
            "The superseded attempt is not expired, or this recovery claim did not bump its generation.",
        ));
    }
    let details = json!({"recorded_revision": recorded, "attempt_id": prior.id});
    match fetched {
        None => Err(AppError::conflict(
            "recovery_revision_required",
            "The expired attempt's checkpoint recorded a revision. Fetch its work-in-progress ref and supply fetched_revision.",
        )
        .with_details(details)),
        Some(f) if f != recorded => Err(AppError::conflict(
            "recovery_revision_mismatch",
            "The fetched revision does not equal the revision the service recorded. The recorded checkpoint SHA is authoritative.",
        )
        .with_details(details)),
        Some(_) => Ok(Evidence::Verified),
    }
}

/// The legacy path: the recoverer must attest both inspections.
fn attested(input: &RecoveryInput) -> Result<Evidence, AppError> {
    if !input.saved_work_checked || !input.running_jobs_checked {
        return Err(AppError::bad_request(
            "Recovery must inspect saved work and running jobs before choosing resume or restart. Release as blocked if inspection is incomplete.",
        ));
    }
    Ok(Evidence::Attested)
}
