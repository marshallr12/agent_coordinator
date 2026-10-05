# Integrator cutover (P4 S6)

This runbook prepares the deterministic integrator on oracle-1. Production
ownership remains `agent` until every prerequisite below is verified. The S6
commands described here require the S6 binaries and service deployment; the
previous P4 release has no shadow queue view. Root host setup, credential issue,
and ruleset changes are owner steps. Never put credentials or App keys in Git.

## Install and observe

1. Repository selection is complete: App `5127380`, installation `166293403`,
   has access to `agent_coordinator` only; `agentc-ruleset-test` is removed.
   Contents and Actions write, Checks,
   Administration and Metadata read are required. The existing private key stays
   at `/etc/agentc/integrator-app.pem` on oracle-1, root-owned mode `0400`.
2. Ship and deploy the reviewed S6 service change using the normal release
   preflight. Build a host-native `agentc-integrator` from that same revision:
   `cargo build --release --locked -p agentc-integrator`. Transfer the binary and
   `deploy/agentc/integrator-host-setup.sh` to oracle-1. On the host run:

   ```sh
   sudo INTEGRATOR=/absolute/path/agentc-integrator bash integrator-host-setup.sh
   ```

   Setup requires systemd 247 or newer and installs the dedicated uid and systemd template, stops an active
   integrator before replacement, and preserves existing configuration. It
   does not enable or start a unit. A shared nonblocking lock prevents the
   shadow and live daemons from running together. The service memory budget is 256 MiB;
   observe restart/OOM logs during the shadow day.
3. Issue a **read-access**, integrator-class credential for the project in the
   dashboard. Save CLI-format TOML (`[[credentials]]`, `origin`, `token`) to
   `/etc/agentc/integrator-credentials.toml`, root-owned mode `0400`, on the host.
   systemd `LoadCredential` exposes private runtime copies to the integrator uid;
   neither source secret needs to become readable to agent implementers.
4. Edit `/etc/agentc/integrator.toml`: set the project id in `projects`, verify
   `origin`, App ids, `checks = "github"`, and credential/key paths under
   `/run/credentials/agentc-integrator@shadow.service/`. Keep insecure loopback
   disabled. This release includes `.agent-coordinator/roster.toml`, mapped to
   the live service roster revision 3. Verify it remains current before the
   shadow. Each `[[required_checks]]` entry
   needs the service's exact `identity`, GitHub `check_name`, and `workflow_path`.
   The names/paths are:

   | Check name | Workflow path |
   | --- | --- |
   | Linux format, Clippy, and workspace tests | .github/workflows/ci.yml |
   | Audit locked dependencies | .github/workflows/ci.yml |
   | Pinned mdBook build and local-link validation | .github/workflows/docs.yml |

   Read the current service roster; do not invent identities. Every service
   required identity must be mapped. The integrator reads this file from T0
   and binds checks to the workflow blobs in T0.
5. Start `sudo systemctl enable --now agentc-integrator@shadow.service`.
   Retain at least 24 hours of timestamped journal records outside Git:
   `sudo journalctl -u agentc-integrator@shadow.service -o cat`.
   Verify continuous coverage, no errors/restarts, observed targets and check
   runs, and `WouldPush` records when approved candidates exist. A quiet queue
   proves polling only; record that limitation and arrange a reviewed live
   candidate test before cutover. Reverts are counted, not computed in shadow.

Shadow calls `GET …/integrator/queue?shadow=true` without updating the live
heartbeat or requiring integrator ownership. It computes local R, lists missing
rule types and privilege findings, reads checks on T0/R and logs proposed
publication. `WouldPush` is a proposal with `authority_verified=false`; it does
not claim approval of checks, privilege decisions or service push authority.
No result, receipt, report, revision, observation, candidate push, rerun or
publication is sent. Local mirrors/intents live under `state_dir/shadow`.
Missing full rulesets are expected before cutover; a missing roster is an error.

## Required-check stability evidence

Pin the current `main` SHA and completed Coordination/Documentation run ids.
Rerun **all jobs**, sequentially, about 20 times on those same workflow runs
(`gh run rerun RUN_ID --repo marshallr12/agent_coordinator`, then
`gh run watch RUN_ID --repo marshallr12/agent_coordinator --exit-status`). This
consumes Actions time; retain the actual attempts and failures. Do not dispatch
new runs against a moving main or use failed-job-only reruns for this sample.
If main moves, restart the sample against the new tip.

Download both job histories without credentials in the output:

```sh
gh api --paginate --slurp 'repos/marshallr12/agent_coordinator/actions/runs/CI_RUN_ID/jobs?filter=all&per_page=100' > ci-jobs.json
gh api --paginate --slurp 'repos/marshallr12/agent_coordinator/actions/runs/DOCS_RUN_ID/jobs?filter=all&per_page=100' > docs-jobs.json
python3 deploy/agentc/integrator-flip-rate.py --sha MAIN_SHA ci-jobs.json docs-jobs.json
```

Each required check needs at least 20 distinct job attempts and a non-success
rate strictly below 2%. Cancellation, pending, neutral and skipped attempts fail
closed. With 20 attempts, one failure fails this gate. This is the observed
sample rate, not a statistical upper bound on future failures. Keep JSON and
report outside the repository and record SHA, run ids, sample size and rate.

## Cutover preflight and rulesets

Pause new agent work and drain current work. On the service host, run the
read-only SQLite preflight immediately before cutover:

```sh
sudo python3 integrator-preflight.py --database /var/lib/agent-coordinator/coordinator.sqlite3
```

Transfer `deploy/agentc/integrator-preflight.py` there first. Every count must
be zero: unresolved publication intents, held integration holds, held physical
reservations, registered/running/unknown jobs, recovery activities, active
integrations and all current task/review attempts. Expired or revoked attempts
must be reconciled too: recovery can be derived without a stored recovery state.
A nonzero count returns failure; inspect/reconcile it through the
existing workflow. Never clear database rows directly. The read snapshot is
not a lock: keep new work paused, repeat the check before flipping ownership.

The owner reviews and applies these three GitHub rulesets. Substitute the
numeric owner user actor id for `OWNER_ACTOR_ID`; never substitute an agent uid.
The [GitHub rules REST reference](https://docs.github.com/en/rest/repos/rules)
defines these actor and rule fields. Use `gh api -X POST repos/marshallr12/agent_coordinator/rulesets --input FILE`
with each reviewed JSON file and the current `X-GitHub-Api-Version: 2026-03-10` header. Record ids and effective read-back. Inspect
existing rulesets to avoid duplicating them on a retry.

A, branch update/creation/deletion authority:

```json
{"name":"integrator-writers","target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["refs/heads/main","refs/heads/ac/results/**"],"exclude":[]}},"rules":[{"type":"update"},{"type":"creation"},{"type":"deletion"}],"bypass_actors":[{"actor_id":5127380,"actor_type":"Integration","bypass_mode":"always"},{"actor_id":OWNER_ACTOR_ID,"actor_type":"User","bypass_mode":"always"}]}
```

B, fast-forward and checks, **no bypass**:

```json
{"name":"integrator-checks","target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["refs/heads/main"],"exclude":[]}},"rules":[{"type":"non_fast_forward"},{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"Linux format, Clippy, and workspace tests","integration_id":15368},{"context":"Audit locked dependencies","integration_id":15368},{"context":"Pinned mdBook build and local-link validation","integration_id":15368}]}}],"bypass_actors":[]}
```

Tags, owner only:

```json
{"name":"owner-release-tags","target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v*"],"exclude":[]}},"rules":[{"type":"creation"},{"type":"update"},{"type":"deletion"}],"bypass_actors":[{"actor_id":OWNER_ACTOR_ID,"actor_type":"User","bypass_mode":"always"}]}
```

Read `/repos/marshallr12/agent_coordinator/rules/branches/main` and the stored
rulesets back. Verify contexts, Actions app id, enforcement, ref patterns and
bypass actors. Keep the day-0 ruleset `day0-main` after B is active: it is the
only no-bypass ban on deleting the default branch (A lets the App and the owner
bypass deletion), and its `non_fast_forward` rule duplicates B. Never verify
protections by force-pushing main. App publication to an ephemeral
result branch must succeed; ordinary collaborator updates must be refused as
already demonstrated in the throwaway U4 test.

## Change ownership and verify

Stop/disable the shadow unit. Replace the read credential with an integrator
**write** credential in the protected host file. Edit both runtime credential
paths in the configuration from `@shadow.service` to `@run.service`.
Repeat preflight and confirm main still matches the measured SHA. The human
sets `integration_owner=integrator` in project policy, preserving the current
policy revision and all other fields. Enable/start
`agentc-integrator@run.service` and check the live heartbeat, watchdog,
privilege gates and a reviewed canary's check receipts, lease publication and
service completion. Read the remote main tip back; push exit status alone is
not evidence. Keep new work paused until the canary passes.

Human changes continue through `scripts/ship.py` with the emergency owner
bypass of A; B still requires successful checks. Record the cutover revision,
rule ids, heartbeat, canary R and completion evidence in the local handoff.
Do not declare S6 complete before the shadow day, stability evidence, full
rulesets, zero preflight, ownership switch and canary all pass.

For rollback, pause work, stop/disable the live unit and inspect unresolved
integrator authority/holds. Reconcile any landed R before changing ownership
back to `agent` through the human policy control. Preserve ruleset B and the
published result/audit history; do not rewind main or delete authority records.
The human owner bypass remains available for checked emergency roll-forward
changes. Resume shadow only after changing its credential paths back and
installing a read credential.
