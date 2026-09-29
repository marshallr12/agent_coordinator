# Integrator soak 2026-09-29T01:19:00-0400

Seed 95c09d05c19d, instance ~/.local/state/agentc-soak.

**PASS**: 6 scenarios, 23 subjects, 23 landings, HRI 0, latency p50 4.6 s / p90 14.6 s / max 19.3 s (goal p50 < 600 s: met), integrator restarts 4.

| scenario | result | subjects | landings | revises | reports | HRI | p50 s | p90 s | max s | restarts |
|---|---|---|---|---|---|---|---|---|---|---|
| clean | pass | 4 | 4 | none | none | 0 | 7.6 | 18.9 | 18.9 | 0 |
| conflicts | pass | 6 | 6 | conflict 4 | none | 0 | 3.9 | 4.6 | 4.6 | 0 |
| target_moves | pass | 2 | 2 | none | unreviewed_landing 1 | 0 | 4.4 | 14.6 | 14.6 | 0 |
| check_failures | pass | 2 | 2 | check_failed 1 | flaky 1 | 0 | 4.5 | 9.0 | 9.0 | 0 |
| crash_restart | pass | 5 | 5 | none | flaky 1 | 0 | 9.7 | 19.3 | 19.3 | 4 |
| reverts | pass | 4 | 4 | none | none | 0 | 4.7 | 5.6 | 5.6 | 0 |

Reports by kind: flaky 2, unreviewed_landing 1.
Revises by reason: check_failed 1, conflict 4.
Integrator steps: ChecksPending 9, Error 2, Idle 2, Observed(not_published) 1, Observed(published) 23, RevertCandidate 2, Revised(check_failed) 1, Revised(conflict) 4.

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

Integrator steps: ChecksPending 3, Observed(published) 2, Revised(check_failed) 1.
Latency samples (reviewed subjects): 2.


## crash_restart

Integrator steps: ChecksPending 4, Error 2, Observed(not_published) 1, Observed(published) 5.
Latency samples (reviewed subjects): 5.

- integrator SIGKILLs: after result, during rerun, mid-push (git survives, push lands while down), mid-push (process group killed, push aborted); server SIGKILL with checks pending

## reverts

Integrator steps: Observed(published) 4, RevertCandidate 2.
Latency samples (reviewed subjects): 3.


