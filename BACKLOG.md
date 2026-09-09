# Implementation backlog

The initial foundation covers most of plan milestones 1–2 and the initial web/CLI
surfaces from milestone 5. Use docs/implementation-status.md for precise limits.

1. Finish job/worktree evidence: native Linux/Windows checkout preparation, stable
   producer identity, durable local job reports, named resource holds, heartbeat
   reconnect, and recovery inspection of still-running work. Acceptance: loss of
   an observer never starts a duplicate producer or permits competing resource use.
2. Implement immutable submissions, exact-source/check receipts, independent
   agent/human/both review, one integration lease per canonical repository/target,
   and validation of the integrated result. Only then enable done/dependency release.
3. Add revisioned lessons, binding rules/provenance, decision queue, bounded context
   search, artifact links/uploads, and service-authoritative Markdown import/export.
   Exercise the audited SithBit and Submission historical fixtures without mutating
   those source repositories or reopening completed imported items.
4. Expand operator account management, recovery/rotation, policy editor, full task
   history pagination, operator recovery controls, and native client commands.
   Keep the short bootstrap snippet and returned instructions aligned with behavior.
5. Implement consistent backups and restore authority invalidation; retention of
   24 hourly and 30 daily copies, documented off-server copying, and restore exercise.
6. Complete Linux packaging and actual native Windows acceptance, the 20-project /
   50-session / 100,000-task load target, clock rollback handling, storage retention,
   and two physical-workstation release testing before production deployment.

No unrestricted task-status edit, automatic force recovery, or unverified
completion shortcut should be added to make these milestones appear complete.
