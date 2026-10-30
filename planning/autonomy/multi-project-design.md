# Supervising and integrating projects other than the coordinator — design

Status: **DESIGN, awaiting owner sign-off** (coordinator task `cbb05ecb`, owner decision U36).
Written 2026-10-12 against `main` at `24a7f3d`. **Design only: no runtime change is made or implied
until the owner answers §11 and files the §12 tasks.** Baseline claims cite `file:line` on that
revision; every "today" statement was read from the code, not from the plans. Where this document and
`plan-final.md` / `p4-design.md` differ on mechanics, this document wins for multi-project questions;
plan-final still wins on intent.

Conventions: **MP-n** is a proposed implementation task (§12). **D-n** is a user-owned decision
(§11). "Host owner" is whoever has root on a supervisor host; "project owner" is whoever owns the
repository and its GitHub settings. Today both are the same person; the design keeps the roles
separate because the trust boundaries differ.

## 0. Corrections to the premises of the task, found while reading the code

These change the design, so they come first.

1. **The weekly admission budget is per project, not global.** The task text says "admission budget
   is already global (U34)". `crates/server/src/admission.rs:235` (`admitted_in_week`) counts
   `tasks WHERE project_id=?`; `book/src/docs/implementation-status.md` states "the budget is per
   project, so one project's tasks never use another's", and `crates/server/tests/attention.rs` has a
   test of two projects each admitting five with a sixth held in only one. (The comment in
   `migrations/0033_admission_control.sql` still says "across all projects"; it is stale.) What *is*
   global is the **spend**: the daily USD caps in `[health]` sum the ledger over all projects
   (`crates/supervisor/src/run_loop/cost.rs:337`, `health.rs:195`). See D-14.
2. **Quiet hours are implemented, at host level.** `deploy/agentc/quiet-hours.sh` (cron, every
   minute) holds the supervisor's kill switch outside a local-time window; `host-setup.sh` takes
   `QUIET_HOURS`; mxmini is configured with it as of 2026-10-12 (its first window opens 22:00 EDT). The server separately has a single
   **UTC** `QuietHours` window that only excludes hours from the stall clock
   (`crates/server/src/attention.rs:70`). What does not exist is anything per project or per host on
   the server side (§10).
3. **The integrator already serves several projects and several repositories in one process.**
   `Config::projects` is a list (`crates/integrator/src/config.rs:34`), `run_loop` visits each
   project every pass (`main.rs:176`), mirrors are keyed by a hash of the repository URL
   (`git.rs:75`), and tips/freezes are keyed by `url#branch` (`state.rs:21`). What it lacks is
   per-project configuration, per-project time and failure budgets, more than one GitHub
   installation, and any non-GitHub checks source (§5).
4. **Coordinator credentials are not project-scoped.** "All authenticated callers can still
   deliberately access every project" (`book/src/docs/implementation-spec.md:73`,
   `coordination-contract.md:30`). `CredentialAttributes` has a class and an access level only
   (`crates/server/src/credential_attributes.rs:38`); the integrator credential works on any project
   whose `integration_owner` is `integrator` (`crates/server/src/integrator.rs:99`). Per-project
   credentials therefore give attribution and revocation, not isolation, until the server grows a
   scope (§4).
5. **The coordinator already hosts five projects, three of them on Azure DevOps.** (Read from
   `GET /api/v1/projects` on production, 2026-10-12.) The integrator, the ruleset watchdog and the
   candidate-push helper are GitHub-only: `RepoId::from_url` accepts only `https://github.com/…`
   (`crates/integrator/src/github.rs:32`) and `GithubChecks` fails on anything else
   (`checks.rs:76`). Those three projects stay on the agent integration path unless D-10 says
   otherwise.
6. **Several single-project assumptions are in the supervisor, not only in configuration** (§1, §3):
   one global mirror branch `[run] branch = "main"` for every project (`run_loop.rs:67`,
   `run_loop/live.rs:293`) while the second real project targets `master`; one fixed instruction-file
   list `AGENTS.md, CONTRIBUTING.md` (`run_loop.rs:47`); a Rust environment built into every launch
   (`profile.rs:227-243`); a host-wide strike counter that stops claiming everything
   (`unsubmitted.rs:90`); and a loop that runs one launch at a time (`run_loop.rs:450`).
7. **`agent-coordinator bootstrap github` and `host init`, named in the plan transcript, do not
   exist.** Nothing in `crates/cli` matches. The ruleset payloads live only in
   `book/src/docs/integrator-cutover.md` and `deploy/agentc/integrator-*.{sh,py}`.
8. **The planning documents referenced by the task are not on `main`.** `planning/autonomy/` lives
   on the `autonomy-plan` branch; this file adds the first file under that path to the task branch.
   Moving the rest is the owner's call (U15 redaction pass applies: the repository is public).

## 1. Supervisor topology

### 1.1 Today

One `agentc-supervisor run` process per host (systemd `agentc-run`, or the sysvinit wrapper).
`iterate` (`run_loop.rs:334`) does recovery, admission, reviews, then `next` per project, and the
first project with a claimable task wins; the launch then **blocks the loop** until it is terminal
(`work`, `run_loop.rs:450`). Projects come from two places:

- `[run.binding]` (or the mirror branch's `.agent-coordinator.toml`) for the main project;
- at most one `[run.canary_binding]` (`run_loop/binding.rs:89`), which must name the same
  coordinator and a different project (`check_against`, `binding.rs:118`) and is polled **first**.

`LiveDriver` holds one `Slot` per project (`live.rs:56`): binding, mirror
(`<state_dir>/mirror.git`, canary `mirror-canary.git`), optional reviewer side. Everything else is
host-wide under `state_dir`: `heartbeat.json`, `kill-switch`, `vendors.json` (429 exhaustion),
`costs.jsonl` (rows carry `project`), `unsubmitted.json` (strikes), launch records, installed binding
copies, one implementer credential file read for all slots (`live.rs:626`). Already keyed by
project id: `[setup.<id>]` (`setup.rs:29`), `[verification.<id>]` (`verification.rs:22`),
`[push_helper.project_configs]` (`config.rs:77`), the launch record's `project`, and the ledger's
`project`.

### 1.2 Options

| | A. One supervisor per host, N project slots | B. One supervisor process per project per host | C. A *tenant instance*: a full second supervisor with its own uids and state |
|---|---|---|---|
| Host-wide arbiters (spend caps, vendor 429 state, disk high-water, kill switch, concurrency) | one place, already correct | duplicated; caps become per project and the host cap is lost unless a shared file is added; two loops can launch simultaneously | separate by construction |
| Fairness across projects | one scheduler (§2) | none without an external arbiter | none |
| Failure isolation | a bug in one project's handling is in the shared loop; per-project strikes and timeouts needed (§2.4) | process-level | process- and uid-level |
| Config | one file with a list; fragments in `/etc/agentc/supervisor.d/` | one file per project; unit template | one config per tenant |
| Fit with code | `Slot`/`Driver::projects()/select()` already abstract it (`run_loop.rs:184-190`) | needs per-project `state_dir`, heartbeat path, unit names | already supported by `Config` (`state_dir`, `implementer_user`, `reviewer_user`) |
| Strongest cross-project isolation | no (shared `agentc-impl` uid) | no (same uids) | yes |

**Recommendation (D-1, D-2): A as the default, C as the escape hatch.** B is rejected: it copies
the loop's complexity per project and loses the only component that can arbitrate host-wide spend
and concurrency. C is how a project with a different trust domain (for example an employer
repository) would be onboarded: its own accounts, its own egress listener and firewall uid rule, its
own credential, its own state. A and C use the same binary and config schema; C is "run another
instance", so no extra code is needed beyond making host-setup parameterized by instance name (MP-7).

### 1.3 Proposed schema (A)

Replace the single/canary binding pair with a list, keeping both old forms working for one release
(they are read into the list; a test proves an existing `supervisor.toml` yields an identical
effective configuration):

```toml
# /etc/agentc/supervisor.toml — host-owner file; the repository cannot add a project.
[[projects]]
project_id = "<coordinator project id>"   # the only required key
# Everything below defaults in code from the coordinator's own project record
# (GET /api/v1/projects: repository_url, target_branch) or from the host layout:
# slug         = "<derived from the project name; a plain directory name>"
# service_url  = <the first project's origin; all slots share one coordinator>
# project_name = <credential directory selector, as today>
# mirror       = "<state_dir>/projects/<slug>/mirror.git"
# branch       = <project target_branch>             # fixes run.branch for non-"main" targets
# instructions = ["AGENTS.md", "CONTRIBUTING.md"]   # read from the mirror, 32 KiB each
# toolchains   = []                                  # §3; the coordinator host sets ["rust"]
# gate         = []                                  # command `project verify` runs under the implementer profile; never read by launches
# egress       = "default"                           # §3.3
# weight       = 1                                   # §2
# daily_usd    = 0                                   # 0 = host cap only
# role         = "main" | "canary"                   # canary: urgent class, ledger rows marked
```

Per-project state moves under `<state_dir>/projects/<slug>/` (mirror, strikes, pause file); host-wide
state stays where it is. The heartbeat gains a `projects` map (last poll time and outcome per slot)
so the attention canary can page a project whose `next` keeps failing while the host heartbeat stays
fresh. The canary role keeps today's semantics (polled first, ledger rows marked `canary`, excluded
from the digest) but becomes a flag on a slot, so a host can have several or none.

Mirrors: the host owner creates the mirror at onboarding (`project add`, §6); the supervisor keeps
refreshing it before every launch exactly as today (`live.rs:291`). The containment suite tests the
mirror HEAD, so it must run per project (§3.5).

Heartbeat and the `agentc-canary` timer: `attention.py` reads one `heartbeat.json` and one
`ATTENTION_PROJECT` (default is the coordinator's id, `attention.py:68`); §7 generalizes both.

## 2. Fair claiming, concurrency and cost caps

### 2.1 Today

`next` is per project and offers work ordered by `(priority, ready_since, id)`
(`crates/server/src/coordination.rs:1596`); lower numbers are more urgent (the end-to-end canary uses
0). The `claim_task` action already carries `priority` and `task_kind` (`next.rs:192`) but not
`ready_since` or `admission_class`, and the supervisor's `suggestion()` drops what it has
(`run_loop.rs`, `suggestion`). The scan order is *canary projects, then main*, and the first non-empty project wins
(`run_loop.rs:361`). With N projects the first-listed project with ready work always wins:
**starvation of later projects**. Reviews are polled in every project before any implementation
(`run_loop.rs:354-357`), which is right (finishing work first) but lets one project's review backlog
spend the whole `reviewer_daily_usd`. Strikes are host-wide: `empty_exit_total_limit` (5) stops
claiming **anything** after five empty exits in a row, whatever the project (`unsubmitted.rs:90`) —
a project whose setup is broken would stop every other project on the host.

### 2.2 Policy options

1. **Strict order** (today): config order is precedence. Simple; starves.
2. **Priority merge:** take the minimum `priority` across projects. Cross-project priorities are not
   comparable (one project's P2 is another's P0), and the owner of a quiet project loses to a noisy
   one.
3. **Weighted fair share with an urgent class (recommended, D-4).** Among projects with a claimable
   task, claim from the one furthest below its share of trailing-24-hour spend, where share is
   `weight / Σ weights` of projects *with ready work*; ties by oldest `ready_since` (two additive fields on the claim action, `ready_since` and
   `admission_class`, are the only server change). Tasks of the
   urgent class bypass the rotation: priority 0, canary-role slots, and admission classes `revert`,
   `fix_target`, `deflake`, `refusal_fix`. Spend is read from the existing ledger (rows already have
   `project`), so heterogeneous task sizes balance without a new server concept. A project with
   ready work unclaimed for the stall threshold (6 h, the digest's `no_progress`) is promoted once,
   so a bad estimate cannot starve it.

Reviews use the same share function for their own spend, so a review backlog in one project cannot
exhaust the reviewer cap for the others.

### 2.3 Caps

- **Host:** keep `[health] implementer_daily_usd` (150) and `reviewer_daily_usd` (50) as host-wide
  ceilings (`health.rs:195`); they remain the only thing that bounds the sum.
- **Project:** new `daily_usd` per slot, enforced by the same `capped` check on the project's own
  ledger rows. Proposed starting values for a new project are an owner decision (D-5); observed
  costs in the dogfood are $1–7 per task (largest $6.70), so a project cap of **$25/day** with the
  host cap unchanged is a conservative first value.
- **Attempt duration:** `budget_minutes` (240) becomes overridable per slot, because a heavy gate
  (§13.1) needs a different budget than the coordinator's.
- **Agent-originated task admission** stays per project and server-side (5/week default when this was written; the owner raised it to 14/week on 2026-10-30, 2 per day). D-14 asks
  whether to keep that; recommendation is yes, plus a per-project override on the project record only
  if a project demonstrates it needs one.

### 2.4 Failure isolation inside the shared loop

- Strikes (`unsubmitted.json`) become per project (`projects/<slug>/unsubmitted.json`) with the
  per-task limit unchanged and the "stop everything" limit applied per project; a separate, larger
  host-wide limit remains as the last resort.
- A per-project **pause file** (`projects/<slug>/pause`) beside the host kill switch, so the owner
  can stop one project without draining the others; `quiet-hours.sh` is unaffected.
- A `next` error for one project is already isolated (`run_loop.rs:366`, collected as `failure`);
  keep that, and add a bounded per-project backoff so a down repository does not add a request per
  poll per project forever.
- Launch-time failures that are project-specific (setup timeout, mirror fetch, push-helper config
  missing) count as that project's strikes only.

### 2.5 Concurrency

Today: one launch at a time per host. For the first non-coordinator project that is acceptable and
**recommended (D-6)**: it keeps cost and host-load reasoning trivial, and fair share (§2.2) makes the
single slot a scheduling rather than a capacity question. Raising it is a refactor of the `Driver`
trait (`child`, `revision` and `active` are singletons in `LiveDriver`, `live.rs:42-52`, and `Driver::exited` has no launch argument) and
of the heartbeat, so it is deferred to MP-12 and gated on measurement: start it only if the median
time a ready task waits for a launch slot exceeds the median launch time over two weeks. When built:
host `max_concurrent_launches` (default 1) and per-project `max_concurrent` (default 1), disk
high-water checked per launch, reviewer launches counted.

## 3. Per-project environment

### 3.1 What is baked in today

| Thing | Where | Rust/coordinator specific? |
|---|---|---|
| `PATH` with `<toolchain_dir>/cargo/bin`, `RUSTUP_HOME`, `CARGO_HOME`, `CARGO_TARGET_DIR` | `profile.rs:227-243` | yes, in every launch |
| Required read-only Cargo config seed, checked by preflight | `config.rs` (`cargo_config_seed`), `confine.rs:385` | yes |
| Default egress list incl. `crates.io`, `index.crates.io`, `static.crates.io` | `egress.rs:13` | partly |
| Role contracts quote `cargo fmt/clippy/test` as the example gate | `contracts/implementer.md:53`, `reviewer.md:25` | example only, but it is the only example |
| Instruction files `AGENTS.md`, `CONTRIBUTING.md` only; a larger file is **truncated** to 32 KiB | `run_loop.rs:47,:53`, `live.rs:257` | generic but fixed |
| One mirror branch for all projects | `run_loop.rs:67`, `live.rs:293` | `main` assumed |
| Containment suite exercises one mirror and "the project's tests" | `deploy/agentc/containment-suite.sh:755-762` | one project |
| `host-setup.sh` defaults `REPO_URL`, push App ids, `EXTRA_EGRESS=agents.sithbit.com` | `host-setup.sh` header | self-hosting defaults |
| Already per project | `[setup.<id>]` command + cache paths + timeout (Claude only); `[verification.<id>]` URL + browser; `[push_helper.project_configs]` | no |

### 3.2 Toolchain profiles

Toolchains are **not** baked into the supervisor; they are root-owned, read-only prefixes the host
owner installs (as `host-setup.sh` already does for Rust and the headless browser, with pinned
versions and checksums), described to the supervisor by a small profile:

```toml
# /etc/agentc/toolchains/<name>.toml (root-owned) or built in for "rust"
[toolchain.node]
path       = ["/opt/agentc/toolchains/node/bin"]
env        = { npm_config_cache = "{home}/.npm", npm_config_userconfig = "/etc/agentc/npmrc-seed" }
seed_files = [{ from = "/etc/agentc/npmrc-seed", to = "{home}/.npmrc" }]   # read-only baseline
egress     = ["registry.npmjs.org"]
```

- `{run}`, `{home}`, `{clone}` placeholders resolve to the launch's private paths, so a profile cannot
  point outside the run.
- A project lists `toolchains = ["rust", "node"]`; the union of `path`, `env`, `seed_files` and
  `egress` is applied; a duplicate `env` key with a different value is a load error.
- The current Rust environment is extracted **unchanged** into the built-in `rust` profile;
  a test asserts byte-identical `environment()` output for the coordinator's config (MP-3). A host
  that sets no `toolchains` for a project gets none — a Python-only project is not handed Cargo.
- `setup` (command, `cache_paths`, timeout) stays as it is, host-owned, Claude-only, run inside the
  same sandbox and network namespace as the harness (`setup.rs`); it now sees the profile's
  environment and the project's egress listener. `cache_paths` remain per launch-account directories
  the host owner creates; two projects must not share a cache directory (load error), so a
  dependency cache cannot be poisoned across projects.
- Role contracts replace "for a Cargo workspace: …" with `{{gate_hint}}`, filled from the project's
  optional `gate_hint` string (default: empty, "as the repository instructions describe").

### 3.3 Egress

The allowlist is owned by the host owner (plan-final §2.3) and must be **per project**: a Rust crate
registry for one project must not widen another's. Today there is one proxy and one allowlist
(`egress.rs:59`, started by `agentc-supervisor egress-proxy`), and every Claude launch reaches it
through a per-launch Unix-socket relay into a private network namespace (`relay.rs:26`,
`sandbox.rs:32` `--unshare-net`). That relay is the natural enforcement point:

- `[egress.listeners.<name>] listen = "127.0.0.1:31xx"; allow = [...]`, with `default` = today's
  list. The proxy process serves every listener.
- A slot selects `egress = "<name>"`; its launches' relay endpoint targets that listener
  (`Endpoint.address`). The effective allowlist is `base ∪ toolchain egress ∪ listener allow`.
- **Enforcement is real for Claude launches** — the launch cannot reach any other loopback port, so
  it cannot use another project's listener. **For Codex launches it is advisory**: Codex shares the
  uid and has no private namespace; the firewall filters by uid, not by project. Multi-project hosts
  therefore run Claude for projects that need a narrow list, and a Codex launch gets only the
  default listener (D-8).
- `preflight` (`preflight.rs:45-60`) probes each listener a project uses: allowed host reachable,
  blocked probe host refused.
- The firewall's uid rule is unchanged for tier A; a tier-C tenant instance gets its own uid rule and
  listener range.

### 3.4 Verification environment

`[verification.<id>]` is already per project and reviewer-only, with the test login a host-owned
secret the supervisor exchanges for a session (`staging_login.rs`, decision U22). That login flow is
specific to the coordinator's own console. A project without UI acceptance criteria needs nothing;
a project that does would need its own login kind, which is out of scope until a project asks. The
schema stays as is; the design's only requirement is that `verification` and `setup` keys be accepted
inline in a `[[projects]]` entry as a convenience (the id-keyed tables remain valid).

### 3.5 Containment suite per project

`containment-suite.sh` proves host properties (uids, firewall, candidate instructions never load,
planted hooks not run, raw push denied, writes outside the run denied, egress) and, optionally, that
the project's own tests pass under the implementer profile. For N projects it is split:

- **host section:** once per host, unchanged;
- **project section** `containment-suite.sh --project <slug>`: clone from that project's mirror (planted
  hook case included); `setup` runs in the sandbox with only its caches writable; the project's
  listener allows its hosts and refuses another project's exclusive host; the push helper config for
  the project is protected and names only that repository; the owner-declared `gate` command (§1.3) passes
  under the implementer profile with `AGENTC_TEST_NESTED_SANDBOX=1`.

A project section is a **precondition of `project add --enable`** and re-run by `agentc-update`'s
promotion step (as the host section is today) and weekly.

## 4. Credentials and grants

### 4.1 Coordinator credentials

| Level | What | Gives | Cost |
|---|---|---|---|
| L0 (today, incl. canary) | one implementer principal + one reviewer principal for every slot | nothing per project | none |
| **L1 (recommended now, D-3)** | one principal pair **per project** (`agentc-impl-<slug>`, `agentc-rev-<slug>`), stored per slot, chosen by the existing `project_name` credential directory (`binding.rs:73`, `live.rs:573`) | per-project attribution in events and the digest, per-project revocation and rotation, per-project success metric ("autonomous" = supervised credential, plan §0) | supervisor reads a credential per slot instead of once (`live.rs:626`) |
| **L2 (required before a different trust domain)** | server-side scope: a credential of class `supervised` or `integrator` carries a project list; every route that resolves a project (path `{project}` or by task/attempt/submission id) refuses others with a labelled 403 | real isolation: a launch for project B cannot claim, read or comment in project A, and cannot use `share_across_projects` knowledge across the scope | a server migration plus an enforcement point per route family; size it with a one-day spike before committing (MP-6) |
| L3 | separate uids/host (tier C) | OS-level separation, separate harness logins and billing | operations |

Until L2 exists the supervisor must not hand a launch more than the coordinator already hands every
principal, and **supervised credentials must not be used to create `share_across_projects`
knowledge** (a lesson written while working on a private repository must not surface in the public
coordinator project's context). The enforcement for that is L2; until then it is an instruction in the
role contract and a digest check (D-17).

Harness logins (Claude token, Codex login) stay per host and per uid, so vendor billing is shared;
per-project spend is read from the ledger, not the vendor.

### 4.2 GitHub: candidate push

The helper (`agentc-push`, own uid `agentc-push`, the only reader of the push App key) is configured
for exactly one repository by `push.toml` (`crates/integrator/src/push/config.rs:22`), and the
supervisor selects the file per project (`PushHelper::config_for`, `config.rs:84`). Its installation
tokens are narrowed to **one repository and `contents: write`** (`push/serve.rs:66`).

For a second repository owned by the same GitHub account the existing App is installed on the new
repository (the installation id does not change) and a second `push-<slug>.toml` differs only in
`repository`. **Recommended (D-9):** one push App and one installation per owner, a config per
project, token scoping doing the per-repo least privilege. One App per repository is available if the
project owner wants key-level isolation, at the cost of a manual GitHub App registration per repo.
A repository under a different account or organization needs its own installation, and a private App
cannot be installed there, so that case is tier C.

Candidate refs live under `refs/agent-coordinator/candidates/` per repository and cannot collide.

### 4.3 GitHub: integrator authority and rulesets

Per repository the project owner (not the host, not an agent) must have in place — the plan's
constitution:

- ruleset A on the target branch and `ac/results/**`: `update`, `creation`, `deletion`, bypass = the
  integrator App (plus owner for emergencies);
- ruleset B: `non_fast_forward` + `required_status_checks` pinned to GitHub Actions
  (`integration_id` 15368), **no bypass**; and a tag ruleset where tags matter;
- the integrator App installed on the repository with *Contents rw, Actions rw, Checks r,
  Administration r, Metadata r* (it deliberately cannot edit rulesets);
- `.agent-coordinator/roster.toml` on the target mapping every service-floor check identity to a
  workflow job (`roster.rs`), and workflows that provide those jobs without secrets.

`agent-coordinator bootstrap github` (MP-8) automates the human-run part: with the **owner's** GitHub
credentials on the owner's machine (never on a host) it prints/applies the ruleset payloads, reads the
branch rules back (the same read the watchdog uses, `GET /repos/{o}/{r}/rules/branches/{b}`),
verifies the App installation covers the repository, proposes `roster.toml` from the workflows (default:
every job of every push-triggered workflow) as a candidate commit, and runs the U4-style negative test
(an ordinary push to the protected branch is rejected). It is dry-run by default and idempotent.

The integrator side is §5. One point belongs here: **Git credentials for `git push` use
`GIT_ASKPASS`, whose prompt names a host, not a repository** (`askpass.rs:50`), so a single process
can serve only repositories covered by one installation. This is why a second GitHub owner is a
second integrator instance (§5.3), not a configuration entry.

## 5. The integrator across projects and repositories

### 5.1 Today

`Integrator::cycle(project)` (`integrate.rs:90`): fetch the project's queue → watch **every target
in the queue** (ruleset watchdog, then tip monitor; failures hold only that target) → work reverts
first, then queue items → return after one action. `run_loop` calls it for each configured project in
turn and sleeps `poll_seconds` (`main.rs:176`): a round robin of one action per project per pass, so
unlike the supervisor it does not starve. Isolation that exists: per-target freeze and watch failure
(`watch.rs`), `ServiceFailure` aborts only that project's cycle, blocked/refused/failed items are
skipped so they cannot stall the rest, `integrator_last_seen` is recorded per project by the queue
poll, one `state.json` keyed by `url#branch`. Deployment today: one unit per configuration, and the
canary project got a second, whole instance (`integrator-host-setup.sh`) with its own state, lock and
config.

What is single-project or single-repo: one `[github]` App/installation for all repositories;
`required_rules`, `roster_path`, `result_branch_prefix`, `checks` kind are global; no wall-clock limit
on a project's cycle other than HTTP's 30 s (`github.rs:56`) — a hung `git fetch` of one repository
delays every project; no failure backoff or surfaced alarm for a project that errors every pass.

### 5.2 Proposal

Keep **one integrator process per GitHub installation** and let it serve many projects:

- Per-project tables: `[[project]] id, required_rules, roster_path, result_branch_prefix` (defaults =
  today's globals), so a repository can differ without a second instance.
- A **wall-clock budget per project per pass** (default 120 s; cancel-safe because all durable state is
  in the service or rebuilt from the pinned result — `integrate.rs` module comment), and consecutive-
  failure **backoff** (30 s → 10 min). After N consecutive failed passes the integrator files a
  `project_cycle_failing` integrator report (existing report/dedupe mechanism,
  `crates/server/src/integrator_reports.rs`), which reaches the digest as an HRI line instead of a
  silent stall.
- **Ordering:** unchanged and independent per repository: reverts first, then service order
  (subject priority, then age). No cross-repository ordering or atomicity is offered; a task in one
  project depending on a landing in another is a coordinator-level dependency, not an integrator
  concern, and is out of scope.
- **One repository ↔ one project.** Two projects pointing at the same repository and branch are
  serialized by integration holds (`implementation-status.md`), which is correct but defeats per-
  project cost accounting; onboarding refuses a repository already bound.
- **Fail fast on a mismatched installation:** at startup and when a new project is added, resolve each
  project's repository owner against the installation's account and refuse to start the project
  (with an actionable message) rather than failing at the first push.
- **Shadow and flip-rate per repository:** the P4 S6 procedure (`integrator shadow`, ~20 reruns of the
  required jobs with flip rate < 2 %, preflight of zero intents/holds/reservations) is run per
  repository before `integration_owner` flips. The flip is per project already
  (`projects.integration_owner`), but the two scripts are not: `integrator-flip-rate.py` hard-codes the
  coordinator's three job names (`CHECKS`), and `integrator-preflight.py` counts held/active state over
  the **whole database** (`current_attempts` is every task with a current attempt, in any project), so
  a second project's activity would block the coordinator's cutover check and vice versa. Both need a
  `--project` filter and the job names from the repository's roster (MP-9).

### 5.3 When to use a second instance

A second instance (as for the canary project) is right when any of these differ: GitHub installation
(other owner/org), App key, service credential/trust domain, or the blast radius the owner wants
(an unstable gate should not share a process with the coordinator's integrator). Units already exist
as templates; host setup is parameterized by instance name (MP-7). A future credential-helper based
askpass with `credential.useHttpPath` would let one process hold several installations; not proposed
now.

### 5.4 Non-GitHub projects

The protocol (pinned R, roll-forward, fast-forward-only target) is remote-agnostic; the checks
source, ruleset watchdog and push authority are not. The three Azure DevOps projects remain on the
agent integration path. Serving them would need an Azure DevOps `ChecksSource` (build validation +
branch policy readback) and an equivalent of the candidate-push helper: a project of its own, not part
of this plan (D-10).

## 6. Onboarding and offboarding

### 6.1 Roles and principle

Everything that widens what an agent can do or reach is done by an owner, outside the repository and
outside any agent: a repository cannot add itself to a host (`/etc/agentc/` is root-owned), cannot
widen its egress list, and cannot grant the integrator anything. Configuration is minimal and
defaulted in code from the coordinator's own project record.

### 6.2 Flow

| # | Step | Who | Tool | Gate |
|---|---|---|---|---|
| 1 | **Eligibility:** GitHub-hosted, one repository per project, repository and branch known to the coordinator, `recovery_mode=agent`, `automatic_integration`, lease and `max_attempt_seconds` compatible with the gate's length (§13.1), required-check floor defined | project owner | console; `onboard check` (MP-7) reads the project record and reports gaps | all green |
| 2 | **Repository readiness:** `AGENTS.md` (supervised launches read it and `CONTRIBUTING.md` only; a repository's `CLAUDE.md`/hooks never auto-load), the roster file, secret-free Actions workflows providing the required jobs, the gate command documented | project owner (a normal PR, or a coordinator task in the target project) | — | merged |
| 3 | **GitHub:** install the push App and the integrator App on the repository; apply rulesets A/B (+ tags) | project owner | `bootstrap github` (MP-8), GitHub UI | read-back shows `non_fast_forward` and required checks |
| 4 | **Credentials:** create the project's implementer/reviewer principals (L1) and, for tier C, the integrator credential; install them root-owned `0600` | host owner + coordinator admin | console; `host-setup.sh` | `agent-coordinator` auth check as that principal |
| 5 | **Host project:** `agentc-supervisor project add --project-id … [--toolchain …]` writes the fragment, creates the mirror and state, the push-helper config from the template, the egress listener if requested; refuses to enable | host owner (sudo) | MP-7 | prints what it created |
| 6 | **Verify (LLM-free):** `agentc-supervisor project verify <slug>`: clone, setup, owner-declared gate under the implementer profile, containment project section, push-helper dry run against a scratch candidate ref, ruleset read-back, `next` as the project's implementer | host owner | MP-7 | all PASS |
| 7 | **Shadow:** enable the slot in shadow mode — `agentc-supervisor shadow` already polls every project (`shadow.rs:161`) and logs would-launches with cost estimates; integrator `shadow` computes R and logs would-push | host owner | existing | one day of logs, estimates in the cap |
| 8 | **First task, owner-admitted:** create or admit one small real task; watch claim → review → integration to `done`; record cost and HRI | project + host owner | console | done, HRI 0 |
| 9 | **Integrator cutover:** per-repository flip-rate run, preflight zero, `integration_owner=integrator` | project owner | `integrator-flip-rate.py`, `integrator-preflight.py` (MP-9 makes them per project) | flip < 2 %, preflight 0 |
| 10 | **Open the queue:** remove the task-admission hold; the project joins fair share; digest and canary cover it (§7) | project owner | console | digest lists the project |

Steps 1–4 are independent of the host and can be done before a host is chosen. Steps 5–7 are repeated
per host (a project can be served by oracle-1 and mxmini; each host owner opts in separately).

### 6.3 Offboarding

Reverse order, each step reversible until the last:

1. Pause the slot (`projects/<slug>/pause`); running launches finish or are drained with the existing
   release-at-checkpoint (`record::release`); wait for no active attempts and no unresolved
   intents/holds (`integrator-preflight.py`).
2. If the integrator is being removed, set `integration_owner=agent` first (otherwise approved work has
   no landing path), then remove the project from `integrator.toml`'s `projects`.
3. Remove the slot from `supervisor.toml`; remove the push-helper config, egress listener, caches;
   delete `projects/<slug>/` (mirror, strikes). Ledger rows and coordinator history are **kept**.
4. Revoke the project's principals; the owner removes the repository from the App installations and
   decides whether to keep the rulesets (recommended: keep, they protect the branch regardless).
5. Archive the coordinator project in the console; nothing is deleted from the service.

`project remove` (MP-7) performs 1–3 and prints 4–5 for the owners; it refuses while attempts are
active.

## 7. Canaries, digest and HRI per project

### 7.1 Today

`attention.py` takes one project (`--project` / `ATTENTION_PROJECT`, default the coordinator's id,
`attention.py:68`): `digest` reads `GET /projects/{id}/digest`; `canary` checks `/healthz`,
`next?role=implementer` as the supervisor, a heartbeat file's age, HRI ≤ `--max-hri`, and digest-unread
days (server `digest_reads` is per project). `e2e-canary.py` creates one trivial task per host and
harness in the host's **canary project** (`E2E_PROJECT`, U33), authorised by
`COORDINATOR_CANARY_PRINCIPALS`; canary tasks are excluded from digest and HRI and ledger rows of the
canary project are marked.

### 7.2 Proposal

- **Pipeline canary (LLM) stays per host and harness, in the host's canary project.** It proves
  claim → launch → push → review → integrator on that host. It cannot prove a *project's* toolchain,
  setup, push repository, egress or rulesets, because it never touches that repository.
- **Add a per-project, LLM-free `project verify` (step 6) as the project canary**, run daily by a
  timer and after every `agentc-update` promotion: it costs nothing, covers exactly the
  project-specific parts, and its failure pages like any canary SLO. A real LLM task in the project
  remains the value signal, not a probe.
- **Digest:** `attention.py` accepts `ATTENTION_PROJECTS` (list). One mail per owner per day with a
  section per project, one signed ack link that records a read for every listed project
  (additive server change: the link names several project ids). Without that change the owner gets N
  links; D-16.
- **HRI, stall and neglect** stay per project on the server; pages carry the project name; `--max-hri`
  becomes per project; the canary reads `projects{}` in the heartbeat (§1.3) so a stuck slot pages even
  if the host loop is alive.
- **Exit metrics for a new project** mirror the dogfood's (HRI/task = 0 excluding reserved decisions;
  value canary not worse than baseline; direct-push canary flat; digest ≤ 1/day) over the first two
  weeks *with the project's own baseline*. Note the likely pressure point (§13.1, D-12): the owner currently
  commits to the second project directly and interactively, so `unreviewed_landing` entries and the
  direct-push canary will be non-zero until D-12 is settled.
- Update `soak.py`/host reports to group the ledger by `project`.

## 8. Review independence across projects

Today: `review_mode` is per project (`either`, `agent`, …); contributors to a submission (principals,
sessions, landing-range authors) cannot review it; the reviewer is a fresh launch in a separate uid
with a per-launch read-only clone and the candidate in an inner sandbox; the supervisor reports
`review_independence` (`distinct_launch` default) and the service stores it
(`migrations/0029_review_independence.sql`) without enforcing a floor. The other-vendor **audit
sample is not built** (`book/src/deploy/README.md:1011`).

Consequences for several projects:

1. **Independence is per submission, so nothing new is needed for correctness.** The same reviewer
   principal serving two projects does not weaken a review.
2. **Same-model blind spots matter more outside the coordinator.** The dogfood's reviewer-
   disagreement record (3 of 3 valid so far) is evidence about this codebase and its contract text, not
   about another repository. For a new project use a **trust ramp**: the owner reviews (`either`
   already allows it) or the audit sample (MP-13) re-reviews with the other vendor at a high rate for
   the first ~20 approvals, then tapers. D-13.
3. **A per-project vendor floor** (`review_vendor = "other"`), honoured by the supervisor when both
   vendors are configured, recorded as `distinct_vendor`; server enforcement of a floor is a later
   option, not needed while hosts are the owner's.
4. **Knowledge isolation** is the real cross-project hazard (§4.1): L2 scoping, and until then the
   instruction not to share across projects.
5. **Reviewers running candidate code from a private repository** use the project's own mirror and
   the project's own egress listener and `verification` entry; they never receive another project's
   run directories (`sandbox.rs:111` hides other runs) or logins (`hide_verification_logins`).

## 9. What stays self-hosting-only

Not generalized, and not to be copied into other projects' configuration:

- **Coordinator deploys** (U9): reserved owner decisions, the release packet, the fence gate [F], the
  old-binary snapshot/rollback drill, and the deploy preflight, which must be zero **across all
  projects** (a Codex session in the second project blocked it on 2026-10-09 — HANDOFF). Multi-project
  hosts must therefore drain *all* slots on the host kill switch; interactive sessions in other
  projects still block the preflight, which is intended.
- **`scripts/ship.py` and `.github/workflows/*`** job names, `release.yml` host bundles, `v*` tag
  ruleset, the coordinator's `roster.toml`, `CANARY.md`.
- **Staging coordinator and its test login** (`deploy/agentc/staging.py`, `staging_login.rs`) as a
  `verification` environment.
- **Service switches** (`COORDINATOR_CANARY_PRINCIPALS`, digest sender, weekly budget flags) are
  coordinator configuration, not per-host.
- **Defaults that name this project** (`attention.py:68` `DEFAULT_PROJECT`; `host-setup.sh` `REPO_URL`,
  push App ids, `EXTRA_EGRESS`) — in a multi-project world they become required parameters; the
  coordinator's values move into its own documented host file.
- **Dogfood measurement** (HRI/task, value canary) is per project; the coordinator's baseline is not a
  baseline for others.

Generic: supervisor, integrator, push helper, host-setup (parameterized), canaries/attention
(parameterized), containment suite (per project), updater.

## 10. Quiet hours

Existing: a host-wide, local-time window enforced by holding the kill switch (`quiet-hours.sh`), plus
a server-wide UTC window that only discounts hours in the stall clock (`attention.rs:70`). They are not
connected, so a secondary host with a night-only window produces `no_progress` stall findings for
work it deliberately does not claim during the day, and the two windows are in different time zones.

Recommendations (D-15):

- **Do not build per-project windows.** The window is a property of the *host's other use* (mxmini is
  the owner's workstation), not of the project. Per-project cost-time-of-day shaping, if ever needed, is
  a `daily_usd` question.
- **Make the host window native** (`[run] quiet_hours = "22:00-07:00"`, local time), because the cron
  + kill-switch shim is Linux/sysvinit-only and cannot work for the WSL2/portable hosts (U37), and so
  the heartbeat can report `quiet` distinct from an owner kill switch (today both are a file; the
  marker `quiet-hours` already distinguishes them to the script but not to the canary).
- **Tell the server** each host's window via the heartbeat-derived host registration (or the digest
  sender's report) *only if* secondary-host stall false positives are actually observed; until then the
  digest's existing `no_progress` rule is acceptable because the always-on host (oracle-1) keeps the
  queue moving.

## 11. User-owned decisions

Each has options and a recommendation. Defaults assume the owner is both host and project owner for
the first non-coordinator project.

| # | Decision | Options | Recommendation |
|---|---|---|---|
| D-1 | Supervisor topology | (a) one supervisor per host with a project list; (b) one process per project per host; (c) tenant instance per trust domain | **(a) as default, (c) as the escape hatch; reject (b)** (§1.2) |
| D-2 | Isolation tier for the first non-coordinator project | (a) shared `agentc-impl`/`agentc-rev` uids, separate clones/runs (today's canary level); (b) per-project uids; (c) separate host/instance | **(a)** for same-owner repositories; (c) for any repository the owner does not control or that holds third-party secrets |
| D-3 | Coordinator credential scoping | (a) none (L0); (b) per-project principals (L1) now, server scope (L2) before any cross-trust-domain project; (c) L2 first | **(b)** |
| D-4 | Claim fairness | (a) strict order; (b) priority merge; (c) spend-weighted fair share with an urgent bypass | **(c)**, all weights 1 to start |
| D-5 | Spend ceilings | (a) host caps only; (b) host caps + per-project `daily_usd` | **(b)**; first project $25/day, reviewer share of the existing $50; owner sets the numbers |
| D-6 | Concurrency | (a) stay at one launch per host; (b) build N-way now | **(a)**; build MP-12 only on measured wait time |
| D-7 | Toolchain delivery | (a) keep Rust baked in; (b) root-owned host-installed profiles; (c) per-project container images | **(b)**; (c) is a different sandbox model and is not proposed |
| D-8 | Egress | (a) one shared list (union); (b) per-project listeners (enforced for Claude, advisory for Codex) | **(b)**; run Codex only on the default listener |
| D-9 | GitHub Apps | (a) one push App + one integrator App per owner, many repositories, token scoping for least privilege; (b) one App per repository | **(a)**; (b) only on the project owner's request |
| D-10 | Non-GitHub projects (three on Azure DevOps today) | (a) stay on the agent path; (b) build an Azure DevOps checks source and push helper | **(a)** now; (b) is a separate decision after the second GitHub project works |
| D-11 | CI for the first project | (a) a secret-free GitHub-hosted Actions workflow providing the required job; (b) a self-hosted runner; (c) keep the producer-registered check on the agent path (no integrator) | **(a)** if the gate fits hosted runners (the project's own gate is ~25 min and heavy — measure first); (b) breaks the "no secrets, no untrusted code on our hosts" CI invariant; (c) is the fallback and a legitimate outcome |
| D-12 | Human workflow in the first project once rulesets apply | (a) full rulesets A/B: humans fast-forward green SHAs only (a `ship` equivalent for that repo); (b) observer mode: integrator lands, humans may still push | **(a)**: ruleset B is the safety basis (fast-forward-only target); (b) leaves `target_rewritten` freezes and unreviewed landings as the normal case |
| D-13 | Review trust ramp | (a) none; (b) owner reviews the first N (`either`); (c) build the other-vendor audit sample and apply it at a high rate to the first ~20 approvals | **(b) now, (c) as MP-13** |
| D-14 | Agent-task weekly admission budget scope | (a) keep per project (what is built); (b) add a global ceiling; (c) per-project override on the project record | **(a)**; (c) only if a project shows a need |
| D-15 | Quiet hours | (a) host-level only, native in the supervisor; (b) per-project windows; (c) leave the cron shim | **(a)** (§10) |
| D-16 | Digest shape | (a) one combined mail with one ack link for all listed projects (needs a small server change); (b) one mail and link per project | **(a)** |
| D-17 | Cross-project knowledge sharing by supervised credentials | (a) forbid by instruction + digest check until L2; (b) allow | **(a)** |
| D-18 | Offboarding retention | (a) keep ledger and coordinator history, delete mirror/state/caches; (b) purge | **(a)** |
| D-19 | Timing | (a) build after the dogfood window closes (2026-10-23 13:07 Z); (b) interleave with the dogfood | **(a)** for supervisor and integrator code (the dogfood measures the current loop; changing it distorts HRI); documentation-only and server-additive tasks (MP-6 spike, MP-10 design) may run earlier |

## 12. Implementation plan

Each task is independently shippable (the repository gate green, existing hosts' behaviour unchanged,
documentation updated in `book/src`) and files as a normal coordinator task *after* sign-off. "Deps"
are hard prerequisites. Sizes are rough (S ≤ 1 day of agent time, M ≈ 2–3, L > 3).

| Task | Content | Deps | Size | Ship effect |
|---|---|---|---|---|
| **MP-1** Project list in supervisor config | `[[projects]]` replacing `[run.binding]`/`[run.canary_binding]` (both still accepted and mapped); slug; per-project mirror path; `branch` and `repository` defaulted from the coordinator project record (fixes `run.branch` for `master`); `instructions` list; per-project state dir; heartbeat `projects{}`; test that the current coordinator config yields an identical effective config | — | M | none visible on the existing host |
| **MP-2** Fair claiming and per-project failure isolation | spend-weighted fair share + urgent bypass using `priority` already in `next` plus two additive action fields (`ready_since`, `admission_class`); per-project strikes, pause file, `daily_usd`, `budget_minutes` override, `next` backoff, starvation guard; unit tests on a fake `Driver` with three projects | MP-1 | M | one-project hosts unchanged |
| **MP-3** Toolchain profiles | extract the Rust environment into the built-in `rust` profile with a byte-identical-output test; profile schema; `host-setup.sh TOOLCHAINS=`; project-neutral role contracts (`{{gate_hint}}`); distinct-cache check | MP-1 | M | none for the coordinator |
| **MP-4** Per-project egress and containment | `[egress.listeners]`, relay endpoint per slot, preflight probes per listener, `containment-suite.sh --project`; Codex-advisory note in docs | MP-1, MP-3 | M | default listener = today |
| **MP-5** Per-project coordinator credentials (L1) | per-slot credential directory, `host-setup.sh` creates/guides per-project principals, docs for rotation | MP-1 | S | optional; L0 stays valid |
| **MP-6** Project-scoped credentials (L2) | day-one spike sizing the enforcement points, then migration + enforcement for `supervised`/`integrator` classes + labelled refusal + knowledge-sharing guard + tests; additive and opt-in per credential | — | L | required before D-2(c)-class projects |
| **MP-7** Host lifecycle commands | `agentc-supervisor project add|verify|remove` and `onboard check`; fragments in `/etc/agentc/supervisor.d/`; push-helper config template; instance-name parameter in `host-setup.sh`/`integrator-host-setup.sh`; LLM-free `verify` timer | MP-1, MP-3, MP-4, MP-5 | L | new commands only |
| **MP-8** `agent-coordinator bootstrap github` | owner-run, dry-run default: ruleset A/B/tag payloads, App-installation check, rules read-back, roster proposal, negative push test; docs replace the hand steps in `integrator-cutover.md` | — | M | new command only |
| **MP-9** Integrator per-project settings, budgets and cutover scripts | `[[project]]` tables, per-project cycle timeout and backoff, `project_cycle_failing` report, repository-owner vs installation check, two-remote end-to-end test in `e2e_tests.rs`; `--project` filter for `integrator-preflight.py` and roster-driven job names for `integrator-flip-rate.py` (with their `-test.py` coverage) | — | M | single-project configs unchanged |
| **MP-10** Attention and canaries for several projects | `ATTENTION_PROJECTS`, combined digest and multi-project ack link (small server change), per-project canary checks including `projects{}` heartbeat, drop `DEFAULT_PROJECT`, `soak.py` per-project grouping, tests in `attention-test.py` | MP-1 (heartbeat shape) | M | coordinator host keeps one project |
| **MP-11** Native quiet hours | `[run] quiet_hours` in the supervisor, heartbeat `quiet` state; shim retained for sysvinit until removed | MP-1 | S | optional |
| **MP-12** Concurrent launches | `max_concurrent_launches`, per-project `max_concurrent`, `Driver` refactor | MP-2 | L | gated on measured wait (D-6) |
| **MP-13** Other-vendor audit sample + review vendor floor | build the audit sample promised by plan-final §2.3; per-project `review_vendor` | — | M | optional per project |
| **MP-14** First non-coordinator project onboarding | the §13 walkthrough as an owner-run task; repository-side tasks in the target project (AGENTS.md, roster, workflow, ship script) filed there | MP-1…MP-5, MP-7, MP-8, MP-9; MP-10 before opening the queue | M (ops) | the first real use |

Critical path to a shadow run on a second project: **MP-1 → MP-3 → MP-4 → MP-7**; fairness (MP-2) and
credentials (MP-5) can proceed in parallel after MP-1 and are required before the queue is opened;
MP-6 is on the path only for a different trust domain.

## 13. Worked example: onboarding the SithBit project

The second real repository is already a project on the production coordinator (created 2026-09-27;
interactive sessions use it today). Facts below are tagged **[live]** (read from the service on
2026-10-12), **[review]** (from `book/src/docs/repository-review.md`, a local checkout read on
2026-09-09 — may be stale) or **[unknown]** (the onboarding probes must establish it). No command
below has been run; none is a statement that the project can be onboarded today.

### 13.1 Facts

| Fact | Value |
|---|---|
| Coordinator project | name `sithbit`, id `3831e59e-826c-4a72-be6a-a2fac61910ef` **[live]** |
| Repository / target branch | `https://github.com/marshallr12/sithbit-solana.git`, **`master`** **[live]** (the 2026-09 review read a local branch `development`; the integrator target must be confirmed) |
| Project policy (rev 4) | `review_mode=either`, `agent_rule_editing=true`, `recovery_mode=agent`, `automatic_integration=true`, `allow_subagent_reviews=true`, `lease_seconds=600`, `integration_owner=agent` **[live]** |
| Required checks (workflow policy rev 1) | one: identity `workspace-gate`, version `v1`, environment `linux-x86_64` **[live]** — a producer-registered check, not yet a GitHub Actions roster mapping |
| Size and conventions | ~3,500 commits; root `CLAUDE.md` with gate guidance, `HANDOFF.md`, `BACKLOG.md`, `DURABLE-RECORD.md`, `SAVERS.md`; Claude hooks (deploy-preflight guard, edited-Rust formatter); a detached gate runner `scripts/gate-detached.sh` whose runs took ~25 minutes; an overnight orchestration with a resource policy **[review]** |
| Toolchain | Rust (hooks format edited Rust files) **[review]**; whether Solana/Anchor CLI, Node or Python are needed **[unknown]** |
| Existing CI, branch protection, repository visibility, App installations | **[unknown]** |
| Secrets needed by tests/gate | **[unknown]** — decides D-11 |
| Task history on this coordinator | exists (imports of the old backlog, e.g. the item-26 fixture); not a blocker |

Gaps the design closes for this project: target branch `master` (MP-1); `CLAUDE.md` is not in the
instruction list and would never auto-load in a supervised launch — its gate guidance must be carried
by `AGENTS.md` or an `instructions` entry (MP-1; note the 32 KiB truncation); lease 600 s is
shorter than the coordinator's 3600 s; the supervisor renews at the cadence the service returns
(`renew_after_seconds`, `lease.rs` `cadence`), so this should work unchanged, but `project verify`
must confirm it and the policy's `max_attempt_seconds` (0 = unlimited) against `budget_minutes`; a gate of ~25 minutes needs `budget_minutes` ≥ 90 per
launch and implies cost on the order of a normal task but a long wall clock (MP-2).

### 13.2 Host configuration (oracle-1; mxmini later, quiet hours already in place)

`/etc/agentc/supervisor.toml` additions, after MP-1/3/4/5 (values in angle brackets are the owner's):

```toml
[[projects]]
project_id   = "3831e59e-826c-4a72-be6a-a2fac61910ef"
toolchains   = ["rust"]                 # + whatever the probe in §13.3 step 2 finds
instructions = ["AGENTS.md", "CONTRIBUTING.md"]
egress       = "sithbit"
weight       = 1
daily_usd    = 25
budget_minutes = 120                    # [unknown] confirm from a gate run

[egress.listeners.sithbit]
listen = "127.0.0.1:3129"
allow  = []                             # filled from the probe: registries the build needs

[setup."3831e59e-826c-4a72-be6a-a2fac61910ef"]
command     = ["cargo", "fetch", "--locked"]       # [unknown] — if the project has a fetch step
cache_paths = ["/var/lib/agentc/impl/caches/sithbit"]

[push_helper.project_configs]
"3831e59e-826c-4a72-be6a-a2fac61910ef" = "/etc/agentc/push-sithbit.toml"
```

`/etc/agentc/push-sithbit.toml` (root-owned, protected like `push.toml`): the existing App id and
installation id, `repository = "https://github.com/marshallr12/sithbit-solana.git"`. `integrator.toml`:
append the project id to `projects` once rulesets are applied; the repository belongs to the same
GitHub account as the coordinator's, so the same installation covers it and no second instance is
needed (§5.3). Credentials: principals `agentc-impl-sithbit` / `agentc-rev-sithbit` (L1), installed
root-owned `0600`.

### 13.3 Walkthrough

1. **Probe, read-only, on the owner's machine.** Establish the **[unknown]** rows: default branch and
   any protection, workflows present and their runtime, which toolchains and registries a clean build
   uses, whether tests need secrets, repository visibility. Output: a one-page note appended to this
   document's successor task; decides D-9/D-11/D-12 for this project.
2. **Decide D-11.** If the ~25-minute gate cannot run as a secret-free GitHub-hosted workflow, the
   project stays on the agent integration path (`integration_owner=agent`, producer-registered
   `workspace-gate`) and only the supervisor side (MP-1…MP-5, MP-7) is used: implementers and
   reviewers work, landing stays as it is today. This is a supported, useful half-way state.
3. **Repository readiness** (a normal task in the `sithbit` project, claimed by a human or an agent):
   an `AGENTS.md` that carries the minimum from `CLAUDE.md` (gate command and where it is, "never push",
   how to read `HANDOFF.md`); `.agent-coordinator/roster.toml` mapping `workspace-gate` to the workflow
   job; the workflow itself, secret-free, triggered on push. Merged by the existing path.
4. **GitHub (owner):** add `sithbit-solana` to the push App and integrator App installations; run
   `agent-coordinator bootstrap github --project 3831e59e-… --dry-run`, review the payloads, apply
   (target `master`, `ac/results/**`); confirm the read-back shows `non_fast_forward` and the required
   check. **Consequence for the owner:** direct pushes to `master` stop; humans use a green-SHA
   fast-forward (a repo-local `ship` script, D-12).
5. **Principals and secrets (owner, sudo):** create the two L1 principals in the console; install their
   tokens root-owned; copy `push.toml` to `push-sithbit.toml` with the new `repository`; no key is
   created, printed or copied by any script.
6. **Host project (owner, sudo):** `agentc-supervisor project add --project-id 3831e59e-… --toolchain rust`
   (creates mirror, state, fragment, helper config; leaves it disabled); toolchains beyond Rust are
   installed by `host-setup.sh TOOLCHAINS=…` first.
7. **`project verify sithbit`:** clone from the mirror (`master`), setup under the sandbox, the gate
   command under the implementer profile with `AGENTC_TEST_NESTED_SANDBOX=1` (tests needing real
   Bubblewrap print `note: skipping` and are *reported as skipped*), the containment project section,
   push-helper dry run to a scratch ref, ruleset read-back, a `next` call as `agentc-impl-sithbit`.
8. **Shadow day:** `agentc-supervisor shadow` and `agentc-integrator shadow` for a day; compare
   estimated cost with `daily_usd`; resolve every blocked/refused line.
9. **First task:** admit one small task (docs or a test, not a protocol change), `weight=1`, watch it
   through review (owner reviews per D-13) and integration; record cost and HRI per host.
10. **Cutover:** `integrator-flip-rate.py` on the real workflow (≥ 20 reruns, each check < 2 %),
    `integrator-preflight.py --project …` zero (both scripts need MP-9's changes), flip `integration_owner=integrator` for this project only.
11. **Open the queue**: keep the agent-task admission (5/week when this was written; 14/week since the owner's 2026-10-30 change), enable the digest section, the project
    verify timer and the per-project canary SLOs; the coordinator dogfood and this project share the
    host through fair share, the coordinator's canary and reverts staying urgent.
12. **Review at two weeks** against the exit metrics in §7.2; adjust `weight`, `daily_usd`,
    `budget_minutes`; decide on a second host.

Rollback at any step before 10 is "pause the slot" (§6.3); after 10, set `integration_owner=agent`
and, if desired, relax ruleset B (owner action).

## 14. Risks and open questions

- **Cross-project access via a shared credential** (L0/L1) is the largest risk and is accepted only for
  same-owner repositories; D-3 and MP-6 exist to retire it.
- **Heavy gates versus the CI model:** a 25-minute workspace gate on hosted runners, rate limits and
  GitHub minutes are unmeasured; the flip-rate gate may fail for a long, resource-sensitive suite (D-11).
- **Owner's own workflow changes** when ruleset B applies (D-12); expect friction and a temptation to
  bypass — the direct-push canary exists for exactly this.
- **Fair share on a single launch slot** can still produce multi-hour waits for the second project when
  the first has long tasks; measure before MP-12.
- **Claude `CLAUDE.md`/hooks of the second project are intentionally not loaded**; project knowledge
  that lives only there is invisible to supervised agents until moved to `AGENTS.md`.
- **Unverified here:** SithBit toolchain, CI and branch protection (§13.1); the L2 enforcement cost
  (MP-6 spike); whether a combined digest ack link is acceptable to the mail flow (MP-10).
- **Process:** this document is evidence-based design, not an approval; filing MP tasks is gated on
  the owner's answers to §11, and building starts after the dogfood window (D-19).

## Appendix: code and document references (revision `24a7f3d`)

Supervisor: `crates/supervisor/src/run_loop.rs` (`iterate` 334, `work` 450, `INSTRUCTION_FILES` 47,
`RunConfig.branch` 67, `canary_binding` 78); `run_loop/binding.rs` (`CanaryBinding` 89,
`check_against` 118); `run_loop/live.rs` (`Slot` 56, `new` 89, mirror branch 293,
`resolve_binding` 559, `place_credential` 573, `read_credentials` 626); `config.rs` (`verification` 49,
`setup` 60, `project_configs` 77); `profile.rs` (`environment` 227); `setup.rs` (`ProjectSetup` 29,
`can_run` 97); `verification.rs` (22); `egress.rs` (`DEFAULT_ALLOW` 13, `serve` 59); `relay.rs`
(`Endpoint` 26); `sandbox.rs` (`shared_args` 32, `hide_other_runs` 111); `run_loop/health.rs`
(`capped` 195); `run_loop/cost.rs` (`spent_since` 337); `run_loop/unsubmitted.rs` (`total_hold` 90);
`shadow.rs` (`project_ids` 161); `preflight.rs` (network checks 45).
Integrator: `crates/integrator/src/config.rs` (`projects` 34, `roster_path` 49, `GithubConfig` 56);
`integrate.rs` (`cycle` 90); `main.rs` (`run_loop` 176); `git.rs` (`mirror_dir` 75); `state.rs`
(`target_key` 21); `github.rs` (`RepoId::from_url` 32); `askpass.rs` (`answer` 50); `watch.rs`;
`roster.rs`; `push/config.rs` (22); `push/serve.rs` (`token_scope` 66).
Server: `admission.rs` (`admitted_in_week` 235); `credential_attributes.rs` (38, `is_integrator` 146);
`integrator.rs` (`require_integrator` 88, `require_integrator_project` 99); `next.rs` (action fields 183-195);
`coordination.rs` (work ordering 1596); `attention.rs` (`QuietHours` 70); `migrations/0029_review_independence.sql`,
`0031_attention_budget.sql`, `0033_admission_control.sql`, `0034_canary_budget_exemption.sql`.
Deploy: `deploy/agentc/{host-setup.sh, integrator-host-setup.sh, quiet-hours.sh, attention.py,
e2e-canary.py, containment-suite.sh, integrator-flip-rate.py, integrator-preflight.py}`.
Documents: `book/src/docs/{implementation-status.md, implementation-spec.md:73, integrator-cutover.md,
repository-review.md}`, `book/src/deploy/README.md` (canary binding ~1351, audit sample 1011);
`autonomy-plan` branch: `planning/autonomy/{plan-final.md §2.3 §2.4 §5, p4-design.md §3 §5}`, root
`HANDOFF.md` (U31–U38, dogfood window).
