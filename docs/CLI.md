# Agent Coordinator CLI

The native `agent-coordinator` command connects an existing harness to one
Agent Coordinator project. It stores no secret in the repository and never
claims work as a side effect of `connect`.

## Repository and credential configuration

Commit a non-secret `.agent-coordinator.toml` at the repository root:

```toml
service_url = "https://coordinator.example.com"
project_id = "project-id-from-the-operator"
```

The file accepts only those two fields. In particular, a token in this file is
rejected. Set the token in the process environment:

```sh
export AGENT_COORDINATOR_TOKEN='token-issued-by-the-operator'
export AGENT_COORDINATOR_ORIGIN='https://coordinator.example.com'
```

Or create `credentials.toml` under the platform's private Agent Coordinator
configuration directory. On Linux the default is
`$XDG_CONFIG_HOME/agent-coordinator/credentials.toml`, or
`$HOME/.config/agent-coordinator/credentials.toml` when `XDG_CONFIG_HOME` is
unset. On Unix, `AGENT_COORDINATOR_HOME` overrides the directory for headless
installs. The override is rejected on Windows because an arbitrary directory
cannot be assumed to have a private user ACL; Windows uses the current user's
platform configuration directory.

```toml
[[credentials]]
origin = "https://coordinator.example.com"
token = "token-issued-by-the-operator"
```

On Unix, the CLI refuses a credential file readable by group or other users;
set mode `0600`. On Windows, keep the file within the current user's protected
profile and restrict its ACL to that user. The configured origin is matched
exactly after normalization, so changing a repository binding cannot redirect
an existing credential to another service.

Every independent harness must use a distinct, stable session name. Pass it on
every ownership command or configure it in that harness's environment:

```sh
export AGENT_COORDINATOR_SESSION='codex-task-42'
```

PowerShell:

```powershell
$env:AGENT_COORDINATOR_TOKEN = 'token-issued-by-the-operator'
$env:AGENT_COORDINATOR_ORIGIN = 'https://coordinator.example.com'
$env:AGENT_COORDINATOR_SESSION = 'codex-task-42'
```

An environment token is used only when `AGENT_COORDINATOR_ORIGIN` independently
matches the repository service origin. The session proof and pending request state are saved outside the repository in
the platform configuration directory. Separate session names always select
separate files and therefore cannot silently share attempt ownership.

For local development only, `--allow-insecure-loopback` permits `http://localhost`
or a loopback IP address. Other HTTP origins are rejected. Redirects are never
followed, and the CLI accepts only `/api/v1` and `/healthz` request paths.

## Foundation workflow

Connect creates a harness session on its first run and reconciles the saved
session on later runs. It returns the session and the bound project's complete
orientation packet:

```sh
agent-coordinator connect
```

List projects or tasks without reserving anything:

```sh
agent-coordinator projects list --limit 50
agent-coordinator tasks list --limit 50
agent-coordinator tasks list --limit 50 --cursor opaque-cursor-from-the-response
```

Create requests read JSON from a file, which avoids shell-specific quoting.
Project creation requires an authorized operator role and is normally performed
through the human web interface; an ordinary agent credential receives a clear
permission error and exit code `4`.

```sh
agent-coordinator projects create --input new-project.json
agent-coordinator tasks create --input new-task.json
```

```powershell
agent-coordinator.exe tasks create --input .\new-task.json
```

Use `--input -` to read a JSON object from standard input. Claiming performs the
required acknowledgment for the exact complete orientation returned by
`connect`, then submits one claim. It never acknowledges incomplete instructions.

```sh
agent-coordinator claim --next
agent-coordinator claim --task task-id --revision 4
```

Lease and checkpoint operations name the attempt and ownership generation
explicitly. Checkpointing does not renew a lease, and release does not mark a
task complete.

```sh
agent-coordinator renew --attempt attempt-id --generation 2
agent-coordinator checkpoint --attempt attempt-id --generation 2 --input checkpoint.json
agent-coordinator release --attempt attempt-id --generation 2 --input release.json
```

The bounded generic command covers another implemented foundation API path
without inventing commands for future workflow surfaces:

```sh
agent-coordinator request --method get --path /api/v1/projects
agent-coordinator request --method post --path /api/v1/projects/project-id/claims --input claim.json
```

Add `--json` for the complete compact service envelope. The default presents
concise tables and labeled summaries while preserving full instructions during
`connect`.

## Interrupted mutations

Before every POST or PATCH, the CLI durably stores its method, path, JSON body,
and newly generated idempotency key. A transport failure, redirect, rate limit, or server
failure leaves that exact request pending and blocks unrelated writes in the
same harness session. Resolve it with:

```sh
agent-coordinator retry
```

`retry` sends the saved body with the saved key. It never manufactures a new key
after a response may have been lost. A definitive success or client/state error
clears the pending request. Claim and renewal replay output is passed through
unchanged, including the service's fresh `current_authority`; callers must not
treat timing from the original receipt as current authority.

Exit codes are `0` for success, `2` for invalid input, `3` for authentication or
setup required, `4` for denied operations, `5` for state or ownership conflicts,
`6` for unsatisfied requirements, and `7` for transport or temporary service
failures.

## Repository bootstrap text

Repositories can place this short bootstrap in `AGENTS.md`, `CLAUDE.md`, or the
equivalent harness instruction file after the CLI workflow has been installed:

```text
This project coordinates work through Agent Coordinator.
Read .agent-coordinator.toml and run `agent-coordinator connect --json` with a
stable session name unique to this harness. Read the complete orientation and
all required instruction sections before claiming work. Reconnect or show the
returned setup/error details when coordination is unavailable; do not silently
fall back to uncoordinated work. Claim a task before changing code, then renew,
checkpoint, and release it with its exact attempt ID and ownership generation.
```
