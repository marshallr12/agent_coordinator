use serde_json::{Value, json};

pub const CONTINUATION_INSTRUCTIONS: &str = "Unless the user's request narrows the scope, keep claiming eligible tasks in the same session after completing or submitting each task. Finish current-attempt cleanup, refresh project instructions, decisions and the eligible queue, then claim the next task with a fresh attempt and separate worktree. Pending review is not completion, but does not prevent taking other eligible work. Never approve your own human review, abandon ownership, or release live or uncertain resource holds to move on. Stop when no eligible work remains or required input, access or capability prevents further authorized progress; report that state without inventing work. The service does not launch or wake agents; this loop is performed by the running agent.";

pub fn mcp_instructions() -> String {
    format!("{MCP_INSTRUCTIONS}\n\n{CONTINUATION_INSTRUCTIONS}")
}

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
            "coordination_tools": ["coordinator_orientation", "coordinator_workflow_policy", "coordinator_decisions_list", "coordinator_tasks_list", "coordinator_task_get", "coordinator_instructions_ack", "coordinator_claim", "coordinator_attempt_get", "coordinator_attempt_renew", "coordinator_checkpoint", "coordinator_attempt_release"],
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
