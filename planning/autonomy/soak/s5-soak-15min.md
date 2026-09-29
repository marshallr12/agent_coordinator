# Integrator soak 2026-09-29T01:23:21-0400

Seed 668335efe03f, instance <scratch>/agentc-soak-long.

**PASS**: 7 scenarios, 108 subjects, 108 landings, HRI 0, latency p50 4.5 s / p90 15.0 s / max 19.3 s (goal p50 < 600 s: met), integrator restarts 4.

| scenario | result | subjects | landings | revises | reports | HRI | p50 s | p90 s | max s | restarts |
|---|---|---|---|---|---|---|---|---|---|---|
| clean | pass | 4 | 4 | none | none | 0 | 8.1 | 19.3 | 19.3 | 0 |
| conflicts | pass | 6 | 6 | conflict 4 | none | 0 | 3.8 | 4.4 | 4.4 | 0 |
| target_moves | pass | 2 | 2 | none | unreviewed_landing 1 | 0 | 4.4 | 14.7 | 14.7 | 0 |
| check_failures | pass | 2 | 2 | check_failed 1 | flaky 1 | 0 | 4.4 | 14.2 | 14.2 | 0 |
| crash_restart | pass | 5 | 5 | none | flaky 1 | 0 | 9.7 | 19.2 | 19.2 | 4 |
| reverts | pass | 4 | 4 | none | none | 0 | 4.5 | 5.2 | 5.2 | 0 |
| mix | pass | 85 | 85 | conflict 18 | flaky 12 | 0 | 4.5 | 15.0 | 15.2 | 0 |

Reports by kind: flaky 14, unreviewed_landing 1.
Revises by reason: check_failed 1, conflict 22.
Integrator steps: ChecksPending 66, Error 2, Idle 2, Observed(not_published) 1, Observed(published) 108, RevertCandidate 2, Revised(check_failed) 1, Revised(conflict) 22.

## clean

Integrator steps: Idle 1, Observed(published) 4.
Latency samples (reviewed subjects): 4.

- 4 disjoint subjects approved together; each published with tip == R

## conflicts

Integrator steps: Observed(published) 6, Revised(conflict) 4.
Latency samples (reviewed subjects): 6.

- 3-way: revises 1-2 recorded their landing; revise 3 serialized_after the third landing, park_reason null; the subject stayed claimable

## target_moves

Integrator steps: ChecksPending 2, Idle 1, Observed(published) 2.
Latency samples (reviewed subjects): 2.

- roll-forward: 2 results pinned, landed on the owner's commit

## check_failures

Integrator steps: ChecksPending 4, Observed(published) 2, Revised(check_failed) 1.
Latency samples (reviewed subjects): 2.


## crash_restart

Integrator steps: ChecksPending 3, Error 2, Observed(not_published) 1, Observed(published) 5.
Latency samples (reviewed subjects): 5.

- integrator SIGKILLs: after result, during rerun, mid-push (git survives, push lands while down), mid-push (process group killed, push aborted); server SIGKILL with checks pending

## reverts

Integrator steps: Observed(published) 4, RevertCandidate 2.
Latency samples (reviewed subjects): 3.


## mix

Integrator steps: ChecksPending 57, Observed(published) 85, Revised(conflict) 18.
Latency samples (reviewed subjects): 85.

- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- roll-forward: 2 results pinned, landed on the owner's commit
- mix: conflict_pair x18, flaky_check x12, mix_clean x18, owner_moves_target x19

