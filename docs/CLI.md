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
agent-coordinator claim --mode recovery --task expired-task-id --revision 7
```

`--mode work` is the default. Recovery mode atomically claims an expired or
revoked attempt for inspection; it does not permit edits until
`recovery resolve` records the required saved-work and running-job checks. `--next` can
select the next eligible item in either mode.

Lease and checkpoint operations name the attempt and ownership generation
explicitly. Checkpointing does not renew a lease, and release does not mark a
task complete.

```sh
agent-coordinator renew --attempt attempt-id --generation 2
agent-coordinator checkpoint --attempt attempt-id --generation 2 --input checkpoint.json
agent-coordinator release --attempt attempt-id --generation 2 --input release.json
```

## Worktrees

Before changing code, prepare and register a separate clean worktree. The CLI
first checks that the named attempt and generation still grant current work
authority. It then verifies that the source checkout is clean and has a Git
remote matching the repository URL configured for the bound project. The
project is never inferred from a directory name.

```sh
agent-coordinator worktree prepare \
  --attempt attempt-id --generation 2 \
  --source /srv/src/project \
  --path "/srv/worktrees/task 42" \
  --branch agent/task-42 --base origin/main
```

```powershell
agent-coordinator.exe worktree prepare `
  --attempt attempt-id --generation 2 `
  --source 'C:\src\project' `
  --path 'C:\agent worktrees\task 42' `
  --branch agent/task-42 --base origin/main
```

The destination's parent must exist, while the destination itself must be new.
Before invoking Git, the CLI saves the exact source, destination, branch, base
selector, resolved full base commit, attempt, and generation outside the
repository. Repeating the same command reconciles that saved destination even
if a branch such as `origin/main` later advances. Different arguments are
refused. The command never resets, stashes, deletes, or cleans any checkout.
It records the resolved per-worktree Git directory identity, full base commit,
branch, path, workstation, and clean state with the service. A lost registration
response remains a normal durable pending mutation and `retry` reuses its body
and idempotency key.

## Resources

Human administrators create canonical resource definitions through the web or
administrative API. Agents can list those definitions and atomically reserve a
complete set for their current attempt:

```sh
agent-coordinator resources list --limit 50
agent-coordinator resources reserve \
  --attempt attempt-id --generation 2 --input reservation.json
agent-coordinator reservations list --limit 50
```

```json
{
  "items": [
    {"resource_id": "resource-id", "units": 1},
    {"resource_id": "second-resource-id", "units": 2}
  ]
}
```

The reservation is all-or-none. An expired attempt, disconnected observer, or
missing heartbeat does not release a physical resource. Once every attached job
has a terminal producer result, the owning agent can release it explicitly:

```sh
agent-coordinator resources release \
  --reservation reservation-id --generation 2 \
  --reason "All attached producers have exited"
```

```powershell
agent-coordinator.exe resources release `
  --reservation reservation-id --generation 2 `
  --reason 'All attached producers have exited'
```

`reservations release` is an equivalent spelling. Uncertain physical resources
require an explicit human resolution through the web or administrative API.

## Durable local jobs

`jobs run` accepts a local program and argument vector from JSON. It requires a
held reservation and the worktree prepared for the same current attempt. The
worktree must be clean at a committed revision; the job record captures the full
commit and tree identities before registration.

```json
{
  "label": "workspace tests",
  "program": "/home/agent/.cargo/bin/cargo",
  "argv": ["test", "--workspace", "--locked"],
  "environment": {
    "PATH": "/home/agent/.cargo/bin:/usr/local/bin:/usr/bin:/bin",
    "HOME": "/home/agent",
    "CARGO_HOME": "/home/agent/.cargo",
    "RUSTUP_HOME": "/home/agent/.rustup",
    "TMPDIR": "/tmp",
    "CARGO_TERM_COLOR": "never"
  },
  "log_limit_bytes": 1048576
}
```

```sh
agent-coordinator jobs run \
  --attempt attempt-id --generation 2 \
  --reservation reservation-id \
  --checkout "/srv/worktrees/task 42" \
  --input job.json
```

PowerShell uses the same JSON shape; `program` must be an absolute native path:

```json
{
  "label": "workspace tests",
  "program": "C:\\Users\\agent\\.cargo\\bin\\cargo.exe",
  "argv": ["test", "--workspace", "--locked"],
  "environment": {
    "PATH": "C:\\Users\\agent\\.cargo\\bin;C:\\Windows\\System32",
    "USERPROFILE": "C:\\Users\\agent",
    "CARGO_HOME": "C:\\Users\\agent\\.cargo",
    "RUSTUP_HOME": "C:\\Users\\agent\\.rustup",
    "SystemRoot": "C:\\Windows",
    "TEMP": "C:\\Users\\agent\\AppData\\Local\\Temp",
    "TMP": "C:\\Users\\agent\\AppData\\Local\\Temp"
  },
  "log_limit_bytes": 1048576
}
```

```powershell
agent-coordinator.exe jobs run `
  --attempt attempt-id --generation 2 `
  --reservation reservation-id `
  --checkout 'C:\agent worktrees\task 42' `
  --input .\job.json
```

Before registration or launch, the CLI durably saves random job, producer,
runner, and reporter identities. It then stores the scoped reporter bearer only
in the protected local job file. The detached guardian clears its environment;
the producer receives only the environment explicitly listed in the JSON file.
`AGENT_COORDINATOR_*` variables are rejected, and raw arguments, environment,
and logs are never uploaded. Standard output and error are kept in separate,
bounded, protected local files shown by `jobs inspect`.
Set `log_limit_bytes` to `0` to discard both streams; the maximum is 67108864
bytes per stream.

Replace the example environment values with the workstation's actual trusted
paths. Include everything the program and its child processes require, such as
`PATH`, the user profile, toolchain directories, temporary directories, and
Windows `SystemRoot`. Do not copy coordinator token, origin, session, or proof
variables into this map.

To allow bounded attempt renewal while the job runs, name the exact current
harness process and a server-limited window:

```sh
agent-coordinator jobs run ... --renew-for-seconds 1800 --watch-pid 12345
```

Renewal stops when that exact process exits, its authority expires, or the
window ends. Job observation may continue after renewal stops. Omitting those
options runs and reports the job without delegated attempt renewal.

Use the returned job ID for later inspection. Remote status and list commands do
not launch or reconnect any producer:

```sh
agent-coordinator jobs list --limit 50
agent-coordinator jobs status --job job-id
agent-coordinator jobs inspect --job job-id
agent-coordinator jobs reconnect --job job-id
```

`jobs inspect` reads protected local state. `jobs reconnect` starts a detached
observation-only guardian and works while the service is temporarily unavailable;
pending observations remain durable for later delivery. It never launches a
producer. If a durable launch intent exists without a recorded process identity,
the state becomes unknown and the guardian refuses to launch again. A missing
local journal is also treated as uncertain because the producer may already have
run. PID alone is never used as producer identity.

## Recovery inspection

Inspect the expired attempt, its registered checkout, jobs, and resource holds,
then record one explicit disposition from a JSON file:

```sh
agent-coordinator recovery inspect --attempt expired-attempt-id
agent-coordinator jobs list --limit 50
agent-coordinator reservations list --limit 50
agent-coordinator recovery resolve \
  --attempt recovery-attempt-id --generation 3 --input recovery.json
```

```json
{
  "saved_work_checked": true,
  "running_jobs_checked": true,
  "disposition": "resume",
  "summary": "The exact producer is terminal and the saved commit was inspected."
}
```

Use the disposition and inspection fields required by the current service
orientation and attempt detail. Recovery does not cancel or restart jobs and
does not clear a resource merely because an observer or lease expired.

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
