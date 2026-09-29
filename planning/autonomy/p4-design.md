# P4-min integrator — design pass (2026-09-27)

Input: plan-final §2.4, §2.4a, §2.4b, §2.5, §2.2 items 1/5a, the P4 row in §3; two read-only code
surveys of `main` at `7c5079d` (service side, client/Git/checks side). Branch `autonomy/p4`, worktree
`~/src/worktrees/agent-coordinator-p4`. This document is the plan for the P4 sessions; plan-final wins
on intent, this file wins on mechanics.

## 0. Findings that change the plan

1. **GitHub has no IPv6** (`dig AAAA github.com api.github.com codeload.github.com` → empty), and the
   production VM is IPv6-only (deploy-gcp-e2-micro.md:33). Therefore:
   - U6's "integrator on the e2-micro" cannot fetch or push without paid IPv4 egress (external IPv4 ≈
     $3.65/month, or Cloud NAT) — conflicts with the "$1 free-tier guard" budget.
   - §2.4 "the service verifies ancestry itself with one compare-API call" is impossible from that VM
     for the same reason. The plan's own fallback applies: **integrator attestation + continuous
     ancestry audit** ([R] for projects without a service-side compare credential).
   - Decision **U16** (below) asks where the integrator runs. Recommendation: **oracle-1** (always-on
     ARM64 host with IPv4, already used by agents), third uid `agentc-integrator`; the plan's generic
     default (supervisor host) already covers this.
2. The commit named `3590591f` in plan-final is not in the repo; the agent publication reconciliation
   landed as **`1ba7d7a`** (2026-09-23). 3590591f is presumably the coordinator task id.
3. The server has **no outbound HTTP, no integrator role, no `integrator_last_seen`**; `next` rejects
   `role=integrator` (tests/workflow.rs:2815). Check receipts are client-reported `jobs` rows bound to
   `(task, attempt, source_revision=R, source_tree)` — GitHub Actions results cannot satisfy them today.
4. `coordinator_local::git_workflow` is reusable library code: deterministic, byte-reproducible R
   (`merge-tree --write-tree` + pinned identity/date, :893-968), CAS publish judged by post-push
   `ls-remote`, never exit code (:717-863). Constraints: linked worktree on a non-target branch
   (:510-521), remote passed as URL, credentials via the uid's Git credential helper.
5. U1 (Windows = observer) vs the workflow layout: `windows-client` is a job **inside** "Coordination
   checks", so requirements must be **job-level check runs**, not workflow-level as `ship.py` does.
   Required set: `Linux format, Clippy, and workspace tests`, `Audit locked dependencies`,
   `Pinned mdBook build and local-link validation`. `ship.py` is updated to the same job set.
6. Candidate refs live under `refs/agent-coordinator/candidates/` (not `refs/heads`), so ruleset A can
   restrict branch `update`/`creation` on `main` + `ac/results/**` without touching agent candidates.
   Agents' `task/*` / `codex/*` branch pushes stay allowed (ruleset A targets named refs only).

## 1. Shape

```
             GitHub (App: contents rw, actions rw, checks r, administration r, metadata r)
                 ▲ push ac/results/<id>, FF main          ▲ check-runs, reruns, rules read-back
                 │                                         │
  agentc-integrator (oracle-1, own uid, systemd, Restart=always, MemoryMax=256M)
   ├─ mirror.git + one linked worktree per result (git_workflow reused)
   ├─ loop: heartbeat → watchdog → queue → integrate → checks → authority → push → observe
   │                                         │ HTTPS, integrator credential (class=integrator)
   ▼                                         ▼
  coordinator service (e2-micro): integrator queue, results, push authority, observations, receipts
```

- **New crate `crates/integrator`** (binary `agentc-integrator`), depending on `coordinator-local`
  and `coordinator-client`. Not a supervisor subcommand: different uid, different host, different
  secrets (App key), and the supervisor must never be able to load the App key.
- **GitHub App auth**: RS256 JWT → installation token (1 h), cached; Git over HTTPS through a
  `GIT_ASKPASS` helper that is this same binary (`agentc-integrator askpass`), so no token is written
  to disk. Crate: `jsonwebtoken` (verify with `rust-api-scout` before use). REST via the workspace
  `reqwest` (rustls).
- **Config** `/etc/agentc/integrator.toml`, every entry defaulted in code (loopback staging service,
  `/var/lib/agentc/integrator` state, poll 30 s); only the App id/key path and service credential are
  needed for production. Example file with defaults commented out.

## 2. Service changes (additive; one migration `0024_integrator`)

Per-project cutover switch; the old LLM path keeps working until it is flipped.

| Item | Mechanics |
|---|---|
| Integrator identity | `credentials.class` gains `integrator` (0023 CHECK is rebuilt in 0024). `projects.integration_owner` `agent`\|`integrator` (default `agent`) + `projects.integrator_last_seen`. On `integrator` projects the LLM routes (`claim` of integration activities, publication-intent, integration-result, finalize, both reconciliations, authorization) refuse `integration_owned_by_integrator` (labelled gate style from P1). |
| Queue | `GET /projects/{p}/integrator/queue` → approved, pins-current subjects in integration phase, ordered by subject priority then age, with C, candidate ref, reviewed base, task digest, roster ids. Also serves as heartbeat (updates `integrator_last_seen`). |
| Results | table `integrator_results(id, submission_id, t0, t0_tree, c, r, r_tree, landing_range_json, roster_json, created_at, UNIQUE(submission_id, t0))`; `POST …/integrator/results` idempotent (replay returns the existing row, a different R for the same key is `result_conflict`). |
| Receipts | table `integrator_receipts(result_id, check_name, run_id, run_attempt, head_sha, app_id, workflow_path, workflow_blob, conclusion, observed_at)`; `POST …/integrator/receipts`. The **deciding run** per (R, check, workflow_blob) = latest completed. Replaces `jobs`-based receipts on integrator projects (`jobs` stay for non-integrator projects). |
| Push authority | `POST …/integrator/push-authority {result_id}` → `{deciding_runs, roster_ids, protected_ids, expires_at}` or refusal. Checks: approvals under the pinned digest; every roster check has a deciding success receipt with `head_sha=R` and `workflow_blob` = blob in T0; protected ids ⊆ roster; **5a** contributors over stored landing ranges (approver among them ⇒ back to review; range intersects a submission neither approved nor integrated ⇒ `stacked_on_unapproved`); `candidate_reverted_in_history`. Issuing takes the **submission-bound hold** (`integration_holds` gains `submission_id`; the global unique index stays for the old path only). |
| Observations | `POST …/integrator/observations {result_id, tip, ancestry: contained\|equal_t0\|moved, evidence}` → service records `published` (task done, dependents unblocked, hold released) / `not_published` / `target_moved` (new result needed). Attested by the integrator (finding 1). A revise pending on a subject whose R is observed contained becomes a follow-up task (**revise loses to a landed push**); `author_withdraw` ⇒ high-priority `revert` task. |
| Revise | integrator is an allowed actor for `conflict{T,C}` and `check_failed{receipt}`; `check_failed` requires reproduction (see §3). **Serialize-before-park**: when the 3/24 h limit trips and the conflicting landing is known, add a dependency on the task whose landing moved T instead of parking. |
| Tip monitor | `POST …/integrator/tip-moves {from, to, by_integrator, range_commits[{sha, trailers}]}` → `unreviewed_landing` digest entry when not by the integrator and any commit has agent trailers or unknown provenance; `target_rewritten` (to ∌ from) freezes the project queue. |
| Revert (M6) | task kind `revert{submission, R, reason, evidence}`; created by human one-click or agent with evidence; always admitted; candidate computed by the integrator (`git revert -m 1 R` / range), `reverted_by` link on the original. Cascade ≤ 3 only when main is broken. |
| Roster/rules from target | integrator reads `.agent-coordinator/roster.toml` and `rules.md` from T0 and sends them with the result; the service enforces the constitution floor (protected ids from `workflow_policies`, which becomes the floor store — kept, not deleted, until the first roster commit). |
| `next` / digest | `next?role=integrator` returns the queue head; labelled gates of the integrator surface in the human queue. |

Deletions (after cutover, in a follow-up once the old path has been idle ≥ 1 week): CLI
`integrations prepare/publish/finish/reconcile-agent`, MCP integration tools, publication intents,
results, reconciliations, journal attestations, human authorization, agent `PATCH policy` for rules
(rules move to the repo). Tables are kept read-only for history (expand-only migrations).

## 3. Integrator loop (one project at a time, one integration at a time — no push slot)

1. **Heartbeat + watchdog**: `GET /repos/{o}/{r}/rules/branches/main`; missing `non_fast_forward` or a
   required check ⇒ freeze (no pushes), page via the service (`ruleset_missing`).
2. **Observe tip** X (`ls-remote`); X ∌ previous tip ⇒ `target_rewritten`, freeze. Report tip moves.
3. **Queue head** S (C, reviewed base). Existing result for (S, X)? reuse R; else compute R with
   `create_integration_result`; conflict ⇒ `revise conflict{X, C}`.
   No-op (R == X) ⇒ observation `contained` unless a revert touches C's range
   (`candidate_reverted_in_history`).
4. **Privilege gate**: workflow files in R whose blob differs from X and add write permissions,
   `secrets.*`, `pull_request_target` or triggers ⇒ labelled (c) decision, skip S.
5. Record result (landing range `merge-base(X,C)..C`, roster from X), push R to `ac/results/<id>`
   (create-only lease), wait for check runs (`GET /repos/{o}/{r}/commits/{R}/check-runs`,
   `app.slug == github-actions`), post receipts.
6. **Flake attribution**: a failed required check ⇒ rerun failed jobs up to 2 more times; `check_failed`
   only if it fails 2 of 2 or 2 of 3 on R **and** the same check passed on X; else `flaky{check,R}`
   digest entry + de-flake task; fails on X too ⇒ `fix-target` digest entry.
7. **Push authority** → re-observe tip == X → `publish_prepared` (CAS with lease X) → re-observe →
   post observation. Never trust the push exit code. Tip moved ⇒ back to 3 with the new X
   (roll-forward). Crash anywhere ⇒ restart resumes from the service's result + a fresh observation.

## 4. Build order and sessions (estimate 4–6 sessions)

| Step | Content | Gate |
|---|---|---|
| S1 | Migration 0024 + integrator credential class + `integration_owner` + queue/heartbeat + results + receipts endpoints; old path untouched | full CI gate |
| S2 | push-authority (digest pin, receipts, 5a contributors, `stacked_on_unapproved`, submission-bound hold) + observations (published/not/moved, revise-loses-to-push) + refusal of LLM routes on integrator projects | full CI gate; server tests per rule |
| S3 | `crates/integrator`: config, App auth + askpass, mirror/worktrees, loop steps 1–3 and 5–7 against a **`ChecksSource` trait** (GitHub impl + local fake for staging) | crate tests with a local bare remote |
| S4 | privilege gate, flake attribution, tip monitor + `unreviewed_landing`, serialize-before-park, revert kind (M6) | full CI gate |
| S5 | staging soak (no LLM, fake checks, local bare remote via `deploy/agentc/staging.py`): conflicts, target moves, check failures, crash/restart, revert; HRI = 0, p50 < 10 min; then ship + deploy the service (U9 preflight) | soak report |
| S6 | cutover: host setup on oracle-1, **shadow day** on production (computes R, reads checks, logs would-push, pushes nothing), flip-rate run (~20 reruns of required jobs on current `main`, each < 2 %), preflight zero intents/holds/reservations/jobs/recovery, user applies rulesets, `integration_owner=integrator` | exit criterion of P4 in plan-final §3 |

## 5. User steps (in order; none needed before S3)

1. **Answer U16** (integrator host). 
2. **U4 ruleset test — DONE 2026-09-29:** on `marshallr12/agentc-ruleset-test`, active ruleset
   `24210180` restricts `update`, sole bypass App `5127380` (`always`). Ordinary-login Git pushes
   were rejected with `GH013`, including after adding the App bypass; the App installation token
   successfully pushed the same candidate `1547f6534183978fe4d373cc63229eb55657e7fe`. Token revoked;
   no org move needed. Evidence: `~/.local/share/agent-coordinator-autonomy/ruleset-test/` outside
   the repository. Original requirement: on a throwaway public repo, a ruleset with `update`
   and bypass = a test GitHub App; confirm a plain collaborator push to the protected branch is rejected
   and an App-token push succeeds. If it fails, U4 (move to an org) comes back.
3. **Create the GitHub App** (before S3's live test): personal account, no webhook; repository
   permissions **Contents: read & write, Actions: read & write, Checks: read, Administration: read,
   Metadata: read**; install on `marshallr12/agent_coordinator` only; download the private key and
   place it on the integrator host as root-owned `0400`, readable by `agentc-integrator` only (never on
   this workstation's user account, never in the repo). Give me the App id and installation id.
   **Registered 2026-09-29:** `marshallr12-agentc-integrator`, App id `5127380`, installation id
   `166293403`, permissions as above, private. Manifest-generated key went directly over SSH to
   oracle-1 `/etc/agentc/integrator-app.pem` (`root:root`, `0400`); the runtime's key access must be
   arranged by host setup. Currently installed only on the test repo: add `agent_coordinator`
   before live use and subsequently remove the test repo from the selected-repository list.
4. **Run the integrator host setup** on the chosen host with sudo (the script will be written in S6;
   the agent does not run root scripts).
5. **Issue the integrator credential** in the dashboard (class `integrator`, write) and install it on
   the host (0600, integrator uid).
6. **At cutover (S6), apply the full rulesets** (payloads will be in the S6 runbook): A — `update`,
   `creation`, `deletion` on `main` and `ac/results/**`, bypass = the App (+ owner for emergencies);
   B — `non_fast_forward` + `required_status_checks` (the three jobs, `integration_id` = GitHub Actions
   15368), **no bypass**; tag ruleset restricting `v*` to the owner. Day-0 ruleset `day0-main` is
   then superseded by B.
7. From cutover on, human changes go through `scripts/ship.py` (U2); direct pushes to `main` stop
   working by design.

## 6. New decision

| # | Decision | Recommendation | Answer |
|---|---|---|---|
| U16 | Integrator host, given the VM is IPv6-only and GitHub is IPv4-only | oracle-1, uid `agentc-integrator`; service trusts integrator attestation + ancestry audit ($0). Alternatives: external IPv4 on the VM (~$3.65/mo, enables service-side compare API); mxmini (not always on) | 2026-09-27: **oracle-1** (as recommended) |
