# Agent Coordinator CLI

The native `agent-coordinator` client connects a harness to the project selected
by `.agent-coordinator.toml`. See the [complete CLI guide in the mdBook](../../book/src/docs/CLI.md)
for session, worktree, task, and review commands.

## Credentials and project selection

Keep the non-secret binding in the repository root, including each worktree:

```toml
service_url = "https://coordinator.example.com"
project_id = "project-id-from-the-operator"
project_name = "Billing"
```

`project_name` is optional. It selects a stable local credential directory;
the worktree's folder name and the server's project display name do not select
or rename that directory. All worktrees with `project_name = "Billing"` use
the same file. Use a portable single directory name without separators, reserved
Windows device names, or trailing dots/spaces. Names are case-sensitive on Linux.

Store credentials outside the repository in the selected `credentials.toml`:

```toml
[[credentials]]
origin = "https://coordinator.example.com"
token = "token-issued-by-the-operator"
```

### File lookup patterns

Replace `<project_name>` with the binding's exact selector, such as `Billing`.

| Platform/configuration | With `project_name` | Without `project_name` (legacy) |
| --- | --- | --- |
| Windows | `%APPDATA%/Agent Coordinator/<project_name>/config/credentials.toml` | `%APPDATA%/Agent Coordinator/agent-coordinator/config/credentials.toml` |
| Linux, absolute `XDG_CONFIG_HOME` set | `$XDG_CONFIG_HOME/agent-coordinator/<project_name>/config/credentials.toml` | `$XDG_CONFIG_HOME/agent-coordinator/credentials.toml` |
| Linux, `XDG_CONFIG_HOME` unset, empty, or relative | `$HOME/.config/agent-coordinator/<project_name>/config/credentials.toml` | `$HOME/.config/agent-coordinator/credentials.toml` |
| Unix, `AGENT_COORDINATOR_HOME` override | `$AGENT_COORDINATOR_HOME/<project_name>/config/credentials.toml` | `$AGENT_COORDINATOR_HOME/credentials.toml` |

On Unix, a non-empty `AGENT_COORDINATOR_HOME` takes precedence over XDG/default
lookup. Set it to an absolute private directory. This override is rejected on
Windows. Unix credential files must exclude group/other access; use mode `0600`.
On Windows, restrict the file ACL to the current user and SYSTEM.

Explicit environment credentials take precedence over files:

1. `AGENT_COORDINATOR_TOKEN`, with matching `AGENT_COORDINATOR_ORIGIN`.
2. `AGENT_COORDINATOR_MCP_TOKEN`, with matching `AGENT_COORDINATOR_MCP_URL`.
3. The file selected above.

Every credential is bound to the repository's service origin. A selected project
file that is missing, invalid, or lacks a matching credential is an error; lookup
does not silently fall back to the legacy file. An independently configured MCP
host does not automatically read the CLI credential file.

### Worktree examples

These Linux examples use `$HOME = /home/alex` with no directory override:

| Worktree | `project_name` | Credential file |
| --- | --- | --- |
| `/home/alex/src/billing` | `Billing` | `/home/alex/.config/agent-coordinator/Billing/config/credentials.toml` |
| `/srv/worktrees/billing-review` | `Billing` | `/home/alex/.config/agent-coordinator/Billing/config/credentials.toml` |
| `/home/alex/src/inventory` | `Inventory` | `/home/alex/.config/agent-coordinator/Inventory/config/credentials.toml` |

On Windows, `C:/src/billing` and `D:/worktrees/billing-review` with the same
`Billing` selector both use
`%APPDATA%/Agent Coordinator/Billing/config/credentials.toml`. An `Inventory`
selector uses `%APPDATA%/Agent Coordinator/Inventory/config/credentials.toml`.

Use a distinct stable `--session` for each independent harness, even when its
worktree shares a credential file. See the mdBook's
[worktree credential examples](../../book/src/docs/CLI.md#project-credential-directories-and-worktrees)
and [subagent identity setup](../../book/src/docs/CLI.md#subagent-identities-and-reviews).
