//! `checkpoint --push-wip`: pushes the prepared worktree's clean commit to a
//! create-only work-in-progress ref with the candidate checkpoint machinery,
//! so the checkpoint can record its full SHA as service-verifiable recovery
//! evidence. The recorded SHA is authoritative; the ref is only transport.

use crate::{ContextData, Failure, candidate_checkpoint, worktree};

/// The durable WIP ref for one commit of one attempt, inside the candidate
/// namespace the push machinery requires. Including the SHA keeps
/// the create-only ref immutable while successive checkpoints push new ones.
pub fn wip_ref(attempt: &str, revision: &str) -> String {
    format!("refs/agent-coordinator/candidates/wip/{attempt}/{revision}")
}

/// The attempt's saved prepared worktree, which must belong to `generation`.
fn prepared(
    context: &ContextData,
    attempt: &str,
    generation: u64,
) -> Result<worktree::PreparationIntent, Failure> {
    let prepared =
        worktree::load_for_attempt(&context.origin, &context.binding.project_id, attempt)
            .map_err(Failure::invalid)?;
    if prepared.generation != generation {
        return Err(Failure::local(
            5,
            "stale_generation",
            "the prepared worktree belongs to a different attempt generation",
            false,
        ));
    }
    Ok(prepared)
}

/// Pushes and reads back the attempt's prepared worktree commit, returning
/// its full SHA. A configured candidate push helper refuses the explicit ref.
pub fn push(context: &ContextData, attempt: &str, generation: u64) -> Result<String, Failure> {
    let prepared = prepared(context, attempt, generation)?;
    let (revision, _tree) = worktree::current_snapshot(&prepared).map_err(Failure::invalid)?;
    let reference = wip_ref(attempt, &revision);
    let pushed = candidate_checkpoint::checkpoint(
        &candidate_checkpoint::CheckpointRequest {
            checkout: &prepared.destination,
            repository: &prepared.repository_url,
            base_revision: &prepared.base_revision,
            explicit_ref: Some(&reference),
            default_ref: &reference,
            credential_digest: &context.credential_digest,
        },
        candidate_checkpoint::helper_socket_from_env()?.as_deref(),
    )?;
    if pushed.revision != revision {
        return Err(Failure::local(
            5,
            "candidate_changed",
            "the verified WIP ref does not match the clean prepared worktree",
            false,
        ));
    }
    Ok(revision)
}

#[cfg(test)]
mod tests {
    use super::wip_ref;

    #[test]
    fn wip_refs_are_per_attempt_and_commit() {
        assert_eq!(
            wip_ref("a1", &"b".repeat(40)),
            format!(
                "refs/agent-coordinator/candidates/wip/a1/{}",
                "b".repeat(40)
            )
        );
    }
}
