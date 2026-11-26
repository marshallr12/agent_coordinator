# Project scaffolding design: `agent-coordinator adopt`

Status: **proposed design, revision 2, not implemented.** No command or
dashboard route described here exists yet.
[Implementation status](implementation-status.md) lists what works today; this
chapter must not be read as a description of it. It is the complete input for
the implementation task in [Implementation plan](#implementation-plan). Task
`e4e9d7e0` (design only) produced revision 1; [Revision notes](#revision-notes)
records what the review changed and why.

## Purpose

Today a person must create a project in the dashboard, copy its two-line
`.agent-coordinator.toml`, and hand-write instructions for Claude Code or Codex
before a repository can use the service. `adopt` migrates an existing local Git
repository to a coordinator-backed project in one guided run. **Goal: the very
next Claude Code or Codex session started in that repository connects to the
service and can claim work, with no further setup.**

Ordering of aims when they conflict: (1) never leak a credential, (2) never
lose or silently rewrite the user's files, (3) leave the next session ready,
(4) minimise questions.

### Assumptions

- The CLI is installed and already holds a working credential for the service
  origin (the design task's stated assumption).
- `claude` or `codex` can be started from the calling terminal. They are needed
  only for the optional backlog migration.

### Non-goals (version 1)

- **Issuing credentials.** Credentials are not project-scoped: the credential
  the CLI already holds reaches the new project. Issuance stays a dashboard
  action by an administrator. `adopt` never writes a credential file.
- **Creating the project without a human.** The service keeps project creation
  human-only; `adopt` prepares the dashboard form and waits for one click.
- Enrolling a workstation that has no credential (`adopt` prints the existing
  setup pointer and exits).
- Configuring an MCP server in Claude Code or Codex. MCP needs a per-harness
  session ID and proof ([MCP guide](mcp-guide.md)); the injected instructions
  already prefer an existing MCP connection and fall back to the CLI.
- Installing or upgrading the CLI (see
  [client upgrade](agent-startup.md#cli-fallback-startup)). `adopt` only checks.
- Pushing to a remote, or changing project or workflow policy.
- Editing `.claude/settings*.json`, Codex configuration, hooks, or permission
  allow-lists.

## Facts this design rests on

Verified against `origin/main` at commit `263b0ad`.

| Fact | Source |
| --- | --- |
| `POST /api/v1/projects` takes `{name, repository_url, target_branch}` (name ≤120, URL ≤2048, branch ≤255, no whitespace or leading dash), requires a **human** actor (`human_gate` `project_administration`), and fails with `project_exists` on a duplicate name. | `crates/server/src/coordination.rs` `create_project`, `admin_or_operator` |
| `GET /api/v1/projects` is paged, excludes archived projects by default, and is readable by any authenticated principal, including an agent credential. | `coordination.rs` `projects` |
| Agent credentials are issued only by a human administrator and are **not project-scoped**. | `crates/server/src/auth.rs` `create_agent` |
| The binding file is exactly `service_url`, `project_id`, optional `project_name` (unknown fields rejected). | `crates/cli/src/config.rs` `RepositoryBinding` |
| The dashboard is a hash-routed single page (`applyRoute`), keeps the fragment across sign-in, and creates projects from a dialog with the fields `project-name`, `repository-url`, `target-branch`. | `web/app.js` `applyRoute`, `openDialog('project')`, `createProject` |
| `required_capabilities` in `/api/v1/info` `client_compatibility` is a requirement **on clients**: a client lacking a listed capability is reported incompatible. It is not a place to advertise optional server features. | `crates/core/src/lib.rs` `CLIENT_CAPABILITIES`; `crates/cli/src/main.rs` `compatibility_report` |
| `client-info` and `compatibility` are dispatched before binding discovery; every other command calls `build_context`, which needs a binding. | `crates/cli/src/main.rs` `run` |
| An agent credential can create tasks, knowledge and decisions. Agent-created tasks pass through admission, which may hold them `planned`. Import *apply* is human-only. | `coordination.rs` `create_task`, `admission.rs`; [import contract](import-contract.md) |
| The startup guide tells agents not to `git pull` a dirty checkout. | [agent startup](agent-startup.md#synchronize-the-checkout-first) |
| CLI exit codes: 0 ok, 2 invalid input, 3 auth/setup required, 4 denied, 5 conflict, 6 unsatisfied requirement, 7 transport/temporary. | [CLI guide](CLI.md) |

Consequences:

- **The CLI cannot create a project by itself.** It hands the human a prefilled
  dashboard form and then *observes* the result with read-only calls.
- **`adopt` sends no mutation to the service.** Every request it makes is a
  `GET`. It therefore needs no session, no idempotency key, and no mutation
  journal, and a rerun is always safe.

## Command surface

```text
agent-coordinator adopt [PATH]          # PATH defaults to the current directory
  --service-url URL        default https://agents.sithbit.com
  --name NAME              project display name (default: repository name)
  --repository-url URL     override the detected remote URL
  --branch BRANCH          target branch (default: detected)
  --project ID             link to this existing project; never opens the dashboard
  --harness claude|codex|both|auto   instruction files to prepare (default auto)
  --no-browser             print the dashboard link; do not try to open it
  --approval-timeout SECS  how long to wait for the project to appear (default 900)
  --commit | --no-commit   commit the files adopt wrote (default --commit)
  --migrate ask|now|off    backlog migration session (default ask)
  --migration-agent claude|codex     harness for the migration (default: detected)
  --replace-binding        overwrite a binding that points elsewhere
  --force-instructions     insert the block even if the file already mentions the binding
  --non-interactive        never prompt; fail with needs_input
  --yes                    accept every default (implies --non-interactive)
  --dry-run                print the plan and diffs; write nothing
  --json                   existing global flag; machine-readable events
```

Every setting has a default, so `agent-coordinator adopt` with no arguments is
the normal invocation. `scaffold` is accepted as a clap alias.

`adopt` is a **binding-less command**: like `client-info` and `compatibility`
it must run before any `.agent-coordinator.toml` exists, so it is dispatched
before `build_context`. It does not require `--session`. The global
`--allow-insecure-loopback` flag keeps its meaning. The service URL must be
HTTPS (loopback HTTP only with that flag); redirects are never followed.

The name `adopt` is unrelated to the existing `session adopt-mcp` command, which
adopts an MCP *session*. Help text and documentation say "adopt a repository" to
keep the two apart.

## Run overview

1. **Preflight** — repository, tool, service, and credential checks.
2. **Detect** — gather defaults from the repository.
3. **Interview** — ask only what could not be detected.
4. **Plan** — print exactly what will happen; confirm (skipped by `--yes`).
5. **Resolve the project** — link an existing one, or wait for the human to
   create it from a prefilled dashboard form.
6. **Write local files** — binding and instruction block.
7. **Verify** — prove the next session will work.
8. **Commit** — local only, limited to the files adopt wrote.
9. **Migrate the backlog** — optional interactive Claude Code or Codex session.
10. **Report** — the next-session summary.

Steps 1–5 change nothing on disk. Nothing in any step changes the service.

### 1. Preflight

Fail early with a concrete fix:

- `PATH` is inside a Git work tree (`git rev-parse --show-toplevel`); adopt
  operates on that root. Bare repositories are refused. A linked worktree is
  accepted: the binding is written to its root and the report says other
  worktrees receive it by checkout.
- The files adopt may write (`.agent-coordinator.toml`, the instruction files
  it will touch) have **no uncommitted changes**. Other dirty files are allowed
  but reported. A merge or rebase in progress is refused. (Exit 6.)
- Service reachable: anonymous `GET {service_url}/api/v1/info`, no redirects.
  Run the same comparison as `compatibility --json`; an incompatible client
  stops the run (exit 6) with the upgrade pointer.
- A credential for the service **origin** is present (environment or the shared
  `credentials.toml`) and `GET /api/v1/me` returns an agent actor with `write`
  access. Missing or rejected: print the same-origin authentication help and
  the credential file location, write nothing, and exit 3.
- `agent-coordinator` is resolvable on `PATH` (compare the running executable's
  directory with `PATH`). Absent is a warning: the next session would not find
  the client.
- An existing `.agent-coordinator.toml`:
  - same origin and a project that exists → **repair mode**: skip steps 3–5 and
    re-run steps 6–10 idempotently;
  - different origin or project → refuse (exit 5) unless `--replace-binding`,
    showing the old and new values;
  - unreadable or invalid → refuse and show the parser error.

### 2. Detect

| Item | Detection | Fallback |
| --- | --- | --- |
| Repository URL | `git remote get-url origin`, or the sole remote. **Strip any `user:password@` userinfo, query and fragment before use or display**; say so if stripping changed it. | Ask. A project needs a non-empty URL. |
| Target branch | `git symbolic-ref --short refs/remotes/origin/HEAD`, else the current branch, else `main` | Ask |
| Project name | Last path segment of the URL minus `.git`; else the directory name | Ask. ≤120 characters. |
| Existing project | `GET /api/v1/projects` (all pages): match by normalised `repository_url` | Offer to link instead of create |
| Harnesses | `claude` / `codex` on `PATH`; presence of `CLAUDE.md`, `.claude/`, `AGENTS.md`, `.codex/` | Both |
| Migration sources | Regular files at the repository root named `HANDOFF.md` or `BACKLOG.md` (case-insensitive). A repository that has `docs/pre-coordinator/`, or whose `HANDOFF.md` contains the stub marker `<!-- agent-coordinator:migrated -->`, is **already migrated** and has no sources. | None |
| Line endings, BOM, trailing newline of the instruction files | Read bytes | LF |

URL normalisation for matching: lowercase the host, drop a trailing `.git` and
`/`, and treat `git@host:org/repo` and `https://host/org/repo` as equal.

### 3. Interview

Prompts appear only when stdin and stderr are terminals and neither
`--non-interactive` nor `--yes` is set. Every prompt has a flag as its
non-interactive form and shows its default in brackets. When a required value
cannot be defaulted and prompting is off, exit 2 with `needs_input` listing
**every** missing flag at once.

1. *Existing project match* — "This repository is already project **X**
   (id …). Link to it? [Y/n]". Yes skips creation.
2. *Project name, repository URL, branch* — shown together; confirm or edit.
3. *Which assistants will work here?* — Claude / Codex / both.
4. *Commit the changes? [Y/n]*.
5. *Start a session now to migrate HANDOFF.md, BACKLOG.md? [Y/n]* — asked
   last, after the repository is connected (step 9), listing files and sizes.

With `--yes`: an exact existing match is linked, the files are committed, and
no migration session starts (it needs a terminal and a person).

With `--non-interactive` alone, or when there is no terminal, confirmations are
never assumed: the plan confirmation, "link to X?" and "use project?" each
require `--yes` or the flag that settles them (`--project ID` for both project
questions), and are otherwise reported together as `needs_input` (exit 2)
before anything is opened or written.

### 4. Plan

A concrete preview, then one confirmation:

```text
Service        https://agents.sithbit.com
Project        create "billing" in the dashboard (one click)   | link existing X (id …)
Credential     reuse the existing credential for this origin
Will write     .agent-coordinator.toml (new)
               AGENTS.md (update: insert 15-line block at top)
               CLAUDE.md (new: "@AGENTS.md")
Will not touch credentials, remotes, hooks, or any other file.
Commit         yes, only the files listed above
Migration      offered afterwards: HANDOFF.md (4 KiB), BACKLOG.md (9 KiB) via claude
```

`--dry-run` prints this plus unified diffs of every file and stops before
step 5; it opens no browser and writes nothing.

### 5. Resolve the project

**Link.** Applies when detection matched an existing project and the user
accepted it, or `--project ID` was given. The CLI confirms the project appears
in `GET /api/v1/projects` (so it exists and is not archived) and continues.

**Create through the dashboard.** Otherwise:

1. Record the set of project IDs currently returned by `GET /api/v1/projects`.
2. Build the link (each value percent-encoded with `encodeURIComponent`
   semantics, so no raw `/`, `&`, `#` or `?` survives in a value):

   ```text
   https://agents.sithbit.com/#/new-project?name=billing&repository_url=https%3A%2F%2Fgithub.com%2Facme%2Fbilling&target_branch=main
   ```

   The link carries only those three values. It never carries a token, a
   session name, or URL userinfo.
3. Print it with the manual equivalent, and open it in the default browser
   unless `--no-browser` (`xdg-open` / `open` / `start`; best effort, never
   fatal):

   ```text
   Create the project in the dashboard (sign in if asked):
     https://agents.sithbit.com/#/new-project?name=billing&…
   If the form does not open prefilled, choose "New project" and enter:
     Name            billing
     Repository URL  https://github.com/acme/billing
     Target branch   main
   Waiting for the project to appear… (Ctrl-C to stop; rerun to continue)
   ```

   Because the manual values are always printed, an older service whose
   dashboard lacks the prefill route still works. No server feature flag is
   needed.
4. Poll `GET /api/v1/projects` every 3 seconds (all pages) until a project
   appears whose ID was **not** in the recorded set and whose normalised
   `repository_url` equals the requested one, or `--approval-timeout` elapses.
   Transport errors back off to 10 seconds and do not end the wait.
5. Exactly one such project: show its name and ID. Interactive runs confirm
   "Use project **billing** (id …)? [Y/n]"; `--yes` accepts. The human may
   have edited the name in the form; the URL is the identity.
6. More than one: list them; interactive runs choose, others exit 5 with the
   `--project ID` hint.
7. Timeout: exit 7. Nothing was written. A rerun detects the project if it now
   exists (step 2) and links it, so there is no saved state to resume.

If the human changes the repository URL in the form, nothing matches; the
timeout message names `--project ID` for that case.

## Dashboard change

One new route in `web/app.js`:
`#/new-project?name=…&repository_url=…&target_branch=…`.

- `applyRoute` separates the query from the fragment **before** splitting on
  `/`, and parses it with `URLSearchParams`.
- It opens the existing project dialog (`openDialog('project')`) and assigns
  the three values with `.value`, never through HTML. A value longer than the
  server bound (120 / 2048 / 255) or containing a control character is
  ignored, leaving its field empty.
- A notice line in the dialog reads "Prefilled by a link. Check the repository
  URL before creating." The dialog is **never submitted automatically**; the
  human presses the existing Create button and the existing
  `POST /api/v1/projects` path runs unchanged.
- After the dialog opens, the fragment is replaced with the overview route
  (`history.replaceState`), so a reload or the Back button does not reopen it.
- Signed out: the existing sign-in flow already calls
  `applyRoute(location.hash)` after authentication, so the dialog opens once
  the human has signed in.
- Accessibility and refresh behaviour follow
  [web UI refresh](web-ui-refresh.md).

There is no new endpoint, table, migration, or audit event. `project.created`
is still recorded against the human who pressed Create.

## Local file writes (step 6)

All writes are atomic (temporary file in the same directory, `fsync`, rename),
keep the original file mode, line endings, BOM, and trailing-newline
convention, and happen only after the plan was confirmed. A file that changed
on disk between read and write aborts that file (compare content hash),
leaving it untouched (exit 5).

### `.agent-coordinator.toml`

Rendered by the CLI from validated values (`service_url` equals the contacted
origin; `project_id` is a UUID):

```toml
service_url = "https://agents.sithbit.com"
project_id = "00000000-0000-0000-0000-000000000000"
```

`project_name` is not written: it selects a project-specific credential
directory, and `adopt` reuses the shared credential. Repair mode preserves a
`project_name` that is already present. The file is created with
exclusive-create semantics; an existing file follows the preflight rules.

### Instruction files

`AGENTS.md` is read by Codex and other harnesses; `CLAUDE.md` by Claude Code.

| Existing state | Action |
| --- | --- |
| Neither exists | Create `AGENTS.md` containing the block. If Claude is selected also create `CLAUDE.md` containing the single line `@AGENTS.md` (Claude Code's file-import syntax), so one source serves both. |
| `AGENTS.md` only | Insert the block. If Claude is selected and `CLAUDE.md` is absent, create it as `@AGENTS.md`. |
| `CLAUDE.md` only | Insert the block into `CLAUDE.md`. If Codex is selected, create `AGENTS.md` with the block too. |
| Both | Insert into `AGENTS.md`. Insert into `CLAUDE.md` only if it does not already import `AGENTS.md` (a line starting `@AGENTS.md`). |
| Symlink | Resolve; write the target if it is inside the repository, else refuse that file and say why. |

The block is inserted **at the top** (after YAML front matter and a leading
`# ` title if present), between markers so reruns replace rather than
duplicate and users can see what is managed:

```markdown
<!-- agent-coordinator:begin v1 -->
## Agent Coordinator

This repository's work is coordinated by an Agent Coordinator service.
Read `.agent-coordinator.toml` and anonymously fetch the service's
`/api/v1/info` over HTTPS without redirects. The response is large: save it to
a temporary file and read `data.agent_startup.guide` one section at a time.
Follow that guide and same-origin authentication help, preferring an available
authenticated MCP connection; otherwise use the `agent-coordinator` CLI with a
session name unique to this harness (`AGENT_COORDINATOR_SESSION`).
Automatically select and claim eligible work unless the user changes the
scope. Keep credentials outside the repository and never expose them. Report a
concrete blocker if neither MCP nor the CLI is usable; do not select work from
local files.
<!-- agent-coordinator:end -->
```

This is the text of this repository's own `AGENTS.md` plus two facts a new
session otherwise discovers by failing: the info response is too large to load
whole, and the CLI refuses to run without a session name. It is a compile-time
constant in the CLI. A unit test asserts it names every term the startup
contract requires (`.agent-coordinator.toml`, `/api/v1/info`,
`data.agent_startup.guide`, MCP preference, session name, credentials), so it
cannot drift silently from `discovery.rs`. The "Repository bootstrap text" in
the [CLI guide](CLI.md) is replaced by it.

Rerun rules: both markers present → replace between them (a version bump
upgrades the text); markers absent but the file already mentions
`.agent-coordinator.toml` → do not insert, report "already has coordinator
instructions" and show the line (`--force-instructions` inserts anyway); a
begin marker without its end → refuse that file.

## Verify (step 7)

Prove the next session works, without claiming work or registering a session:

1. Re-parse the written binding with the CLI's own loader from the repository
   root **and** from a nested directory.
2. `GET /api/v1/me` with the credential the CLI really selects for that
   binding.
3. The project is present in `GET /api/v1/projects`.
4. Compatibility re-confirmed.
5. Each selected harness's instruction file contains the intact block; the
   harness executable is on `PATH` (warning only if missing).
6. **Sandbox hint**: warn that a harness sandbox can hide `$HOME/.config` and
   the network, and name the remedies already in the CLI guide
   (`AGENT_COORDINATOR_TOKEN` / `AGENT_COORDINATOR_ORIGIN` in the process
   environment, or `--state-dir` inside the allowed area). Advice only.

Failure of 1–4 exits 6 naming the failing check. Files already written remain
(they are correct); a rerun is repair mode and verifies again.

## Commit (step 8)

Default **yes**, because the startup guide forbids `git pull` in a dirty
checkout and other clones and worktrees only receive the binding by commit.
`--no-commit` (or answering no) skips it.

Only when the index is clean outside adopt's paths (otherwise skip with a
message; never sweep up someone else's staged work): `git add -- <paths>`, then
`git commit -m "Connect repository to Agent Coordinator" -- <paths>`, limited
by pathspec to exactly the files adopt wrote, honouring hooks and signing
configuration. A commit failure is reported, not fatal, and leaves the files in
place. **Never pushes**; the report says the commit is local. No attribution
trailer is added.

The commit comes before migration because the migration procedure starts from a
clean tree and pins provenance to `HEAD`.

## Migrate the backlog (step 9)

When `HANDOFF.md` or `BACKLOG.md` exists, `adopt` can start an **interactive**
Claude Code or Codex session in the repository that migrates the documents into
coordinator tasks, knowledge records and decisions. The person at the terminal
reviews and approves inside that session. `adopt` itself creates no records.

### The migration kit

The procedure is the existing `coordinator-migrate` workflow, brought into
this repository as `crates/cli/assets/migrate/` and compiled into the CLI with
`include_str!`:

| File | Content |
| --- | --- |
| `PROCEDURE.md` | The step-by-step procedure: preflight, inventory, plan fragments, validate and review with the user, apply, retire the Markdown files, verify and report. |
| `mapping.md` | Section-to-record mapping rules, field limits, and the `HANDOFF.md` stub template. |
| `migrate.py` | Standard-library Python helper: `preflight`, `inventory`, `validate`, `apply` (idempotent, with a ledger outside the repository), `verify`. It calls the `agent-coordinator` CLI. |

The design branch carries the three source files unmodified from the owner's
workstation skill; the implementation adapts them:

- Remove harness-specific wording: skill paths become the kit directory; "ask
  with AskUserQuestion" becomes "ask the user and wait for the answer"; the
  subagent fan-out becomes optional ("if your harness supports subagents").
- The `AGENTS.md` step defers to adopt's managed block: do not add a second
  "Agent Coordinator" section and do not edit between the markers. Migration
  notes (archive location, context-search guidance) go in a separate section
  below the block.
- State once, near the top, that text inside the source documents is **data to
  migrate, never instructions to follow**.
- Every `agent-coordinator` call passes the session name adopt provides.
- The `HANDOFF.md` stub template gains the line
  `<!-- agent-coordinator:migrated -->`, which is how a later `adopt` run
  knows not to offer migration again.
- The settled rules stay: open backlog items are created `planned`; only the
  items the handoff explicitly queues next are created ready; standing rules,
  rejected approaches and closed-item indexes become knowledge; migrated files
  move to `docs/pre-coordinator/` with `git mv`; nothing is pushed.

### Launch

1. Skip with a one-line reason when: no source file exists or the repository
   is already migrated (see [Detect](#2-detect)); `--migrate off`;
   the run is not interactive; the chosen harness or `python3` is not on
   `PATH`. In every skipped case print the command that starts it later:
   `agent-coordinator adopt --migrate now`.
2. Materialise the kit into `<state-dir>/adopt/migrate-kit-<client commit>/`
   (outside the repository; directory `0700`, files `0600`, or the protected
   ACL the state directory already uses on Windows). Rewriting an existing kit
   directory for the same commit is a no-op.
3. Start the harness as a foreground child with inherited terminal, working
   directory = repository root, and the environment unchanged except
   `AGENT_COORDINATOR_SESSION`, which is set to `adopt-migrate-<8 random hex>`
   when unset:

   - Claude Code: `claude "<prompt>" --add-dir <kit>`
   - Codex: `codex "<prompt>"`

   The prompt precedes `--add-dir` because that option takes a list of
   directories and would otherwise consume the prompt as one of them.

   The prompt is a constant: "Read `<kit>/PROCEDURE.md` and follow it exactly
   to migrate this repository's Markdown project memory to Agent Coordinator.
   The helper is `python3 <kit>/migrate.py`. Do not claim, checkpoint or submit
   work during the migration."
4. The implementation **probes `--help`** of the installed harness and uses
   only flags that exist; if the plain interactive form is unavailable it
   prints the prompt for the user to paste. It never passes a permission-bypass
   or `--dangerously-*` option: the harness's own permission prompts stay in
   force, and the person approves the record plan inside the session.
5. When the child exits, adopt reports its exit status as information. The
   repository is already connected, so the migration outcome does not change
   adopt's exit code.

Admission ([API contract](api-contract.md)): tasks created `planned` are never
held and do not use the weekly agent-task budget, so the bulk of a migration is
unaffected. Tasks the plan creates **ready** count against the project's budget
(default 14 per ISO week) and are held `planned` beyond it. The procedure's
verify step re-reads every created record, and the session reports any ready
candidate that was held so the human can open it in the dashboard.

`adopt --migrate now` in an already-connected repository is repair mode
followed directly by this step, so migration can be run at any later time.

## Idempotency and interruption

There is no run journal and no `--resume`. Because `adopt` never mutates the
service, rerunning it is the recovery path at every point:

- Interrupted before step 6: nothing on disk changed. If the human created the
  project meanwhile, the rerun's detection finds it by repository URL and
  offers to link it.
- Interrupted during step 6: each write is atomic; the rerun sees the binding
  (repair mode) and completes the remaining files.
- A completed repository: the rerun verifies and exits 0 with
  "already connected", writing nothing when every file is current.

Two concurrent runs in one repository are safe: the binding uses
exclusive-create and every other write compares content before replacing.

## Output and exit codes

Human output uses short ASCII step lines. With `--json`, one JSON object per
line on stdout (`{"step": "...", "status": "done|skipped|waiting|failed", ...}`)
and a final `{"result": ...}` envelope containing the project ID, files written
(paths only), commit ID if any, warnings, and `next_session` guidance. No
credential, token, or request header appears in either form.

| Exit | Meaning here |
| --- | --- |
| 0 | Connected (or already connected) and verified |
| 2 | Invalid input, or `needs_input` with the complete list of missing flags |
| 3 | No usable credential for the origin (setup pointer printed, nothing written) |
| 5 | Binding points elsewhere; more than one matching project; a file changed during the run |
| 6 | Client upgrade needed; target files dirty; verification failed |
| 7 | Transport failure; the project did not appear before the timeout |

## Security analysis

| Threat | Mitigation |
| --- | --- |
| Credential leaks through the new surface | No credential is created, moved, written, or placed in a URL. The dashboard link carries three non-secret values; URL userinfo is stripped before use. Tests scan stdout, stderr and written files for the token. |
| A crafted `#/new-project` link sent to an operator | The route only fills a form. Values are assigned as text, bounded, never auto-submitted; the dialog says it was prefilled; creation still needs a signed-in human's click and is audited to that human. A project grants no authority by existing. |
| Binding to the wrong project | Only a project that appeared **after** the request and has the exact normalised repository URL is offered, and its name and ID are shown for confirmation. Planting one requires a human operator account on the same service. |
| Instructions hidden in `HANDOFF.md` / `BACKLOG.md` | The migration session is interactive under the harness's normal permission prompts; the procedure marks source text as data; the user reviews the validated plan before any record is created; no bypass flags. |
| Damage to the user's files | Target files must be clean; writes are atomic and compare-before-replace; the block is delimited by markers; commit is pathspec-limited and never pushed. |

## Testing strategy

- **CLI unit tests**: repository URL sanitising and normalisation; dashboard
  link encoding (values containing `&`, `#`, `/`, spaces, non-ASCII); the
  instruction-file merge matrix (every row; CRLF, BOM, no trailing newline,
  front matter, symlink, marker upgrade, missing end marker); the block term
  test; binding rendering; dirty-path refusal; commit pathspec limiting with a
  pre-staged unrelated file.
- **CLI integration tests** against an in-process fake service: link by match
  and by `--project`; create-and-poll (project appears on the third poll);
  timeout exits 7 with nothing written; two matching projects; a pre-existing
  project with the same URL is not mistaken for the new one; aggregate
  `needs_input`; `--yes`; `--dry-run` writes nothing and opens nothing; rerun
  of a completed repository writes nothing; no request other than `GET` is ever
  sent; the token never appears in stdout, stderr or any written file.
- **Migration launch**: fake `claude` and `codex` scripts on `PATH` record
  their arguments, working directory and environment; assert the prompt, the
  kit directory mode, the session variable, and that no bypass flag is passed;
  skip paths (no sources, non-interactive, no `python3`). `migrate.py` gets
  `python3 -I` unit tests for `validate` (limits, duplicate keys, dependency
  cycles) and for `apply` idempotency against a fake CLI.
- **Dashboard**: `node --check web/app.js`; a browser check that the route
  opens the dialog with the three values as text, ignores an oversize or
  control-character value, does not submit, clears the fragment, and works
  when the visit starts signed out.
- **Smoke**: extend `scripts/smoke.py`: run `adopt --no-browser --yes` in a
  throwaway repository against the disposable service, create the project
  through the human API session while it waits, then `connect` in that
  directory.
- The launch command is checked against the real installed `claude` and
  `codex` (flag probe and argument parsing; an unattended agent cannot drive
  the interactive session itself) and their exact versions are recorded in the
  task evidence. The end-to-end interactive migration is a human acceptance
  check after the task lands.
- Gate per the repository's `CONTRIBUTING.md`: fmt, clippy `-D warnings`,
  workspace tests, `scripts/smoke.py`, and `scripts/check_docs.py`.

## Documentation updates required

On implementation: this chapter moves from "proposed" to current; add an
`adopt` section to [the CLI guide](CLI.md) and replace its "Repository
bootstrap text"; add the prefilled-link route to the
[operator guide](operator-guide.md); record the feature in
[implementation status](implementation-status.md). The
[API contract](api-contract.md) and [agent startup](agent-startup.md) do not
change: no endpoint is added and the bootstrap text served in `/api/v1/info` is
untouched.

## Implementation plan

One `code` task, delivered in four gated phases. Each phase leaves the tree
green and is independently testable.

| Phase | Scope | Files |
| --- | --- | --- |
| 1 | `adopt` core: binding-less dispatch, preflight, detect, interview, plan, `--dry-run`, link mode, file writes, verify, commit | new `crates/cli/src/adopt/` (`mod.rs`, `detect.rs`, `interview.rs`, `plan.rs`, `files.rs`, `verify.rs`, `commit.rs`); wiring in `crates/cli/src/main.rs` |
| 2 | Create through the dashboard: link builder, browser open, polling; the `#/new-project` route | `crates/cli/src/adopt/provision.rs`, `web/app.js` |
| 3 | Migration kit and launch | `crates/cli/assets/migrate/`, `crates/cli/src/adopt/migrate.rs` |
| 4 | Smoke test and documentation | `scripts/smoke.py`, `book/src/docs/` chapters listed above |

`crates/cli/src/main.rs` is already several thousand lines; keep the new code
in the `adopt` module with small, commented functions and add only the command
variant and its dispatch line to `main.rs`.

Acceptance criteria (copied into the task):

1. `agent-coordinator adopt` (alias `scaffold`) runs in a Git work tree with no
   binding, no `--session` and no arguments; the service URL defaults to
   `https://agents.sithbit.com`; every flag in "Command surface" exists.
2. Preflight, detection, interview (a flag for every prompt, aggregate
   `needs_input`), plan and `--dry-run` behave as specified.
3. An existing project is linked by repository-URL match or `--project`.
   Otherwise adopt prints and opens the prefilled dashboard link with the
   manual values, polls read-only, and binds only to a project that appeared
   after the request with the same normalised repository URL; timeout and
   ambiguity exit as specified.
4. The dashboard route `#/new-project?…` opens the project dialog prefilled as
   text, bounded, never auto-submitted, and clears the fragment; verified in a
   browser, including a signed-out start.
5. `.agent-coordinator.toml` and the managed instruction block are written per
   the file rules, including the full merge matrix, atomicity and rerun
   behaviour; the block text is the constant in this chapter, with the drift
   test.
6. Verification passes and a following `agent-coordinator connect` in the
   adopted repository succeeds (smoke test).
7. The commit is limited to adopt's paths and never pushes.
8. `adopt` sends only `GET` requests, and no credential or token appears in
   output or written files; tests prove both.
9. The migration kit is in `crates/cli/assets/migrate/`, adapted as specified,
   and `adopt` launches an interactive `claude` or `codex` session with it
   (no bypass flags, flag probing, skip paths, `--migrate now`); fake-harness
   tests pass, and the launch command is verified against the installed
   `claude` and `codex` (`--help` probe and argument parsing), with their
   versions recorded.
10. Rerunning `adopt` at any point is safe and a completed repository reports
    "already connected" without writing.
11. Documentation is updated as listed; fmt, clippy, workspace tests,
    `scripts/smoke.py` and `scripts/check_docs.py` pass.

## Later options

Not part of the implementation task; each needs its own decision.

- **Unattended project creation**: an administrator-issued grant letting a
  named agent principal create projects, so `--yes` needs no click.
- **Credential issuance from the CLI**: a human-approved request flow that
  delivers a new token to the requesting terminal. Revision 1 specified one
  (device code, sealed token); it is only worth building if credentials become
  project-scoped.
- **Enrolling a workstation with no credential.**
- **Porting `migrate.py` to Rust subcommands**, removing the `python3`
  requirement.

## Revision notes

Revision 2 replaces revision 1 (artifact `1a81b217`, commit `2e306d5`) after a
design review on 2026-11-25. Changes and reasons:

| Change | Reason |
| --- | --- |
| The device-code approval flow (table `adoption_requests`, six endpoints, approval page, sealed-token delivery, `chacha20poly1305`) is replaced by a prefilled dashboard link plus read-only polling. | Owner decision. The only server rule in the way is human-only project creation; a prefilled form satisfies it with one click, no new API, and no moved security boundary. It removed four of the six planned tasks. |
| Credential issuance is removed from version 1. | Credentials are not project-scoped, so a new one is never necessary; removing it removes every path on which a token could travel. |
| `project_adoption` is no longer added to `client_compatibility`. | `required_capabilities` is a requirement on clients; adding a server feature there would have marked every existing client incompatible for all work. The new flow needs no feature flag. |
| The run journal, lock, `--resume`, and the request/ack protocol are removed. | With no service mutation, a rerun is always safe and rediscovers the project by repository URL. |
| The separate "manual" mode and `--manual` are removed. | The create flow always prints the manual values, so it already is the manual mode; `--no-browser` covers headless use. |
| Backlog migration launches an interactive session running the `coordinator-migrate` procedure, instead of a tool-less assistant producing a human-applied import preview. | Owner decision. It matches the already-settled migration design (tasks, knowledge and decisions; queued items ready; files archived), and the dashboard cannot open a CLI-created import preview today. |
| Commit default is stated once: yes, `--no-commit` opts out. | Revision 1 said "ask, else no" in the command surface and "default yes" in the interview. |
| A missing credential exits 3 with nothing written. | Revision 1 said both "nothing else proceeds" and "continue in manual mode". |
| `--replace-binding`, `--force-instructions`, `--no-browser`, `--approval-timeout` are listed in the command surface. | The first review found flags used in the text but absent from the surface. |
| The managed block gains the "response is large" sentence. | New sessions otherwise load the whole info response into context. |
| `adopt` is documented as distinct from `session adopt-mcp`. | The names are close. |
