use serde_json::{Value, json};

/// Public, deployment-independent bootstrap material. Project state stays behind auth.
pub fn agent_startup() -> Value {
    json!({
        "schema_version": 1,
        "guide": include_str!("../../../book/src/docs/agent-startup.md"),
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
        "local_bootstrap": "Read .agent-coordinator.toml. Fetch service_url + /api/v1/info anonymously over HTTPS without redirects. Read data.agent_startup.guide and same-origin authentication_help. Follow that workflow and automatically claim eligible work unless this session's user request changes the scope. Never expose credentials."
    })
}
