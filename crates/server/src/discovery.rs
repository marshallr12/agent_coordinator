use serde_json::{Value, json};

pub const CONTINUATION_INSTRUCTIONS: &str = "Unless the user's request narrows the scope, keep claiming eligible tasks in the same session after completing or submitting each task. Finish current-attempt cleanup; remove eligible task worktrees only after a fresh task read confirms done, following the cleanup instructions below. Refresh project instructions, decisions and the eligible queue, then claim the next task with a fresh attempt and separate worktree. Pending review is not completion, but does not prevent taking other eligible work. Never approve your own human review, abandon ownership, or release live or uncertain resource holds to move on. Stop when no eligible work remains or required input, access or capability prevents further authorized progress; report that state without inventing work. The service does not launch or wake agents; this loop is performed by the running agent.";

pub fn mcp_instructions() -> String {
    format!(
        "{MCP_INSTRUCTIONS}\n\n{REVIEW_SELECTION_INSTRUCTIONS}\n\n{CONTINUATION_INSTRUCTIONS}\n\n{WORKTREE_CLEANUP_INSTRUCTIONS}"
    )
}

pub const WORKTREE_CLEANUP_INSTRUCTIONS: &str = "After a fresh task read confirms the subject task is done, the completing agent removes its task-specific implementation and integration worktrees. Submission, review approval, or publication alone does not authorize removal; retain worktrees while review or integration is pending. Always preserve the main parent checkout and any main target checkout, plus unrelated worktrees and branches unless separately authorized. Save commits, handoff, and required logs/artifacts outside the worktrees first. Match each exact resolved path and Git worktree identity against the task's registered checkout and git worktree list --porcelain. Confirm no agent or live/uncertain job still uses the tree; inspect tracked changes, untracked files, and meaningful ignored files. Retain the tree if work or evidence is unsaved, ownership is unclear, or inspection fails. From outside the worktree, run git worktree remove with its verified absolute path, without --force or recursive filesystem deletion. Verify removal and report the removed path, or the retained path and concrete blocker, in the final handoff. Cleanup is local agent work; the service does not delete worktrees.";

pub const REVIEW_SELECTION_INSTRUCTIONS: &str = "Unless the user's request narrows the scope, before claiming new implementation work inspect pending agent reviews in the authorized project. Apply this review-first selection at startup and after each completed review or task. Preserve current ownership and finish its cleanup first. Page through tasks waiting_review and inspect their workflow activities; ordinary task candidates exclude reviews. Use coordinator_task_workflow and coordinator_activity_get, then coordinator_activity_claim for the highest-priority eligible agent_review or either_review, using its subject task's priority and exact submission/policy revisions. With the native CLI use tasks list, reviews list --task TASK_ID, reviews status and reviews claim. You must be independent of every recorded contributor; a new session with the same agent identity does not make you independent. Skip human reviews, completed or currently owned reviews, and blocked or policy-stale submissions; follow the existing recovery or human reconciliation workflow when needed. If a claim conflicts, refresh and reconsider eligibility rather than repeatedly claiming it. Review the saved candidate and evidence before recording a decision through coordinator_review or reviews decide. If no agent review is eligible, claim a ready implementation task. Do not change policy or fabricate approval to make a review eligible.";

pub const MCP_INSTRUCTIONS: &str = "This configured MCP connection supports coordination without the native CLI. Inspect coordinator_session_get; register a newly provisioned session only with its protected configured identity. Read coordinator_orientation, current workflow policy, decisions and tasks; acknowledge complete current instructions; then claim eligible work. Persist each mutation's idempotency_key and exact arguments before sending, and reuse both after uncertainty. Keep bearer tokens and session proofs in protected HTTP headers, never tool arguments or output. Persist attempt/generation, renew before expiry, checkpoint and release when paused. Missing CLI is not a blocker for MCP listing, claiming, renewal, checkpointing or release. Native worktree preparation, managed jobs, binary transfer and guarded Git operations still need the native client. Before those operations securely adopt the same quiescent MCP session with session adopt-mcp, or checkpoint/release and freshly claim using a separate CLI session. Never borrow ownership across sessions or abandon a lease when a local capability is missing. MCP metadata, reads and receipt replay do not renew ownership. Human review must come from a human. Read the trusted service's /api/v1/info data.agent_startup.guide for the complete portable bootstrap and transition workflow.";

/// Public, deployment-independent bootstrap material. Project state stays behind auth.
pub fn agent_startup() -> Value {
    json!({
        "schema_version": 2,
        "guide": include_str!("../../../book/src/docs/agent-startup.md"),
        "connection_preference": ["configured_authenticated_mcp", "native_cli"],
        "missing_client_behavior": "Use configured MCP for coordination without a CLI. If neither connection is usable, report setup needs before claiming. A URL alone does not configure MCP tools.",
        "mcp": {
            "requires_native_cli": false,
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
        "local_bootstrap": "Read .agent-coordinator.toml. Fetch service_url + /api/v1/info anonymously over HTTPS without redirects. Read data.agent_startup.guide and same-origin authentication_help. Prefer an available authenticated MCP connection; otherwise use the native CLI. Automatically claim eligible work unless this session's user request changes the scope. If neither is usable, report setup needs. Never expose credentials."
    })
}
