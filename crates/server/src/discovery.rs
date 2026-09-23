use serde_json::{Value, json};

pub const CONTINUATION_INSTRUCTIONS: &str = "Keep claiming eligible tasks in the same session after completing or submitting each task. A request to perform or spawn a review does not by itself narrow the session to review-only. Only an explicit single-task, review-only, read-only, stop, or equivalent user boundary overrides this loop. Finish current-attempt cleanup; remove eligible task worktrees only after a fresh task read confirms done, following the cleanup instructions below. Refresh project instructions, policy, decisions and the eligible queue after every task or subagent result. Treat a subagent's final message as an intermediate workflow event, not as the parent agent's final-response trigger. Advance eligible review, integration, publication or finalization work through its guarded workflow, then claim the next eligible task with a fresh attempt and separate worktree. Review approval alone does not complete code work. Pending review or integration does not prevent taking other eligible work. Never approve your own human review, abandon ownership, or release live or uncertain resource holds to move on. Do not give the parent agent's final response until no eligible work remains or required input, access, policy or capability prevents further authorized progress; report that terminal state without inventing work. The service does not launch or wake agents and cannot restart an ended host turn. This loop is performed by the still-running parent agent; a harness that requires deterministic continuation must keep that turn active or automatically re-prompt it until a terminal condition is observed.";

pub fn mcp_instructions() -> String {
    format!(
        "{MCP_INSTRUCTIONS}\n\n{REVIEW_SELECTION_INSTRUCTIONS}\n\n{CONTINUATION_INSTRUCTIONS}\n\n{WORKTREE_CLEANUP_INSTRUCTIONS}"
    )
}

pub const WORKTREE_CLEANUP_INSTRUCTIONS: &str = "After a fresh task read confirms the subject task is done, the completing agent removes its task-specific implementation and integration worktrees. Submission, review approval, or publication alone does not authorize removal; retain worktrees while review or integration is pending. Always preserve the main parent checkout and any main target checkout, plus unrelated worktrees and branches unless separately authorized. Save commits, handoff, and required logs/artifacts outside the worktrees first. Match each exact resolved path and Git worktree identity against the task's registered checkout and git worktree list --porcelain. Confirm no agent or live/uncertain job still uses the tree; inspect tracked changes, untracked files, and meaningful ignored files. Retain the tree if work or evidence is unsaved, ownership is unclear, or inspection fails. From outside the worktree, run git worktree remove with its verified absolute path, without --force or recursive filesystem deletion. Verify removal and report the removed path, or the retained path and concrete blocker, in the final handoff. Cleanup is local agent work; the service does not delete worktrees. After successful worktree removal, delete its exact local and remote task branches only after verifying exclusive task ownership, full refs and commit IDs, and that both local and remote task tips are fully merged into the intended target. Record refs before removing the tree. Preserve main, parent, default, configured target, and host-protected branches, branches used by any remaining worktree, and unrelated or shared branches. Fetch the exact task and target refs from the verified project remote and recheck identities and worktree use before deletion. Use git branch -d -- TASK_BRANCH locally; never override refusal with -D. Delete only the exact remote task ref using git push --force-with-lease=refs/heads/TASK_BRANCH:EXPECTED_OID REMOTE :refs/heads/TASK_BRANCH with freshly verified values. The explicit expected-ref lease rejects a changed remote branch; never use an unguarded force, implicit lease, wildcard, mirror, or prune. Verify and report each removed or retained ref; inspect failed or uncertain results without guessing a new expected commit or blindly retrying deletion. A ref conclusively verified absent in the intended repository is already removed; report it and apply all remaining guards independently to refs that still exist. Never recreate an absent ref. A failed or uncertain lookup is not proof of absence. Also page through existing completed tasks in the authorized project and inspect their registered worktrees on this workstation, applying the same gates to each task individually: fresh done confirmation, saved evidence, safe worktree removal before branch deletion, and no dirty, unmerged, shared, live, or uncertain work. Report per-task paths, refs, and retention reasons; never bulk-delete worktrees or branches.";

pub const REVIEW_SELECTION_INSTRUCTIONS: &str = "Before claiming new implementation work inspect pending agent reviews in the authorized project, unless the user explicitly requested a single-task, review-only, read-only or stopped session. Merely asking to perform or spawn a review is not such a boundary. Apply this review-first selection at startup and after each completed review or task. Preserve current ownership and finish its cleanup first. Page through tasks waiting_review and inspect their workflow activities; ordinary task candidates exclude reviews. Use coordinator_task_workflow and coordinator_activity_get, then coordinator_activity_claim for the highest-priority eligible agent_review or either_review, using its subject task's priority and exact submission/policy revisions. With the native CLI use tasks list, reviews list --task TASK_ID, reviews status and reviews claim. You must be independent of every recorded contributor; a new session with the same agent identity does not make you independent. When project policy enables allow_subagent_reviews and the still-running parent cannot review its own or a contributor's submission, automatically create or reuse a stable project subagent identity and spawn a non-contributing review subagent with its own session and proof. Confirm its complete contribution history before claim. Record implementation helpers in the owner's checkpoint contributor_session_ids before they work; never rename a contributor to obtain eligibility. Skip human reviews, completed reviews, reviews owned by another active worker, and blocked or policy-stale submissions; follow the existing recovery or human reconciliation procedure where required. Claim the review activity through the review workflow with its exact current submission and policy revisions. On a conflict refresh and reconsider eligibility rather than repeatedly attempting the same review. Inspect the saved candidate and evidence before recording a decision through coordinator_review or reviews decide. A child review result is intermediate: the parent refreshes live workflow state, advances eligible integration or other required work, and continues selection without waiting for another user prompt. If no agent review is eligible, claim a ready implementation task. Do not change policy or fabricate approval or independence to make a review eligible.";

pub const MCP_INSTRUCTIONS: &str = "This configured MCP connection supports coordination without the native CLI. Inspect coordinator_session_get; register a newly provisioned session only with its protected configured identity. Read coordinator_orientation, current workflow policy, decisions and tasks; acknowledge complete current instructions; then claim eligible work. Before mutations, verify host/transport-managed durable journaling: the standalone agent-coordinator-mcp adapter exposes coordinator_transport_status with durable_mutation_journal=true. Direct HTTP hosts need independently verified equivalent persistence; authentication alone or model-written notes are insufficient. If unavailable, report missing durable capability and stop before mutations. Supply each mutation's idempotency_key; the host/transport persists its exact arguments before dispatch. Use coordinator_transport_retry on the standalone adapter after uncertainty; otherwise replay the host's saved request and key. Keep bearer tokens and session proofs in protected HTTP headers, never tool arguments or output. Persist attempt/generation, renew before expiry, checkpoint and release when paused. Missing CLI is not a blocker for MCP listing, claiming, renewal, checkpointing or release. Native worktree preparation, managed jobs, binary transfer and guarded Git operations still need the native client. Before those operations securely adopt the same quiescent MCP session with session adopt-mcp, or checkpoint/release and freshly claim using a separate CLI session. Never borrow ownership across sessions or abandon a lease when a local capability is missing. MCP metadata, reads and receipt replay do not renew ownership. Use coordinator_agent_publication_reconcile only with a verified durable local journal and a fresh exact observation of the immutable base or result, confirmed publisher termination, an expired or revoked owner, current policy and decisions, and no live or uncertain jobs or held reservations; changed targets and ambiguity stay human-gated. The service records workstation evidence and does not inspect Git. Human review must come from a human. Read the trusted service's /api/v1/info data.agent_startup.guide for the complete portable bootstrap and transition workflow.";

/// Public, deployment-independent bootstrap material. Project state stays behind auth.
pub fn agent_startup() -> Value {
    json!({
        "schema_version": 2,
        "guide": include_str!("../../../book/src/docs/agent-startup.md"),
        "connection_preference": ["configured_authenticated_mcp", "native_cli"],
        "missing_client_behavior": "Use configured MCP for coordination without a CLI. If neither connection is usable, report setup needs before claiming. A URL alone does not configure MCP tools.",
        "mcp": {
            "requires_native_cli": false,
            "durable_mutations": {"required":true,"standalone_executable":"agent-coordinator-mcp","capability_tool":"coordinator_transport_status","retry_tool":"coordinator_transport_retry","scope":"Adapter tools are local; the HTTP endpoint cannot attest to a direct host journal."},
            "session_probe": "coordinator_session_get",
            "session_registration": "coordinator_session_register",
            "instructions": mcp_instructions(),
            "coordination_tools": ["coordinator_orientation", "coordinator_workflow_policy", "coordinator_decisions_list", "coordinator_tasks_list", "coordinator_task_get", "coordinator_task_workflow", "coordinator_activity_get", "coordinator_activity_claim", "coordinator_activity_release", "coordinator_review", "coordinator_instructions_ack", "coordinator_claim", "coordinator_attempt_get", "coordinator_attempt_renew", "coordinator_checkpoint", "coordinator_attempt_release"],
            "authentication": "Protected bearer token, session ID and session proof supplied by the MCP host; no public OAuth enrollment."
        },
        "local_operations": {
            "requires_native_cli": ["worktree prepare", "jobs run", "jobs inspect", "jobs reconnect", "artifacts upload", "artifacts download", "submissions code", "integrations prepare", "integrations publish", "integrations reconcile", "integrations finish"],
            "missing_capability": "Checkpoint through the owning connection and release with a clear handoff. A workstation-only limitation should not block other workstations.",
            "session_adoption": "session adopt-mcp --mcp-writes-quiescent",
            "alternative_transition": "Reconcile pending writes, checkpoint and release through the original session, then connect and freshly claim through the other client."
        },
        "repository_binding": {
            "file": ".agent-coordinator.toml",
            "required_fields": ["service_url", "project_id"],
            "contains_secrets": false
        },
        "client": {
            "executable": "agent-coordinator",
            "windows_executable": "agent-coordinator.exe",
            "discovery": "PATH or the workstation's configured native client installation",
            "session_commands": "sequential",
            "request_bodies": "JSON files passed with --input"
        },
        "routes": {
            "info": "/api/v1/info",
            "authentication_help": "/api/v1/help/authentication",
            "projects": "/api/v1/projects",
            "session_registration": "/api/v1/sessions",
            "project_orientation": "/api/v1/projects/{project_id}/orientation",
            "tasks": "/api/v1/projects/{project_id}/tasks",
            "required_checks": "/api/v1/projects/{project_id}/workflow-policy",
            "mcp": "/mcp"
        },
        "state_authority": "Authenticated live project policy, tasks, decisions, and evidence; no local backlog is required.",
        "automatic_continuation": {
            "implicit_review_scope": false,
            "subagent_result_is_terminal": false,
            "terminal_conditions": ["no_eligible_work", "required_input", "access_blocker", "policy_blocker", "capability_blocker"],
            "host_requirement": "Keep the parent turn active or automatically re-prompt it; the service cannot wake an ended turn."
        },
        "local_bootstrap": "Read .agent-coordinator.toml. Fetch service_url + /api/v1/info anonymously over HTTPS without redirects. Read data.agent_startup.guide and same-origin authentication_help. Prefer an available authenticated MCP connection; otherwise use the native CLI. Automatically claim eligible work unless this session's user request changes the scope. If neither is usable, report setup needs. Never expose credentials."
    })
}
