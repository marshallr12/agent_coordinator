# Implementation handoff — 2026-09-09

The job/worktree evidence milestone is implemented on top of the executable
foundation. The complete design remains in PLAN.md; product decisions are settled.
See docs/implementation-status.md for the working subset and BACKLOG.md for next
work: immutable submissions, exact-source checks, independent review, serialized
integration, and integrated-result validation. Task completion remains unavailable
until that entire workflow is enforced.

Agents now prepare isolated worktrees with durable preparation intents and reserve
named shared capacity before launching a local producer. Jobs have durable producer
identities, protected journals, bounded local logs, scoped reporters, and optional
bounded renewal tied to an exact live harness process. Reconnect only observes;
uncertain launch intent never authorizes a replacement producer. The service never
runs workstation commands. Supported producer programs keep their work in the
foreground; detached or external work requires separate inspection before release.

Physical resource holds survive lease expiry, session closure, credential
revocation, and observer loss. Task requeue, recovery resolution, and checkout
changes cannot bypass unresolved evidence. A recovery owner may release an old
reservation after its jobs have terminal results. Humans may reconcile uncertain
holds with termination/isolation evidence; this preserves the producer's original
reported state. The dashboard shows both records separately.

Instruction version 2 returns the worktree/resource/job sequence and examples.
Existing sessions must reconnect and read the new instructions before claiming.
Use docs/CLI.md for Linux and native Windows commands. The CLI's producer environment
is cleared except for a narrow platform allowlist; configure needed toolchain
variables and never supply coordinator agent credentials to the producer.
Native Windows Git calls normalize canonical drive/UNC paths at the command
boundary, preserving canonical paths for checkout identity checks. Unsupported
device namespaces are refused; UNC conversion is not a network-filesystem
durability guarantee.

Linux validation: the integrated workspace tests, warnings-denied Clippy, formatting,
and JavaScript syntax checks pass. The extended service/CLI smoke exercise passes
worktree reconciliation with spaces, job launch, two reconnects without relaunch,
live-work release rejection, terminal reporting, and explicit capacity release,
alongside the original two-harness/two-project coordination checks. The locked
advisory audit reports no known vulnerabilities.

Browser validation used a disposable database and account: resource creation,
stale/unknown job evidence, human resource reconciliation without fabricated
success, released capacity counts, sign-out, and desktop/phone layouts were checked.
Temporary servers were stopped. No source projects or their histories were changed.

Native Windows validation for this milestone is still being completed. The first
run found two pre-launch/log-capture failures; do not treat Windows as verified until
the corrected run passes. The prior foundation run 34398311217 passed Linux,
native Windows, and the dependency audit.

The service has not been deployed publicly. Host, DNS name, and backup destination
remain installation choices. The 20-project/50-session/100,000-task target, backups,
restore fencing, retention, and two physical-workstation release exercise remain
unverified release work. Source inputs must currently be clean committed snapshots;
logs remain local and service artifact upload is deferred.
