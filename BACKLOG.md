# Implementation backlog

The foundation and job/worktree evidence milestone are implemented. Use
[implementation status](docs/implementation-status.md) for precise limits.

Completed: native worktree preparation, stable producer identity, durable local
job reports, global named resource reservations, scoped health reporting,
observation-only reconnect, and recovery inspection. Uncertain producers retain
physical resource holds; resolving them records evidence without rewriting results.

Next work, preserving the original backlog numbering:

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
